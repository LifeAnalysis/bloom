//! `bloom` — bloom daemon and CLI.
//!
//! For v1, the CLI drives the same in-process daemon — there's no
//! separate long-running server. Each invocation builds the daemon,
//! performs the requested VFS operation, and exits. A `serve` subcommand
//! exists as a placeholder for the eventual long-running NFS-mounted
//! daemon.

#![forbid(unsafe_code)]

mod commands {
    pub mod qr;
}
mod github_source;
mod session_sentinel;
mod triad_enrollment;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::AtomicI32;
use std::time::{SystemTime, UNIX_EPOCH};

static UPDATE_EXIT_CODE: AtomicI32 = AtomicI32::new(0);

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use bloom_daemon::Daemon;
use bloom_daemon::ipc::{IpcClient, IpcServer, default_socket_path};
use bloom_hyperliquid::{HyperliquidClient, HyperliquidNetwork, UsdSendRequest, pretty_json};
use bloom_proto::{HomeDir, HomeWritePermit};
use bloom_vfs::{
    VfsPath,
    handler::{Entry, EntryKind, Handler},
};
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{Shell, generate};
use tracing::{debug, info, trace};
use tracing_subscriber::EnvFilter;

#[cfg(target_os = "linux")]
const DEFAULT_MOUNT_PATH: &str = "/bloom";
#[cfg(target_os = "macos")]
const DEFAULT_MOUNT_PATH: &str = "/Volumes/bloom";
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const DEFAULT_MOUNT_PATH: &str = "/bloom";

const ALPHA_DISCLOSURE: &str = "⚠️  Bloom is experimental, unaudited alpha software. Do not use with funds you cannot afford to lose. Review every generated transaction plan before signing.";
#[derive(Debug, Clone, PartialEq, Eq)]
enum EndpointSource {
    Default,
    Explicit,
}

#[derive(Debug, Clone)]
struct ResolvedEndpoint {
    socket: PathBuf,
    source: EndpointSource,
    display: String,
}

impl ResolvedEndpoint {
    fn default_for_home(home: &HomeDir) -> Self {
        let socket = default_socket_path(home.root());
        Self {
            display: format!("unix:{}", socket.display()),
            socket,
            source: EndpointSource::Default,
        }
    }

    fn explicit(raw: &str) -> Result<Self> {
        let path = parse_unix_endpoint(raw)?;
        Ok(Self {
            display: format!("unix:{}", path.display()),
            socket: path,
            source: EndpointSource::Explicit,
        })
    }

    fn explicit_socket(path: PathBuf) -> Self {
        Self {
            display: format!("unix:{}", path.display()),
            socket: path,
            source: EndpointSource::Explicit,
        }
    }

    fn is_explicit(&self) -> bool {
        matches!(self.source, EndpointSource::Explicit)
    }
}

fn parse_unix_endpoint(raw: &str) -> Result<PathBuf> {
    if let Some(rest) = raw.strip_prefix("unix:") {
        if rest.is_empty() {
            anyhow::bail!("empty unix endpoint path");
        }
        Ok(PathBuf::from(rest))
    } else if raw.starts_with("tcp:") || raw.starts_with("fd:") || raw == "stdio" {
        anyhow::bail!("unsupported Bloom endpoint '{raw}' (only unix:/path is implemented)");
    } else {
        Ok(PathBuf::from(raw))
    }
}

fn resolve_client_endpoint(
    home: &HomeDir,
    connect: Option<&str>,
    ipc_socket: Option<&Path>,
) -> Result<ResolvedEndpoint> {
    if let Some(raw) = connect {
        return ResolvedEndpoint::explicit(raw);
    }
    if let Some(path) = ipc_socket {
        return Ok(ResolvedEndpoint::explicit_socket(path.to_path_buf()));
    }
    Ok(ResolvedEndpoint::default_for_home(home))
}

fn resolve_server_endpoint(home: &HomeDir, endpoint: Option<&str>) -> Result<ResolvedEndpoint> {
    if let Some(raw) = endpoint {
        return ResolvedEndpoint::explicit(raw);
    }
    Ok(ResolvedEndpoint::default_for_home(home))
}

fn configured_broker_client() -> Result<bloom_machine_client::MachineBrokerClient> {
    let installed = installed_macos_triad_paths()?;
    let broker_socket = std::env::var_os("BLOOM_BROKER_SOCKET")
        .map(std::path::PathBuf::from)
        .or_else(|| installed.as_ref().map(|paths| paths.broker_socket.clone()))
        .unwrap_or_else(|| std::path::PathBuf::from("/var/run/bloom/broker.sock"));
    let machine_identity = std::env::var_os("BLOOM_MACHINE_IDENTITY")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            installed
                .as_ref()
                .map(|paths| paths.machine_identity.clone())
        })
        .unwrap_or_else(|| std::path::PathBuf::from("/var/run/bloom/machine-identity.json"));
    let edge_manifest = std::env::var_os("BLOOM_EDGE_MANIFEST")
        .map(std::path::PathBuf::from)
        .or_else(|| installed.as_ref().map(|paths| paths.edge_manifest.clone()))
        .unwrap_or_else(|| std::path::PathBuf::from("/etc/bloom/edge-manifest.json"));
    bloom_machine_client::MachineBrokerClient::connect_unix_from_files(
        broker_socket,
        machine_identity,
        edge_manifest,
    )
    .context("load authenticated Machine-to-Broker edge")
}

async fn installed_triad_health_check(expected_build: &str) -> Result<()> {
    use bloom_triad_protocol::{
        Digest32, Empty, MachineBrokerRequest, MachineBrokerResponse, ReadinessState,
    };

    let expected_build =
        Digest32::new(expected_build.to_owned()).context("parse expected release digest")?;
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        configured_broker_client()?.request(MachineBrokerRequest::BrokerReadiness(Empty {})),
    )
    .await
    .context("authenticated Broker readiness timed out")?
    .context("request authenticated Broker readiness")?;
    let readiness = match response {
        MachineBrokerResponse::BrokerReadiness(readiness) => readiness,
        _ => bail!("Broker returned the wrong response to broker.readiness"),
    };
    if readiness.service_id.as_str() != "bloom-broker"
        || readiness.build_digest != expected_build
        || readiness.state != ReadinessState::Ready
    {
        bail!("Broker/Signer triad is not ready on the exact installed build");
    }
    Ok(())
}

fn configured_broker_connection() -> Result<(
    bloom_machine_client::MachineBrokerClient,
    bloom_triad_protocol::ProvenanceCatalog,
)> {
    let broker = configured_broker_client()?;
    let installed = installed_macos_triad_paths()?;
    let provenance_catalog = std::env::var_os("BLOOM_PROVENANCE_CATALOG")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            installed
                .as_ref()
                .map(|paths| paths.provenance_catalog.clone())
        })
        .unwrap_or_else(|| std::path::PathBuf::from("/etc/bloom/provenance-catalog.json"));
    let catalog = bloom_machine_client::load_provenance_catalog(provenance_catalog)
        .context("load installer-owned provenance catalog")?;
    Ok((broker, catalog))
}

#[derive(Clone)]
struct InstalledMacosTriadPaths {
    broker_socket: PathBuf,
    machine_identity: PathBuf,
    edge_manifest: PathBuf,
    provenance_catalog: PathBuf,
}

fn installed_macos_triad_paths() -> Result<Option<InstalledMacosTriadPaths>> {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::MetadataExt as _;

        let uid = rustix::process::geteuid().as_raw();
        let enrollment = PathBuf::from(format!(
            "/Library/Application Support/BloomTriad/enrollments/{uid}.json"
        ));
        let metadata = match std::fs::symlink_metadata(&enrollment) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).context("inspect installed Bloom enrollment"),
        };
        if !metadata.file_type().is_file()
            || metadata.file_type().is_symlink()
            || metadata.uid() != 0
            || metadata.mode() & 0o022 != 0
            || metadata.nlink() != 1
        {
            bail!("installed Bloom enrollment has unsafe ownership or type");
        }
        let config = PathBuf::from(format!(
            "/Library/Application Support/BloomTriad/config/{uid}"
        ));
        return Ok(Some(InstalledMacosTriadPaths {
            broker_socket: PathBuf::from(format!(
                "/private/var/run/bloom/{uid}/machine-broker/broker.sock"
            )),
            machine_identity: config.join("machine/identity.json"),
            edge_manifest: config.join("edge-manifest.json"),
            provenance_catalog: config.join("provenance-catalog.json"),
        }));
    }
    #[cfg(not(target_os = "macos"))]
    Ok(None)
}

fn build_write_daemon(home: HomeDir) -> Result<(Arc<HomeWritePermit>, Daemon)> {
    let permit = Arc::new(HomeWritePermit::acquire(&home)?);
    let daemon = match configured_broker_connection() {
        Ok((broker, catalog)) => {
            Daemon::from_home_with_permit_and_broker(home, permit.clone(), broker, catalog)
        }
        Err(error) => {
            tracing::warn!(
                error = %error,
                "Broker unavailable; Machine remains available for reads, staging, and simulation"
            );
            Daemon::from_home_with_permit(home, permit.clone())
        }
    }
    .context("build daemon")?;
    Ok((permit, daemon))
}

async fn launch_custody_ceremony(
    home: &HomeDir,
    requested_name: &str,
    method: bloom_machine_client::CustodyPrepareMethod,
    ceremony_kind: bloom_triad_protocol::CeremonyKind,
    wallet_id: Option<bloom_triad_protocol::Token>,
    expected_input_class: &str,
) -> Result<()> {
    use rand::RngCore as _;
    use sha2::Digest as _;

    bloom_keystore::Keystore::validate_name(requested_name)
        .context("requested wallet name must be a safe single path segment")?;
    bloom_triad_protocol::Token::new(requested_name.to_owned())
        .context("requested wallet name must be a protocol token")?;
    let client = configured_broker_client()
        .context("custody requires the authenticated Machine-to-Broker edge")?;
    let mut operation_bytes = [0_u8; 32];
    rand::thread_rng().fill_bytes(&mut operation_bytes);
    let operation_id = bloom_triad_protocol::OperationId::from_bytes(operation_bytes);
    let reviewed_terms = serde_jcs::to_vec(&serde_json::json!({
        "ceremony_kind": ceremony_kind,
        "requested_machine_name": requested_name,
        "wallet_id": wallet_id.clone(),
    }))
    .context("canonicalize custody launch terms")?;
    let response = client
        .prepare_custody(
            method,
            bloom_triad_protocol::CustodyPrepareRequest {
                ceremony_kind,
                custody_operation_id: operation_id,
                wallet_id,
                key_ref: None,
                exact_terms_digest: bloom_triad_protocol::Digest32::from_bytes(
                    sha2::Sha256::digest(reviewed_terms).into(),
                ),
                expected_input_class: bloom_triad_protocol::Token::new(expected_input_class)
                    .context("custody input class")?,
                browser_output_recipient_key: None,
            },
        )
        .await
        .map_err(anyhow::Error::new)
        .context("prepare Broker custody ceremony")?;
    let projection = bloom_machine_client::CeremonyProjection::from_custody_prepare(
        &response,
        current_unix_ms(),
    )
    .map_err(anyhow::Error::new)
    .context("construct Machine custody projection")?;
    let projection_path = persist_ceremony_projection(home, &projection)?;
    println!("operation_id: {}", response.custody_operation_id);
    println!("ceremony_kind: {:?}", response.ceremony_kind);
    println!("ceremony_url: {}", response.ceremony_url);
    println!(
        "ceremony_expires_at_ms: {}",
        response.ceremony_expires_at_ms.get()
    );
    println!("projection: {}", projection_path.display());
    Ok(())
}

const MAX_POLICY_DOCUMENT_BYTES: u64 = 1024 * 1024;

