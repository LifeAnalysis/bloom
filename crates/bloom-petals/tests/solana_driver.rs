//! Host-integration tests for the real Solana driver Petal.
//!
//! These execute the committed content-addressed component through the actual
//! Wasmtime/Petal host path (`PetalStore` → `PetalRunner` → `PetalVm` →
//! `PetalRouter` over the VFS) with a scripted mediated-chain host. They prove:
//!
//! - the Petal-built message is byte-identical to the frozen golden vector
//!   and is accepted by the independent `solana-system-transfer-v1` verifier
//!   (cross-implementation differential through the real execution path);
//! - chain reads flow only through the mediated host interface, exactly once,
//!   on the named profile — and fail closed when denied;
//! - assembly reproduces the exact signed-transaction bytes;
//! - no signing, key-derivation, VFS, or HTTP authority is exercised.

use std::collections::VecDeque;
use std::sync::Arc;

use async_trait::async_trait;
use bloom_petals::{
    ChainRequest, ChainResponse, DenyHost, HostError, PetalHost, PetalRouter, PetalRunner,
    PetalStore, PetalVm,
};
use bloom_vfs::path::VfsPath;
use bloom_vfs::{Handler, Vfs};
use parking_lot::Mutex;
use serde_json::{Value, json};

use bloom_solana::adapter::{
    ADAPTER_SCHEMA, FixtureKeyRef, REQUIRED_OPERATION_CLASS, REQUIRED_SUITE, TransferClaimV1,
    VerifierInputV1,
};
use bloom_solana::golden;
use bloom_solana::{Pubkey, verify_native_transfer};

const PETAL: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../petals/solana-driver");

/// A mediated-chain host: scripted responses, every request recorded. Every
/// other authority (signing, keys, HTTP, VFS) stays default-denied, so any
/// attempt by the component to exercise it fails the test outright.
#[derive(Default)]
struct ScriptedChainHost {
    responses: Mutex<VecDeque<Result<ChainResponse, HostError>>>,
    requests: Mutex<Vec<ChainRequest>>,
}

impl ScriptedChainHost {
    fn with(responses: Vec<Result<ChainResponse, HostError>>) -> Self {
        Self {
            responses: Mutex::new(responses.into_iter().collect()),
            requests: Mutex::default(),
        }
    }
}

#[async_trait]
impl PetalHost for ScriptedChainHost {
    async fn vfs_lookup(&self, _path: &str) -> Result<bloom_petals::HostVfsEntry, HostError> {
        Err(HostError::Denied("no VFS authority".into()))
    }
    async fn vfs_read(&self, _path: &str) -> Result<Vec<u8>, HostError> {
        Err(HostError::Denied("no VFS authority".into()))
    }
    async fn vfs_list(&self, _path: &str) -> Result<Vec<bloom_petals::HostVfsEntry>, HostError> {
        Err(HostError::Denied("no VFS authority".into()))
    }
    async fn vfs_write(&self, _path: &str, _bytes: &[u8]) -> Result<(), HostError> {
        Err(HostError::Denied("no VFS authority".into()))
    }

    async fn chain_read(&self, req: ChainRequest) -> Result<ChainResponse, HostError> {
        self.requests.lock().push(req);
        self.responses
            .lock()
            .pop_front()
            .unwrap_or_else(|| Err(HostError::Denied("chain_read".into())))
    }
}

/// A host that denies everything, including chain reads.
struct AllDenyHost(DenyHost);

#[async_trait]
impl PetalHost for AllDenyHost {
    async fn vfs_lookup(&self, path: &str) -> Result<bloom_petals::HostVfsEntry, HostError> {
        self.0.vfs_lookup(path).await
    }
    async fn vfs_read(&self, path: &str) -> Result<Vec<u8>, HostError> {
        self.0.vfs_read(path).await
    }
    async fn vfs_list(&self, path: &str) -> Result<Vec<bloom_petals::HostVfsEntry>, HostError> {
        self.0.vfs_list(path).await
    }
    async fn vfs_write(&self, path: &str, _bytes: &[u8]) -> Result<(), HostError> {
        self.0.vfs_write(path, &[]).await
    }
}

