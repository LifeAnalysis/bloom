use serde::{Deserialize, Serialize};

use crate::{Base64UrlBytes, DecimalU64, Digest32, OperationId, Token};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SignedPolicySnapshot {
    pub wallet_id: Token,
    pub version: DecimalU64,
    pub canonical_policy: Base64UrlBytes,
    pub policy_digest: Digest32,
    pub policy_signing_key_id: Token,
    pub signer_signature: Base64UrlBytes,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyCompareAndSwapRequest {
    pub operation_id: OperationId,
    pub wallet_id: Token,
    pub baseline_version: DecimalU64,
    pub baseline_digest: Digest32,
    pub proposed_canonical_policy: Base64UrlBytes,
    pub proposed_policy_digest: Digest32,
    pub authority_diff_digest: Digest32,
    pub ceremony_receipt_digest: Digest32,
    pub broker_validation_receipt_digest: Digest32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyCommitReceipt {
    pub operation_id: OperationId,
    pub wallet_id: Token,
    pub previous_version: DecimalU64,
    pub committed: SignedPolicySnapshot,
    pub authority_diff_digest: Digest32,
    pub signer_key_id: Token,
    pub signer_signature: Base64UrlBytes,
}