async fn prepare_policy_update(
    home: &HomeDir,
    requested_name: &str,
    policy_file: &Path,
    assurance_level: &str,
) -> Result<()> {
    use rand::RngCore as _;
    use sha2::Digest as _;

    bloom_keystore::Keystore::validate_name(requested_name)
        .context("wallet name must be a safe single path segment")?;
    let wallet_id = bloom_triad_protocol::Token::new(requested_name.to_owned())
        .context("wallet name must be a protocol token")?;
    let assurance_level = bloom_triad_protocol::Token::new(assurance_level.to_owned())
        .context("assurance level must be a protocol token")?;
    let metadata = std::fs::metadata(policy_file)
        .with_context(|| format!("inspect proposed policy {}", policy_file.display()))?;
    anyhow::ensure!(
        metadata.is_file() && metadata.len() <= MAX_POLICY_DOCUMENT_BYTES,
        "proposed policy must be a regular file no larger than {MAX_POLICY_DOCUMENT_BYTES} bytes"
    );
    let input = std::fs::read(policy_file)
        .with_context(|| format!("read proposed policy {}", policy_file.display()))?;
    let proposed: bloom_triad_protocol::CanonicalWalletPolicy = serde_json::from_slice(&input)
        .with_context(|| {
            format!(
                "parse proposed policy {} as canonical policy JSON",
                policy_file.display()
            )
        })?;
    anyhow::ensure!(
        proposed.wallet_id == wallet_id,
        "proposed policy wallet_id does not match requested wallet"
    );
    let proposed_bytes =
        serde_jcs::to_vec(&proposed).context("canonicalize proposed policy document")?;

    let client = configured_broker_client()
        .context("policy update requires the authenticated Machine-to-Broker edge")?;
    let baseline = client
        .policy(wallet_id.clone())
        .await
        .map_err(anyhow::Error::new)
        .context("read Signer-authenticated policy baseline from Broker")?;
    let baseline_bytes = baseline.canonical_policy.decode();
    anyhow::ensure!(
        bloom_triad_protocol::Digest32::from_bytes(sha2::Sha256::digest(&baseline_bytes).into())
            == baseline.policy_digest,
        "Broker policy baseline digest does not match its canonical bytes"
    );
    let baseline_policy: bloom_triad_protocol::CanonicalWalletPolicy =
        serde_json::from_slice(&baseline_bytes).context("parse Broker policy baseline")?;
    anyhow::ensure!(
        serde_jcs::to_vec(&baseline_policy).context("canonicalize Broker policy baseline")?
            == baseline_bytes,
        "Broker policy baseline is not canonical"
    );
    anyhow::ensure!(
        baseline_policy.wallet_id == wallet_id,
        "Broker policy baseline names another wallet"
    );

    let authority_diff =
        bloom_triad_protocol::canonical_policy_authority_diff(&baseline_policy, &proposed);
    let mut operation_bytes = [0_u8; 32];
    rand::thread_rng().fill_bytes(&mut operation_bytes);
    let operation_id = bloom_triad_protocol::OperationId::from_bytes(operation_bytes);
    let request = bloom_triad_protocol::PolicyUpdateRequest {
        operation_id,
        wallet_id,
        baseline_version: baseline.version,
        baseline_digest: baseline.policy_digest,
        proposed_canonical_policy: bloom_triad_protocol::Base64UrlBytes::from_bytes(
            &proposed_bytes,
        ),
        proposed_policy_digest: bloom_triad_protocol::Digest32::from_bytes(
            sha2::Sha256::digest(&proposed_bytes).into(),
        ),
        authority_diff_digest: authority_diff
            .digest()
            .map_err(anyhow::Error::new)
            .context("digest canonical policy authority diff")?,
        assurance_level,
    };
    let response = client
        .validate_policy_update(request)
        .await
        .map_err(anyhow::Error::new)
        .context("validate policy update and prepare Broker-originated custody ceremony")?;
    let projection =
        bloom_machine_client::CeremonyProjection::from_policy_prepare(&response, current_unix_ms())
            .map_err(anyhow::Error::new)
            .context("construct Machine policy-update projection")?;
    let projection_path = persist_ceremony_projection(home, &projection)?;
    println!("operation_id: {}", response.operation_id);
    println!("ceremony_kind: {:?}", response.ceremony_kind);
    println!(
        "review_manifest_digest: {}",
        response.review_manifest_digest
    );
    println!("ceremony_url: {}", response.ceremony_url);
    println!(
        "ceremony_expires_at_ms: {}",
        response.ceremony_expires_at_ms.get()
    );
    println!("projection: {}", projection_path.display());
    Ok(())
}

async fn commit_policy_update(home: &HomeDir, operation_id: String) -> Result<()> {
    let operation_id = bloom_triad_protocol::OperationId::new(operation_id)
        .context("operation ID must be 64 lowercase hexadecimal characters")?;
    let client = configured_broker_client()
        .context("policy commit requires the authenticated Machine-to-Broker edge")?;
    let ceremony_receipt = client
        .custody_result(bloom_triad_protocol::OperationRequest {
            operation_id: operation_id.clone(),
        })
        .await
        .map_err(anyhow::Error::new)
        .context("retrieve completed policy-update ceremony receipt")?;
    anyhow::ensure!(
        is_completed_policy_update_receipt(&ceremony_receipt, &operation_id),
        "policy commit requires the matching completed policy_update ceremony receipt"
    );

    let receipt = client
        .commit_policy_update(bloom_triad_protocol::PolicyCommitUpdateRequest {
            operation_id: operation_id.clone(),
            ceremony_receipt,
        })
        .await
        .map_err(anyhow::Error::new)
        .context("commit policy update through Broker and Signer compare-and-swap")?;
    anyhow::ensure!(
        receipt.operation_id == operation_id,
        "Broker policy commit receipt operation identity mismatch"
    );

    if let Ok(status) = client.ceremony_status(operation_id.clone()).await {
        let now_ms = current_unix_ms();
        let mut projection = match load_ceremony_projection(home, &operation_id)? {
            Some(mut projection) => {
                projection
                    .reconcile_custody(&status, now_ms)
                    .map_err(anyhow::Error::new)
                    .context("reconcile committed policy-update projection")?;
                projection
            }
            None => bloom_machine_client::CeremonyProjection::from_custody_status(&status, now_ms)
                .map_err(anyhow::Error::new)
                .context("rebuild committed policy-update projection")?,
        };
        projection.expire_launch_secret(now_ms);
        persist_ceremony_projection(home, &projection)?;
    }

    println!(
        "{}",
        serde_json::to_string_pretty(&receipt).context("encode policy commit receipt")?
    );
    Ok(())
}

fn is_completed_policy_update_receipt(
    receipt: &bloom_triad_protocol::CustodyResult,
    operation_id: &bloom_triad_protocol::OperationId,
) -> bool {
    receipt.custody_operation_id == *operation_id
        && receipt.ceremony_kind == bloom_triad_protocol::CeremonyKind::PolicyUpdate
        && receipt.public_status == bloom_triad_protocol::CeremonyState::Succeeded
}

fn current_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

fn ceremony_projection_path(home: &HomeDir, operation_id: &str) -> PathBuf {
    home.root()
        .join("triad")
        .join("ceremonies")
        .join(format!("{operation_id}.json"))
}

fn persist_ceremony_projection(
    home: &HomeDir,
    projection: &bloom_machine_client::CeremonyProjection,
) -> Result<PathBuf> {
    use std::io::Write as _;

    let operation_id = projection
        .operation_id()
        .context("custody projection is missing operation identity")?;
    let path = ceremony_projection_path(home, operation_id.as_str());
    let parent = path.parent().context("ceremony projection parent")?;
    std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("protect {}", parent.display()))?;
    }
    let mut suffix = [0_u8; 8];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut suffix);
    let temp_path = parent.join(format!(
        ".{}.{}.tmp",
        operation_id.as_str(),
        hex::encode(suffix)
    ));
    let bytes = serde_json::to_vec_pretty(projection).context("encode ceremony projection")?;
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temp_path)
        .with_context(|| format!("create {}", temp_path.display()))?;
    file.write_all(&bytes)
        .with_context(|| format!("write {}", temp_path.display()))?;
    file.sync_all()
        .with_context(|| format!("sync {}", temp_path.display()))?;
    std::fs::rename(&temp_path, &path).with_context(|| format!("publish {}", path.display()))?;
    Ok(path)
}

fn load_ceremony_projection(
    home: &HomeDir,
    operation_id: &bloom_triad_protocol::OperationId,
) -> Result<Option<bloom_machine_client::CeremonyProjection>> {
    let path = ceremony_projection_path(home, operation_id.as_str());
    match std::fs::read(&path) {
        Ok(bytes) => {
            let projection = serde_json::from_slice(&bytes)
                .with_context(|| format!("parse {}", path.display()))?;
            Ok(Some(projection))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}

async fn handle_ceremony(home: &HomeDir, command: CeremonyCmd) -> Result<()> {
    let (operation_id, action) = match command {
        CeremonyCmd::Status { operation_id } => (operation_id, "status"),
        CeremonyCmd::Cancel { operation_id } => (operation_id, "cancel"),
        CeremonyCmd::Result { operation_id } => (operation_id, "result"),
    };
    let operation_id = bloom_triad_protocol::OperationId::new(operation_id)
        .context("operation ID must be 64 lowercase hexadecimal characters")?;
    let client = configured_broker_client()
        .context("ceremony operations require the authenticated Machine-to-Broker edge")?;
    if action == "result" {
        let result = client
            .custody_result(bloom_triad_protocol::OperationRequest {
                operation_id: operation_id.clone(),
            })
            .await
            .map_err(anyhow::Error::new)
            .context("retrieve Broker custody result")?;
        anyhow::ensure!(
            result.custody_operation_id == operation_id,
            "Broker custody result operation identity mismatch"
        );
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "ceremony_kind": result.ceremony_kind,
                "operation_id": result.custody_operation_id,
                "state": result.public_status,
                "wallet_id": result.wallet_id,
                "public_key_refs": result.public_key_refs,
                "credential_summaries": result.credential_summaries,
                "receipt_digest": result.receipt_digest,
                "has_encrypted_browser_result": result.encrypted_browser_result.is_some(),
            }))
            .context("encode public custody result")?
        );
        return Ok(());
    }

    let status = if action == "cancel" {
        client
            .cancel_ceremony(operation_id.clone())
            .await
            .map_err(anyhow::Error::new)
            .context("cancel Broker ceremony")?
    } else {
        client
            .ceremony_status(operation_id.clone())
            .await
            .map_err(anyhow::Error::new)
            .context("read Broker ceremony status")?
    };
    anyhow::ensure!(
        status.operation_id == operation_id,
        "Broker ceremony status operation identity mismatch"
    );
    let now_ms = current_unix_ms();
    let mut projection = match load_ceremony_projection(home, &operation_id)? {
        Some(mut projection) => {
            projection
                .reconcile_custody(&status, now_ms)
                .map_err(anyhow::Error::new)
                .context("reconcile durable Machine ceremony projection")?;
            projection
        }
        None => bloom_machine_client::CeremonyProjection::from_custody_status(&status, now_ms)
            .map_err(anyhow::Error::new)
            .context("rebuild Machine ceremony projection from Broker")?,
    };
    projection.expire_launch_secret(now_ms);
    let path = persist_ceremony_projection(home, &projection)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&projection).context("encode ceremony projection")?
    );
    println!("projection: {}", path.display());
    Ok(())
}

#[derive(Parser, Debug)]
#[command(
    name = "bloom",
    version,
    about = "Bloom — an agentic Ethereum wallet as a virtual filesystem",
    long_about = "Bloom mounts an agentic Ethereum wallet as a directory for agents. EXPERIMENTAL / UNAUDITED ALPHA: do not use with funds you cannot afford to lose, and review every generated transaction plan before signing. Read balances, contracts, ENS, prices, and status with cat/ls; stage wallet actions by writing intents into an outbox; confirm only after reviewing the generated plan. New agents should read https://bloom.directory/SKILL.md, then run bloom init and bloom serve --mount ~/bloom. Use bloom vfs only as a fallback when mounting is unavailable."
)]
struct Cli {
    /// Override home directory (default: ~/.bloom).
    #[arg(long, env = "BLOOM_HOME")]
    home: Option<PathBuf>,

    /// Connect to an explicit Bloom IPC endpoint.
    ///
    /// Currently only Unix socket endpoints are supported:
    /// `unix:/path/to/bloom.sock`. A bare path is accepted as a
    /// compatibility shorthand.
    #[arg(long, value_name = "ENDPOINT")]
    connect: Option<String>,

    /// Compatibility alias for `--connect unix:<path>`.
    #[arg(long, value_name = "PATH")]
    ipc_socket: Option<PathBuf>,

    /// Suppress daemon/diagnostic logs on stderr (values still print on
    /// stdout). `RUST_LOG` overrides this when set.
    #[arg(long, short, global = true)]
    quiet: bool,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Show daemon status (chains configured, version, uptime).
    Status,
    /// VFS path operations (no NFS mount required).
    #[command(subcommand)]
    Vfs(VfsCmd),
    /// Wallet management.
    #[command(subcommand)]
    Wallet(WalletCmd),
    /// Inspect or cancel a Broker-owned custody ceremony by operation ID.
    #[command(subcommand)]
    Ceremony(CeremonyCmd),
    /// Paid/free HTTP requests via the `/requests` VFS surface.
    #[command(subcommand)]
    Request(RequestCmd),
    /// Run the daemon as a long-lived process.
    Serve {
        /// IPC endpoint to bind.
        ///
        /// Currently only Unix socket endpoints are supported:
        /// `unix:/path/to/bloom.sock`. A bare path is accepted as a
        /// compatibility shorthand.
        #[arg(long, value_name = "ENDPOINT")]
        endpoint: Option<String>,

        /// Mount the VFS for the lifetime of the daemon.
        ///
        /// With no PATH, defaults to /bloom on Linux and /Volumes/bloom on macOS.
        #[arg(
            long,
            value_name = "PATH",
            num_args = 0..=1,
            default_missing_value = DEFAULT_MOUNT_PATH
        )]
        mount: Option<PathBuf>,
    },
    /// Talk to a running `bloom serve` over its UDS JSON-RPC socket.
    #[command(subcommand)]
    Ipc(IpcCmd),
    /// Manage wasm petals: install, app, list, uninstall.
    #[command(subcommand, visible_alias = "petal")]
    Petals(PetalsCmd),
    /// Hyperliquid HyperCore reads and tightly scoped test actions.
    #[command(subcommand)]
    Hyperliquid(HyperliquidCmd),
    /// Check for newer bloom releases on GitHub and inspect the
    /// current update-checker state.
    #[command(subcommand)]
    Update(UpdateCmd),
    /// Initialise ~/.bloom with default config + dirs.
    Init,

    /// Print a shell completion script.
    Completions { shell: Shell },
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
enum CeremonyCmd {
    /// Refresh and print the durable Machine projection from Broker status.
    Status { operation_id: String },
    /// Cancel a ceremony before its atomic commit marker.
    Cancel { operation_id: String },
    /// Retrieve the signed public custody result. Encrypted Browser output is
    /// never printed by Machine.
    Result { operation_id: String },
}

