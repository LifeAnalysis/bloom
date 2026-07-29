use serde::{Deserialize, Serialize};

use crate::{
    ActivationMode, Base64UrlBytes, ClaimAssurance, CryptoSuite, DecimalU64, Digest32,
    HpkeEnvelope, KeyRef, OperationId, PetalUseClaim, SealedApprovalTerms, Token,
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CeremonyKind {
    SealedApproval,
    WalletRegistration,
    WalletImport,
    WalletExport,
    WalletDelete,
    WalletRecovery,
    CredentialAdd,
    CredentialReplace,
    CredentialRemove,
    BackendEnrollment,
    KeyDerive,
    PolicyUpdate,
}

impl CeremonyKind {
    /// Normative successful terminal state from §13.6. Registration completes
    /// only after its recovery-output acknowledgement; every other custody
    /// workflow uses the generic/credential `SUCCEEDED` terminal.
    pub const fn successful_terminal_state(self) -> Option<crate::CeremonyState> {
        match self {
            Self::SealedApproval => None,
            Self::WalletRegistration => Some(crate::CeremonyState::Completed),
            Self::WalletImport
            | Self::WalletExport
            | Self::WalletDelete
            | Self::WalletRecovery
            | Self::CredentialAdd
            | Self::CredentialReplace
            | Self::CredentialRemove
            | Self::BackendEnrollment
            | Self::KeyDerive
            | Self::PolicyUpdate => Some(crate::CeremonyState::Succeeded),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewManifest {
    pub schema: Token,
    pub approval_id: Digest32,
    pub approval_digest: Digest32,
    pub canonical_plan: String,
    pub canonical_plan_digest: Digest32,
    pub exact_payload_digests: Vec<Digest32>,
    pub exact_hashes: Vec<Digest32>,
    pub petal_use_claim: Option<PetalUseClaim>,
    pub claim_assurance: Option<ClaimAssurance>,
    pub attributed_advisory_items: Vec<String>,
    pub issued_at_ms: DecimalU64,
    pub expires_at_ms: DecimalU64,
    pub broker_key_id: Token,
    pub broker_signature: Base64UrlBytes,
}

impl ReviewManifest {
    pub fn unsigned_canonical_bytes(&self) -> Result<Vec<u8>, crate::ProtocolError> {
        #[derive(Serialize)]
        struct Unsigned<'a> {
            schema: &'a Token,
            approval_id: &'a Digest32,
            approval_digest: &'a Digest32,
            canonical_plan: &'a str,
            canonical_plan_digest: &'a Digest32,
            exact_payload_digests: &'a [Digest32],
            exact_hashes: &'a [Digest32],
            petal_use_claim: &'a Option<PetalUseClaim>,
            claim_assurance: &'a Option<ClaimAssurance>,
            attributed_advisory_items: &'a [String],
            issued_at_ms: &'a DecimalU64,
            expires_at_ms: &'a DecimalU64,
            broker_key_id: &'a Token,
        }
        serde_jcs::to_vec(&Unsigned {
            schema: &self.schema,
            approval_id: &self.approval_id,
            approval_digest: &self.approval_digest,
            canonical_plan: &self.canonical_plan,
            canonical_plan_digest: &self.canonical_plan_digest,
            exact_payload_digests: &self.exact_payload_digests,
            exact_hashes: &self.exact_hashes,
            petal_use_claim: &self.petal_use_claim,
            claim_assurance: &self.claim_assurance,
            attributed_advisory_items: &self.attributed_advisory_items,
            issued_at_ms: &self.issued_at_ms,
            expires_at_ms: &self.expires_at_ms,
            broker_key_id: &self.broker_key_id,
        })
        .map_err(canonical_error)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CeremonyPrepareRequest {
    pub activation_operation_id: OperationId,
    pub terms: SealedApprovalTerms,
    pub review_manifest_digest: Digest32,
    pub exact_ordered_payload_digests: Vec<Digest32>,
    pub exact_ordered_hashes: Vec<Digest32>,
    pub replacement_approval_id: Option<Digest32>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SignerCeremonyContribution {
    pub ceremony_id: Digest32,
    pub signer_nonce: Digest32,
    pub approval_digest: Digest32,
    pub review_manifest_digest: Digest32,
    pub key_ref: KeyRef,
    pub allowed_crypto_suites: Vec<CryptoSuite>,
    pub activation_mode: ActivationMode,
    pub wallet_revocation_epoch: DecimalU64,
    pub required_user_verification: bool,
    pub ephemeral_encryption_public_key: Option<Base64UrlBytes>,
    pub expires_at_ms: DecimalU64,
    pub signer_key_id: Token,
    pub signer_signature: Base64UrlBytes,
}

/// Complete Signer-owned material needed for Broker to render and verify an
/// approval ceremony. The signed contribution alone is insufficient because
/// WebAuthn challenge bytes and credential options are also Signer-derived.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SignerPreparedApproval {
    pub contribution: SignerCeremonyContribution,
    pub challenges: Vec<CeremonyChallenge>,
    pub webauthn_options: CeremonyWebAuthnOptions,
    pub verification_credentials: Vec<WebAuthnCredential>,
}

impl SignerCeremonyContribution {
    pub fn unsigned_canonical_bytes(&self) -> Result<Vec<u8>, crate::ProtocolError> {
        #[derive(Serialize)]
        struct Unsigned<'a> {
            ceremony_id: &'a Digest32,
            signer_nonce: &'a Digest32,
            approval_digest: &'a Digest32,
            review_manifest_digest: &'a Digest32,
            key_ref: &'a KeyRef,
            allowed_crypto_suites: &'a [CryptoSuite],
            activation_mode: ActivationMode,
            wallet_revocation_epoch: &'a DecimalU64,
            required_user_verification: bool,
            ephemeral_encryption_public_key: &'a Option<Base64UrlBytes>,
            expires_at_ms: &'a DecimalU64,
            signer_key_id: &'a Token,
        }
        serde_jcs::to_vec(&Unsigned {
            ceremony_id: &self.ceremony_id,
            signer_nonce: &self.signer_nonce,
            approval_digest: &self.approval_digest,
            review_manifest_digest: &self.review_manifest_digest,
            key_ref: &self.key_ref,
            allowed_crypto_suites: &self.allowed_crypto_suites,
            activation_mode: self.activation_mode.clone(),
            wallet_revocation_epoch: &self.wallet_revocation_epoch,
            required_user_verification: self.required_user_verification,
            ephemeral_encryption_public_key: &self.ephemeral_encryption_public_key,
            expires_at_ms: &self.expires_at_ms,
            signer_key_id: &self.signer_key_id,
        })
        .map_err(canonical_error)
    }

    pub fn digest(&self) -> Result<Digest32, crate::ProtocolError> {
        digest_canonical(serde_jcs::to_vec(self).map_err(canonical_error)?)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WebAuthnAssertion {
    pub credential_id: Base64UrlBytes,
    pub authenticator_data: Base64UrlBytes,
    pub client_data_json: Base64UrlBytes,
    pub signature: Base64UrlBytes,
    pub user_handle: Option<Base64UrlBytes>,
}

/// Raw WebAuthn credential-creation response.
///
/// Broker and Signer each verify these bytes. The protocol deliberately does
/// not carry a Broker-produced "verified" boolean or parsed public key.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WebAuthnAttestation {
    pub credential_id: Base64UrlBytes,
    pub client_data_json: Base64UrlBytes,
    pub attestation_object: Base64UrlBytes,
    pub transports: Vec<Token>,
}

/// Public credential metadata owned by Signer after successful attestation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WebAuthnCredential {
    pub credential_id: Base64UrlBytes,
    pub cose_public_key: Base64UrlBytes,
    pub user_handle: Base64UrlBytes,
    pub rp_id: Token,
    pub prf_salt: Base64UrlBytes,
    pub sign_count: DecimalU64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialPrfInput {
    pub credential_id: Base64UrlBytes,
    pub prf_salt: Base64UrlBytes,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CeremonyWebAuthnOptions {
    pub allowed_credentials: Vec<CredentialPrfInput>,
    pub registration_user_handle: Option<Base64UrlBytes>,
    pub registration_prf_salt: Option<Base64UrlBytes>,
}

/// The exact browser proof phases permitted for a ceremony.
///
/// Registration needs a creation attestation. Credential changes additionally
/// need an assertion from existing root authority. Recovery authenticates with
/// encrypted recovery input instead of pretending an old passkey is present.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WebAuthnCeremonyProof {
    Assertion {
        assertion: WebAuthnAssertion,
    },
    Registration {
        attestation: WebAuthnAttestation,
        prf_assertion: Option<WebAuthnAssertion>,
    },
    AuthorityCredentialChange {
        authority_assertion: WebAuthnAssertion,
        new_credential_attestation: WebAuthnAttestation,
        new_credential_prf_assertion: Option<WebAuthnAssertion>,
    },
    RecoveryCredentialChange {
        new_credential_attestation: WebAuthnAttestation,
        new_credential_prf_assertion: Option<WebAuthnAssertion>,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CeremonyCompleteRequest {
    pub activation_operation_id: OperationId,
    pub proof: WebAuthnCeremonyProof,
    pub contribution: SignerCeremonyContribution,
    pub encrypted_local_prf: Option<HpkeEnvelope>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SignerActivationReceipt {
    pub activation_operation_id: OperationId,
    pub ceremony_id: Digest32,
    pub approval_id: Digest32,
    pub approval_digest: Digest32,
    pub review_manifest_digest: Digest32,
    pub key_ref: KeyRef,
    pub allowed_crypto_suites: Vec<CryptoSuite>,
    pub activation_mode: ActivationMode,
    pub wallet_revocation_epoch: DecimalU64,
    pub replaced_approval_id: Option<Digest32>,
    pub activated_at_ms: DecimalU64,
    pub expires_at_ms: DecimalU64,
    pub signer_key_id: Token,
    pub signer_signature: Base64UrlBytes,
}

impl SignerActivationReceipt {
    pub fn unsigned_canonical_bytes(&self) -> Result<Vec<u8>, crate::ProtocolError> {
        #[derive(Serialize)]
        struct Unsigned<'a> {
            activation_operation_id: &'a OperationId,
            ceremony_id: &'a Digest32,
            approval_id: &'a Digest32,
            approval_digest: &'a Digest32,
            review_manifest_digest: &'a Digest32,
            key_ref: &'a KeyRef,
            allowed_crypto_suites: &'a [CryptoSuite],
            activation_mode: ActivationMode,
            wallet_revocation_epoch: &'a DecimalU64,
            replaced_approval_id: &'a Option<Digest32>,
            activated_at_ms: &'a DecimalU64,
            expires_at_ms: &'a DecimalU64,
            signer_key_id: &'a Token,
        }
        serde_jcs::to_vec(&Unsigned {
            activation_operation_id: &self.activation_operation_id,
            ceremony_id: &self.ceremony_id,
            approval_id: &self.approval_id,
            approval_digest: &self.approval_digest,
            review_manifest_digest: &self.review_manifest_digest,
            key_ref: &self.key_ref,
            allowed_crypto_suites: &self.allowed_crypto_suites,
            activation_mode: self.activation_mode.clone(),
            wallet_revocation_epoch: &self.wallet_revocation_epoch,
            replaced_approval_id: &self.replaced_approval_id,
            activated_at_ms: &self.activated_at_ms,
            expires_at_ms: &self.expires_at_ms,
            signer_key_id: &self.signer_key_id,
        })
        .map_err(canonical_error)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SealedApprovalPrepareResponse {
    pub approval_id: Digest32,
    pub state: ApprovalPrepareState,
    pub ceremony_url: String,
    pub ceremony_expires_at_ms: DecimalU64,
    pub review_manifest_digest: Digest32,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ApprovalPrepareState {
    #[serde(rename = "AWAITING_CEREMONY")]
    AwaitingCeremony,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CeremonySession {
    pub ceremony_id: Digest32,
    pub ceremony_kind: CeremonyKind,
    pub operation_id: OperationId,
    pub review_manifest_digest: Digest32,
    pub signer_nonce: Digest32,
    pub signer_contribution: SignerSessionContribution,
    pub webauthn_options: serde_json::Value,
    pub required_user_verification: bool,
    pub hpke_recipient_key: Base64UrlBytes,
    pub browser_output_recipient_key: Option<Base64UrlBytes>,
    pub expires_at_ms: DecimalU64,
    pub single_use: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "contribution", rename_all = "snake_case")]
pub enum SignerSessionContribution {
    SealedApproval(SignerCeremonyContribution),
    Custody(CustodySignerContribution),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CustodyPrepareRequest {
    pub ceremony_kind: CeremonyKind,
    pub custody_operation_id: OperationId,
    pub wallet_id: Option<Token>,
    pub key_ref: Option<KeyRef>,
    pub exact_terms_digest: Digest32,
    pub expected_input_class: Token,
    pub browser_output_recipient_key: Option<Base64UrlBytes>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CustodySignerContribution {
    pub ceremony_id: Digest32,
    pub ceremony_kind: CeremonyKind,
    pub custody_operation_id: OperationId,
    pub signer_nonce: Digest32,
    pub review_manifest_digest: Digest32,
    pub wallet_id: Option<Token>,
    pub key_ref: Option<KeyRef>,
    pub expected_input_class: Token,
    pub required_user_verification: bool,
    pub hpke_recipient_key: Base64UrlBytes,
    pub browser_output_recipient_key: Option<Base64UrlBytes>,
    pub expires_at_ms: DecimalU64,
    pub signer_key_id: Token,
    pub signer_signature: Base64UrlBytes,
}

/// Complete Signer-owned material needed for Broker to render and verify a
/// custody ceremony.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SignerPreparedCustody {
    pub contribution: CustodySignerContribution,
    pub challenges: Vec<CeremonyChallenge>,
    pub webauthn_options: CeremonyWebAuthnOptions,
    pub verification_credentials: Vec<WebAuthnCredential>,
}

/// Restart-safe Broker-facing ceremony status. Terminal receipts are returned
/// verbatim so Broker can reconcile without replaying a browser proof.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "state", content = "result", rename_all = "snake_case")]
pub enum SignerCeremonyStatus {
    Pending,
    CompletedApproval(Box<SignerActivationReceipt>),
    CompletedCustody(Box<CustodyResult>),
    Missing,
}

/// The generic `ceremony.prepare` body. Policy update is the sole custody kind
/// whose semantic review is originated by Broker and therefore shares this
/// method with sealed-approval preparation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "ceremony_kind", content = "request", rename_all = "snake_case")]
pub enum SignerCeremonyPrepareRequest {
    SealedApproval(Box<CeremonyPrepareRequest>),
    PolicyUpdate(Box<crate::PolicyUpdateCeremonyPrepareRequest>),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "ceremony_kind", content = "prepared", rename_all = "snake_case")]
pub enum SignerCeremonyPrepareResponse {
    SealedApproval(SignerPreparedApproval),
    PolicyUpdate(SignerPreparedCustody),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "ceremony_kind", content = "request", rename_all = "snake_case")]
pub enum SignerCeremonyCompleteRequest {
    SealedApproval(Box<CeremonyCompleteRequest>),
    PolicyUpdate(Box<crate::PolicyUpdateCeremonyCompleteRequest>),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "ceremony_kind", content = "result", rename_all = "snake_case")]
pub enum SignerCeremonyCompleteResponse {
    SealedApproval(Box<SignerActivationReceipt>),
    PolicyUpdate(Box<CustodyResult>),
}

impl CustodySignerContribution {
    pub fn unsigned_canonical_bytes(&self) -> Result<Vec<u8>, crate::ProtocolError> {
        #[derive(Serialize)]
        struct Unsigned<'a> {
            ceremony_id: &'a Digest32,
            ceremony_kind: CeremonyKind,
            custody_operation_id: &'a OperationId,
            signer_nonce: &'a Digest32,
            review_manifest_digest: &'a Digest32,
            wallet_id: &'a Option<Token>,
            key_ref: &'a Option<KeyRef>,
            expected_input_class: &'a Token,
            required_user_verification: bool,
            hpke_recipient_key: &'a Base64UrlBytes,
            browser_output_recipient_key: &'a Option<Base64UrlBytes>,
            expires_at_ms: &'a DecimalU64,
            signer_key_id: &'a Token,
        }
        serde_jcs::to_vec(&Unsigned {
            ceremony_id: &self.ceremony_id,
            ceremony_kind: self.ceremony_kind,
            custody_operation_id: &self.custody_operation_id,
            signer_nonce: &self.signer_nonce,
            review_manifest_digest: &self.review_manifest_digest,
            wallet_id: &self.wallet_id,
            key_ref: &self.key_ref,
            expected_input_class: &self.expected_input_class,
            required_user_verification: self.required_user_verification,
            hpke_recipient_key: &self.hpke_recipient_key,
            browser_output_recipient_key: &self.browser_output_recipient_key,
            expires_at_ms: &self.expires_at_ms,
            signer_key_id: &self.signer_key_id,
        })
        .map_err(canonical_error)
    }

    pub fn digest(&self) -> Result<Digest32, crate::ProtocolError> {
        digest_canonical(serde_jcs::to_vec(self).map_err(canonical_error)?)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CustodyPrepareResponse {
    pub ceremony_kind: CeremonyKind,
    pub custody_operation_id: OperationId,
    pub state: CustodyPrepareState,
    pub ceremony_url: String,
    pub ceremony_expires_at_ms: DecimalU64,
    pub signer_contribution_digest: Digest32,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum CustodyPrepareState {
    #[serde(rename = "AWAITING_USER")]
    AwaitingUser,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CustodyCompleteRequest {
    pub ceremony_kind: CeremonyKind,
    pub custody_operation_id: OperationId,
    pub ceremony_id: Digest32,
    pub proof: WebAuthnCeremonyProof,
    pub encrypted_input: Option<HpkeEnvelope>,
    pub public_binding_digest: Digest32,
}

/// Canonical challenge payload embedded as the WebAuthn challenge bytes.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CeremonyChallenge {
    pub schema: Token,
    pub ceremony_id: Digest32,
    pub ceremony_kind: CeremonyKind,
    pub operation_id: OperationId,
    pub signer_nonce: Digest32,
    pub review_manifest_digest: Digest32,
    pub signer_contribution_digest: Digest32,
    pub exact_terms_digest: Digest32,
    pub phase: CeremonyPhase,
}

impl CeremonyChallenge {
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, crate::ProtocolError> {
        serde_jcs::to_vec(self).map_err(|error| {
            crate::ProtocolError::new(crate::ProtocolErrorCode::MalformedFrame, error.to_string())
        })
    }

    pub fn webauthn_challenge(&self) -> Result<Base64UrlBytes, crate::ProtocolError> {
        Ok(Base64UrlBytes::from_bytes(&self.canonical_bytes()?))
    }

    pub fn digest(&self) -> Result<Digest32, crate::ProtocolError> {
        use sha2::{Digest as _, Sha256};
        Ok(Digest32::from_bytes(
            Sha256::digest(self.canonical_bytes()?).into(),
        ))
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CeremonyPhase {
    Approve,
    RegisterCredential,
    ConfirmPrf,
}

/// RFC 9180 associated data for local passkey PRF delivery.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LocalPrfHpkeAad {
    pub ceremony_id: Digest32,
    pub signer_nonce: Digest32,
    pub approval_id: Digest32,
    pub approval_digest: Digest32,
    pub review_manifest_digest: Digest32,
    pub key_ref: KeyRef,
    pub allowed_crypto_suites: Vec<CryptoSuite>,
    pub credential_id: Base64UrlBytes,
    pub activation_mode: ActivationMode,
    pub wallet_revocation_epoch: DecimalU64,
}

impl LocalPrfHpkeAad {
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, crate::ProtocolError> {
        serde_jcs::to_vec(self).map_err(|error| {
            crate::ProtocolError::new(crate::ProtocolErrorCode::MalformedFrame, error.to_string())
        })
    }
}

/// RFC 9180 associated data for a custody input.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CustodyHpkeAad {
    pub ceremony_id: Digest32,
    pub ceremony_kind: CeremonyKind,
    pub custody_operation_id: OperationId,
    pub signer_nonce: Digest32,
    pub signer_contribution_digest: Digest32,
    pub wallet_id: Option<Token>,
    pub key_ref: Option<KeyRef>,
    pub credential_id: Option<Base64UrlBytes>,
    pub expected_input_class: Token,
}

impl CustodyHpkeAad {
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, crate::ProtocolError> {
        serde_jcs::to_vec(self).map_err(|error| {
            crate::ProtocolError::new(crate::ProtocolErrorCode::MalformedFrame, error.to_string())
        })
    }
}

/// RFC 9180 associated data for a sensitive custody result returned directly
/// from Signer to a Browser-owned one-use recipient key.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CustodyOutputHpkeAad {
    pub ceremony_id: Digest32,
    pub ceremony_kind: CeremonyKind,
    pub custody_operation_id: OperationId,
    pub signer_contribution_digest: Digest32,
    pub public_binding_digest: Digest32,
}

impl CustodyOutputHpkeAad {
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, crate::ProtocolError> {
        serde_jcs::to_vec(self).map_err(canonical_error)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CustodyResult {
    pub ceremony_kind: CeremonyKind,
    pub custody_operation_id: OperationId,
    pub public_status: crate::CeremonyState,
    pub wallet_id: Option<Token>,
    pub public_key_refs: Vec<KeyRef>,
    pub credential_summaries: Vec<CredentialSummary>,
    pub initial_policy: Option<crate::SignedPolicySnapshot>,
    pub receipt_digest: Digest32,
    pub encrypted_browser_result: Option<HpkeEnvelope>,
    pub signer_key_id: Token,
    pub signer_signature: Base64UrlBytes,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialSummary {
    pub credential_id: Base64UrlBytes,
    pub rp_id: Token,
    pub active: bool,
}

impl CustodyResult {
    pub fn unsigned_canonical_bytes(&self) -> Result<Vec<u8>, crate::ProtocolError> {
        #[derive(Serialize)]
        struct Unsigned<'a> {
            ceremony_kind: CeremonyKind,
            custody_operation_id: &'a OperationId,
            public_status: crate::CeremonyState,
            wallet_id: &'a Option<Token>,
            public_key_refs: &'a [KeyRef],
            credential_summaries: &'a [CredentialSummary],
            initial_policy: &'a Option<crate::SignedPolicySnapshot>,
            receipt_digest: &'a Digest32,
            encrypted_browser_result: &'a Option<HpkeEnvelope>,
            signer_key_id: &'a Token,
        }
        serde_jcs::to_vec(&Unsigned {
            ceremony_kind: self.ceremony_kind,
            custody_operation_id: &self.custody_operation_id,
            public_status: self.public_status,
            wallet_id: &self.wallet_id,
            public_key_refs: &self.public_key_refs,
            credential_summaries: &self.credential_summaries,
            initial_policy: &self.initial_policy,
            receipt_digest: &self.receipt_digest,
            encrypted_browser_result: &self.encrypted_browser_result,
            signer_key_id: &self.signer_key_id,
        })
        .map_err(canonical_error)
    }
}

fn digest_canonical(bytes: Vec<u8>) -> Result<Digest32, crate::ProtocolError> {
    use sha2::{Digest as _, Sha256};
    Ok(Digest32::from_bytes(Sha256::digest(bytes).into()))
}

fn canonical_error(error: impl std::fmt::Display) -> crate::ProtocolError {
    crate::ProtocolError::new(crate::ProtocolErrorCode::MalformedFrame, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CeremonyState;

    #[test]
    fn custody_success_states_match_section_13_6() {
        assert_eq!(
            CeremonyKind::WalletRegistration.successful_terminal_state(),
            Some(CeremonyState::Completed)
        );
        for kind in [
            CeremonyKind::WalletImport,
            CeremonyKind::WalletExport,
            CeremonyKind::WalletDelete,
            CeremonyKind::WalletRecovery,
            CeremonyKind::CredentialAdd,
            CeremonyKind::CredentialReplace,
            CeremonyKind::CredentialRemove,
            CeremonyKind::BackendEnrollment,
            CeremonyKind::KeyDerive,
            CeremonyKind::PolicyUpdate,
        ] {
            assert_eq!(
                kind.successful_terminal_state(),
                Some(CeremonyState::Succeeded),
                "{kind:?}"
            );
        }
        assert_eq!(
            CeremonyKind::SealedApproval.successful_terminal_state(),
            None
        );
    }
}
