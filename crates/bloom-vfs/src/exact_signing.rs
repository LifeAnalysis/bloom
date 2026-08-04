//! Durable Machine orchestration for the existing exact Broker signing flow.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use bloom_broker_api::{
    CryptoSuite, DecimalU64, Digest32, OperationId, ProvenanceCatalog, ProvenanceSubject,
    RequestNonce, Token,
};
use bloom_machine_client::{ExactPayloadSignOutcome, ExactPayloadSignRequest, MachineBrokerClient};
use fs2::FileExt as _;
use rand::RngCore as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

const STATE_SCHEMA: &str = "bloom.machine_exact_signing.v1";
const APPROVAL_TTL_MS: u64 = 5 * 60 * 1000;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone)]
pub struct BrokerExactPayloadSigner {
    broker: MachineBrokerClient,
    provenance_catalog: ProvenanceCatalog,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExactPayloadOutcome {
    ApprovalRequired {
        approval_id: Digest32,
        ceremony_url: String,
        ceremony_expires_at_ms: u64,
    },
    Signed(Vec<u8>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExactSigningState {
    schema: String,
    action_id: String,
    wallet_id: Token,
    operation_class: Token,
    payload_digest: Digest32,
    claimed_hash: Digest32,
    provenance_digest: Digest32,
    approval_operation_id: OperationId,
    signing_operation_id: OperationId,
    request_nonce: RequestNonce,
    issued_at_ms: DecimalU64,
    expires_at_ms: DecimalU64,
    canonical_plan_facts_digest: Digest32,
    approval_id: Option<Digest32>,
}

impl BrokerExactPayloadSigner {
    pub fn new(broker: MachineBrokerClient, provenance_catalog: ProvenanceCatalog) -> Self {
        Self {
            broker,
            provenance_catalog,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn sign_or_prepare(
        &self,
        state_path: &Path,
        action_id: &str,
        wallet: &str,
        operation_class: &str,
        preimage: &[u8],
        claimed_hash: Digest32,
        canonical_plan_facts: &serde_json::Value,
    ) -> Result<ExactPayloadOutcome, String> {
        let parent = state_path
            .parent()
            .ok_or_else(|| "exact signing state path has no parent".to_owned())?;
        fs::create_dir_all(parent)
            .map_err(|error| format!("create exact signing state directory: {error}"))?;
        let lock_path = state_path.with_extension("lock");
        let lock = tokio::task::spawn_blocking(move || {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(lock_path)
                .map_err(|error| format!("open exact signing lock: {error}"))?;
            file.lock_exclusive()
                .map_err(|error| format!("lock exact signing state: {error}"))?;
            Ok::<_, String>(file)
        })
        .await
        .map_err(|error| format!("join exact signing lock task: {error}"))??;

        let result = self
            .sign_or_prepare_locked(
                state_path,
                action_id,
                wallet,
                operation_class,
                preimage,
                claimed_hash,
                canonical_plan_facts,
            )
            .await;
        let _ = lock.unlock();
        result
    }

    #[allow(clippy::too_many_arguments)]
    async fn sign_or_prepare_locked(
        &self,
        state_path: &Path,
        action_id: &str,
        wallet: &str,
        operation_class: &str,
        preimage: &[u8],
        claimed_hash: Digest32,
        canonical_plan_facts: &serde_json::Value,
    ) -> Result<ExactPayloadOutcome, String> {
        let operation_class_token = Token::new(operation_class.to_owned())
            .map_err(|error| format!("operation class: {error}"))?;
        let provenance = self
            .provenance_catalog
            .records
            .iter()
            .find(|record| {
                provenance_operation_class(&record.subject) == Some(operation_class)
                    && record
                        .operation_classes
                        .iter()
                        .any(|entry| entry.operation_class == operation_class_token)
            })
            .ok_or_else(|| format!("installer provenance does not authorize {operation_class}"))?;
        let provenance_digest = provenance
            .digest()
            .map_err(|error| format!("digest installer provenance: {error}"))?;
        let payload_digest = Digest32::from_bytes(Sha256::digest(preimage).into());
        let plan_bytes = serde_jcs::to_vec(canonical_plan_facts)
            .map_err(|error| format!("canonicalize exact signing facts: {error}"))?;
        let canonical_plan_facts_digest = Digest32::from_bytes(Sha256::digest(plan_bytes).into());
        let wallet_id = Token::new(wallet.to_owned()).map_err(|error| error.to_string())?;

        let mut state = match fs::read(state_path) {
            Ok(bytes) => serde_json::from_slice::<ExactSigningState>(&bytes)
                .map_err(|error| format!("read exact signing state: {error}"))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let now = now_ms()?;
                ExactSigningState {
                    schema: STATE_SCHEMA.into(),
                    action_id: action_id.to_owned(),
                    wallet_id: wallet_id.clone(),
                    operation_class: operation_class_token.clone(),
                    payload_digest: payload_digest.clone(),
                    claimed_hash: claimed_hash.clone(),
                    provenance_digest: provenance_digest.clone(),
                    approval_operation_id: random_operation_id(),
                    signing_operation_id: random_operation_id(),
                    request_nonce: random_request_nonce(),
                    issued_at_ms: DecimalU64::new(now),
                    expires_at_ms: DecimalU64::new(now.saturating_add(APPROVAL_TTL_MS)),
                    canonical_plan_facts_digest: canonical_plan_facts_digest.clone(),
                    approval_id: None,
                }
            }
            Err(error) => return Err(format!("read exact signing state: {error}")),
        };
        if state.schema != STATE_SCHEMA
            || state.action_id != action_id
            || state.wallet_id != wallet_id
            || state.operation_class != operation_class_token
            || state.payload_digest != payload_digest
            || state.claimed_hash != claimed_hash
            || state.provenance_digest != provenance_digest
            || state.canonical_plan_facts_digest != canonical_plan_facts_digest
        {
            return Err("exact signing retry differs from its persisted operation identity".into());
        }
        if state.expires_at_ms.get() <= now_ms()? {
            return Err("exact signing approval operation expired; stage a new action".into());
        }
        write_state(state_path, &state)?;
        let request = ExactPayloadSignRequest {
            wallet_id,
            preimage: preimage.to_vec(),
            claimed_hash,
            crypto_suite: CryptoSuite::Secp256k1Keccak256Recoverable,
            provenance: provenance.subject.clone(),
            provenance_digest,
            activation_mode: None,
            approval_operation_id: state.approval_operation_id.clone(),
            signing_operation_id: state.signing_operation_id.clone(),
            request_nonce: state.request_nonce.clone(),
            issued_at_ms: state.issued_at_ms.clone(),
            expires_at_ms: state.expires_at_ms.clone(),
            canonical_plan_facts_digest,
            approval_id: state.approval_id.clone(),
        };
        match self.broker.sign_exact_payload(request).await {
            Ok(ExactPayloadSignOutcome::ApprovalRequired(prepared)) => {
                state.approval_id = Some(prepared.approval_id.clone());
                write_state(state_path, &state)?;
                Ok(ExactPayloadOutcome::ApprovalRequired {
                    approval_id: prepared.approval_id,
                    ceremony_url: prepared.ceremony_url,
                    ceremony_expires_at_ms: prepared.ceremony_expires_at_ms.get(),
                })
            }
            Ok(ExactPayloadSignOutcome::Signed(result)) => {
                let signature = result
                    .signatures
                    .first()
                    .ok_or_else(|| "Broker returned no exact signature".to_owned())?;
                if result.signatures.len() != 1 {
                    return Err("Broker returned an unexpected exact signature count".into());
                }
                Ok(ExactPayloadOutcome::Signed(signature.bytes.decode()))
            }
            Err(error) => Err(error.to_string()),
        }
    }
}

fn provenance_operation_class(subject: &ProvenanceSubject) -> Option<&str> {
    match subject {
        ProvenanceSubject::Cli { command_class, .. } => Some(command_class.as_str()),
        ProvenanceSubject::System {
            operation_class, ..
        } => Some(operation_class.as_str()),
        ProvenanceSubject::Petal { .. } => None,
    }
}

fn random_operation_id() -> OperationId {
    let mut bytes = [0_u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    OperationId::from_bytes(bytes)
}

fn random_request_nonce() -> RequestNonce {
    let mut bytes = [0_u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    RequestNonce::from_bytes(bytes)
}

fn now_ms() -> Result<u64, String> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system clock precedes Unix epoch".to_owned())?;
    u64::try_from(duration.as_millis()).map_err(|_| "system time overflow".to_owned())
}

fn write_state(path: &Path, state: &ExactSigningState) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "exact signing state path has no parent".to_owned())?;
    let temporary = parent.join(format!(
        ".exact-signing.{}.{}.{}.tmp",
        std::process::id(),
        now_ms()?,
        TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let bytes = serde_json::to_vec(state)
        .map_err(|error| format!("encode exact signing state: {error}"))?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .map_err(|error| format!("create exact signing state update: {error}"))?;
    let result = file
        .write_all(&bytes)
        .and_then(|()| file.sync_all())
        .and_then(|()| fs::rename(&temporary, path));
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(|error| format!("commit exact signing state: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use bloom_broker_api::{
        ApprovalPrepareState, Base64UrlBytes, KeyRef, KeySpec, MachineBrokerRequest,
        MachineBrokerResponse, MachineBrokerService, NormalizedSignature,
        PROVENANCE_CATALOG_SCHEMA, ProtocolError, ProtocolErrorCode, ProvenanceOperationClass,
        ProvenanceRecord, SealedApprovalPrepareResponse, ServiceFuture, SigningResult,
        WalletPublic,
    };

    struct MockBroker {
        requests: Mutex<Vec<MachineBrokerRequest>>,
    }

    impl MachineBrokerService for MockBroker {
        fn dispatch<'a>(
            &'a self,
            request: MachineBrokerRequest,
        ) -> ServiceFuture<'a, MachineBrokerResponse> {
            Box::pin(async move {
                self.requests.lock().unwrap().push(request.clone());
                match request {
                    MachineBrokerRequest::WalletGetPublic(_) => {
                        Ok(MachineBrokerResponse::WalletGetPublic(WalletPublic {
                            wallet_id: token("wallet"),
                            wallet_kind: token("local"),
                            key_refs: vec![KeyRef {
                                backend: token("local"),
                                backend_instance: token("primary"),
                                locator: "wallet/root".into(),
                                key_spec: KeySpec::Secp256k1,
                                public_key_fingerprint: digest(3),
                                derivation: None,
                            }],
                            policy_version: DecimalU64::new(1),
                            policy_digest: digest(4),
                            wallet_revocation_epoch: DecimalU64::new(1),
                        }))
                    }
                    MachineBrokerRequest::SealedApprovalPrepare(request) => {
                        Ok(MachineBrokerResponse::SealedApprovalPrepare(
                            SealedApprovalPrepareResponse {
                                approval_id: request.terms.approval_id()?,
                                state: ApprovalPrepareState::AwaitingCeremony,
                                ceremony_url: "http://localhost:18734/ceremony/test".into(),
                                ceremony_expires_at_ms: request.terms.expires_at_ms,
                                review_manifest_digest: digest(8),
                            },
                        ))
                    }
                    MachineBrokerRequest::SigningSign(request) => {
                        Ok(MachineBrokerResponse::SigningSign(SigningResult {
                            operation_id: request.operation_id,
                            operation_digest: request.operation_digest,
                            signatures: vec![NormalizedSignature {
                                crypto_suite: request.crypto_suite,
                                bytes: Base64UrlBytes::from_bytes(&[7_u8; 65]),
                            }],
                            signer_receipt_digest: digest(9),
                            broker_receipt_digest: digest(10),
                        }))
                    }
                    _ => Err(ProtocolError::new(
                        ProtocolErrorCode::UnknownMethod,
                        "unexpected request",
                    )),
                }
            })
        }
    }

    #[tokio::test]
    async fn persists_identity_reuses_approval_and_rejects_payload_drift() {
        let broker = Arc::new(MockBroker {
            requests: Mutex::new(Vec::new()),
        });
        let signer = BrokerExactPayloadSigner::new(
            MachineBrokerClient::new(broker.clone()),
            ProvenanceCatalog {
                schema: PROVENANCE_CATALOG_SCHEMA.into(),
                records: vec![ProvenanceRecord {
                    subject: ProvenanceSubject::System {
                        component_id: token("bloom-machine"),
                        operation_class: token("transaction.confirm"),
                    },
                    publisher: token("bloom-installer"),
                    operation_classes: vec![ProvenanceOperationClass {
                        operation_class: token("transaction.confirm"),
                        fee_asset: None,
                    }],
                    installer_key_id: token("test-key"),
                    installer_signature: Base64UrlBytes::from_bytes(&[]),
                }],
            },
        );
        let temporary = tempfile::tempdir().unwrap();
        let state = temporary.path().join("exact.json");
        let payload = b"exact transaction bytes";
        let hash = Digest32::from_bytes(alloy::primitives::keccak256(payload).into());
        let first = signer
            .sign_or_prepare(
                &state,
                "action-1",
                "wallet",
                "transaction.confirm",
                payload,
                hash.clone(),
                &serde_json::json!({"amount": "1"}),
            )
            .await
            .unwrap();
        assert!(matches!(
            first,
            ExactPayloadOutcome::ApprovalRequired { .. }
        ));
        let second = signer
            .sign_or_prepare(
                &state,
                "action-1",
                "wallet",
                "transaction.confirm",
                payload,
                hash,
                &serde_json::json!({"amount": "1"}),
            )
            .await
            .unwrap();
        assert_eq!(second, ExactPayloadOutcome::Signed(vec![7_u8; 65]));
        let requests_after_sign = broker.requests.lock().unwrap().len();
        let error = signer
            .sign_or_prepare(
                &state,
                "action-1",
                "wallet",
                "transaction.confirm",
                b"altered",
                Digest32::from_bytes(alloy::primitives::keccak256(b"altered").into()),
                &serde_json::json!({"amount": "1"}),
            )
            .await
            .unwrap_err();
        assert!(error.contains("differs from its persisted operation identity"));
        assert_eq!(broker.requests.lock().unwrap().len(), requests_after_sign);
    }

    fn token(value: &str) -> Token {
        Token::new(value).unwrap()
    }

    fn digest(byte: u8) -> Digest32 {
        Digest32::from_bytes([byte; 32])
    }
}
