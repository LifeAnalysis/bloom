//! Lifecycle, restart, mutation, idempotency, and concurrency tests for the
//! chain-neutral chain-action outbox, driven by the deterministic fixture
//! driver.

use bloom_chain_action::fixture::{FixtureDriver, ScriptedOutcome};
use bloom_chain_action::{
    ActionState, BroadcastOutcome, ChainActionOutbox, OutboxError, ReconciliationOutcome,
};
use std::fs;
use std::thread;
use tempfile::TempDir;

const PKG: &str = "aa00aa00aa00aa00aa00aa00aa00aa00aa00aa00aa00aa00aa00aa00aa00aa00";

fn driver() -> FixtureDriver {
    FixtureDriver::new(PKG, b"fixture-secret")
}

fn op(n: u8) -> String {
    format!("{:064x}", n)
}

fn stage(outbox: &ChainActionOutbox, d: &FixtureDriver, n: u8) -> bloom_chain_action::Action {
    outbox
        .stage(d.stage_request(&op(n), "wallet-1", "key-ref-1", "dest-1", 1_000, 100, 0))
        .unwrap()
}

fn sign(outbox: &ChainActionOutbox, d: &FixtureDriver, n: u8) -> Vec<u8> {
    let action = outbox.load(&op(n)).unwrap();
    let artifact = d.assemble_artifact(&hex::decode(&action.envelope.payload_hex).unwrap());
    let signature = d.fixture_sign(&hex::decode(&action.envelope.payload_hex).unwrap());
    outbox
        .record_signed(&op(n), 200, &signature, &artifact)
        .unwrap();
    artifact
}

fn journal_file(root: &TempDir, n: u8, seq: &str) -> std::path::PathBuf {
    root.path()
        .join("actions")
        .join(op(n))
        .join("journal")
        .join(format!("{seq}.json"))
}

#[test]
fn happy_path_stage_sign_sent_confirmed() {
    let dir = TempDir::new().unwrap();
    let outbox = ChainActionOutbox::new(dir.path()).unwrap();
    let d = driver();

    let a = stage(&outbox, &d, 1);
    assert_eq!(a.state, ActionState::Staged);
    assert_eq!(a.envelope.schema, "bloom.chain-action/1");

    sign(&outbox, &d, 1);
    assert_eq!(outbox.load(&op(1)).unwrap().state, ActionState::Signed);

    outbox.record_broadcast_attempt(&op(1), 300).unwrap();
    outbox
        .record_broadcast_outcome(&op(1), 400, 1, BroadcastOutcome::Accepted)
        .unwrap();
    assert_eq!(outbox.load(&op(1)).unwrap().state, ActionState::Sent);

    let done = outbox
        .record_reconciliation(
            &op(1),
            500,
            ReconciliationOutcome::Confirmed {
                detail: "slot 42".into(),
            },
        )
        .unwrap();
    assert_eq!(done.state, ActionState::Confirmed);
    assert!(done.state.is_terminal());
}

#[test]
fn staging_is_idempotent_for_identical_envelope() {
    let dir = TempDir::new().unwrap();
    let outbox = ChainActionOutbox::new(dir.path()).unwrap();
    let d = driver();
    let request = d.stage_request(&op(2), "w", "k", "dest", 5, 100, 0);

    outbox.stage(request.clone()).unwrap();
    let again = outbox.stage(request).unwrap();
    assert_eq!(again.state, ActionState::Staged);
    assert_eq!(outbox.list().unwrap().len(), 1);
}

#[test]
fn staging_rejects_mutated_envelope_for_same_operation() {
    let dir = TempDir::new().unwrap();
    let outbox = ChainActionOutbox::new(dir.path()).unwrap();
    let d = driver();
    stage(&outbox, &d, 3);

    let mut other = d.stage_request(&op(3), "w", "k", "dest", 5, 100, 0);
    other.payload[0] ^= 0xff;
    let err = outbox.stage(other).unwrap_err();
    assert!(matches!(err, OutboxError::EnvelopeMismatch(_)));
}

