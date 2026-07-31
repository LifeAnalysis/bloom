//! Machine-owned, keyless client surface for the Broker.
//!
//! This crate intentionally knows only the public Machine↔Broker protocol. It
//! contains no private-key, WKEK, PRF, provider-credential, or custody
//! plaintext type.

#![forbid(unsafe_code)]

mod projection;

pub use projection::{
    CachedWalletProjectionReader, FileProjectionStore, ProjectionFreshness, ProjectionVerification,
    WalletProjection, WalletProjectionReader,
};

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use bloom_triad_local_transport::{LocalIdentity, PeerAcl};
use bloom_triad_protocol::{
    ActivationMode, ApprovalLifecycleState, ApprovalLimits, ApprovalPrepareRequest,
    ApprovalPublicStatus, ApprovalSelector, ApprovalSubject, Base64UrlBytes, CeremonyPublicStatus,
    CeremonyState, CredentialPublic, CryptoSuite, CustodyPrepareRequest, CustodyPrepareResponse,
    CustodyResult, DecimalU64, Digest32, IdRequest, KeyPublic, KeyRef, KeyRequest,
    MachineBrokerRequest, MachineBrokerResponse, MachineBrokerService, MachineSignRequest,
    OperationId, OperationPublicStatus, OperationRequest, PetalUseClaim, PolicyCommitReceipt,
    PolicyCommitUpdateRequest, PolicyUpdatePrepareResponse, PolicyUpdateRequest, ProtocolError,
    ProtocolErrorCode, ProvenanceCatalog, ProvenanceSubject, RequestNonce,
    SealedApprovalPrepareResponse, SealedApprovalTerms, SignOperationIdentity,
    SignedPolicySnapshot, SigningPayloads, SigningResult, Token, WalletPublic, WalletRequest,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use sha3::Keccak256;

/// Production Machine→Broker connector. It carries only the public typed
/// protocol over mutually authenticated, signed, bounded Unix socket frames.
#[derive(Clone)]
pub struct UnixMachineBrokerService {
    socket_path: PathBuf,
    identity: LocalIdentity,
    broker: PeerAcl,
}

impl UnixMachineBrokerService {
    pub fn new(socket_path: impl Into<PathBuf>, identity: LocalIdentity, broker: PeerAcl) -> Self {
        Self {
            socket_path: socket_path.into(),
            identity,
            broker,
        }
    }
}

impl MachineBrokerService for UnixMachineBrokerService {
    fn dispatch<'a>(
        &'a self,
        request: MachineBrokerRequest,
    ) -> bloom_triad_protocol::ServiceFuture<'a, MachineBrokerResponse> {
        Box::pin(async move {
            let mut stream = tokio::net::UnixStream::connect(&self.socket_path)
                .await
                .map_err(|error| service_unavailable(format!("connect Broker: {error}")))?;
            bloom_triad_local_transport::call(
                &mut stream,
                &self.identity,
                &self.broker,
                request,
                30_000,
            )
            .await
        })
    }
}

#[derive(Clone)]
pub struct MachineBrokerClient {
    service: Arc<dyn MachineBrokerService>,
}

impl MachineBrokerClient {
    pub fn new(service: Arc<dyn MachineBrokerService>) -> Self {
        Self { service }
    }

    pub fn connect_unix(
        socket_path: impl Into<PathBuf>,
        identity: LocalIdentity,
        broker: PeerAcl,
    ) -> Self {
        Self::new(Arc::new(UnixMachineBrokerService::new(
            socket_path,
            identity,
            broker,
        )))
    }

    /// Loads the Machine application identity and the root-owned edge manifest
    /// without permitting an unauthenticated transport fallback.
    pub fn connect_unix_from_files(
        socket_path: impl Into<PathBuf>,
        identity_path: impl AsRef<Path>,
        edge_manifest_path: impl AsRef<Path>,
    ) -> Result<Self, ProtocolError> {
        let identity_path = identity_path.as_ref();
        let manifest_path = edge_manifest_path.as_ref();
        let (identity, manifest) = bloom_triad_local_transport::load_identity_and_manifest(
            identity_path,
            manifest_path,
            "bloom-machine",
        )?;
        let machine = manifest.machine.into_acl()?;
        let broker = manifest.broker.into_acl()?;
        if machine.service_id != identity.service_id
            || machine.boot_epoch != identity.boot_epoch
            || machine.application_key_id != identity.application_key_id
            || machine.application_public_key != identity.signing_key.verifying_key().to_bytes()
        {
            return Err(ProtocolError::new(
                ProtocolErrorCode::UnauthenticatedPeer,
                "Machine identity does not match the pinned edge manifest",
            ));
        }
        if broker.service_id.as_str() != "bloom-broker" {
            return Err(ProtocolError::new(
                ProtocolErrorCode::UnauthenticatedPeer,
                "edge manifest Broker service ID is invalid",
            ));
        }
        Ok(Self::connect_unix(socket_path, identity, broker))
    }

    pub async fn request(
        &self,
        request: MachineBrokerRequest,
    ) -> Result<MachineBrokerResponse, ProtocolError> {
        self.service.dispatch(request).await
    }

    pub async fn sign(&self, request: MachineSignRequest) -> Result<SigningResult, ProtocolError> {
        let expected = ExpectedSigningResult::from_request(&request);
        match self
            .request(MachineBrokerRequest::SigningSign(request))
            .await?
        {
            MachineBrokerResponse::SigningSign(result) => expected.validate(result),
            _ => Err(response_mismatch("signing.sign")),
        }
    }

    pub async fn sign_batch(
        &self,
        request: MachineSignRequest,
    ) -> Result<SigningResult, ProtocolError> {
        let expected = ExpectedSigningResult::from_request(&request);
        match self
            .request(MachineBrokerRequest::SigningSignBatch(request))
            .await?
        {
            MachineBrokerResponse::SigningSignBatch(result) => expected.validate(result),
            _ => Err(response_mismatch("signing.sign_batch")),
        }
    }

    /// Validate and translate a payload-bearing Petal request. Provenance is
    /// supplied independently by the trusted runner, never copied from guest
    /// fields without comparison.
    pub async fn sign_petal_payload(
        &self,
        request: TrustedPetalSignRequest,
    ) -> Result<SigningResult, ProtocolError> {
        self.sign_petal_payload_for_key(request, None).await
    }

    /// Sign a validated payload-bearing Petal request with an explicitly
    /// selected Signer-owned delegated key over the existing `signing.sign`
    /// method. The public key projection is fetched from Broker so Machine
    /// never infers delegated-key suite support locally.
    pub async fn sign_petal_payload_with_key(
        &self,
        request: TrustedPetalSignRequest,
        key_ref: KeyRef,
    ) -> Result<SigningResult, ProtocolError> {
        self.sign_petal_payload_for_key(request, Some(key_ref))
            .await
    }

    async fn sign_petal_payload_for_key(
        &self,
        request: TrustedPetalSignRequest,
        selected_key_ref: Option<KeyRef>,
    ) -> Result<SigningResult, ProtocolError> {
        request.validate()?;
        if request.selector == bloom_triad_protocol::PetalSignSelector::Exact
            && selected_key_ref.is_none()
        {
            return Err(ProtocolError::new(
                ProtocolErrorCode::KeyrefMismatch,
                "exact Petal signing requires an explicit Signer-owned KeyRef",
            ));
        }
        let wallet = self.wallet(request.wallet_id.clone()).await?;
        let key_ref = match selected_key_ref {
            Some(key_ref) => {
                let key = self
                    .key(KeyRequest {
                        key_ref: key_ref.clone(),
                    })
                    .await?;
                if key.key_ref != key_ref {
                    return Err(ProtocolError::new(
                        ProtocolErrorCode::KeyrefMismatch,
                        "Broker returned public metadata for a different delegated key",
                    ));
                }
                if !key.supported_crypto_suites.contains(&request.crypto_suite) {
                    return Err(ProtocolError::new(
                        ProtocolErrorCode::SuiteNotAllowed,
                        "selected delegated key does not support the requested CryptoSuite",
                    ));
                }
                key_ref
            }
            None => unique_key_for_suite(&wallet.key_refs, request.crypto_suite)?,
        };
        let payload_digest = Digest32::from_bytes(Sha256::digest(&request.preimage).into());
        let ordered_hash = suite_hash(request.crypto_suite, &request.preimage);
        let ProvenanceSubject::Petal {
            package_hash,
            route,
        } = &request.trusted_provenance
        else {
            return Err(ProtocolError::new(
                ProtocolErrorCode::ProvenanceMismatch,
                "Petal signing requires trusted Petal provenance",
            ));
        };
        if request.claimed_hash != ordered_hash
            || &request.claim.package_hash != package_hash
            || &request.claim.route != route
            || request.claim.operation_class != request.operation_class
            || request.claim.crypto_suite != request.crypto_suite
            || request.claim.payload_digest != payload_digest
            || request.claim.ordered_hashes != [ordered_hash.clone()]
        {
            return Err(ProtocolError::new(
                ProtocolErrorCode::ClaimInvalid,
                "payload, hash, operation class, or trusted Petal provenance differs from claim",
            ));
        }
        let approval_id = request.approval_id.clone().ok_or_else(|| {
            ProtocolError::new(
                ProtocolErrorCode::ApprovalNotFound,
                "payload signing requires an approval hint",
            )
        })?;
        let operation_id = request.operation_id()?;
        let (claim_digest, assurance_digest, petal_use_claim, claim_assurance_evidence) =
            match request.selector {
                bloom_triad_protocol::PetalSignSelector::Exact => (None, None, None, None),
                bloom_triad_protocol::PetalSignSelector::Reusable => (
                    Some(jcs_digest(&request.claim)?),
                    Some(jcs_digest(&request.claim.claim_assurance)?),
                    Some(request.claim),
                    request
                        .claim_assurance_evidence
                        .as_deref()
                        .map(Base64UrlBytes::from_bytes),
                ),
            };
        let operation_digest = SignOperationIdentity {
            operation_id: operation_id.clone(),
            approval_id: approval_id.clone(),
            key_ref: key_ref.clone(),
            crypto_suite: request.crypto_suite,
            ordered_payload_digests: vec![payload_digest],
            ordered_hashes: vec![ordered_hash],
            petal_use_claim_digest: claim_digest,
            claim_assurance_digest: assurance_digest,
            policy_version: wallet.policy_version,
            policy_digest: wallet.policy_digest,
        }
        .digest()?;
        self.sign(MachineSignRequest {
            operation_id,
            operation_digest,
            approval_id,
            key_ref,
            crypto_suite: request.crypto_suite,
            payloads: SigningPayloads::Single {
                payload: Base64UrlBytes::from_bytes(&request.preimage),
            },
            petal_use_claim,
            claim_assurance_evidence,
            provenance: request.trusted_provenance,
        })
        .await
    }

