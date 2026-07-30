//! Minimal authenticated login-session sentinel for the macOS
//! Unix-principal installation profile.

use std::{
    fs,
    io::ErrorKind,
    os::unix::{
        fs::{FileTypeExt as _, MetadataExt as _, PermissionsExt as _},
        net::UnixListener as StdUnixListener,
    },
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context as _, Result, bail};
use bloom_triad_local_transport::{PeerAcl, authenticate_server, load_identity_and_manifest};
use rustix::{
    fs::{Gid, fchown},
    process::geteuid,
};
use tokio::{io::AsyncReadExt as _, net::UnixListener};

const SESSION_SERVICE_ID: &str = "bloom-session";
const BROKER_SERVICE_ID: &str = "bloom-broker";

pub async fn run() -> Result<()> {
    let effective_uid = geteuid().as_raw();
    if effective_uid == 0 {
        bail!("the login-session sentinel must not run as root");
    }

    let enrollment_root = env_path(
        "BLOOM_ENROLLMENT_ROOT",
        "/Library/Application Support/BloomTriad/enrollments",
    );
    let Some(_) = load_enrollment(&enrollment_root, effective_uid)? else {
        // The global LaunchAgent is offered to every GUI login. An
        // unenrolled login is the normal successful no-op case.
        return Ok(());
    };
    let config_root = env_path(
        "BLOOM_CONFIG_ROOT",
        "/Library/Application Support/BloomTriad/config",
    )
    .join(effective_uid.to_string());
    let identity_path = config_root.join("session/identity.json");
    let manifest_path = config_root.join("edge-manifest.json");
    require_login_owned_private_file(&identity_path, effective_uid)?;
    let (identity, manifest) =
        load_identity_and_manifest(&identity_path, &manifest_path, SESSION_SERVICE_ID)
            .context("load authenticated session identity")?;
    let broker_acl = manifest
        .broker
        .into_acl()
        .context("load pinned Broker session peer")?;
    if broker_acl.service_id.as_str() != BROKER_SERVICE_ID {
        bail!("edge manifest has the wrong session peer");
    }
    let socket_gid = manifest
        .session_socket_gid
        .ok_or_else(|| anyhow::anyhow!("edge manifest has no session socket group"))?;

    let runtime_root = env_path("BLOOM_RUNTIME_ROOT", "/private/var/run/bloom");
    let session_dir = runtime_root.join(effective_uid.to_string()).join("session");
    require_session_directory(&session_dir, effective_uid, socket_gid)?;
    let socket_path = session_dir.join("session.sock");
    remove_owned_stale_socket(&socket_path, effective_uid, socket_gid)?;

    let listener = StdUnixListener::bind(&socket_path)
        .with_context(|| format!("bind session sentinel {}", socket_path.display()))?;
    fchown(&listener, None, Some(Gid::from_raw(socket_gid)))
        .context("set session sentinel socket group")?;
    fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o660))
        .context("set session sentinel socket mode")?;
    require_socket_metadata(&socket_path, effective_uid, socket_gid)?;
    listener
        .set_nonblocking(true)
        .context("make session sentinel socket nonblocking")?;
    let listener = UnixListener::from_std(listener).context("adopt session sentinel socket")?;
    let _socket_guard = SocketGuard {
        path: socket_path,
        uid: effective_uid,
        gid: socket_gid,
    };

    serve_authenticated_brokers(listener, identity, broker_acl).await
}

async fn serve_authenticated_brokers(
    listener: UnixListener,
    identity: bloom_triad_local_transport::LocalIdentity,
    broker_acl: PeerAcl,
) -> Result<()> {
    loop {
        let (mut stream, _) = listener
            .accept()
            .await
            .context("accept Broker session connection")?;
        if let Err(error) = authenticate_server(&mut stream, &identity, &broker_acl).await {
            tracing::warn!(%error, "session_sentinel.rejected_peer");
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        }
        tracing::info!("session_sentinel.broker_authenticated");
        let mut unexpected = [0_u8; 1];
        match stream.read(&mut unexpected).await {
            Ok(0) => {
                // Broker crashed or was deliberately restarted while the
                // login remains live. Keep the sentinel and authenticate its
                // replacement.
                tracing::info!("session_sentinel.broker_disconnected");
            }
            Ok(_) => bail!("authenticated Broker sent unexpected session-channel data"),
            Err(error) => return Err(error).context("monitor Broker session connection"),
        }
    }
}

fn load_enrollment(root: &Path, effective_uid: u32) -> Result<Option<()>> {
    let path = root.join(format!("{effective_uid}.json"));
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("inspect Bloom enrollment"),
    };
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || metadata.uid() != 0
        || metadata.mode() & 0o022 != 0
        || metadata.nlink() != 1
    {
        bail!("Bloom enrollment is not an immutable root-owned regular file");
    }
    Ok(Some(()))
}

fn require_login_owned_private_file(path: &Path, effective_uid: u32) -> Result<()> {
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("inspect {}", path.display()))?;
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || metadata.uid() != effective_uid
        || metadata.mode() & 0o077 != 0
        || metadata.nlink() != 1
    {
        bail!("session identity is not a login-owned private regular file");
    }
    Ok(())
}

fn require_session_directory(path: &Path, uid: u32, gid: u32) -> Result<()> {
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("inspect {}", path.display()))?;
    if !metadata.file_type().is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != uid
        || metadata.gid() != gid
        || metadata.mode() & 0o7777 != 0o710
        || metadata.nlink() < 2
    {
        bail!("session socket directory has the wrong owner, group, mode, or type");
    }
    Ok(())
}

fn remove_owned_stale_socket(path: &Path, uid: u32, gid: u32) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata)
            if metadata.file_type().is_socket()
                && metadata.uid() == uid
                && metadata.gid() == gid
                && metadata.mode() & 0o777 == 0o660
                && metadata.nlink() == 1 =>
        {
            fs::remove_file(path).context("remove stale session sentinel socket")
        }
        Ok(_) => bail!("refusing to replace a substituted session sentinel socket"),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("inspect session sentinel socket"),
    }
}

fn require_socket_metadata(path: &Path, uid: u32, gid: u32) -> Result<()> {
    let metadata = fs::symlink_metadata(path).context("inspect session sentinel socket")?;
    if !metadata.file_type().is_socket()
        || metadata.uid() != uid
        || metadata.gid() != gid
        || metadata.mode() & 0o777 != 0o660
        || metadata.nlink() != 1
    {
        bail!("session sentinel socket has the wrong owner, group, mode, or type");
    }
    Ok(())
}

fn env_path(name: &str, default: &str) -> PathBuf {
    std::env::var_os(name)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(default))
}

struct SocketGuard {
    path: PathBuf,
    uid: u32,
    gid: u32,
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        if let Ok(metadata) = fs::symlink_metadata(&self.path)
            && metadata.file_type().is_socket()
            && metadata.uid() == self.uid
            && metadata.gid() == self.gid
            && metadata.nlink() == 1
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}
