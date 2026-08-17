//! Session-level CLI lifecycle tests: the exact code path the CLI commands
//! drive (stage → confirm → cancel/deny/retry/status) with a simulated chain
//! and fault injection — no network, no daemon.

use std::sync::Arc;

use bloom_chain_action::ActionState;
use bloom_chain_rpc::fault::{Fault, FaultProxy, ScriptedFault};
use bloom_chain_rpc::mediator::{ChainRpcProfile, DEFAULT_MAX_RESPONSE_BYTES};
use bloom_chain_rpc::sim::SimChain;
use bloom_solana_cli::commands::{confirm_operation, request_from_action, staged_from_action};
use bloom_solana_cli::session::{self, Session};
use bloom_solana_machine::fixture::ExactApprovalLedger;
use bloom_solana_machine::{AccountRegistry, TransferRequest};
use sha2::Digest as _;

const GENESIS: &str = "genesis-sim";
const DESTINATION: &str = "CZ8YUVdk7znjrUmnb5n7kgySk9yRAsQDYmyCxzfSky9t";

fn chain_profile() -> ChainRpcProfile {
    ChainRpcProfile {
        name: "solana-simnet".into(),
        family: "solana".into(),
        expected_genesis_hex: GENESIS.into(),
        allowed_read_methods: vec![
            "getGenesisHash".into(),
            "getBlockHeight".into(),
            "getLatestBlockhash".into(),
            "getFeeForMessage".into(),
            "isBlockhashValid".into(),
            "getSignatureStatuses".into(),
        ],
        allow_broadcast: true,
        max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
    }
}

struct Fixture {
    session: Session,
    chain: Arc<SimChain>,
}

async fn fixture(faults: Vec<ScriptedFault>) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.keep();
    fixture_at(root, faults).await
}

async fn fixture_at(root: std::path::PathBuf, faults: Vec<ScriptedFault>) -> Fixture {
    let chain = Arc::new(SimChain::new(GENESIS));
    let proxy = FaultProxy::shared(chain.clone(), faults);
    let config = bloom_solana_cli::profiles::ProfileConfig {
        name: "simnet".into(),
        family: "solana".into(),
        expected_genesis_hex: GENESIS.into(),
        http_endpoint: "unused".into(),
        allow_broadcast: true,
        max_fee_lamports: Some(100_000),
    };
    // The account must be enabled before the session (the session derives
    // its fixture signer from the first enabled account).
    let registry = AccountRegistry::open(&root).unwrap();
    registry
        .enable(
            "w1",
            "fixture-child-0",
            &session::fixture_public_key_hex("w1"),
            "simnet",
            "solana:simnet",
            1,
        )
        .unwrap();
    let approvals = Arc::new(ExactApprovalLedger::denying());
    let session = session::open(&root, config, approvals, Arc::new(proxy)).unwrap();
    // The machine profile name is the raw profile name.
    let _ = chain_profile();
    Fixture { session, chain }
}

fn request_for(session: &Session) -> TransferRequest {
    let account = session.accounts.get("w1", "simnet").unwrap();
    TransferRequest {
        operation_id: format!("{:0>64}", "a1"),
        wallet_id: "w1".into(),
        fee_payer_base58: account.address_base58.clone(),
        destination_base58: DESTINATION.into(),
        lamports: 1_000_000_000,
        key_ref: bloom_solana::adapter::FixtureKeyRef {
            backend: "local".into(),
            locator: account.key_ref_locator.clone(),
            public_key_hex: account.public_key_hex.clone(),
        },
        expires_at_ms: 0,
        max_fee_lamports: 100_000,
        claimed_caip2: "solana:simnet".into(),
    }
}

#[tokio::test]
async fn stage_then_confirm_signs_and_denial_leaves_it_unsigned() {
    let f = fixture(vec![]).await;
    let request = request_for(&f.session);

    // Stage.
    f.session
        .machine
        .stage_transfer(&request, 1_000)
        .await
        .unwrap();
    assert_eq!(
        f.session.machine.load_action(&request.operation_id).state,
        ActionState::Staged
    );

    // A confirm against the denying ledger fails and signs nothing: the
    // confirming session shares the staged outbox root.
    let root = f.session.state_root.clone();
    let denied = fixture_at(root.clone(), vec![]).await;
    assert!(
        confirm_operation(
            &denied.session.machine,
            &denied.session.accounts,
            &request.operation_id
        )
        .await
        .is_err()
    );
    assert_eq!(
        denied
            .session
            .machine
            .load_action(&request.operation_id)
            .state,
        ActionState::Staged
    );
    assert!(
        denied
            .session
            .machine
            .load_action(&request.operation_id)
            .artifact
            .is_none()
    );

    // Confirm on an approving machine over the same state: signs once.
    let approving = fixture_at(root, vec![]).await;
    // Replace its denying ledger by confirming through a machine with a
    // fresh ledger: confirm_operation uses the session's ledger.
    {
        let chain = Arc::new(SimChain::new(GENESIS));
        let proxy = FaultProxy::shared(chain.clone(), vec![]);
        let config = bloom_solana_cli::profiles::ProfileConfig {
            name: "simnet".into(),
            family: "solana".into(),
            expected_genesis_hex: GENESIS.into(),
            http_endpoint: "unused".into(),
            allow_broadcast: true,
            max_fee_lamports: Some(100_000),
        };
        let approvals = Arc::new(ExactApprovalLedger::new());
        let session = session::open(
            &approving.session.state_root,
            config,
            approvals,
            Arc::new(proxy),
        )
        .unwrap();
        confirm_operation(&session.machine, &session.accounts, &request.operation_id)
            .await
            .unwrap();
        assert_eq!(
            session.machine.load_action(&request.operation_id).state,
            ActionState::Signed
        );
    }
    let _ = f;
}