#[test]
fn signing_twice_is_rejected_and_idempotent_for_identical_bytes() {
    let dir = TempDir::new().unwrap();
    let outbox = ChainActionOutbox::new(dir.path()).unwrap();
    let d = driver();
    stage(&outbox, &d, 4);
    sign(&outbox, &d, 4);

    let action = outbox.load(&op(4)).unwrap();
    let artifact = action.artifact.as_ref().unwrap();
    // Identical bytes: idempotent success.
    outbox
        .record_signed(&op(4), 250, &artifact.signature, &artifact.artifact)
        .unwrap();
    // Different bytes: never re-sign.
    assert!(matches!(
        outbox
            .record_signed(&op(4), 260, &[9u8; 64], &[8u8; 128])
            .unwrap_err(),
        OutboxError::AlreadySigned
    ));
    assert_eq!(outbox.load(&op(4)).unwrap().state, ActionState::Signed);
}

#[test]
fn timeout_creates_ambiguous_and_retry_uses_identical_bytes() {
    let dir = TempDir::new().unwrap();
    let outbox = ChainActionOutbox::new(dir.path()).unwrap();
    let mut d = driver().with_script(vec![ScriptedOutcome::Timeout, ScriptedOutcome::Accept]);
    stage(&outbox, &d, 5);
    sign(&outbox, &d, 5);

    // First dispatch times out after submission: ambiguous.
    outbox.record_broadcast_attempt(&op(5), 300).unwrap();
    let out1 = d.next_broadcast_outcome();
    outbox
        .record_broadcast_outcome(&op(5), 400, 1, out1)
        .unwrap();
    let amb = outbox.load(&op(5)).unwrap();
    assert_eq!(amb.state, ActionState::Ambiguous);
    let artifact_digest = amb.artifact.as_ref().unwrap().digest_hex.clone();

    // A second signature is refused even in ambiguous state.
    let action = outbox.load(&op(5)).unwrap();
    let sig = action.artifact.as_ref().unwrap().signature.clone();
    assert!(matches!(
        outbox
            .record_signed(&op(5), 450, &sig, &[7u8; 96])
            .unwrap_err(),
        OutboxError::AlreadySigned
    ));

    // Retry must reuse the exact persisted artifact bytes.
    outbox.record_broadcast_attempt(&op(5), 500).unwrap();
    let retry = outbox.load(&op(5)).unwrap();
    assert_eq!(retry.attempts.len(), 2);
    assert_eq!(retry.attempts[0].artifact_digest_hex, artifact_digest);
    assert_eq!(retry.attempts[1].artifact_digest_hex, artifact_digest);

    let out2 = d.next_broadcast_outcome();
    outbox
        .record_broadcast_outcome(&op(5), 600, 2, out2)
        .unwrap();
    assert_eq!(outbox.load(&op(5)).unwrap().state, ActionState::Sent);

    outbox
        .record_reconciliation(
            &op(5),
            700,
            ReconciliationOutcome::Confirmed {
                detail: "ok".into(),
            },
        )
        .unwrap();
    assert_eq!(outbox.load(&op(5)).unwrap().state, ActionState::Confirmed);
}

#[test]
fn definitive_rejection_fails_with_known_non_effect() {
    let dir = TempDir::new().unwrap();
    let outbox = ChainActionOutbox::new(dir.path()).unwrap();
    let mut d = driver().with_script(vec![ScriptedOutcome::Reject]);
    stage(&outbox, &d, 6);
    sign(&outbox, &d, 6);
    outbox.record_broadcast_attempt(&op(6), 300).unwrap();
    let out = d.next_broadcast_outcome();
    let failed = outbox
        .record_broadcast_outcome(&op(6), 400, 1, out)
        .unwrap();
    assert_eq!(failed.state, ActionState::Failed);
    assert!(failed.state.is_terminal());
}

#[test]
fn broadcast_attempt_requires_signature() {
    let dir = TempDir::new().unwrap();
    let outbox = ChainActionOutbox::new(dir.path()).unwrap();
    let d = driver();
    stage(&outbox, &d, 7);
    assert!(matches!(
        outbox.record_broadcast_attempt(&op(7), 300).unwrap_err(),
        OutboxError::InvalidTransition {
            from: "staged",
            to: "broadcast_attempted"
        }
    ));
}

