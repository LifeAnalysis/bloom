//! Chain-neutral durable chain-action outbox.
//!
//! This crate implements the Machine-owned generic staged-action state machine
//! and persistence layer described by the verified chain Petal architecture:
//! an immutable staged envelope plus an append-only, digest-chained transition
//! journal. It is deliberately chain-neutral — it carries versioned bounded
//! bytes and canonical facts, never Solana- or EVM-specific types — and has no
//! dependency on the Petal VM, Broker, Signer, or any chain SDK.
//!
//! Semantics frozen by this crate:
//!
//! - Staging is idempotent: re-staging the identical envelope succeeds;
//!   re-staging the same operation id with different content is rejected.
//! - Signing happens exactly once. A second signature is never requested or
//!   recorded, including after an ambiguous broadcast.
//! - Broadcast retries reuse the exact persisted signed artifact; the retry
//!   path cannot substitute different bytes.
//! - A timeout after dispatch is `Ambiguous`: the effect is unknown, and only
//!   reconciliation (or expiry/quarantine) can leave that state.
//! - Recovery is deterministic: reopening an outbox replays and re-validates
//!   the journal, rejecting envelope mutation, journal mutation, and
//!   sequence gaps.
//!
//! # Rollback resistance (trusted high-water anchor)
//!
//! The journal alone is **tamper-evident but not rollback-resistant**: a
//! valid prefix of the digest chain is indistinguishable from the full chain,
//! so removal of complete trailing records is undetectable from the journal
//! alone. The [`Checkpoint`] high-water anchor closes that gap. Machine calls
//! [`ChainActionOutbox::checkpoint`] after durable transitions; recovery
//! treats the checkpoint as a floor and fails closed with
//! [`OutboxError::JournalRollbackDetected`] when the journal is shorter than
//! the pinned sequence, with [`OutboxError::CheckpointHeadMismatch`] when the
//! head digest differs, and with [`OutboxError::CheckpointDigestMismatch`]
//! when the checkpoint itself is mutated.
//!
//! With a checkpoint written, persistence is rollback-resistant **while the
//! checkpoint file remains trusted**: rollback detection holds as long as
//! the checkpoint is not itself rewritten. This is rollback resistance
//! within a stated trust boundary — the Machine-owned directory — not
//! adversarial rollback-proofness: an attacker with write access to
//! Machine-owned state can rewrite the journal and the checkpoint
//! consistently. Anchoring the latest sequence and digest in a separate
//! process or store remains available for deployments that need to remove
//! that residual as well.
//!
//! The fixture driver in [`fixture`] is a deterministic, non-cryptographic
//! test double used to exercise the outbox without any chain SDK.

#![forbid(unsafe_code)]

pub mod fixture;

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// The envelope schema version persisted with every staged action.
pub const ENVELOPE_SCHEMA: &str = "bloom.chain-action/1";

/// Maximum unsigned payload bytes accepted at staging.
pub const MAX_PAYLOAD_BYTES: usize = 4096;
/// Maximum signed-artifact bytes accepted at signing.
pub const MAX_ARTIFACT_BYTES: usize = 8192;
/// Maximum signature bytes accepted at signing.
pub const MAX_SIGNATURE_BYTES: usize = 256;
/// Maximum length of any bounded string field.
pub const MAX_STRING_BYTES: usize = 256;

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    let out: [u8; 32] = h.finalize().into();
    hex::encode(out)
}

fn is_lower_hex_64(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn validate_bounded(s: &str, what: &'static str) -> Result<(), OutboxError> {
    if s.is_empty() || s.len() > MAX_STRING_BYTES {
        return Err(OutboxError::InvalidEnvelope(format!(
            "{what} must contain 1-{MAX_STRING_BYTES} bytes"
        )));
    }
    Ok(())
}

/// Driver provenance bound into every staged action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriverBinding {
    /// Content-addressed driver package hash (64 lowercase hex).
    pub package_hash: String,
    /// Exact driver route that staged the action.
    pub route: String,
    pub abi_version: u32,
    pub state_schema: u32,
}

/// Chain identity bound into every staged action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainBinding {
    /// Chain family token, e.g. `solana`.
    pub family: String,
    /// Operator-configured chain profile name.
    pub profile: String,
    /// Claimed CAIP-2 reference; visible, not verifier-proven.
    pub claimed_caip2: String,
}

/// The immutable staged envelope, written once at staging.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub schema: String,
    /// Caller-generated 256-bit operation id (64 lowercase hex).
    pub operation_id: String,
    pub idempotency_key: String,
    pub driver: DriverBinding,
    pub wallet_id: String,
    /// Opaque, backend-qualified key reference (bounded string).
    pub key_ref: String,
    pub chain: ChainBinding,
    pub operation_class: String,
    pub crypto_suite: String,
    /// Unsigned payload bytes.
    pub payload_hex: String,
    /// SHA-256 of the payload bytes.
    pub payload_digest_hex: String,
    pub created_at_ms: u64,
    /// 0 means no expiry.
    pub expires_at_ms: u64,
}

