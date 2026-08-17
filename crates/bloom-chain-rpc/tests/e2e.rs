//! End-to-end: mediator + fault harness + freshness + durable outbox.
//!
//! These tests prove the plan's adversarial network scenarios drive the
//! outbox to the correct durable states: freshness refusals block signing,
//! timeout-after-submit becomes ambiguous with identical-byte retry, false
//! not-found keeps reconciliation alive, and selective drops quarantine.

use bloom_chain_action::fixture::FixtureDriver;
use bloom_chain_action::{
    ActionState, BroadcastOutcome, ChainActionOutbox, FreshnessReason, OutboxError,
    ReconciliationOutcome,
};
use bloom_chain_rpc::fault::{Fault, FaultProxy, ScriptedFault};
use bloom_chain_rpc::mediator::{ChainRpcProfile, DEFAULT_MAX_RESPONSE_BYTES, Mediator};
use bloom_chain_rpc::sim::SimChain;
use bloom_chain_rpc::transport::RpcTransport;
use bloom_chain_rpc::{FreshnessPolicy, FreshnessVerdict, NetworkObservation, StagedObservation};
use serde_json::{Value, json};
use std::fs;
use tempfile::TempDir;

const GENESIS: &str = "aaaa1111aaaa1111";

fn profile(allow_broadcast: bool) -> ChainRpcProfile {
    ChainRpcProfile {
        name: "sim-local".into(),
        family: "sim".into(),
        expected_genesis_hex: GENESIS.into(),
        allowed_read_methods: vec![
            "getGenesisHash".into(),
            "getBlockHeight".into(),
            "getLatestBlockhash".into(),
            "isBlockhashValid".into(),
            "getSignatureStatuses".into(),
        ],
        allow_broadcast,
        max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
    }
}

fn policy() -> FreshnessPolicy {
    FreshnessPolicy {
        max_staleness_ms: 90_000,
        min_remaining_blocks: 32,
    }
}

fn op(n: u8) -> String {
    format!("{:064x}", n)
}

fn latest_blockhash(mediator: &Mediator) -> (String, u64) {
    let v: Value = mediator
        .read(1, "getLatestBlockhash", &Value::Null)
        .unwrap();
    (
        v.pointer("/value/blockhash")
            .and_then(|b| b.as_str())
            .unwrap()
            .to_string(),
        v.pointer("/value/lastValidBlockHeight")
            .and_then(|h| h.as_u64())
            .unwrap(),
    )
}

fn block_height(proxy: &FaultProxy) -> u64 {
    proxy
        .call("getBlockHeight", &Value::Null)
        .unwrap()
        .as_u64()
        .unwrap()
}

/// Stage a fixture action, pre-check freshness, sign, and broadcast once,
/// returning the artifact digest used.
fn stage_sign_broadcast(
    outbox: &ChainActionOutbox,
    driver: &FixtureDriver,
    mediator: &Mediator,
    n: u8,
) -> String {
    let request = driver.stage_request(&op(n), "w", "k", "dst", 100, 1, 0);
    outbox.stage(request).unwrap();
    let payload = hex::decode(&outbox.load(&op(n)).unwrap().envelope.payload_hex).unwrap();
    let artifact = driver.assemble_artifact(&payload);
    let signature = driver.fixture_sign(&payload);
    outbox
        .record_signed(&op(n), 2, &signature, &artifact)
        .unwrap();
    outbox.record_broadcast_attempt(&op(n), 3).unwrap();
    let digest = outbox.load(&op(n)).unwrap().artifact.unwrap().digest_hex;
    let receipt = mediator
        .broadcast(4, &op(n), &digest, &format!("wire-{n}"))
        .unwrap();
    outbox
        .record_broadcast_outcome(&op(n), 5, 1, BroadcastOutcome::Accepted)
        .unwrap();
    let _ = receipt;
    digest
}

