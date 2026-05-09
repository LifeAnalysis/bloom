//! Background poll-based executor for [`super::WatchRegistry`].
//!
//! The executor runs a single tokio task that wakes up every
//! `tick_interval` (default 2 s) and walks every spec in the registry.
//! For each spec it issues the appropriate JSON-RPC call against the
//! corresponding [`beth_chain::ChainClient`] and, when the observed
//! state has changed since the last tick, appends a JSON line to a
//! per-watch `live` file. Live files are rotated to numbered
//! `history.jsonl.<n>` segments once they grow past 1 MB, with a
//! sentinel record so tailing agents can keep up.
//!
//! Per-watch storage layout (sibling to the registry's `<wallet>/<id>.toml`):
//!
//! ```text
//! <home>/watch/
//! └── <wallet>/
//!     ├── <id>.toml             # spec, owned by WatchRegistry
//!     └── <id>/
//!         ├── live              # current jsonl tail
//!         ├── history.jsonl     # latest rotated archive
//!         ├── history.jsonl.1   # older
//!         └── …
//! ```
//!
//! The HTTP-based [`beth_chain::ChainClient`] does not expose
//! subscriptions, so the executor uses polling exclusively. A future
//! revision can add a websocket fast path while keeping this loop as
//! the fallback.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use alloy::primitives::{Address, U256};
use alloy::providers::Provider;
use alloy::rpc::types::eth::Filter;
use serde::Serialize;
use tokio::io::AsyncWriteExt;
use tokio::sync::{watch, Mutex};
use tokio::task::JoinHandle;
use tokio::time::interval;
use tracing::{debug, warn};

use beth_chain::ChainRegistry;
use beth_proto::HomeDir;

use crate::{WatchError, WatchKind, WatchRegistry, WatchSpec};

/// Live file rotation threshold (1 MiB).
pub const ROTATE_THRESHOLD_BYTES: u64 = 1024 * 1024;

/// Default tick interval (2 s).
pub const DEFAULT_TICK_INTERVAL: Duration = Duration::from_secs(2);

/// Background watch loop. Wraps a [`WatchRegistry`] and a
/// [`ChainRegistry`] so the spawned task has everything it needs.
pub struct WatchExecutor {
    chains: ChainRegistry,
    registry: Arc<WatchRegistry>,
    home: HomeDir,
    tick: Duration,
    handle: Mutex<Option<JoinHandle<()>>>,
    shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
}

impl WatchExecutor {
    /// Create a new executor. Does not spawn the background task —
    /// call [`start`](Self::start) for that.
    pub fn new(chains: ChainRegistry, registry: Arc<WatchRegistry>, home: HomeDir) -> Self {
        let (tx, rx) = watch::channel(false);
        Self {
            chains,
            registry,
            home,
            tick: DEFAULT_TICK_INTERVAL,
            handle: Mutex::new(None),
            shutdown_tx: tx,
            shutdown_rx: rx,
        }
    }

    /// Override the polling interval (default: 2 s).
    pub fn with_tick(mut self, tick: Duration) -> Self {
        self.tick = tick;
        self
    }

    /// Path to the per-watch live file. Note: this is the *flat-by-id*
    /// path callers use through the VFS. The executor's loop writes to
    /// `live_path_for_spec` which includes the wallet partition.
    pub fn live_path(&self, id: &str) -> PathBuf {
        match self.registry.find_by_id(id) {
            Some(spec) => Self::live_path_for_spec(&self.home, &spec),
            None => self.home.watch_dir().join(id).join("live"),
        }
    }

    /// Same as [`live_path`](Self::live_path) but for the rotated history.
    pub fn history_path(&self, id: &str) -> PathBuf {
        match self.registry.find_by_id(id) {
            Some(spec) => Self::history_path_for_spec(&self.home, &spec),
            None => self.home.watch_dir().join(id).join("history.jsonl"),
        }
    }

    /// Per-watch directory that holds `live`, `history.jsonl`, and the
    /// numbered rotations. Sibling to `<wallet>/<id>.toml`.
    pub fn watch_dir_for_spec(home: &HomeDir, spec: &WatchSpec) -> PathBuf {
        home.watch_dir().join(&spec.wallet).join(&spec.id)
    }

