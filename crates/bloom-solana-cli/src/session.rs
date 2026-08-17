//! Session composition: build the real machine stack for one CLI invocation.
//!
//! Everything is composed from configured profiles and durable state under
//! the session root — the fixture Ed25519 signer (seed from the account
//! registry's fixture-era identity) stands in for the BIP-39 Signer edge,
//! and the exact-approval ledger is driven by the confirm command.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use bloom_chain_rpc::mediator::Mediator;
use bloom_chain_rpc::{FreshnessPolicy, RpcTransport};
use bloom_solana_machine::fixture::{ExactApprovalLedger, FixtureEd25519Signer};
use bloom_solana_machine::host::MediatorHost;
use bloom_solana_machine::mount::{PINNED_SOLANA_DRIVER_PACKAGE_HASH, mount_pinned_solana_driver};
use bloom_solana_machine::{AccountRegistry, SolanaMachine};
use sha2::Digest as _;

use crate::profiles::ProfileConfig;

pub struct Session {
    pub machine: SolanaMachine,
    pub accounts: AccountRegistry,
    #[allow(dead_code)] // carried for future CLI surfaces (profile switching)
    pub profile: ProfileConfig,
    #[allow(dead_code)]
    pub state_root: PathBuf,
}

/// Default honest-runtime freshness policy for the CLI (devnet-paced).
pub fn freshness_policy() -> FreshnessPolicy {
    FreshnessPolicy {
        max_staleness_ms: 90_000,
        min_remaining_blocks: 16,
    }
}

/// Build a session over the durable state root for the named profile.
///
/// `approvals` is supplied by the command: staging uses a denying ledger so
/// nothing signs without an explicit confirm; confirm uses a ledger that
/// approves exactly one operation.
pub fn open(
    state_root: &Path,
    profile: ProfileConfig,
    approvals: Arc<ExactApprovalLedger>,
    transport: Arc<dyn RpcTransport>,
) -> anyhow::Result<Session> {
    let chain_profile = bloom_chain_rpc::mediator::ChainRpcProfile {
        name: format!("solana-{}", profile.name),
        family: "solana".into(),
        expected_genesis_hex: profile.expected_genesis_hex.clone(),
        allowed_read_methods: vec![
            "getGenesisHash".into(),
            "getHealth".into(),
            "getBlockHeight".into(),
            "getLatestBlockhash".into(),
            "getFeeForMessage".into(),
            "isBlockhashValid".into(),
            "getSignatureStatuses".into(),
            "getBalance".into(),
        ],
        allow_broadcast: profile.allow_broadcast,
        max_response_bytes: bloom_chain_rpc::mediator::DEFAULT_MAX_RESPONSE_BYTES,
    };
    let mediator = Arc::new(Mediator::new(chain_profile, vec![Box::new(transport)])?);
    let host = Arc::new(MediatorHost::new(mediator.clone(), system_now_ms));
    let petal_state = state_root.join("petal-state");
    std::fs::create_dir_all(&petal_state)?;
    let petal_dir = petal_source_dir()?;
    let vfs = Arc::new(mount_pinned_solana_driver(&petal_dir, &petal_state, host)?);
    let outbox_root = state_root.join("outbox");
    std::fs::create_dir_all(&outbox_root)?;
    let outbox = Arc::new(bloom_chain_action::ChainActionOutbox::new(&outbox_root)?);
    let accounts = AccountRegistry::open(state_root)?;
    let signer = fixture_signer_for_root(&accounts);
    let machine = SolanaMachine::new(
        vfs,
        mediator,
        outbox,
        signer,
        approvals,
        freshness_policy(),
        &profile.name,
        PINNED_SOLANA_DRIVER_PACKAGE_HASH,
    );
    Ok(Session {
        machine,
        accounts,
        profile,
        state_root: state_root.to_path_buf(),
    })
}

/// The committed Petal package directory. `BLOOM_SOLANA_PETAL_DIR` overrides
/// for out-of-tree testing; production always resolves the in-repo package.
pub fn petal_source_dir() -> anyhow::Result<PathBuf> {
    if let Ok(dir) = std::env::var("BLOOM_SOLANA_PETAL_DIR") {
        return Ok(PathBuf::from(dir));
    }
    // Compiled from the workspace: the crate sits at crates/bloom-solana-cli.
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .ancestors()
        .nth(2)
        .map(|root| root.join("petals/solana-driver"))
        .ok_or_else(|| anyhow::anyhow!("cannot locate workspace root"))
}

/// Fixture-era signing identity.
///
/// Deterministic stand-in custody: seed = SHA-256("bloom-solana-fixture" ||
/// wallet_id), derived fresh each invocation — no secret is ever stored.
/// `account enable` registers exactly the public key this derivation
/// produces, so account identity and signing identity always agree.
///
/// This is fixture glue; the BIP-39 swap replaces this function (and the
/// signer) wholesale with the real Signer edge.
fn fixture_signer_for_wallet(wallet_id: &str) -> FixtureEd25519Signer {
    let mut hasher = sha2::Sha256::new();
    sha2::Digest::update(&mut hasher, b"bloom-solana-fixture");
    sha2::Digest::update(&mut hasher, wallet_id.as_bytes());
    let seed: [u8; 32] = sha2::Digest::finalize(hasher).into();
    FixtureEd25519Signer::from_seed(seed)
}

/// The fixture public key for a wallet (what `account enable` registers).
pub fn fixture_public_key_hex(wallet_id: &str) -> String {
    use bloom_solana_machine::SigningAuthority as _;
    hex::encode(fixture_signer_for_wallet(wallet_id).public_key_bytes())
}

/// Resolve the session signer: the first enabled account's wallet fixes the
/// fixture identity; with no accounts, the default wallet's does.
fn fixture_signer_for_root(accounts: &AccountRegistry) -> Arc<FixtureEd25519Signer> {
    let wallet = accounts
        .list()
        .first()
        .map(|a| a.wallet_id.clone())
        .unwrap_or_else(|| "default".to_string());
    Arc::new(fixture_signer_for_wallet(&wallet))
}

pub fn system_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