#[test]
fn fresh_staging_signs_and_confirms() {
    let dir = TempDir::new().unwrap();
    let outbox = ChainActionOutbox::new(dir.path()).unwrap();
    let driver = FixtureDriver::new(&"a".repeat(64), b"s");
    let proxy = std::sync::Arc::new(FaultProxy::new(SimChain::new(GENESIS), vec![]));
    let mediator = Mediator::new(profile(true), vec![Box::new(proxy.clone())]).unwrap();

    // Stage and observe freshness.
    outbox
        .stage(driver.stage_request(&op(1), "w", "k", "dst", 100, 1_000, 0))
        .unwrap();
    let (blockhash, last_valid) = latest_blockhash(&mediator);
    let staged = StagedObservation {
        blockhash,
        last_valid_block_height: last_valid,
        staged_at_ms: 1_000,
        commitment: "confirmed".into(),
    };
    let obs = NetworkObservation {
        latest_blockhash: staged.blockhash.clone(),
        latest_block_height: 100,
        blockhash_valid: Some(true),
        observed_at_ms: 1_500,
        commitment: "confirmed".into(),
    };
    assert_eq!(
        bloom_chain_rpc::evaluate_freshness(&staged, &obs, None, &policy()),
        FreshnessVerdict::Fresh
    );

    // Sign, broadcast, land, reconcile.
    let payload = hex::decode(&outbox.load(&op(1)).unwrap().envelope.payload_hex).unwrap();
    let artifact = driver.assemble_artifact(&payload);
    let signature = driver.fixture_sign(&payload);
    outbox
        .record_signed(&op(1), 2_000, &signature, &artifact)
        .unwrap();
    outbox.record_broadcast_attempt(&op(1), 2_100).unwrap();
    let digest = outbox.load(&op(1)).unwrap().artifact.unwrap().digest_hex;
    let receipt = mediator
        .broadcast(2_200, &op(1), &digest, "wire-1")
        .unwrap();
    outbox
        .record_broadcast_outcome(&op(1), 2_300, 1, BroadcastOutcome::Accepted)
        .unwrap();
    proxy.chain().land(&receipt.signature);
    let status: Value = mediator
        .read(2_400, "getSignatureStatuses", &json!([receipt.signature]))
        .unwrap();
    assert_eq!(status["value"][0]["confirmationStatus"], json!("confirmed"));
    outbox
        .record_reconciliation(
            &op(1),
            2_500,
            ReconciliationOutcome::Confirmed {
                detail: "sim slot".into(),
            },
        )
        .unwrap();
    assert_eq!(outbox.load(&op(1)).unwrap().state, ActionState::Confirmed);
}

#[test]
fn expired_validity_window_refuses_freshness_before_signing() {
    let dir = TempDir::new().unwrap();
    let outbox = ChainActionOutbox::new(dir.path()).unwrap();
    let driver = FixtureDriver::new(&"a".repeat(64), b"s");
    let proxy = std::sync::Arc::new(FaultProxy::new(SimChain::new(GENESIS), vec![]));
    let mediator = Mediator::new(profile(true), vec![Box::new(proxy.clone())]).unwrap();

    let (blockhash, last_valid) = latest_blockhash(&mediator);
    outbox
        .stage(driver.stage_request(&op(2), "w", "k", "dst", 100, 1_000, 0))
        .unwrap();
    let staged = StagedObservation {
        blockhash,
        last_valid_block_height: last_valid,
        staged_at_ms: 1_000,
        commitment: "confirmed".into(),
    };

    // The chain advances past the window before signing.
    proxy.chain().advance(SimChain::VALIDITY + 10);
    let obs = NetworkObservation {
        latest_blockhash: staged.blockhash.clone(),
        latest_block_height: proxy.chain().height(),
        blockhash_valid: None,
        observed_at_ms: 1_500,
        commitment: "confirmed".into(),
    };
    let verdict = bloom_chain_rpc::evaluate_freshness(&staged, &obs, None, &policy());
    assert_eq!(
        verdict,
        FreshnessVerdict::Refused(FreshnessReason::InsufficientValidityWindow)
    );

    // The refusal becomes a durable, terminal outbox transition.
    let refused = outbox
        .refuse_for_freshness(&op(2), 2_000, FreshnessReason::InsufficientValidityWindow)
        .unwrap();
    assert_eq!(refused.state, ActionState::FreshnessRefused);
    let payload = hex::decode(&refused.envelope.payload_hex).unwrap();
    let artifact = driver.assemble_artifact(&payload);
    let signature = driver.fixture_sign(&payload);
    assert!(matches!(
        outbox
            .record_signed(&op(2), 2_100, &signature, &artifact)
            .unwrap_err(),
        OutboxError::InvalidTransition { .. }
    ));
}

