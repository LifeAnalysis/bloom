//! Daemon library — wires the engines (keystore, chain, tx, vfs) into a
//! single runtime that can serve VFS calls. The actual NFS mount lives
//! in `beth-mount` and is feature-gated; this library always exposes the
//! VFS via [`Daemon`] for in-process consumers like the CLI.

#![forbid(unsafe_code)]

pub mod ipc;

mod ens_resolver;

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use beth_chain::{ChainClient, ChainRegistry};
use beth_defi::EnsoClient;
use beth_ens::EnsClient;
use beth_etherscan::EtherscanClient;
use beth_keystore::Keystore;
use beth_prices::PricesClient;
use beth_proto::{AddressBook, AuditLog, Config, HomeDir};
use beth_tx::outbox::Outbox;
use beth_tx::tx_engine::TxEngine;
use beth_vfs::handlers::{
    AddressBookHandler, ChainsHandler, DefiHandler, DocsHandler, PricesHandler, SimulateHandler,
    StatusHandler, ToolsHandler, WalletsHandler, WatchHandler,
};
use beth_vfs::Vfs;
use beth_watch::{WatchExecutor, WatchRegistry};
use thiserror::Error;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::{info, warn};

#[derive(Debug, Error)]
pub enum DaemonError {
    #[error("home: {0}")]
    Home(#[from] beth_proto::HomeError),
    #[error("config: {0}")]
    Config(#[from] beth_proto::ConfigError),
    #[error("keystore: {0}")]
    Keystore(String),
    #[error("chain: {0}")]
    Chain(#[from] beth_chain::ChainError),
    #[error("outbox: {0}")]
    Outbox(String),
    #[error("audit: {0}")]
    Audit(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("watch: {0}")]
    Watch(String),
}

/// All wired-up state the daemon owns. Cheap to clone (everything is
/// behind Arc/clone-safe inner types).
#[derive(Clone)]
pub struct Daemon {
    pub home: HomeDir,
    pub config: Config,
    pub chains: ChainRegistry,
    pub keystore: Keystore,
    pub tx_engine: TxEngine,
    pub address_book: Arc<AddressBook>,
    pub audit: Arc<AuditLog>,
    pub vfs: Vfs,
    pub watch_registry: Arc<WatchRegistry>,
    pub watch_executor: Arc<WatchExecutor>,
}

impl Daemon {
    /// Build a fully-wired daemon from the home directory, materialising
    /// any missing subdirs as needed.
    pub fn from_home(home: HomeDir) -> Result<Self, DaemonError> {
        home.ensure()?;
        let config = Config::load_or_init(&home.config_path())?;

        let mut clients: Vec<ChainClient> = Vec::new();
        for spec in config.chains.values() {
            match ChainClient::new(spec.clone()) {
                Ok(c) => clients.push(c),
                Err(e) => tracing::warn!(chain=%spec.name, "skipping chain: {e}"),
            }
        }
        let chains = ChainRegistry::default();
        for c in clients {
            chains.add(c);
        }

        let keystore =
            Keystore::new(home.keystore_dir()).map_err(|e| DaemonError::Keystore(e.to_string()))?;

        let outbox =
            Outbox::new(home.outbox_dir()).map_err(|e| DaemonError::Outbox(e.to_string()))?;
        let mut tx_engine = TxEngine::new(
            outbox,
            config.stage_ttl.as_millis(),
            config.block_mainnet_broadcast,
        );

        // Wire ENS resolver into TxEngine when a mainnet-style chain is
        // configured. We pick the first chain with id 1 / 11155111 / 5 /
        // 17000 (the ENS canonical-registry chains) for resolution.
        if let Some(ens_client) = pick_ens_client(&chains) {
            tx_engine =
                tx_engine.with_resolver(Arc::new(ens_resolver::EnsAdapter::new(ens_client)) as _);
        }

        let address_book_path = home.root().join("addressbook.toml");
        let address_book = AddressBook::load(&address_book_path).unwrap_or_default();
        let address_book_arc = Arc::new(address_book.clone());

        let audit =
            AuditLog::open(home.audit_path()).map_err(|e| DaemonError::Audit(e.to_string()))?;

        let watch_registry = Arc::new(
            WatchRegistry::new(home.watch_dir()).map_err(|e| DaemonError::Watch(e.to_string()))?,
        );
        let watch_executor = Arc::new(WatchExecutor::new(
            chains.clone(),
            watch_registry.clone(),
            home.clone(),
        ));

        let etherscan = config
            .etherscan
            .as_ref()
            .map(|c| match url::Url::parse(&c.api_url) {
                Ok(url) => EtherscanClient::with_base_url(c.api_key.clone(), url),
                Err(_) => EtherscanClient::new(c.api_key.clone()),
            });
        let etherscan_arc = etherscan.map(Arc::new);

        let prices = PricesClient::new();

        let mut vfs_builder = Vfs::builder()
            .mount(
                "chains",
                Arc::new(ChainsHandler::new(chains.clone()).with_etherscan(etherscan_arc.clone()))
                    as _,
            )
            .mount(
                "wallets",
                Arc::new(WalletsHandler::new(
                    keystore.clone(),
                    chains.clone(),
                    tx_engine.clone(),
                    address_book.clone(),
                )) as _,
            )
            .mount("tools", Arc::new(ToolsHandler::new()) as _)
            .mount(
                "status",
                Arc::new(StatusHandler::new(
                    chains.clone(),
                    keystore.clone(),
                    tx_engine.clone(),
                    Arc::new(audit.clone()),
                    Some(prices.clone()),
                    Some(home.cache_dir().join("etherscan")),
                    config
                        .etherscan
                        .as_ref()
                        .map(|c| !c.api_key.is_empty())
                        .unwrap_or(false),
                    home.root().to_path_buf(),
                    SystemTime::now(),
                    env!("CARGO_PKG_VERSION"),
                )) as _,
            )
            .mount("docs", Arc::new(DocsHandler::new()) as _)
            .mount(
                "simulate",
                Arc::new(SimulateHandler::new(
                    chains.clone(),
                    address_book_arc.clone(),
                )) as _,
            )
            .mount(
                "watch",
                Arc::new(WatchHandler::new(
                    watch_registry.clone(),
                    watch_executor.clone(),
                    home.clone(),
                )) as _,
            )
            .mount("prices", Arc::new(PricesHandler::new(prices)) as _)
            .mount(
                "addressbook",
                Arc::new(
                    AddressBookHandler::open(&address_book_path)
                        .map_err(|e| DaemonError::Audit(e.to_string()))?,
                ) as _,
            );

        // DeFi: Enso's public REST works without an API key for chains
        // they support keyless (currently quote+route on Base mainnet).
        // Mount whenever an `[enso]` block exists in config; an empty
        // api_key just means unauthenticated calls (rate-limited).
        if let Some(enso_cfg) = &config.enso {
            let mut enso = EnsoClient::new(&enso_cfg.api_key);
            if let Ok(url) = url::Url::parse(&enso_cfg.api_url) {
                enso = enso.with_base_url(url);
            }
            if enso_cfg.api_key.is_empty() {
                warn!("enso api_key empty; mounting defi/ for keyless access (rate-limited)");
            }
            vfs_builder = vfs_builder.mount(
                "defi",
                Arc::new(
                    DefiHandler::new(
                        enso,
                        chains.clone(),
                        keystore.clone(),
                        tx_engine.clone(),
                        address_book_arc.clone(),
                    )
                    .with_default_chain(config.default_chain.clone()),
                ) as _,
            );
        }

        let vfs = vfs_builder.build();

        info!(home=%home.root().display(), chains=?config.chains.keys().collect::<Vec<_>>(), "daemon.built");

        Ok(Self {
            home,
            config,
            chains,
            keystore,
            tx_engine,
            address_book: address_book_arc,
            audit: Arc::new(audit),
            vfs,
            watch_registry,
            watch_executor,
        })
    }

    /// Convenience for the default home dir (`~/.bloom-eth`).
    pub fn from_default_home() -> Result<Self, DaemonError> {
        let home = HomeDir::resolve("~/.bloom-eth")?;
        Self::from_home(home)
    }

    /// Spawn long-lived background tasks: currently the outbox expiry
    /// sweeper that runs every 60s and moves any pending entry past its
    /// `expires_ms` into `failed/` (fix #3). Caller keeps the returned
    /// [`BackgroundTasks`] alive; dropping it triggers graceful shutdown.
    ///
    /// Safe to call multiple times — each call spawns a fresh task and
    /// returns its own handle. Short-lived CLI commands generally don't
    /// need this; it's primarily for `beth serve` and the in-process
    /// daemon used by integration tests.
    pub fn spawn_background_tasks(&self) -> BackgroundTasks {
        let outbox = self.tx_engine.outbox.clone();
        let (tx, mut rx) = watch::channel(false);
        let interval = Duration::from_secs(60);
        let handle = tokio::spawn(async move {
            // Tick at `interval`, but exit promptly when the cancel
            // channel flips. We use `tokio::select!` so a long sleep
            // doesn't delay shutdown.
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        let now_ms = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .map(|d| d.as_millis())
                            .unwrap_or(0);
                        match outbox.sweep_expired(now_ms) {
                            Ok(0) => {}
                            Ok(n) => tracing::info!(swept=n, "outbox.sweep_expired"),
                            Err(e) => tracing::warn!(error=%e, "outbox.sweep_expired failed"),
                        }
                    }
                    _ = rx.changed() => {
                        if *rx.borrow() {
                            break;
                        }
                    }
                }
            }
        });
        BackgroundTasks {
            cancel: tx,
            handle: Some(handle),
        }
    }

