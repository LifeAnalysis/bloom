//! Persistence for staged / sent / failed Solana transfers.
//!
//! Layout (identical to `bloom-tx`'s outbox):
//! `<home>/outbox/<wallet>/<chain>/{pending,sent,failed}/<id>/...`
//!
//! `intent.json` is write-once; the mined-outcome sibling is `receipt.json`,
//! written only by the reconciliation loop. Broadcast attempts carry a marker
//! (`broadcast_attempted.json`) plus a `raw_tx` blob whose hash is bound into
//! the marker, so a retry can never substitute different bytes.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::types::{
    RECEIPT_FILE, SolanaReceipt, SolanaSentEntry, SolanaTxStatus, StagedSolanaTransfer,
};

#[derive(Debug, Error)]
pub enum OutboxError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("staged transfer '{id}' is in '{actual}', not '{expected}'")]
    StateMismatch {
        id: String,
        expected: &'static str,
        actual: &'static str,
    },
    #[error("invalid id '{0}'")]
    InvalidId(String),
    #[error("invalid wallet '{0}'")]
    InvalidWallet(String),
    #[error("invalid chain '{0}'")]
    InvalidChain(String),
    #[error("raw transaction bytes do not match the recorded hash")]
    RawTxHashMismatch,
    #[error("{0}")]
    Other(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SolanaOutboxState {
    Pending,
    Sent,
    Failed,
}

impl SolanaOutboxState {
    pub fn dirname(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Sent => "sent",
            Self::Failed => "failed",
        }
    }

    /// Map a status to its on-disk state (used by the stage/confirm paths
    /// once the transaction engine lands; see the plan).
    #[allow(dead_code)]
    pub fn from_status(s: &SolanaTxStatus) -> Self {
        match s {
            SolanaTxStatus::Pending => Self::Pending,
            SolanaTxStatus::Sent | SolanaTxStatus::Success => Self::Sent,
            SolanaTxStatus::Failed | SolanaTxStatus::Cancelled => Self::Failed,
        }
    }

    /// Parse an on-disk directory name back into a state (used by the VFS
    /// projection).
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "sent" => Some(Self::Sent),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// A located entry: its persisted state, staged record, and directory.
#[derive(Debug, Clone)]
pub struct SolanaOutboxEntry {
    pub state: SolanaOutboxState,
    pub staged: StagedSolanaTransfer,
    pub dir: PathBuf,
}

/// A broadcast attempt's durable marker, mirroring `bloom-tx`'s
/// `BroadcastAttempt` (without a nonce — Solana has none).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SolanaBroadcastAttempt {
    pub schema: String,
    pub signature: String,
    pub raw_tx_blake3: String,
    pub raw_tx_path: String,
    pub fee_payer: String,
    pub destination: String,
    pub lamports: u64,
    pub blockhash: String,
    pub created_ms: u128,
}

pub const BROADCAST_ATTEMPT_FILE: &str = "broadcast_attempted.json";
pub const BROADCAST_RAW_TX: &str = "raw_tx";
const BROADCAST_SCHEMA: &str = "bloom.solana-broadcast-attempt/1";

#[derive(Clone)]
pub struct SolanaOutbox {
    inner: Arc<OutboxInner>,
}

struct OutboxInner {
    root: PathBuf,
    next_id: RwLock<u64>,
}