    pub fn live_path_for_spec(home: &HomeDir, spec: &WatchSpec) -> PathBuf {
        Self::watch_dir_for_spec(home, spec).join("live")
    }

    pub fn history_path_for_spec(home: &HomeDir, spec: &WatchSpec) -> PathBuf {
        Self::watch_dir_for_spec(home, spec).join("history.jsonl")
    }

    /// Spawn the polling task. Idempotent: if the task is already running,
    /// the second call is a no-op.
    pub fn start(self: &Arc<Self>) -> Result<(), WatchError> {
        let mut guard = self.handle.try_lock().map_err(|_| {
            WatchError::Io(std::io::Error::other(
                "watch executor handle locked; concurrent start",
            ))
        })?;
        if guard.is_some() {
            return Ok(());
        }

        let this = Arc::clone(self);
        let mut shutdown_rx = self.shutdown_rx.clone();
        let tick = self.tick;
        let handle = tokio::spawn(async move {
            let mut state = ExecutorState::default();
            let mut ticker = interval(tick);
            // Skip the immediate first tick burst so callers can fund
            // accounts and then observe the first genuine "change".
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        if let Err(e) = this.tick_once(&mut state).await {
                            warn!(error = %e, "watch.tick.error");
                        }
                    }
                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() {
                            debug!("watch.executor.shutdown");
                            break;
                        }
                    }
                }
            }
        });
        *guard = Some(handle);
        Ok(())
    }

    /// Stop the background task. Safe to call from any context.
    pub async fn stop(&self) {
        let _ = self.shutdown_tx.send(true);
        let mut guard = self.handle.lock().await;
        if let Some(h) = guard.take() {
            // Best-effort: aborting + awaiting is enough for shutdown.
            h.abort();
            let _ = h.await;
        }
    }

    /// Run a single poll pass over every registered watch. Public so tests
    /// can drive it deterministically.
    pub async fn tick_once(&self, state: &mut ExecutorState) -> Result<(), WatchError> {
        for spec in self.registry.list_all() {
            if let Err(e) = self.process_spec(&spec, state).await {
                warn!(wallet = %spec.wallet, id = %spec.id, error = %e, "watch.spec.error");
            }
        }
        Ok(())
    }

    async fn process_spec(
        &self,
        spec: &WatchSpec,
        state: &mut ExecutorState,
    ) -> Result<(), WatchError> {
        let key = format!("{}/{}", spec.wallet, spec.id);
        match &spec.kind {
            WatchKind::Balance {
                address,
                threshold_wei: _,
                comparator: _,
            } => {
                // Pick the wallet's "first" chain - this watch kind in the
                // current spec doesn't carry a chain ref, so we sample
                // every configured chain. To keep behaviour deterministic
                // we pick the first chain by registered name order.
                let chain_name = match self.chains.list_names().into_iter().next() {
                    Some(n) => n,
                    None => return Ok(()),
                };
                let client = match self.chains.get(&chain_name) {
                    Some(c) => c,
                    None => return Ok(()),
                };
                let addr: Address = address.parse().map_err(|e: alloy::hex::FromHexError| {
                    WatchError::InvalidId(format!("bad address {address}: {e}"))
                })?;
                let bal = client
                    .balance(addr)
                    .await
                    .map_err(|e| WatchError::Io(std::io::Error::other(e.to_string())))?;
                let prev = state.balance.get(&key).copied();
                if prev != Some(bal) {
                    let record = serde_json::json!({
                        "ts": now_ms(),
                        "kind": "balance",
                        "addr": format!("{:#x}", addr),
                        "balance_wei": bal.to_string(),
                        "prev_wei": prev.map(|p| p.to_string()),
                        "chain": chain_name,
                    });
                    self.append_record(spec, &record).await?;
                    state.balance.insert(key, bal);
                }
            }
            WatchKind::Block { chain } => {
                let client = match self.chains.get(chain) {
                    Some(c) => c,
                    None => return Ok(()),
                };
                let head = client
                    .block_number()
                    .await
                    .map_err(|e| WatchError::Io(std::io::Error::other(e.to_string())))?;
                let prev = state.block.get(&key).copied().unwrap_or(0);
                if head > prev {
                    for n in (prev + 1)..=head {
                        let record = serde_json::json!({
                            "ts": now_ms(),
                            "kind": "block",
                            "chain": chain,
                            "number": n,
                        });
                        self.append_record(spec, &record).await?;
                    }
                    state.block.insert(key, head);
                }
            }
            WatchKind::GasPrice {
                chain,
                threshold_gwei,
            } => {
                let client = match self.chains.get(chain) {
                    Some(c) => c,
                    None => return Ok(()),
                };
                let gp = client
                    .gas_price()
                    .await
                    .map_err(|e| WatchError::Io(std::io::Error::other(e.to_string())))?;
                let prev = state.gas.get(&key).copied();
                let crossed = match prev {
                    None => true,
                    Some(prev_gp) => prev_gp != gp,
                };
                if crossed {
                    let record = serde_json::json!({
                        "ts": now_ms(),
                        "kind": "gas_price",
                        "chain": chain,
                        "gas_price_wei": gp.to_string(),
                        "prev_wei": prev.map(|p| p.to_string()),
                        "threshold_gwei": threshold_gwei,
                    });
                    self.append_record(spec, &record).await?;
                    state.gas.insert(key, gp);
                }
            }
            WatchKind::Event {
                chain,
                contract,
                topic0,
            } => {
                let client = match self.chains.get(chain) {
                    Some(c) => c,
                    None => return Ok(()),
                };
                let head = client
                    .block_number()
                    .await
                    .map_err(|e| WatchError::Io(std::io::Error::other(e.to_string())))?;
                let from_block = state.event_block.get(&key).copied().map(|b| b + 1);
                let from_block = from_block.unwrap_or(head.saturating_sub(0));
                if from_block > head {
                    return Ok(());
                }
                let addr: Address = contract.parse().map_err(|e: alloy::hex::FromHexError| {
                    WatchError::InvalidId(format!("bad contract {contract}: {e}"))
                })?;
                let topic: alloy::primitives::B256 =
                    topic0.parse().map_err(|e: alloy::hex::FromHexError| {
                        WatchError::InvalidId(format!("bad topic0 {topic0}: {e}"))
                    })?;
                let filter = Filter::new()
                    .address(addr)
                    .from_block(from_block)
                    .to_block(head)
                    .event_signature(topic);
                let logs = client
                    .provider()
                    .get_logs(&filter)
                    .await
                    .map_err(|e| WatchError::Io(std::io::Error::other(e.to_string())))?;
                for log in logs {
                    let record = serde_json::json!({
                        "ts": now_ms(),
                        "kind": "event",
                        "chain": chain,
                        "contract": contract,
                        "topic0": topic0,
                        "log": log,
                    });
                    self.append_record(spec, &record).await?;
                }
                state.event_block.insert(key, head);
            }
        }
        Ok(())
    }

    /// Append one JSON record (one line) to `<wallet>/<id>/live`,
    /// rotating into history.jsonl[.N] when the live file exceeds the
    /// rotation threshold.
    pub async fn append_record<T: Serialize>(
        &self,
        spec: &WatchSpec,
        record: &T,
    ) -> Result<(), WatchError> {
        let dir = Self::watch_dir_for_spec(&self.home, spec);
        tokio::fs::create_dir_all(&dir).await?;
        let live = Self::live_path_for_spec(&self.home, spec);

        // Rotate first if needed, *before* writing the new record.
        if let Ok(meta) = tokio::fs::metadata(&live).await {
            if meta.len() >= ROTATE_THRESHOLD_BYTES {
                self.rotate(spec).await?;
            }
        }

        let line = serde_json::to_string(record)
            .map_err(|e| WatchError::Io(std::io::Error::other(e.to_string())))?;
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&live)
            .await?;
        file.write_all(line.as_bytes()).await?;
        file.write_all(b"\n").await?;
        Ok(())
    }

    /// Rotate `<id>/live` into the next available
    /// `<id>/history.jsonl(.N)` slot and write a `{sentinel:"rotate"}`
    /// line into the new live file.
    pub async fn rotate(&self, spec: &WatchSpec) -> Result<(), WatchError> {
        let dir = Self::watch_dir_for_spec(&self.home, spec);
        tokio::fs::create_dir_all(&dir).await?;
        let live = Self::live_path_for_spec(&self.home, spec);
        if !live.exists() {
            return Ok(());
        }
        let history = Self::history_path_for_spec(&self.home, spec);

        // Determine next free slot. If `history.jsonl` does not exist,
        // move live there. Otherwise find the largest N already in use
        // and write to N+1.
        let target = if !history.exists() {
            history.clone()
        } else {
            let mut n: u32 = 1;
            loop {
                let candidate = dir.join(format!("history.jsonl.{}", n));
                if !candidate.exists() {
                    break candidate;
                }
                n += 1;
            }
        };

        // Sentinel: written as the *first* line of the new live file so
        // tailing agents see it before any subsequent record.
        let sentinel = serde_json::json!({
            "sentinel": "rotate",
            "next": target.file_name().and_then(|s| s.to_str()).unwrap_or("history.jsonl"),
            "ts": now_ms(),
        });

        tokio::fs::rename(&live, &target).await?;
        let mut new_live = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&live)
            .await?;
        let line = serde_json::to_string(&sentinel)
            .map_err(|e| WatchError::Io(std::io::Error::other(e.to_string())))?;
        new_live.write_all(line.as_bytes()).await?;
        new_live.write_all(b"\n").await?;
        Ok(())
    }
}