    /// Mount this daemon's [`Vfs`] over NFS at `path`.
    ///
    /// Only available with `--features mount` on this crate (which in
    /// turn enables `beth-mount/mount`). Requires that `path` exists
    /// and is an empty directory; the platform mount command is
    /// invoked synchronously, so on Linux the kernel NFS client must
    /// be available (`nfs-common` package).
    ///
    /// Returns a handle whose `unmount` runs the platform `umount`
    /// command and aborts the embedded server. Drop also triggers a
    /// best-effort cleanup so a panicked test doesn't leak a mount.
    #[cfg(feature = "mount")]
    pub async fn mount(
        &self,
        path: &std::path::Path,
    ) -> Result<beth_mount::NfsMountHandle, beth_mount::MountError> {
        beth_mount::serve_nfs(self.vfs.clone(), path).await
    }
}

/// Handle to background tasks owned by a running [`Daemon`]. Drop to
/// signal shutdown; the spawned tasks read the watch and exit at the
/// next tick. Holding this past daemon lifetime keeps the sweeper alive.
pub struct BackgroundTasks {
    cancel: watch::Sender<bool>,
    handle: Option<JoinHandle<()>>,
}

impl BackgroundTasks {
    /// Trigger graceful shutdown and wait for the sweeper task to exit.
    pub async fn shutdown(mut self) {
        let _ = self.cancel.send(true);
        if let Some(h) = self.handle.take() {
            let _ = h.await;
        }
    }
}