#[derive(Subcommand, Debug)]
enum VfsCmd {
    /// `cat /bloom/<path>` — read a file via the VFS.
    Cat { path: String },
    /// `ls /bloom/<path>` — list a directory via the VFS.
    Ls {
        #[arg(default_value = "/")]
        path: String,
    },
    /// `stat /bloom/<path>` — inspect VFS metadata without a kernel mount.
    Stat { path: String },
    /// Write data to a writable VFS path. Reads from stdin if `--data` is omitted.
    Write {
        path: String,
        #[arg(long)]
        data: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
enum PetalsCmd {
    /// Install a Petal package directory, `.petal.tar`, or trusted GitHub source repository.
    Install {
        /// Path to a package directory, `.petal.tar`, or trusted GitHub source repository URL.
        path: String,
        /// Git tag, branch, or commit SHA to install from a GitHub source repository.
        #[arg(long = "ref", value_name = "TAG_OR_SHA")]
        ref_: Option<String>,
    },
    /// Validate a Petal package directory and optionally emit a deterministic `.petal.tar`.
    Build {
        /// Package directory containing petal.toml, README.md, AGENTS.md, and petal/<name>/.
        package_dir: String,
        /// Write a deterministic `.petal.tar` archive.
        #[arg(long, value_name = "ARCHIVE")]
        out: Option<String>,
    },
    /// List installed petals.
    Ls,
    /// Remove an installed petal (and any petname pointing at it).
    Uninstall {
        /// Content hash of the petal to remove: full 64-char hex, a
        /// unique prefix of at least 12 chars (as printed by `ls`),
        /// a Petal name, or a petname.
        target: String,
    },
}

#[derive(Subcommand, Debug)]
enum PetalAppCmd {
    /// Validate a v2 package directory and optionally emit a deterministic `.petal.tar`.
    Build {
        /// Package directory containing petal.toml, README.md, AGENTS.md, and app/<name>/.
        package_dir: String,
        /// Write a deterministic `.petal.tar` archive.
        #[arg(long, value_name = "ARCHIVE")]
        out: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
enum RequestCmd {
    /// Create a request from one-line, TOML, or HTTP-message-like input.
    New {
        /// Request text, e.g. `GET https://example.com/data`.
        request: String,
        /// Paying wallet. If omitted, config.default_wallet or the only wallet is used.
        #[arg(long)]
        wallet: Option<String>,
        /// Stage/probe only; never spends or signs.
        #[arg(long)]
        dry_run: bool,
    },
    /// Show the staged payment plan for an id or `latest`.
    Plan { id: String },
    /// Confirm a pending paid request.
    Confirm {
        id: String,
        /// Confirmation text: `y`/`yes`/`confirm`, or the wallet's policy override
        /// sentinel to bypass soft limits. Defaults to `confirm`.
        #[arg(long, default_value = "confirm")]
        text: String,
    },
    /// Print response body for an id or `latest`.
    Body { id: String },
    /// Print receipt JSON for an id or `latest`.
    Receipt { id: String },
}

/// Subcommands for `bloom update`.
#[derive(Subcommand, Debug)]
enum UpdateCmd {
    /// Force a refresh against GitHub and print the result as JSON.
    /// Exits 0 if up to date, 1 if behind, 2 if unknown/error.
    Check,
    /// Print the cached snapshot without making a network call.
    Status,
}

#[derive(Subcommand, Debug)]
enum HyperliquidCmd {
    /// Print account clearinghouse state.
    Account {
        user: String,
        #[arg(long, default_value = "mainnet")]
        network: String,
    },
    /// Print spot/unified clearinghouse state.
    SpotState {
        user: String,
        #[arg(long, default_value = "mainnet")]
        network: String,
    },
    /// Print open orders.
    OpenOrders {
        user: String,
        #[arg(long, default_value = "mainnet")]
        network: String,
    },
    /// Print user fills.
    Fills {
        user: String,
        #[arg(long, default_value = "mainnet")]
        network: String,
    },
    /// Print user funding history for a coin.
    Funding {
        user: String,
        coin: String,
        #[arg(long)]
        start_time: Option<u64>,
        #[arg(long)]
        end_time: Option<u64>,
        #[arg(long, default_value = "mainnet")]
        network: String,
    },
    /// Print an L2 order book snapshot.
    Book {
        coin: String,
        #[arg(long, default_value = "mainnet")]
        network: String,
    },
    /// Print candle snapshots for a time range.
    Candles {
        coin: String,
        interval: String,
        start_time: u64,
        end_time: u64,
        #[arg(long, default_value = "mainnet")]
        network: String,
    },
    /// Print market metadata.
    Metadata {
        #[arg(long, default_value = "perp")]
        kind: String,
        #[arg(long, default_value = "mainnet")]
        network: String,
    },
    /// Manage daemon-held ephemeral API-wallet sessions.
    Session {
        #[command(subcommand)]
        command: HyperliquidSessionCmd,
    },
    /// Run the read-only smoke suite for an account.
    TestReads {
        user: String,
        #[arg(long, default_value = "BTC")]
        coin: String,
        #[arg(long, default_value = "mainnet")]
        network: String,
    },
    /// Transfer USDC internally between Hyperliquid accounts (usdSend, Sealed Approval).
    /// Requires transfer_cap_usd in the wallet [hyperliquid] policy.
    SendAsset {
        wallet: String,
        destination: String,
        amount: String,
        #[arg(long, default_value = "mainnet")]
        network: String,
    },
    /// Place a far-away post-only perp order, then cancel it if it rests.
    TestPostOnlyCancel {
        wallet: String,
        #[arg(long, default_value = "BTC")]
        coin: String,
        /// Perp asset id. BTC is normally 0 on mainnet.
        #[arg(long, default_value_t = 0)]
        asset: u32,
        /// Explicit limit price. Defaults to roughly 50% of current mid.
        #[arg(long)]
        price: Option<String>,
        /// Explicit size. Defaults to a size whose limit notional is just above $10.
        #[arg(long)]
        size: Option<String>,
        /// Refuse if price * size is above this USD cap.
        #[arg(long, default_value_t = 15.0)]
        max_notional_usd: f64,
        /// Required acknowledgement for a live-order test command.
        #[arg(long)]
        danger_accept_live_orders: bool,
        #[arg(long, default_value = "mainnet")]
        network: String,
    },
}

#[derive(Subcommand, Debug)]
enum HyperliquidSessionCmd {
    /// Create an approved ephemeral API-wallet session in the running daemon.
    Create {
        wallet: String,
        #[arg(long)]
        id: Option<String>,
        #[arg(long)]
        agent_name: Option<String>,
        /// Vault/subaccount address this session trades on. When set, risk
        /// monitoring and cleanup target this account and every submit must
        /// carry a matching vaultAddress.
        #[arg(long)]
        vault_address: Option<String>,
        #[arg(long, default_value = "mainnet")]
        network: String,
    },
    /// Print session status.
    Status {
        wallet: String,
        id: String,
        #[arg(long, default_value = "mainnet")]
        network: String,
    },
    /// Print session audit records.
    Audit {
        wallet: String,
        id: String,
        #[arg(long, default_value = "mainnet")]
        network: String,
    },
    /// Stop a session without submitting cleanup orders.
    Stop {
        wallet: String,
        id: String,
        #[arg(long, default_value = "mainnet")]
        network: String,
    },
    /// Cancel all open orders for the session account.
    CancelAll {
        wallet: String,
        id: String,
        #[arg(long, default_value = "mainnet")]
        network: String,
    },
    /// Cancel orders and submit reduce-only IOC closes for open positions.
    CloseAll {
        wallet: String,
        id: String,
        #[arg(long, default_value = "mainnet")]
        network: String,
    },
}

#[derive(Subcommand, Debug)]
enum WalletCmd {
    /// Start a Broker-hosted wallet registration ceremony.
    New { name: String },
    /// Start a Broker-hosted wallet import ceremony. The private key is entered
    /// only in the ceremony browser and never crosses the Machine process.
    Import { name: String },
    /// List configured wallets.
    List,
    /// Print a table of all wallets with their total portfolio value across
    /// all connected chains. Queries Hyperliquid clearinghouse state for each
    /// wallet. Use `--network` to select mainnet (default) or testnet.
    Portfolio {
        /// Hyperliquid network to query. Defaults to mainnet.
        #[arg(long, default_value = "mainnet")]
        network: String,
    },
    /// Print a wallet's deposit address. Default output is the bare checksummed
    /// address (one line, scriptable); `--qr` adds a scannable QR block above it,
    /// and `--qr-out <path>` writes a scannable SVG of the address to a file.
    Address {
        name: String,
        #[arg(long)]
        qr: bool,
        /// Write a scannable SVG QR of the deposit address to this path.
        #[arg(long, value_name = "PATH")]
        qr_out: Option<PathBuf>,
    },
    /// Request wallet re-arming. This is currently fail-closed because the
    /// normative ceremony-kind inventory has no wallet-unlock kind.
    Unlock { name: String },
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
    /// Submit confirmation of a staged transaction through the Machine VFS.
    Confirm {
        wallet: String,
        chain: String,
        id: String,
        /// Confirmation text (default "y"; "override" bypasses soft
        /// policy warnings).
        #[arg(long, default_value = "y")]
        text: String,
    },
    /// Submit a same-nonce self-send replacement request for a staged tx.
    Cancel {
        wallet: String,
        chain: String,
        id: String,
        /// Confirmation text. Must be non-empty.
        #[arg(long, default_value = "y")]
        text: String,
    },
    /// Submit a same-nonce replacement request from a new intent body.
    Replace {
        wallet: String,
        chain: String,
        id: String,
        /// Replacement intent body (JSON, TOML, or shell-style). If omitted, read stdin.
        #[arg(long)]
        intent: Option<String>,
    },
    /// Submit an atomic batch of staged transactions.
    ///
    /// Each TX is `chain:id`, for example `base:0001-abc`.
    ConfirmBatch {
        wallet: String,
        /// Staged tx references in the exact order to broadcast.
        txs: Vec<String>,
        /// Confirmation text for each tx.
        #[arg(long, default_value = "override")]
        text: String,
    },
    /// Validate a proposed policy and prepare its Broker-originated review
    /// ceremony. The input is JSON and is canonicalized before submission.
    UpdatePolicy {
        name: String,
        #[arg(long)]
        file: PathBuf,
        #[arg(long, default_value = "user_verified")]
        assurance_level: String,
    },
    /// Commit a policy update using its completed policy_update ceremony
    /// receipt. This never provides a direct commit path.
    CommitPolicy { operation_id: String },
    /// Re-bind an existing PRF-based passkey wallet to a new passkey
    /// credential. Unlocks with the current credential first to prove
    /// ownership, then runs a fresh WebAuthn registration ceremony and
    /// re-encrypts the private key under the new PRF output. The wallet
    /// address does not change.
    ///
    /// Use this to rotate authenticators (e.g. new YubiKey or new device)
    /// without moving funds. A recovery key is printed once after rebind.
    RebindPasskey { name: String },
    /// Permanently delete a wallet. All wallet files are removed from disk.
    /// This cannot be undone — make sure you have the recovery key or the
    /// private key stored elsewhere before deleting a passkey wallet.
    Delete { name: String },
}

struct WalletPortfolioRow {
    name: String,
    address: String,
    account_value: f64,
    withdrawable: f64,
    positions: Vec<String>,
}

fn print_portfolio_table(rows: &[WalletPortfolioRow], network: &str) {
    if rows.is_empty() {
        println!("no wallets found");
        return;
    }
    println!("\n  Bloom Wallet Portfolio — Hyperliquid {network}\n");
    println!(
        "  {:<18} {:<44} {:>12} {:>12} POSITIONS",
        "WALLET", "ADDRESS", "ACCT VALUE", "WITHDRAWABLE"
    );
    println!("  {}", "-".repeat(120));
    for row in rows {
        let pos_str = if row.positions.is_empty() {
            "—".to_string()
        } else {
            row.positions.join(", ")
        };
        println!(
            "  {:<18} {:<44} ${:>11} ${:>11} {}",
            row.name.chars().take(18).collect::<String>(),
            &row.address[..row.address.len().min(44)],
            format!("{:.4}", row.account_value),
            format!("{:.4}", row.withdrawable),
            pos_str
        );
    }
    println!("  {}", "-".repeat(120));
    let total_value: f64 = rows.iter().map(|r| r.account_value).sum();
    let total_wd: f64 = rows.iter().map(|r| r.withdrawable).sum();
    let total_pos: usize = rows.iter().map(|r| r.positions.len()).sum();
    println!(
        "  {:<18} {:<44} ${:>11} ${:>11} {} position(s)\n",
        "TOTAL",
        "",
        format!("{:.4}", total_value),
        format!("{:.4}", total_wd),
        total_pos
    );
}

#[tokio::main]
async fn main() -> ExitCode {
    if std::env::args_os().len() == 9
        && std::env::args_os().nth(1).as_deref()
            == Some(std::ffi::OsStr::new("--triad-render-macos-enrollment"))
    {
        return match triad_enrollment::run_from_process_args() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("Bloom macOS enrollment generation failed: {error:#}");
                ExitCode::FAILURE
            }
        };
    }
    if std::env::args_os().len() == 3
        && std::env::args_os().nth(1).as_deref()
            == Some(std::ffi::OsStr::new("--triad-health-check"))
    {
        let expected_build = match std::env::args_os().nth(2) {
            Some(value) => match value.into_string() {
                Ok(value) => value,
                Err(_) => {
                    eprintln!("Bloom triad health check failed: build digest is not UTF-8");
                    return ExitCode::FAILURE;
                }
            },
            None => return ExitCode::FAILURE,
        };
        return match installed_triad_health_check(&expected_build).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("Bloom triad health check failed: {error:#}");
                ExitCode::FAILURE
            }
        };
    }
    if std::env::args_os().len() == 2
        && std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("--session-sentinel"))
    {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
            )
            .with_target(false)
            .with_writer(std::io::stderr)
            .try_init();
        return match session_sentinel::run().await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("Bloom session sentinel failed: {error:#}");
                ExitCode::FAILURE
            }
        };
    }
    let cli = Cli::parse();

    // RUST_LOG wins when set; otherwise default to `info`, or `error`
    // under `--quiet` so `vfs cat`/`ls` output stays clean.
    let default_level = if cli.quiet { "error" } else { "info" };
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level)),
        )
        .with_target(false)
        .with_writer(std::io::stderr)
        .try_init();

    match run(cli).await {
        Ok(()) => {
            let code = UPDATE_EXIT_CODE.load(std::sync::atomic::Ordering::SeqCst);
            if code != 0 {
                return ExitCode::from(code as u8);
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {:#}", e);
            ExitCode::FAILURE
        }
    }
}

fn reject_archive_output_inside_package(package_dir: &str, out: &str) -> Result<()> {
    let package_dir = std::fs::canonicalize(package_dir)
        .with_context(|| format!("canonicalize package dir {package_dir}"))?;
    let out_path = std::path::Path::new(out);
    let out_parent = out_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    let out_parent = std::fs::canonicalize(out_parent)
        .with_context(|| format!("canonicalize archive parent {out}"))?;
    let out_abs = out_parent.join(out_path.file_name().unwrap_or_default());
    if out_abs.starts_with(&package_dir) {
        bail!(
            "--out must be outside the package directory so archives are not packaged into future builds"
        );
    }
    Ok(())
}

/// Returns `None` when no daemon socket is present (daemon not started),
/// propagating all other errors normally. A stale socket (file exists but
/// connection refused) is removed and surfaced as an error rather than
/// silently falling back to in-process — a stale socket almost always
/// means the daemon crashed and the caller should restart it explicitly.
async fn try_ipc(
    client: &IpcClient,
    endpoint: &ResolvedEndpoint,
    method: &str,
    params: serde_json::Value,
) -> std::io::Result<Option<serde_json::Value>> {
    match client.call(method, params).await {
        Ok(v) => Ok(Some(v)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if endpoint.is_explicit() {
                return Err(std::io::Error::new(
                    e.kind(),
                    format!(
                        "explicit Bloom endpoint {} is not available: {e}",
                        endpoint.display
                    ),
                ));
            }
            debug!(error = %e, "ipc.no_daemon_fallback");
            Ok(None)
        }
        Err(e) if is_endpoint_permission_denial(&e) => {
            if endpoint.is_explicit() {
                return Err(std::io::Error::new(
                    e.kind(),
                    format!("explicit Bloom endpoint {} failed: {e}", endpoint.display),
                ));
            }
            debug!(endpoint = %endpoint.display, error = %e, "ipc.permission_fallback");
            Ok(None)
        }
        Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
            if endpoint.is_explicit() {
                return Err(std::io::Error::new(
                    e.kind(),
                    format!(
                        "explicit Bloom endpoint {} is not responding: {e}",
                        endpoint.display
                    ),
                ));
            }
            // Only remove if it is actually a socket, not a regular
            // file or symlink placed by another process.
            #[cfg(unix)]
            {
                use std::os::unix::fs::FileTypeExt;
                let removed = std::fs::symlink_metadata(client.socket())
                    .is_ok_and(|m| m.file_type().is_socket())
                    && std::fs::remove_file(client.socket()).is_ok();
                let detail = if removed {
                    "stale socket removed"
                } else {
                    "socket not responding"
                };
                Err(std::io::Error::other(format!(
                    "daemon socket exists but is not responding ({detail}); \
                     start the daemon with 'bloom serve'",
                )))
            }
            #[cfg(not(unix))]
            {
                let _ = std::fs::remove_file(client.socket());
                return Err(std::io::Error::other(
                    "daemon socket exists but is not responding (stale socket removed); \
                     start the daemon with 'bloom serve'",
                ));
            }
        }
        Err(e) => Err(e),
    }
}

