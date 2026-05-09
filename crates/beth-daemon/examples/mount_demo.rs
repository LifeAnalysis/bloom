//! Mount the bloom-eth VFS at a caller-supplied path and block until
//! SIGTERM/SIGINT.
//!
//! Used by the dockerized integration test in `tests/docker/`. The
//! example is built with `--features mount` so it compiles only when
//! the embednfs glue is available.
//!
//! Usage:
//!     mount_demo <mount-point> [home-dir]
//!
//! On exit we run `unmount` so a Ctrl-C from the test driver leaves
//! the container in a clean state.

#[cfg(not(feature = "mount"))]
fn main() {
    eprintln!("mount_demo: rebuild with --features mount");
    std::process::exit(2);
}

#[cfg(feature = "mount")]
#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> anyhow::Result<()> {
    use std::path::PathBuf;

    use beth_daemon::Daemon;
    use beth_proto::HomeDir;
    use tracing_subscriber::EnvFilter;

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let mut args = std::env::args();
    let _argv0 = args.next();
    let mount_point = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("usage: mount_demo <mount-point> [home-dir]"))?;
    let home_arg = args.next();

    let home = match home_arg {
        Some(p) => HomeDir::at(PathBuf::from(p)),
        None => HomeDir::resolve("~/.bloom-eth")?,
    };

    let mount_path = PathBuf::from(&mount_point);
    if !mount_path.exists() {
        std::fs::create_dir_all(&mount_path)?;
    }

    let daemon = Daemon::from_home(home)?;
    tracing::info!(mount = %mount_path.display(), "mounting bloom-eth vfs");
    let handle = daemon.mount(&mount_path).await?;
    tracing::info!(mount = %mount_path.display(), nfs = %beth_mount::MountHandle::nfs_addr(&handle), "mounted");

    // Touch a sentinel so the test driver can detect the mount is live
    // without needing extra IPC. The Linux kernel attaches NFS mounts
    // synchronously, so by the time `daemon.mount` returns Ok the
    // mount is already visible — but a sentinel makes the harness
    // robust against busy CI rebuilds.
    if let Some(parent) = mount_path.parent() {
        let _ = std::fs::write(
            parent.join(".beth-mounted"),
            mount_path.display().to_string(),
        );
    }

    // Wait for SIGTERM/SIGINT, then unmount cleanly.
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    tokio::select! {
        _ = sigterm.recv() => tracing::info!("got SIGTERM"),
        _ = sigint.recv() => tracing::info!("got SIGINT"),
    }

    if let Err(e) = beth_mount::MountHandle::unmount(&handle).await {
        tracing::warn!(error = %e, "unmount failed; relying on Drop fallback");
    }
    Ok(())
}
