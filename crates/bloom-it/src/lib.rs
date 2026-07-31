//! Shared helpers for `bloom-it`'s integration tests.
//!
//! The original tests inlined an anvil-spawn helper inside each
//! `tests/*.rs` file; with multiple revert/trace tests landing alongside
//! the existing stage-confirm flow we hoist the spawn / fund / config
//! helpers here so each test only needs the bits specific to its
//! scenario.

use std::net::TcpListener;
use std::process::Stdio;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use bloom_auth_api::{
    AssuranceLevel, CANONICAL_INTENT_HEADER_SCHEMA_V1, CanonicalEnvelope, CanonicalIntentHeader,
    DaemonGrantTerms, EVM_SEALED_INTENT_SUBJECT_KIND, EVM_SEALED_INTENT_SUBJECT_SCHEMA_V1,
    EVM_TX_SIGN_INTENT, ExecutorKind, GrantStore, PetalPolicySnapshot, SealedAction,
    SealedApprovalGrant,
    petal_identity::{
        FIRST_PARTY_PETAL_VERSION_V0, PETAL_ID_EVM_WALLET, PLACEHOLDER_DIGEST_EVM_WALLET,
    },
};
use bloom_machine_client::MachineBrokerClient;
use bloom_triad_protocol::{
    ApprovalLifecycleState, ApprovalPrepareState, ApprovalPublicStatus, Base64UrlBytes, DecimalU64,
    Digest32, KeyRef, KeySpec, MachineBrokerRequest, MachineBrokerResponse, MachineBrokerService,
    NormalizedSignature, ProvenanceCatalog, ProvenanceFeeAsset, ProvenanceOperationClass,
    ProvenanceRecord, ProvenanceSubject, ServiceFuture, SigningPayloads, SigningResult, Token,
    WalletPublic,
};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::time::timeout;

/// Default funder; anvil's prefunded account #0.
pub const FUNDER_PRIV_KEY: &str =
    "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";

/// Test-only Broker boundary for real-chain integration tests. It signs the
/// exact payload bytes received over the production Machine client contract;
/// it does not expose or emulate the retired hash-only PetalHost path.
pub struct ExactSigningBrokerFixture {
    active: AtomicBool,
    signer: alloy_signer_local::PrivateKeySigner,
    key_ref: KeyRef,
    requests: parking_lot::Mutex<Vec<MachineBrokerRequest>>,
}

impl ExactSigningBrokerFixture {
    pub fn activate(&self) {
        self.active.store(true, Ordering::SeqCst);
    }

    pub fn requests(&self) -> Vec<MachineBrokerRequest> {
        self.requests.lock().clone()
    }
}