fn is_endpoint_permission_denial(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::PermissionDenied || e.raw_os_error() == Some(1)
}

fn system_time_to_unix_ms(time: SystemTime) -> u128 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn unix_ms_to_system_time(ms: u128) -> SystemTime {
    let ms = ms.min(u64::MAX as u128) as u64;
    UNIX_EPOCH + std::time::Duration::from_millis(ms)
}

fn entry_kind_label(kind: EntryKind) -> &'static str {
    match kind {
        EntryKind::Dir => "dir",
        EntryKind::File => "file",
        EntryKind::Symlink => "symlink",
    }
}

fn print_vfs_stat(
    path: &str,
    name: &str,
    kind: &str,
    mode: u32,
    size: u64,
    link_target: Option<&str>,
    modified: Option<SystemTime>,
) {
    let (modified, modified_source) = match modified {
        Some(t) => (t, "artifact"),
        None => (SystemTime::now(), "synthetic_now"),
    };
    let modified_ms = system_time_to_unix_ms(modified);
    println!("path: {path}");
    println!("name: {name}");
    println!("kind: {kind}");
    println!("mode: {:04o}", mode & 0o7777);
    println!("size: {size}");
    println!("modified_ms: {modified_ms}");
    println!("modified: {}", humantime::format_rfc3339(modified));
    println!("modified_source: {modified_source}");
    if let Some(target) = link_target {
        println!("link_target: {target}");
    }
}

fn print_vfs_stat_entry(path: &str, entry: &Entry) {
    print_vfs_stat(
        path,
        &entry.name,
        entry_kind_label(entry.kind),
        entry.mode,
        entry.size,
        entry.link_target.as_deref(),
        entry.modified,
    )
}

fn print_vfs_stat_json(path: &str, entry: &serde_json::Value) -> Result<()> {
    let name = entry
        .get("name")
        .and_then(|v| v.as_str())
        .context("ipc lookup: missing name")?;
    let kind = entry
        .get("kind")
        .and_then(|v| v.as_str())
        .context("ipc lookup: missing kind")?;
    let mode = entry
        .get("mode")
        .and_then(|v| v.as_u64())
        .context("ipc lookup: missing mode")? as u32;
    let size = entry
        .get("size")
        .and_then(|v| v.as_u64())
        .context("ipc lookup: missing size")?;
    let link_target = entry.get("link_target").and_then(|v| v.as_str());
    let modified = entry
        .get("modified_ms")
        .and_then(|v| v.as_u64())
        .map(|ms| unix_ms_to_system_time(ms as u128));
    print_vfs_stat(path, name, kind, mode, size, link_target, modified);
    Ok(())
}