impl Envelope {
    fn validate(&self) -> Result<(), OutboxError> {
        if self.schema != ENVELOPE_SCHEMA {
            return Err(OutboxError::EnvelopeSchema(self.schema.clone()));
        }
        if !is_lower_hex_64(&self.operation_id) {
            return Err(OutboxError::InvalidEnvelope(
                "operation_id must be 64 lowercase hex characters".into(),
            ));
        }
        if !is_lower_hex_64(&self.driver.package_hash) {
            return Err(OutboxError::InvalidEnvelope(
                "driver package_hash must be 64 lowercase hex characters".into(),
            ));
        }
        validate_bounded(&self.idempotency_key, "idempotency_key")?;
        validate_bounded(&self.driver.route, "driver route")?;
        validate_bounded(&self.wallet_id, "wallet_id")?;
        validate_bounded(&self.key_ref, "key_ref")?;
        validate_bounded(&self.chain.family, "chain family")?;
        validate_bounded(&self.chain.profile, "chain profile")?;
        validate_bounded(&self.chain.claimed_caip2, "claimed CAIP-2")?;
        validate_bounded(&self.operation_class, "operation_class")?;
        validate_bounded(&self.crypto_suite, "crypto_suite")?;
        if self.payload_digest_hex != sha256_hex(&self.payload_bytes()?) {
            return Err(OutboxError::PayloadDigestMismatch);
        }
        Ok(())
    }

    fn payload_bytes(&self) -> Result<Vec<u8>, OutboxError> {
        hex::decode(&self.payload_hex)
            .map_err(|e| OutboxError::InvalidEnvelope(format!("payload_hex: {e}")))
    }
}

/// A request to stage a new action. The payload digest is computed here;
/// everything else is copied verbatim into the immutable envelope.
#[derive(Debug, Clone)]
pub struct NewAction {
    pub operation_id: String,
    pub idempotency_key: String,
    pub driver: DriverBinding,
    pub wallet_id: String,
    pub key_ref: String,
    pub chain: ChainBinding,
    pub operation_class: String,
    pub crypto_suite: String,
    pub payload: Vec<u8>,
    pub created_at_ms: u64,
    pub expires_at_ms: u64,
}

/// Why the honest runtime refused to sign for freshness reasons. These are
/// liveness/consistency refusals against lagging or inconsistent providers;
/// they do not detect a consistently malicious RPC (see the Solana plan).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FreshnessReason {
    /// The staged blockhash is too old or reported invalid; restage with a
    /// fresh blockhash and a new approval.
    BlockhashRefreshRequired,
    /// Observations disagree (blockhash, height, validity, or commitment);
    /// the network view cannot be trusted enough to sign.
    NetworkObservationInconsistent,
    /// The remaining block-height validity window is too small to sign into.
    InsufficientValidityWindow,
}

/// One journal transition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Transition {
    /// First journal record; written with the immutable envelope.
    Staged,
    /// Pre-sign freshness refusal. Terminal for this action: signing is
    /// blocked and progress requires restaging under a new approval.
    FreshnessRefused { reason: FreshnessReason },
    /// Asserted fee observation for the exact staged payload, with the
    /// approved ceiling. Informational: fees are machine-asserted facts,
    /// never verifier-proven, and this records what was observed and bound.
    FeeObserved { lamports: u64, max_lamports: u64 },
    /// Exact signature and assembled signed artifact. Recorded at most once.
    Signed {
        signature_hex: String,
        artifact_hex: String,
        artifact_digest_hex: String,
    },
    /// Intent to dispatch the persisted artifact. Does not change the state.
    BroadcastAttempted {
        attempt: u64,
        artifact_digest_hex: String,
    },
    /// Provider accepted the dispatch.
    BroadcastAccepted { attempt: u64 },
    /// Timeout or unknown result after dispatch. The effect is unknown.
    BroadcastAmbiguous { attempt: u64 },
    /// Definitive rejection before any dispatch effect.
    BroadcastRejected { attempt: u64, reason: String },
    /// Reconciliation observed a terminal on-chain effect.
    Confirmed { detail: String },
    /// Reconciliation observed a definitive non-effect or failure.
    Failed { reason: String },
    /// User cancellation; only legal before anything is signed.
    Cancelled,
    /// Validity window elapsed. `proven_non_effect` records whether the
    /// expiry proves no dispatch occurred (true only when nothing was ever
    /// dispatched through this outbox).
    Expired { proven_non_effect: bool },
    /// Operator-visible quarantine for effects that cannot be resolved.
    Quarantined { reason: String },
}

/// The public lifecycle state of an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionState {
    Staged,
    Signed,
    Sent,
    Ambiguous,
    Confirmed,
    Failed,
    Cancelled,
    Expired,
    Quarantined,
    /// Pre-sign freshness refusal. Terminal; a fresh action must be staged.
    FreshnessRefused,
}

impl ActionState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Staged => "staged",
            Self::Signed => "signed",
            Self::Sent => "sent",
            Self::Ambiguous => "ambiguous",
            Self::Confirmed => "confirmed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Expired => "expired",
            Self::Quarantined => "quarantined",
            Self::FreshnessRefused => "freshness_refused",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Confirmed
                | Self::Failed
                | Self::Cancelled
                | Self::Expired
                | Self::Quarantined
                | Self::FreshnessRefused
        )
    }
}

