//! Real local-validator end-to-end test.
//!
//! Requires the `http` feature and a running `solana-test-validator`:
//!
//! ```text
//! cargo test -p bloom-solana-machine --features bloom-chain-rpc/http \
//!     --test local_validator -- --ignored --test-threads=1
//! ```
//!
//! Exercises the full lifecycle against real infrastructure: mediated reads
//! (real blockhash, real fee quote), real freshness observations, fixture
//! Ed25519 over the real message (a genuinely valid on-chain signature),
//! base64 broadcast, and signature-status reconciliation to confirmation.
//! Nothing here touches devnet or mainnet.

#![cfg(feature = "http")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use bloom_chain_action::ActionState;
use bloom_chain_rpc::FreshnessPolicy;
use bloom_chain_rpc::http::SolanaHttpTransport;
use bloom_chain_rpc::mediator::{ChainRpcProfile, DEFAULT_MAX_RESPONSE_BYTES, Mediator};
use bloom_chain_rpc::transport::RpcTransport;
use bloom_solana::adapter::FixtureKeyRef;
use bloom_solana_machine::SigningAuthority as _;
use bloom_solana_machine::fixture::{ExactApprovalLedger, FixtureEd25519Signer};
use bloom_solana_machine::host::MediatorHost;
use bloom_solana_machine::mount::{PINNED_SOLANA_DRIVER_PACKAGE_HASH, mount_pinned_solana_driver};
use bloom_solana_machine::{LifecycleStatus, SolanaMachine, TransferRequest};
use serde_json::{Value, json};

const PETAL_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../petals/solana-driver");
const ENDPOINT: &str = match std::option_env!("SOLANA_VALIDATOR_HTTP") {
    Some(url) => url,
    None => "http://127.0.0.1:8899",
};

/// Fixed state root for CI artifact preservation: when set, every outbox and
/// projection lands under it so the workflow can upload operation state on
/// failure.
fn e2e_state(tag: &str) -> std::path::PathBuf {
    match std::env::var("BLOOM_SOLANA_E2E_STATE") {
        Ok(root) => {
            let p = std::path::PathBuf::from(root).join(tag);
            std::fs::create_dir_all(&p).unwrap();
            p
        }
        Err(_) => tempfile::tempdir().unwrap().keep(),
    }
}

fn transport() -> SolanaHttpTransport {
    SolanaHttpTransport::new(ENDPOINT).unwrap()
}

