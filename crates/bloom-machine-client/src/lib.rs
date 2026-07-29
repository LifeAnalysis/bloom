//! Machine-owned, keyless client surface for the Broker.
//!
//! This crate intentionally knows only the public Machine↔Broker protocol. It
//! contains no private-key, WKEK, PRF, provider-credential, or custody
//! plaintext type.

#![forbid(unsafe_code)]

use std::{path::PathBuf, sync::Arc};

use bloom_triad_protocol::{
    ApprovalLifecycleState, ApprovalPrepareRequest, ApprovalPublicStatus, Base64UrlBytes,
    CeremonyPublicStatus, CeremonyState, CredentialPublic, CryptoSuite, CustodyPrepareRequest,
    CustodyPrepareResponse, CustodyResult, DecimalU64, Digest32, IdRequest, KeyPublic, KeyRef,
    KeyRequest, MachineBrokerRequest, MachineBrokerResponse, MachineBrokerService,
    MachineSignRequest, OperationId, OperationRequest, PetalUseClaim, ProtocolError,
    ProtocolErrorCode, ProvenanceSubject, SealedApprovalPrepareResponse, SignOperationIdentity,
    SigningPayloads, SigningResult, Token, WalletPublic, WalletRequest,
};
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use sha3::Keccak256;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const FRAME_MAX_BYTES: usize = 1024 * 1024;

/// Production Machine→Broker connector. It carries only the public typed
/// protocol over a bounded Unix socket frame; peer/application envelope
/// authentication is layered by the W8 process transport.
#[derive(Clone, Debug)]
pub struct UnixMachineBrokerService {
    socket_path: PathBuf,
}

impl UnixMachineBrokerService {
    pub fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: socket_path.into(),
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
            let frame = bloom_triad_protocol::encode_frame(&request)?;
            stream
                .write_all(&frame)
                .await
                .map_err(|error| service_unavailable(format!("write Broker request: {error}")))?;
            stream
                .shutdown()
                .await
                .map_err(|error| service_unavailable(format!("finish Broker request: {error}")))?;

            let mut prefix = [0_u8; 4];
            stream
                .read_exact(&mut prefix)
                .await
                .map_err(|error| service_unavailable(format!("read Broker response: {error}")))?;
            let length = u32::from_be_bytes(prefix) as usize;
            if length > FRAME_MAX_BYTES {
                return Err(ProtocolError::new(
                    ProtocolErrorCode::LimitExceededFrame,
                    "Broker response frame exceeds 1 MiB",
                ));
            }
            let mut payload = vec![0_u8; length];
            stream
                .read_exact(&mut payload)
                .await
                .map_err(|error| service_unavailable(format!("read Broker response: {error}")))?;
            let mut response_frame = Vec::with_capacity(length + 4);
            response_frame.extend_from_slice(&prefix);
            response_frame.extend_from_slice(&payload);
            bloom_triad_protocol::decode_frame::<Result<MachineBrokerResponse, ProtocolError>>(
                &response_frame,
            )?
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

    pub fn connect_unix(socket_path: impl Into<PathBuf>) -> Self {
        Self::new(Arc::new(UnixMachineBrokerService::new(socket_path)))
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
        request.validate()?;
        let wallet = self.wallet(request.wallet_id.clone()).await?;
        let key_ref = unique_key_for_suite(&wallet.key_refs, request.crypto_suite)?;
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
        let claim_digest = jcs_digest(&request.claim)?;
        let assurance_digest = jcs_digest(&request.claim.claim_assurance)?;
        let operation_digest = SignOperationIdentity {
            operation_id: operation_id.clone(),
            approval_id: approval_id.clone(),
            key_ref: key_ref.clone(),
            crypto_suite: request.crypto_suite,
            ordered_payload_digests: vec![payload_digest],
            ordered_hashes: vec![ordered_hash],
            petal_use_claim_digest: Some(claim_digest),
            claim_assurance_digest: Some(assurance_digest),
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
            petal_use_claim: Some(request.claim),
            claim_assurance_evidence: request
                .claim_assurance_evidence
                .as_deref()
                .map(Base64UrlBytes::from_bytes),
            provenance: request.trusted_provenance,
        })
        .await
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
    pub claim: PetalUseClaim,
    pub claim_assurance_evidence: Option<Vec<u8>>,
    pub approval_id: Option<Digest32>,
    pub trusted_provenance: ProvenanceSubject,
    pub frozen_action: Option<Vec<u8>>,
    pub frozen_advisory: Option<Vec<u8>>,
}