impl fmt::Display for ActionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The persisted signed artifact of an action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedArtifact {
    pub signature: Vec<u8>,
    pub artifact: Vec<u8>,
    pub digest_hex: String,
}

/// One broadcast attempt and its recorded outcome, if any.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptView {
    pub attempt: u64,
    pub artifact_digest_hex: String,
    pub outcome: Option<AttemptOutcome>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AttemptOutcome {
    Accepted,
    Ambiguous,
    Rejected { reason: String },
}

/// The outcome a caller observed for a dispatch, passed to
/// [`ChainActionOutbox::record_broadcast_outcome`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BroadcastOutcome {
    Accepted,
    Ambiguous,
    Rejected { reason: String },
}

/// The outcome of reconciliation, passed to
/// [`ChainActionOutbox::record_reconciliation`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconciliationOutcome {
    Confirmed { detail: String },
    Failed { reason: String },
    Quarantined { reason: String },
}

/// A replayed action: immutable envelope plus derived lifecycle state.
#[derive(Debug, Clone)]
pub struct Action {
    pub envelope: Envelope,
    pub state: ActionState,
    pub artifact: Option<SignedArtifact>,
    pub attempts: Vec<AttemptView>,
    pub journal: Vec<Record>,
}

/// One digest-chained journal record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub seq: u64,
    pub at_ms: u64,
    pub transition: Transition,
    /// SHA-256 over the canonical serialization of this record's
    /// `{seq, at_ms, transition, prev_digest_hex}` tuple.
    pub prev_digest_hex: String,
    pub record_digest_hex: String,
}

#[derive(Debug, Error)]
pub enum OutboxError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("action '{0}' not found")]
    NotFound(String),
    #[error("invalid envelope: {0}")]
    InvalidEnvelope(String),
    #[error("envelope schema '{0}' is not {ENVELOPE_SCHEMA}")]
    EnvelopeSchema(String),
    #[error("envelope payload digest mismatch (envelope mutated on disk)")]
    PayloadDigestMismatch,
    #[error("signed artifact digest mismatch (journal mutated on disk)")]
    ArtifactDigestMismatch,
    #[error("journal record {0} digest mismatch (journal mutated on disk)")]
    JournalChainMismatch(u64),
    #[error("journal sequence gap: expected {expected}, found {found:?}")]
    SequenceGap { expected: u64, found: Option<u64> },
    #[error("first journal record is not 'staged'")]
    MissingStagedRecord,
    #[error("operation_id must be 64 lowercase hex characters")]
    InvalidOperationId,
    #[error("payload exceeds {MAX_PAYLOAD_BYTES} bytes: {0}")]
    PayloadTooLarge(usize),
    #[error("signed artifact exceeds {MAX_ARTIFACT_BYTES} bytes: {0}")]
    ArtifactTooLarge(usize),
    #[error("signature exceeds {MAX_SIGNATURE_BYTES} bytes: {0}")]
    SignatureTooLarge(usize),
    #[error("operation '{0}' already staged with different envelope content")]
    EnvelopeMismatch(String),
    #[error("action is already signed; a second signature is never recorded")]
    AlreadySigned,
    #[error("transition to '{to}' is not valid from state '{from}'")]
    InvalidTransition {
        from: &'static str,
        to: &'static str,
    },
    #[error("broadcast attempt {0} not found")]
    AttemptNotFound(u64),
    #[error("broadcast attempt {0} already has a different recorded outcome")]
    AttemptOutcomeConflict(u64),
    #[error("journal record {0} already exists")]
    JournalSeqExists(u64),
    #[error(
        "journal rollback detected: checkpoint pins seq {checkpoint_seq} but only {have} records exist"
    )]
    JournalRollbackDetected { have: u64, checkpoint_seq: u64 },
    #[error("checkpoint digest mismatch (checkpoint mutated on disk)")]
    CheckpointDigestMismatch,
    #[error("journal record {0} does not match the trusted checkpoint")]
    CheckpointHeadMismatch(u64),
}

/// A trusted high-water checkpoint pinning the journal head at write time.
///
/// The checkpoint is Machine-owned durable state written beside the journal.
/// Recovery treats it as a floor: a journal shorter than `seq`, or a head
/// record whose digest differs from `record_digest_hex`, is rollback or
/// tampering and fails closed. A journal that merely grew past the checkpoint
/// is fine — the checkpoint is a floor, not a ceiling. (An attacker with
/// write access to both files could rewrite them consistently; the trust
/// anchor is the Machine-owned directory permission, matching the EVM
/// outbox's threat model.)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub schema: String,
    pub operation_id: String,
    /// 1-based journal sequence this checkpoint pins.
    pub seq: u64,
    pub record_digest_hex: String,
    pub at_ms: u64,
    pub checkpoint_digest_hex: String,
}

pub const CHECKPOINT_SCHEMA: &str = "bloom.chain-action.checkpoint/1";

/// The chain-neutral durable outbox.
///
/// All mutating operations are serialized by an in-process mutex and persist
/// atomically (write-temp-then-rename). Concurrent staging of the identical
/// envelope is idempotent; concurrent staging of divergent envelopes for the
/// same operation id is rejected for every loser. Cross-process mutation of
/// the same root directory is out of scope for this slice, matching the
/// existing EVM outbox.
#[derive(Debug)]
pub struct ChainActionOutbox {
    root: PathBuf,
    lock: Mutex<()>,
    tmp_counter: AtomicU64,
}

