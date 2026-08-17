//! End-to-end Machine lifecycle tests: every step runs through the real
//! pinned WASM Petal component, the real Wasmtime host path, the mediated
//! RPC stack, and the durable outbox. Signing is fixture Ed25519 (real
//! signatures over raw bytes) until the BIP-39 Signer edge lands.

use std::sync::Arc;

use async_trait::async_trait;
use bloom_chain_action::ActionState;
use bloom_chain_rpc::FreshnessPolicy;
use bloom_chain_rpc::fault::{Fault, FaultProxy, ScriptedFault};
use bloom_chain_rpc::mediator::{ChainRpcProfile, DEFAULT_MAX_RESPONSE_BYTES, Mediator};
use bloom_chain_rpc::sim::SimChain;
use bloom_solana::adapter::FixtureKeyRef;
use bloom_solana_machine::fixture::{ExactApprovalLedger, FixtureEd25519Signer};
use bloom_solana_machine::host::MediatorHost;
use bloom_solana_machine::mount::{PINNED_SOLANA_DRIVER_PACKAGE_HASH, mount_pinned_solana_driver};
use bloom_solana_machine::{
    ApprovalAuthority, ExactApprovalFacts, LifecycleStatus, MachineError, SigningAuthority,
    SolanaMachine, TransferRequest,
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
            "getHealth".into(),
            "getBlockHeight".into(),
            "getLatestBlockhash".into(),
            "isBlockhashValid".into(),
            "getSignatureStatuses".into(),
        ],
        allow_broadcast: true,
        max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
    }
}

fn policy() -> FreshnessPolicy {
    FreshnessPolicy {
        max_staleness_ms: 90_000,
        min_remaining_blocks: 32,
    }
}

struct MachineParts {
    machine: SolanaMachine,
    chain: Arc<SimChain>,
    ledger: Arc<ExactApprovalLedger>,
}

fn signer() -> FixtureEd25519Signer {
    FixtureEd25519Signer::from_seed((0..32u8).collect::<Vec<_>>().try_into().unwrap())
}

async fn machine_with(chain: SimChain, faults: Vec<ScriptedFault>) -> MachineParts {
    // One shared chain: the proxy mediates every call, tests control state
    // (advancing, landing) through the same instance.
    let chain = Arc::new(chain);
    let proxy = FaultProxy::shared(chain.clone(), faults);
    let mediator = Arc::new(Mediator::new(profile(), vec![Box::new(proxy)]).unwrap());
    let host = Arc::new(MediatorHost::new(mediator.clone(), || 1_000));
    let state = tempfile::tempdir().unwrap();
    let vfs = Arc::new(
        mount_pinned_solana_driver(std::path::Path::new(PETAL_DIR), state.path(), host).unwrap(),
    );
    std::mem::forget(state);
    let outbox = Arc::new(
        bloom_chain_action::ChainActionOutbox::new(tempfile::tempdir().unwrap().keep()).unwrap(),
    );
    let signer = Arc::new(signer());
    let ledger = Arc::new(ExactApprovalLedger::new());
    let machine = SolanaMachine::new(
        vfs,
        mediator,
        outbox,
        signer,
        ledger.clone(),
        policy(),
        "solana-devnet",
        PINNED_SOLANA_DRIVER_PACKAGE_HASH,
    );
    MachineParts {
        machine,
        chain,
        ledger,
    }
}

fn request(machine: &SolanaMachine, op: &str) -> TransferRequest {
    // The destination is the golden vector's; the fee payer is the fixture
    // signer's derived key, discovered from the machine's signer via the
    // public key through a fresh fixture instance (same seed).
    let s = signer();
    let pk = s.public_key_bytes();
    let fee_payer = bs58::encode(pk).into_string();
    let _ = machine;
    TransferRequest {
        operation_id: format!("{op:0>64}"),
        wallet_id: "wallet-1".into(),
        fee_payer_base58: fee_payer,
        destination_base58: "CZ8YUVdk7znjrUmnb5n7kgySk9yRAsQDYmyCxzfSky9t".into(),
        lamports: 1_000_000_000,
        key_ref: FixtureKeyRef {
            backend: "local".into(),
            locator: "solana-child-0".into(),
            public_key_hex: hex::encode(pk),
        },
        expires_at_ms: 0,
        claimed_caip2: "solana:devnet".into(),
    }
}