    /// Prepare or execute one exact payload-bearing Machine/CLI operation.
    ///
    /// The caller persists the returned approval ID and reuses the exact
    /// immutable request identities after the ceremony. No hash-only fallback
    /// exists: both the payload bytes and the suite-derived hash are bound into
    /// the approval and sign operation.
    pub async fn sign_exact_payload(
        &self,
        request: ExactPayloadSignRequest,
    ) -> Result<ExactPayloadSignOutcome, ProtocolError> {
        request.validate()?;
        let wallet = self.wallet(request.wallet_id.clone()).await?;
        let key_ref = unique_key_for_suite(&wallet.key_refs, request.crypto_suite)?;
        let activation_mode = request
            .activation_mode
            .clone()
            .unwrap_or_else(|| default_activation_mode(&key_ref));
        let payload_digest = Digest32::from_bytes(Sha256::digest(&request.preimage).into());
        let ordered_hash = suite_hash(request.crypto_suite, &request.preimage);
        if request.claimed_hash != ordered_hash {
            return Err(ProtocolError::new(
                ProtocolErrorCode::SelectorMismatch,
                "exact payload hash does not match the selected CryptoSuite",
            ));
        }

        if let Some(approval_id) = request.approval_id {
            let operation_digest = SignOperationIdentity {
                operation_id: request.signing_operation_id.clone(),
                approval_id: approval_id.clone(),
                key_ref: key_ref.clone(),
                crypto_suite: request.crypto_suite,
                ordered_payload_digests: vec![payload_digest],
                ordered_hashes: vec![ordered_hash],
                petal_use_claim_digest: None,
                claim_assurance_digest: None,
                policy_version: wallet.policy_version,
                policy_digest: wallet.policy_digest,
            }
            .digest()?;
            return self
                .sign(MachineSignRequest {
                    operation_id: request.signing_operation_id,
                    operation_digest,
                    approval_id,
                    key_ref,
                    crypto_suite: request.crypto_suite,
                    payloads: SigningPayloads::Single {
                        payload: Base64UrlBytes::from_bytes(&request.preimage),
                    },
                    petal_use_claim: None,
                    claim_assurance_evidence: None,
                    provenance: request.provenance,
                })
                .await
                .map(ExactPayloadSignOutcome::Signed);
        }

        let terms = SealedApprovalTerms {
            subject: approval_subject(&request.provenance),
            wallet_id: request.wallet_id,
            key_ref,
            allowed_crypto_suites: vec![request.crypto_suite],
            selector: ApprovalSelector::Exact {
                ordered_payload_digests: vec![payload_digest],
                ordered_hashes: vec![ordered_hash],
            },
            limits: ApprovalLimits {
                max_operations: DecimalU64::new(1),
                max_signatures: DecimalU64::new(1),
                operation_rate_limits: Vec::new(),
                signature_rate_limits: Vec::new(),
                value_limits: Vec::new(),
            },
            activation_mode,
            wallet_revocation_epoch: wallet.wallet_revocation_epoch,
            policy_version: wallet.policy_version,
            policy_digest: wallet.policy_digest,
            provenance_digest: request.provenance_digest,
            request_nonce: request.request_nonce,
            issued_at_ms: request.issued_at_ms.clone(),
            not_before_ms: request.issued_at_ms,
            expires_at_ms: request.expires_at_ms,
            renewal_of: None,
        };
        terms.validate()?;
        self.prepare_approval(ApprovalPrepareRequest {
            operation_id: request.approval_operation_id,
            terms,
            canonical_plan_facts_digest: request.canonical_plan_facts_digest,
        })
        .await
        .map(ExactPayloadSignOutcome::ApprovalRequired)
    }

    pub async fn prepare_approval(
        &self,
        request: ApprovalPrepareRequest,
    ) -> Result<SealedApprovalPrepareResponse, ProtocolError> {
        match self
            .request(MachineBrokerRequest::SealedApprovalPrepare(request))
            .await?
        {
            MachineBrokerResponse::SealedApprovalPrepare(response) => Ok(response),
            _ => Err(response_mismatch("sealed_approval.prepare")),
        }
    }

    pub async fn approval_status(
        &self,
        approval_id: Digest32,
    ) -> Result<ApprovalPublicStatus, ProtocolError> {
        match self
            .request(MachineBrokerRequest::SealedApprovalStatus(IdRequest {
                id: approval_id,
            }))
            .await?
        {
            MachineBrokerResponse::SealedApprovalStatus(status) => Ok(status),
            _ => Err(response_mismatch("sealed_approval.status")),
        }
    }

    pub async fn operation_status(
        &self,
        operation_id: OperationId,
    ) -> Result<OperationPublicStatus, ProtocolError> {
        match self
            .request(MachineBrokerRequest::OperationStatus(OperationRequest {
                operation_id,
            }))
            .await?
        {
            MachineBrokerResponse::OperationStatus(status) => Ok(status),
            _ => Err(response_mismatch("operation.status")),
        }
    }

    pub async fn policy(&self, wallet_id: Token) -> Result<SignedPolicySnapshot, ProtocolError> {
        match self
            .request(MachineBrokerRequest::PolicyRead(WalletRequest {
                wallet_id,
            }))
            .await?
        {
            MachineBrokerResponse::PolicyRead(policy) => Ok(policy),
            _ => Err(response_mismatch("policy.read")),
        }
    }

    pub async fn validate_policy_update(
        &self,
        request: PolicyUpdateRequest,
    ) -> Result<PolicyUpdatePrepareResponse, ProtocolError> {
        match self
            .request(MachineBrokerRequest::PolicyValidateUpdate(request))
            .await?
        {
            MachineBrokerResponse::PolicyValidateUpdate(response) => Ok(response),
            _ => Err(response_mismatch("policy.validate_update")),
        }
    }

    pub async fn commit_policy_update(
        &self,
        request: PolicyCommitUpdateRequest,
    ) -> Result<PolicyCommitReceipt, ProtocolError> {
        match self
            .request(MachineBrokerRequest::PolicyCommitUpdate(request))
            .await?
        {
            MachineBrokerResponse::PolicyCommitUpdate(receipt) => Ok(receipt),
            _ => Err(response_mismatch("policy.commit_update")),
        }
    }

    pub async fn ceremony_status(
        &self,
        operation_id: OperationId,
    ) -> Result<CeremonyPublicStatus, ProtocolError> {
        let id = Digest32::new(operation_id.as_str().to_owned())?;
        match self
            .request(MachineBrokerRequest::CeremonyStatus(IdRequest { id }))
            .await?
        {
            MachineBrokerResponse::CeremonyStatus(status) => Ok(status),
            _ => Err(response_mismatch("ceremony.status")),
        }
    }

    pub async fn cancel_ceremony(
        &self,
        operation_id: OperationId,
    ) -> Result<CeremonyPublicStatus, ProtocolError> {
        let id = Digest32::new(operation_id.as_str().to_owned())?;
        match self
            .request(MachineBrokerRequest::CeremonyCancel(IdRequest { id }))
            .await?
        {
            MachineBrokerResponse::CeremonyCancel(status) => Ok(status),
            _ => Err(response_mismatch("ceremony.cancel")),
        }
    }

    pub async fn wallets(&self) -> Result<Vec<WalletPublic>, ProtocolError> {
        match self
            .request(MachineBrokerRequest::WalletListPublic(
                bloom_triad_protocol::Empty {},
            ))
            .await?
        {
            MachineBrokerResponse::WalletListPublic(wallets) => Ok(wallets),
            _ => Err(response_mismatch("wallet.list_public")),
        }
    }

    pub async fn wallet(&self, wallet_id: Token) -> Result<WalletPublic, ProtocolError> {
        match self
            .request(MachineBrokerRequest::WalletGetPublic(WalletRequest {
                wallet_id,
            }))
            .await?
        {
            MachineBrokerResponse::WalletGetPublic(wallet) => Ok(wallet),
            _ => Err(response_mismatch("wallet.get_public")),
        }
    }