/// Per-tick observed state, keyed by `<wallet>/<id>`. Public for tests.
#[derive(Default)]
pub struct ExecutorState {
    pub balance: HashMap<String, U256>,
    pub block: HashMap<String, u64>,
    pub gas: HashMap<String, u128>,
    pub event_block: HashMap<String, u64>,
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

// Type assertion: ensure WatchExecutor remains Send + Sync.
const _: fn() = || {
    fn assert_send<T: Send + Sync>() {}
    assert_send::<WatchExecutor>();
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration as StdDuration;
    use tempfile::tempdir;

    #[tokio::test]
    async fn rotate_moves_live_and_writes_sentinel() {
        let tmp = tempdir().unwrap();
        let home = HomeDir::at(tmp.path());
        home.ensure().unwrap();
        let registry = Arc::new(WatchRegistry::new(home.watch_dir()).unwrap());
        let chains = ChainRegistry::default();
        let exec = WatchExecutor::new(chains, registry, home.clone());

        let spec = WatchSpec {
            id: "w-0001".into(),
            wallet: "alice".into(),
            created_ms: 1,
            kind: WatchKind::GasPrice {
                chain: "anvil".into(),
                threshold_gwei: 1.0,
            },
            note: None,
        };

        // Seed live with junk past the threshold.
        let dir = WatchExecutor::watch_dir_for_spec(&home, &spec);
        std::fs::create_dir_all(&dir).unwrap();
        let live = WatchExecutor::live_path_for_spec(&home, &spec);
        std::fs::write(&live, vec![b'x'; (ROTATE_THRESHOLD_BYTES + 1) as usize]).unwrap();

        exec.append_record(&spec, &serde_json::json!({"ping": 1}))
            .await
            .unwrap();

        // After append, live should be small (sentinel + new record only),
        // and history.jsonl should hold the old contents.
        let live_meta = std::fs::metadata(&live).unwrap();
        assert!(live_meta.len() < ROTATE_THRESHOLD_BYTES);
        let history = WatchExecutor::history_path_for_spec(&home, &spec);
        assert!(history.exists());

        let live_body = std::fs::read_to_string(&live).unwrap();
        let first_line = live_body.lines().next().unwrap();
        let v: serde_json::Value = serde_json::from_str(first_line).unwrap();
        assert_eq!(v["sentinel"], "rotate");
    }

    #[tokio::test]
    async fn start_stop_idempotent() {
        let tmp = tempdir().unwrap();
        let home = HomeDir::at(tmp.path());
        home.ensure().unwrap();
        let registry = Arc::new(WatchRegistry::new(home.watch_dir()).unwrap());
        let chains = ChainRegistry::default();
        let exec = Arc::new(
            WatchExecutor::new(chains, registry, home).with_tick(StdDuration::from_millis(50)),
        );
        exec.start().unwrap();
        // Second start is a no-op.
        exec.start().unwrap();
        exec.stop().await;
    }
}