/// Machine/VFS projection of a Broker-owned ceremony. The URL is present only
/// while the Broker reports an actionable awaiting state and before expiry.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CeremonyProjection {
    identity: Option<CeremonyProjectionIdentity>,
    ceremony_url: Option<String>,
    expires_at_ms: Option<DecimalU64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum CeremonyProjectionIdentity {
    Approval(Digest32),
    Custody {
        operation_id: OperationId,
        ceremony_kind: bloom_triad_protocol::CeremonyKind,
    },
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
            CeremonyProjectionIdentity::Approval(response.approval_id.clone()),
            response.ceremony_url.clone(),
            response.ceremony_expires_at_ms.clone(),
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
            response.ceremony_url.clone(),
            response.ceremony_expires_at_ms.clone(),
            now_ms,
        )
    }

    pub fn reconcile_approval(
        &mut self,
        status: &ApprovalPublicStatus,
        now_ms: u64,
    ) -> Result<(), ProtocolError> {
        if self.identity
            != Some(CeremonyProjectionIdentity::Approval(
                status.approval_id.clone(),
            ))
        {
            self.clear();
            return Err(projection_mismatch(
                "approval status does not match originating projection",
            ));
        }
        if status.state == ApprovalLifecycleState::AwaitingCeremony {
            match (&status.ceremony_url, &status.ceremony_expires_at_ms) {
                (Some(url), Some(expiry))
                    if self.ceremony_url.as_ref() == Some(url)
                        && self.expires_at_ms.as_ref() == Some(expiry)
                        && expiry.get() > now_ms => {}
                (Some(_), Some(_)) => {
                    self.clear();
                    return Err(projection_mismatch(
                        "approval ceremony URL or expiry changed",
                    ));
                }
                _ => self.clear(),
            }
        } else {
            self.clear();
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
            self.clear();
            return Err(projection_mismatch(
                "custody status does not match originating projection",
            ));
        }
        if status.state == CeremonyState::AwaitingUser
            && self.ceremony_url.is_some()
            && self.expires_at_ms.as_ref() == Some(&status.expires_at_ms)
            && status.expires_at_ms.get() > now_ms
        {
            return Ok(());
        }
        self.clear();
        Ok(())
    }

    pub fn ceremony_url(&self) -> Option<&str> {
        self.ceremony_url.as_deref()
    }

    pub fn expires_at_ms(&self) -> Option<u64> {
        self.expires_at_ms.as_ref().map(DecimalU64::get)
    }

    pub fn clear(&mut self) {
        self.identity = None;
        self.ceremony_url = None;
        self.expires_at_ms = None;
    }

    fn awaiting(
        identity: CeremonyProjectionIdentity,
        url: String,
        expiry: DecimalU64,
        now_ms: u64,
    ) -> Result<Self, ProtocolError> {
        if url.is_empty() || expiry.get() <= now_ms {
            Err(projection_mismatch(
                "ceremony URL must be non-empty and unexpired",
            ))
        } else {
            Ok(Self {
                identity: Some(identity),
                ceremony_url: Some(url),
                expires_at_ms: Some(expiry),
            })
        }
    }
}

fn projection_mismatch(message: &str) -> ProtocolError {
    ProtocolError::new(ProtocolErrorCode::MalformedFrame, message)
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
    use std::sync::Mutex;

    use bloom_triad_protocol::{
        ApprovalPrepareState, CeremonyKind, CustodyPrepareState, DeclaredFee, KeySpec,
        NormalizedSignature, RequestNonce, ServiceFuture, SignatureEncoding,
    };

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
            claim_assurance_evidence: None,
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
                    receipt_digest: Some(digest(9)),
                },
                1_999,
            )
            .unwrap();
        assert_eq!(projection, CeremonyProjection::default());

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
        assert_eq!(projection, CeremonyProjection::default());
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
    async fn unix_service_transports_bounded_typed_frames() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("broker.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let expected = digest(42);
        let server_expected = expected.clone();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut prefix = [0_u8; 4];
            stream.read_exact(&mut prefix).await.unwrap();
            let length = u32::from_be_bytes(prefix) as usize;
            let mut payload = vec![0_u8; length];
            stream.read_exact(&mut payload).await.unwrap();
            let mut frame = prefix.to_vec();
            frame.extend_from_slice(&payload);
            assert_eq!(
                bloom_triad_protocol::decode_frame::<MachineBrokerRequest>(&frame).unwrap(),
                MachineBrokerRequest::ActionValidate(server_expected.clone())
            );
            let response: Result<MachineBrokerResponse, ProtocolError> =
                Ok(MachineBrokerResponse::ActionValidate(server_expected));
            stream
                .write_all(&bloom_triad_protocol::encode_frame(&response).unwrap())
                .await
                .unwrap();
        });

        let response = MachineBrokerClient::connect_unix(socket)
            .request(MachineBrokerRequest::ActionValidate(expected.clone()))
            .await
            .unwrap();
        assert_eq!(response, MachineBrokerResponse::ActionValidate(expected));
        server.await.unwrap();
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
