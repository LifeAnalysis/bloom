use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::{
    Base64UrlBytes, BootEpoch, ClaimAssurance, CryptoSuite, DecimalU64, Digest32, KeyRef,
    OperationId, PetalUseClaim, ProtocolError, ProtocolErrorCode, ProvenanceSubject,
    SigningPayloads, Token,
};

const SIGN_OPERATION_DOMAIN: &[u8] = b"bloom-sign-operation/v1";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MachineSignRequest {
    pub operation_id: OperationId,
    pub operation_digest: Digest32,
    pub approval_id: Digest32,
    pub key_ref: KeyRef,
    pub crypto_suite: CryptoSuite,
    pub payloads: SigningPayloads,
    pub petal_use_claim: Option<PetalUseClaim>,
    pub claim_assurance_evidence: Option<Base64UrlBytes>,
    pub provenance: ProvenanceSubject,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectorKind {
    Exact,
    Petal,
}

/// Guest-selected approval shape for an explicitly scoped Petal signing key.
/// This is intentionally distinct from the Broker's wire-level
/// [`SelectorKind`]: `Reusable` maps to the Petal selector while `Exact` maps
/// to the payload-digest selector.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PetalSignSelector {
    Exact,
    Reusable,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SignOperationIdentity {
    pub operation_id: OperationId,
    pub approval_id: Digest32,
    pub key_ref: KeyRef,
    pub crypto_suite: CryptoSuite,
    pub ordered_payload_digests: Vec<Digest32>,
    pub ordered_hashes: Vec<Digest32>,
    pub petal_use_claim_digest: Option<Digest32>,
    pub claim_assurance_digest: Option<Digest32>,
    pub policy_version: DecimalU64,
    pub policy_digest: Digest32,
}

impl SignOperationIdentity {
    pub fn digest(&self) -> Result<Digest32, ProtocolError> {
        let mut hasher = Sha256::new();
        hasher.update(SIGN_OPERATION_DOMAIN);
        hasher.update(serde_jcs::to_vec(self).map_err(|error| {
            ProtocolError::new(
                ProtocolErrorCode::MalformedFrame,
                format!("operation identity JCS encoding failed: {error}"),
            )
        })?);
        Ok(Digest32::from_bytes(hasher.finalize().into()))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UnsignedSignRequest {
    pub schema: Token,
    pub attempt_id: Digest32,
    pub operation_id: OperationId,
    pub operation_digest: Digest32,
    pub attempt_digest: Digest32,
    pub audience: Token,
    pub issuer_service_id: Token,
    pub issuer_boot_epoch: BootEpoch,
    pub broker_signing_key_id: Token,
    pub approval_id: Digest32,
    pub wallet_id: Token,
    pub key_ref: KeyRef,
    pub crypto_suite: CryptoSuite,
    pub selector_kind: SelectorKind,
    pub ordered_payload_digests: Vec<Digest32>,
    pub ordered_hashes: Vec<Digest32>,
    pub signature_count: DecimalU64,
    pub petal_use_claim_digest: Option<Digest32>,
    pub claim_assurance_digest: Option<Digest32>,
    pub policy_version: DecimalU64,
    pub policy_digest: Digest32,
    pub validation_receipt_digest: Digest32,
    pub issued_at_ms: DecimalU64,
    pub not_before_ms: DecimalU64,
    pub expires_at_ms: DecimalU64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SignRequest {
    #[serde(flatten)]
    pub unsigned: UnsignedSignRequest,
    pub broker_signature: Base64UrlBytes,
}

impl SignRequest {
    pub fn validate_shape(&self) -> Result<(), ProtocolError> {
        if self.unsigned.schema.as_str() != "bloom.sign-request/1"
            || self.unsigned.audience.as_str() != "bloom-signer"
            || self.unsigned.expires_at_ms.get() <= self.unsigned.not_before_ms.get()
            || self.unsigned.issued_at_ms.get() > self.unsigned.expires_at_ms.get()
            || self
                .unsigned
                .expires_at_ms
                .get()
                .saturating_sub(self.unsigned.issued_at_ms.get())
                > 30_000
            || self.unsigned.signature_count.get() != self.unsigned.ordered_hashes.len() as u64
        {
            return Err(ProtocolError::new(
                ProtocolErrorCode::MalformedFrame,
                "SignRequest structural constraints failed",
            ));
        }
        if self.unsigned.operation_digest != self.unsigned.operation_identity().digest()?
            || self.unsigned.attempt_digest != self.unsigned.computed_attempt_digest()?
        {
            return Err(ProtocolError::new(
                ProtocolErrorCode::OperationIdConflict,
                "SignRequest operation or attempt digest mismatch",
            ));
        }
        Ok(())
    }
}

impl UnsignedSignRequest {
    pub fn operation_identity(&self) -> SignOperationIdentity {
        SignOperationIdentity {
            operation_id: self.operation_id.clone(),
            approval_id: self.approval_id.clone(),
            key_ref: self.key_ref.clone(),
            crypto_suite: self.crypto_suite,
            ordered_payload_digests: self.ordered_payload_digests.clone(),
            ordered_hashes: self.ordered_hashes.clone(),
            petal_use_claim_digest: self.petal_use_claim_digest.clone(),
            claim_assurance_digest: self.claim_assurance_digest.clone(),
            policy_version: self.policy_version.clone(),
            policy_digest: self.policy_digest.clone(),
        }
    }

    pub fn canonical_attempt_preimage(&self) -> Result<Vec<u8>, ProtocolError> {
        let mut value = serde_json::to_value(self).map_err(|error| {
            ProtocolError::new(
                ProtocolErrorCode::MalformedFrame,
                format!("attempt serialization failed: {error}"),
            )
        })?;
        let object = value.as_object_mut().ok_or_else(|| {
            ProtocolError::new(
                ProtocolErrorCode::MalformedFrame,
                "attempt must serialize as a JSON object",
            )
        })?;
        object.remove("attempt_digest");
        serde_jcs::to_vec(&value).map_err(|error| {
            ProtocolError::new(
                ProtocolErrorCode::MalformedFrame,
                format!("attempt JCS encoding failed: {error}"),
            )
        })
    }

    pub fn computed_attempt_digest(&self) -> Result<Digest32, ProtocolError> {
        Ok(Digest32::from_bytes(
            Sha256::digest(self.canonical_attempt_preimage()?).into(),
        ))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerValidationReceipt {
    pub approval_id: Digest32,
    pub approval_digest: Digest32,
    pub operation_digest: Digest32,
    pub policy_version: DecimalU64,
    pub policy_digest: Digest32,
    pub claim_digest: Option<Digest32>,
    pub assurance_digest: Option<Digest32>,
    pub reservation_ids: Vec<Digest32>,
    pub effective_claim_assurance: Option<ClaimAssurance>,
    pub broker_key_id: Token,
    pub broker_signature: Base64UrlBytes,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedSignature {
    pub crypto_suite: CryptoSuite,
    pub bytes: Base64UrlBytes,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SigningResult {
    pub operation_id: OperationId,
    pub operation_digest: Digest32,
    pub signatures: Vec<NormalizedSignature>,
    pub signer_receipt_digest: Digest32,
    pub broker_receipt_digest: Digest32,
}