impl ChainActionOutbox {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, OutboxError> {
        let root = root.into();
        fs::create_dir_all(root.join("actions"))?;
        Ok(Self {
            root,
            lock: Mutex::new(()),
            tmp_counter: AtomicU64::new(0),
        })
    }

    fn action_dir(&self, operation_id: &str) -> PathBuf {
        self.root.join("actions").join(operation_id)
    }

    fn journal_dir(&self, operation_id: &str) -> PathBuf {
        self.action_dir(operation_id).join("journal")
    }

    fn tmp_path(&self, path: &Path) -> PathBuf {
        let n = self.tmp_counter.fetch_add(1, Ordering::Relaxed);
        let mut name = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        name.push_str(format!(".tmp-{n}").as_str());
        path.with_file_name(name)
    }

    /// Atomically create `path` with `bytes`. Returns `Ok(false)` if the file
    /// already exists (used for crash-safe first-writer-wins semantics).
    fn write_new(&self, path: &Path, bytes: &[u8]) -> Result<bool, OutboxError> {
        use std::io::Write;
        let tmp = self.tmp_path(path);
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create_new(true);
        let mut file = match opts.open(&tmp) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let _ = fs::remove_file(&tmp);
                return self.write_new(path, bytes);
            }
            Err(e) => return Err(e.into()),
        };
        file.write_all(bytes)?;
        file.sync_all()?;
        match fs::rename(&tmp, path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let _ = fs::remove_file(&tmp);
                Ok(false)
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Stage a new action. Idempotent for byte-identical envelopes; rejects a
    /// divergent envelope for an existing operation id.
    pub fn stage(&self, request: NewAction) -> Result<Action, OutboxError> {
        if !is_lower_hex_64(&request.operation_id) {
            return Err(OutboxError::InvalidOperationId);
        }
        if request.payload.len() > MAX_PAYLOAD_BYTES {
            return Err(OutboxError::PayloadTooLarge(request.payload.len()));
        }
        let envelope = Envelope {
            schema: ENVELOPE_SCHEMA.to_string(),
            operation_id: request.operation_id,
            idempotency_key: request.idempotency_key,
            driver: request.driver,
            wallet_id: request.wallet_id,
            key_ref: request.key_ref,
            chain: request.chain,
            operation_class: request.operation_class,
            crypto_suite: request.crypto_suite,
            payload_hex: hex::encode(&request.payload),
            payload_digest_hex: sha256_hex(&request.payload),
            created_at_ms: request.created_at_ms,
            expires_at_ms: request.expires_at_ms,
        };
        envelope.validate()?;

        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let dir = self.action_dir(&envelope.operation_id);
        fs::create_dir_all(&dir)?;
        let envelope_path = dir.join("envelope.json");
        let envelope_bytes = serde_json::to_vec_pretty(&envelope)?;

        if envelope_path.exists() {
            let stored = fs::read(&envelope_path)?;
            if stored != envelope_bytes {
                return Err(OutboxError::EnvelopeMismatch(envelope.operation_id));
            }
        } else {
            self.write_new(&envelope_path, &envelope_bytes)?;
            // Lost an identical race: fall through and verify below.
            let stored = fs::read(&envelope_path)?;
            if stored != envelope_bytes {
                return Err(OutboxError::EnvelopeMismatch(envelope.operation_id));
            }
        }

        let journal = self.journal_dir(&envelope.operation_id);
        fs::create_dir_all(&journal)?;
        let staged = self.build_record(1, request.created_at_ms, Transition::Staged, "");
        let staged_bytes = serde_json::to_vec_pretty(&staged)?;
        if !journal.join("00000001.json").exists() {
            self.write_new(&journal.join("00000001.json"), &staged_bytes)?;
        }

        self.load(&envelope.operation_id)
    }

    /// Load and fully validate an action: envelope digest, journal chain,
    /// sequence contiguity, and transition legality.
    pub fn load(&self, operation_id: &str) -> Result<Action, OutboxError> {
        if !is_lower_hex_64(operation_id) {
            return Err(OutboxError::InvalidOperationId);
        }
        let envelope_path = self.action_dir(operation_id).join("envelope.json");
        if !envelope_path.exists() {
            return Err(OutboxError::NotFound(operation_id.to_string()));
        }
        let envelope: Envelope = serde_json::from_slice(&fs::read(&envelope_path)?)?;
        if envelope.operation_id != operation_id {
            return Err(OutboxError::InvalidEnvelope(
                "envelope operation_id does not match its directory".into(),
            ));
        }
        envelope.validate()?;

        let journal = self.journal_dir(operation_id);
        let mut seqs: Vec<(u64, PathBuf)> = Vec::new();
        for entry in fs::read_dir(&journal)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.ends_with(".json") {
                continue;
            }
            let stem = name.trim_end_matches(".json");
            let seq: u64 = stem
                .parse()
                .map_err(|_| OutboxError::InvalidEnvelope(format!("bad journal file {name}")))?;
            seqs.push((seq, entry.path()));
        }
        seqs.sort_by_key(|(s, _)| *s);

        let mut records = Vec::with_capacity(seqs.len());
        let mut prev_digest = String::new();
        for (idx, (seq, path)) in seqs.into_iter().enumerate() {
            let expected = idx as u64 + 1;
            if seq != expected {
                return Err(OutboxError::SequenceGap {
                    expected,
                    found: Some(seq),
                });
            }
            let record: Record = serde_json::from_slice(&fs::read(&path)?)?;
            if record.seq != seq {
                return Err(OutboxError::SequenceGap {
                    expected: seq,
                    found: Some(record.seq),
                });
            }
            if record.prev_digest_hex != prev_digest {
                return Err(OutboxError::JournalChainMismatch(seq));
            }
            let canonical = RecordDigestInput::canonical(
                record.seq,
                record.at_ms,
                &record.transition,
                &record.prev_digest_hex,
            );
            if record.record_digest_hex != sha256_hex(&canonical) {
                return Err(OutboxError::JournalChainMismatch(seq));
            }
            prev_digest = record.record_digest_hex.clone();
            records.push(record);
        }
        if records.is_empty() || !matches!(records[0].transition, Transition::Staged) {
            return Err(OutboxError::MissingStagedRecord);
        }

        // Trusted high-water checkpoint: a floor under the journal head.
        let cp_path = self.action_dir(operation_id).join("checkpoint.json");
        if cp_path.exists() {
            let cp: Checkpoint = serde_json::from_slice(&fs::read(&cp_path)?)?;
            if cp.schema != CHECKPOINT_SCHEMA || cp.operation_id != operation_id {
                return Err(OutboxError::CheckpointDigestMismatch);
            }
            let canonical = CheckpointDigestInput::canonical(&cp);
            if cp.checkpoint_digest_hex != sha256_hex(&canonical) {
                return Err(OutboxError::CheckpointDigestMismatch);
            }
            if (records.len() as u64) < cp.seq {
                return Err(OutboxError::JournalRollbackDetected {
                    have: records.len() as u64,
                    checkpoint_seq: cp.seq,
                });
            }
            if records[(cp.seq - 1) as usize].record_digest_hex != cp.record_digest_hex {
                return Err(OutboxError::CheckpointHeadMismatch(cp.seq));
            }
        }

        replay(envelope, records)
    }

    /// List all staged operation ids, sorted.
    pub fn list(&self) -> Result<Vec<String>, OutboxError> {
        let mut out = Vec::new();
        let dir = self.root.join("actions");
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            if let Some(name) = entry.file_name().to_str() {
                out.push(name.to_string());
            }
        }
        out.sort();
        Ok(out)
    }

    /// Record the exact signature and assembled signed artifact. Legal exactly
    /// once, from `Staged`. Recording identical bytes again is idempotent;
    /// anything else is rejected — a second signature is never recorded.
    pub fn record_signed(
        &self,
        operation_id: &str,
        now_ms: u64,
        signature: &[u8],
        artifact: &[u8],
    ) -> Result<Action, OutboxError> {
        if signature.len() > MAX_SIGNATURE_BYTES {
            return Err(OutboxError::SignatureTooLarge(signature.len()));
        }
        if artifact.len() > MAX_ARTIFACT_BYTES {
            return Err(OutboxError::ArtifactTooLarge(artifact.len()));
        }
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let action = self.load(operation_id)?;
        match action.state {
            ActionState::Staged => {}
            // Any state with a persisted artifact refuses a second signature
            // for the lifetime of the action: identical bytes are idempotent,
            // divergent bytes are `AlreadySigned`.
            ActionState::Signed | ActionState::Ambiguous | ActionState::Sent => {
                let existing = action.artifact.as_ref().expect("artifact present");
                if existing.signature == signature && existing.artifact == artifact {
                    return Ok(action); // idempotent
                }
                return Err(OutboxError::AlreadySigned);
            }
            other => {
                return Err(OutboxError::InvalidTransition {
                    from: other.as_str(),
                    to: "signed",
                });
            }
        }
        let digest = sha256_hex(artifact);
        self.append(
            operation_id,
            now_ms,
            Transition::Signed {
                signature_hex: hex::encode(signature),
                artifact_hex: hex::encode(artifact),
                artifact_digest_hex: digest,
            },
        )?;
        self.load(operation_id)
    }

    /// Record intent to dispatch the persisted artifact. Legal from `Signed`
    /// and, as an identical-byte retry, from `Ambiguous`. The attempt always
    /// references the artifact digest persisted at signing, so a retry cannot
    /// substitute different bytes.
    pub fn record_broadcast_attempt(
        &self,
        operation_id: &str,
        now_ms: u64,
    ) -> Result<Action, OutboxError> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let action = self.load(operation_id)?;
        match action.state {
            ActionState::Signed | ActionState::Ambiguous => {}
            other => {
                return Err(OutboxError::InvalidTransition {
                    from: other.as_str(),
                    to: "broadcast_attempted",
                });
            }
        }
        let artifact = action.artifact.as_ref().expect("artifact present");
        let attempt = action.attempts.len() as u64 + 1;
        self.append(
            operation_id,
            now_ms,
            Transition::BroadcastAttempted {
                attempt,
                artifact_digest_hex: artifact.digest_hex.clone(),
            },
        )?;
        self.load(operation_id)
    }

    /// Record the observed outcome of a broadcast attempt. `Ambiguous` moves
    /// the action to the `Ambiguous` state; `Accepted` to `Sent`; a definitive
    /// `Rejected` to `Failed`.
    pub fn record_broadcast_outcome(
        &self,
        operation_id: &str,
        now_ms: u64,
        attempt: u64,
        outcome: BroadcastOutcome,
    ) -> Result<Action, OutboxError> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let action = self.load(operation_id)?;
        let view = action
            .attempts
            .iter()
            .find(|a| a.attempt == attempt)
            .ok_or(OutboxError::AttemptNotFound(attempt))?;
        if let Some(existing) = &view.outcome {
            let same = match (&existing, &outcome) {
                (AttemptOutcome::Accepted, BroadcastOutcome::Accepted) => true,
                (AttemptOutcome::Ambiguous, BroadcastOutcome::Ambiguous) => true,
                (
                    AttemptOutcome::Rejected { reason: a },
                    BroadcastOutcome::Rejected { reason: b },
                ) => a == b,
                _ => false,
            };
            return if same {
                Ok(action) // idempotent
            } else {
                Err(OutboxError::AttemptOutcomeConflict(attempt))
            };
        }
        match action.state {
            ActionState::Signed | ActionState::Ambiguous => {}
            other => {
                return Err(OutboxError::InvalidTransition {
                    from: other.as_str(),
                    to: "broadcast_outcome",
                });
            }
        }
        let transition = match outcome {
            BroadcastOutcome::Accepted => Transition::BroadcastAccepted { attempt },
            BroadcastOutcome::Ambiguous => Transition::BroadcastAmbiguous { attempt },
            BroadcastOutcome::Rejected { reason } => {
                Transition::BroadcastRejected { attempt, reason }
            }
        };
        self.append(operation_id, now_ms, transition)?;
        self.load(operation_id)
    }

    /// Record a reconciliation result. Confirmed/Failed/Quarantined are legal
    /// from `Sent` and `Ambiguous` (Quarantined also from `Signed`).
    pub fn record_reconciliation(
        &self,
        operation_id: &str,
        now_ms: u64,
        outcome: ReconciliationOutcome,
    ) -> Result<Action, OutboxError> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let action = self.load(operation_id)?;
        let transition = match (&action.state, outcome) {
            (
                ActionState::Sent | ActionState::Ambiguous,
                ReconciliationOutcome::Confirmed { detail },
            ) => Transition::Confirmed { detail },
            (
                ActionState::Sent | ActionState::Ambiguous,
                ReconciliationOutcome::Failed { reason },
            ) => Transition::Failed { reason },
            (
                ActionState::Signed | ActionState::Sent | ActionState::Ambiguous,
                ReconciliationOutcome::Quarantined { reason },
            ) => Transition::Quarantined { reason },
            (state, _) => {
                return Err(OutboxError::InvalidTransition {
                    from: state.as_str(),
                    to: "reconciliation",
                });
            }
        };
        self.append(operation_id, now_ms, transition)?;
        self.load(operation_id)
    }

    /// Record the asserted fee observation and its approved ceiling. Legal
    /// exactly once, from `Staged`, before anything is signed.
    pub fn record_fee_observed(
        &self,
        operation_id: &str,
        now_ms: u64,
        lamports: u64,
        max_lamports: u64,
    ) -> Result<Action, OutboxError> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let action = self.load(operation_id)?;
        if action
            .journal
            .iter()
            .any(|r| matches!(r.transition, Transition::FeeObserved { .. }))
        {
            return Err(OutboxError::InvalidTransition {
                from: action.state.as_str(),
                to: "fee_observed",
            });
        }
        match action.state {
            ActionState::Staged => {}
            other => {
                return Err(OutboxError::InvalidTransition {
                    from: other.as_str(),
                    to: "fee_observed",
                });
            }
        }
        self.append(
            operation_id,
            now_ms,
            Transition::FeeObserved {
                lamports,
                max_lamports,
            },
        )?;
        self.load(operation_id)
    }

    /// Cancel an action. Legal only from `Staged`: once a signature exists,
    /// cancellation cannot prove a non-effect.
    pub fn cancel(&self, operation_id: &str, now_ms: u64) -> Result<Action, OutboxError> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let action = self.load(operation_id)?;
        match action.state {
            ActionState::Staged => {}
            other => {
                return Err(OutboxError::InvalidTransition {
                    from: other.as_str(),
                    to: "cancelled",
                });
            }
        }
        self.append(operation_id, now_ms, Transition::Cancelled)?;
        self.load(operation_id)
    }

    /// Record a pre-sign freshness refusal. Terminal: legal only from
    /// `Staged`, blocks signing, and requires restaging under a new approval.
    pub fn refuse_for_freshness(
        &self,
        operation_id: &str,
        now_ms: u64,
        reason: FreshnessReason,
    ) -> Result<Action, OutboxError> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let action = self.load(operation_id)?;
        match action.state {
            ActionState::Staged => {}
            other => {
                return Err(OutboxError::InvalidTransition {
                    from: other.as_str(),
                    to: "freshness_refused",
                });
            }
        }
        self.append(
            operation_id,
            now_ms,
            Transition::FreshnessRefused { reason },
        )?;
        self.load(operation_id)
    }

    /// Write a trusted high-water checkpoint pinning the current journal head.
    pub fn checkpoint(&self, operation_id: &str, now_ms: u64) -> Result<Checkpoint, OutboxError> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let action = self.load(operation_id)?;
        let last = action
            .journal
            .last()
            .ok_or(OutboxError::MissingStagedRecord)?;
        let cp = Checkpoint {
            schema: CHECKPOINT_SCHEMA.to_string(),
            operation_id: operation_id.to_string(),
            seq: last.seq,
            record_digest_hex: last.record_digest_hex.clone(),
            at_ms: now_ms,
            checkpoint_digest_hex: String::new(),
        };
        let canonical = CheckpointDigestInput::canonical(&cp);
        let mut cp = cp;
        cp.checkpoint_digest_hex = sha256_hex(&canonical);
        let path = self.action_dir(operation_id).join("checkpoint.json");
        let bytes = serde_json::to_vec_pretty(&cp)?;
        let tmp = self.tmp_path(&path);
        fs::write(&tmp, &bytes)?;
        fs::rename(&tmp, &path)?;
        Ok(cp)
    }

    /// Transition every non-terminal action whose validity window has elapsed
    /// to `Expired`. Returns the ids that were expired. Expiry proves a
    /// non-effect only when nothing was ever dispatched through this outbox.
    pub fn sweep_expired(&self, now_ms: u64) -> Result<Vec<String>, OutboxError> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut expired = Vec::new();
        for id in self.list()? {
            let action = self.load(&id)?;
            if action.envelope.expires_at_ms == 0
                || now_ms < action.envelope.expires_at_ms
                || action.state.is_terminal()
            {
                continue;
            }
            let proven_non_effect =
                matches!(action.state, ActionState::Staged) || action.attempts.is_empty();
            self.append(&id, now_ms, Transition::Expired { proven_non_effect })?;
            expired.push(id);
        }
        Ok(expired)
    }

    fn build_record(
        &self,
        seq: u64,
        at_ms: u64,
        transition: Transition,
        prev_digest_hex: &str,
    ) -> Record {
        let canonical = RecordDigestInput::canonical(seq, at_ms, &transition, prev_digest_hex);
        Record {
            seq,
            at_ms,
            transition,
            prev_digest_hex: prev_digest_hex.to_string(),
            record_digest_hex: sha256_hex(&canonical),
        }
    }

    fn append(
        &self,
        operation_id: &str,
        now_ms: u64,
        transition: Transition,
    ) -> Result<(), OutboxError> {
        let action = self.load(operation_id)?;
        let seq = action.journal.len() as u64 + 1;
        let prev = action
            .journal
            .last()
            .map(|r| r.record_digest_hex.clone())
            .unwrap_or_default();
        let record = self.build_record(seq, now_ms, transition, &prev);
        let path = self
            .journal_dir(operation_id)
            .join(format!("{seq:08}.json"));
        let bytes = serde_json::to_vec_pretty(&record)?;
        if !self.write_new(&path, &bytes)? {
            return Err(OutboxError::JournalSeqExists(seq));
        }
        Ok(())
    }
}

