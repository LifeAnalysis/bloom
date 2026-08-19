//! End-to-end transfer lifecycle: stage → sign → broadcast, driven by a stub
//! Solana RPC node and a real-Ed25519 Broker fixture.

use std::sync::Arc;

use bloom_broker_api::{
    ApprovalPrepareRequest, ApprovalPrepareState, Base64UrlBytes, CryptoSuite, DecimalU64,
    Digest32, KeyPublic, KeyRef, KeyRequest, KeyRole, KeySpec, MachineBrokerRequest,
    MachineBrokerResponse, MachineBrokerService, NormalizedSignature, ProtocolError,
    ProtocolErrorCode, ProvenanceCatalog, ProvenanceOperationClass, ProvenanceRecord,
    ProvenanceSubject, SealedApprovalPrepareResponse, ServiceFuture, SigningPayloads,
    SigningResult, Token, WalletPublic, WalletRequest,
};
use bloom_machine_client::MachineBrokerClient;
use bloom_solana::{EndpointSpec, SolanaClient, SolanaSpec};
use bloom_solana_tx::engine::SolanaTransferEngine;
use bloom_solana_tx::outbox::{SolanaOutbox, SolanaOutboxState};
use bloom_solana_tx::signing::SolanaTransferSigner;
use bloom_solana_tx::types::SolanaTxStatus;
use sha2::{Digest as _, Sha256};

fn token(s: &str) -> Token {
    Token::new(s).unwrap()
}
fn digest(byte: u8) -> Digest32 {
    Digest32::from_bytes([byte; 32])
}

struct BrokerFixture {
    child_signing_key: ed25519_dalek::SigningKey,
    child_key_ref: KeyRef,
}

impl BrokerFixture {
    fn new() -> Self {
        let child_signing_key = ed25519_dalek::SigningKey::from_bytes(&[0xaa; 32]);
        let pubkey = child_signing_key.verifying_key().to_bytes();
        Self {
            child_signing_key,
            child_key_ref: KeyRef {
                backend: token("local"),
                backend_instance: token("primary"),
                locator: "wallet/derived/solana-0".into(),
                key_spec: KeySpec::Ed25519,
                public_key_fingerprint: Digest32::from_bytes(Sha256::digest(pubkey).into()),
                derivation: None,
            },
        }
    }
    fn child_pubkey(&self) -> [u8; 32] {
        self.child_signing_key.verifying_key().to_bytes()
    }
}

impl MachineBrokerService for BrokerFixture {
    fn dispatch<'a>(
        &'a self,
        request: MachineBrokerRequest,
    ) -> ServiceFuture<'a, MachineBrokerResponse> {
        Box::pin(async move {
            match request {
                MachineBrokerRequest::WalletGetPublic(WalletRequest { wallet_id }) => {
                    Ok(MachineBrokerResponse::WalletGetPublic(WalletPublic {
                        wallet_id,
                        wallet_kind: token("local"),
                        root_key_ref: None,
                        key_refs: vec![self.child_key_ref.clone()],
                        policy_version: DecimalU64::new(1),
                        policy_digest: digest(1),
                        wallet_revocation_epoch: DecimalU64::new(1),
                    }))
                }
                MachineBrokerRequest::KeyGetPublic(KeyRequest { key_ref }) => {
                    Ok(MachineBrokerResponse::KeyGetPublic(KeyPublic {
                        role: KeyRole::Derived,
                        key_ref,
                        canonical_public_key: Base64UrlBytes::from_bytes(&self.child_pubkey()),
                        addresses: vec![],
                        supported_crypto_suites: vec![CryptoSuite::Ed25519Message],
                    }))
                }
                MachineBrokerRequest::SigningSign(sign_request) => {
                    let SigningPayloads::Single { payload } = &sign_request.payloads else {
                        return Err(ProtocolError::new(
                            ProtocolErrorCode::MalformedFrame,
                            "expected single payload",
                        ));
                    };
                    use ed25519_dalek::Signer as _;
                    let signature = self.child_signing_key.sign(payload.decode().as_slice());
                    Ok(MachineBrokerResponse::SigningSign(SigningResult {
                        operation_id: sign_request.operation_id,
                        operation_digest: sign_request.operation_digest,
                        signatures: vec![NormalizedSignature {
                            crypto_suite: CryptoSuite::Ed25519Message,
                            bytes: Base64UrlBytes::from_bytes(&signature.to_bytes()),
                        }],
                        signer_receipt_digest: digest(90),
                        broker_receipt_digest: digest(91),
                    }))
                }
                MachineBrokerRequest::SealedApprovalPrepare(ApprovalPrepareRequest {
                    terms,
                    ..
                }) => Ok(MachineBrokerResponse::SealedApprovalPrepare(
                    SealedApprovalPrepareResponse {
                        approval_id: terms.approval_id().unwrap_or_else(|_| digest(7)),
                        state: ApprovalPrepareState::AwaitingCeremony,
                        ceremony_url: "http://localhost:18734/ceremony".into(),
                        ceremony_expires_at_ms: terms.expires_at_ms,
                        review_manifest_digest: digest(92),
                    },
                )),
                other => Err(ProtocolError::new(
                    ProtocolErrorCode::UnknownMethod,
                    format!("unhandled {other:?}"),
                )),
            }
        })
    }
}

