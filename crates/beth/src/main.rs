//! `beth` — bloom-eth daemon and CLI.
//!
//! For v1, the CLI drives the same in-process daemon — there's no
//! separate long-running server. Each invocation builds the daemon,
//! performs the requested VFS operation, and exits. A `serve` subcommand
//! exists as a placeholder for the eventual long-running NFS-mounted
//! daemon.

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use beth_daemon::ipc::{default_socket_path, IpcClient, IpcServer};
use beth_daemon::Daemon;
use beth_proto::HomeDir;
use beth_vfs::{handler::Handler, VfsPath};
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "beth",
    version,
    about = "bloom-eth — Ethereum as a virtual filesystem"
)]
struct Cli {
    /// Override home directory (default: ~/.bloom-eth).
    #[arg(long, env = "BETH_HOME")]
    home: Option<PathBuf>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Show daemon status (chains configured, version, home dir).
    Status,
    /// VFS path operations (no NFS mount required).
    #[command(subcommand)]
    Vfs(VfsCmd),
    /// Wallet management.
    #[command(subcommand)]
    Wallet(WalletCmd),
    /// Run the daemon as a long-lived process. The NFS mount adapter is
    /// feature-gated and currently a stub; this exists so that the
    /// invocation contract is stable.
    Serve,
    /// Talk to a running `beth serve` over its UDS JSON-RPC socket.
    #[command(subcommand)]
    Ipc(IpcCmd),
    /// Initialise ~/.bloom-eth with default config + dirs.
    Init,
}