async fn run(cli: Cli) -> Result<()> {
    let (connect, ipc_socket) = if cli.connect.is_some() {
        (cli.connect, None)
    } else if cli.ipc_socket.is_some() {
        (None, cli.ipc_socket)
    } else if let Ok(endpoint) = std::env::var("BLOOM_RPC_ENDPOINT") {
        (Some(endpoint), None)
    } else {
        (
            None,
            std::env::var_os("BLOOM_IPC_SOCKET").map(PathBuf::from),
        )
    };
    let home = match cli.home {
        Some(p) => {
            debug!(path = %p.display(), "cli.home.override");
            HomeDir::at(p)
        }
        None => HomeDir::resolve("~/.bloom").context("resolving home dir")?,
    };
    let client_endpoint = resolve_client_endpoint(&home, connect.as_deref(), ipc_socket.as_deref())
        .context("resolve Bloom endpoint")?;
    trace!(cmd = ?cli.cmd, home = %home.root().display(), "cli.dispatch");

    match cli.cmd {
        Cmd::Init => {
            eprintln!("{ALPHA_DISCLOSURE}");
            let (_home_permit, d) = build_write_daemon(home.clone()).context("init daemon")?;
            let preinstalled = github_source::ensure_preinstalled_petals(&home, &d)
                .context("provision configured pre-installed Petals")?;
            println!("home: {}", d.home.root().display());
            println!("config: {}", d.home.config_path().display());
            println!("chains: {:?}", d.chains.list_names());
            println!("preinstalled_petals: {preinstalled:?}");
            println!("next: bloom wallet new main");
            println!("then: bloom wallet address main --qr");
            println!("mount: mkdir -p ~/bloom && bloom serve --mount ~/bloom");
            println!("fallback: bloom vfs cat /docs/README.md");
            println!("agent setup: https://bloom.directory/SKILL.md");
            Ok(())
        }
        Cmd::Status => {
            let d = Daemon::from_home(home).context("build daemon")?;
            println!("version: {}", env!("CARGO_PKG_VERSION"));
            println!("home: {}", d.home.root().display());
            println!("chains: {:?}", d.chains.list_names());
            println!("default_chain: {}", d.config.default_chain);
            println!(
                "default_wallet: {}",
                d.config.default_wallet.as_deref().unwrap_or("<none>")
            );
            if d.config.hyperliquid.is_some() {
                println!("hyperliquid_vfs: enabled (/hyperliquid)");
                // Which wallets have an actual trading boundary in force.
                let policed: Vec<String> = d
                    .keystore
                    .list()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|w| w.policy.hyperliquid.is_configured())
                    .map(|w| w.name)
                    .collect();
                if policed.is_empty() {
                    println!(
                        "hyperliquid_policy: none configured (any wallet can trade unconstrained \
                         once unlocked — add [hyperliquid] to a wallet policy)"
                    );
                } else {
                    println!("hyperliquid_policy: configured for {}", policed.join(", "));
                }
            } else {
                println!("hyperliquid_vfs: disabled (add [hyperliquid] to config.toml)");
            }
            println!("try: bloom vfs ls /");
            if d.keystore.list()?.is_empty() {
                println!("no wallets yet — create one with bloom wallet new main");
            } else {
                println!("deposit: bloom wallet address <wallet> --qr");
                println!("agent workflow: browse the mounted VFS or use bloom vfs cat/ls/write");
            }
            if let Some(snap) = d.update_checker.quick_check_cached() {
                let latest = snap.latest.as_deref().unwrap_or("?");
                let latest_display = latest.strip_prefix('v').unwrap_or(latest);
                let available = match snap.available() {
                    bloom_update::UpdateAvailable::OutOfDate => "out_of_date",
                    bloom_update::UpdateAvailable::UpToDate => "up_to_date",
                    bloom_update::UpdateAvailable::Unknown => "unknown",
                };
                println!("latest_release: {}", latest);
                println!("update_available: {}", available);
                if matches!(snap.available(), bloom_update::UpdateAvailable::OutOfDate) {
                    eprintln!(
                        "hint: bloom v{} is available (you have v{}); see /status/update",
                        latest_display,
                        env!("CARGO_PKG_VERSION")
                    );
                }
            }
            Ok(())
        }
        Cmd::Vfs(VfsCmd::Cat { path }) => {
            let p = VfsPath::parse(&path).context("parse path")?;
            let client = IpcClient::new(&client_endpoint.socket);
            let ipc_res = try_ipc(
                &client,
                &client_endpoint,
                "read",
                serde_json::json!({ "path": path }),
            )
            .await
            .with_context(|| format!("ipc read via {}", client_endpoint.display))?;
            let bytes = if let Some(res) = ipc_res {
                debug!(endpoint = %client_endpoint.display, "cli.vfs.cat.via_ipc");
                let b64 = res
                    .get("bytes_b64")
                    .and_then(|v| v.as_str())
                    .context("ipc read: missing bytes_b64")?;
                B64.decode(b64).context("ipc read: bad base64")?
            } else {
                debug!("cli.vfs.cat.via_inproc: no daemon socket present");
                let d = Daemon::from_home(home).context("build daemon")?;
                d.vfs.read(&p).await.context("vfs read")?
            };
            std::io::Write::write_all(&mut std::io::stdout(), &bytes)?;
            Ok(())
        }
        Cmd::Vfs(VfsCmd::Ls { path }) => {
            let p = VfsPath::parse(&path).context("parse path")?;
            let client = IpcClient::new(&client_endpoint.socket);
            let ipc_res = try_ipc(
                &client,
                &client_endpoint,
                "list",
                serde_json::json!({ "path": path }),
            )
            .await
            .with_context(|| format!("ipc list via {}", client_endpoint.display))?;
            if let Some(res) = ipc_res {
                debug!(endpoint = %client_endpoint.display, "cli.vfs.ls.via_ipc");
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
                debug!("cli.vfs.ls.via_inproc: no daemon socket present");
                let d = Daemon::from_home(home).context("build daemon")?;
                let entries = d.vfs.list(&p).await.context("vfs list")?;
                for e in entries {
                    println!("{}\t{:?}", e.name, e.kind);
                }
            }
            Ok(())
        }
        Cmd::Vfs(VfsCmd::Stat { path }) => {
            let p = VfsPath::parse(&path).context("parse path")?;
            let client = IpcClient::new(&client_endpoint.socket);
            let ipc_res = try_ipc(
                &client,
                &client_endpoint,
                "lookup",
                serde_json::json!({ "path": path }),
            )
            .await
            .with_context(|| format!("ipc lookup via {}", client_endpoint.display))?;
            if let Some(res) = ipc_res {
                debug!(endpoint = %client_endpoint.display, "cli.vfs.stat.via_ipc");
                print_vfs_stat_json(&path, &res)?;
            } else {
                debug!("cli.vfs.stat.via_inproc: no daemon socket present");
                let d = Daemon::from_home(home).context("build daemon")?;
                let entry = d.vfs.lookup(&p).await.context("vfs lookup")?;
                print_vfs_stat_entry(&path, &entry);
            }
            Ok(())
        }
        Cmd::Vfs(VfsCmd::Write { path, data }) => {
            let p = VfsPath::parse(&path).context("parse path")?;
            let body = match data {
                Some(s) => s.into_bytes(),
                None => {
                    let mut buf = Vec::new();
                    std::io::Read::read_to_end(&mut std::io::stdin(), &mut buf)?;
                    buf
                }
            };
            let client = IpcClient::new(&client_endpoint.socket);
            let ipc_res = try_ipc(
                &client,
                &client_endpoint,
                "write",
                serde_json::json!({ "path": path, "bytes_b64": B64.encode(&body) }),
            )
            .await
            .with_context(|| format!("ipc write via {}", client_endpoint.display))?;
            if ipc_res.is_some() {
                debug!(endpoint = %client_endpoint.display, "cli.vfs.write.via_ipc");
            } else {
                debug!("cli.vfs.write.via_inproc: no daemon socket present");
                let (_home_permit, d) = build_write_daemon(home)?;
                d.vfs.write(&p, &body).await.context("vfs write")?;
            }
            Ok(())
        }
        Cmd::Request(RequestCmd::New {
            request,
            wallet,
            dry_run,
        }) => {
            let body = request_body_with_wallet(request, wallet.as_deref());
            let path = if dry_run {
                "/requests/new.dry-run"
            } else {
                "/requests/new"
            };
            let client = IpcClient::new(&client_endpoint.socket);
            let ipc_res = try_ipc(
                &client,
                &client_endpoint,
                "write",
                serde_json::json!({ "path": path, "bytes_b64": B64.encode(body.as_bytes()) }),
            )
            .await
            .with_context(|| format!("ipc request new via {}", client_endpoint.display))?;
            if ipc_res.is_some() {
                debug!(endpoint = %client_endpoint.display, "cli.request.new.via_ipc");
                if dry_run {
                    println!("dry_run: true (unpaid probe/staging only; no spend/signing)");
                }
                return Ok(());
            }
            debug!("cli.request.new.via_inproc: no daemon socket present");
            let (_home_permit, d) = build_write_daemon(home)?;
            d.vfs
                .write(&VfsPath::parse(path)?, body.as_bytes())
                .await
                .context("request new")?;
            let latest = d
                .vfs
                .read(&VfsPath::parse("/requests/latest")?)
                .await
                .context("read latest request")?;
            let latest = String::from_utf8_lossy(&latest);
            println!("request: {}", latest.trim());
            if dry_run {
                println!("dry_run: true (unpaid probe/staging only; no spend/signing)");
            }
            Ok(())
        }
        Cmd::Request(RequestCmd::Plan { id }) => {
            let d = Daemon::from_home(home).context("build daemon")?;
            let path = VfsPath::parse(&format!("/requests/{id}/plan.md"))?;
            let bytes = d.vfs.read(&path).await.context("request plan")?;
            std::io::Write::write_all(&mut std::io::stdout(), &bytes)?;
            Ok(())
        }
        Cmd::Request(RequestCmd::Confirm { id, text }) => {
            let path = format!("/requests/{id}/confirm");
            let p = VfsPath::parse(&path)?;
            let body = text.into_bytes();
            let client = IpcClient::new(&client_endpoint.socket);
            // Confirmation uses the ordinary Machine VFS lane. Any signing
            // requirement must be satisfied through Broker; the CLI never
            // accepts an unlock secret or hosts a ceremony.
            let confirm_params = serde_json::json!({
                "path": path,
                "bytes_b64": B64.encode(&body),
            });
            match try_ipc(&client, &client_endpoint, "write", confirm_params.clone()).await {
                Ok(Some(_)) => {
                    debug!(endpoint = %client_endpoint.display, "cli.request.confirm.via_ipc");
                    return Ok(());
                }
                Ok(None) => {
                    debug!("cli.request.confirm.via_inproc: no daemon socket present");
                    // Fall through to the in-process fallback below.
                }
                Err(e) => {
                    return Err(anyhow::Error::new(e)).with_context(|| {
                        format!("ipc request confirm via {}", client_endpoint.display)
                    });
                }
            }
            let (_home_permit, d) = build_write_daemon(home)?;
            d.vfs.write(&p, &body).await.context("request confirm")?;
            Ok(())
        }
        Cmd::Request(RequestCmd::Body { id }) => {
            let d = Daemon::from_home(home).context("build daemon")?;
            let path = VfsPath::parse(&format!("/requests/{id}/response/body"))?;
            let bytes = d.vfs.read(&path).await.context("request body")?;
            std::io::Write::write_all(&mut std::io::stdout(), &bytes)?;
            Ok(())
        }
        Cmd::Request(RequestCmd::Receipt { id }) => {
            let d = Daemon::from_home(home).context("build daemon")?;
            let path = VfsPath::parse(&format!("/requests/{id}/receipt.json"))?;
            let bytes = d.vfs.read(&path).await.context("request receipt")?;
            std::io::Write::write_all(&mut std::io::stdout(), &bytes)?;
            Ok(())
        }
        Cmd::Wallet(WalletCmd::New { name }) => {
            launch_custody_ceremony(
                &home,
                &name,
                bloom_machine_client::CustodyPrepareMethod::WalletRegistration,
                bloom_triad_protocol::CeremonyKind::WalletRegistration,
                None,
                "passkey-prf",
            )
            .await
        }
        Cmd::Wallet(WalletCmd::Import { name }) => {
            launch_custody_ceremony(
                &home,
                &name,
                bloom_machine_client::CustodyPrepareMethod::WalletImport,
                bloom_triad_protocol::CeremonyKind::WalletImport,
                None,
                "raw-wallet-import",
            )
            .await
        }
        Cmd::Wallet(WalletCmd::List) => {
            let d = Daemon::from_home(home).context("build daemon")?;
            for info in d.keystore.list()? {
                let kind = match info.kind {
                    bloom_keystore::WalletKind::Local => "local",
                    bloom_keystore::WalletKind::Watch => "watch",
                    bloom_keystore::WalletKind::PasskeyGated => "passkey",
                };
                println!("{}\t{}\t{}", info.name, info.address, kind);
            }
            Ok(())
        }
        Cmd::Wallet(WalletCmd::Portfolio { network }) => {
            let d = Daemon::from_home(home).context("build daemon")?;
            let client = hl_client(&d.home, &network)?;
            let wallets = d.keystore.list()?;
            let mut set = tokio::task::JoinSet::new();
            for (idx, info) in wallets.into_iter().enumerate() {
                let client = client.clone();
                set.spawn(async move {
                    let address = format!("{:?}", info.address).to_ascii_lowercase();
                    let res = client
                        .info(serde_json::json!({
                            "type": "clearinghouseState",
                            "user": address,
                        }))
                        .await;
                    (idx, info, address, res)
                });
            }
            let mut rows: Vec<(usize, WalletPortfolioRow)> = Vec::new();
            while let Some(joined) = set.join_next().await {
                let (idx, info, address, ch_result) = joined?;
                let (account_value, withdrawable, positions) = match ch_result {
                    Ok(v) => {
                        let av = v
                            .get("marginSummary")
                            .and_then(|m| m.get("accountValue"))
                            .and_then(|a| a.as_str())
                            .and_then(|s| s.parse::<f64>().ok())
                            .unwrap_or(0.0);
                        let wd = v
                            .get("withdrawable")
                            .and_then(|w| w.as_str())
                            .and_then(|s| s.parse::<f64>().ok())
                            .unwrap_or(0.0);
                        let positions: Vec<String> = v
                            .get("assetPositions")
                            .and_then(|a| a.as_array())
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|ap| {
                                        ap.get("position")
                                            .and_then(|p| p.get("coin"))
                                            .and_then(|c| c.as_str())
                                            .map(|s| s.to_string())
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        (av, wd, positions)
                    }
                    Err(_) => (0.0, 0.0, Vec::new()),
                };
                rows.push((
                    idx,
                    WalletPortfolioRow {
                        name: info.name,
                        address,
                        account_value,
                        withdrawable,
                        positions,
                    },
                ));
            }
            rows.sort_by_key(|(i, _)| *i);
            let table: Vec<WalletPortfolioRow> = rows.into_iter().map(|(_, r)| r).collect();
            print_portfolio_table(&table, &network);
            Ok(())
        }
        Cmd::Wallet(WalletCmd::Address { name, qr, qr_out }) => {
            let d = Daemon::from_home(home).context("build daemon")?;
            // Read-only: an unsigned/stale passkey policy must not block this.
            let info = d.keystore.info_unverified(&name)?;
            let address = bloom_proto::checksum_address(&info.address);
            if let Some(path) = qr_out {
                match commands::qr::render_qr_svg(&address) {
                    Some(svg) => {
                        std::fs::write(&path, svg)
                            .with_context(|| format!("write QR SVG to {}", path.display()))?;
                        eprintln!("wrote deposit QR SVG: {}", path.display());
                    }
                    None => anyhow::bail!("address too large to encode as a QR code"),
                }
            }
            if qr && let Some(code) = commands::qr::render_qr(&address) {
                println!("{code}");
            }
            println!("{address}");
            Ok(())
        }
        Cmd::Wallet(WalletCmd::Unlock { name }) => {
            bail!(
                "wallet unlock for '{name}' is fail-closed: §17.1 defines \
                 wallet.unlock_prepare but §13.1 has no wallet_unlock ceremony_kind"
            )
        }
        Cmd::Wallet(WalletCmd::UpdatePolicy {
            name,
            file,
            assurance_level,
        }) => prepare_policy_update(&home, &name, &file, &assurance_level).await,
        Cmd::Wallet(WalletCmd::CommitPolicy { operation_id }) => {
            commit_policy_update(&home, operation_id).await
        }
        Cmd::Wallet(WalletCmd::RebindPasskey { name }) => {
            let wallet_id = bloom_triad_protocol::Token::new(name.clone())
                .context("wallet ID must be a protocol token")?;
            launch_custody_ceremony(
                &home,
                &name,
                bloom_machine_client::CustodyPrepareMethod::CredentialReplace,
                bloom_triad_protocol::CeremonyKind::CredentialReplace,
                Some(wallet_id),
                "credential-prf",
            )
            .await
        }
        Cmd::Wallet(WalletCmd::Delete { name }) => {
            let wallet_id = bloom_triad_protocol::Token::new(name.clone())
                .context("wallet ID must be a protocol token")?;
            launch_custody_ceremony(
                &home,
                &name,
                bloom_machine_client::CustodyPrepareMethod::WalletDelete,
                bloom_triad_protocol::CeremonyKind::WalletDelete,
                Some(wallet_id),
                "none",
            )
            .await
        }
        Cmd::Wallet(WalletCmd::Stage {
            wallet,
            chain,
            intent,
        }) => {
            let body = match intent {
                Some(s) => s,
                None => {
                    let mut buf = String::new();
                    std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)?;
                    buf
                }
            };
            let path = format!("/wallets/{wallet}/chains/{chain}/outbox/new.tx");
            let client = IpcClient::new(&client_endpoint.socket);
            let ipc_res = try_ipc(
                &client,
                &client_endpoint,
                "write",
                serde_json::json!({ "path": path, "bytes_b64": B64.encode(body.as_bytes()) }),
            )
            .await
            .with_context(|| format!("ipc wallet stage via {}", client_endpoint.display))?;
            if ipc_res.is_some() {
                debug!(endpoint = %client_endpoint.display, "cli.wallet.stage.via_ipc");
                return Ok(());
            }
            debug!("cli.wallet.stage.via_inproc: no daemon socket present");
            let (home_permit, d) = build_write_daemon(home)?;
            let parsed = bloom_tx::intent_parser::parse(&body).context("parse intent")?;
            let info = d.keystore.info(&wallet)?;
            let client = d
                .chains
                .get(&chain)
                .with_context(|| format!("chain '{}'", chain))?;
            let staged = d
                .tx_engine
                .stage(
                    &home_permit,
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
            text,
        }) => {
            let path = format!("/wallets/{wallet}/chains/{chain}/outbox/pending/{id}/confirm");
            let body = text.into_bytes();
            let client = IpcClient::new(&client_endpoint.socket);
            let ipc_res = try_ipc(
                &client,
                &client_endpoint,
                "write",
                serde_json::json!({
                    "path": path,
                    "bytes_b64": B64.encode(&body),
                }),
            )
            .await
            .with_context(|| format!("ipc wallet confirm via {}", client_endpoint.display))?;
            if ipc_res.is_some() {
                debug!(endpoint = %client_endpoint.display, "cli.wallet.confirm.via_ipc");
                return Ok(());
            }
            debug!("cli.wallet.confirm.via_inproc: no daemon socket present");
            let (_home_permit, d) = build_write_daemon(home)?;
            d.vfs
                .write(&VfsPath::parse(&path)?, &body)
                .await
                .context("wallet confirm")?;
            Ok(())
        }
        Cmd::Wallet(WalletCmd::Cancel {
            wallet,
            chain,
            id,
            text,
        }) => {
            wallet_outbox_action_vfs_write(WalletOutboxActionWrite {
                home,
                client_endpoint: &client_endpoint,
                wallet: wallet.clone(),
                chain,
                id: id.clone(),
                action: "cancel",
                body: text.into_bytes(),
            })
            .await?;
            println!("cancel submitted for {id}");
            Ok(())
        }
        Cmd::Wallet(WalletCmd::Replace {
            wallet,
            chain,
            id,
            intent,
        }) => {
            let body = match intent {
                Some(s) => s,
                None => {
                    let mut buf = String::new();
                    std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)?;
                    buf
                }
            };
            wallet_outbox_action_vfs_write(WalletOutboxActionWrite {
                home,
                client_endpoint: &client_endpoint,
                wallet,
                chain,
                id: id.clone(),
                action: "replace",
                body: body.into_bytes(),
            })
            .await?;
            println!("replacement submitted for {id}");
            Ok(())
        }
        Cmd::Wallet(WalletCmd::ConfirmBatch { wallet, txs, text }) => {
            if txs.is_empty() {
                bail!("confirm-batch needs at least one tx ref like base:0001-abc");
            }
            for tx in &txs {
                let _ = parse_batch_tx_ref(tx)?;
            }
            bail!(
                "atomic batch confirmation for wallet '{wallet}' is unavailable until the \
                 Machine signing.sign_batch projection is connected to Broker; the legacy \
                 in-process policy-session ceremony has been removed (confirmation text: \
                 {text:?})"
            )
        }
        Cmd::Ceremony(command) => handle_ceremony(&home, command).await,
        Cmd::Serve { endpoint, mount } => {
            eprintln!("{ALPHA_DISCLOSURE}");
            let (_home_permit, d) = build_write_daemon(home.clone())?;
            github_source::ensure_preinstalled_petals(&home, &d)
                .context("provision configured pre-installed Petals before serving")?;
            // Spawn the outbox expiry sweeper for the lifetime of the
            // serve command (fix #3). The handle is dropped (and the task
            // signalled to stop) right before the function returns.
            let sweeper = d.spawn_background_tasks();
            let mount_handle = mount_bloom(&d, mount.as_deref()).await?;
            let chains: Vec<String> = d.chains.list_names();
            println!(
                "bloom serve: home={} chains={:?}",
                d.home.root().display(),
                chains
            );
            if let Some(mount_path) = mount.as_deref() {
                println!("mount: {}", mount_path.display());
            }
            let endpoint = resolve_server_endpoint(&d.home, endpoint.as_deref())
                .context("resolve serve endpoint")?;
            let socket = endpoint.socket.clone();
            println!("ipc endpoint: {}", endpoint.display);
            println!("ipc socket: {}", socket.display());
            info!(home = %d.home.root().display(), chains = ?chains, endpoint = %endpoint.display, socket = %socket.display(), mount = ?mount, "cli.serve.starting");
            let server = IpcServer::new(d.vfs.clone(), env!("CARGO_PKG_VERSION"), chains)
                .with_petals(d.petals.clone());
            let server2 = server.clone();
            // Trigger graceful shutdown on Ctrl-C or SIGTERM.
            let shutdown = tokio::spawn(async move {
                #[cfg(unix)]
                {
                    use tokio::signal::unix::{SignalKind, signal};
                    let mut sigterm = signal(SignalKind::terminate())
                        .expect("SIGTERM handler registration failed");
                    tokio::select! {
                        _ = tokio::signal::ctrl_c() => info!("cli.serve.ctrl_c_received"),
                        _ = sigterm.recv() => info!("cli.serve.sigterm_received"),
                    }
                }
                #[cfg(not(unix))]
                {
                    let _ = tokio::signal::ctrl_c().await;
                    info!("cli.serve.ctrl_c_received");
                }
                server2.trigger_shutdown();
            });
            let serve_result = server.serve(&socket).await.context("ipc serve");
            shutdown.abort();
            // Stop the outbox expiry sweeper (fix #3) and any other
            // daemon-owned workers (watch executor, etc., fix #6).
            let unmount_result = unmount_bloom(mount_handle).await;
            sweeper.shutdown().await;
            d.shutdown().await;
            serve_result?;
            unmount_result?;
            info!("cli.serve.shutdown_complete");
            println!("shutting down");
            Ok(())
        }
        Cmd::Hyperliquid(cmd) => handle_hyperliquid(home, &client_endpoint, cmd).await,
        Cmd::Update(cmd) => handle_update(&home, cmd).await,
        Cmd::Petals(cmd) => {
            let _home_permit = HomeWritePermit::acquire(&home)?;
            run_petals(home, cmd).await
        }

        Cmd::Completions { shell } => {
            generate(shell, &mut Cli::command(), "bloom", &mut std::io::stdout());
            Ok(())
        }
        Cmd::Ipc(IpcCmd::Call { method, params }) => {
            let endpoint = client_endpoint;
            if !endpoint.socket.exists() {
                debug!(endpoint = %endpoint.display, "cli.ipc.call.no_socket: daemon may not be running");
            }
            let client = IpcClient::new(&endpoint.socket);
            let v: serde_json::Value = match params {
                Some(s) => serde_json::from_str(&s).context("parse params JSON")?,
                None => serde_json::Value::Null,
            };
            debug!(%method, endpoint = %endpoint.display, "cli.ipc.call");
            let result = client
                .call(&method, v)
                .await
                .with_context(|| format!("ipc call to {}", endpoint.display))?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            Ok(())
        }
    }
}

