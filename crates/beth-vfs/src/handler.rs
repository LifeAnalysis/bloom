//! The Handler trait — every top-level subtree implements it.

use std::time::Duration;

use async_trait::async_trait;
use thiserror::Error;

use crate::path::VfsPath;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Dir,
    File,
    Symlink,
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub kind: EntryKind,
    /// File size hint (best-effort; may be 0 for synthetic files).
    pub size: u64,
    /// Posix mode bits (just informational; the mount layer decides
    /// the real mode).
    pub mode: u32,
    /// For symlinks, the target.
    pub link_target: Option<String>,
}

impl Entry {
    pub fn dir(name: &str) -> Self {
        Self {
            name: name.into(),
            kind: EntryKind::Dir,
            size: 0,
            mode: 0o755,
            link_target: None,
        }
    }
    /// Build a read-only file entry (mode 0o444). This is the right
    /// default for almost everything in the bloom-eth tree — chain
    /// views, status, tools output, prices, docs, audit views, wallet
    /// metadata files, watch outputs. Per the v1 spec only a small set
    /// of injection points (wallets/new, sign/*, outbox writes,
    /// watch/new, defi intents new+confirm, policy.toml) are writable;
    /// those should use [`Entry::writable_file`].
    pub fn file(name: &str) -> Self {
        Self::read_only_file(name)
    }
    /// Explicit read-only constructor. Equivalent to [`Entry::file`];
    /// prefer this name in handlers that mix read-only and writable
    /// entries side-by-side so the intent is loud at the call site.
    pub fn read_only_file(name: &str) -> Self {
        Self {
            name: name.into(),
            kind: EntryKind::File,
            size: 0,
            mode: 0o444,
            link_target: None,
        }
    }
    /// Build a writable file entry (mode 0o644). Use for the small set
    /// of NFS-injectable inputs the daemon accepts.
    pub fn writable_file(name: &str) -> Self {
        Self {
            name: name.into(),
            kind: EntryKind::File,
            size: 0,
            mode: 0o644,
            link_target: None,
        }
    }
    pub fn symlink(name: &str, target: &str) -> Self {
        Self {
            name: name.into(),
            kind: EntryKind::Symlink,
            size: 0,
            mode: 0o777,
            link_target: Some(target.into()),
        }
    }
}

#[derive(Debug, Error)]
pub enum HandlerError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("not a directory: {0}")]
    NotADir(String),
    #[error("not a file: {0}")]
    NotAFile(String),
    #[error("permission denied")]
    PermissionDenied,
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("backend: {0}")]
    Backend(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

impl HandlerError {
    pub fn invalid(s: impl Into<String>) -> Self {
        HandlerError::Invalid(s.into())
    }
    pub fn backend(s: impl Into<String>) -> Self {
        HandlerError::Backend(s.into())
    }
    pub fn not_found(s: impl Into<String>) -> Self {
        HandlerError::NotFound(s.into())
    }
}

/// One handler per top-level subtree (`chains/`, `wallets/`, etc).
///
/// Path semantics: the `path` passed in is the *suffix* under the
/// handler's mount segment, e.g. for `chains/ethereum/head/number` the
/// `chains` handler receives `ethereum/head/number`.
#[async_trait]
pub trait Handler: Send + Sync {
    /// Return the kind / metadata for this path (or NotFound).
    async fn lookup(&self, path: &VfsPath) -> Result<Entry, HandlerError>;

    /// Return the bytes for a regular file. Default: NotAFile.
    async fn read(&self, path: &VfsPath) -> Result<Vec<u8>, HandlerError> {
        Err(HandlerError::NotAFile(path.to_string_path()))
    }

    /// Handle a write to a writable file. Default: read-only.
    async fn write(&self, path: &VfsPath, _data: &[u8]) -> Result<(), HandlerError> {
        let _ = path;
        Err(HandlerError::PermissionDenied)
    }

    /// List directory children. Default: NotADir.
    async fn list(&self, path: &VfsPath) -> Result<Vec<Entry>, HandlerError> {
        Err(HandlerError::NotADir(path.to_string_path()))
    }

    /// Optional per-path TTL for the router-level read cache. `None`
    /// (default) means "never cache at the router". Handlers backing
    /// volatile or RPC-heavy paths should return a small `Duration`
    /// here (e.g. 1s for chain head, 30s for etherscan-backed reads).
    fn cache_ttl(&self, path: &VfsPath) -> Option<Duration> {
        let _ = path;
        None
    }

    /// Whether a successful read of `path` has externally-visible side
    /// effects worth recording in the audit log (signing, broadcast,
    /// etc). Default: pure-data reads, no audit entry. Handlers that
    /// emit signatures, perform broadcasts, or otherwise mutate state
    /// in response to reads should override this and return `true`.
    fn is_read_side_effecting(&self, path: &VfsPath) -> bool {
        let _ = path;
        false
    }
}