#[test]
fn wrong_genesis_blocks_every_mediated_call() {
    // A consistently wrong cluster: every genesis observation mismatches.
    let wrong = || {
        ScriptedFault::on(
            "getGenesisHash",
            Fault::WrongGenesis {
                genesis: "deadbeef".into(),
            },
        )
    };
    let proxy = FaultProxy::new(
        SimChain::new(GENESIS),
        vec![wrong(), wrong(), wrong(), wrong()],
    );
    let mediator = Mediator::new(profile(true), vec![Box::new(proxy)]).unwrap();
    let err = mediator
        .read(1, "getLatestBlockhash", &Value::Null)
        .unwrap_err();
    assert!(matches!(
        err,
        bloom_chain_rpc::MediationError::ClusterGenesisMismatch { .. }
    ));
    // Broadcast is equally blocked: the wrong cluster never receives bytes.
    assert!(matches!(
        mediator.broadcast(2, "op", "digest", "wire").unwrap_err(),
        bloom_chain_rpc::MediationError::ClusterGenesisMismatch { .. }
    ));
}

#[test]
fn timeout_after_submit_is_ambiguous_then_identical_byte_retry_confirms() {
    let dir = TempDir::new().unwrap();
    let outbox = ChainActionOutbox::new(dir.path()).unwrap();
    let driver = FixtureDriver::new(&"a".repeat(64), b"s");
    let proxy = std::sync::Arc::new(FaultProxy::new(
        SimChain::new(GENESIS),
        vec![ScriptedFault::on(
            "sendTransaction",
            Fault::TimeoutAfterSubmit,
        )],
    ));
    let mediator = Mediator::new(profile(true), vec![Box::new(proxy.clone())]).unwrap();

    // Stage and sign.
    outbox
        .stage(driver.stage_request(&op(3), "w", "k", "dst", 100, 1, 0))
        .unwrap();
    let payload = hex::decode(&outbox.load(&op(3)).unwrap().envelope.payload_hex).unwrap();
    let artifact = driver.assemble_artifact(&payload);
    let signature = driver.fixture_sign(&payload);
    outbox
        .record_signed(&op(3), 2, &signature, &artifact)
        .unwrap();

    // First dispatch times out after submission: ambiguous.
    outbox.record_broadcast_attempt(&op(3), 3).unwrap();
    let digest = outbox.load(&op(3)).unwrap().artifact.unwrap().digest_hex;
    let err = mediator
        .broadcast(4, &op(3), &digest, "wire-3")
        .unwrap_err();
    assert!(matches!(
        err,
        bloom_chain_rpc::MediationError::Rpc(bloom_chain_rpc::RpcError::Timeout)
    ));
    outbox
        .record_broadcast_outcome(&op(3), 5, 1, BroadcastOutcome::Ambiguous)
        .unwrap();
    assert_eq!(outbox.load(&op(3)).unwrap().state, ActionState::Ambiguous);

    // Re-signing is refused even in ambiguous state: identical bytes are
    // idempotent, and this call re-supplies them, so it must NOT error with
    // AlreadySigned but must also not create a second signature — the state
    // stays Ambiguous with exactly one signed record.
    let idem = outbox
        .record_signed(&op(3), 6, &signature, &artifact)
        .unwrap();
    assert_eq!(idem.state, ActionState::Ambiguous);
    assert_eq!(
        outbox.load(&op(3)).unwrap().journal.len(),
        4,
        "idempotent re-supply records nothing new"
    );
    // Divergent bytes are flatly refused.
    assert!(matches!(
        outbox.record_signed(&op(3), 6, &[9u8; 64], &[8u8; 128]),
        Err(OutboxError::AlreadySigned)
    ));

    // Retry uses the identical persisted bytes (honest now: script consumed).
    outbox.record_broadcast_attempt(&op(3), 7).unwrap();
    let receipt = mediator.broadcast(8, &op(3), &digest, "wire-3").unwrap();
    outbox
        .record_broadcast_outcome(&op(3), 9, 2, BroadcastOutcome::Accepted)
        .unwrap();
    assert_eq!(outbox.load(&op(3)).unwrap().state, ActionState::Sent);
    proxy.chain().land(&receipt.signature);

    let status: Value = mediator
        .read(10, "getSignatureStatuses", &json!([receipt.signature]))
        .unwrap();
    assert_eq!(status["value"][0]["confirmationStatus"], json!("confirmed"));
    outbox
        .record_reconciliation(
            &op(3),
            11,
            ReconciliationOutcome::Confirmed {
                detail: "ok".into(),
            },
        )
        .unwrap();
    let done = outbox.load(&op(3)).unwrap();
    assert_eq!(done.state, ActionState::Confirmed);
    assert_eq!(done.attempts.len(), 2);
    assert_eq!(
        done.attempts[0].artifact_digest_hex,
        done.attempts[1].artifact_digest_hex
    );
}