#[tokio::test]
async fn cancel_after_signing_is_refused() {
    let f = fixture(vec![]).await;
    let request = request_for(&f.session);
    f.session
        .machine
        .stage_transfer(&request, 1_000)
        .await
        .unwrap();
    // Approve+sign through a confirming machine on the same root.
    let root = f.session.state_root.clone();
    let chain = Arc::new(SimChain::new(GENESIS));
    let proxy = FaultProxy::shared(chain, vec![]);
    let config = bloom_solana_cli::profiles::ProfileConfig {
        name: "simnet".into(),
        family: "solana".into(),
        expected_genesis_hex: GENESIS.into(),
        http_endpoint: "unused".into(),
        allow_broadcast: true,
        max_fee_lamports: Some(100_000),
    };
    let session = session::open(
        &root,
        config,
        Arc::new(ExactApprovalLedger::new()),
        Arc::new(proxy),
    )
    .unwrap();
    confirm_operation(&session.machine, &session.accounts, &request.operation_id)
        .await
        .unwrap();
    // Cancellation/denial now fails closed.
    assert!(
        session
            .machine
            .cancel(&request.operation_id, 2_000)
            .is_err()
    );
}

#[tokio::test]
async fn retry_requires_ambiguous_and_reuses_identical_bytes() {
    let f = fixture(vec![ScriptedFault::on(
        "sendTransaction",
        Fault::TimeoutAfterSubmit,
    )])
    .await;
    let request = request_for(&f.session);
    let root = f.session.state_root.clone();
    f.session
        .machine
        .stage_transfer(&request, 1_000)
        .await
        .unwrap();

    let chain = Arc::new(SimChain::new(GENESIS));
    let proxy = FaultProxy::shared(
        chain.clone(),
        vec![ScriptedFault::on(
            "sendTransaction",
            Fault::TimeoutAfterSubmit,
        )],
    );
    let config = bloom_solana_cli::profiles::ProfileConfig {
        name: "simnet".into(),
        family: "solana".into(),
        expected_genesis_hex: GENESIS.into(),
        http_endpoint: "unused".into(),
        allow_broadcast: true,
        max_fee_lamports: Some(100_000),
    };
    let session = session::open(
        &root,
        config,
        Arc::new(ExactApprovalLedger::new()),
        Arc::new(proxy),
    )
    .unwrap();
    confirm_operation(&session.machine, &session.accounts, &request.operation_id)
        .await
        .unwrap();

    // Retry before any broadcast attempt is refused (state is Signed, not
    // Ambiguous).
    assert!(
        session
            .machine
            .retry_ambiguous(&request.operation_id, 1_500)
            .await
            .is_err()
    );

    // First broadcast times out after submit → ambiguous.
    session
        .machine
        .broadcast(&request.operation_id, 1_600)
        .await
        .unwrap();
    assert_eq!(
        session.machine.load_action(&request.operation_id).state,
        ActionState::Ambiguous
    );
    let first = session.machine.load_action(&request.operation_id);

    // A retry attempt cannot mutate the artifact: the journal pins it.
    let action = session.machine.load_action(&request.operation_id);
    let staged = staged_from_action(&action);
    let rebuilt = request_from_action(&session.accounts, &action).unwrap();
    assert_eq!(rebuilt.operation_id, first.envelope.operation_id);
    assert_eq!(staged.payload_digest_hex, first.envelope.payload_digest_hex);

    // Honest retry with identical bytes confirms.
    session
        .machine
        .retry_ambiguous(&request.operation_id, 1_700)
        .await
        .unwrap();
    let retried = session.machine.load_action(&request.operation_id);
    assert_eq!(retried.attempts.len(), 2);
    assert_eq!(
        retried.attempts[0].artifact_digest_hex,
        retried.attempts[1].artifact_digest_hex
    );

    let sig = bs58::encode(&retried.artifact.clone().unwrap().signature).into_string();
    chain.land(&sig);
    session
        .machine
        .reconcile(&request.operation_id, 1_800)
        .await
        .unwrap();
    assert_eq!(
        session.machine.load_action(&request.operation_id).state,
        ActionState::Confirmed
    );
}