impl SolanaOutbox {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, OutboxError> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        Ok(Self {
            inner: Arc::new(OutboxInner {
                root,
                next_id: RwLock::new(1),
            }),
        })
    }

    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    fn validate_segment(seg: &str) -> Result<(), OutboxError> {
        if seg.is_empty() || seg.contains('/') || seg.contains('\\') || seg == "." || seg == ".." {
            return Err(OutboxError::InvalidWallet(seg.into()));
        }
        Ok(())
    }

    pub fn wallet_chain_dir(&self, wallet: &str, chain: &str) -> Result<PathBuf, OutboxError> {
        Self::validate_segment(wallet).map_err(|_| OutboxError::InvalidWallet(wallet.into()))?;
        Self::validate_segment(chain).map_err(|_| OutboxError::InvalidChain(chain.into()))?;
        Ok(self.inner.root.join(wallet).join(chain))
    }

    fn state_dir(
        &self,
        wallet: &str,
        chain: &str,
        state: SolanaOutboxState,
    ) -> Result<PathBuf, OutboxError> {
        Ok(self.wallet_chain_dir(wallet, chain)?.join(state.dirname()))
    }

    /// Allocate a fresh id like `0001-12345` (mirrors `bloom-tx`).
    pub fn allocate_id(&self) -> String {
        let mut g = self.inner.next_id.write();
        let id = *g;
        *g = id.wrapping_add(1);
        let suffix = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_millis() % 100_000)
            .unwrap_or(0);
        format!("{id:04}-{suffix:05}")
    }

    /// Persist a staged transfer in `pending/<id>/` along with its plan file.
    /// `intent.json` is write-once; `plan.md` is caller-owned.
    pub fn write_pending(
        &self,
        staged: &StagedSolanaTransfer,
        plan_md: &str,
    ) -> Result<PathBuf, OutboxError> {
        let dir = self
            .state_dir(&staged.wallet, &staged.chain, SolanaOutboxState::Pending)?
            .join(&staged.id);
        fs::create_dir_all(&dir)?;
        fs::write(dir.join("intent.json"), serde_json::to_vec_pretty(staged)?)?;
        fs::write(dir.join("plan.md"), plan_md.as_bytes())?;
        Ok(dir)
    }

    /// Write the broadcast-attempt marker and its raw-tx blob (mode 0600),
    /// binding the raw bytes' blake3 hash into the marker.
    pub fn write_broadcast_attempt(
        &self,
        entry: &SolanaOutboxEntry,
        signature: &str,
        raw_tx: &[u8],
        created_ms: u128,
    ) -> Result<(), OutboxError> {
        let raw_blake3 = blake3_hash(raw_tx);
        let attempt = SolanaBroadcastAttempt {
            schema: BROADCAST_SCHEMA.to_string(),
            signature: signature.to_string(),
            raw_tx_blake3: raw_blake3,
            raw_tx_path: BROADCAST_RAW_TX.to_string(),
            fee_payer: entry.staged.fee_payer.clone(),
            destination: entry.staged.destination.clone(),
            lamports: entry.staged.lamports,
            blockhash: entry.staged.blockhash.clone(),
            created_ms,
        };
        fs::write(
            entry.dir.join(BROADCAST_ATTEMPT_FILE),
            serde_json::to_vec_pretty(&attempt)?,
        )?;
        let path = entry.dir.join(BROADCAST_RAW_TX);
        let mut opts = fs::OpenOptions::new();
        opts.create(true).write(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts.open(path)?;
        file.write_all(raw_tx)?;
        Ok(())
    }

    /// Read the recorded raw-tx bytes for an entry, verifying their blake3
    /// hash against the marker (a retry cannot substitute different bytes).
    pub fn read_broadcast_raw_tx(&self, entry: &SolanaOutboxEntry) -> Result<Vec<u8>, OutboxError> {
        let attempt: SolanaBroadcastAttempt =
            serde_json::from_slice(&fs::read(entry.dir.join(BROADCAST_ATTEMPT_FILE))?)?;
        let raw = fs::read(entry.dir.join(BROADCAST_RAW_TX))?;
        if blake3_hash(&raw) != attempt.raw_tx_blake3 {
            return Err(OutboxError::RawTxHashMismatch);
        }
        Ok(raw)
    }

    /// Move `pending/<id>` → `<new_state>/<id>` (atomic via `fs::rename`).
    pub fn transition(
        &self,
        entry: &SolanaOutboxEntry,
        new_state: SolanaOutboxState,
    ) -> Result<PathBuf, OutboxError> {
        if entry.state == new_state {
            return Ok(entry.dir.clone());
        }
        let target_parent = self.state_dir(&entry.staged.wallet, &entry.staged.chain, new_state)?;
        fs::create_dir_all(&target_parent)?;
        let target = target_parent.join(&entry.staged.id);
        if target.exists() {
            fs::remove_dir_all(&target)?;
        }
        fs::rename(&entry.dir, &target)?;
        if matches!(
            new_state,
            SolanaOutboxState::Sent | SolanaOutboxState::Failed
        ) {
            let _ = fs::remove_file(target.join(BROADCAST_RAW_TX));
        }
        Ok(target)
    }

    /// Search for `id` across pending/sent/failed, returning the first hit.
    pub fn read(
        &self,
        wallet: &str,
        chain: &str,
        id: &str,
    ) -> Result<SolanaOutboxEntry, OutboxError> {
        for state in [
            SolanaOutboxState::Pending,
            SolanaOutboxState::Sent,
            SolanaOutboxState::Failed,
        ] {
            let dir = self.state_dir(wallet, chain, state)?.join(id);
            if dir.join("intent.json").exists() {
                let staged = serde_json::from_slice(&fs::read(dir.join("intent.json"))?)?;
                return Ok(SolanaOutboxEntry { state, staged, dir });
            }
        }
        Err(OutboxError::NotFound(id.into()))
    }

    /// Read `id` only if it currently lives in `expected`, mirroring
    /// `bloom-tx`'s fail-closed state check.
    pub fn read_in_state(
        &self,
        wallet: &str,
        chain: &str,
        id: &str,
        expected: SolanaOutboxState,
    ) -> Result<SolanaOutboxEntry, OutboxError> {
        let dir = self.state_dir(wallet, chain, expected)?.join(id);
        if dir.join("intent.json").exists() {
            let staged = serde_json::from_slice(&fs::read(dir.join("intent.json"))?)?;
            return Ok(SolanaOutboxEntry {
                state: expected,
                staged,
                dir,
            });
        }
        for other in [
            SolanaOutboxState::Pending,
            SolanaOutboxState::Sent,
            SolanaOutboxState::Failed,
        ] {
            if other == expected {
                continue;
            }
            if self
                .state_dir(wallet, chain, other)?
                .join(id)
                .join("intent.json")
                .exists()
            {
                return Err(OutboxError::StateMismatch {
                    id: id.to_string(),
                    expected: expected.dirname(),
                    actual: other.dirname(),
                });
            }
        }
        Err(OutboxError::NotFound(id.into()))
    }

    pub fn list(
        &self,
        wallet: &str,
        chain: &str,
        state: SolanaOutboxState,
    ) -> Result<Vec<String>, OutboxError> {
        let dir = self.state_dir(wallet, chain, state)?;
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir()
                && let Some(name) = entry.file_name().to_str()
            {
                out.push(name.to_string());
            }
        }
        out.sort();
        Ok(out)
    }

    /// Walk every `<root>/<wallet>/<chain>/sent/<id>/` and return a
    /// [`SolanaSentEntry`] per entry whose `intent.json` parses and has a
    /// recorded signature. Malformed entries are skipped with a warning.
    pub fn walk_all_sent(&self) -> Result<Vec<SolanaSentEntry>, OutboxError> {
        let mut out = Vec::new();
        if !self.inner.root.exists() {
            return Ok(out);
        }
        for w in fs::read_dir(&self.inner.root)? {
            let w = w?;
            if !w.file_type()?.is_dir() {
                continue;
            }
            let wname = match w.file_name().into_string() {
                Ok(n) => n,
                Err(_) => continue,
            };
            for c in fs::read_dir(w.path())? {
                let c = c?;
                if !c.file_type()?.is_dir() {
                    continue;
                }
                let cname = match c.file_name().into_string() {
                    Ok(n) => n,
                    Err(_) => continue,
                };
                let sent = c.path().join("sent");
                if !sent.exists() {
                    continue;
                }
                for ent in fs::read_dir(&sent)? {
                    let ent = match ent {
                        Ok(e) => e,
                        Err(e) => {
                            tracing::warn!(error = %e, path = %sent.display(), "solana_outbox.walk_sent.readdir_failed");
                            continue;
                        }
                    };
                    let dir = ent.path();
                    let intent_path = dir.join("intent.json");
                    if !intent_path.exists() {
                        continue;
                    }
                    match parse_sent_entry(&wname, &cname, &dir, &intent_path) {
                        Some(se) => out.push(se),
                        None => {
                            tracing::warn!(path = %dir.display(), "solana_outbox.walk_sent.skip_malformed")
                        }
                    }
                }
            }
        }
        Ok(out)
    }

    /// Write a sibling artefact next to an existing sent entry.
    pub fn write_sent_sibling(
        &self,
        entry: &SolanaSentEntry,
        name: &str,
        bytes: &[u8],
    ) -> Result<(), OutboxError> {
        let dir = self
            .state_dir(&entry.wallet, &entry.chain, SolanaOutboxState::Sent)?
            .join(&entry.id);
        fs::create_dir_all(&dir)?;
        self.write_artefact(&dir, name, bytes)
    }

    fn write_artefact(&self, dir: &Path, name: &str, body: &[u8]) -> Result<(), OutboxError> {
        if name.contains('/') || name.contains('\\') {
            return Err(OutboxError::InvalidId(name.into()));
        }
        fs::write(dir.join(name), body)?;
        Ok(())
    }

    /// Read the mined-outcome `receipt.json` for an entry in any state.
    pub fn read_receipt(
        &self,
        wallet: &str,
        chain: &str,
        id: &str,
    ) -> Result<Option<SolanaReceipt>, OutboxError> {
        let entry = match self.read(wallet, chain, id) {
            Ok(e) => e,
            Err(OutboxError::NotFound(_)) => return Ok(None),
            Err(e) => return Err(e),
        };
        let path = entry.dir.join(RECEIPT_FILE);
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(serde_json::from_slice(&fs::read(&path)?)?))
    }

    /// Record the transaction signature on a still-pending entry (the signing
    /// step), rewriting `intent.json` so the signature is durable before the
    /// entry transitions to `sent`.
    pub fn record_signature(
        &self,
        wallet: &str,
        chain: &str,
        id: &str,
        signature: &str,
    ) -> Result<SolanaOutboxEntry, OutboxError> {
        let entry = self.read_in_state(wallet, chain, id, SolanaOutboxState::Pending)?;
        let mut staged = entry.staged.clone();
        staged.signature = Some(signature.to_string());
        fs::write(
            entry.dir.join("intent.json"),
            serde_json::to_vec_pretty(&staged)?,
        )?;
        Ok(SolanaOutboxEntry {
            state: entry.state,
            staged,
            dir: entry.dir,
        })
    }

    /// Cancel a still-pending entry (Solana: only legal before signing).
    pub fn cancel(&self, wallet: &str, chain: &str, id: &str) -> Result<(), OutboxError> {
        let entry = self.read_in_state(wallet, chain, id, SolanaOutboxState::Pending)?;
        let mut staged = entry.staged.clone();
        staged.status = SolanaTxStatus::Cancelled;
        let entry = SolanaOutboxEntry {
            state: entry.state,
            staged: staged.clone(),
            dir: entry.dir.clone(),
        };
        let new_dir = self.transition(&entry, SolanaOutboxState::Failed)?;
        fs::write(
            new_dir.join("intent.json"),
            serde_json::to_vec_pretty(&staged)?,
        )?;
        fs::write(new_dir.join("cancel.txt"), b"cancelled by user")?;
        Ok(())
    }

    /// Remove pending entries whose expiry has elapsed.
    pub fn sweep_expired(&self, now_ms: u128) -> Result<usize, OutboxError> {
        let mut count = 0;
        if !self.inner.root.exists() {
            return Ok(0);
        }
        for w in fs::read_dir(&self.inner.root)? {
            let w = w?;
            if !w.file_type()?.is_dir() {
                continue;
            }
            for c in fs::read_dir(w.path())? {
                let c = c?;
                if !c.file_type()?.is_dir() {
                    continue;
                }
                let pending = c.path().join("pending");
                if !pending.exists() {
                    continue;
                }
                for ent in fs::read_dir(&pending)? {
                    let ent = ent?;
                    let intent_path = ent.path().join("intent.json");
                    if !intent_path.exists() {
                        continue;
                    }
                    let staged: StagedSolanaTransfer =
                        serde_json::from_slice(&fs::read(&intent_path)?)?;
                    if staged.expires_ms != 0 && now_ms >= staged.expires_ms {
                        let entry = SolanaOutboxEntry {
                            state: SolanaOutboxState::Pending,
                            staged,
                            dir: ent.path(),
                        };
                        self.transition(&entry, SolanaOutboxState::Failed)?;
                        count += 1;
                    }
                }
            }
        }
        Ok(count)
    }
}

fn blake3_hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

fn parse_sent_entry(
    wallet: &str,
    chain: &str,
    dir: &Path,
    intent_path: &Path,
) -> Option<SolanaSentEntry> {
    let bytes = fs::read(intent_path).ok()?;
    let staged: StagedSolanaTransfer = serde_json::from_slice(&bytes).ok()?;
    let signature = staged.signature.as_ref()?.clone();
    let sent_at = fs::metadata(intent_path).ok()?.modified().ok()?;
    let mined = dir.join(RECEIPT_FILE).exists();
    Some(SolanaSentEntry {
        wallet: wallet.to_string(),
        chain: chain.to_string(),
        id: staged.id,
        signature,
        fee_payer: staged.fee_payer,
        destination: staged.destination,
        lamports: staged.lamports,
        blockhash: staged.blockhash,
        last_valid_block_height: staged.last_valid_block_height,
        sent_at,
        mined,
    })
}