async fn mount(host: Arc<dyn PetalHost>) -> anyhow::Result<Vfs> {
    let temp = tempfile::tempdir().unwrap();
    let store = PetalStore::open(temp.path().join("petals")).unwrap();
    store.install_petal_package_dir(PETAL).unwrap();
    let registry =
        Arc::new(bloom_petals::NameRegistry::open(temp.path().join("registry")).unwrap());
    let runner = PetalRunner::new(store, registry, PetalVm::new().unwrap());
    // Leak the tempdir: the VFS must outlive this function for the test body.
    std::mem::forget(temp);
    Ok(Vfs::builder()
        .mount("petals", Arc::new(PetalRouter::new(runner, host)))
        .build())
}

async fn write_route(vfs: &Vfs, path: &str, body: Value) -> Result<(), bloom_vfs::HandlerError> {
    let mounted = VfsPath::parse(path).unwrap();
    vfs.write(&mounted, &serde_json::to_vec(&body).unwrap())
        .await
}

async fn read_result(vfs: &Vfs, path: &str) -> Value {
    let mounted = VfsPath::parse(path).unwrap();
    let bytes = vfs.read(&mounted).await.expect("read staged result");
    serde_json::from_slice(&bytes).expect("result is JSON")
}

fn golden_blockhash_hex() -> String {
    hex::encode(golden::blockhash())
}

#[tokio::test]
async fn stage_reproduces_golden_message_and_verifier_accepts() {
    let host = Arc::new(ScriptedChainHost::default());
    let vfs = mount(host).await.unwrap();

    write_route(
        &vfs,
        "/petals/solana-driver/transfer.stage.json",
        json!({
            "chain_profile": "solana-devnet",
            "fee_payer_base58": golden::FEE_PAYER,
            "destination_base58": golden::DESTINATION,
            "lamports": golden::LAMPORTS,
            "blockhash_hex": golden_blockhash_hex(),
        }),
    )
    .await
    .expect("stage succeeds");

    let result = read_result(&vfs, "/petals/solana-driver/transfer.stage.json").await;
    assert_eq!(result["state"], "ok");
    assert_eq!(result["message_hex"], golden::MESSAGE_HEX);
    assert_eq!(result["payload_digest_hex"], golden::MESSAGE_DIGEST_HEX);
    assert_eq!(result["fee_payer_base58"], golden::FEE_PAYER);
    assert_eq!(result["destination_base58"], golden::DESTINATION);
    assert_eq!(result["lamports"], json!(golden::LAMPORTS));
    assert_eq!(result["operation_class"], "solana.native-transfer");
    assert_eq!(result["crypto_suite"], "ed25519-message");

    // Cross-implementation differential: the Anza-built bytes (inside wasm,
    // through the real host path) are accepted by the independent verifier.
    let verified = verify_native_transfer(
        &golden::message_bytes(),
        golden::fee_payer(),
        golden::destination(),
        golden::LAMPORTS,
        Some(golden::message_digest()),
    )
    .unwrap();
    assert_eq!(verified.message_digest, golden::message_digest());
}