#[test]
fn outcome_for_unknown_attempt_is_rejected() {
    let dir = TempDir::new().unwrap();
    let outbox = ChainActionOutbox::new(dir.path()).unwrap();
    let d = driver();
    stage(&outbox, &d, 8);
    sign(&outbox, &d, 8);
    assert!(matches!(
        outbox
            .record_broadcast_outcome(&op(8), 400, 9, BroadcastOutcome::Accepted)
            .unwrap_err(),
        OutboxError::AttemptNotFound(9)
    ));
}

#[test]
fn conflicting_outcomes_for_same_attempt_are_rejected() {
    let dir = TempDir::new().unwrap();
    let outbox = ChainActionOutbox::new(dir.path()).unwrap();
    let d = driver();
    stage(&outbox, &d, 9);
    sign(&outbox, &d, 9);
    outbox.record_broadcast_attempt(&op(9), 300).unwrap();
    outbox
        .record_broadcast_outcome(&op(9), 400, 1, BroadcastOutcome::Accepted)
        .unwrap();
    assert!(matches!(
        outbox
            .record_broadcast_outcome(&op(9), 500, 1, BroadcastOutcome::Ambiguous)
            .unwrap_err(),
        OutboxError::AttemptOutcomeConflict(1)
    ));
}

#[test]
fn identical_outcome_is_idempotent() {
    let dir = TempDir::new().unwrap();
    let outbox = ChainActionOutbox::new(dir.path()).unwrap();
    let d = driver();
    stage(&outbox, &d, 10);
    sign(&outbox, &d, 10);
    outbox.record_broadcast_attempt(&op(10), 300).unwrap();
    outbox
        .record_broadcast_outcome(&op(10), 400, 1, BroadcastOutcome::Accepted)
        .unwrap();
    let again = outbox
        .record_broadcast_outcome(&op(10), 500, 1, BroadcastOutcome::Accepted)
        .unwrap();
    assert_eq!(again.state, ActionState::Sent);
    assert_eq!(
        again.journal.len(),
        4,
        "idempotent outcome records no new journal entry"
    );
}

#[test]
fn cancel_is_legal_only_before_signing() {
    let dir = TempDir::new().unwrap();
    let outbox = ChainActionOutbox::new(dir.path()).unwrap();
    let d = driver();
    stage(&outbox, &d, 11);
    assert_eq!(
        outbox.cancel(&op(11), 300).unwrap().state,
        ActionState::Cancelled
    );

    stage(&outbox, &d, 12);
    sign(&outbox, &d, 12);
    assert!(matches!(
        outbox.cancel(&op(12), 300).unwrap_err(),
        OutboxError::InvalidTransition {
            from: "signed",
            to: "cancelled"
        }
    ));
}

#[test]
fn quarantine_from_ambiguous_is_terminal() {
    let dir = TempDir::new().unwrap();
    let outbox = ChainActionOutbox::new(dir.path()).unwrap();
    let mut d = driver().with_script(vec![ScriptedOutcome::Timeout]);
    stage(&outbox, &d, 13);
    sign(&outbox, &d, 13);
    outbox.record_broadcast_attempt(&op(13), 300).unwrap();
    let out = d.next_broadcast_outcome();
    outbox
        .record_broadcast_outcome(&op(13), 400, 1, out)
        .unwrap();

    let q = outbox
        .record_reconciliation(
            &op(13),
            500,
            ReconciliationOutcome::Quarantined {
                reason: "provider split".into(),
            },
        )
        .unwrap();
    assert_eq!(q.state, ActionState::Quarantined);
    assert!(q.state.is_terminal());
    // Nothing leaves quarantine.
    assert!(
        outbox
            .record_reconciliation(
                &op(13),
                600,
                ReconciliationOutcome::Confirmed {
                    detail: String::new()
                }
            )
            .is_err()
    );
}