#[tokio::test]
async fn stale_stage_is_refused_at_confirm() {
    let f = fixture(vec![]).await;
    let request = request_for(&f.session);
    f.session
        .machine
        .stage_transfer(&request, 1_000)
        .await
        .unwrap();

    // The validity window closes before confirm.
    f.chain.advance(SimChain::VALIDITY + 10);

    let chain = Arc::new(SimChain::new(GENESIS));
    chain.advance(SimChain::VALIDITY + 10);
    let proxy = FaultProxy::shared(chain, vec![]);
    let config = bloom_solana_cli::profiles::ProfileConfig {
        name: "simnet".into(),
        family: "solana".into(),
        expected_genesis_hex: GENESIS.into(),
        http_endpoint: "unused".into(),
        allow_broadcast: true,
        max_fee_lamports: Some(100_000),
    };
    let session = session::open(
        &f.session.state_root,
        config,
        Arc::new(ExactApprovalLedger::new()),
        Arc::new(proxy),
    )
    .unwrap();
    assert!(
        confirm_operation(&session.machine, &session.accounts, &request.operation_id)
            .await
            .is_err(),
        "stale stage must refuse to sign"
    );
}

#[tokio::test]
async fn fee_null_and_skew_at_confirm_refuse_before_signing() {
    for fault in [
        || Fault::FeeNull,
        || Fault::FeeSkew {
            extra_lamports: 500,
        },
    ] {
        let f = fixture(vec![]).await;
        let request = request_for(&f.session);
        f.session
            .machine
            .stage_transfer(&request, 1_000)
            .await
            .unwrap();

        let chain = Arc::new(SimChain::new(GENESIS));
        let proxy = FaultProxy::shared(
            chain,
            // The first getFeeForMessage call after stage is confirm's.
            vec![ScriptedFault::on("getFeeForMessage", fault())],
        );
        let config = bloom_solana_cli::profiles::ProfileConfig {
            name: "simnet".into(),
            family: "solana".into(),
            expected_genesis_hex: GENESIS.into(),
            http_endpoint: "unused".into(),
            allow_broadcast: true,
            max_fee_lamports: Some(100_000),
        };
        let session = session::open(
            &f.session.state_root,
            config,
            Arc::new(ExactApprovalLedger::new()),
            Arc::new(proxy),
        )
        .unwrap();
        assert!(
            confirm_operation(&session.machine, &session.accounts, &request.operation_id)
                .await
                .is_err(),
            "faulted fee quote must refuse"
        );
        assert!(
            session
                .machine
                .load_action(&request.operation_id)
                .artifact
                .is_none()
        );
    }
}

#[tokio::test]
async fn restart_reopens_state_and_projection_is_stable() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let request_id;
    {
        let f = fixture_at(root.clone(), vec![]).await;
        let request = request_for(&f.session);
        request_id = request.operation_id.clone();
        f.session
            .machine
            .stage_transfer(&request, 1_000)
            .await
            .unwrap();
    }
    // "Restart": a fresh process over the same state root.
    let f2 = fixture_at(root, vec![]).await;
    let action = f2.session.machine.load_action(&request_id);
    assert_eq!(action.state, ActionState::Staged);
    let projection = f2.session.machine.project(&request_id, 2_000).unwrap();
    assert_eq!(projection.verified.lamports, 1_000_000_000);
    assert_eq!(projection.asserted.fee_lamports, 5_000);
}

#[tokio::test]
async fn wrong_cluster_fails_every_lifecycle_call() {
    // The transport speaks a different cluster than the profile pins.
    let chain = Arc::new(SimChain::new("some-other-genesis"));
    let proxy = FaultProxy::shared(chain.clone(), vec![]);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let registry = AccountRegistry::open(&root).unwrap();
    registry
        .enable(
            "w1",
            "fixture-child-0",
            &session::fixture_public_key_hex("w1"),
            "simnet",
            "solana:simnet",
            1,
        )
        .unwrap();
    let config = bloom_solana_cli::profiles::ProfileConfig {
        name: "simnet".into(),
        family: "solana".into(),
        expected_genesis_hex: GENESIS.into(),
        http_endpoint: "unused".into(),
        allow_broadcast: true,
        max_fee_lamports: Some(100_000),
    };
    let session = session::open(
        &root,
        config,
        Arc::new(ExactApprovalLedger::denying()),
        Arc::new(proxy),
    )
    .unwrap();
    let request = request_for(&session);
    assert!(
        session
            .machine
            .stage_transfer(&request, 1_000)
            .await
            .is_err()
    );
}

#[test]
fn fixture_signer_identity_matches_registered_key() {
    use ed25519_dalek::SigningKey;
    let key = SigningKey::from_bytes(&{
        let mut hasher = sha2::Sha256::new();
        sha2::Digest::update(&mut hasher, b"bloom-solana-fixture");
        sha2::Digest::update(&mut hasher, b"w1");
        let out: [u8; 32] = sha2::Digest::finalize(hasher).into();
        out
    });
    assert_eq!(
        hex::encode(key.verifying_key().to_bytes()),
        session::fixture_public_key_hex("w1")
    );
}
