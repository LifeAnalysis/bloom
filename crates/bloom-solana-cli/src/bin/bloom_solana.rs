//! `bloom-solana` — the devnet-first Solana CLI.
//!
//! Composes the real machine stack: pinned Petal, mediated RPC, durable
//! outbox, fixture signing (until the BIP-39 Signer edge lands). Commands
//! mirror the lifecycle; nothing here rebuilds a confirmed transaction or
//! re-signs an ambiguous one.

use bloom_solana_cli::{commands, profiles, session};

use std::path::PathBuf;
use std::sync::Arc;

use bloom_solana_cli::session::Session;
use bloom_solana_machine::fixture::ExactApprovalLedger;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "bloom-solana", about = "Bloom Solana devnet CLI")]
struct Cli {
    /// Durable state root (profiles, outbox, accounts, petal state).
    #[arg(long, default_value = default_state_root_os(), global = true)]
    state_dir: PathBuf,

    #[command(subcommand)]
    command: Command,
}

fn default_state_root_os() -> &'static str {
    // clap needs a &'static str default; resolve via HOME at runtime in run().
    ".bloom-solana"
}

#[derive(Subcommand)]
enum Command {
    /// List configured cluster profiles and enabled accounts.
    Status {
        #[arg(long, default_value = "devnet")]
        profile: String,
    },
    /// Enable the wallet's Solana account on a cluster profile.
    AccountEnable {
        #[arg(long)]
        wallet: String,
        #[arg(long, default_value = "devnet")]
        profile: String,
    },
    /// Stage a native SOL transfer (no signing, no broadcast).
    TransferStage {
        #[arg(long)]
        wallet: String,
        #[arg(long, default_value = "devnet")]
        profile: String,
        /// Destination base58 address.
        #[arg(long)]
        destination: String,
        /// Integer lamports.
        #[arg(long, conflicts_with = "sol")]
        lamports: Option<u64>,
        /// Fixed-point SOL (at most nine decimals; never parsed as float).
        #[arg(long = "sol", conflicts_with = "lamports")]
        sol: Option<String>,
        /// Approved hard fee ceiling in lamports.
        #[arg(long, default_value = "100000")]
        max_fee_lamports: u64,
    },
    /// Inspect the immutable staged operation.
    OperationInspect { operation_id: String },
    /// Confirm the exact approval for a staged operation and sign.
    OperationConfirm {
        operation_id: String,
        /// Required acknowledgment that the displayed facts are approved.
        #[arg(long)]
        yes: bool,
    },
    /// Deny a staged operation (cancel; only while unsigned).
    OperationDeny { operation_id: String },
    /// Cancel a staged operation (only while unsigned).
    OperationCancel { operation_id: String },
    /// Inspect lifecycle status/finality.
    OperationStatus { operation_id: String },
    /// Retry an ambiguous operation with the exact persisted bytes.
    OperationRetry { operation_id: String },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(run(cli))
}

async fn run(mut cli: Cli) -> anyhow::Result<()> {
    if *cli.state_dir.as_os_str() == *std::ffi::OsStr::new(".bloom-solana")
        && let Some(home) = std::env::var_os("HOME")
    {
        cli.state_dir = PathBuf::from(home).join(".bloom/solana");
    }
    std::fs::create_dir_all(&cli.state_dir)?;
    match cli.command {
        Command::Status { profile } => status(&cli.state_dir, &profile),
        Command::AccountEnable { wallet, profile } => {
            account_enable(&cli.state_dir, &wallet, &profile)
        }
        Command::TransferStage {
            wallet,
            profile,
            destination,
            lamports,
            sol,
            max_fee_lamports,
        } => {
            let lamports = match (lamports, sol) {
                (Some(l), None) => l,
                (None, Some(s)) => {
                    bloom_solana_cli::amount::parse_amount(&s, bloom_solana_cli::amount::Unit::Sol)
                        .map_err(|e| anyhow::anyhow!("--sol: {e}"))?
                }
                (None, None) => anyhow::bail!("one of --lamports or --sol is required"),
                (Some(_), Some(_)) => unreachable!("clap conflicts"),
            };
            transfer_stage(
                &cli.state_dir,
                &wallet,
                &profile,
                &destination,
                lamports,
                max_fee_lamports,
            )
            .await
        }
        Command::OperationInspect { operation_id } => {
            let session = open_readonly(&cli.state_dir, "devnet")?;
            render_operation(&session, &operation_id)
        }
        Command::OperationConfirm { operation_id, yes } => {
            operation_confirm(&cli.state_dir, &operation_id, yes).await
        }
        Command::OperationDeny { operation_id } => {
            let session = open_readonly(&cli.state_dir, "devnet")?;
            session
                .machine
                .cancel(&operation_id, session::system_now_ms())?;
            println!("denied (cancelled): {operation_id}");
            Ok(())
        }
        Command::OperationCancel { operation_id } => {
            let session = open_readonly(&cli.state_dir, "devnet")?;
            session
                .machine
                .cancel(&operation_id, session::system_now_ms())?;
            println!("cancelled: {operation_id}");
            Ok(())
        }
        Command::OperationStatus { operation_id } => {
            let session = open_readonly(&cli.state_dir, "devnet")?;
            render_operation(&session, &operation_id)
        }
        Command::OperationRetry { operation_id } => {
            let session = open_readonly(&cli.state_dir, "devnet")?;
            let now = session::system_now_ms();
            session.machine.retry_ambiguous(&operation_id, now).await?;
            println!("retry dispatched identical bytes: {operation_id}");
            Ok(())
        }
    }
}