#[tokio::test]
async fn stage_performs_exactly_one_mediated_read_on_named_profile() {
    let host = Arc::new(ScriptedChainHost::with(vec![Ok(ChainResponse {
        result_json: json!({
            "blockhash": bs58_of(golden::blockhash()),
            "lastValidBlockHeight": 250u64,
        })
        .to_string(),
    })]));
    let vfs = mount(host.clone()).await.unwrap();

    write_route(
        &vfs,
        "/petals/solana-driver/transfer.stage.json",
        json!({
            "chain_profile": "solana-devnet",
            "fee_payer_base58": golden::FEE_PAYER,
            "destination_base58": golden::DESTINATION,
            "lamports": golden::LAMPORTS,
        }),
    )
    .await
    .expect("stage with mediated blockhash succeeds");

    let result = read_result(&vfs, "/petals/solana-driver/transfer.stage.json").await;
    assert_eq!(result["message_hex"], golden::MESSAGE_HEX);
    assert_eq!(result["last_valid_block_height"], json!(250u64));

    let requests = host.requests.lock();
    assert_eq!(requests.len(), 1, "exactly one mediated read");
    assert_eq!(requests[0].chain, "solana-devnet");
    assert_eq!(requests[0].method, "getLatestBlockhash");
}

fn bs58_of(bytes: [u8; 32]) -> String {
    Pubkey::from_bytes(bytes).to_string()
}

#[tokio::test]
async fn stage_fails_closed_when_chain_read_is_denied() {
    let host = Arc::new(ScriptedChainHost::default());
    let vfs = mount(host.clone()).await.unwrap();

    let err = write_route(
        &vfs,
        "/petals/solana-driver/transfer.stage.json",
        json!({
            "chain_profile": "solana-devnet",
            "fee_payer_base58": golden::FEE_PAYER,
            "destination_base58": golden::DESTINATION,
            "lamports": golden::LAMPORTS,
        }),
    )
    .await
    .expect_err("denied chain read must fail the stage");

    assert!(
        err.to_string().to_lowercase().contains("denied"),
        "expected denial, got: {err}"
    );
    // No partial result was persisted.
    let result = read_result(&vfs, "/petals/solana-driver/transfer.stage.json").await;
    assert_eq!(result["state"], "empty");
    assert_eq!(host.requests.lock().len(), 1);
}

#[tokio::test]
async fn stage_rejects_inconsistent_provider_payloads() {
    // Missing blockhash field.
    let host = Arc::new(ScriptedChainHost::with(vec![Ok(ChainResponse {
        result_json: json!({ "unexpected": true }).to_string(),
    })]));
    let vfs = mount(host).await.unwrap();
    let err = write_route(
        &vfs,
        "/petals/solana-driver/transfer.stage.json",
        json!({
            "chain_profile": "solana-devnet",
            "fee_payer_base58": golden::FEE_PAYER,
            "destination_base58": golden::DESTINATION,
            "lamports": golden::LAMPORTS,
        }),
    )
    .await
    .expect_err("missing blockhash must fail");
    assert!(!err.to_string().is_empty());

    // Non-JSON provider response.
    let host = Arc::new(ScriptedChainHost::with(vec![Ok(ChainResponse {
        result_json: "not json".to_string(),
    })]));
    let vfs = mount(host).await.unwrap();
    write_route(
        &vfs,
        "/petals/solana-driver/transfer.stage.json",
        json!({
            "chain_profile": "solana-devnet",
            "fee_payer_base58": golden::FEE_PAYER,
            "destination_base58": golden::DESTINATION,
            "lamports": golden::LAMPORTS,
        }),
    )
    .await
    .expect_err("garbage provider JSON must fail");

    // Well-formed JSON with an invalid base58 blockhash.
    let host = Arc::new(ScriptedChainHost::with(vec![Ok(ChainResponse {
        result_json: json!({
            "blockhash": "!!not-base58!!",
            "lastValidBlockHeight": 250u64,
        })
        .to_string(),
    })]));
    let vfs = mount(host).await.unwrap();
    write_route(
        &vfs,
        "/petals/solana-driver/transfer.stage.json",
        json!({
            "chain_profile": "solana-devnet",
            "fee_payer_base58": golden::FEE_PAYER,
            "destination_base58": golden::DESTINATION,
            "lamports": golden::LAMPORTS,
        }),
    )
    .await
    .expect_err("invalid base58 blockhash must fail");
}