/// Canonical digest preimage for a journal record.
#[derive(Serialize)]
struct RecordDigestInput<'a> {
    seq: u64,
    at_ms: u64,
    transition: &'a Transition,
    prev_digest_hex: &'a str,
}

impl<'a> RecordDigestInput<'a> {
    fn canonical(
        seq: u64,
        at_ms: u64,
        transition: &'a Transition,
        prev_digest_hex: &'a str,
    ) -> Vec<u8> {
        serde_json::to_vec(&Self {
            seq,
            at_ms,
            transition,
            prev_digest_hex,
        })
        .expect("record digest input serializes")
    }
}

/// Canonical digest preimage for a checkpoint (digest field excluded).
#[derive(Serialize)]
struct CheckpointDigestInput<'a> {
    schema: &'a str,
    operation_id: &'a str,
    seq: u64,
    record_digest_hex: &'a str,
    at_ms: u64,
}

impl<'a> CheckpointDigestInput<'a> {
    fn canonical(cp: &'a Checkpoint) -> Vec<u8> {
        serde_json::to_vec(&Self {
            schema: &cp.schema,
            operation_id: &cp.operation_id,
            seq: cp.seq,
            record_digest_hex: &cp.record_digest_hex,
            at_ms: cp.at_ms,
        })
        .expect("checkpoint digest input serializes")
    }
}

