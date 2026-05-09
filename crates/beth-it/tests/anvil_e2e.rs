//! End-to-end integration test for the bloom-eth stage-confirm flow.
//!
//! Runs against a real `anvil` instance spawned as a child process. Marked
//! `#[ignore]` so it only runs when explicitly requested:
//!
//! ```text
//! cargo test -p beth-it -- --ignored
//! ```
//!
//! Requires the `anvil` and `cast` binaries from Foundry to be available
//! at `~/.foundry/bin/{anvil,cast}` (or on `$PATH`).

use std::net::TcpListener;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use beth_daemon::Daemon;
use beth_proto::{ChainSpec, Config, HomeDir};
use beth_vfs::handler::Handler;
use beth_vfs::VfsPath;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::time::{sleep, timeout};

const ANVIL_BIN: &str = "/Users/joshua/.foundry/bin/anvil";
const CAST_BIN: &str = "/Users/joshua/.foundry/bin/cast";
const FUNDER_PRIV_KEY: &str = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";

/// RAII guard that kills the spawned anvil process on drop.
struct AnvilGuard {
    child: Option<Child>,
    port: u16,
}

impl AnvilGuard {
    fn rpc_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

impl Drop for AnvilGuard {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            // Best-effort kill. start_kill is sync; the OS will reap.
            let _ = c.start_kill();
        }
    }
}

/// Pick an OS-assigned free TCP port by binding to :0 and releasing it.
fn pick_free_port() -> Result<u16> {
    let l = TcpListener::bind("127.0.0.1:0")?;
    Ok(l.local_addr()?.port())
}

/// Spawn anvil on the given port and wait until its stdout shows it is
/// listening. Returns a guard that kills the child on drop.
async fn spawn_anvil() -> Result<AnvilGuard> {
    let port = pick_free_port()?;
    let mut cmd = Command::new(ANVIL_BIN);
    cmd.arg("--port")
        .arg(port.to_string())
        .arg("--host")
        .arg("127.0.0.1")
        // Determinism: chain id 31337, default mnemonic.
        .arg("--chain-id")
        .arg("31337")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = cmd.spawn().context("spawn anvil")?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("anvil stdout missing"))?;

    // Read lines until we see "Listening on" or hit a timeout.
    let mut reader = BufReader::new(stdout).lines();
    let wait = async {
        loop {
            match reader.next_line().await? {
                Some(line) => {
                    if line.contains("Listening on") {
                        return Ok::<(), anyhow::Error>(());
                    }
                }
                None => return Err(anyhow!("anvil exited before becoming ready")),
            }
        }
    };
    timeout(Duration::from_secs(15), wait)
        .await
        .map_err(|_| anyhow!("timed out waiting for anvil to start"))??;

    Ok(AnvilGuard {
        child: Some(child),
        port,
    })
}