#[tokio::test]
async fn stage_input_validation() {
    let host = Arc::new(ScriptedChainHost::default());
    let vfs = mount(host).await.unwrap();
    let base = json!({
        "chain_profile": "solana-devnet",
        "fee_payer_base58": golden::FEE_PAYER,
        "destination_base58": golden::DESTINATION,
        "lamports": golden::LAMPORTS,
        "blockhash_hex": golden_blockhash_hex(),
    });

    // Self-transfer.
    let mut req = base.clone();
    req["destination_base58"] = json!(golden::FEE_PAYER);
    write_route(&vfs, "/petals/solana-driver/transfer.stage.json", req)
        .await
        .expect_err("self-transfer rejected");

    // Zero lamports.
    let mut req = base.clone();
    req["lamports"] = json!(0u64);
    write_route(&vfs, "/petals/solana-driver/transfer.stage.json", req)
        .await
        .expect_err("zero lamports rejected");

    // Bad base58 destination.
    let mut req = base.clone();
    req["destination_base58"] = json!("!!!not-base58!!!");
    write_route(&vfs, "/petals/solana-driver/transfer.stage.json", req)
        .await
        .expect_err("bad base58 rejected");

    // Unknown field.
    let mut req = base.clone();
    req["surprise"] = json!(1u64);
    write_route(&vfs, "/petals/solana-driver/transfer.stage.json", req)
        .await
        .expect_err("unknown field rejected");
}

#[tokio::test]
async fn unknown_route_is_not_found() {
    let host = Arc::new(ScriptedChainHost::default());
    let vfs = mount(host).await.unwrap();
    write_route(
        &vfs,
        "/petals/solana-driver/transfer.broadcast.json",
        json!({}),
    )
    .await
    .expect_err("undeclared route is not found");
}

#[tokio::test]
async fn assemble_golden_signature_matches_expected_transaction() {
    let host = Arc::new(ScriptedChainHost::default());
    let vfs = mount(host).await.unwrap();

    write_route(
        &vfs,
        "/petals/solana-driver/transfer.assemble.json",
        json!({
            "message_hex": golden::MESSAGE_HEX,
            "signature_hex": golden::SIGNATURE_HEX,
        }),
    )
    .await
    .expect("assemble succeeds");

    let result = read_result(&vfs, "/petals/solana-driver/transfer.assemble.json").await;
    let mut expected = vec![1u8];
    expected.extend_from_slice(&golden::signature());
    expected.extend_from_slice(&golden::message_bytes());
    assert_eq!(result["transaction_hex"], json!(hex::encode(&expected)));
    assert_eq!(result["payload_digest_hex"], golden::MESSAGE_DIGEST_HEX);
    // The digest commitment matches the actual bytes.
    let tx = hex::decode(result["transaction_hex"].as_str().unwrap()).unwrap();
    assert_eq!(
        result["transaction_digest_hex"],
        json!(hex::encode(bloom_solana::message_digest(&tx)))
    );
}

#[tokio::test]
async fn assemble_rejects_wrong_signature_length_and_bad_hex() {
    let host = Arc::new(ScriptedChainHost::default());
    let vfs = mount(host).await.unwrap();

    let short = hex::encode([0u8; 63]);
    write_route(
        &vfs,
        "/petals/solana-driver/transfer.assemble.json",
        json!({ "message_hex": golden::MESSAGE_HEX, "signature_hex": short }),
    )
    .await
    .expect_err("63-byte signature rejected");

    write_route(
        &vfs,
        "/petals/solana-driver/transfer.assemble.json",
        json!({ "message_hex": "zz", "signature_hex": golden::SIGNATURE_HEX }),
    )
    .await
    .expect_err("non-hex message rejected");
}