/// Replay a validated journal into an [`Action`], enforcing every transition.
fn replay(envelope: Envelope, records: Vec<Record>) -> Result<Action, OutboxError> {
    let mut state = ActionState::Staged;
    let mut artifact: Option<SignedArtifact> = None;
    let mut attempts: Vec<AttemptView> = Vec::new();

    for record in &records {
        match &record.transition {
            Transition::Staged => {
                if record.seq != 1 {
                    return Err(OutboxError::MissingStagedRecord);
                }
            }
            Transition::Signed {
                signature_hex,
                artifact_hex,
                artifact_digest_hex,
            } => {
                if state != ActionState::Staged || artifact.is_some() {
                    return Err(OutboxError::InvalidTransition {
                        from: state.as_str(),
                        to: "signed",
                    });
                }
                let signature = hex::decode(signature_hex)
                    .map_err(|e| OutboxError::InvalidEnvelope(format!("signature_hex: {e}")))?;
                let bytes = hex::decode(artifact_hex)
                    .map_err(|e| OutboxError::InvalidEnvelope(format!("artifact_hex: {e}")))?;
                if signature.len() > MAX_SIGNATURE_BYTES || bytes.len() > MAX_ARTIFACT_BYTES {
                    return Err(OutboxError::InvalidEnvelope(
                        "oversized signed material".into(),
                    ));
                }
                if sha256_hex(&bytes) != *artifact_digest_hex {
                    return Err(OutboxError::ArtifactDigestMismatch);
                }
                artifact = Some(SignedArtifact {
                    signature,
                    artifact: bytes,
                    digest_hex: artifact_digest_hex.clone(),
                });
                state = ActionState::Signed;
            }
            Transition::BroadcastAttempted {
                attempt,
                artifact_digest_hex,
            } => {
                if state != ActionState::Signed && state != ActionState::Ambiguous {
                    return Err(OutboxError::InvalidTransition {
                        from: state.as_str(),
                        to: "broadcast_attempted",
                    });
                }
                let current = artifact.as_ref().expect("artifact present");
                if *artifact_digest_hex != current.digest_hex {
                    return Err(OutboxError::ArtifactDigestMismatch);
                }
                let next = attempts.len() as u64 + 1;
                if *attempt != next {
                    return Err(OutboxError::AttemptNotFound(*attempt));
                }
                attempts.push(AttemptView {
                    attempt: *attempt,
                    artifact_digest_hex: artifact_digest_hex.clone(),
                    outcome: None,
                });
            }
            Transition::BroadcastAccepted { attempt } => {
                finish_attempt(
                    &mut attempts,
                    &mut state,
                    *attempt,
                    AttemptOutcome::Accepted,
                    ActionState::Sent,
                    "sent",
                )?;
            }
            Transition::BroadcastAmbiguous { attempt } => {
                finish_attempt(
                    &mut attempts,
                    &mut state,
                    *attempt,
                    AttemptOutcome::Ambiguous,
                    ActionState::Ambiguous,
                    "ambiguous",
                )?;
            }
            Transition::BroadcastRejected { attempt, reason } => {
                finish_attempt(
                    &mut attempts,
                    &mut state,
                    *attempt,
                    AttemptOutcome::Rejected {
                        reason: reason.clone(),
                    },
                    ActionState::Failed,
                    "failed",
                )?;
            }
            Transition::Confirmed { .. } => {
                require(
                    &state,
                    &[ActionState::Sent, ActionState::Ambiguous],
                    "confirmed",
                )?;
                state = ActionState::Confirmed;
            }
            Transition::Failed { .. } => {
                require(
                    &state,
                    &[ActionState::Sent, ActionState::Ambiguous],
                    "failed",
                )?;
                state = ActionState::Failed;
            }
            Transition::Cancelled => {
                require(&state, &[ActionState::Staged], "cancelled")?;
                state = ActionState::Cancelled;
            }
            Transition::FreshnessRefused { .. } => {
                require(&state, &[ActionState::Staged], "freshness_refused")?;
                state = ActionState::FreshnessRefused;
            }
            Transition::FeeObserved { .. } => {
                // Informational, no lifecycle effect; staging-only.
                require(&state, &[ActionState::Staged], "fee_observed")?;
            }
            Transition::Expired { .. } => {
                require(
                    &state,
                    &[
                        ActionState::Staged,
                        ActionState::Signed,
                        ActionState::Sent,
                        ActionState::Ambiguous,
                    ],
                    "expired",
                )?;
                state = ActionState::Expired;
            }
            Transition::Quarantined { .. } => {
                require(
                    &state,
                    &[
                        ActionState::Signed,
                        ActionState::Sent,
                        ActionState::Ambiguous,
                    ],
                    "quarantined",
                )?;
                state = ActionState::Quarantined;
            }
        }
    }

    Ok(Action {
        envelope,
        state,
        artifact,
        attempts,
        journal: records,
    })
}

fn require(
    state: &ActionState,
    allowed: &[ActionState],
    to: &'static str,
) -> Result<(), OutboxError> {
    if allowed.contains(state) {
        Ok(())
    } else {
        Err(OutboxError::InvalidTransition {
            from: state.as_str(),
            to,
        })
    }
}

fn finish_attempt(
    attempts: &mut [AttemptView],
    state: &mut ActionState,
    attempt: u64,
    outcome: AttemptOutcome,
    next: ActionState,
    to: &'static str,
) -> Result<(), OutboxError> {
    require(state, &[ActionState::Signed, ActionState::Ambiguous], to)?;
    let view = attempts
        .iter_mut()
        .find(|a| a.attempt == attempt)
        .ok_or(OutboxError::AttemptNotFound(attempt))?;
    if view.outcome.is_some() {
        return Err(OutboxError::AttemptOutcomeConflict(attempt));
    }
    view.outcome = Some(outcome);
    *state = next;
    Ok(())
}