fn catalog() -> ProvenanceCatalog {
    ProvenanceCatalog {
        schema: bloom_broker_api::PROVENANCE_CATALOG_SCHEMA.into(),
        records: vec![ProvenanceRecord {
            subject: ProvenanceSubject::System {
                component_id: token("bloom-machine"),
                operation_class: token("solana.transfer.confirm"),
            },
            publisher: token("bloom-installer"),
            petal_lineage: None,
            operation_classes: vec![ProvenanceOperationClass {
                operation_class: token("solana.native-transfer"),
                fee_asset: Some(bloom_broker_api::ProvenanceFeeAsset {
                    chain: token("solana"),
                    asset: "native".into(),
                }),
            }],
            installer_key_id: token("installer-key"),
            installer_signature: Base64UrlBytes::from_bytes(&[11; 64]),
        }],
    }
}

/// A stub Solana JSON-RPC node answering blockhash + sendTransaction.
async fn spawn_node() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = vec![0u8; 8192];
                let n = socket.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]).to_string();
                let body = request.split("\r\n\r\n").nth(1).unwrap_or_default();
                let method = serde_json::from_str::<serde_json::Value>(body)
                    .ok()
                    .and_then(|v| v.get("method").and_then(|m| m.as_str()).map(String::from))
                    .unwrap_or_default();
                let result = match method.as_str() {
                    "getLatestBlockhash" => {
                        let blockhash = bs58::encode([0x42u8; 32]).into_string();
                        format!(
                            r#"{{"context":{{"slot":1}},"value":{{"blockhash":"{blockhash}","lastValidBlockHeight":100}}}}"#
                        )
                    }
                    "sendTransaction" => {
                        r#""4Nd1mBQtrMJVYVfKf2PJy9NZUZdTAsp7D4xWLs4gDB4TdSXKZT9HYqjs""#.to_string()
                    }
                    _ => r#"{"code":-32601,"message":"method not found"}"#.to_string(),
                };
                let payload = format!(r#"{{"jsonrpc":"2.0","id":1,"result":{result}}}"#);
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    payload.len(),
                    payload
                );
                let _ = socket.write_all(response.as_bytes()).await;
            });
        }
    });
    format!("http://{addr}/")
}

fn client(endpoint: &str) -> SolanaClient {
    SolanaClient::build(&SolanaSpec {
        name: "solana-devnet".into(),
        endpoints: vec![EndpointSpec {
            url: endpoint.to_string(),
            weight: 100,
            cu_per_sec: None,
            max_rps: None,
            http_only: false,
        }],
        expected_genesis_hex: None,
        allow_broadcast: true,
    })
    .unwrap()
}

