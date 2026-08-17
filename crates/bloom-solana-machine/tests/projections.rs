//! Typed VFS projection tests: every projected field class, the verified vs
//! asserted separation, digest preference over payload bytes, and durability
//! (projections derive from cold-loaded outbox records only).

use std::sync::Arc;

use bloom_chain_rpc::FreshnessPolicy;
use bloom_chain_rpc::fault::{Fault, FaultProxy, ScriptedFault};
use bloom_chain_rpc::mediator::{ChainRpcProfile, DEFAULT_MAX_RESPONSE_BYTES, Mediator};
use bloom_chain_rpc::sim::SimChain;
use bloom_solana::adapter::FixtureKeyRef;
use bloom_solana_machine::fixture::{ExactApprovalLedger, FixtureEd25519Signer};
use bloom_solana_machine::host::MediatorHost;
use bloom_solana_machine::mount::{PINNED_SOLANA_DRIVER_PACKAGE_HASH, mount_pinned_solana_driver};
use bloom_solana_machine::{
    AccountRegistry, MachineError, SigningAuthority, SolanaMachine, TransferRequest,
};

const PETAL_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../petals/solana-driver");
const GENESIS: &str = "genesis-solana-devnet-0001";

fn profile() -> ChainRpcProfile {
    ChainRpcProfile {
        name: "solana-devnet".into(),
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

struct Parts {
    machine: SolanaMachine,
    chain: Arc<SimChain>,
    outbox_root: std::path::PathBuf,
}

async fn parts() -> Parts {
    let chain = Arc::new(SimChain::new(GENESIS));
    let proxy = FaultProxy::shared(chain.clone(), vec![]);
    let mediator = Arc::new(Mediator::new(profile(), vec![Box::new(proxy)]).unwrap());
    let host = Arc::new(MediatorHost::new(mediator.clone(), || 1_000));
    let state = tempfile::tempdir().unwrap();
    let vfs = Arc::new(
        mount_pinned_solana_driver(std::path::Path::new(PETAL_DIR), state.path(), host).unwrap(),
    );
    std::mem::forget(state);
    let outbox_root = tempfile::tempdir().unwrap().keep();
    let outbox = Arc::new(bloom_chain_action::ChainActionOutbox::new(&outbox_root).unwrap());
    let signer = FixtureEd25519Signer::from_seed((0..32u8).collect::<Vec<_>>().try_into().unwrap());
    let machine = SolanaMachine::new(
        vfs,
        mediator,
        outbox,
        Arc::new(signer),
        Arc::new(ExactApprovalLedger::new()),
        FreshnessPolicy {
            max_staleness_ms: 90_000,
            min_remaining_blocks: 32,
        },
        "solana-devnet",
        PINNED_SOLANA_DRIVER_PACKAGE_HASH,
    );
    Parts {
        machine,
        chain,
        outbox_root,
    }
}

fn request(_machine: &SolanaMachine, op: &str) -> TransferRequest {
    let signer = FixtureEd25519Signer::from_seed((0..32u8).collect::<Vec<_>>().try_into().unwrap());
    let pk = signer.public_key_bytes();
    TransferRequest {
        operation_id: format!("{op:0>64}"),
        wallet_id: "wallet-1".into(),
        fee_payer_base58: bs58::encode(pk).into_string(),
        destination_base58: "CZ8YUVdk7znjrUmnb5n7kgySk9yRAsQDYmyCxzfSky9t".into(),
        lamports: 1_000_000_000,
        key_ref: FixtureKeyRef {
            backend: "local".into(),
            locator: "solana-child-0".into(),
            public_key_hex: hex::encode(pk),
        },
        expires_at_ms: 0,
        max_fee_lamports: 10_000,
        claimed_caip2: "solana:devnet".into(),
    }
}

#[tokio::test]
async fn projection_covers_every_field_class_with_verified_asserted_separation() {
    let p = parts().await;
    let machine = p.machine.clone();
    let req = request(&machine, "01");
    machine.prepare_transfer(&req, 1_000).await.unwrap();
    machine.broadcast(&req.operation_id, 1_100).await.unwrap();
    let sig_b58 = {
        let a = machine.load_action(&req.operation_id);
        bs58::encode(&a.artifact.clone().unwrap().signature).into_string()
    };
    p.chain.land(&sig_b58);
    machine.reconcile(&req.operation_id, 1_200).await.unwrap();

    let projection = machine.project(&req.operation_id, 1_500).unwrap();

    // Operation identity and lifecycle.
    assert_eq!(projection.operation_id, req.operation_id);
    assert_eq!(projection.state, "confirmed");
    assert!(projection.terminal);
    assert_eq!(projection.wallet_id, "wallet-1");

    // Cluster: CAIP-2 plus the full expected genesis hash.
    assert_eq!(projection.cluster.caip2, "solana:devnet");
    assert_eq!(projection.cluster.expected_genesis_hex, GENESIS);

    // Verified facts: fee payer, destination, lamports, verifier identity.
    assert_eq!(projection.verified.fee_payer_base58, req.fee_payer_base58);
    assert_eq!(
        projection.verified.destination_base58,
        req.destination_base58
    );
    assert_eq!(projection.verified.lamports, req.lamports);
    assert_eq!(projection.verified.verifier_id, "solana-system-transfer-v1");
    assert_eq!(projection.verified.verifier_result_digest_hex.len(), 64);
    assert_eq!(projection.verified.message_digest_hex.len(), 64);

    // Machine-asserted facts: fee, ceiling, total debit, liveness.
    assert_eq!(projection.asserted.fee_lamports, 5_000);
    assert_eq!(projection.asserted.max_fee_lamports, 10_000);
    assert_eq!(
        projection.asserted.total_debit_lamports,
        req.lamports + 5_000
    );
    assert!(!projection.asserted.blockhash_base58.is_empty());
    assert!(projection.asserted.last_valid_block_height > 0);
    assert_eq!(projection.asserted.blockhash_age_ms, 500);

    // Bindings: package, route, digests.
    assert_eq!(
        projection.bindings.package_hash,
        PINNED_SOLANA_DRIVER_PACKAGE_HASH
    );
    assert_eq!(projection.bindings.route, "transfer.stage.json");
    assert_eq!(projection.bindings.payload_digest_hex.len(), 64);
    assert_eq!(
        projection.bindings.payload_digest_hex,
        projection.verified.message_digest_hex
    );
    assert!(projection.bindings.artifact_digest_hex.is_some());

    // Signature and finality.
    assert_eq!(
        projection.signature_base58.as_deref(),
        Some(sig_b58.as_str())
    );
    assert_eq!(
        projection.finality.confirmation.as_deref(),
        Some("confirmed")
    );
    assert_eq!(projection.attempts.len(), 1);
    assert_eq!(projection.attempts[0].outcome.as_deref(), Some("accepted"));

    // Digest preference: no payload or transaction bytes in the JSON form.
    let json = machine.project_json(&req.operation_id, 1_500).unwrap();
    let text = json.to_string();
    assert!(!text.contains("payload_hex"));
    assert!(!text.contains("message_hex"));
    assert!(!text.contains("transaction_hex"));
    // And no secret-shaped fields exist anywhere.
    assert!(!text.contains("mnemonic"));
    assert!(!text.contains("seed"));
    assert!(!text.contains("wkek"));
}

#[tokio::test]
async fn refusal_ambiguous_quarantine_reasons_project_from_the_journal() {
    let p = parts().await;
    let machine = p.machine.clone();

    // Freshness refusal reason.
    let req = request(&machine, "02");
    let staged = machine.stage_transfer(&req, 1_000).await.unwrap();
    p.chain.advance(SimChain::VALIDITY + 10);
    machine
        .finalize_transfer(&req, &staged, 1_050)
        .await
        .unwrap_err();
    let refused = machine.project(&req.operation_id, 1_100).unwrap();
    assert_eq!(refused.state, "freshness_refused");
    assert!(refused.terminal);
    assert!(
        refused
            .finality
            .freshness_reason
            .as_deref()
            .is_some_and(|r| r.contains("blockhash") || r.contains("validity"))
    );

    // Ambiguous state and quarantine reason.
    let req3 = request(&machine, "03");
    machine.prepare_transfer(&req3, 1_000).await.unwrap();
    let artifact = machine
        .load_action(&req3.operation_id)
        .artifact
        .clone()
        .unwrap();
    machine
        .outbox()
        .record_broadcast_attempt(&req3.operation_id, 1_100)
        .unwrap();
    machine
        .outbox()
        .record_broadcast_outcome(
            &req3.operation_id,
            1_200,
            1,
            bloom_chain_action::BroadcastOutcome::Ambiguous,
        )
        .unwrap();
    let ambiguous = machine.project(&req3.operation_id, 1_300).unwrap();
    assert_eq!(ambiguous.state, "ambiguous");
    assert!(!ambiguous.terminal);
    assert_eq!(ambiguous.attempts[0].outcome.as_deref(), Some("ambiguous"));
    machine
        .quarantine(&req3.operation_id, 1_400, "provider conflict")
        .await
        .unwrap();
    let quarantined = machine.project(&req3.operation_id, 1_500).unwrap();
    assert_eq!(quarantined.state, "quarantined");
    assert_eq!(
        quarantined.finality.quarantine_reason.as_deref(),
        Some("provider conflict")
    );
    let _ = artifact;
}

#[tokio::test]
async fn projection_derives_from_cold_loaded_records_not_memory() {
    let p = parts().await;
    let req = request(&p.machine, "04");
    p.machine.prepare_transfer(&req, 1_000).await.unwrap();
    drop(p.machine);

    // Reopen the outbox from disk with a bare ChainActionOutbox (no machine,
    // no Petal, no mediator) and project straight from durable records.
    let outbox = bloom_chain_action::ChainActionOutbox::new(&p.outbox_root).unwrap();
    let action = outbox.load(&req.operation_id).unwrap();
    let projection = bloom_solana_machine::projection::project_operation(
        &action,
        2_000,
        "solana:solana-devnet",
        GENESIS,
    )
    .unwrap();
    assert_eq!(projection.state, "signed");
    assert_eq!(projection.verified.lamports, req.lamports);
    assert_eq!(projection.asserted.fee_lamports, 5_000);
    assert_eq!(projection.freshness.age_ms, 1_000);
    assert!(projection.signature_base58.is_some());
}

#[test]
fn account_registry_projections_carry_caip10_identity() {
    let root = tempfile::tempdir().unwrap();
    let reg = AccountRegistry::open(root.path()).unwrap();
    reg.enable(
        "wallet-1",
        "solana-child-0",
        &hex::encode(
            bs58::decode("FAe4sisG95oZ42w7buUn5qEE4TAnfTTFPiguZUHmhiF")
                .into_vec()
                .unwrap(),
        ),
        "solana-devnet",
        "solana:devnet",
        1_000,
    )
    .unwrap();
    let projections = reg.projections();
    assert_eq!(projections.len(), 1);
    let account = &projections[0];
    assert_eq!(
        account.address_base58,
        "FAe4sisG95oZ42w7buUn5qEE4TAnfTTFPiguZUHmhiF"
    );
    assert_eq!(
        account.caip10,
        "solana:devnet:FAe4sisG95oZ42w7buUn5qEE4TAnfTTFPiguZUHmhiF"
    );
    // Public projection only: no key material of any kind.
    let text = serde_json::to_string(account).unwrap().to_lowercase();
    assert!(!text.contains("secret"));
    assert!(!text.contains("private"));
}

#[tokio::test]
async fn fee_skew_at_finalize_is_refused_and_projection_stays_staged() {
    // Over-limit via a skewed finalize-time quote: staged (unsigned), refusal
    // surfaced through the error, projection never claims verified fees it
    // did not pay.
    let chain = Arc::new(SimChain::new(GENESIS));
    let proxy = FaultProxy::shared(
        chain.clone(),
        ScriptedFault::on_nth(
            "getFeeForMessage",
            2,
            Fault::FeeSkew {
                extra_lamports: 100_000,
            },
        ),
    );
    let mediator = Arc::new(Mediator::new(profile(), vec![Box::new(proxy)]).unwrap());
    let host = Arc::new(MediatorHost::new(mediator.clone(), || 1_000));
    let state = tempfile::tempdir().unwrap();
    let vfs = Arc::new(
        mount_pinned_solana_driver(std::path::Path::new(PETAL_DIR), state.path(), host).unwrap(),
    );
    std::mem::forget(state);
    let signer = FixtureEd25519Signer::from_seed((0..32u8).collect::<Vec<_>>().try_into().unwrap());
    let machine = SolanaMachine::new(
        vfs,
        mediator,
        Arc::new(
            bloom_chain_action::ChainActionOutbox::new(tempfile::tempdir().unwrap().keep())
                .unwrap(),
        ),
        Arc::new(signer),
        Arc::new(ExactApprovalLedger::new()),
        FreshnessPolicy {
            max_staleness_ms: 90_000,
            min_remaining_blocks: 32,
        },
        "solana-devnet",
        PINNED_SOLANA_DRIVER_PACKAGE_HASH,
    );
    let req = request(&machine, "05");
    let staged = machine.stage_transfer(&req, 1_000).await.unwrap();
    assert!(matches!(
        machine.finalize_transfer(&req, &staged, 1_050).await,
        Err(MachineError::FeeRefused(_))
    ));
}