impl Drop for BackgroundTasks {
    fn drop(&mut self) {
        // Best-effort fire-and-forget cancel. If the runtime is still up
        // the task will see the flip and exit; if the runtime is being
        // torn down, abort the join handle to avoid a leak.
        let _ = self.cancel.send(true);
        if let Some(h) = self.handle.take() {
            h.abort();
        }
    }
}

/// Pick an ENS-capable chain client from the registry. Prefers chain id 1
/// (mainnet); falls back to Sepolia / Goerli / Holesky.
fn pick_ens_client(chains: &ChainRegistry) -> Option<EnsClient> {
    for name in chains.list_names() {
        let Some(c) = chains.get(&name) else {
            continue;
        };
        let id = c.spec().chain_id;
        if matches!(id, 1 | 5 | 11155111 | 17000) {
            return Some(EnsClient::mainnet(c));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_from_tempdir() {
        let dir = tempfile::tempdir().unwrap();
        let home = HomeDir::at(dir.path());
        let d = Daemon::from_home(home).unwrap();
        assert!(!d.config.chains.is_empty());
        assert!(d.vfs.handler("tools").is_some());
        assert!(d.vfs.handler("wallets").is_some());
        assert!(d.vfs.handler("chains").is_some());
        assert!(d.vfs.handler("simulate").is_some());
        assert!(d.vfs.handler("watch").is_some());
        assert!(d.vfs.handler("prices").is_some());
        assert!(d.vfs.handler("addressbook").is_some());
    }

    /// Fix #3: the spawned sweeper drops expired pending entries into
    /// `failed/` on its own. We don't wait for the natural 60s tick;
    /// instead the test calls `outbox.sweep_expired` itself to keep
    /// runtime short, but verifies that `spawn_background_tasks` returns
    /// a guard that cleans up cleanly when shut down.
    #[tokio::test]
    async fn sweep_background_task_handles_shutdown() {
        let dir = tempfile::tempdir().unwrap();
        let home = HomeDir::at(dir.path());
        let d = Daemon::from_home(home).unwrap();
        let tasks = d.spawn_background_tasks();
        // Seed an already-expired pending entry; the foreground call
        // exercises the same code the spawned task runs.
        let staged = beth_proto::StagedTx {
            id: "0001-test".into(),
            wallet: "alice".into(),
            chain: "anvil".into(),
            chain_id: 31337,
            from: "0x0000000000000000000000000000000000000001".into(),
            to: "0x0000000000000000000000000000000000000002".into(),
            value_wei: "0".into(),
            data_hex: "0x".into(),
            gas_limit: 21000,
            max_fee_per_gas: None,
            max_priority_fee_per_gas: None,
            gas_price: None,
            nonce: 0,
            policy_checks: vec![],
            created_ms: 0,
            expires_ms: 1,
            status: beth_proto::TxStatus::Pending,
            tx_hash: None,
            token: None,
        };
        d.tx_engine.outbox.write_pending(&staged, "p").unwrap();
        let n = d.tx_engine.outbox.sweep_expired(2).unwrap();
        assert_eq!(n, 1);

        // Shutdown completes promptly.
        tokio::time::timeout(std::time::Duration::from_secs(2), tasks.shutdown())
            .await
            .expect("background task did not honour shutdown signal");
    }
}