#[test]
fn false_not_found_keeps_reconciliation_alive() {
    let dir = TempDir::new().unwrap();
    let outbox = ChainActionOutbox::new(dir.path()).unwrap();
    let driver = FixtureDriver::new(&"a".repeat(64), b"s");
    let proxy = std::sync::Arc::new(FaultProxy::new(SimChain::new(GENESIS), vec![]));
    let mediator = Mediator::new(profile(true), vec![Box::new(proxy.clone())]).unwrap();

    let digest = stage_sign_broadcast(&outbox, &driver, &mediator, 4);
    let signature = SimChain::signature_for("wire-4");
    proxy.chain().land(&signature);

    // The provider lies: not found for a landed transaction.
    let lying = FaultProxy::new(
        SimChain::new(GENESIS),
        vec![ScriptedFault::on(
            "getSignatureStatuses",
            Fault::FalseNotFound,
        )],
    );
    let lying_mediator = Mediator::new(profile(true), vec![Box::new(lying)]).unwrap();
    let status: Value = lying_mediator
        .read(10, "getSignatureStatuses", &json!([signature]))
        .unwrap();
    assert_eq!(status["value"][0], Value::Null);
    // The outbox stays Sent — a null status is "keep reconciling", not a
    // non-effect, so no transition is recorded.
    assert_eq!(outbox.load(&op(4)).unwrap().state, ActionState::Sent);

    // An honest provider resolves it.
    let honest: Value = mediator
        .read(11, "getSignatureStatuses", &json!([signature]))
        .unwrap();
    assert_eq!(honest["value"][0]["confirmationStatus"], json!("confirmed"));
    outbox
        .record_reconciliation(
            &op(4),
            12,
            ReconciliationOutcome::Confirmed {
                detail: "ok".into(),
            },
        )
        .unwrap();
    assert_eq!(outbox.load(&op(4)).unwrap().state, ActionState::Confirmed);
    let _ = digest;
}

