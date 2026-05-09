//! Server-side glue for the `mount` feature: bind an embednfs server,
//! shell out to the platform mount command, hand back a handle.
//!
//! Lifecycle:
//!
//! 1. [`serve_nfs`] picks a localhost port (ephemeral by default),
//!    binds an `embednfs::NfsServer` wrapping a [`crate::adapter::BethFs`],
//!    and spawns the accept loop on a tokio task.
//! 2. It then runs the platform mount command synchronously inside a
//!    `spawn_blocking` (waiting on a child process inside an async fn
//!    works, but `spawn_blocking` keeps stdout/stderr capture cleaner).
//! 3. The returned [`NfsMountHandle`] aborts the server task and runs
//!    `umount` when [`MountHandle::unmount`] is called. The `Drop` impl
//!    issues a best-effort `umount` so a panic'd test still cleans up.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::{Duration, Instant};

use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use beth_vfs::Vfs;

use crate::adapter::BethFs;
use crate::{build_mount_args, MountConfig, MountError, MountHandle};

const MOUNT_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const UMOUNT_COMMAND_TIMEOUT: Duration = Duration::from_secs(5);

fn run_command_with_timeout(
    cmd_name: &str,
    args: &[String],
    timeout: Duration,
) -> Result<Output, MountError> {
    let mut child = std::process::Command::new(cmd_name)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let deadline = Instant::now() + timeout;
    loop {
        if child.try_wait()?.is_some() {
            return child.wait_with_output().map_err(MountError::from);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            return Err(MountError::Mount(format!(
                "{cmd_name} timed out after {}s",
                timeout.as_secs()
            )));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn linux_mount_fallback_args(cfg: &MountConfig, server: SocketAddr) -> Vec<String> {
    let port = server.port();
    let mut opts = format!(
        "vers=4.1,proto=tcp,port={port},noac,lookupcache=none,rsize=65536,wsize=65536,timeo=10"
    );
    if cfg.readonly {
        opts.push_str(",ro");
    }
    vec![
        "-t".to_string(),
        "nfs4".to_string(),
        "-o".to_string(),
        opts,
        format!("{}:/", server.ip()),
        cfg.mount_path.display().to_string(),
    ]
}

/// Handle to a live mount established by [`serve_nfs`]. Holds the bound
/// NFS server task plus the mount path so `unmount` can tear both down.
pub struct NfsMountHandle {
    nfs_addr: SocketAddr,
    mount_path: PathBuf,
    server_task: parking_lot::Mutex<Option<JoinHandle<()>>>,
    unmounted: parking_lot::Mutex<bool>,
}

impl NfsMountHandle {
    fn new(nfs_addr: SocketAddr, mount_path: PathBuf, server_task: JoinHandle<()>) -> Self {
        Self {
            nfs_addr,
            mount_path,
            server_task: parking_lot::Mutex::new(Some(server_task)),
            unmounted: parking_lot::Mutex::new(false),
        }
    }
}

impl Drop for NfsMountHandle {
    fn drop(&mut self) {
        // Best-effort cleanup if the caller forgot to await `unmount`.
        // `umount` might fail (already unmounted, kernel busy, etc.);
        // log and move on rather than panic from Drop.
        let already = *self.unmounted.lock();
        if !already {
            let mp = self.mount_path.clone();
            if let Err(e) = std::process::Command::new("umount")
                .arg("-l")
                .arg("-f")
                .arg(&mp)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                warn!(mount_path = %mp.display(), error = %e, "umount on drop failed");
            }
        }
        if let Some(task) = self.server_task.lock().take() {
            task.abort();
        }
    }
}

#[async_trait::async_trait]
impl MountHandle for NfsMountHandle {
    async fn unmount(&self) -> Result<(), MountError> {
        // Idempotent: a second call is a no-op.
        {
            let mut flag = self.unmounted.lock();
            if *flag {
                return Ok(());
            }
            *flag = true;
        }
        let mp = self.mount_path.clone();
        let status = tokio::task::spawn_blocking(move || {
            let args = vec!["-l".to_string(), "-f".to_string(), mp.display().to_string()];
            run_command_with_timeout("umount", &args, UMOUNT_COMMAND_TIMEOUT)
        })
        .await
        .map_err(|e| MountError::Mount(format!("umount join: {e}")))??;
        if !status.status.success() {
            let stderr = String::from_utf8_lossy(&status.stderr);
            return Err(MountError::Mount(format!(
                "umount exited {}: {}",
                status.status, stderr
            )));
        }
        if let Some(task) = self.server_task.lock().take() {
            task.abort();
        }
        Ok(())
    }

    fn mount_path(&self) -> &Path {
        &self.mount_path
    }

    fn nfs_addr(&self) -> SocketAddr {
        self.nfs_addr
    }
}

/// Bind an embedded NFS server, mount it at `mount_point`, and return
/// a handle whose `unmount` tears both down.
///
/// `mount_point` must already exist as an empty directory. We don't
/// create it for you — that decision belongs to whoever owns the path.
pub async fn serve_nfs(vfs: Vfs, mount_point: &Path) -> Result<NfsMountHandle, MountError> {
    serve_nfs_with(
        vfs,
        MountConfig {
            mount_path: mount_point.to_path_buf(),
            nfs_listen: "127.0.0.1:0".parse().expect("static loopback"),
            readonly: false,
        },
    )
    .await
}

/// Same as [`serve_nfs`] but lets the caller pin the listen address /
/// readonly flag. `cfg.nfs_listen` may use port 0 to pick an ephemeral
/// port; the actual bound address is reflected in the returned handle.
pub async fn serve_nfs_with(vfs: Vfs, cfg: MountConfig) -> Result<NfsMountHandle, MountError> {
    if !cfg.mount_path.exists() {
        return Err(MountError::Config(format!(
            "mount path does not exist: {}",
            cfg.mount_path.display()
        )));
    }
    if !cfg.mount_path.is_dir() {
        return Err(MountError::Config(format!(
            "mount path is not a directory: {}",
            cfg.mount_path.display()
        )));
    }

    // Bind the embednfs server on the requested address. Resolve the
    // OS-assigned port immediately so we can wire it into the mount
    // command — `mount.nfs4 port=N` needs the real number.
    let listener = TcpListener::bind(cfg.nfs_listen).await?;
    let local = listener.local_addr()?;
    info!(addr = %local, mount_path = %cfg.mount_path.display(), "starting embedded nfs server");

    let fs = BethFs::new(vfs);
    let server = embednfs::NfsServer::new(fs);
    let server_task = tokio::spawn(async move {
        if let Err(e) = server.serve(listener).await {
            warn!(error = %e, "nfs server exited");
        }
    });

    // Run the mount command. `spawn_blocking` so a long-running mount
    // (Linux can sit in the kernel for a few seconds while it sets up
    // the client struct) doesn't pin the runtime worker.
    let args = build_mount_args(&cfg, local);
    let cmd_name = crate::detect_mount_command();
    let mut attempts = vec![(cmd_name.to_string(), args)];
    if cfg!(target_os = "linux") {
        attempts.push(("mount".to_string(), linux_mount_fallback_args(&cfg, local)));
    }
    debug!(?attempts, "running mount command attempts");
    let mp_for_log = cfg.mount_path.clone();
    let mount_result = tokio::task::spawn_blocking({
        let attempts = attempts.clone();
        move || {
            let mut errors = Vec::new();
            for (cmd_name, args) in attempts {
                match run_command_with_timeout(&cmd_name, &args, MOUNT_COMMAND_TIMEOUT) {
                    Ok(output) if output.status.success() => return Ok(output),
                    Ok(output) => {
                        let stdout = String::from_utf8_lossy(&output.stdout);
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        errors.push(format!(
                            "{} {:?} exited {}: stdout={} stderr={}",
                            cmd_name, args, output.status, stdout, stderr
                        ));
                    }
                    Err(e) => errors.push(format!("{} {:?}: {}", cmd_name, args, e)),
                }
            }
            Err(MountError::Mount(errors.join("; ")))
        }
    })
    .await
    .map_err(|e| MountError::Mount(format!("mount join: {e}")))?;

    let output = match mount_result {
        Ok(o) => o,
        Err(e) => {
            server_task.abort();
            return Err(e);
        }
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        server_task.abort();
        return Err(MountError::Mount(format!(
            "mount exited {}: stdout={} stderr={}",
            output.status, stdout, stderr
        )));
    }

    info!(mount_path = %mp_for_log.display(), nfs_addr = %local, "mount established");
    Ok(NfsMountHandle::new(local, cfg.mount_path, server_task))
}