    pub async fn keys(&self, wallet_id: Token) -> Result<Vec<KeyPublic>, ProtocolError> {
        match self
            .request(MachineBrokerRequest::KeyListPublic(WalletRequest {
                wallet_id,
            }))
            .await?
        {
            MachineBrokerResponse::KeyListPublic(keys) => Ok(keys),
            _ => Err(response_mismatch("key.list_public")),
        }
    }

    pub async fn key(&self, request: KeyRequest) -> Result<KeyPublic, ProtocolError> {
        match self
            .request(MachineBrokerRequest::KeyGetPublic(request))
            .await?
        {
            MachineBrokerResponse::KeyGetPublic(key) => Ok(key),
            _ => Err(response_mismatch("key.get_public")),
        }
    }

    pub async fn credentials(
        &self,
        wallet_id: Token,
    ) -> Result<Vec<CredentialPublic>, ProtocolError> {
        match self
            .request(MachineBrokerRequest::CredentialListPublic(WalletRequest {
                wallet_id,
            }))
            .await?
        {
            MachineBrokerResponse::CredentialListPublic(credentials) => Ok(credentials),
            _ => Err(response_mismatch("credential.list_public")),
        }
    }

    pub async fn custody_result(
        &self,
        request: OperationRequest,
    ) -> Result<CustodyResult, ProtocolError> {
        match self
            .request(MachineBrokerRequest::CustodyResult(request))
            .await?
        {
            MachineBrokerResponse::CustodyResult(result) => Ok(result),
            _ => Err(response_mismatch("custody.result")),
        }
    }

    pub async fn prepare_custody(
        &self,
        method: CustodyPrepareMethod,
        request: CustodyPrepareRequest,
    ) -> Result<CustodyPrepareResponse, ProtocolError> {
        let request = match method {
            CustodyPrepareMethod::WalletRegistration => {
                MachineBrokerRequest::WalletRegistrationPrepare(request)
            }
            CustodyPrepareMethod::WalletUnlock => {
                MachineBrokerRequest::WalletUnlockPrepare(request)
            }
            CustodyPrepareMethod::WalletImport => {
                MachineBrokerRequest::WalletImportPrepare(request)
            }
            CustodyPrepareMethod::WalletExport => {
                MachineBrokerRequest::WalletExportPrepare(request)
            }
            CustodyPrepareMethod::WalletDelete => {
                MachineBrokerRequest::WalletDeletePrepare(request)
            }
            CustodyPrepareMethod::KeyDerive => MachineBrokerRequest::KeyDerivePrepare(request),
            CustodyPrepareMethod::KeyEnroll => MachineBrokerRequest::KeyEnrollPrepare(request),
            CustodyPrepareMethod::CredentialAdd => {
                MachineBrokerRequest::CredentialAddPrepare(request)
            }
            CustodyPrepareMethod::CredentialReplace => {
                MachineBrokerRequest::CredentialReplacePrepare(request)
            }
            CustodyPrepareMethod::CredentialRemove => {
                MachineBrokerRequest::CredentialRemovePrepare(request)
            }
            CustodyPrepareMethod::Recovery => MachineBrokerRequest::RecoveryPrepare(request),
        };
        let expected = method.wire_name();
        match (method, self.request(request).await?) {
            (
                CustodyPrepareMethod::WalletRegistration,
                MachineBrokerResponse::WalletRegistrationPrepare(response),
            )
            | (
                CustodyPrepareMethod::WalletUnlock,
                MachineBrokerResponse::WalletUnlockPrepare(response),
            )
            | (
                CustodyPrepareMethod::WalletImport,
                MachineBrokerResponse::WalletImportPrepare(response),
            )
            | (
                CustodyPrepareMethod::WalletExport,
                MachineBrokerResponse::WalletExportPrepare(response),
            )
            | (
                CustodyPrepareMethod::WalletDelete,
                MachineBrokerResponse::WalletDeletePrepare(response),
            )
            | (
                CustodyPrepareMethod::KeyDerive,
                MachineBrokerResponse::KeyDerivePrepare(response),
            )
            | (
                CustodyPrepareMethod::KeyEnroll,
                MachineBrokerResponse::KeyEnrollPrepare(response),
            )
            | (
                CustodyPrepareMethod::CredentialAdd,
                MachineBrokerResponse::CredentialAddPrepare(response),
            )
            | (
                CustodyPrepareMethod::CredentialReplace,
                MachineBrokerResponse::CredentialReplacePrepare(response),
            )
            | (
                CustodyPrepareMethod::CredentialRemove,
                MachineBrokerResponse::CredentialRemovePrepare(response),
            )
            | (CustodyPrepareMethod::Recovery, MachineBrokerResponse::RecoveryPrepare(response)) => {
                Ok(response)
            }
            _ => Err(response_mismatch(expected)),
        }
    }
}

struct ExpectedSigningResult {
    operation_id: OperationId,
    operation_digest: Digest32,
    crypto_suite: CryptoSuite,
    signature_count: usize,
}

impl ExpectedSigningResult {
    fn from_request(request: &MachineSignRequest) -> Self {
        let signature_count = match &request.payloads {
            SigningPayloads::Single { .. } => 1,
            SigningPayloads::Batch { children } => children.len(),
        };
        Self {
            operation_id: request.operation_id.clone(),
            operation_digest: request.operation_digest.clone(),
            crypto_suite: request.crypto_suite,
            signature_count,
        }
    }

    fn validate(&self, result: SigningResult) -> Result<SigningResult, ProtocolError> {
        let expected_length = match self.crypto_suite.signature_encoding() {
            bloom_triad_protocol::SignatureEncoding::Secp256k1Recoverable65 => 65,
            bloom_triad_protocol::SignatureEncoding::Ed25519Raw64 => 64,
        };
        if result.operation_id != self.operation_id
            || result.operation_digest != self.operation_digest
            || result.signatures.len() != self.signature_count
            || result.signatures.iter().any(|signature| {
                signature.crypto_suite != self.crypto_suite
                    || signature.bytes.decode().len() != expected_length
            })
        {
            return Err(ProtocolError::new(
                ProtocolErrorCode::OperationIdConflict,
                "Broker signing response does not match operation, digest, count, suite, or encoding",
            ));
        }
        Ok(result)
    }
}

#[derive(Clone, Debug)]
pub struct TrustedPetalSignRequest {
    pub wallet_id: Token,
    pub preimage: Vec<u8>,
    pub claimed_hash: Digest32,
    pub crypto_suite: CryptoSuite,
    pub operation_class: Token,
    pub selector: bloom_triad_protocol::PetalSignSelector,
    pub claim: PetalUseClaim,
    pub claim_assurance_evidence: Option<Vec<u8>>,
    pub approval_id: Option<Digest32>,
    pub trusted_provenance: ProvenanceSubject,
    pub frozen_action: Option<Vec<u8>>,
    pub frozen_advisory: Option<Vec<u8>>,
}

#[derive(Clone, Debug)]
pub struct ExactPayloadSignRequest {
    pub wallet_id: Token,
    pub preimage: Vec<u8>,
    pub claimed_hash: Digest32,
    pub crypto_suite: CryptoSuite,
    pub provenance: ProvenanceSubject,
    pub provenance_digest: Digest32,
    /// `None` selects the fail-closed v1 default: boot-bound for local keys and
    /// backend-managed for non-local enrolled backends.
    pub activation_mode: Option<ActivationMode>,
    pub approval_operation_id: OperationId,
    pub signing_operation_id: OperationId,
    pub request_nonce: RequestNonce,
    pub issued_at_ms: DecimalU64,
    pub expires_at_ms: DecimalU64,
    pub canonical_plan_facts_digest: Digest32,
    pub approval_id: Option<Digest32>,
}