#[test]
fn expiry_sweep_expires_only_eligible_actions() {
    let dir = TempDir::new().unwrap();
    let outbox = ChainActionOutbox::new(dir.path()).unwrap();
    let mut d = driver().with_script(vec![ScriptedOutcome::Timeout]);

    // 20: staged, expires at 1000.
    outbox
        .stage(d.stage_request(&op(20), "w", "k", "dst", 1, 100, 1000))
        .unwrap();
    // 21: signed + ambiguous, expires at 1000.
    outbox
        .stage(d.stage_request(&op(21), "w", "k", "dst", 1, 100, 1000))
        .unwrap();
    sign(&outbox, &d, 21);
    outbox.record_broadcast_attempt(&op(21), 300).unwrap();
    let out = d.next_broadcast_outcome();
    outbox
        .record_broadcast_outcome(&op(21), 400, 1, out)
        .unwrap();
    // 22: terminal confirmed, expires at 1000 — untouched.
    outbox
        .stage(d.stage_request(&op(22), "w", "k", "dst", 1, 100, 1000))
        .unwrap();
    sign(&outbox, &d, 22);
    outbox.record_broadcast_attempt(&op(22), 300).unwrap();
    outbox
        .record_broadcast_outcome(&op(22), 400, 1, BroadcastOutcome::Accepted)
        .unwrap();
    outbox
        .record_reconciliation(
            &op(22),
            500,
            ReconciliationOutcome::Confirmed { detail: "x".into() },
        )
        .unwrap();
    // 23: staged, no expiry — untouched.
    stage(&outbox, &d, 23);

    let expired = outbox.sweep_expired(2000).unwrap();
    assert_eq!(expired, vec![op(20), op(21)]);
    assert_eq!(outbox.load(&op(20)).unwrap().state, ActionState::Expired);
    let e21 = outbox.load(&op(21)).unwrap();
    assert_eq!(e21.state, ActionState::Expired);
    // Expiry after an ambiguous dispatch cannot prove a non-effect.
    let exp = e21
        .journal
        .iter()
        .find(|r| matches!(r.transition, bloom_chain_action::Transition::Expired { .. }))
        .unwrap();
    assert!(matches!(
        exp.transition,
        bloom_chain_action::Transition::Expired {
            proven_non_effect: false
        }
    ));
    assert_eq!(outbox.load(&op(22)).unwrap().state, ActionState::Confirmed);
    assert_eq!(outbox.load(&op(23)).unwrap().state, ActionState::Staged);

    // Sweep is idempotent.
    assert!(outbox.sweep_expired(3000).unwrap().is_empty());
}

#[test]
fn restart_recovers_state_and_terminal_actions_stay_frozen() {
    let dir = TempDir::new().unwrap();
    {
        let outbox = ChainActionOutbox::new(dir.path()).unwrap();
        let d = driver();
        stage(&outbox, &d, 30);
        sign(&outbox, &d, 30);
        outbox.record_broadcast_attempt(&op(30), 300).unwrap();
        outbox
            .record_broadcast_outcome(&op(30), 400, 1, BroadcastOutcome::Accepted)
            .unwrap();
    }
    // "Crash": drop the instance, reopen from disk.
    let outbox2 = ChainActionOutbox::new(dir.path()).unwrap();
    let recovered = outbox2.load(&op(30)).unwrap();
    assert_eq!(recovered.state, ActionState::Sent);
    assert_eq!(recovered.attempts.len(), 1);
    assert!(recovered.artifact.is_some());

    // Finish after restart.
    outbox2
        .record_reconciliation(
            &op(30),
            900,
            ReconciliationOutcome::Confirmed {
                detail: "ok".into(),
            },
        )
        .unwrap();
    assert_eq!(outbox2.load(&op(30)).unwrap().state, ActionState::Confirmed);

    // Third open: terminal state persists; nothing can move it.
    let outbox3 = ChainActionOutbox::new(dir.path()).unwrap();
    assert_eq!(outbox3.load(&op(30)).unwrap().state, ActionState::Confirmed);
    assert!(
        outbox3
            .record_reconciliation(
                &op(30),
                999,
                ReconciliationOutcome::Failed { reason: "x".into() }
            )
            .is_err()
    );
}

#[test]
fn stray_temp_files_are_ignored_on_recovery() {
    let dir = TempDir::new().unwrap();
    let outbox = ChainActionOutbox::new(dir.path()).unwrap();
    let d = driver();
    stage(&outbox, &d, 31);
    // Simulate a crash mid-write: leftover temp file next to real records.
    fs::write(
        journal_file(&dir, 31, "00000001").with_extension("json.tmp-99"),
        b"garbage",
    )
    .unwrap();
    fs::write(
        dir.path()
            .join("actions")
            .join(op(31))
            .join("envelope.json.tmp-98"),
        b"garbage",
    )
    .unwrap();
    let loaded = outbox.load(&op(31)).unwrap();
    assert_eq!(loaded.state, ActionState::Staged);
}