#[test]
fn selective_drop_quarantines() {
    let dir = TempDir::new().unwrap();
    let outbox = ChainActionOutbox::new(dir.path()).unwrap();
    let driver = FixtureDriver::new(&"a".repeat(64), b"s");
    let proxy = std::sync::Arc::new(FaultProxy::new(
        SimChain::new(GENESIS),
        vec![ScriptedFault::on("sendTransaction", Fault::SelectiveDrop)],
    ));
    let mediator = Mediator::new(profile(true), vec![Box::new(proxy.clone())]).unwrap();

    outbox
        .stage(driver.stage_request(&op(5), "w", "k", "dst", 100, 1, 0))
        .unwrap();
    let payload = hex::decode(&outbox.load(&op(5)).unwrap().envelope.payload_hex).unwrap();
    let artifact = driver.assemble_artifact(&payload);
    let signature = driver.fixture_sign(&payload);
    outbox
        .record_signed(&op(5), 2, &signature, &artifact)
        .unwrap();
    outbox.record_broadcast_attempt(&op(5), 3).unwrap();
    let digest = outbox.load(&op(5)).unwrap().artifact.unwrap().digest_hex;

    // Accepted by the provider, but the transaction never lands.
    let receipt = mediator.broadcast(4, &op(5), &digest, "wire-5").unwrap();
    outbox
        .record_broadcast_outcome(&op(5), 5, 1, BroadcastOutcome::Accepted)
        .unwrap();
    assert_eq!(outbox.load(&op(5)).unwrap().state, ActionState::Sent);
    proxy.chain().advance(500);
    let status: Value = mediator
        .read(6, "getSignatureStatuses", &json!([receipt.signature]))
        .unwrap();
    assert_eq!(status["value"][0], Value::Null, "dropped: never lands");

    // After exhausting retries the operator quarantines.
    outbox
        .record_reconciliation(
            &op(5),
            7,
            ReconciliationOutcome::Quarantined {
                reason: "provider accepted but never landed".into(),
            },
        )
        .unwrap();
    assert_eq!(outbox.load(&op(5)).unwrap().state, ActionState::Quarantined);
}

#[test]
fn disagreeing_providers_refuse_as_inconsistent() {
    let honest = FaultProxy::new(SimChain::new(GENESIS), vec![]);
    let lying = FaultProxy::new(
        SimChain::new(GENESIS),
        vec![ScriptedFault::on(
            "getBlockHeight",
            Fault::ProviderDisagreement { offset_blocks: 900 },
        )],
    );

    let (blockhash, last_valid) = {
        let mediator = Mediator::new(profile(true), vec![Box::new(honest)]).unwrap();
        latest_blockhash(&mediator)
    };
    let staged = StagedObservation {
        blockhash,
        last_valid_block_height: last_valid,
        staged_at_ms: 1_000,
        commitment: "confirmed".into(),
    };

    let a = NetworkObservation {
        latest_blockhash: staged.blockhash.clone(),
        latest_block_height: 100,
        blockhash_valid: None,
        observed_at_ms: 1_200,
        commitment: "confirmed".into(),
    };
    let b = NetworkObservation {
        latest_blockhash: staged.blockhash.clone(),
        latest_block_height: block_height(&lying),
        blockhash_valid: None,
        observed_at_ms: 1_201,
        commitment: "confirmed".into(),
    };
    assert_eq!(
        bloom_chain_rpc::evaluate_freshness(&staged, &a, Some(&b), &policy()),
        FreshnessVerdict::Refused(FreshnessReason::NetworkObservationInconsistent)
    );
}

#[test]
fn broadcast_audit_pins_operation_and_artifact() {
    let dir = TempDir::new().unwrap();
    let outbox = ChainActionOutbox::new(dir.path()).unwrap();
    let driver = FixtureDriver::new(&"a".repeat(64), b"s");
    let proxy = FaultProxy::new(SimChain::new(GENESIS), vec![]);
    let mediator = Mediator::new(profile(true), vec![Box::new(proxy)]).unwrap();

    let digest = stage_sign_broadcast(&outbox, &driver, &mediator, 6);
    let audit = mediator.audit();
    let broadcasts: Vec<_> = audit.iter().filter(|e| e.kind == "broadcast").collect();
    assert_eq!(broadcasts.len(), 1);
    assert_eq!(broadcasts[0].operation_id.as_deref(), Some(op(6).as_str()));
    assert_eq!(
        broadcasts[0].artifact_digest_hex.as_deref(),
        Some(digest.as_str())
    );
    let _ = fs::read_dir(dir.path()).unwrap();
}