#[tokio::test]
async fn adapter_accepts_petal_built_message_behind_fixture_key_ref() {
    // The full static-verifier surface: the Petal's output feeds the versioned
    // adapter (fixture KeyRef standing in for the BIP-39 types) and verifies.
    let host = Arc::new(ScriptedChainHost::default());
    let vfs = mount(host).await.unwrap();
    write_route(
        &vfs,
        "/petals/solana-driver/transfer.stage.json",
        json!({
            "chain_profile": "solana-devnet",
            "fee_payer_base58": golden::FEE_PAYER,
            "destination_base58": golden::DESTINATION,
            "lamports": golden::LAMPORTS,
            "blockhash_hex": golden_blockhash_hex(),
        }),
    )
    .await
    .unwrap();
    let result = read_result(&vfs, "/petals/solana-driver/transfer.stage.json").await;

    let input = VerifierInputV1 {
        schema: ADAPTER_SCHEMA.to_string(),
        operation_class: REQUIRED_OPERATION_CLASS.to_string(),
        crypto_suite: REQUIRED_SUITE.to_string(),
        message_hex: result["message_hex"].as_str().unwrap().to_string(),
        payload_digest_hex: result["payload_digest_hex"].as_str().unwrap().to_string(),
        claim: TransferClaimV1 {
            fee_payer_base58: result["fee_payer_base58"].as_str().unwrap().to_string(),
            destination_base58: result["destination_base58"].as_str().unwrap().to_string(),
            lamports: result["lamports"].as_u64().unwrap(),
        },
        key_ref: FixtureKeyRef {
            backend: "local".to_string(),
            locator: "solana-child-0".to_string(),
            public_key_hex: hex::encode(golden::fee_payer().as_bytes()),
        },
        evidence: None,
    };
    input.validate().unwrap();
    let verified = bloom_solana::adapter::run_verifier(&input).unwrap();
    assert_eq!(verified.verified.destination, golden::DESTINATION);
    assert_eq!(verified.verified.lamports, golden::LAMPORTS);
}

#[tokio::test]
async fn default_deny_host_rejects_every_mediated_read() {
    // The AllDenyHost proves the Petal has no unmediated fallback.
    let host = Arc::new(AllDenyHost(DenyHost));
    let vfs = mount(host).await.unwrap();
    write_route(
        &vfs,
        "/petals/solana-driver/transfer.stage.json",
        json!({
            "chain_profile": "solana-devnet",
            "fee_payer_base58": golden::FEE_PAYER,
            "destination_base58": golden::DESTINATION,
            "lamports": golden::LAMPORTS,
        }),
    )
    .await
    .expect_err("no mediated read, no stage");
}

#[test]
fn committed_artifacts_match_content_addressed_build_manifest() {
    // The committed component and build-manifest pin each other: any drift
    // between them means the package on disk is not what was built.
    let manifest_path = format!("{PETAL}/artifacts/build-manifest.json");
    let manifest_bytes = std::fs::read(&manifest_path).unwrap();
    let manifest: Value = serde_json::from_slice(&manifest_bytes).unwrap();
    assert_eq!(manifest["schema"], "bloom.petal.build-manifest.v1");
    let routes = manifest["routes"].as_array().unwrap();
    assert_eq!(routes.len(), 2, "stage and assemble routes");
    let mut patterns: Vec<&str> = routes
        .iter()
        .map(|r| r["pattern"].as_str().unwrap())
        .collect();
    patterns.sort();
    assert_eq!(
        patterns,
        vec!["transfer.assemble.json", "transfer.stage.json"]
    );
    for route in routes {
        let artifact = format!("{}/{}", PETAL, route["artifact_path"].as_str().unwrap());
        let bytes = std::fs::read(&artifact).unwrap();
        let digest = hex::encode(blake3::hash(&bytes).as_bytes());
        assert_eq!(
            digest,
            route["artifact_hash"].as_str().unwrap(),
            "artifact {} drifted from its recorded digest",
            route["artifact_path"]
        );
    }
}