#[test]
fn journal_sequence_gap_is_detected() {
    let dir = TempDir::new().unwrap();
    let outbox = ChainActionOutbox::new(dir.path()).unwrap();
    let d = driver();
    stage(&outbox, &d, 32);
    sign(&outbox, &d, 32);
    outbox.record_broadcast_attempt(&op(32), 300).unwrap();
    // Removing a MIDDLE record breaks contiguity. (Removing the final record
    // is indistinguishable from a crash before that append and must load as
    // the older state — see `restart_recovers_state_and_terminal_actions_stay_frozen`.)
    fs::remove_file(journal_file(&dir, 32, "00000002")).unwrap();
    assert!(matches!(
        outbox.load(&op(32)).unwrap_err(),
        OutboxError::SequenceGap {
            expected: 2,
            found: Some(3)
        }
    ));
}

#[test]
fn tail_truncated_journal_loads_as_older_state() {
    let dir = TempDir::new().unwrap();
    let outbox = ChainActionOutbox::new(dir.path()).unwrap();
    let d = driver();
    stage(&outbox, &d, 36);
    sign(&outbox, &d, 36);
    // A crash between appends leaves a valid prefix; recovery must accept it.
    fs::remove_file(journal_file(&dir, 36, "00000002")).unwrap();
    let action = outbox.load(&op(36)).unwrap();
    assert_eq!(action.state, ActionState::Staged);
    // The action can then be signed for real.
    sign(&outbox, &d, 36);
    assert_eq!(outbox.load(&op(36)).unwrap().state, ActionState::Signed);
}

#[test]
fn envelope_mutation_on_disk_is_detected() {
    let dir = TempDir::new().unwrap();
    let outbox = ChainActionOutbox::new(dir.path()).unwrap();
    let d = driver();
    stage(&outbox, &d, 33);

    let action = outbox.load(&op(33)).unwrap();
    let path = dir
        .path()
        .join("actions")
        .join(op(33))
        .join("envelope.json");
    let raw = fs::read_to_string(&path).unwrap();
    // Tamper with the recorded payload digest (keep it valid JSON).
    let tampered = raw.replace(&action.envelope.payload_digest_hex, &"f".repeat(64));
    assert_ne!(raw, tampered);
    fs::write(&path, tampered).unwrap();

    assert!(matches!(
        outbox.load(&op(33)).unwrap_err(),
        OutboxError::PayloadDigestMismatch
    ));
}

#[test]
fn journal_mutation_on_disk_is_detected() {
    let dir = TempDir::new().unwrap();
    let outbox = ChainActionOutbox::new(dir.path()).unwrap();
    let d = driver();
    stage(&outbox, &d, 34);
    sign(&outbox, &d, 34);

    // Mutate a field the digest chain covers (the timestamp).
    let path = journal_file(&dir, 34, "00000002");
    let raw = fs::read_to_string(&path).unwrap();
    fs::write(&path, raw.replace("\"at_ms\": 200", "\"at_ms\": 999")).unwrap();
    assert!(matches!(
        outbox.load(&op(34)).unwrap_err(),
        OutboxError::JournalChainMismatch(2)
    ));

    // Mutate the signature bytes inside another signed record.
    stage(&outbox, &d, 35);
    sign(&outbox, &d, 35);
    let path35 = journal_file(&dir, 35, "00000002");
    let raw35 = fs::read_to_string(&path35).unwrap();
    fs::write(&path35, raw35.replace("00", "11")).unwrap();
    assert!(outbox.load(&op(35)).is_err());
}