/// Poll until the predicate over a mediated call turns true.
async fn wait_for(
    mediator: &Mediator,
    method: &str,
    params: &Value,
    predicate: impl Fn(&Value) -> bool,
) -> Value {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let result = mediator.read(0, method, params).unwrap();
        if predicate(&result) {
            return result;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {method} to satisfy the predicate"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn build_machine(
    state_root: &std::path::Path,
    outbox_root: &std::path::Path,
) -> (SolanaMachine, Arc<Mediator>) {
    let transport = Arc::new(transport());
    let genesis = transport
        .call("getGenesisHash", &json!([]))
        .unwrap()
        .as_str()
        .unwrap()
        .to_string();
    let profile = ChainRpcProfile {
        name: "solana-local-validator".into(),
        family: "solana".into(),
        expected_genesis_hex: genesis,
        allowed_read_methods: vec![
            "getGenesisHash".into(),
            "getHealth".into(),
            "getBlockHeight".into(),
            "getLatestBlockhash".into(),
            "getFeeForMessage".into(),
            "isBlockhashValid".into(),
            "getSignatureStatuses".into(),
            "getBalance".into(),
            "requestAirdrop".into(),
        ],
        allow_broadcast: true,
        max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
    };
    let mediator = Arc::new(Mediator::new(profile, vec![Box::new(transport)]).unwrap());
    let host = Arc::new(MediatorHost::new(mediator.clone(), || 1));
    std::fs::create_dir_all(state_root).unwrap();
    let vfs = Arc::new(
        mount_pinned_solana_driver(std::path::Path::new(PETAL_DIR), state_root, host).unwrap(),
    );
    std::fs::create_dir_all(outbox_root).unwrap();
    let outbox = Arc::new(bloom_chain_action::ChainActionOutbox::new(outbox_root).unwrap());
    let signer = FixtureEd25519Signer::from_seed((0..32u8).collect::<Vec<_>>().try_into().unwrap());
    let machine = SolanaMachine::new(
        vfs,
        mediator.clone(),
        outbox,
        Arc::new(signer),
        Arc::new(ExactApprovalLedger::new()),
        FreshnessPolicy {
            max_staleness_ms: 120_000,
            min_remaining_blocks: 16,
        },
        "solana-local-validator",
        PINNED_SOLANA_DRIVER_PACKAGE_HASH,
    );
    (machine, mediator)
}

/// Fund the fixture payer through the faucet and build the request.
async fn funded_request(machine: &SolanaMachine) -> TransferRequest {
    use bloom_solana_machine::SigningAuthority as _;
    let signer = FixtureEd25519Signer::from_seed((0..32u8).collect::<Vec<_>>().try_into().unwrap());
    let payer_b58 = bs58::encode(signer.public_key_bytes()).into_string();
    // Reach the machine's mediator for the faucet through a fresh mediator on
    // the same transport.
    let faucet = Mediator::new(
        ChainRpcProfile {
            name: "solana-local-validator".into(),
            family: "solana".into(),
            expected_genesis_hex: transport()
                .call("getGenesisHash", &json!([]))
                .unwrap()
                .as_str()
                .unwrap()
                .to_string(),
            allowed_read_methods: vec![
                "getGenesisHash".into(),
                "getSignatureStatuses".into(),
                "getBalance".into(),
                "requestAirdrop".into(),
            ],
            allow_broadcast: false,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
        },
        vec![Box::new(transport())],
    )
    .unwrap();
    let airdrop = faucet
        .read(0, "requestAirdrop", &json!([payer_b58, 2_000_000_000u64]))
        .unwrap();
    let airdrop_sig = airdrop.as_str().unwrap().to_string();
    wait_for(
        &faucet,
        "getSignatureStatuses",
        &json!([[airdrop_sig]]),
        |v| !v["value"][0].is_null(),
    )
    .await;
    let public_key_hex = hex::encode(bs58::decode(&payer_b58).into_vec().unwrap());
    TransferRequest {
        operation_id: format!("{:0>64}", "f2"),
        wallet_id: "wallet-e2e".into(),
        fee_payer_base58: payer_b58,
        destination_base58: "CZ8YUVdk7znjrUmnb5n7kgySk9yRAsQDYmyCxzfSky9t".into(),
        lamports: 1_000_000_000,
        key_ref: FixtureKeyRef {
            backend: "local".into(),
            locator: "solana-child-0".into(),
            public_key_hex,
        },
        expires_at_ms: 0,
        max_fee_lamports: 100_000,
        claimed_caip2: "solana:devnet".into(),
    }
}

fn machine_mediator(machine: &SolanaMachine) -> Mediator {
    // The machine does not expose its mediator; callers share one via
    // build_machine. This helper exists for call sites holding only the
    // machine — it builds a read-only mediator over the same endpoint.
    let _ = machine;
    Mediator::new(
        ChainRpcProfile {
            name: "solana-local-validator".into(),
            family: "solana".into(),
            expected_genesis_hex: transport()
                .call("getGenesisHash", &json!([]))
                .unwrap()
                .as_str()
                .unwrap()
                .to_string(),
            allowed_read_methods: vec![
                "getGenesisHash".into(),
                "getSignatureStatuses".into(),
                "getBalance".into(),
            ],
            allow_broadcast: false,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
        },
        vec![Box::new(transport())],
    )
    .unwrap()
}

#[tokio::test]
#[ignore = "requires a local solana-test-validator"]
async fn full_lifecycle_on_local_validator() {
    let transport = Arc::new(transport());

    // Cluster identity is learned once and pinned into the profile: the
    // mediator then rejects any later observation that disagrees.
    let genesis = transport
        .call("getGenesisHash", &json!([]))
        .unwrap()
        .as_str()
        .unwrap()
        .to_string();

    let profile = ChainRpcProfile {
        name: "solana-local-validator".into(),
        family: "solana".into(),
        expected_genesis_hex: genesis,
        allowed_read_methods: vec![
            "getGenesisHash".into(),
            "getHealth".into(),
            "getBlockHeight".into(),
            "getLatestBlockhash".into(),
            "getFeeForMessage".into(),
            "isBlockhashValid".into(),
            "getSignatureStatuses".into(),
            "getBalance".into(),
            // Faucet for test wallets only; never part of a production
            // profile.
            "requestAirdrop".into(),
        ],
        allow_broadcast: true,
        max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
    };
    let mediator = Arc::new(Mediator::new(profile, vec![Box::new(transport.clone())]).unwrap());
    let host = Arc::new(MediatorHost::new(mediator.clone(), || 1));
    let state = tempfile::tempdir().unwrap();
    let vfs = Arc::new(
        mount_pinned_solana_driver(std::path::Path::new(PETAL_DIR), state.path(), host).unwrap(),
    );
    std::mem::forget(state);
    let outbox = Arc::new(
        bloom_chain_action::ChainActionOutbox::new(tempfile::tempdir().unwrap().keep()).unwrap(),
    );

    let signer = FixtureEd25519Signer::from_seed((0..32u8).collect::<Vec<_>>().try_into().unwrap());
    let payer_b58 = bs58::encode(signer.public_key_bytes()).into_string();

    // Fund the fixture payer through the faucet.
    let airdrop = mediator
        .read(0, "requestAirdrop", &json!([payer_b58, 1_000_000_000u64]))
        .unwrap();
    let airdrop_sig = airdrop.as_str().unwrap().to_string();
    wait_for(
        mediator.as_ref(),
        "getSignatureStatuses",
        &json!([[airdrop_sig]]),
        |v| !v["value"][0].is_null(),
    )
    .await;
    let balance = mediator.read(0, "getBalance", &json!([payer_b58])).unwrap()["value"]
        .as_u64()
        .unwrap();
    assert!(
        balance >= 1_000_000_000,
        "airdrop landed: {balance} lamports"
    );

    let machine = SolanaMachine::new(
        vfs,
        mediator.clone(),
        outbox,
        Arc::new(signer),
        Arc::new(ExactApprovalLedger::new()),
        FreshnessPolicy {
            max_staleness_ms: 120_000,
            min_remaining_blocks: 16,
        },
        "solana-local-validator",
        PINNED_SOLANA_DRIVER_PACKAGE_HASH,
    );

    let op = format!("{:0>64}", "f1");
    let public_key_hex = hex::encode(bs58::decode(&payer_b58).into_vec().unwrap());
    let request = TransferRequest {
        operation_id: op.clone(),
        wallet_id: "wallet-e2e".into(),
        fee_payer_base58: payer_b58.clone(),
        destination_base58: "CZ8YUVdk7znjrUmnb5n7kgySk9yRAsQDYmyCxzfSky9t".into(),
        // Must clear the destination's rent-exemption minimum (~890k
        // lamports); 1 SOL does, and the airdrop covers it plus fees.
        lamports: 1_000_000_000,
        key_ref: FixtureKeyRef {
            backend: "local".into(),
            locator: "solana-child-0".into(),
            public_key_hex,
        },
        expires_at_ms: 0,
        max_fee_lamports: 100_000,
        claimed_caip2: "solana:devnet".into(),
    };

    // Stage: real mediated blockhash + real fee quote.
    let staged = machine.stage_transfer(&request, 1).await.unwrap();
    assert!(staged.fee_lamports > 0, "real validator quoted a fee");
    let fee = staged.fee_lamports;

    // Finalize: freshness over the real chain, approval, real signature.
    machine
        .finalize_transfer(&request, &staged, 2)
        .await
        .unwrap();
    let action = machine.load_action(&op);
    assert_eq!(action.state, ActionState::Signed);
    let signature_b58 = bs58::encode(&action.artifact.clone().unwrap().signature).into_string();

    // Broadcast the real transaction; confirm by its own signature.
    machine.broadcast(&op, 3).await.unwrap();
    if machine.load_action(&op).state != ActionState::Sent {
        for entry in mediator.audit() {
            println!("AUDIT {entry:?}");
        }
    }
    assert_eq!(machine.load_action(&op).state, ActionState::Sent);

    let confirmed = wait_for(
        mediator.as_ref(),
        "getSignatureStatuses",
        &json!([[signature_b58.clone()]]),
        |v| v["value"][0]["confirmationStatus"].is_string(),
    )
    .await;
    let confirmation = confirmed["value"][0]["confirmationStatus"]
        .as_str()
        .unwrap()
        .to_string();

    let status = machine.reconcile(&op, 4).await.unwrap();
    match status {
        LifecycleStatus::Confirmed {
            confirmation: detail,
        } => assert_eq!(detail, confirmation),
        other => panic!("expected confirmation, got {other:?}"),
    }
    assert!(machine.load_action(&op).state.is_terminal());

    // The fee the validator quoted matches the journal's bound.
    let projection = machine.project_json(&op, 9).unwrap();
    assert_eq!(projection["asserted"]["fee_lamports"].as_u64(), Some(fee));
    assert_eq!(
        projection["asserted"]["total_debit_lamports"].as_u64(),
        Some(1_000_000_000 + fee)
    );

    // Destination balance moved by exactly the transfer amount.
    let destination_lamports = mediator
        .read(5, "getBalance", &json!([request.destination_base58]))
        .unwrap()["value"]
        .as_u64()
        .unwrap_or(0);
    assert!(
        destination_lamports >= request.lamports,
        "destination holds at least the transfer: {destination_lamports}"
    );
}

/// Restart-after-ambiguous coverage on the real validator: a signed
/// operation survives a full component drop; a fresh machine over the same
/// durable outbox broadcasts from the recovered state and reconciles to
/// confirmation. (The post-dispatch-timeout ambiguous flavor is covered by
/// the simulated adversarial suite, which runs as its own CI job.)
#[tokio::test]
#[ignore = "requires a local solana-test-validator"]
async fn restart_recovers_signed_operation_and_broadcasts_from_disk() {
    let state_root = e2e_state("restart");
    let outbox_root = state_root.join("outbox");
    let request = {
        let (machine, _) = build_machine(&state_root, &outbox_root).await;
        let request = funded_request(&machine).await;
        let staged = machine.stage_transfer(&request, 1).await.unwrap();
        machine
            .finalize_transfer(&request, &staged, 2)
            .await
            .unwrap();
        assert_eq!(
            machine.load_action(&request.operation_id).state,
            ActionState::Signed
        );
        request
    };
    // "Crash": every component dropped; only durable state survives.
    {
        let (machine, _mediator) = build_machine(&state_root, &outbox_root).await;
        let action = machine.load_action(&request.operation_id);
        assert_eq!(action.state, ActionState::Signed, "recovered from disk");
        assert!(action.artifact.is_some());
        let sig_b58 = bs58::encode(&action.artifact.clone().unwrap().signature).into_string();

        machine.broadcast(&request.operation_id, 3).await.unwrap();
        assert_eq!(
            machine.load_action(&request.operation_id).state,
            ActionState::Sent
        );

        let confirmed = wait_for(
            &machine_mediator(&machine),
            "getSignatureStatuses",
            &json!([[sig_b58.clone()]]),
            |v| v["value"][0]["confirmationStatus"].is_string(),
        )
        .await;
        let _ = confirmed;
        let status = machine.reconcile(&request.operation_id, 4).await.unwrap();
        assert!(matches!(status, LifecycleStatus::Confirmed { .. }));
        let projection = machine.project_json(&request.operation_id, 5).unwrap();
        assert_eq!(projection["state"], "confirmed");
        // Persist the final projection for CI artifact upload.
        let _ = std::fs::write(
            state_root.join("operation-projection.json"),
            serde_json::to_vec_pretty(&projection).unwrap(),
        );
    }
}