#[derive(Subcommand, Debug)]
enum IpcCmd {
    /// Send a raw JSON-RPC call. `params` is a JSON literal (default: null).
    Call {
        method: String,
        #[arg(long)]
        params: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
enum VfsCmd {
    /// `cat /eth/<path>` — read a file via the VFS.
    Cat { path: String },
    /// `ls /eth/<path>` — list a directory via the VFS.
    Ls { path: String },
    /// Write data to a writable VFS path. Reads from stdin if `--data` is omitted.
    Write {
        path: String,
        #[arg(long)]
        data: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
enum WalletCmd {
    /// Create a new local wallet.
    New {
        name: String,
        #[arg(long, env = "BETH_PASSPHRASE")]
        passphrase: String,
    },
    /// Import a wallet from a hex private key.
    Import {
        name: String,
        private_key: String,
        #[arg(long, env = "BETH_PASSPHRASE")]
        passphrase: String,
    },
    /// List configured wallets.
    List,
    /// Unlock a wallet for the lifetime of the process.
    Unlock {
        name: String,
        #[arg(long, env = "BETH_PASSPHRASE")]
        passphrase: String,
    },
    /// Stage a tx by writing an intent file. Convenience for the
    /// outbox flow.
    Stage {
        wallet: String,
        chain: String,
        /// Intent body (JSON, TOML, or shell-style). If omitted, read
        /// from stdin.
        #[arg(long)]
        intent: Option<String>,
    },
    /// Unlock then broadcast a staged tx in one shot. Required because
    /// the v1 CLI rebuilds the daemon per invocation, so a separate
    /// `unlock` doesn't persist.
    Confirm {
        wallet: String,
        chain: String,
        id: String,
        #[arg(long, env = "BETH_PASSPHRASE")]
        passphrase: String,
        /// Confirmation text (default "y"; "override" bypasses soft
        /// policy warnings).
        #[arg(long, default_value = "y")]
        text: String,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .with_writer(std::io::stderr)
        .try_init();

    let cli = Cli::parse();
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {:#}", e);
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<()> {
    let home = match cli.home {
        Some(p) => HomeDir::at(p),
        None => HomeDir::resolve("~/.bloom-eth").context("resolving home dir")?,
    };

    match cli.cmd {
        Cmd::Init => {
            let d = Daemon::from_home(home.clone()).context("init daemon")?;
            println!("home: {}", d.home.root().display());
            println!("config: {}", d.home.config_path().display());
            println!("chains: {:?}", d.chains.list_names());
            Ok(())
        }
        Cmd::Status => {
            let d = Daemon::from_home(home).context("build daemon")?;
            println!("version: {}", env!("CARGO_PKG_VERSION"));
            println!("home: {}", d.home.root().display());
            println!("chains: {:?}", d.chains.list_names());
            println!(
                "block_mainnet_broadcast: {}",
                d.config.block_mainnet_broadcast
            );
            Ok(())
        }
        Cmd::Vfs(VfsCmd::Cat { path }) => {
            let socket = default_socket_path(home.root());
            let p = VfsPath::parse(&path).context("parse path")?;
            let bytes = if socket.exists() {
                let client = IpcClient::new(&socket);
                let res = client
                    .call("read", serde_json::json!({ "path": path }))
                    .await
                    .context("ipc read")?;
                let b64 = res
                    .get("bytes_b64")
                    .and_then(|v| v.as_str())
                    .context("ipc read: missing bytes_b64")?;
                use base64::engine::general_purpose::STANDARD as B64;
                use base64::Engine as _;
                B64.decode(b64).context("ipc read: bad base64")?
            } else {
                let d = Daemon::from_home(home).context("build daemon")?;
                d.vfs.read(&p).await.context("vfs read")?
            };
            std::io::Write::write_all(&mut std::io::stdout(), &bytes)?;
            Ok(())
        }
        Cmd::Vfs(VfsCmd::Ls { path }) => {
            let socket = default_socket_path(home.root());
            let p = VfsPath::parse(&path).context("parse path")?;
            if socket.exists() {
                let client = IpcClient::new(&socket);
                let res = client
                    .call("list", serde_json::json!({ "path": path }))
                    .await
                    .context("ipc list")?;
                let arr = res.as_array().context("ipc list: expected array")?;
                for e in arr {
                    let name = e.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    let kind = match e.get("kind").and_then(|v| v.as_str()).unwrap_or("file") {
                        "dir" => "Dir",
                        "symlink" => "Symlink",
                        _ => "File",
                    };
                    println!("{}\t{}", name, kind);
                }
            } else {
                let d = Daemon::from_home(home).context("build daemon")?;
                let entries = d.vfs.list(&p).await.context("vfs list")?;
                for e in entries {
                    println!("{}\t{:?}", e.name, e.kind);
                }
            }
            Ok(())
        }
        Cmd::Vfs(VfsCmd::Write { path, data }) => {
            let socket = default_socket_path(home.root());
            let p = VfsPath::parse(&path).context("parse path")?;
            let body = match data {
                Some(s) => s.into_bytes(),
                None => {
                    let mut buf = Vec::new();
                    std::io::Read::read_to_end(&mut std::io::stdin(), &mut buf)?;
                    buf
                }
            };
            if socket.exists() {
                use base64::engine::general_purpose::STANDARD as B64;
                use base64::Engine as _;
                let client = IpcClient::new(&socket);
                client
                    .call(
                        "write",
                        serde_json::json!({ "path": path, "bytes_b64": B64.encode(&body) }),
                    )
                    .await
                    .context("ipc write")?;
            } else {
                let d = Daemon::from_home(home).context("build daemon")?;
                d.vfs.write(&p, &body).await.context("vfs write")?;
            }
            Ok(())
        }
        Cmd::Wallet(WalletCmd::New { name, passphrase }) => {
            let d = Daemon::from_home(home).context("build daemon")?;
            let info = d.keystore.create_local(&name, &passphrase)?;
            println!("created wallet '{}': {}", info.name, info.address);
            Ok(())
        }
        Cmd::Wallet(WalletCmd::Import {
            name,
            private_key,
            passphrase,
        }) => {
            let d = Daemon::from_home(home).context("build daemon")?;
            let info = d.keystore.import_hex(&name, &private_key, &passphrase)?;
            println!("imported wallet '{}': {}", info.name, info.address);
            Ok(())
        }
        Cmd::Wallet(WalletCmd::List) => {
            let d = Daemon::from_home(home).context("build daemon")?;
            for info in d.keystore.list()? {
                let kind = match info.kind {
                    beth_keystore::WalletKind::Local => "local",
                    beth_keystore::WalletKind::Watch => "watch",
                };
                println!("{}\t{}\t{}", info.name, info.address, kind);
            }
            Ok(())
        }
        Cmd::Wallet(WalletCmd::Unlock { name, passphrase }) => {
            let d = Daemon::from_home(home).context("build daemon")?;
            d.keystore.unlock(&name, &passphrase)?;
            println!("unlocked '{}' (in-memory; ends with this process)", name);
            Ok(())
        }
        Cmd::Wallet(WalletCmd::Stage {
            wallet,
            chain,
            intent,
        }) => {
            let d = Daemon::from_home(home).context("build daemon")?;
            let body = match intent {
                Some(s) => s,
                None => {
                    let mut buf = String::new();
                    std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)?;
                    buf
                }
            };
            let parsed = beth_tx::intent_parser::parse(&body).context("parse intent")?;
            let info = d.keystore.info(&wallet)?;
            let client = d
                .chains
                .get(&chain)
                .with_context(|| format!("chain '{}'", chain))?;
            let staged = d
                .tx_engine
                .stage(
                    &wallet,
                    info.address,
                    parsed,
                    &client,
                    &info.policy,
                    Some(&d.address_book),
                )
                .await?;
            println!("{}", staged.id);
            Ok(())
        }
        Cmd::Wallet(WalletCmd::Confirm {
            wallet,
            chain,
            id,
            passphrase,
            text,
        }) => {
            let d = Daemon::from_home(home).context("build daemon")?;
            d.keystore.unlock(&wallet, &passphrase)?;
            let signer = d.keystore.signer(&wallet)?;
            let client = d
                .chains
                .get(&chain)
                .with_context(|| format!("chain '{}'", chain))?;
            let staged = d
                .tx_engine
                .confirm(&wallet, &chain, &id, &client, &signer, &text)
                .await?;
            println!(
                "broadcast {} hash={}",
                staged.id,
                staged.tx_hash.as_deref().unwrap_or("?")
            );
            Ok(())
        }
        Cmd::Serve => {
            let d = Daemon::from_home(home).context("build daemon")?;
            let chains: Vec<String> = d.chains.list_names();
            println!(
                "beth serve: home={} chains={:?}",
                d.home.root().display(),
                chains
            );
            let socket = default_socket_path(d.home.root());
            println!("ipc socket: {}", socket.display());
            let server = IpcServer::new(d.vfs.clone(), env!("CARGO_PKG_VERSION"), chains);
            let server2 = server.clone();
            // Trigger graceful shutdown on Ctrl-C.
            let shutdown = tokio::spawn(async move {
                let _ = tokio::signal::ctrl_c().await;
                server2.trigger_shutdown();
            });
            server.serve(&socket).await.context("ipc serve")?;
            shutdown.abort();
            println!("shutting down");
            Ok(())
        }
        Cmd::Ipc(IpcCmd::Call { method, params }) => {
            let socket = default_socket_path(home.root());
            let client = IpcClient::new(&socket);
            let v: serde_json::Value = match params {
                Some(s) => serde_json::from_str(&s).context("parse params JSON")?,
                None => serde_json::Value::Null,
            };
            let result = client
                .call(&method, v)
                .await
                .with_context(|| format!("ipc call to {}", socket.display()))?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            Ok(())
        }
    }
}