impl MachineBrokerService for ExactSigningBrokerFixture {
    fn dispatch<'a>(
        &'a self,
        request: MachineBrokerRequest,
    ) -> ServiceFuture<'a, MachineBrokerResponse> {
        Box::pin(async move {
            self.requests.lock().push(request.clone());
            match request {
                MachineBrokerRequest::WalletGetPublic(request) => {
                    Ok(MachineBrokerResponse::WalletGetPublic(WalletPublic {
                        wallet_id: request.wallet_id,
                        wallet_kind: Token::new("local").unwrap(),
                        key_refs: vec![self.key_ref.clone()],
                        policy_version: DecimalU64::new(1),
                        policy_digest: Digest32::from_bytes([7; 32]),
                        wallet_revocation_epoch: DecimalU64::new(0),
                    }))
                }
                MachineBrokerRequest::SealedApprovalPrepare(request) => {
                    Ok(MachineBrokerResponse::SealedApprovalPrepare(
                        bloom_triad_protocol::SealedApprovalPrepareResponse {
                            approval_id: request.terms.approval_id()?,
                            state: ApprovalPrepareState::AwaitingCeremony,
                            ceremony_url:
                                "http://localhost:18734/ceremony/exact-signing-test-secret".into(),
                            ceremony_expires_at_ms: request.terms.expires_at_ms,
                            review_manifest_digest: Digest32::from_bytes([8; 32]),
                        },
                    ))
                }
                MachineBrokerRequest::SealedApprovalStatus(request) => {
                    let active = self.active.load(Ordering::SeqCst);
                    Ok(MachineBrokerResponse::SealedApprovalStatus(
                        ApprovalPublicStatus {
                            approval_id: request.id,
                            wallet_id: Token::new("alice").unwrap(),
                            state: if active {
                                ApprovalLifecycleState::Active
                            } else {
                                ApprovalLifecycleState::AwaitingCeremony
                            },
                            effective_claim_assurance: None,
                            ceremony_url: (!active).then(|| {
                                "http://localhost:18734/ceremony/exact-signing-test-secret".into()
                            }),
                            ceremony_expires_at_ms: (!active).then(|| DecimalU64::new(u64::MAX)),
                        },
                    ))
                }
                MachineBrokerRequest::SigningSign(request) => {
                    let SigningPayloads::Single { payload } = &request.payloads else {
                        panic!("exact signing fixture expects one payload");
                    };
                    use alloy::signers::SignerSync as _;
                    let hash = alloy::primitives::keccak256(payload.decode());
                    let signature = self.signer.sign_hash_sync(&hash).unwrap();
                    Ok(MachineBrokerResponse::SigningSign(SigningResult {
                        operation_id: request.operation_id,
                        operation_digest: request.operation_digest,
                        signatures: vec![NormalizedSignature {
                            crypto_suite: request.crypto_suite,
                            bytes: Base64UrlBytes::from_bytes(&signature.as_bytes()),
                        }],
                        signer_receipt_digest: Digest32::from_bytes([9; 32]),
                        broker_receipt_digest: Digest32::from_bytes([10; 32]),
                    }))
                }
                other => panic!("unexpected exact signing Broker request: {other:?}"),
            }
        })
    }
}

pub fn exact_signing_broker(
    private_key_hex: &str,
) -> Result<(MachineBrokerClient, Arc<ExactSigningBrokerFixture>)> {
    let signer = private_key_hex
        .parse()
        .map_err(|error| anyhow!("parse exact-signing fixture key: {error}"))?;
    let fixture = Arc::new(ExactSigningBrokerFixture {
        active: AtomicBool::new(false),
        signer,
        key_ref: KeyRef {
            backend: Token::new("local").unwrap(),
            backend_instance: Token::new("integration-test").unwrap(),
            locator: "integration-test-wallet-key".into(),
            key_spec: KeySpec::Secp256k1,
            public_key_fingerprint: Digest32::from_bytes([6; 32]),
            derivation: None,
        },
        requests: parking_lot::Mutex::new(Vec::new()),
    });
    let service: Arc<dyn MachineBrokerService> = fixture.clone();
    Ok((MachineBrokerClient::new(service), fixture))
}

pub fn exact_signing_catalog(operation_classes: &[&str]) -> ProvenanceCatalog {
    ProvenanceCatalog {
        schema: bloom_triad_protocol::PROVENANCE_CATALOG_SCHEMA.into(),
        records: operation_classes
            .iter()
            .map(|operation_class| ProvenanceRecord {
                subject: ProvenanceSubject::System {
                    component_id: Token::new("bloom-machine").unwrap(),
                    operation_class: Token::new(*operation_class).unwrap(),
                },
                publisher: Token::new("bloom-installer").unwrap(),
                operation_classes: vec![ProvenanceOperationClass {
                    operation_class: Token::new(*operation_class).unwrap(),
                    fee_asset: Some(ProvenanceFeeAsset {
                        chain: Token::new("ethereum").unwrap(),
                        asset: "native".into(),
                    }),
                }],
                installer_key_id: Token::new("installer-key").unwrap(),
                installer_signature: Base64UrlBytes::from_bytes(&[11; 64]),
            })
            .collect(),
    }
}

/// Foundry binaries; rely on `$PATH`. Override with `BLOOM_ANVIL_BIN` /
/// `BLOOM_CAST_BIN` if you need to point at a specific install.
pub fn anvil_bin() -> String {
    std::env::var("BLOOM_ANVIL_BIN").unwrap_or_else(|_| "anvil".to_string())
}