async fn run_petals(home: HomeDir, cmd: PetalsCmd) -> Result<()> {
    let cmd = match cmd {
        PetalsCmd::Build { package_dir, out } => {
            if let Some(out) = out.as_deref() {
                reject_archive_output_inside_package(&package_dir, out)?;
            }
            let package = bloom_petals::package::build_petal_package_dir(&package_dir)
                .with_context(|| format!("build Petal package {package_dir}"))?;
            let consent = bloom_petals::package::petal_consent_summary(&package)
                .context("build Petal consent summary")?;
            println!("hash: {}", package.hash);
            println!("contract: {}", bloom_petals::package::ROUTE_PACKAGE);
            println!(
                "wit_digest: {}",
                bloom_petals::package::contract_wit_digest()
            );
            println!("petal_mount: petals/{}/", package.name);
            println!("routes: {}", package.route_index.routes.len());
            println!("artifacts: {package_dir}/artifacts");
            print_petal_consent(&consent);
            if let Some(out) = out {
                let file =
                    std::fs::File::create(&out).with_context(|| format!("create archive {out}"))?;
                package
                    .write_petal_tar(file)
                    .with_context(|| format!("write archive {out}"))?;
                println!("archive: {out}");
            }
            return Ok(());
        }
        other => other,
    };

    let d = Daemon::from_home(home.clone()).context("build daemon")?;
    match cmd {
        PetalsCmd::Install { path, ref_ } => {
            if let Some(repo) = github_source::parse_github_install_url(&path)? {
                let installed =
                    github_source::install_github_source(&home, &d, &repo, ref_.as_deref())?;
                println!();
                println!("hash: {}", installed.result.hash);
                println!("mode: petal");
                println!("size: {} bytes", installed.result.size);
                if installed.result.already_present {
                    println!("note: already installed");
                }
                if let Some(app) = &installed.meta.petal {
                    println!("petal_mount: petals/{}/", app.name);
                }
                println!("routes: {}", installed.index.routes.len());
                println!(
                    "source: {}/{}@{}",
                    installed.provenance.owner,
                    installed.provenance.repo,
                    installed
                        .provenance
                        .selected_tag
                        .as_deref()
                        .unwrap_or(&installed.provenance.requested_ref)
                );
                println!("resolved_commit: {}", installed.provenance.resolved_commit);
                print_petal_consent(&installed.consent);
                return Ok(());
            }

            if ref_.is_some() {
                bail!("--ref is only supported for trusted GitHub source installs");
            }
            let path_meta = std::fs::metadata(&path).with_context(|| format!("stat {path}"))?;
            let is_petal_dir = path_meta.is_dir();
            if !is_petal_dir && !path.ends_with(".petal.tar") {
                bail!(
                    "petals install only accepts Petal package directories, .petal.tar archives, or trusted GitHub source repositories"
                );
            }
            let consent_package = if is_petal_dir {
                bloom_petals::package::PreparedPetalPackage::from_dir(&path)
                    .with_context(|| format!("read Petal package dir {path}"))?
            } else {
                bloom_petals::package::PreparedPetalPackage::from_petal_tar(&path)
                    .with_context(|| format!("read Petal package archive {path}"))?
            };
            let mut consent = bloom_petals::package::petal_consent_summary(&consent_package)
                .context("build app consent summary")?;
            apply_configured_petal_endpoints(&d, &mut consent)?;
            let (result, meta, index) = if is_petal_dir {
                d.petals
                    .store()
                    .install_petal_package_dir(&path)
                    .with_context(|| format!("install Petal package dir {path}"))?
            } else {
                d.petals
                    .store()
                    .install_petal_package_tar(&path)
                    .with_context(|| format!("install Petal package archive {path}"))?
            };
            println!("hash: {}", result.hash);
            println!("mode: petal");
            println!("size: {} bytes", result.size);
            if result.already_present {
                println!("note: already installed");
            }
            if let Some(app) = &meta.petal {
                println!("petal_mount: petals/{}/", app.name);
            }
            println!("routes: {}", index.routes.len());
            print_petal_consent(&consent);
            Ok(())
        }
        PetalsCmd::Build { .. } => {
            unreachable!("Petal build commands are handled before daemon startup")
        }
        PetalsCmd::Ls => {
            let package_hashes = d
                .petals
                .store()
                .list_package_hashes()
                .context("list Petal packages")?;
            if package_hashes.is_empty() {
                println!("(no petals installed)");
                return Ok(());
            }
            for h in package_hashes {
                let meta = d.petals.store().load_meta(&h).context("load meta")?;
                let app = meta
                    .petal
                    .as_ref()
                    .map(|app| format!("  app=petals/{}/", app.name))
                    .unwrap_or_default();
                let source = meta.source.as_ref().map_or_else(String::new, |source| {
                    let selected = source
                        .selected_tag
                        .as_deref()
                        .unwrap_or(&source.requested_ref);
                    format!("  source={}/{}@{}", source.owner, source.repo, selected)
                });
                println!(
                    "{}  {:<7}  {:>7}  caps=[]  name=-{}{}",
                    &meta.hash[..bloom_petals::store::HASH_PREFIX_LEN],
                    "app",
                    meta.size,
                    app,
                    source
                );
            }
            Ok(())
        }
        PetalsCmd::Uninstall { target } => {
            let removed = d.petals.uninstall(&target).context("uninstall petal")?;
            if removed {
                println!("removed {target}");
            } else {
                println!("not installed: {target}");
            }
            Ok(())
        }
    }
}

fn print_petal_consent(summary: &bloom_petals::package::PetalConsentSummary) {
    println!("consent:");
    if let Some(package_summary) = &summary.package_summary {
        println!("  summary: {package_summary}");
    }
    println!("  docs: {}", summary.docs.join(", "));
    if !summary.capabilities.is_empty() {
        println!("  capabilities: {}", summary.capabilities.join(", "));
    }
    if !summary.network.is_empty() {
        println!("  network:");
        for rule in &summary.network {
            println!("{}", format_petal_consent_net_rule(rule));
        }
    }
    if !summary.sign_intents.is_empty() {
        println!("  signing_intents: {}", summary.sign_intents.join(", "));
    }
    if !summary.store_namespaces.is_empty() {
        println!("  private_store:");
        for ns in &summary.store_namespaces {
            let visibility = if ns.secret { "secret" } else { "private" };
            println!("    - {} {}", ns.namespace, visibility);
        }
    }
    if !summary.routes.is_empty() {
        println!("  routes:");
        for route in &summary.routes {
            let ops = route
                .ops
                .iter()
                .map(|op| format!("{op:?}").to_ascii_lowercase())
                .collect::<Vec<_>>()
                .join(",");
            let mut flags = Vec::new();
            if route.side_effecting_read {
                flags.push("side_effecting_read".to_string());
            }
            if route.write_async {
                flags.push("write_async".to_string());
            }
            if let Some(ttl) = route.cache_ttl_ms {
                flags.push(format!("cache_ttl_ms={ttl}"));
            }
            let caps = if route.required_caps.is_empty() {
                "-".to_string()
            } else {
                route.required_caps.join(",")
            };
            if flags.is_empty() {
                println!("    - {} ops=[{}] caps=[{}]", route.path, ops, caps);
            } else {
                println!(
                    "    - {} ops=[{}] caps=[{}] flags=[{}]",
                    route.path,
                    ops,
                    caps,
                    flags.join(",")
                );
            }
        }
    }
}

fn apply_configured_petal_endpoints(
    daemon: &Daemon,
    summary: &mut bloom_petals::package::PetalConsentSummary,
) -> Result<()> {
    let bindings = daemon
        .config
        .petals
        .runtime
        .get(&summary.name)
        .map(|app| &app.endpoints)
        .cloned()
        .unwrap_or_default();
    bloom_petals::package::apply_petal_consent_endpoint_bindings(summary, &bindings)
        .context("apply configured Petal endpoint bindings")
}