#[test]
fn invalid_transitions_are_rejected_across_the_lattice() {
    let dir = TempDir::new().unwrap();
    let outbox = ChainActionOutbox::new(dir.path()).unwrap();
    let d = driver();
    stage(&outbox, &d, 40);

    // Confirming directly from staged is illegal.
    assert!(matches!(
        outbox
            .record_reconciliation(
                &op(40),
                300,
                ReconciliationOutcome::Confirmed { detail: "x".into() }
            )
            .unwrap_err(),
        OutboxError::InvalidTransition {
            from: "staged",
            to: "reconciliation"
        }
    ));
    // An outcome without an attempt is illegal.
    assert!(matches!(
        outbox
            .record_broadcast_outcome(&op(40), 300, 1, BroadcastOutcome::Accepted)
            .unwrap_err(),
        OutboxError::AttemptNotFound(1)
    ));

    stage(&outbox, &d, 41);
    sign(&outbox, &d, 41);
    outbox.record_broadcast_attempt(&op(41), 300).unwrap();
    outbox
        .record_broadcast_outcome(&op(41), 400, 1, BroadcastOutcome::Accepted)
        .unwrap();
    // A new attempt after acceptance (state Sent) is illegal.
    assert!(matches!(
        outbox.record_broadcast_attempt(&op(41), 500).unwrap_err(),
        OutboxError::InvalidTransition {
            from: "sent",
            to: "broadcast_attempted"
        }
    ));
}

#[test]
fn oversized_payload_is_rejected_at_staging() {
    let dir = TempDir::new().unwrap();
    let outbox = ChainActionOutbox::new(dir.path()).unwrap();
    let d = driver();
    let mut req = d.stage_request(&op(42), "w", "k", "dst", 1, 100, 0);
    req.payload = vec![0u8; 4097];
    assert!(matches!(
        outbox.stage(req).unwrap_err(),
        OutboxError::PayloadTooLarge(4097)
    ));
}

#[test]
fn concurrent_identical_staging_is_idempotent() {
    let dir = TempDir::new().unwrap();
    let outbox = std::sync::Arc::new(ChainActionOutbox::new(dir.path()).unwrap());
    let d = std::sync::Arc::new(driver());
    let request = d.stage_request(&op(50), "w", "k", "dst", 7, 100, 0);

    let handles: Vec<_> = (0..8)
        .map(|_| {
            let ob = outbox.clone();
            let req = request.clone();
            thread::spawn(move || ob.stage(req).is_ok())
        })
        .collect();
    let oks: usize = handles
        .into_iter()
        .map(|h| h.join().unwrap() as usize)
        .sum();
    assert_eq!(oks, 8, "every identical concurrent stage succeeds");
    assert_eq!(outbox.list().unwrap(), vec![op(50)]);
    assert_eq!(outbox.load(&op(50)).unwrap().journal.len(), 1);
}

#[test]
fn concurrent_divergent_staging_exactly_one_wins() {
    let dir = TempDir::new().unwrap();
    let outbox = std::sync::Arc::new(ChainActionOutbox::new(dir.path()).unwrap());
    let d = std::sync::Arc::new(driver());

    let handles: Vec<_> = (0..8)
        .map(|i| {
            let ob = outbox.clone();
            let dd = d.clone();
            thread::spawn(move || {
                let req = dd.stage_request(&op(51), "w", "k", "dst", i, 100, 0);
                ob.stage(req).is_ok()
            })
        })
        .collect();
    let oks: usize = handles
        .into_iter()
        .map(|h| h.join().unwrap() as usize)
        .sum();
    assert_eq!(oks, 1, "exactly one divergent envelope wins");
    assert!(outbox.load(&op(51)).is_ok());
}

#[test]
fn concurrent_conflicting_outcomes_one_wins() {
    let dir = TempDir::new().unwrap();
    let outbox = std::sync::Arc::new(ChainActionOutbox::new(dir.path()).unwrap());
    let d = driver();
    stage(&outbox, &d, 52);
    sign(&outbox, &d, 52);
    outbox.record_broadcast_attempt(&op(52), 300).unwrap();

    let ob = outbox.clone();
    let accepted = thread::spawn(move || {
        ob.record_broadcast_outcome(&op(52), 400, 1, BroadcastOutcome::Accepted)
    });
    let ob2 = outbox.clone();
    let ambiguous = thread::spawn(move || {
        ob2.record_broadcast_outcome(&op(52), 400, 1, BroadcastOutcome::Ambiguous)
    });
    let r1 = accepted.join().unwrap();
    let r2 = ambiguous.join().unwrap();
    // Exactly one applies; the other is a conflict error.
    assert!(r1.is_ok() ^ r2.is_ok());
    let final_state = outbox.load(&op(52)).unwrap().state;
    assert!(final_state == ActionState::Sent || final_state == ActionState::Ambiguous);
    // And the journal holds exactly one outcome record (staged, signed,
    // attempt, outcome).
    let journal_len = outbox.load(&op(52)).unwrap().journal.len();
    assert_eq!(journal_len, 4);
}