impl ExactPayloadSignRequest {
    fn validate(&self) -> Result<(), ProtocolError> {
        if self.expires_at_ms.get() <= self.issued_at_ms.get() {
            return Err(ProtocolError::new(
                ProtocolErrorCode::MalformedFrame,
                "exact approval validity interval is invalid",
            ));
        }
        SigningPayloads::Single {
            payload: Base64UrlBytes::from_bytes(&self.preimage),
        }
        .validate()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExactPayloadSignOutcome {
    ApprovalRequired(SealedApprovalPrepareResponse),
    Signed(SigningResult),
}

/// Durable Machine projection of a Broker-owned ceremony. Launch secrets are
/// retained only while Broker reports an actionable awaiting state.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CeremonyProjection {
    identity: Option<CeremonyProjectionIdentity>,
    ceremony_state: Option<CeremonyProjectionState>,
    ceremony_url: Option<String>,
    ceremony_expires_at_ms: Option<DecimalU64>,
    review_manifest_digest: Option<Digest32>,
    receipt_digest: Option<Digest32>,
    last_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum CeremonyProjectionIdentity {
    Approval {
        approval_id: Digest32,
    },
    Custody {
        operation_id: OperationId,
        ceremony_kind: bloom_triad_protocol::CeremonyKind,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "state", rename_all = "snake_case")]
pub enum CeremonyProjectionState {
    Approval(ApprovalLifecycleState),
    Custody(CeremonyState),
}

impl CeremonyProjection {
    pub fn from_approval_prepare(
        response: &SealedApprovalPrepareResponse,
        now_ms: u64,
    ) -> Result<Self, ProtocolError> {
        if response.state != bloom_triad_protocol::ApprovalPrepareState::AwaitingCeremony {
            return Err(projection_mismatch(
                "approval prepare is not awaiting ceremony",
            ));
        }
        Self::awaiting(
            CeremonyProjectionIdentity::Approval {
                approval_id: response.approval_id.clone(),
            },
            CeremonyProjectionState::Approval(ApprovalLifecycleState::AwaitingCeremony),
            response.ceremony_url.clone(),
            response.ceremony_expires_at_ms.clone(),
            Some(response.review_manifest_digest.clone()),
            now_ms,
        )
    }

    pub fn from_custody_prepare(
        response: &CustodyPrepareResponse,
        now_ms: u64,
    ) -> Result<Self, ProtocolError> {
        if response.state != bloom_triad_protocol::CustodyPrepareState::AwaitingUser {
            return Err(projection_mismatch("custody prepare is not awaiting user"));
        }
        Self::awaiting(
            CeremonyProjectionIdentity::Custody {
                operation_id: response.custody_operation_id.clone(),
                ceremony_kind: response.ceremony_kind,
            },
            CeremonyProjectionState::Custody(CeremonyState::AwaitingUser),
            response.ceremony_url.clone(),
            response.ceremony_expires_at_ms.clone(),
            None,
            now_ms,
        )
    }

    pub fn from_policy_prepare(
        response: &PolicyUpdatePrepareResponse,
        now_ms: u64,
    ) -> Result<Self, ProtocolError> {
        if response.ceremony_kind != bloom_triad_protocol::CeremonyKind::PolicyUpdate {
            return Err(projection_mismatch(
                "policy prepare did not return policy_update ceremony kind",
            ));
        }
        Self::awaiting(
            CeremonyProjectionIdentity::Custody {
                operation_id: response.operation_id.clone(),
                ceremony_kind: response.ceremony_kind,
            },
            CeremonyProjectionState::Custody(CeremonyState::AwaitingUser),
            response.ceremony_url.clone(),
            response.ceremony_expires_at_ms.clone(),
            Some(response.review_manifest_digest.clone()),
            now_ms,
        )
    }

    pub fn from_custody_status(
        status: &CeremonyPublicStatus,
        now_ms: u64,
    ) -> Result<Self, ProtocolError> {
        if status.state != CeremonyState::AwaitingUser {
            return Ok(Self {
                identity: Some(CeremonyProjectionIdentity::Custody {
                    operation_id: status.operation_id.clone(),
                    ceremony_kind: status.ceremony_kind,
                }),
                ceremony_state: Some(CeremonyProjectionState::Custody(status.state)),
                ceremony_url: None,
                ceremony_expires_at_ms: None,
                review_manifest_digest: None,
                receipt_digest: status.receipt_digest.clone(),
                last_error: None,
            });
        }
        let url = status.ceremony_url.clone().ok_or_else(|| {
            projection_mismatch("awaiting custody status is missing ceremony URL")
        })?;
        Self::awaiting(
            CeremonyProjectionIdentity::Custody {
                operation_id: status.operation_id.clone(),
                ceremony_kind: status.ceremony_kind,
            },
            CeremonyProjectionState::Custody(status.state),
            url,
            status.expires_at_ms.clone(),
            None,
            now_ms,
        )
    }

    pub fn from_custody_result(result: &CustodyResult) -> Self {
        Self {
            identity: Some(CeremonyProjectionIdentity::Custody {
                operation_id: result.custody_operation_id.clone(),
                ceremony_kind: result.ceremony_kind,
            }),
            ceremony_state: Some(CeremonyProjectionState::Custody(result.public_status)),
            ceremony_url: None,
            ceremony_expires_at_ms: None,
            receipt_digest: Some(result.receipt_digest.clone()),
            review_manifest_digest: None,
            last_error: None,
        }
    }

    pub fn reconcile_approval(
        &mut self,
        status: &ApprovalPublicStatus,
        now_ms: u64,
    ) -> Result<(), ProtocolError> {
        if self.identity
            != Some(CeremonyProjectionIdentity::Approval {
                approval_id: status.approval_id.clone(),
            })
        {
            self.fail_closed("approval status does not match originating projection");
            return Err(projection_mismatch(
                "approval status does not match originating projection",
            ));
        }
        self.ceremony_state = Some(CeremonyProjectionState::Approval(status.state));
        if status.state == ApprovalLifecycleState::AwaitingCeremony {
            match (&status.ceremony_url, &status.ceremony_expires_at_ms) {
                (Some(url), Some(expiry))
                    if self.ceremony_url.as_ref() == Some(url)
                        && self.ceremony_expires_at_ms.as_ref() == Some(expiry)
                        && expiry.get() > now_ms =>
                {
                    self.last_error = None;
                }
                (Some(_), Some(_)) => {
                    self.fail_closed("approval ceremony URL or expiry changed");
                    return Err(projection_mismatch(
                        "approval ceremony URL or expiry changed",
                    ));
                }
                _ => self.clear_launch_secret(),
            }
        } else {
            self.clear_launch_secret();
        }
        Ok(())
    }

    pub fn reconcile_custody(
        &mut self,
        status: &CeremonyPublicStatus,
        now_ms: u64,
    ) -> Result<(), ProtocolError> {
        if self.identity
            != Some(CeremonyProjectionIdentity::Custody {
                operation_id: status.operation_id.clone(),
                ceremony_kind: status.ceremony_kind,
            })
        {
            self.fail_closed("custody status does not match originating projection");
            return Err(projection_mismatch(
                "custody status does not match originating projection",
            ));
        }
        self.ceremony_state = Some(CeremonyProjectionState::Custody(status.state));
        self.receipt_digest = status.receipt_digest.clone();
        if status.state == CeremonyState::AwaitingUser
            && self.ceremony_url.is_some()
            && self.ceremony_expires_at_ms.as_ref() == Some(&status.expires_at_ms)
            && status.ceremony_url.as_deref() == self.ceremony_url.as_deref()
            && status.expires_at_ms.get() > now_ms
        {
            self.last_error = None;
            return Ok(());
        }
        self.clear_launch_secret();
        Ok(())
    }

    pub fn reconcile_custody_result(
        &mut self,
        result: &CustodyResult,
    ) -> Result<(), ProtocolError> {
        if self.identity
            != Some(CeremonyProjectionIdentity::Custody {
                operation_id: result.custody_operation_id.clone(),
                ceremony_kind: result.ceremony_kind,
            })
        {
            self.fail_closed("custody result does not match originating projection");
            return Err(projection_mismatch(
                "custody result does not match originating projection",
            ));
        }
        self.ceremony_state = Some(CeremonyProjectionState::Custody(result.public_status));
        self.receipt_digest = Some(result.receipt_digest.clone());
        self.clear_launch_secret();
        Ok(())
    }

    pub fn expire_launch_secret(&mut self, now_ms: u64) {
        if self
            .ceremony_expires_at_ms
            .as_ref()
            .is_some_and(|expiry| expiry.get() <= now_ms)
        {
            self.clear_launch_secret();
            self.last_error = Some("ceremony launch URL expired".into());
        }
    }

    pub fn ceremony_url(&self) -> Option<&str> {
        self.ceremony_url.as_deref()
    }

    pub fn expires_at_ms(&self) -> Option<u64> {
        self.ceremony_expires_at_ms.as_ref().map(DecimalU64::get)
    }

    pub fn operation_id(&self) -> Option<&OperationId> {
        match self.identity.as_ref() {
            Some(CeremonyProjectionIdentity::Custody { operation_id, .. }) => Some(operation_id),
            _ => None,
        }
    }

    pub fn approval_id(&self) -> Option<&Digest32> {
        match self.identity.as_ref() {
            Some(CeremonyProjectionIdentity::Approval { approval_id }) => Some(approval_id),
            _ => None,
        }
    }

    pub fn ceremony_kind(&self) -> Option<bloom_triad_protocol::CeremonyKind> {
        match self.identity.as_ref() {
            Some(CeremonyProjectionIdentity::Custody { ceremony_kind, .. }) => Some(*ceremony_kind),
            _ => None,
        }
    }

    pub fn state(&self) -> Option<CeremonyProjectionState> {
        self.ceremony_state
    }

    pub fn receipt_digest(&self) -> Option<&Digest32> {
        self.receipt_digest.as_ref()
    }

    pub fn review_manifest_digest(&self) -> Option<&Digest32> {
        self.review_manifest_digest.as_ref()
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    fn fail_closed(&mut self, message: &str) {
        self.clear_launch_secret();
        self.last_error = Some(message.into());
    }

    fn clear_launch_secret(&mut self) {
        self.ceremony_url = None;
        self.ceremony_expires_at_ms = None;
    }

    fn awaiting(
        identity: CeremonyProjectionIdentity,
        state: CeremonyProjectionState,
        url: String,
        expiry: DecimalU64,
        review_manifest_digest: Option<Digest32>,
        now_ms: u64,
    ) -> Result<Self, ProtocolError> {
        if url.is_empty() || expiry.get() <= now_ms {
            Err(projection_mismatch(
                "ceremony URL must be non-empty and unexpired",
            ))
        } else {
            Ok(Self {
                identity: Some(identity),
                ceremony_state: Some(state),
                ceremony_url: Some(url),
                ceremony_expires_at_ms: Some(expiry),
                review_manifest_digest,
                receipt_digest: None,
                last_error: None,
            })
        }
    }
}

fn projection_mismatch(message: &str) -> ProtocolError {
    ProtocolError::new(ProtocolErrorCode::MalformedFrame, message)
}

fn approval_subject(provenance: &ProvenanceSubject) -> ApprovalSubject {
    match provenance {
        ProvenanceSubject::Petal {
            package_hash,
            route,
        } => ApprovalSubject::Petal {
            package_hash: package_hash.clone(),
            route: route.clone(),
            agent_id: None,
        },
        ProvenanceSubject::Cli {
            client_id,
            command_class,
        } => ApprovalSubject::Cli {
            client_id: client_id.clone(),
            command_class: command_class.clone(),
        },
        ProvenanceSubject::System {
            component_id,
            operation_class,
        } => ApprovalSubject::System {
            component_id: component_id.clone(),
            operation_class: operation_class.clone(),
        },
    }
}

fn default_activation_mode(key_ref: &KeyRef) -> ActivationMode {
    if key_ref.backend.as_str() == "local" {
        ActivationMode::BootBound
    } else {
        ActivationMode::BackendManaged
    }
}

/// Load the installer-owned public provenance catalog used to bind approval
/// terms. Broker independently verifies every record signature before use.
#[cfg(unix)]
pub fn load_provenance_catalog(path: impl AsRef<Path>) -> Result<ProvenanceCatalog, ProtocolError> {
    use std::os::unix::fs::MetadataExt as _;

    let path = path.as_ref();
    let metadata = std::fs::symlink_metadata(path).map_err(|error| {
        ProtocolError::new(
            ProtocolErrorCode::UnauthenticatedPeer,
            format!("inspect {}: {error}", path.display()),
        )
    })?;
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || metadata.uid() != 0
        || metadata.mode() & 0o022 != 0
    {
        return Err(ProtocolError::new(
            ProtocolErrorCode::UnauthenticatedPeer,
            "provenance catalog must be a root-owned, non-symlink regular file not writable by group or other",
        ));
    }
    let bytes = std::fs::read(path).map_err(|error| {
        ProtocolError::new(
            ProtocolErrorCode::UnauthenticatedPeer,
            format!("read {}: {error}", path.display()),
        )
    })?;
    decode_provenance_catalog(&bytes)
}

fn decode_provenance_catalog(bytes: &[u8]) -> Result<ProvenanceCatalog, ProtocolError> {
    if bytes.len() > 1024 * 1024 {
        return Err(ProtocolError::new(
            ProtocolErrorCode::LimitExceededFrame,
            "provenance catalog exceeds 1 MiB",
        ));
    }
    let catalog: ProvenanceCatalog = serde_json::from_slice(bytes).map_err(|error| {
        ProtocolError::new(
            ProtocolErrorCode::MalformedFrame,
            format!("parse provenance catalog: {error}"),
        )
    })?;
    catalog.validate_shape()?;
    Ok(catalog)
}

impl TrustedPetalSignRequest {
    fn validate(&self) -> Result<(), ProtocolError> {
        if self.preimage.is_empty()
            || !matches!(
                &self.trusted_provenance,
                ProvenanceSubject::Petal { route, .. } if !route.is_empty()
            )
        {
            return Err(ProtocolError::new(
                ProtocolErrorCode::MalformedFrame,
                "payload and trusted route must be non-empty",
            ));
        }
        SigningPayloads::Single {
            payload: Base64UrlBytes::from_bytes(&self.preimage),
        }
        .validate()
    }

    fn operation_id(&self) -> Result<OperationId, ProtocolError> {
        #[derive(Serialize)]
        struct Identity<'a> {
            wallet_id: &'a Token,
            approval_id: &'a Option<Digest32>,
            payload_digest: Digest32,
            claimed_hash: &'a Digest32,
            crypto_suite: CryptoSuite,
            operation_class: &'a Token,
            selector: bloom_triad_protocol::PetalSignSelector,
            claim_digest: Digest32,
            trusted_provenance: &'a ProvenanceSubject,
            frozen_action_digest: Option<Digest32>,
            frozen_advisory_digest: Option<Digest32>,
        }
        let identity = Identity {
            wallet_id: &self.wallet_id,
            approval_id: &self.approval_id,
            payload_digest: Digest32::from_bytes(Sha256::digest(&self.preimage).into()),
            claimed_hash: &self.claimed_hash,
            crypto_suite: self.crypto_suite,
            operation_class: &self.operation_class,
            selector: self.selector,
            claim_digest: jcs_digest(&self.claim)?,
            trusted_provenance: &self.trusted_provenance,
            frozen_action_digest: self
                .frozen_action
                .as_ref()
                .map(|bytes| Digest32::from_bytes(Sha256::digest(bytes).into())),
            frozen_advisory_digest: self
                .frozen_advisory
                .as_ref()
                .map(|bytes| Digest32::from_bytes(Sha256::digest(bytes).into())),
        };
        let mut hasher = Sha256::new();
        hasher.update(b"bloom-machine-petal-operation/v1");
        hasher.update(serde_jcs::to_vec(&identity).map_err(canonical_error)?);
        Ok(OperationId::from_bytes(hasher.finalize().into()))
    }
}

fn unique_key_for_suite(keys: &[KeyRef], suite: CryptoSuite) -> Result<KeyRef, ProtocolError> {
    let mut matching = keys.iter().filter(|key| key.key_spec == suite.key_spec());
    let key = matching.next().cloned().ok_or_else(|| {
        ProtocolError::new(
            ProtocolErrorCode::KeyrefMismatch,
            "wallet has no key compatible with requested CryptoSuite",
        )
    })?;
    if matching.next().is_some() {
        return Err(ProtocolError::new(
            ProtocolErrorCode::KeyrefMismatch,
            "wallet has multiple compatible keys; signing selection is ambiguous",
        ));
    }
    Ok(key)
}

fn suite_hash(suite: CryptoSuite, payload: &[u8]) -> Digest32 {
    match suite {
        CryptoSuite::Secp256k1Keccak256Recoverable => {
            Digest32::from_bytes(Keccak256::digest(payload).into())
        }
        CryptoSuite::Secp256k1Sha256Recoverable | CryptoSuite::Ed25519Message => {
            Digest32::from_bytes(Sha256::digest(payload).into())
        }
    }
}

fn jcs_digest<T: Serialize>(value: &T) -> Result<Digest32, ProtocolError> {
    Ok(Digest32::from_bytes(
        Sha256::digest(serde_jcs::to_vec(value).map_err(canonical_error)?).into(),
    ))
}

fn canonical_error(error: serde_json::Error) -> ProtocolError {
    ProtocolError::new(
        ProtocolErrorCode::MalformedFrame,
        format!("canonical request encoding failed: {error}"),
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CustodyPrepareMethod {
    WalletRegistration,
    WalletUnlock,
    WalletImport,
    WalletExport,
    WalletDelete,
    KeyDerive,
    KeyEnroll,
    CredentialAdd,
    CredentialReplace,
    CredentialRemove,
    Recovery,
}

impl CustodyPrepareMethod {
    pub const ALL: [Self; 11] = [
        Self::WalletRegistration,
        Self::WalletUnlock,
        Self::WalletImport,
        Self::WalletExport,
        Self::WalletDelete,
        Self::KeyDerive,
        Self::KeyEnroll,
        Self::CredentialAdd,
        Self::CredentialReplace,
        Self::CredentialRemove,
        Self::Recovery,
    ];

    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::WalletRegistration => "wallet.registration_prepare",
            Self::WalletUnlock => "wallet.unlock_prepare",
            Self::WalletImport => "wallet.import_prepare",
            Self::WalletExport => "wallet.export_prepare",
            Self::WalletDelete => "wallet.delete_prepare",
            Self::KeyDerive => "key.derive_prepare",
            Self::KeyEnroll => "key.enroll_prepare",
            Self::CredentialAdd => "credential.add_prepare",
            Self::CredentialReplace => "credential.replace_prepare",
            Self::CredentialRemove => "credential.remove_prepare",
            Self::Recovery => "recovery.prepare",
        }
    }
}

fn response_mismatch(method: &str) -> ProtocolError {
    ProtocolError::new(
        ProtocolErrorCode::MalformedFrame,
        format!("Broker returned a mismatched response for {method}"),
    )
}

fn service_unavailable(message: String) -> ProtocolError {
    ProtocolError::new(ProtocolErrorCode::ServiceUnavailable, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{os::unix::fs::MetadataExt, sync::Mutex};

    use bloom_triad_protocol::{
        ApprovalPrepareState, CeremonyKind, CustodyPrepareState, DeclaredFee, KeySpec,
        NormalizedSignature, RequestNonce, ServiceFuture, SignatureEncoding,
    };
    use ed25519_dalek::SigningKey;

    struct MockBroker {
        wallet: WalletPublic,
        requests: Mutex<Vec<MachineBrokerRequest>>,
        corrupt_response: bool,
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
                        Ok(MachineBrokerResponse::WalletGetPublic(self.wallet.clone()))
                    }
                    MachineBrokerRequest::KeyGetPublic(request) => {
                        let mut returned_key_ref = request.key_ref;
                        let supported_crypto_suites =
                            if returned_key_ref.locator.contains("unsupported-suite") {
                                vec![]
                            } else {
                                vec![CryptoSuite::Secp256k1Sha256Recoverable]
                            };
                        if returned_key_ref.locator.contains("wrong-key") {
                            returned_key_ref.locator = "wallet/delegated/substituted".into();
                        }
                        Ok(MachineBrokerResponse::KeyGetPublic(KeyPublic {
                            key_ref: returned_key_ref,
                            canonical_public_key: Base64UrlBytes::from_bytes(&[2; 33]),
                            addresses: vec!["0x0000000000000000000000000000000000000001".into()],
                            supported_crypto_suites,
                        }))
                    }
                    MachineBrokerRequest::SigningSign(request) => {
                        Ok(MachineBrokerResponse::SigningSign(SigningResult {
                            operation_id: request.operation_id,
                            operation_digest: if self.corrupt_response {
                                digest(99)
                            } else {
                                request.operation_digest
                            },
                            signatures: vec![NormalizedSignature {
                                crypto_suite: request.crypto_suite,
                                bytes: Base64UrlBytes::from_bytes(&[7; 65]),
                            }],
                            signer_receipt_digest: digest(90),
                            broker_receipt_digest: digest(91),
                        }))
                    }
                    MachineBrokerRequest::SealedApprovalPrepare(request) => {
                        Ok(MachineBrokerResponse::SealedApprovalPrepare(
                            SealedApprovalPrepareResponse {
                                approval_id: request.terms.approval_id()?,
                                state: ApprovalPrepareState::AwaitingCeremony,
                                ceremony_url: "http://localhost:18734/ceremony/exact-owner-secret"
                                    .into(),
                                ceremony_expires_at_ms: request.terms.expires_at_ms,
                                review_manifest_digest: digest(92),
                            },
                        ))
                    }
                    MachineBrokerRequest::CeremonyStatus(request) => {
                        let operation_id =
                            OperationId::new(request.id.as_str().to_owned()).unwrap();
                        Ok(MachineBrokerResponse::CeremonyStatus(
                            CeremonyPublicStatus {
                                ceremony_id: digest(81),
                                ceremony_kind: CeremonyKind::WalletImport,
                                operation_id,
                                state: CeremonyState::AwaitingUser,
                                expires_at_ms: DecimalU64::new(9_000),
                                ceremony_url: Some(
                                    "http://localhost:18734/ceremony/owner-secret".into(),
                                ),
                                receipt_digest: None,
                            },
                        ))
                    }
                    MachineBrokerRequest::CeremonyCancel(request) => {
                        let operation_id =
                            OperationId::new(request.id.as_str().to_owned()).unwrap();
                        Ok(MachineBrokerResponse::CeremonyCancel(
                            CeremonyPublicStatus {
                                ceremony_id: digest(81),
                                ceremony_kind: CeremonyKind::WalletImport,
                                operation_id,
                                state: CeremonyState::Cancelled,
                                expires_at_ms: DecimalU64::new(9_000),
                                ceremony_url: None,
                                receipt_digest: None,
                            },
                        ))
                    }
                    _ => Err(ProtocolError::new(
                        ProtocolErrorCode::UnknownMethod,
                        "unexpected mock request",
                    )),
                }
            })
        }
    }

    #[tokio::test]
    async fn payload_translation_binds_bytes_claim_and_trusted_provenance() {
        let key_ref = key_ref();
        let broker = Arc::new(MockBroker {
            wallet: WalletPublic {
                wallet_id: token("wallet"),
                wallet_kind: token("local"),
                key_refs: vec![key_ref.clone()],
                policy_version: DecimalU64::new(7),
                policy_digest: digest(7),
                wallet_revocation_epoch: DecimalU64::new(2),
            },
            requests: Mutex::new(Vec::new()),
            corrupt_response: false,
        });
        let client = MachineBrokerClient::new(broker.clone());
        let payload = b"exact final bytes".to_vec();
        let payload_digest = Digest32::from_bytes(Sha256::digest(&payload).into());
        let request = TrustedPetalSignRequest {
            wallet_id: token("wallet"),
            preimage: payload.clone(),
            claimed_hash: payload_digest.clone(),
            crypto_suite: CryptoSuite::Secp256k1Sha256Recoverable,
            operation_class: token("order.place"),
            selector: bloom_triad_protocol::PetalSignSelector::Reusable,
            claim: PetalUseClaim {
                package_hash: digest(40),
                route: "orders/place".into(),
                operation_class: token("order.place"),
                crypto_suite: CryptoSuite::Secp256k1Sha256Recoverable,
                payload_digest: payload_digest.clone(),
                ordered_hashes: vec![payload_digest],
                declared_debits: vec![],
                declared_destinations: vec![],
                declared_fee: DeclaredFee::None,
                nonce: RequestNonce::from_bytes([5; 16]),
                claim_assurance: bloom_triad_protocol::ClaimAssurance::MachineAsserted,
            },
            claim_assurance_evidence: Some(b"machine evidence".to_vec()),
            approval_id: Some(digest(50)),
            trusted_provenance: ProvenanceSubject::Petal {
                package_hash: digest(40),
                route: "orders/place".into(),
            },
            frozen_action: Some(b"place order".to_vec()),
            frozen_advisory: Some(b"price moved".to_vec()),
        };

        let result = client.sign_petal_payload(request).await.unwrap();
        assert_eq!(result.signatures[0].bytes.decode(), vec![7; 65]);
        assert_eq!(
            result.signatures[0].crypto_suite.signature_encoding(),
            SignatureEncoding::Secp256k1Recoverable65
        );
        let requests = broker.requests.lock().unwrap();
        let MachineBrokerRequest::SigningSign(signed) = &requests[1] else {
            panic!("second request must be signing.sign");
        };
        assert_eq!(signed.key_ref, key_ref);
        assert_eq!(
            signed.payloads,
            SigningPayloads::Single {
                payload: Base64UrlBytes::from_bytes(&payload)
            }
        );
    }

    #[tokio::test]
    async fn petal_payload_can_use_an_explicit_broker_validated_delegated_key() {
        let root_key_ref = key_ref();
        let broker = Arc::new(MockBroker {
            wallet: WalletPublic {
                wallet_id: token("wallet"),
                wallet_kind: token("local"),
                key_refs: vec![root_key_ref],
                policy_version: DecimalU64::new(7),
                policy_digest: digest(7),
                wallet_revocation_epoch: DecimalU64::new(2),
            },
            requests: Mutex::new(Vec::new()),
            corrupt_response: false,
        });
        let client = MachineBrokerClient::new(broker.clone());
        let payload = b"delegated petal action".to_vec();
        let payload_digest = Digest32::from_bytes(Sha256::digest(&payload).into());
        let mut delegated_key_ref = key_ref();
        delegated_key_ref.locator = "wallet/delegated/1".into();
        let request = TrustedPetalSignRequest {
            wallet_id: token("wallet"),
            preimage: payload,
            claimed_hash: payload_digest.clone(),
            crypto_suite: CryptoSuite::Secp256k1Sha256Recoverable,
            operation_class: token("order.cancel"),
            selector: bloom_triad_protocol::PetalSignSelector::Reusable,
            claim: PetalUseClaim {
                package_hash: digest(40),
                route: "orders/cancel".into(),
                operation_class: token("order.cancel"),
                crypto_suite: CryptoSuite::Secp256k1Sha256Recoverable,
                payload_digest: payload_digest.clone(),
                ordered_hashes: vec![payload_digest],
                declared_debits: vec![],
                declared_destinations: vec![],
                declared_fee: DeclaredFee::None,
                nonce: RequestNonce::from_bytes([5; 16]),
                claim_assurance: bloom_triad_protocol::ClaimAssurance::MachineAsserted,
            },
            claim_assurance_evidence: Some(b"machine evidence".to_vec()),
            approval_id: Some(digest(50)),
            trusted_provenance: ProvenanceSubject::Petal {
                package_hash: digest(40),
                route: "orders/cancel".into(),
            },
            frozen_action: Some(b"cancel order".to_vec()),
            frozen_advisory: None,
        };

        client
            .sign_petal_payload_with_key(request.clone(), delegated_key_ref.clone())
            .await
            .unwrap();

        let requests = broker.requests.lock().unwrap();
        assert!(matches!(
            &requests[0],
            MachineBrokerRequest::WalletGetPublic(_)
        ));
        let MachineBrokerRequest::KeyGetPublic(key_request) = &requests[1] else {
            panic!("explicit delegated signing must fetch its public key projection");
        };
        assert_eq!(key_request.key_ref, delegated_key_ref);
        let MachineBrokerRequest::SigningSign(sign_request) = &requests[2] else {
            panic!("explicit delegated signing must use signing.sign");
        };
        assert_eq!(sign_request.key_ref, delegated_key_ref);
        assert_eq!(sign_request.petal_use_claim.as_ref(), Some(&request.claim));
        assert_eq!(
            sign_request
                .claim_assurance_evidence
                .as_ref()
                .map(Base64UrlBytes::decode),
            Some(b"machine evidence".to_vec())
        );
        let reusable_operation_id = sign_request.operation_id.clone();
        drop(requests);

        broker.requests.lock().unwrap().clear();
        let mut exact_request = request.clone();
        exact_request.selector = bloom_triad_protocol::PetalSignSelector::Exact;
        client
            .sign_petal_payload_with_key(exact_request, delegated_key_ref.clone())
            .await
            .unwrap();
        let requests = broker.requests.lock().unwrap();
        let MachineBrokerRequest::SigningSign(exact_sign_request) = &requests[2] else {
            panic!("exact explicit delegated signing must use signing.sign");
        };
        assert_eq!(exact_sign_request.key_ref, delegated_key_ref);
        assert!(exact_sign_request.petal_use_claim.is_none());
        assert!(exact_sign_request.claim_assurance_evidence.is_none());
        assert_ne!(exact_sign_request.operation_id, reusable_operation_id);
        let expected_exact_digest = SignOperationIdentity {
            operation_id: exact_sign_request.operation_id.clone(),
            approval_id: digest(50),
            key_ref: delegated_key_ref.clone(),
            crypto_suite: CryptoSuite::Secp256k1Sha256Recoverable,
            ordered_payload_digests: vec![Digest32::from_bytes(
                Sha256::digest(b"delegated petal action").into(),
            )],
            ordered_hashes: vec![Digest32::from_bytes(
                Sha256::digest(b"delegated petal action").into(),
            )],
            petal_use_claim_digest: None,
            claim_assurance_digest: None,
            policy_version: DecimalU64::new(7),
            policy_digest: digest(7),
        }
        .digest()
        .unwrap();
        assert_eq!(exact_sign_request.operation_digest, expected_exact_digest);
        drop(requests);
        broker.requests.lock().unwrap().clear();

        let mut unsupported_key_ref = key_ref();
        unsupported_key_ref.locator = "wallet/delegated/unsupported-suite".into();
        let error = client
            .sign_petal_payload_with_key(request.clone(), unsupported_key_ref)
            .await
            .unwrap_err();
        assert_eq!(error.code, ProtocolErrorCode::SuiteNotAllowed);
        assert_eq!(
            broker.requests.lock().unwrap().len(),
            2,
            "suite rejection must happen before signing.sign"
        );

        let mut substituted_key_ref = key_ref();
        substituted_key_ref.locator = "wallet/delegated/wrong-key".into();
        let error = client
            .sign_petal_payload_with_key(request, substituted_key_ref)
            .await
            .unwrap_err();
        assert_eq!(error.code, ProtocolErrorCode::KeyrefMismatch);
        assert_eq!(
            broker.requests.lock().unwrap().len(),
            4,
            "substituted key metadata must be rejected before signing.sign"
        );
    }

    fn exact_request(payload: Vec<u8>, approval_id: Option<Digest32>) -> ExactPayloadSignRequest {
        ExactPayloadSignRequest {
            wallet_id: token("wallet"),
            claimed_hash: Digest32::from_bytes(Keccak256::digest(&payload).into()),
            preimage: payload,
            crypto_suite: CryptoSuite::Secp256k1Keccak256Recoverable,
            provenance: ProvenanceSubject::Cli {
                client_id: token("bloom-cli"),
                command_class: token("transaction.confirm"),
            },
            provenance_digest: digest(60),
            activation_mode: Some(ActivationMode::BootBound),
            approval_operation_id: OperationId::from_bytes([61; 32]),
            signing_operation_id: OperationId::from_bytes([62; 32]),
            request_nonce: RequestNonce::from_bytes([63; 16]),
            issued_at_ms: DecimalU64::new(1_000),
            expires_at_ms: DecimalU64::new(601_000),
            canonical_plan_facts_digest: digest(64),
            approval_id,
        }
    }

    #[tokio::test]
    async fn exact_payload_prepares_then_signs_without_a_hash_only_path() {
        let broker = Arc::new(MockBroker {
            wallet: WalletPublic {
                wallet_id: token("wallet"),
                wallet_kind: token("local"),
                key_refs: vec![key_ref()],
                policy_version: DecimalU64::new(7),
                policy_digest: digest(7),
                wallet_revocation_epoch: DecimalU64::new(2),
            },
            requests: Mutex::new(Vec::new()),
            corrupt_response: false,
        });
        let client = MachineBrokerClient::new(broker.clone());
        let payload = b"canonical unsigned EVM envelope".to_vec();
        let prepared = client
            .sign_exact_payload(exact_request(payload.clone(), None))
            .await
            .unwrap();
        let ExactPayloadSignOutcome::ApprovalRequired(prepared) = prepared else {
            panic!("first call must prepare an exact approval");
        };

        {
            let requests = broker.requests.lock().unwrap();
            let MachineBrokerRequest::SealedApprovalPrepare(request) = &requests[1] else {
                panic!("second call must be sealed_approval.prepare");
            };
            assert_eq!(
                request.terms.selector,
                ApprovalSelector::Exact {
                    ordered_payload_digests: vec![Digest32::from_bytes(
                        Sha256::digest(&payload).into()
                    )],
                    ordered_hashes: vec![Digest32::from_bytes(Keccak256::digest(&payload).into())],
                }
            );
            assert_eq!(request.terms.provenance_digest, digest(60));
            assert_eq!(request.terms.limits.max_operations.get(), 1);
            assert_eq!(request.terms.limits.max_signatures.get(), 1);
        }

        let signed = client
            .sign_exact_payload(exact_request(payload.clone(), Some(prepared.approval_id)))
            .await
            .unwrap();
        let ExactPayloadSignOutcome::Signed(signed) = signed else {
            panic!("approved retry must call signing.sign");
        };
        assert_eq!(signed.signatures[0].bytes.decode(), vec![7; 65]);
        let requests = broker.requests.lock().unwrap();
        let MachineBrokerRequest::SigningSign(request) = &requests[3] else {
            panic!("fourth call must be signing.sign");
        };
        assert_eq!(
            request.payloads,
            SigningPayloads::Single {
                payload: Base64UrlBytes::from_bytes(&payload)
            }
        );
        assert!(request.petal_use_claim.is_none());
        assert!(request.claim_assurance_evidence.is_none());
    }

    #[tokio::test]
    async fn exact_payload_rejects_changed_hash_before_prepare_or_sign() {
        let broker = Arc::new(MockBroker {
            wallet: WalletPublic {
                wallet_id: token("wallet"),
                wallet_kind: token("local"),
                key_refs: vec![key_ref()],
                policy_version: DecimalU64::new(1),
                policy_digest: digest(1),
                wallet_revocation_epoch: DecimalU64::new(0),
            },
            requests: Mutex::new(Vec::new()),
            corrupt_response: false,
        });
        let mut request = exact_request(b"payload".to_vec(), None);
        request.claimed_hash = digest(99);
        let error = MachineBrokerClient::new(broker.clone())
            .sign_exact_payload(request)
            .await
            .unwrap_err();
        assert_eq!(error.code, ProtocolErrorCode::SelectorMismatch);
        assert_eq!(broker.requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn guest_provenance_substitution_fails_before_signing() {
        let broker = Arc::new(MockBroker {
            wallet: WalletPublic {
                wallet_id: token("wallet"),
                wallet_kind: token("local"),
                key_refs: vec![key_ref()],
                policy_version: DecimalU64::new(1),
                policy_digest: digest(1),
                wallet_revocation_epoch: DecimalU64::new(0),
            },
            requests: Mutex::new(Vec::new()),
            corrupt_response: false,
        });
        let payload = b"payload".to_vec();
        let hash = Digest32::from_bytes(Sha256::digest(&payload).into());
        let error = MachineBrokerClient::new(broker.clone())
            .sign_petal_payload(TrustedPetalSignRequest {
                wallet_id: token("wallet"),
                preimage: payload,
                claimed_hash: hash.clone(),
                crypto_suite: CryptoSuite::Secp256k1Sha256Recoverable,
                operation_class: token("order.place"),
                selector: bloom_triad_protocol::PetalSignSelector::Reusable,
                claim: PetalUseClaim {
                    package_hash: digest(41),
                    route: "forged/route".into(),
                    operation_class: token("order.place"),
                    crypto_suite: CryptoSuite::Secp256k1Sha256Recoverable,
                    payload_digest: hash.clone(),
                    ordered_hashes: vec![hash],
                    declared_debits: vec![],
                    declared_destinations: vec![],
                    declared_fee: DeclaredFee::None,
                    nonce: RequestNonce::from_bytes([8; 16]),
                    claim_assurance: bloom_triad_protocol::ClaimAssurance::MachineAsserted,
                },
                claim_assurance_evidence: None,
                approval_id: Some(digest(52)),
                trusted_provenance: ProvenanceSubject::Petal {
                    package_hash: digest(40),
                    route: "orders/place".into(),
                },
                frozen_action: None,
                frozen_advisory: None,
            })
            .await
            .unwrap_err();
        assert_eq!(error.code, ProtocolErrorCode::ClaimInvalid);
        assert!(
            broker
                .requests
                .lock()
                .unwrap()
                .iter()
                .all(|request| !matches!(request, MachineBrokerRequest::SigningSign(_)))
        );
    }

    #[test]
    fn broker_ceremony_projection_is_visible_only_while_awaiting() {
        let prepare = CustodyPrepareResponse {
            ceremony_kind: CeremonyKind::WalletImport,
            custody_operation_id: OperationId::from_bytes([4; 32]),
            state: CustodyPrepareState::AwaitingUser,
            ceremony_url: "http://127.0.0.1:18734/c/opaque".into(),
            ceremony_expires_at_ms: DecimalU64::new(2_000),
            signer_contribution_digest: digest(6),
        };
        let mut projection = CeremonyProjection::from_custody_prepare(&prepare, 1_000).unwrap();
        assert_eq!(
            projection.ceremony_url(),
            Some("http://127.0.0.1:18734/c/opaque")
        );
        projection
            .reconcile_custody(
                &CeremonyPublicStatus {
                    ceremony_id: digest(8),
                    ceremony_kind: CeremonyKind::WalletImport,
                    operation_id: OperationId::from_bytes([4; 32]),
                    state: CeremonyState::AwaitingUser,
                    expires_at_ms: DecimalU64::new(2_000),
                    ceremony_url: Some("http://127.0.0.1:18734/c/opaque".into()),
                    receipt_digest: None,
                },
                1_999,
            )
            .unwrap();
        assert!(projection.ceremony_url().is_some());
        projection
            .reconcile_custody(
                &CeremonyPublicStatus {
                    ceremony_id: digest(8),
                    ceremony_kind: CeremonyKind::WalletImport,
                    operation_id: OperationId::from_bytes([4; 32]),
                    state: CeremonyState::Succeeded,
                    expires_at_ms: DecimalU64::new(2_000),
                    ceremony_url: None,
                    receipt_digest: Some(digest(9)),
                },
                1_999,
            )
            .unwrap();
        assert_eq!(projection.ceremony_url(), None);
        assert_eq!(
            projection.operation_id(),
            Some(&OperationId::from_bytes([4; 32]))
        );
        assert_eq!(
            projection.state(),
            Some(CeremonyProjectionState::Custody(CeremonyState::Succeeded))
        );
        assert_eq!(projection.receipt_digest(), Some(&digest(9)));
        let encoded = serde_json::to_vec(&projection).unwrap();
        assert_eq!(
            serde_json::from_slice::<CeremonyProjection>(&encoded).unwrap(),
            projection
        );

        let approval = SealedApprovalPrepareResponse {
            approval_id: digest(10),
            state: ApprovalPrepareState::AwaitingCeremony,
            ceremony_url: "http://127.0.0.1:18734/c/approval".into(),
            ceremony_expires_at_ms: DecimalU64::new(3_000),
            review_manifest_digest: digest(11),
        };
        let mut projection = CeremonyProjection::from_approval_prepare(&approval, 2_000).unwrap();
        projection
            .reconcile_approval(
                &ApprovalPublicStatus {
                    approval_id: digest(10),
                    wallet_id: token("wallet"),
                    state: ApprovalLifecycleState::Expired,
                    effective_claim_assurance: None,
                    ceremony_url: Some("must-not-leak".into()),
                    ceremony_expires_at_ms: Some(DecimalU64::new(3_000)),
                },
                2_500,
            )
            .unwrap();
        assert_eq!(projection.ceremony_url(), None);
        assert_eq!(projection.approval_id(), Some(&digest(10)));
        assert_eq!(
            projection.state(),
            Some(CeremonyProjectionState::Approval(
                ApprovalLifecycleState::Expired
            ))
        );
    }

    #[tokio::test]
    async fn ceremony_status_and_cancel_use_shared_operation_surface() {
        let broker = Arc::new(MockBroker {
            wallet: WalletPublic {
                wallet_id: token("wallet"),
                wallet_kind: token("local"),
                key_refs: vec![key_ref()],
                policy_version: DecimalU64::new(1),
                policy_digest: digest(1),
                wallet_revocation_epoch: DecimalU64::new(0),
            },
            requests: Mutex::new(Vec::new()),
            corrupt_response: false,
        });
        let operation_id = OperationId::from_bytes([82; 32]);
        let client = MachineBrokerClient::new(broker.clone());
        let status = client.ceremony_status(operation_id.clone()).await.unwrap();
        assert_eq!(status.operation_id, operation_id);
        assert!(status.ceremony_url.is_some());
        let rebuilt = CeremonyProjection::from_custody_status(&status, 8_000).unwrap();
        assert_eq!(rebuilt.ceremony_url(), status.ceremony_url.as_deref());

        let cancelled = client.cancel_ceremony(operation_id.clone()).await.unwrap();
        assert_eq!(cancelled.operation_id, operation_id);
        assert_eq!(cancelled.state, CeremonyState::Cancelled);
        assert!(cancelled.ceremony_url.is_none());

        let requests = broker.requests.lock().unwrap();
        assert!(matches!(
            &requests[0],
            MachineBrokerRequest::CeremonyStatus(_)
        ));
        assert!(matches!(
            &requests[1],
            MachineBrokerRequest::CeremonyCancel(_)
        ));
    }

    #[tokio::test]
    async fn cross_operation_signing_response_fails_closed() {
        let broker = Arc::new(MockBroker {
            wallet: WalletPublic {
                wallet_id: token("wallet"),
                wallet_kind: token("local"),
                key_refs: vec![key_ref()],
                policy_version: DecimalU64::new(1),
                policy_digest: digest(1),
                wallet_revocation_epoch: DecimalU64::new(0),
            },
            requests: Mutex::new(Vec::new()),
            corrupt_response: true,
        });
        let payload = b"payload".to_vec();
        let hash = Digest32::from_bytes(Sha256::digest(&payload).into());
        let error = MachineBrokerClient::new(broker)
            .sign_petal_payload(TrustedPetalSignRequest {
                wallet_id: token("wallet"),
                preimage: payload,
                claimed_hash: hash.clone(),
                crypto_suite: CryptoSuite::Secp256k1Sha256Recoverable,
                operation_class: token("order.place"),
                selector: bloom_triad_protocol::PetalSignSelector::Reusable,
                claim: PetalUseClaim {
                    package_hash: digest(40),
                    route: "orders/place".into(),
                    operation_class: token("order.place"),
                    crypto_suite: CryptoSuite::Secp256k1Sha256Recoverable,
                    payload_digest: hash.clone(),
                    ordered_hashes: vec![hash],
                    declared_debits: vec![],
                    declared_destinations: vec![],
                    declared_fee: DeclaredFee::None,
                    nonce: RequestNonce::from_bytes([9; 16]),
                    claim_assurance: bloom_triad_protocol::ClaimAssurance::MachineAsserted,
                },
                claim_assurance_evidence: None,
                approval_id: Some(digest(52)),
                trusted_provenance: ProvenanceSubject::Petal {
                    package_hash: digest(40),
                    route: "orders/place".into(),
                },
                frozen_action: None,
                frozen_advisory: None,
            })
            .await
            .unwrap_err();
        assert_eq!(error.code, ProtocolErrorCode::OperationIdConflict);
    }

    fn token(value: &str) -> Token {
        Token::new(value).unwrap()
    }

    #[tokio::test]
    async fn unix_service_transports_authenticated_signed_envelopes() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("broker.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let uid = std::fs::metadata(directory.path()).unwrap().uid();
        let machine = local_identity("bloom-machine", "machine-key", 7);
        let broker = local_identity("bloom-broker", "broker-key", 8);
        let machine_acl = peer_acl(uid, &machine);
        let broker_acl = peer_acl(uid, &broker);
        let expected = digest(42);
        let server_expected = expected.clone();
        let server_identity = broker.clone();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request = bloom_triad_local_transport::receive_request::<MachineBrokerRequest>(
                &mut stream,
                &server_identity,
                &machine_acl,
            )
            .await
            .unwrap();
            assert_eq!(
                request.unsigned.body,
                MachineBrokerRequest::ActionValidate(server_expected.clone())
            );
            let response: Result<MachineBrokerResponse, ProtocolError> =
                Ok(MachineBrokerResponse::ActionValidate(server_expected));
            bloom_triad_local_transport::send_response(
                &mut stream,
                &server_identity,
                &request,
                response,
            )
            .await
            .unwrap();
        });

        let response = MachineBrokerClient::connect_unix(socket, machine, broker_acl)
            .request(MachineBrokerRequest::ActionValidate(expected.clone()))
            .await
            .unwrap();
        assert_eq!(response, MachineBrokerResponse::ActionValidate(expected));
        server.await.unwrap();
    }

    #[test]
    fn security_files_fail_closed_on_writable_or_non_root_metadata() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let identity_path = directory.path().join("machine-identity.json");
        let manifest_path = directory.path().join("edge-manifest.json");
        std::fs::write(&identity_path, b"{}").unwrap();
        std::fs::write(&manifest_path, b"{}").unwrap();

        std::fs::set_permissions(&identity_path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let error =
            MachineBrokerClient::connect_unix_from_files("unused", &identity_path, &manifest_path)
                .err()
                .expect("insecure identity metadata must fail");
        assert_eq!(error.code, ProtocolErrorCode::UnauthenticatedPeer);

        std::fs::set_permissions(&identity_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::set_permissions(&manifest_path, std::fs::Permissions::from_mode(0o666)).unwrap();
        let error =
            MachineBrokerClient::connect_unix_from_files("unused", &identity_path, &manifest_path)
                .err()
                .expect("writable manifest metadata must fail");
        assert_eq!(error.code, ProtocolErrorCode::UnauthenticatedPeer);
    }

    fn local_identity(service: &str, key_id: &str, byte: u8) -> LocalIdentity {
        LocalIdentity {
            service_id: token(service),
            boot_epoch: bloom_triad_protocol::BootEpoch::from_bytes([byte; 16]),
            application_key_id: token(key_id),
            signing_key: Arc::new(SigningKey::from_bytes(&[byte; 32])),
        }
    }

    fn peer_acl(uid: u32, identity: &LocalIdentity) -> PeerAcl {
        PeerAcl {
            effective_uid: uid,
            service_id: identity.service_id.clone(),
            boot_epoch: identity.boot_epoch.clone(),
            application_key_id: identity.application_key_id.clone(),
            application_public_key: identity.signing_key.verifying_key().to_bytes(),
        }
    }

    fn digest(byte: u8) -> Digest32 {
        Digest32::from_bytes([byte; 32])
    }

    fn key_ref() -> KeyRef {
        KeyRef {
            backend: token("local"),
            backend_instance: token("primary"),
            locator: "wallet/root".into(),
            key_spec: KeySpec::Secp256k1,
            public_key_fingerprint: digest(3),
            derivation: None,
        }
    }
}