fn format_petal_consent_net_rule(rule: &bloom_petals::package::PetalConsentNetRule) -> String {
    let binding = rule
        .binding
        .as_deref()
        .map(|binding| format!(" binding={binding}"))
        .unwrap_or_default();
    let effective = rule
        .effective_origin
        .as_deref()
        .map(|origin| format!(" effective_origin={origin}"))
        .unwrap_or_default();
    format!(
        "    - declared_host={}{}{} methods=[{}] paths=[{}]",
        rule.host,
        binding,
        effective,
        rule.methods.join(","),
        rule.paths.join(",")
    )
}

#[cfg(feature = "mount")]
async fn mount_bloom(
    daemon: &Daemon,
    mount: Option<&std::path::Path>,
) -> Result<Option<bloom_mount::NfsMountHandle>> {
    match mount {
        Some(path) => daemon
            .mount(path)
            .await
            .map(Some)
            .with_context(|| format!("mount bloom vfs at {}", path.display())),
        None => Ok(None),
    }
}

struct WalletOutboxActionWrite<'a> {
    home: HomeDir,
    client_endpoint: &'a ResolvedEndpoint,
    wallet: String,
    chain: String,
    id: String,
    action: &'a str,
    body: Vec<u8>,
}

async fn wallet_outbox_action_vfs_write(input: WalletOutboxActionWrite<'_>) -> Result<()> {
    let WalletOutboxActionWrite {
        home,
        client_endpoint,
        wallet,
        chain,
        id,
        action,
        body,
    } = input;
    if !matches!(action, "cancel" | "replace") {
        bail!("unsupported wallet outbox action '{action}'");
    }
    let path = format!("/wallets/{wallet}/chains/{chain}/outbox/pending/{id}/{action}");
    let client = IpcClient::new(&client_endpoint.socket);
    let ipc_res = try_ipc(
        &client,
        client_endpoint,
        "write",
        serde_json::json!({
            "path": path,
            "bytes_b64": B64.encode(&body),
        }),
    )
    .await
    .with_context(|| format!("ipc wallet outbox {action} via {}", client_endpoint.display))?;
    if ipc_res.is_some() {
        debug!(endpoint = %client_endpoint.display, action, "cli.wallet.outbox_action.via_ipc");
        return Ok(());
    }

    debug!(
        action,
        "cli.wallet.outbox_action.via_inproc: no daemon socket present"
    );
    let p = VfsPath::parse(&path)?;
    let (_home_permit, d) = build_write_daemon(home)?;
    d.vfs
        .write(&p, &body)
        .await
        .with_context(|| format!("wallet outbox {action}"))?;
    Ok(())
}

fn request_body_with_wallet(mut request: String, wallet: Option<&str>) -> String {
    let Some(wallet) = wallet else {
        return request;
    };
    if let Ok(mut value) = request.parse::<toml::Value>()
        && value.get("url").is_some()
        && let Some(table) = value.as_table_mut()
    {
        table.insert("wallet".into(), toml::Value::String(wallet.to_string()));
        return toml::to_string_pretty(&value).unwrap_or_else(|_| {
            let mut fallback = request.clone();
            fallback.push('\n');
            fallback.push_str(&format!("wallet = \"{wallet}\""));
            fallback
        });
    }
    let Some(first_newline) = request.find('\n') else {
        request.push(' ');
        request.push_str(&format!("wallet={wallet}"));
        return request;
    };
    request.insert_str(first_newline, &format!(" wallet={wallet}"));
    request
}

fn parse_batch_tx_ref(s: &str) -> Result<(String, String)> {
    let (chain, id) = s
        .split_once(':')
        .with_context(|| format!("tx ref '{s}' must be chain:id"))?;
    let chain = chain.trim();
    let id = id.trim();
    if chain.is_empty() || id.is_empty() {
        bail!("tx ref '{s}' must include non-empty chain and id");
    }
    Ok((chain.to_string(), id.to_string()))
}

async fn handle_hyperliquid(
    home: HomeDir,
    endpoint: &ResolvedEndpoint,
    cmd: HyperliquidCmd,
) -> Result<()> {
    match cmd {
        HyperliquidCmd::Account { user, network } => {
            print_hl_info(&home, &network, hl_user_req("clearinghouseState", &user)).await
        }
        HyperliquidCmd::SpotState { user, network } => {
            print_hl_info(
                &home,
                &network,
                hl_user_req("spotClearinghouseState", &user),
            )
            .await
        }
        HyperliquidCmd::OpenOrders { user, network } => {
            print_hl_info(&home, &network, hl_user_req("openOrders", &user)).await
        }
        HyperliquidCmd::Fills { user, network } => {
            print_hl_info(&home, &network, hl_user_req("userFills", &user)).await
        }
        HyperliquidCmd::Funding {
            user,
            coin,
            start_time,
            end_time,
            network,
        } => {
            let mut req = serde_json::json!({
                "type": "userFunding",
                "user": user.to_ascii_lowercase(),
                "coin": coin,
            });
            let obj = req.as_object_mut().expect("json object");
            if let Some(start) = start_time {
                obj.insert("startTime".into(), serde_json::json!(start));
            }
            if let Some(end) = end_time {
                obj.insert("endTime".into(), serde_json::json!(end));
            }
            print_hl_info(&home, &network, req).await
        }
        HyperliquidCmd::Book { coin, network } => {
            print_hl_info(
                &home,
                &network,
                serde_json::json!({"type": "l2Book", "coin": coin}),
            )
            .await
        }
        HyperliquidCmd::Candles {
            coin,
            interval,
            start_time,
            end_time,
            network,
        } => {
            print_hl_info(
                &home,
                &network,
                serde_json::json!({
                    "type": "candleSnapshot",
                    "req": {
                        "coin": coin,
                        "interval": interval,
                        "startTime": start_time,
                        "endTime": end_time,
                    }
                }),
            )
            .await
        }
        HyperliquidCmd::Metadata { kind, network } => {
            let body = match kind.as_str() {
                "perp" => serde_json::json!({"type": "meta"}),
                "perp-contexts" => serde_json::json!({"type": "metaAndAssetCtxs"}),
                "spot" => serde_json::json!({"type": "spotMeta"}),
                "spot-contexts" => serde_json::json!({"type": "spotMetaAndAssetCtxs"}),
                "mids" => serde_json::json!({"type": "allMids"}),
                other => bail!(
                    "unknown metadata kind '{other}' (use perp, perp-contexts, spot, spot-contexts, mids)"
                ),
            };
            print_hl_info(&home, &network, body).await
        }
        HyperliquidCmd::Session { command } => handle_hl_session(endpoint, command).await,
        HyperliquidCmd::SendAsset {
            wallet,
            destination,
            amount,
            network,
        } => {
            let path = format!("/hyperliquid/{network}/exchange/{wallet}/send_asset.json");
            let body = serde_json::to_vec(&UsdSendRequest {
                destination,
                amount,
                nonce: None,
            })?;
            hl_session_ipc_write_with_sealed_approval(endpoint, &path, body, &wallet).await?;
            let last_response =
                format!("/hyperliquid/{network}/exchange/{wallet}/last_response.json");
            match hl_session_ipc_read(endpoint, &last_response).await {
                Ok(bytes) => std::io::Write::write_all(&mut std::io::stdout(), &bytes)?,
                Err(_) => println!("usdSend submitted"),
            }
            Ok(())
        }
        HyperliquidCmd::TestReads {
            user,
            coin,
            network,
        } => test_hl_reads(&home, &network, &user, &coin).await,
        HyperliquidCmd::TestPostOnlyCancel {
            wallet,
            coin,
            asset,
            price,
            size,
            max_notional_usd,
            danger_accept_live_orders,
            network,
        } => {
            test_hl_post_only_cancel(
                home,
                TestPostOnlyCancelArgs {
                    wallet,
                    coin,
                    asset,
                    price,
                    size,
                    max_notional_usd,
                    danger_accept_live_orders,
                    network,
                },
            )
            .await
        }
    }
}

async fn handle_update(home: &HomeDir, cmd: UpdateCmd) -> Result<()> {
    match cmd {
        UpdateCmd::Status => {
            let installed = env!("CARGO_PKG_VERSION");
            let snap = bloom_update::read_cache_only(installed, &home.cache_dir());
            let json = serde_json::to_string_pretty(&snap).context("serialise update snapshot")?;
            println!("{json}");
            Ok(())
        }
        UpdateCmd::Check => {
            // An explicit check needs only a checker; avoid constructing
            // the full daemon and its unrelated VFS/transaction services.
            let checker =
                bloom_update::UpdateChecker::new(env!("CARGO_PKG_VERSION"), home.cache_dir())
                    .context("build update checker")?;
            let snap = checker.refresh().await;
            let json = serde_json::to_string_pretty(&snap).context("serialise update snapshot")?;
            println!("{json}");
            let code = match snap.available() {
                bloom_update::UpdateAvailable::OutOfDate => 1,
                bloom_update::UpdateAvailable::UpToDate => 0,
                bloom_update::UpdateAvailable::Unknown => 2,
            };
            if code != 0 {
                UPDATE_EXIT_CODE.store(code, std::sync::atomic::Ordering::SeqCst);
            }
            Ok(())
        }
    }
}

async fn handle_hl_session(endpoint: &ResolvedEndpoint, cmd: HyperliquidSessionCmd) -> Result<()> {
    match cmd {
        HyperliquidSessionCmd::Create {
            wallet,
            id,
            agent_name,
            vault_address,
            network,
        } => {
            let path = hl_session_wallet_path(&network, &wallet, "new.json");
            let body = serde_json::json!({
                "id": id,
                "agent_name": agent_name,
                "vault_address": vault_address,
            });
            hl_session_ipc_write_with_sealed_approval(
                endpoint,
                &path,
                serde_json::to_vec(&body)?,
                &wallet,
            )
            .await?;
            let last_response =
                format!("/hyperliquid/{network}/exchange/{wallet}/last_response.json");
            match hl_session_ipc_read(endpoint, &last_response).await {
                Ok(bytes) => std::io::Write::write_all(&mut std::io::stdout(), &bytes)?,
                Err(_) => println!("created Hyperliquid agent session"),
            }
            Ok(())
        }
        HyperliquidSessionCmd::Status {
            wallet,
            id,
            network,
        } => {
            let path = hl_session_path(&network, &wallet, &id, "status.json");
            let bytes = hl_session_ipc_read(endpoint, &path).await?;
            std::io::Write::write_all(&mut std::io::stdout(), &bytes)?;
            Ok(())
        }
        HyperliquidSessionCmd::Audit {
            wallet,
            id,
            network,
        } => {
            let path = hl_session_path(&network, &wallet, &id, "audit.jsonl");
            let bytes = hl_session_ipc_read(endpoint, &path).await?;
            std::io::Write::write_all(&mut std::io::stdout(), &bytes)?;
            Ok(())
        }
        HyperliquidSessionCmd::Stop {
            wallet,
            id,
            network,
        } => {
            hl_session_ipc_write(
                endpoint,
                &hl_session_path(&network, &wallet, &id, "stop"),
                Vec::new(),
            )
            .await
        }
        HyperliquidSessionCmd::CancelAll {
            wallet,
            id,
            network,
        } => {
            hl_session_ipc_write(
                endpoint,
                &hl_session_path(&network, &wallet, &id, "cancel_all"),
                Vec::new(),
            )
            .await
        }
        HyperliquidSessionCmd::CloseAll {
            wallet,
            id,
            network,
        } => {
            hl_session_ipc_write(
                endpoint,
                &hl_session_path(&network, &wallet, &id, "close_all"),
                Vec::new(),
            )
            .await
        }
    }
}

fn hl_session_wallet_path(network: &str, wallet: &str, file: &str) -> String {
    format!("/hyperliquid/{network}/agent_sessions/{wallet}/{file}")
}

fn hl_session_path(network: &str, wallet: &str, id: &str, file: &str) -> String {
    format!("/hyperliquid/{network}/agent_sessions/{wallet}/{id}/{file}")
}

async fn hl_session_ipc_read(endpoint: &ResolvedEndpoint, path: &str) -> Result<Vec<u8>> {
    let client = IpcClient::new(&endpoint.socket);
    let Some(res) = try_ipc(
        &client,
        endpoint,
        "read",
        serde_json::json!({ "path": path }),
    )
    .await
    .with_context(|| format!("ipc read via {}", endpoint.display))?
    else {
        bail!("Hyperliquid agent sessions require a running bloom serve daemon");
    };
    let b64 = res
        .get("bytes_b64")
        .and_then(|v| v.as_str())
        .context("ipc read: missing bytes_b64")?;
    B64.decode(b64).context("ipc read: bad base64")
}

async fn hl_session_ipc_write(
    endpoint: &ResolvedEndpoint,
    path: &str,
    body: Vec<u8>,
) -> Result<()> {
    let client = IpcClient::new(&endpoint.socket);
    let res = try_ipc(
        &client,
        endpoint,
        "write",
        serde_json::json!({
            "path": path,
            "bytes_b64": B64.encode(&body),
        }),
    )
    .await
    .with_context(|| format!("ipc write via {}", endpoint.display))?;
    if res.is_none() {
        bail!("Hyperliquid agent sessions require a running bloom serve daemon");
    }
    Ok(())
}