#[test]
fn concurrent_sign_and_attempt_cannot_interleave() {
    let dir = TempDir::new().unwrap();
    let outbox = std::sync::Arc::new(ChainActionOutbox::new(dir.path()).unwrap());
    let d = std::sync::Arc::new(driver());
    stage(&outbox, &d, 53);

    let ob = outbox.clone();
    let dd = d.clone();
    let signer = thread::spawn(move || {
        let action = ob.load(&op(53)).unwrap();
        let payload = hex::decode(&action.envelope.payload_hex).unwrap();
        let artifact = dd.assemble_artifact(&payload);
        let signature = dd.fixture_sign(&payload);
        ob.record_signed(&op(53), 200, &signature, &artifact)
    });
    let ob2 = outbox.clone();
    let attempter = thread::spawn(move || ob2.record_broadcast_attempt(&op(53), 300));
    let (r1, r2) = (signer.join().unwrap(), attempter.join().unwrap());
    // Either sign-then-attempt-fails (attempt ran first) or attempt fails then
    // sign succeeds; never a recorded attempt without a signature.
    let action = outbox.load(&op(53)).unwrap();
    assert!(action.attempts.is_empty() || r1.is_ok());
    if r2.is_ok() {
        assert_eq!(action.state, ActionState::Signed);
    }
    assert!(r1.is_ok(), "signing always succeeds from staged");
}

#[test]
fn size_and_hex_validation_on_inputs() {
    let dir = TempDir::new().unwrap();
    let outbox = ChainActionOutbox::new(dir.path()).unwrap();
    let d = driver();

    let mut bad_id = d.stage_request(&op(60), "w", "k", "dst", 1, 100, 0);
    bad_id.operation_id = "not-hex".into();
    assert!(matches!(
        outbox.stage(bad_id).unwrap_err(),
        OutboxError::InvalidOperationId
    ));

    let mut bad_pkg = d.stage_request(&op(61), "w", "k", "dst", 1, 100, 0);
    bad_pkg.driver.package_hash = "zz".into();
    assert!(matches!(
        outbox.stage(bad_pkg).unwrap_err(),
        OutboxError::InvalidEnvelope(_)
    ));
}

#[test]
fn fixture_driver_end_to_end_orchestration() {
    // The fixture driver plays the orchestrator role: build, sign, dispatch
    // with a scripted timeout, reconcile by retry, confirm.
    let dir = TempDir::new().unwrap();
    let outbox = ChainActionOutbox::new(dir.path()).unwrap();
    let mut d = driver().with_script(vec![ScriptedOutcome::Timeout, ScriptedOutcome::Accept]);

    let request = d.stage_request(&op(70), "wallet-1", "key-ref-1", "dest-9", 42, 100, 0);
    outbox.stage(request).unwrap();
    let payload = hex::decode(&outbox.load(&op(70)).unwrap().envelope.payload_hex).unwrap();
    let artifact = d.assemble_artifact(&payload);
    let signature = d.fixture_sign(&payload);
    outbox
        .record_signed(&op(70), 200, &signature, &artifact)
        .unwrap();

    for attempt in 1..=2u64 {
        outbox
            .record_broadcast_attempt(&op(70), 300 + attempt)
            .unwrap();
        let outcome = d.next_broadcast_outcome();
        outbox
            .record_broadcast_outcome(&op(70), 400 + attempt, attempt, outcome)
            .unwrap();
    }
    assert_eq!(outbox.load(&op(70)).unwrap().state, ActionState::Sent);
    outbox
        .record_reconciliation(
            &op(70),
            900,
            ReconciliationOutcome::Confirmed {
                detail: "slot 9".into(),
            },
        )
        .unwrap();
    let done = outbox.load(&op(70)).unwrap();
    assert_eq!(done.state, ActionState::Confirmed);
    assert_eq!(done.attempts.len(), 2);
    // The retried attempt reused the exact same artifact digest.
    assert_eq!(
        done.attempts[0].artifact_digest_hex,
        done.attempts[1].artifact_digest_hex
    );
}