pub fn cast_bin() -> String {
    std::env::var("BLOOM_CAST_BIN").unwrap_or_else(|_| "cast".to_string())
}

/// RAII guard that kills the spawned anvil process on drop.
pub struct AnvilGuard {
    child: Option<Child>,
    port: u16,
}

impl AnvilGuard {
    pub fn rpc_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Anvil serves WebSocket pubsub on the same TCP port as HTTP, so
    /// we just rewrite the scheme. Used by the `rpc_ws_subscriptions`
    /// integration test (WP-4).
    pub fn ws_url(&self) -> String {
        format!("ws://127.0.0.1:{}", self.port)
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Detach the underlying anvil `Child` so a test can `.kill()` /
    /// `.wait()` it explicitly. Used by the RPC failover test which
    /// needs to take an endpoint down mid-run and observe the
    /// fallback layer route around it.
    pub fn take_child(&mut self) -> Option<Child> {
        self.child.take()
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
pub fn pick_free_port() -> Result<u16> {
    let l = TcpListener::bind("127.0.0.1:0")?;
    Ok(l.local_addr()?.port())
}

/// Spawn anvil on a free port and wait until its stdout reports it is
/// listening. Returns a guard that kills the child on drop.
pub async fn spawn_anvil() -> Result<AnvilGuard> {
    let port = pick_free_port()?;
    let mut cmd = Command::new(anvil_bin());
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

/// Send a raw transaction with `cast send` from the prefunded funder
/// account; returns stdout as captured.
pub async fn cast_send(rpc_url: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(cast_bin())
        .arg("send")
        .arg("--private-key")
        .arg(FUNDER_PRIV_KEY)
        .arg("--rpc-url")
        .arg(rpc_url)
        .args(args)
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
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

pub async fn mint_evm_test_grant(
    grant_store: &dyn GrantStore,
    wallet: &str,
    action_id: &str,
    action_kind: &str,
    chain_id: u64,
    account: &str,
    now_ms: u64,
) -> Result<SealedApprovalGrant> {
    let expires_ms = now_ms.saturating_add(120_000);
    let header = CanonicalIntentHeader {
        schema: CANONICAL_INTENT_HEADER_SCHEMA_V1.into(),
        wallet: wallet.into(),
        surface: "outbox".into(),
        action_id: action_id.into(),
        petal_id: PETAL_ID_EVM_WALLET.into(),
        petal_digest: PLACEHOLDER_DIGEST_EVM_WALLET.into(),
        petal_version: FIRST_PARTY_PETAL_VERSION_V0.into(),
        executor_kind: ExecutorKind::FirstParty,
        network: format!("eip155:{chain_id}"),
        account: account.into(),
        action_kind: action_kind.into(),
        value_movement: true,
        authority_change: false,
        expires_ms,
    };
    let envelope = CanonicalEnvelope::new(
        header.clone(),
        EVM_SEALED_INTENT_SUBJECT_KIND,
        EVM_SEALED_INTENT_SUBJECT_SCHEMA_V1,
        serde_json::to_vec(&serde_json::json!({
            "schema": "bloom.it.evm_test_grant_subject.v1",
            "wallet": wallet,
            "action_id": action_id,
            "action_kind": action_kind,
            "chain_id": chain_id,
            "account": account,
        }))?,
    );
    let mut terms = DaemonGrantTerms::minimal(AssuranceLevel::Standard);
    terms.allowed_sign_intents = vec![EVM_TX_SIGN_INTENT.into()];
    terms.max_signatures = 1;
    let action = SealedAction::new(
        envelope,
        format!("integration grant for {action_kind} {action_id}"),
        Vec::new(),
        terms,
        PetalPolicySnapshot::minimal(&header),
        now_ms,
    )?;
    grant_store
        .mint(&action, expires_ms, now_ms)
        .await
        .map_err(Into::into)
}