/// Fund `to_addr` with `value_eth` ETH from anvil's prefunded account #0.
async fn fund_via_cast(rpc_url: &str, to_addr: &str, value_eth: u64) -> Result<()> {
    let out = Command::new(CAST_BIN)
        .arg("send")
        .arg("--private-key")
        .arg(FUNDER_PRIV_KEY)
        .arg("--rpc-url")
        .arg(rpc_url)
        .arg(to_addr)
        .arg("--value")
        .arg(format!("{}ether", value_eth))
        .output()
        .await
        .context("invoke cast send")?;
    if !out.status.success() {
        return Err(anyhow!(
            "cast send failed: stdout={} stderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(())
}

/// Build a config.toml under `home` that points the anvil chain at our spawned
/// node and enables broadcast.
fn write_config(home_root: &std::path::Path, rpc_url: &str) -> Result<()> {
    let mut cfg = Config::local_default();
    let mut spec = ChainSpec::anvil_default();
    spec.rpc_urls = vec![rpc_url.to_string()];
    spec.allow_broadcast = true;
    cfg.chains.insert("anvil".to_string(), spec);
    let path = home_root.join("config.toml");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    cfg.save(&path).map_err(|e| anyhow!("save config: {e}"))?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn anvil_full_stage_confirm_flow() -> Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,beth_tx=info")),
        )
        .with_test_writer()
        .try_init();

    // 1. Anvil up.
    let anvil = spawn_anvil().await?;
    let rpc_url = anvil.rpc_url();

    // 2. Build a daemon under a temp home pointing at our anvil.
    let tmp = tempfile::tempdir()?;
    let home_root = tmp.path().to_path_buf();
    write_config(&home_root, &rpc_url)?;
    let home = HomeDir::at(&home_root);
    let daemon = Daemon::from_home(home).map_err(|e| anyhow!("daemon: {e}"))?;

    // 3. Create a wallet via the keystore.
    let passphrase = "integration-test-pass";
    let info = daemon
        .keystore
        .create_local("alice", passphrase)
        .map_err(|e| anyhow!("create_local: {e}"))?;
    let alice_addr = format!("{:#x}", info.address);

    // 4. Fund alice from anvil's prefunded #0 via `cast send`.
    fund_via_cast(&rpc_url, &alice_addr, 10).await?;

    // Allow the funding tx to be picked up by anvil's auto-mine.
    sleep(Duration::from_millis(250)).await;

    // 5. Verify the balance is reflected through the VFS.
    let bal_path = VfsPath::parse("/wallets/alice/chains/anvil/balance.eth").unwrap();
    let bal_bytes = daemon
        .vfs
        .read(&bal_path)
        .await
        .map_err(|e| anyhow!("read balance: {e}"))?;
    let bal_str = String::from_utf8(bal_bytes)?;
    assert!(
        bal_str.contains("ETH"),
        "balance.eth missing 'ETH' suffix: {bal_str:?}"
    );
    assert!(
        bal_str.starts_with("10"),
        "expected 10 ETH balance, got {bal_str:?}"
    );

    // 6. Stage a send by writing into outbox/new.tx.
    //    Recipient: anvil prefunded account #1
    //    (0x70997970C51812dc3A010C7d01b50e0d17dc79C8).
    let recipient = "0x70997970C51812dc3A010C7d01b50e0d17dc79C8";
    let intent = serde_json::json!({
        "kind": "send",
        "to": recipient,
        "value": "1 eth",
        "chain": "anvil",
    })
    .to_string();
    let new_tx_path = VfsPath::parse("/wallets/alice/chains/anvil/outbox/new.tx").unwrap();
    daemon
        .vfs
        .write(&new_tx_path, intent.as_bytes())
        .await
        .map_err(|e| anyhow!("stage write: {e}"))?;

    // Confirm a pending entry now exists.
    let pending_dir = VfsPath::parse("/wallets/alice/chains/anvil/outbox/pending").unwrap();
    let entries = daemon
        .vfs
        .list(&pending_dir)
        .await
        .map_err(|e| anyhow!("list pending: {e}"))?;
    assert_eq!(entries.len(), 1, "expected exactly one pending entry");
    let pending_id = entries[0].name.clone();

    // 7. Read plan.md and policy_check.json from the pending dir.
    let plan_path = VfsPath::parse(&format!(
        "/wallets/alice/chains/anvil/outbox/pending/{pending_id}/plan.md"
    ))
    .unwrap();
    let plan_bytes = daemon
        .vfs
        .read(&plan_path)
        .await
        .map_err(|e| anyhow!("read plan.md: {e}"))?;
    let plan = String::from_utf8(plan_bytes)?;
    assert!(!plan.is_empty(), "plan.md is empty");

    let policy_path = VfsPath::parse(&format!(
        "/wallets/alice/chains/anvil/outbox/pending/{pending_id}/policy_check.json"
    ))
    .unwrap();
    let policy_bytes = daemon
        .vfs
        .read(&policy_path)
        .await
        .map_err(|e| anyhow!("read policy_check.json: {e}"))?;
    let _: serde_json::Value =
        serde_json::from_slice(&policy_bytes).context("policy_check.json must be valid JSON")?;

    // 8. Unlock the wallet, then write `confirm` to broadcast.
    daemon
        .keystore
        .unlock("alice", passphrase)
        .map_err(|e| anyhow!("unlock: {e}"))?;
    let confirm_path = VfsPath::parse(&format!(
        "/wallets/alice/chains/anvil/outbox/pending/{pending_id}/confirm"
    ))
    .unwrap();
    daemon
        .vfs
        .write(&confirm_path, b"y")
        .await
        .map_err(|e| anyhow!("confirm write: {e}"))?;

    // 9. Verify the entry now lives in `sent/` with a tx_hash artefact.
    let sent_dir = VfsPath::parse("/wallets/alice/chains/anvil/outbox/sent").unwrap();
    let sent_entries = daemon
        .vfs
        .list(&sent_dir)
        .await
        .map_err(|e| anyhow!("list sent: {e}"))?;
    assert!(
        sent_entries.iter().any(|e| e.name == pending_id),
        "expected {pending_id} in sent/, got {:?}",
        sent_entries.iter().map(|e| &e.name).collect::<Vec<_>>()
    );

    let tx_hash_path = VfsPath::parse(&format!(
        "/wallets/alice/chains/anvil/outbox/sent/{pending_id}/tx_hash"
    ))
    .unwrap();
    let tx_hash_bytes = daemon
        .vfs
        .read(&tx_hash_path)
        .await
        .map_err(|e| anyhow!("read tx_hash: {e}"))?;
    let tx_hash = String::from_utf8(tx_hash_bytes)?;
    assert!(
        tx_hash.starts_with("0x") && tx_hash.len() >= 66,
        "tx_hash looks malformed: {tx_hash:?}"
    );

    // intent.json should reflect Sent status with a tx_hash.
    let intent_path = VfsPath::parse(&format!(
        "/wallets/alice/chains/anvil/outbox/sent/{pending_id}/intent.json"
    ))
    .unwrap();
    let intent_bytes = daemon
        .vfs
        .read(&intent_path)
        .await
        .map_err(|e| anyhow!("read intent.json: {e}"))?;
    let intent_val: serde_json::Value = serde_json::from_slice(&intent_bytes)?;
    assert_eq!(
        intent_val.get("status").and_then(|v| v.as_str()),
        Some("sent"),
        "intent.json status should be 'sent', got {intent_val}"
    );
    assert!(
        intent_val
            .get("tx_hash")
            .and_then(|v| v.as_str())
            .map(|s| s.starts_with("0x"))
            .unwrap_or(false),
        "intent.json missing tx_hash"
    );

    // Drop guard kills anvil.
    drop(anvil);
    Ok(())
}