#[tokio::test]
async fn full_transfer_lifecycle_stage_sign_broadcast() {
    let endpoint = spawn_node().await;
    let dir = tempfile::tempdir().unwrap();
    let outbox = SolanaOutbox::new(dir.path().join("outbox")).unwrap();
    let broker = Arc::new(BrokerFixture::new());
    let signer =
        SolanaTransferSigner::from_catalog(MachineBrokerClient::new(broker.clone()), &catalog())
            .unwrap();
    let engine =
        SolanaTransferEngine::new(outbox.clone(), client(&endpoint), signer, "solana-devnet");

    let fee_payer = broker.child_pubkey();
    let destination = ed25519_dalek::SigningKey::from_bytes(&[0xbb; 32])
        .verifying_key()
        .to_bytes();

    // Stage.
    let staged = engine
        .stage("wallet", &fee_payer, &destination, 1_000_000, 1_000)
        .await
        .unwrap();
    assert_eq!(staged.status, SolanaTxStatus::Pending);
    assert_eq!(staged.lamports, 1_000_000);
    assert_eq!(staged.blockhash, bs58::encode([0x42u8; 32]).into_string());

    // First sign attempt prepares the ceremony (no approval id yet).
    let first = engine
        .sign("wallet", &staged.id, &fee_payer, None, 1_100)
        .await
        .unwrap();
    let approval_id = match first {
        bloom_solana_tx::signing::SolanaSignOutcome::ApprovalRequired { approval_id, .. } => {
            approval_id
        }
        other => panic!("expected ApprovalRequired, got {other:?}"),
    };
    // Still pending: no signature recorded yet.
    assert!(
        outbox
            .read_in_state(
                "wallet",
                "solana-devnet",
                &staged.id,
                SolanaOutboxState::Pending
            )
            .is_ok()
    );

    // Retry with the approval id: signs and moves to sent.
    let signed = engine
        .sign("wallet", &staged.id, &fee_payer, Some(approval_id), 1_200)
        .await
        .unwrap();
    assert!(matches!(
        signed,
        bloom_solana_tx::signing::SolanaSignOutcome::Signed { .. }
    ));
    let sent = outbox
        .read_in_state(
            "wallet",
            "solana-devnet",
            &staged.id,
            SolanaOutboxState::Sent,
        )
        .unwrap();
    assert!(sent.staged.signature.is_some());

    // Broadcast: submits the assembled transaction and records the attempt.
    let signature = engine.broadcast("wallet", &staged.id, 1_300).await.unwrap();
    assert_eq!(
        signature,
        "4Nd1mBQtrMJVYVfKf2PJy9NZUZdTAsp7D4xWLs4gDB4TdSXKZT9HYqjs"
    );
    // The broadcast attempt marker is recorded next to the sent entry.
    assert!(
        sent.dir
            .join(bloom_solana_tx::outbox::BROADCAST_ATTEMPT_FILE)
            .exists()
    );
}

#[tokio::test]
async fn broadcast_refuses_when_operator_disables_it() {
    let endpoint = spawn_node().await;
    let dir = tempfile::tempdir().unwrap();
    let outbox = SolanaOutbox::new(dir.path().join("outbox")).unwrap();
    let broker = Arc::new(BrokerFixture::new());
    let signer =
        SolanaTransferSigner::from_catalog(MachineBrokerClient::new(broker.clone()), &catalog())
            .unwrap();
    let mut spec = SolanaSpec {
        name: "solana-devnet".into(),
        endpoints: vec![EndpointSpec {
            url: endpoint,
            weight: 100,
            cu_per_sec: None,
            max_rps: None,
            http_only: false,
        }],
        expected_genesis_hex: None,
        allow_broadcast: false,
    };
    spec.allow_broadcast = false;
    let client = SolanaClient::build(&spec).unwrap();
    let engine = SolanaTransferEngine::new(outbox, client, signer, "solana-devnet");

    // The broadcast gate is the operator's release posture: it fires before
    // any outbox lookup, so even a valid path is refused.
    let err = engine
        .broadcast("wallet", "0001-00001", 1_000)
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            bloom_solana_tx::engine::EngineError::BroadcastDisabled(_)
        ),
        "{err}"
    );
}