async fn hl_session_ipc_write_with_sealed_approval(
    endpoint: &ResolvedEndpoint,
    path: &str,
    body: Vec<u8>,
    wallet: &str,
) -> Result<()> {
    match hl_session_ipc_write_once(endpoint, path, &body).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            bail!("Hyperliquid Sealed Approval requires a running bloom serve daemon");
        }
        Err(e) if is_ipc_permission_denied(&e) => {
            bail!(
                "Hyperliquid signing for wallet '{wallet}' requires the Broker-backed \
                 payload-bearing Sealed Approval flow; the legacy Machine-hosted ceremony \
                 has been removed"
            )
        }
        Err(e) => Err(e).with_context(|| format!("ipc write via {}", endpoint.display)),
    }
}

async fn hl_session_ipc_write_once(
    endpoint: &ResolvedEndpoint,
    path: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let client = IpcClient::new(&endpoint.socket);
    client
        .call(
            "write",
            serde_json::json!({
                "path": path,
                "bytes_b64": B64.encode(body),
            }),
        )
        .await
        .map(|_| ())
}

fn is_ipc_permission_denied(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::PermissionDenied
        || e.to_string().contains("\"code\":-32007")
        || e.to_string()
            .to_ascii_lowercase()
            .contains("permission denied")
}

fn hl_network(raw: &str) -> Result<HyperliquidNetwork> {
    match raw {
        "mainnet" => Ok(HyperliquidNetwork::Mainnet),
        "testnet" => Ok(HyperliquidNetwork::Testnet),
        other => bail!("unknown Hyperliquid network '{other}' (use mainnet or testnet)"),
    }
}

fn hl_client(home: &HomeDir, raw: &str) -> Result<HyperliquidClient> {
    let network = hl_network(raw)?;
    let mut client = HyperliquidClient::new(network);
    // Honor [hyperliquid] mainnet_url/testnet_url overrides, same as the daemon
    // (so local/staging/proxy deployments work from the CLI too).
    if let Ok(config) = bloom_proto::Config::load_or_init(&home.config_path())
        && let Some(hl) = config.hyperliquid
    {
        let raw_url = match network {
            HyperliquidNetwork::Mainnet => hl.mainnet_url,
            HyperliquidNetwork::Testnet => hl.testnet_url,
        };
        if let Ok(url) = raw_url.parse::<url::Url>() {
            client = client.with_base_url(url);
        }
    }
    Ok(client)
}

fn hl_user_req(kind: &str, user: &str) -> serde_json::Value {
    serde_json::json!({
        "type": kind,
        "user": user.to_ascii_lowercase(),
    })
}

async fn print_hl_info(home: &HomeDir, network: &str, body: serde_json::Value) -> Result<()> {
    let client = hl_client(home, network)?;
    let value = client.info(body).await?;
    std::io::Write::write_all(&mut std::io::stdout(), &pretty_json(&value))?;
    Ok(())
}

async fn test_hl_reads(home: &HomeDir, network: &str, user: &str, coin: &str) -> Result<()> {
    let client = hl_client(home, network)?;
    let now = bloom_hyperliquid::now_ms();
    let start = now.saturating_sub(60 * 60 * 1000);
    let calls = [
        ("account", hl_user_req("clearinghouseState", user)),
        ("spot_state", hl_user_req("spotClearinghouseState", user)),
        ("open_orders", hl_user_req("openOrders", user)),
        (
            "frontend_open_orders",
            hl_user_req("frontendOpenOrders", user),
        ),
        ("fills", hl_user_req("userFills", user)),
        (
            "funding",
            serde_json::json!({
                "type": "userFunding",
                "user": user.to_ascii_lowercase(),
                "coin": coin,
                "startTime": start,
                "endTime": now,
            }),
        ),
        ("portfolio", hl_user_req("portfolio", user)),
        ("rate_limit", hl_user_req("userRateLimit", user)),
        ("mids", serde_json::json!({"type": "allMids"})),
        ("perp_meta", serde_json::json!({"type": "meta"})),
        (
            "perp_contexts",
            serde_json::json!({"type": "metaAndAssetCtxs"}),
        ),
        ("spot_meta", serde_json::json!({"type": "spotMeta"})),
        (
            "spot_contexts",
            serde_json::json!({"type": "spotMetaAndAssetCtxs"}),
        ),
        ("book", serde_json::json!({"type": "l2Book", "coin": coin})),
        (
            "candles",
            serde_json::json!({
                "type": "candleSnapshot",
                "req": {
                    "coin": coin,
                    "interval": "1m",
                    "startTime": start,
                    "endTime": now,
                }
            }),
        ),
    ];

    let mut out = serde_json::Map::new();
    for (name, body) in calls {
        match client.info(body).await {
            Ok(value) => {
                out.insert(name.to_string(), value);
            }
            Err(e) => {
                out.insert(
                    name.to_string(),
                    serde_json::json!({"error": e.to_string()}),
                );
            }
        }
    }
    std::io::Write::write_all(
        &mut std::io::stdout(),
        &pretty_json(&serde_json::Value::Object(out)),
    )?;
    Ok(())
}

struct TestPostOnlyCancelArgs {
    wallet: String,
    coin: String,
    asset: u32,
    price: Option<String>,
    size: Option<String>,
    max_notional_usd: f64,
    danger_accept_live_orders: bool,
    network: String,
}

async fn test_hl_post_only_cancel(_home: HomeDir, args: TestPostOnlyCancelArgs) -> Result<()> {
    let TestPostOnlyCancelArgs {
        wallet: _wallet,
        coin: _coin,
        asset: _asset,
        price: _price,
        size: _size,
        max_notional_usd,
        danger_accept_live_orders,
        network: _network,
    } = args;
    if !danger_accept_live_orders {
        bail!("refusing live Hyperliquid test order without --danger-accept-live-orders");
    }
    if max_notional_usd <= 0.0 {
        bail!("--max-notional-usd must be positive");
    }
    bail!(
        "direct owner-key Hyperliquid test orders are disabled; create a Sealed Approval agent session and submit through /hyperliquid/<network>/agent_sessions/<wallet>/<session>/order.json"
    )
}

#[cfg(not(feature = "mount"))]
async fn mount_bloom(daemon: &Daemon, mount: Option<&std::path::Path>) -> Result<Option<()>> {
    let _ = daemon;
    match mount {
        Some(path) => anyhow::bail!(
            "mount support is not enabled in this build; rebuild with --features mount (release binaries are built with --all-features): {}",
            path.display()
        ),
        None => Ok(None),
    }
}

#[cfg(feature = "mount")]
async fn unmount_bloom(handle: Option<bloom_mount::NfsMountHandle>) -> Result<()> {
    if let Some(handle) = handle {
        bloom_mount::MountHandle::unmount(&handle)
            .await
            .context("unmount bloom vfs")?;
    }
    Ok(())
}

#[cfg(not(feature = "mount"))]
async fn unmount_bloom(handle: Option<()>) -> Result<()> {
    let _ = handle;
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    use super::{
        Cli, Cmd, WalletCmd, ceremony_projection_path, format_petal_consent_net_rule,
        is_completed_policy_update_receipt, load_ceremony_projection, persist_ceremony_projection,
        request_body_with_wallet,
    };

    #[test]
    fn petal_consent_network_line_includes_named_binding() {
        let line = format_petal_consent_net_rule(&bloom_petals::package::PetalConsentNetRule {
            binding: Some("clob".into()),
            host: "clob.polymarket.com".into(),
            effective_origin: Some("https://clob.internal.example".into()),
            methods: vec!["POST".into()],
            paths: vec!["/order".into()],
        });
        assert_eq!(
            line,
            "    - declared_host=clob.polymarket.com binding=clob effective_origin=https://clob.internal.example methods=[POST] paths=[/order]"
        );
    }

    #[test]
    fn request_wallet_injection_preserves_http_message_body() {
        let input = concat!(
            "POST https://api.example.com/data\n",
            "content-type: application/json\n",
            "\n",
            "{\"ok\":true}"
        )
        .to_string();
        let output = request_body_with_wallet(input, Some("gavin"));
        assert!(output.starts_with("POST https://api.example.com/data wallet=gavin\n"));
        assert!(output.ends_with("\n\n{\"ok\":true}"));
    }

    #[test]
    fn custody_projection_persists_atomically_and_without_secret_world_access() {
        let temp = tempfile::tempdir().unwrap();
        let home = bloom_proto::HomeDir::at(temp.path());
        let operation_id = bloom_triad_protocol::OperationId::from_bytes([61; 32]);
        let status = bloom_triad_protocol::CeremonyPublicStatus {
            ceremony_id: bloom_triad_protocol::Digest32::from_bytes([62; 32]),
            ceremony_kind: bloom_triad_protocol::CeremonyKind::WalletImport,
            operation_id: operation_id.clone(),
            state: bloom_triad_protocol::CeremonyState::AwaitingUser,
            expires_at_ms: bloom_triad_protocol::DecimalU64::new(u64::MAX),
            ceremony_url: Some("http://localhost:18734/ceremony/owner-readable-secret".into()),
            receipt_digest: None,
        };
        let projection =
            bloom_machine_client::CeremonyProjection::from_custody_status(&status, 1).unwrap();
        let path = persist_ceremony_projection(&home, &projection).unwrap();
        assert_eq!(path, ceremony_projection_path(&home, operation_id.as_str()));
        assert_eq!(
            load_ceremony_projection(&home, &operation_id)
                .unwrap()
                .unwrap(),
            projection
        );
        assert!(
            std::fs::read_dir(path.parent().unwrap())
                .unwrap()
                .all(|entry| !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .contains(".tmp"))
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn policy_cli_exposes_prepare_and_receipt_only_commit() {
        let prepared = Cli::try_parse_from([
            "bloom",
            "wallet",
            "update-policy",
            "wallet",
            "--file",
            "proposed.json",
        ])
        .unwrap();
        assert!(matches!(
            prepared.cmd,
            Cmd::Wallet(WalletCmd::UpdatePolicy {
                name,
                file,
                assurance_level,
            }) if name == "wallet"
                && file.as_os_str() == "proposed.json"
                && assurance_level == "user_verified"
        ));

        let committed =
            Cli::try_parse_from(["bloom", "wallet", "commit-policy", &"ab".repeat(32)]).unwrap();
        assert!(matches!(
            committed.cmd,
            Cmd::Wallet(WalletCmd::CommitPolicy { .. })
        ));
        assert!(
            Cli::try_parse_from(["bloom", "wallet", "update-policy", "wallet"]).is_err(),
            "prepare must require explicit proposed bytes"
        );
        assert!(
            Cli::try_parse_from(["bloom", "wallet", "sign-policy", "wallet"]).is_err(),
            "the legacy direct policy-signing path must stay removed"
        );
    }

    #[test]
    fn policy_commit_accepts_only_matching_completed_generic_custody_receipt() {
        let operation_id = bloom_triad_protocol::OperationId::from_bytes([71; 32]);
        let mut receipt = bloom_triad_protocol::CustodyResult {
            ceremony_kind: bloom_triad_protocol::CeremonyKind::PolicyUpdate,
            custody_operation_id: operation_id.clone(),
            public_status: bloom_triad_protocol::CeremonyState::Succeeded,
            wallet_id: Some(bloom_triad_protocol::Token::new("wallet").unwrap()),
            public_key_refs: Vec::new(),
            credential_summaries: Vec::new(),
            initial_policy: None,
            receipt_digest: bloom_triad_protocol::Digest32::from_bytes([72; 32]),
            encrypted_browser_result: None,
            signer_key_id: bloom_triad_protocol::Token::new("signer-ceremony-key").unwrap(),
            signer_signature: bloom_triad_protocol::Base64UrlBytes::from_bytes(&[73; 64]),
        };
        assert!(is_completed_policy_update_receipt(&receipt, &operation_id));

        receipt.public_status = bloom_triad_protocol::CeremonyState::Completed;
        assert!(!is_completed_policy_update_receipt(&receipt, &operation_id));
        receipt.public_status = bloom_triad_protocol::CeremonyState::Succeeded;
        receipt.ceremony_kind = bloom_triad_protocol::CeremonyKind::WalletDelete;
        assert!(!is_completed_policy_update_receipt(&receipt, &operation_id));
    }
}

#[cfg(test)]
mod hl_cli_tests {
    use super::*;

    #[test]
    fn post_only_cancel_test_requires_danger_flag() {
        let tmp = tempfile::tempdir().unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let err = rt
            .block_on(test_hl_post_only_cancel(
                HomeDir::at(tmp.path()),
                TestPostOnlyCancelArgs {
                    wallet: "minnow".into(),
                    coin: "BTC".into(),
                    asset: 0,
                    price: None,
                    size: None,
                    max_notional_usd: 15.0,
                    danger_accept_live_orders: false,
                    network: "mainnet".into(),
                },
            ))
            .unwrap_err();
        assert!(err.to_string().contains("--danger-accept-live-orders"));
    }

    #[test]
    fn hl_client_honors_config_endpoint_override() {
        let tmp = tempfile::tempdir().unwrap();
        let home = HomeDir::at(tmp.path());
        let mut cfg = bloom_proto::Config::local_default();
        cfg.hyperliquid = Some(bloom_proto::config::HyperliquidConfig {
            mainnet_url: "http://localhost:9999/".into(),
            ..Default::default()
        });
        std::fs::write(home.config_path(), toml::to_string(&cfg).unwrap()).unwrap();
        // Mainnet uses the configured override.
        let client = hl_client(&home, "mainnet").unwrap();
        assert_eq!(client.base_url().as_str(), "http://localhost:9999/");
        // Testnet wasn't overridden → default public endpoint.
        let tclient = hl_client(&home, "testnet").unwrap();
        assert!(tclient.base_url().as_str().contains("hyperliquid-testnet"));
    }
}
