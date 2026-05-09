//! NFS mount adapter for bloom-eth.
//!
//! This crate exposes a [`beth_vfs::Vfs`] over NFSv4.1 by spinning up an
//! in-process [`embednfs`] server bound to a localhost port and then
//! shelling out to the platform mount command (`mount.nfs4` on Linux,
//! `mount_nfs` on macOS) to attach it as a real filesystem mount.
//!
//! # Feature gating
//!
//! The actual server lives behind the `mount` cargo feature so default
//! builds stay portable — embednfs is a git dependency and the platform
//! mount tooling is not present everywhere. Without `--features mount`
//! the crate still exposes the [`MountConfig`] / [`MountHandle`] /
//! [`MountError`] surface plus the OS helpers ([`detect_mount_command`],
//! [`build_mount_args`]) so the daemon and CLI compile uniformly. The
//! stub [`mount`] entry returns [`MountError::NotEnabled`].
//!
//! With `--features mount`, the additional entry point [`serve_nfs`]
//! starts the server, issues the platform mount, and hands back a
//! [`NfsMountHandle`] whose `Drop` runs `umount`.

#![forbid(unsafe_code)]

use std::any::Any;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[cfg(feature = "mount")]
pub mod adapter;
#[cfg(feature = "mount")]
mod server;

#[cfg(feature = "mount")]
pub use server::{serve_nfs, NfsMountHandle};

/// Configuration for mounting a bloom-eth VFS over NFS.
#[derive(Debug, Clone)]
pub struct MountConfig {
    /// Filesystem path where the NFS export should be attached.
    /// Callers must expand `~` and resolve relative paths before
    /// constructing this — the adapter takes the value as-is.
    pub mount_path: PathBuf,
    /// Address the embedded NFS server should listen on.
    /// `127.0.0.1:0` selects an ephemeral port.
    pub nfs_listen: SocketAddr,
    /// Mount the export read-only.
    pub readonly: bool,
}

impl MountConfig {
    /// Build a config that picks an ephemeral port on loopback and
    /// mounts read-write at `mount_path`.
    pub fn ephemeral(mount_path: impl Into<PathBuf>) -> Self {
        Self {
            mount_path: mount_path.into(),
            nfs_listen: "127.0.0.1:0".parse().expect("static loopback addr"),
            readonly: false,
        }
    }
}

/// Errors produced by the mount adapter.
#[derive(thiserror::Error, Debug)]
pub enum MountError {
    #[error("nfs feature not enabled")]
    NotEnabled,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("mount system call failed: {0}")]
    Mount(String),
    #[error("invalid config: {0}")]
    Config(String),
}

/// Handle to a live mount. Dropping the handle does *not* unmount —
/// callers must invoke [`MountHandle::unmount`] explicitly to ensure
/// the underlying syscall is awaited.
#[async_trait::async_trait]
pub trait MountHandle: Send + Sync {
    /// Tear down the mount and stop the embedded server.
    async fn unmount(&self) -> Result<(), MountError>;
    /// Filesystem path where the export is attached.
    fn mount_path(&self) -> &Path;
    /// Address the embedded NFS server is listening on.
    fn nfs_addr(&self) -> SocketAddr;
}

/// Stub mount entry point used by callers that haven't migrated to
/// [`serve_nfs`] yet. Always returns [`MountError::NotEnabled`].
///
/// The `_vfs` argument is opaque so callers without `--features mount`
/// can still program against this surface. With the feature enabled
/// you should call [`serve_nfs`] directly with a typed `Vfs`.
pub async fn mount(
    _cfg: MountConfig,
    _vfs: Arc<dyn Any + Send + Sync>,
) -> Result<Box<dyn MountHandle>, MountError> {
    #[cfg(feature = "mount")]
    {
        tracing::warn!("mount.placeholder_called: use serve_nfs with a typed Vfs instead");
        Err(MountError::NotEnabled)
    }
    #[cfg(not(feature = "mount"))]
    {
        tracing::debug!("mount.feature_disabled: build without 'mount' cargo feature");
        Err(MountError::NotEnabled)
    }
}

/// Test/utility handle that satisfies [`MountHandle`] without doing any
/// real I/O. Useful for daemon-level tests that only need to pretend a
/// mount exists.
pub struct NoopHandle {
    mount_path: PathBuf,
    nfs_addr: SocketAddr,
}