fn status(state_root: &std::path::Path, profile_name: &str) -> anyhow::Result<()> {
    let configured = profiles::load_profiles(state_root)?;
    println!("configured profiles:");
    for p in &configured {
        println!(
            "  {} ({}), broadcast={} (profile flag; broadcast also needs the command release flag)",
            p.name, p.http_endpoint, p.allow_broadcast
        );
    }
    let accounts = bloom_solana_machine::AccountRegistry::open(state_root)?;
    let accounts = accounts.projections();
    if accounts.is_empty() {
        println!("no enabled accounts (see `account enable --wallet … --profile {profile_name}`)");
    } else {
        println!("enabled accounts:");
        for a in accounts {
            println!("  {} {} caip10={}", a.wallet_id, a.address_base58, a.caip10);
        }
    }
    Ok(())
}

fn account_enable(
    state_root: &std::path::Path,
    wallet: &str,
    profile_name: &str,
) -> anyhow::Result<()> {
    let registry = bloom_solana_machine::AccountRegistry::open(state_root)?;
    let public_key_hex = session::fixture_public_key_hex(wallet);
    let cluster_caip2 = format!("solana:{profile_name}");
    let account = registry.enable(
        wallet,
        "fixture-child-0",
        &public_key_hex,
        profile_name,
        &cluster_caip2,
        session::system_now_ms(),
    )?;
    println!("enabled: {}", account.address_base58);
    println!("caip10:  {}", account.caip10);
    println!(
        "note: fixture signing identity; the BIP-39 edge replaces it at the integration checkpoint"
    );
    Ok(())
}

fn open_readonly(state_root: &std::path::Path, profile_name: &str) -> anyhow::Result<Session> {
    let (config, _chain) = profiles::resolve(state_root, profile_name, false)?;
    // Read-only commands never broadcast; a sim transport suffices for the
    // mediated interface object the host requires.
    let transport: Arc<dyn bloom_chain_rpc::RpcTransport> =
        Arc::new(bloom_chain_rpc::SimChain::new(&config.expected_genesis_hex));
    session::open(
        state_root,
        config,
        Arc::new(ExactApprovalLedger::denying()),
        transport,
    )
}

#[allow(clippy::too_many_arguments)]
async fn transfer_stage(
    state_root: &std::path::Path,
    wallet: &str,
    profile_name: &str,
    destination: &str,
    lamports: u64,
    max_fee_lamports: u64,
) -> anyhow::Result<()> {
    let (config, _chain) = profiles::resolve(state_root, profile_name, false)?;
    // Account check first: no transport, no network, nothing staged for a
    // wallet without an enabled Solana account.
    let registry = bloom_solana_machine::AccountRegistry::open(state_root)?;
    let account = registry
        .get(wallet, profile_name)
        .map_err(|e| anyhow::anyhow!("account not enabled on {profile_name}: {e}"))?;
    let http = crate::http_transport(&config)?;
    let approvals = Arc::new(ExactApprovalLedger::denying());
    let session = session::open(state_root, config, approvals, http)?;

    let request = bloom_solana_machine::TransferRequest {
        operation_id: new_operation_id(),
        wallet_id: wallet.to_string(),
        fee_payer_base58: account.address_base58.clone(),
        destination_base58: destination.to_string(),
        lamports,
        key_ref: bloom_solana::adapter::FixtureKeyRef {
            backend: "local".into(),
            locator: account.key_ref_locator.clone(),
            public_key_hex: account.public_key_hex.clone(),
        },
        expires_at_ms: 0,
        max_fee_lamports,
        claimed_caip2: format!("solana:{}", profile_name),
    };
    let now = session::system_now_ms();
    let staged = session.machine.stage_transfer(&request, now).await?;
    println!("staged: {}", staged.operation_id);
    println!(
        "inspect with: bloom-solana operation inspect {}",
        staged.operation_id
    );
    println!(
        "confirm with: bloom-solana operation confirm {} --yes",
        staged.operation_id
    );
    Ok(())
}