/// A ledger that records the facts it approved, for assertions.
struct RecordingLedger {
    inner: ExactApprovalLedger,
    facts: std::sync::Mutex<Vec<ExactApprovalFacts>>,
}

#[async_trait]
impl ApprovalAuthority for RecordingLedger {
    async fn approve_exact(
        &self,
        facts: &ExactApprovalFacts,
    ) -> Result<bloom_solana_machine::ApprovalToken, bloom_solana_machine::ApprovalDenied> {
        self.facts.lock().unwrap().push(facts.clone());
        self.inner.approve_exact(facts).await
    }
}

#[tokio::test]
async fn full_lifecycle_stage_sign_broadcast_confirm() {
    let parts = machine_with(SimChain::new(GENESIS), vec![]).await;
    let machine = parts.machine.clone();
    let req = request(&machine, "01");

    let prepared = machine.prepare_transfer(&req, 1_000).await.unwrap();
    assert_eq!(prepared.state, ActionState::Signed);
    assert!(prepared.artifact.is_some());

    // Broadcast: accepted.
    machine.broadcast(&req.operation_id, 1_100).await.unwrap();
    assert_eq!(
        machine.load_action(&req.operation_id).state,
        ActionState::Sent
    );

    // Land the transaction under its own signature and reconcile.
    let action = machine.load_action(&req.operation_id);
    let sig_b58 = bs58::encode(&action.artifact.clone().unwrap().signature).into_string();
    parts.land(&sig_b58);
    let status = machine.reconcile(&req.operation_id, 1_200).await.unwrap();
    assert_eq!(
        status,
        LifecycleStatus::Confirmed {
            confirmation: "confirmed".into()
        }
    );

    // Projection surface.
    let projection = machine.project(&req.operation_id).unwrap();
    assert_eq!(projection["state"], "confirmed");
    assert_eq!(projection["terminal"], serde_json::json!(true));
    assert_eq!(projection["payload_digest_hex"].as_str().unwrap().len(), 64);
    assert!(
        projection["signature_base58"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
    );
    assert_eq!(projection["attempts"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn freshness_refusal_blocks_signing_terminally() {
    let parts = machine_with(SimChain::new(GENESIS), vec![]).await;
    let machine = parts.machine.clone();
    let req = request(&machine, "02");

    // Stage while fresh; the ceremony window passes and the validity window
    // closes before finalization.
    let staged = machine.stage_transfer(&req, 1_000).await.unwrap();
    parts.chain.advance(SimChain::VALIDITY + 10);
    let err = machine
        .finalize_transfer(&req, &staged, 1_050)
        .await
        .unwrap_err();
    // Either refusal is correct here: the explicit isBlockhashValid=false
    // (BlockhashRefreshRequired) or the closed window
    // (InsufficientValidityWindow) — the provider reports both facts.
    assert!(matches!(err, MachineError::FreshnessRefused(_)));
    let action = machine.load_action(&req.operation_id);
    assert_eq!(action.state, ActionState::FreshnessRefused);
    assert!(action.artifact.is_none(), "nothing was ever signed");
    assert_eq!(parts.ledger.approval_count(), 0, "no approval consumed");
}

#[tokio::test]
async fn approval_denial_leaves_operation_staged_and_unsigned() {
    let parts = machine_with(SimChain::new(GENESIS), vec![]).await;
    let machine = parts.machine;
    let req = request(&machine, "03");

    let denying = Arc::new(ExactApprovalLedger::denying());
    let machine_denied = machine.with_approvals(denying);
    let err = machine_denied
        .prepare_transfer(&req, 1_000)
        .await
        .unwrap_err();
    assert!(matches!(err, MachineError::Approval(_)));

    let action = machine.load_action(&req.operation_id);
    assert_eq!(action.state, ActionState::Staged);
    assert!(action.artifact.is_none());
    // Still cancellable — nothing was signed.
    machine.cancel(&req.operation_id, 1_100).unwrap();
    assert_eq!(
        machine.load_action(&req.operation_id).state,
        ActionState::Cancelled
    );
}

#[tokio::test]
async fn signer_key_mismatch_fails_closed_before_staging_effects() {
    let parts = machine_with(SimChain::new(GENESIS), vec![]).await;
    let machine = parts.machine;
    let mut req = request(&machine, "04");
    // The request names a different child than the signer owns.
    req.key_ref.public_key_hex = "11".repeat(32);
    req.fee_payer_base58 = req.destination_base58.clone();
    let err = machine.prepare_transfer(&req, 1_000).await.unwrap_err();
    assert!(matches!(err, MachineError::SignerKeyMismatch));
}

#[tokio::test]
async fn ambiguous_timeout_never_resigns_and_identical_retry_confirms() {
    let parts = machine_with(
        SimChain::new(GENESIS),
        vec![ScriptedFault::on(
            "sendTransaction",
            Fault::TimeoutAfterSubmit,
        )],
    )
    .await;
    let machine = parts.machine.clone();
    let req = request(&machine, "05");

    machine.prepare_transfer(&req, 1_000).await.unwrap();
    machine.broadcast(&req.operation_id, 1_100).await.unwrap();
    let ambiguous = machine.load_action(&req.operation_id);
    assert_eq!(ambiguous.state, ActionState::Ambiguous);

    // Exact approval was one-shot: no second signature can be produced for
    // this payload, and record_signed refuses divergent bytes anyway.
    let artifact = ambiguous.artifact.clone().unwrap();
    assert!(
        machine
            .outbox()
            .record_signed(&req.operation_id, 1_200, &[9u8; 64], &artifact.artifact)
            .is_err()
    );

    // Reconcile from ambiguous retries the identical bytes (honest now).
    machine.reconcile(&req.operation_id, 1_300).await.unwrap();
    let retried = machine.load_action(&req.operation_id);
    assert_eq!(retried.attempts.len(), 2);
    assert_eq!(
        retried.attempts[0].artifact_digest_hex, retried.attempts[1].artifact_digest_hex,
        "retry reused the exact persisted bytes"
    );
    assert_eq!(retried.state, ActionState::Sent);

    let sig_b58 = bs58::encode(&artifact.signature).into_string();
    parts.land(&sig_b58);
    let status = machine.reconcile(&req.operation_id, 1_400).await.unwrap();
    assert!(matches!(status, LifecycleStatus::Confirmed { .. }));
    let done = machine.load_action(&req.operation_id);
    assert_eq!(done.state, ActionState::Confirmed);
}

#[tokio::test]
async fn wrong_genesis_blocks_the_entire_lifecycle() {
    let parts = machine_with(SimChain::new("some-other-cluster"), vec![]).await;
    let machine = parts.machine;
    let req = request(&machine, "06");
    let err = machine.prepare_transfer(&req, 1_000).await.unwrap_err();
    // The wrong cluster fails the mediated read inside the Petal; nothing is
    // durably staged anywhere in the pipeline.
    assert!(!err.to_string().is_empty());
    assert!(
        machine.outbox().load(&req.operation_id).is_err(),
        "no envelope may be persisted for the wrong cluster"
    );
}

#[tokio::test]
async fn selective_drop_quarantines_after_retries() {
    let parts = machine_with(
        SimChain::new(GENESIS),
        vec![ScriptedFault::on("sendTransaction", Fault::SelectiveDrop)],
    )
    .await;
    let machine = parts.machine;
    let req = request(&machine, "07");

    machine.prepare_transfer(&req, 1_000).await.unwrap();
    machine.broadcast(&req.operation_id, 1_100).await.unwrap();
    assert_eq!(
        machine.load_action(&req.operation_id).state,
        ActionState::Sent
    );

    // Provider says not found forever; reconciliation keeps it alive.
    for _ in 0..3 {
        let status = machine.reconcile(&req.operation_id, 1_200).await.unwrap();
        assert_eq!(status, LifecycleStatus::Sent);
    }
    machine
        .quarantine(&req.operation_id, 2_000, "never landed after retries")
        .await
        .unwrap();
    assert_eq!(
        machine.load_action(&req.operation_id).state,
        ActionState::Quarantined
    );
}

#[tokio::test]
async fn exact_approval_is_one_shot_per_payload() {
    let parts = machine_with(SimChain::new(GENESIS), vec![]).await;
    let machine = parts.machine;
    let recording = Arc::new(RecordingLedger {
        inner: ExactApprovalLedger::new(),
        facts: std::sync::Mutex::default(),
    });
    let machine = machine.with_approvals(recording.clone());

    let req = request(&machine, "08");
    machine.prepare_transfer(&req, 1_000).await.unwrap();

    // Same payload (same mediated blockhash in this window) under a new
    // operation id: the approval replay must fail closed.
    let req2 = request(&machine, "09");
    let err = machine.prepare_transfer(&req2, 1_000).await.unwrap_err();
    assert!(matches!(err, MachineError::Approval(_)));

    let facts = recording.facts.lock().unwrap();
    assert_eq!(facts.len(), 2, "both attempts reached the authority");
    assert_eq!(facts[0].payload_digest_hex, facts[1].payload_digest_hex);
    assert_eq!(facts[0].verifier_id, "solana-system-transfer-v1");
    assert_eq!(facts[0].verifier_result_digest_hex.len(), 64);
}

#[tokio::test]
async fn pinned_mount_rejects_drifted_artifacts() {
    let source = std::path::Path::new(PETAL_DIR);
    let drift = tempfile::tempdir().unwrap();
    copy_dir(source, drift.path());
    // Flip one byte in a committed route artifact.
    let artifact = drift.path().join("artifacts/routes/r000001.wasm");
    let mut bytes = std::fs::read(&artifact).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    std::fs::write(&artifact, &bytes).unwrap();

    let err = mount_pinned_solana_driver(
        drift.path(),
        tempfile::tempdir().unwrap().path(),
        Arc::new(bloom_petals::DenyHost),
    )
    .err()
    .expect("drifted artifact must fail the pin");
    assert!(
        err.to_string()
            .contains("does not match its recorded digest")
    );
}

#[tokio::test]
async fn pinned_mount_rejects_manifest_package_hash_mismatch() {
    let source = std::path::Path::new(PETAL_DIR);
    drift_manifest(source, |manifest| {
        manifest["source_package_hash"] = serde_json::json!("ff".repeat(32));
    });
}

fn drift_manifest(source: &std::path::Path, edit: impl FnOnce(&mut serde_json::Value)) {
    let drift = tempfile::tempdir().unwrap();
    copy_dir(source, drift.path());
    let path = drift.path().join("artifacts/build-manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    edit(&mut manifest);
    std::fs::write(&path, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
    let err = mount_pinned_solana_driver(
        drift.path(),
        tempfile::tempdir().unwrap().path(),
        Arc::new(bloom_petals::DenyHost),
    )
    .err()
    .expect("manifest hash drift must fail the pin");
    assert!(err.to_string().contains("package pin mismatch"));
}

fn copy_dir(source: &std::path::Path, target: &std::path::Path) {
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        let from = entry.path();
        let to = target.join(&name);
        if from.is_dir() {
            if name == "target" {
                continue;
            }
            std::fs::create_dir_all(&to).unwrap();
            copy_dir(&from, &to);
        } else {
            std::fs::copy(&from, &to).unwrap();
        }
    }
}

impl MachineParts {
    fn land(&self, signature_b58: &str) {
        // The sim keys landed transactions by whatever signature string the
        // reconciler will query.
        self.chain.land(signature_b58);
    }
}
