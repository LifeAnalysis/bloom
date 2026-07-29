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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WebAuthnAssertion {
    pub credential_id: Base64UrlBytes,
    pub authenticator_data: Base64UrlBytes,
    pub client_data_json: Base64UrlBytes,
    pub signature: Base64UrlBytes,
    pub user_handle: Option<Base64UrlBytes>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CeremonyCompleteRequest {
    pub activation_operation_id: OperationId,
    pub assertion: WebAuthnAssertion,
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
    pub expires_at_ms: DecimalU64,
    pub signer_key_id: Token,
    pub signer_signature: Base64UrlBytes,
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
    pub assertion: WebAuthnAssertion,
    pub encrypted_input: HpkeEnvelope,
    pub public_binding_digest: Digest32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CustodyResult {
    pub ceremony_kind: CeremonyKind,
    pub custody_operation_id: OperationId,
    pub public_status: crate::CeremonyState,
    pub receipt_digest: Digest32,
    pub encrypted_browser_result: Option<HpkeEnvelope>,
}