async fn operation_confirm(
    state_root: &std::path::Path,
    operation_id: &str,
    yes: bool,
) -> anyhow::Result<()> {
    // Pre-render from durable records before any signing happens.
    {
        let session = open_readonly(state_root, "devnet")?;
        render_operation(&session, operation_id)?;
    }
    if !yes {
        anyhow::bail!("confirmation requires --yes after reviewing the displayed facts");
    }
    let (config, _) = profiles::resolve(state_root, "devnet", false)?;
    let transport = crate::http_transport(&config)?;
    let approvals = Arc::new(ExactApprovalLedger::new());
    let session = session::open(state_root, config, approvals, transport)?;
    commands::confirm_operation(&session.machine, &session.accounts, operation_id).await?;
    println!("signed (exact approval consumed): {operation_id}");
    println!("broadcast separately; retry stays identical-byte");
    Ok(())
}

fn render_operation(session: &Session, operation_id: &str) -> anyhow::Result<()> {
    let now = session::system_now_ms();
    let projection = session.machine.project(operation_id, now)?;
    println!("operation: {}", projection.operation_id);
    println!(
        "state:     {} (terminal={})",
        projection.state, projection.terminal
    );
    println!(
        "cluster:   {} genesis={}",
        projection.cluster.caip2, projection.cluster.expected_genesis_hex
    );
    println!();
    println!("[verifier-proven]");
    println!("  fee payer:  {}", projection.verified.fee_payer_base58);
    println!("  destination: {}", projection.verified.destination_base58);
    println!("  lamports:    {}", projection.verified.lamports);
    println!(
        "  verifier:    {} result={}",
        projection.verified.verifier_id, projection.verified.verifier_result_digest_hex
    );
    println!();
    println!("[machine-asserted — not verifier-proven]");
    println!(
        "  fee:         {} (ceiling {})",
        projection.asserted.fee_lamports, projection.asserted.max_fee_lamports
    );
    println!(
        "  total debit: {}",
        projection.asserted.total_debit_lamports
    );
    println!(
        "  blockhash:   {} (age {}ms, last-valid height {})",
        projection.asserted.blockhash_base58,
        projection.asserted.blockhash_age_ms,
        projection.asserted.last_valid_block_height
    );
    println!();
    println!("digests:  {}", projection.digest_summary());
    println!(
        "package:  {} route={} abi={}",
        projection.bindings.package_hash,
        projection.bindings.route,
        projection.bindings.abi_version
    );
    if let Some(signature) = &projection.signature_base58 {
        println!("signature: {signature}");
    }
    for attempt in &projection.attempts {
        println!(
            "attempt {}: {} {}",
            attempt.attempt,
            attempt.artifact_digest_hex,
            attempt.outcome.as_deref().unwrap_or("pending")
        );
    }
    if let Some(confirmation) = &projection.finality.confirmation {
        println!("finality: confirmed ({confirmation})");
    }
    if let Some(reason) = &projection.finality.quarantine_reason {
        println!("quarantined: {reason}");
    }
    if let Some(reason) = &projection.finality.freshness_reason {
        println!("freshness refused: {reason}");
    }
    if let Some(reason) = &projection.finality.failure_reason {
        println!("failed: {reason}");
    }
    Ok(())
}

fn new_operation_id() -> String {
    use sha2::Digest as _;
    let mut hasher = sha2::Sha256::new();
    hasher.update(b"bloom-solana-cli");
    hasher.update(session::system_now_ms().to_be_bytes());
    hasher.update(std::process::id().to_be_bytes());
    hex::encode(hasher.finalize())
}

/// The real HTTP transport behind the mediated stack; the sim is never used
/// for lifecycle effects in the CLI.
fn http_transport(
    config: &profiles::ProfileConfig,
) -> anyhow::Result<Arc<dyn bloom_chain_rpc::RpcTransport>> {
    #[cfg(feature = "http")]
    {
        Ok(Arc::new(bloom_chain_rpc::http::SolanaHttpTransport::new(
            &config.http_endpoint,
        )?))
    }
    #[cfg(not(feature = "http"))]
    {
        let _ = config;
        anyhow::bail!(
            "this binary was built without the http feature; rebuild with --features http"
        )
    }
}