impl NoopHandle {
    pub fn new(mount_path: PathBuf, nfs_addr: SocketAddr) -> Self {
        Self {
            mount_path,
            nfs_addr,
        }
    }
}

#[async_trait::async_trait]
impl MountHandle for NoopHandle {
    async fn unmount(&self) -> Result<(), MountError> {
        Ok(())
    }
    fn mount_path(&self) -> &Path {
        &self.mount_path
    }
    fn nfs_addr(&self) -> SocketAddr {
        self.nfs_addr
    }
}

/// Returns the platform-appropriate userspace mount command name.
///
/// Linux ships `mount.nfs4` from `nfs-utils`; macOS ships `mount_nfs`.
/// Other platforms fall back to `mount` so callers at least produce a
/// recognisable error rather than panicking.
pub fn detect_mount_command() -> &'static str {
    if cfg!(target_os = "linux") {
        "mount.nfs4"
    } else if cfg!(target_os = "macos") {
        "mount_nfs"
    } else {
        "mount"
    }
}

/// Build the `-o` option string and trailing positional arguments for
/// the platform mount command, targeting the embedded NFSv4.1 server at
/// `server`.
///
/// Mirrors bloom's option set: `noac,lookupcache=none` to disable kernel
/// caching that interferes with reactive VFS updates, `vers=4.1`,
/// `proto=tcp`, `nolocks` (no NLM — the embedded server doesn't speak
/// it), explicit `mountport`/`port` so we can target the auto-assigned
/// ephemeral port, generous `rsize`/`wsize` so most JSON/TOML payloads
/// fit in a single op (the adapter buffers multi-block writes anyway
/// but a single op stays simpler for the common case), and `timeo=10`
/// for snappy failure on a wedged server.
pub fn build_mount_args(cfg: &MountConfig, server: SocketAddr) -> Vec<String> {
    let port = server.port();
    let mut opts = format!(
        "noac,lookupcache=none,vers=4.1,proto=tcp,nolocks,mountport={port},port={port},rsize=65536,wsize=65536,timeo=10"
    );
    if cfg.readonly {
        opts.push_str(",ro");
    }
    vec![
        "-o".to_string(),
        opts,
        format!("{}:/", server.ip()),
        cfg.mount_path.display().to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_mount_command_matches_platform() {
        let got = detect_mount_command();
        if cfg!(target_os = "linux") {
            assert_eq!(got, "mount.nfs4");
        } else if cfg!(target_os = "macos") {
            assert_eq!(got, "mount_nfs");
        } else {
            assert_eq!(got, "mount");
        }
    }

    #[test]
    fn build_mount_args_includes_version_and_port() {
        let cfg = MountConfig {
            mount_path: PathBuf::from("/tmp/beth"),
            nfs_listen: "127.0.0.1:0".parse().unwrap(),
            readonly: false,
        };
        let server: SocketAddr = "127.0.0.1:54321".parse().unwrap();
        let args = build_mount_args(&cfg, server);
        let joined = args.join(" ");
        assert!(joined.contains("vers=4.1"), "missing vers=4.1 in {joined}");
        assert!(
            joined.contains("port=54321"),
            "missing port=54321 in {joined}"
        );
        assert!(
            joined.contains("mountport=54321"),
            "missing mountport=54321 in {joined}"
        );
        assert!(args.last().unwrap().ends_with("/tmp/beth"));
    }

    #[tokio::test]
    async fn mount_returns_not_enabled_by_default() {
        let cfg = MountConfig {
            mount_path: PathBuf::from("/tmp/beth"),
            nfs_listen: "127.0.0.1:0".parse().unwrap(),
            readonly: false,
        };
        let vfs: Arc<dyn Any + Send + Sync> = Arc::new(());
        match mount(cfg, vfs).await {
            Ok(_) => panic!("expected MountError::NotEnabled, got Ok"),
            Err(MountError::NotEnabled) => {}
            Err(other) => panic!("expected NotEnabled, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn noop_handle_round_trips() {
        let addr: SocketAddr = "127.0.0.1:1234".parse().unwrap();
        let h = NoopHandle::new(PathBuf::from("/tmp/x"), addr);
        assert_eq!(h.mount_path(), Path::new("/tmp/x"));
        assert_eq!(h.nfs_addr(), addr);
        h.unmount().await.unwrap();
    }
}
