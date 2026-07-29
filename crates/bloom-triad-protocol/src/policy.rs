use serde::{Deserialize, Serialize};

use sha2::{Digest as _, Sha256};
use std::collections::BTreeSet;

use crate::{
    Base64UrlBytes, CeremonyKind, CustodyCompleteRequest, CustodyPrepareRequest, CustodyResult,
    DecimalU64, Digest32, OperationId, ProtocolError, ProtocolErrorCode, Token,
};

const POLICY_UPDATE_TERMS_DOMAIN: &[u8] = b"bloom-policy-update-terms/v1";
const POLICY_AUTHORITY_DIFF_DOMAIN: &[u8] = b"bloom-policy-authority-diff/v1";

/// Canonical wallet-policy document written by Signer and interpreted by
/// Broker. Keeping its closed shape in the protocol package lets registration
/// create a valid initial document without teaching Signer policy semantics.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalWalletPolicy {
    pub wallet_id: Token,
    pub maximum_approval_lifetime_ms: u64,
    pub allowed_petal_packages: Vec<Digest32>,
    pub allowed_destinations: Vec<PolicyDestination>,
    pub required_verifiers: Vec<RequiredVerifier>,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyDestination {
    pub chain: Token,
    pub destination: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RequiredVerifier {
    pub verifier_id: Token,
    pub verifier_digest: Digest32,
}

/// Compute the canonical, human-reviewable authority delta between two policy
/// documents. Machine uses this only to populate its proposed request; Broker
/// independently recomputes and verifies the result before preparing review.
pub fn canonical_policy_authority_diff(
    current: &CanonicalWalletPolicy,
    proposed: &CanonicalWalletPolicy,
) -> PolicyAuthorityDiff {
    fn set_diff<T: Ord + Clone>(
        before: impl IntoIterator<Item = T>,
        after: impl IntoIterator<Item = T>,
    ) -> (Vec<T>, Vec<T>) {
        let before = before.into_iter().collect::<BTreeSet<_>>();
        let after = after.into_iter().collect::<BTreeSet<_>>();
        (
            after.difference(&before).cloned().collect(),
            before.difference(&after).cloned().collect(),
        )
    }

    let (added_petal_packages, removed_petal_packages) = set_diff(
        current.allowed_petal_packages.iter().cloned(),
        proposed.allowed_petal_packages.iter().cloned(),
    );
    let (added_destinations, removed_destinations) = set_diff(
        current
            .allowed_destinations
            .iter()
            .map(|value| PolicyAuthorityDestination {
                chain: value.chain.clone(),
                destination: value.destination.clone(),
            }),
        proposed
            .allowed_destinations
            .iter()
            .map(|value| PolicyAuthorityDestination {
                chain: value.chain.clone(),
                destination: value.destination.clone(),
            }),
    );
    let (added_required_verifiers, removed_required_verifiers) = set_diff(
        current
            .required_verifiers
            .iter()
            .map(|value| PolicyAuthorityVerifier {
                verifier_id: value.verifier_id.clone(),
                verifier_digest: value.verifier_digest.clone(),
            }),
        proposed
            .required_verifiers
            .iter()
            .map(|value| PolicyAuthorityVerifier {
                verifier_id: value.verifier_id.clone(),
                verifier_digest: value.verifier_digest.clone(),
            }),
    );

    PolicyAuthorityDiff {
        maximum_approval_lifetime_ms_before: DecimalU64::new(current.maximum_approval_lifetime_ms),
        maximum_approval_lifetime_ms_after: DecimalU64::new(proposed.maximum_approval_lifetime_ms),
        added_petal_packages,
        removed_petal_packages,
        added_destinations,
        removed_destinations,
        added_required_verifiers,
        removed_required_verifiers,
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SignedPolicySnapshot {
    pub wallet_id: Token,
    pub version: DecimalU64,
    pub canonical_policy: Base64UrlBytes,
    pub policy_digest: Digest32,
    pub policy_signing_key_id: Token,
    pub policy_verifying_key: Base64UrlBytes,
    pub signer_signature: Base64UrlBytes,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyUpdateRequest {
    pub operation_id: OperationId,
    pub wallet_id: Token,
    pub baseline_version: DecimalU64,
    pub baseline_digest: Digest32,
    pub proposed_canonical_policy: Base64UrlBytes,
    pub proposed_policy_digest: Digest32,
    pub authority_diff_digest: Digest32,
    pub assurance_level: Token,
}

impl PolicyUpdateRequest {
    pub fn terms_digest(&self) -> Result<Digest32, ProtocolError> {
        let mut hasher = Sha256::new();
        hasher.update(POLICY_UPDATE_TERMS_DOMAIN);
        hasher.update(serde_jcs::to_vec(self).map_err(canonical_error)?);
        Ok(Digest32::from_bytes(hasher.finalize().into()))
    }
}

/// Broker-derived, canonical summary of every authority-bearing policy change.
///
/// Set-valued fields are sorted and deduplicated before this value is
/// constructed. The complete proposed policy remains independently bound by
/// `proposed_policy_digest`; this summary is the exact human-reviewable delta.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyAuthorityDiff {
    pub maximum_approval_lifetime_ms_before: DecimalU64,
    pub maximum_approval_lifetime_ms_after: DecimalU64,
    pub added_petal_packages: Vec<Digest32>,
    pub removed_petal_packages: Vec<Digest32>,
    pub added_destinations: Vec<PolicyAuthorityDestination>,
    pub removed_destinations: Vec<PolicyAuthorityDestination>,
    pub added_required_verifiers: Vec<PolicyAuthorityVerifier>,
    pub removed_required_verifiers: Vec<PolicyAuthorityVerifier>,
}

impl PolicyAuthorityDiff {
    pub fn digest(&self) -> Result<Digest32, ProtocolError> {
        let mut hasher = Sha256::new();
        hasher.update(POLICY_AUTHORITY_DIFF_DOMAIN);
        hasher.update(serde_jcs::to_vec(self).map_err(canonical_error)?);
        Ok(Digest32::from_bytes(hasher.finalize().into()))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyAuthorityDestination {
    pub chain: Token,
    pub destination: String,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyAuthorityVerifier {
    pub verifier_id: Token,
    pub verifier_digest: Digest32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyValidationReceipt {
    pub update_terms_digest: Digest32,
    pub review_manifest_digest: Digest32,
    pub broker_key_id: Token,
    pub broker_signature: Base64UrlBytes,
}

impl PolicyValidationReceipt {
    pub fn unsigned_canonical_bytes(&self) -> Result<Vec<u8>, ProtocolError> {
        #[derive(Serialize)]
        struct Unsigned<'a> {
            update_terms_digest: &'a Digest32,
            review_manifest_digest: &'a Digest32,
            broker_key_id: &'a Token,
        }
        serde_jcs::to_vec(&Unsigned {
            update_terms_digest: &self.update_terms_digest,
            review_manifest_digest: &self.review_manifest_digest,
            broker_key_id: &self.broker_key_id,
        })
        .map_err(canonical_error)
    }

    pub fn digest(&self) -> Result<Digest32, ProtocolError> {
        Ok(Digest32::from_bytes(
            Sha256::digest(serde_jcs::to_vec(self).map_err(canonical_error)?).into(),
        ))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyUpdateReviewManifest {
    pub schema: Token,
    pub operation_id: OperationId,
    pub wallet_id: Token,
    pub baseline_version: DecimalU64,
    pub baseline_digest: Digest32,
    pub proposed_policy_digest: Digest32,
    pub authority_diff_digest: Digest32,
    pub authority_diff: PolicyAuthorityDiff,
    pub assurance_level: Token,
    pub issued_at_ms: DecimalU64,
    pub expires_at_ms: DecimalU64,
    pub broker_key_id: Token,
    pub broker_signature: Base64UrlBytes,
}

impl PolicyUpdateReviewManifest {
    pub fn unsigned_canonical_bytes(&self) -> Result<Vec<u8>, ProtocolError> {
        #[derive(Serialize)]
        struct Unsigned<'a> {
            schema: &'a Token,
            operation_id: &'a OperationId,
            wallet_id: &'a Token,
            baseline_version: &'a DecimalU64,
            baseline_digest: &'a Digest32,
            proposed_policy_digest: &'a Digest32,
            authority_diff_digest: &'a Digest32,
            authority_diff: &'a PolicyAuthorityDiff,
            assurance_level: &'a Token,
            issued_at_ms: &'a DecimalU64,
            expires_at_ms: &'a DecimalU64,
            broker_key_id: &'a Token,
        }
        serde_jcs::to_vec(&Unsigned {
            schema: &self.schema,
            operation_id: &self.operation_id,
            wallet_id: &self.wallet_id,
            baseline_version: &self.baseline_version,
            baseline_digest: &self.baseline_digest,
            proposed_policy_digest: &self.proposed_policy_digest,
            authority_diff_digest: &self.authority_diff_digest,
            authority_diff: &self.authority_diff,
            assurance_level: &self.assurance_level,
            issued_at_ms: &self.issued_at_ms,
            expires_at_ms: &self.expires_at_ms,
            broker_key_id: &self.broker_key_id,
        })
        .map_err(canonical_error)
    }

    pub fn digest(&self) -> Result<Digest32, ProtocolError> {
        Ok(Digest32::from_bytes(
            Sha256::digest(serde_jcs::to_vec(self).map_err(canonical_error)?).into(),
        ))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyUpdatePrepareResponse {
    pub operation_id: OperationId,
    pub ceremony_kind: CeremonyKind,
    pub ceremony_url: String,
    pub ceremony_expires_at_ms: DecimalU64,
    pub review_manifest_digest: Digest32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyCommitUpdateRequest {
    pub operation_id: OperationId,
    pub ceremony_receipt: CustodyResult,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyCompareAndSwapRequest {
    pub update: PolicyUpdateRequest,
    pub ceremony_receipt: CustodyResult,
    pub broker_validation_receipt: PolicyValidationReceipt,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyUpdateCeremonyPrepareRequest {
    pub custody: CustodyPrepareRequest,
    pub update: PolicyUpdateRequest,
    pub broker_validation_receipt: PolicyValidationReceipt,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyUpdateCeremonyCompleteRequest {
    pub custody: CustodyCompleteRequest,
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

fn canonical_error(error: impl std::fmt::Display) -> ProtocolError {
    ProtocolError::new(
        ProtocolErrorCode::MalformedFrame,
        format!("policy canonicalization failed: {error}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(value: &str) -> Token {
        Token::new(value).unwrap()
    }

    fn digest(value: u8) -> Digest32 {
        Digest32::from_bytes([value; 32])
    }

    #[test]
    fn authority_diff_is_sorted_deduplicated_and_complete() {
        let current = CanonicalWalletPolicy {
            wallet_id: token("wallet"),
            maximum_approval_lifetime_ms: 10,
            allowed_petal_packages: vec![digest(2), digest(1), digest(2)],
            allowed_destinations: vec![PolicyDestination {
                chain: token("ethereum"),
                destination: "old".into(),
            }],
            required_verifiers: vec![RequiredVerifier {
                verifier_id: token("human"),
                verifier_digest: digest(3),
            }],
        };
        let proposed = CanonicalWalletPolicy {
            wallet_id: token("wallet"),
            maximum_approval_lifetime_ms: 20,
            allowed_petal_packages: vec![digest(4), digest(2), digest(4)],
            allowed_destinations: vec![PolicyDestination {
                chain: token("ethereum"),
                destination: "new".into(),
            }],
            required_verifiers: Vec::new(),
        };

        let diff = canonical_policy_authority_diff(&current, &proposed);
        assert_eq!(diff.maximum_approval_lifetime_ms_before.get(), 10);
        assert_eq!(diff.maximum_approval_lifetime_ms_after.get(), 20);
        assert_eq!(diff.added_petal_packages, vec![digest(4)]);
        assert_eq!(diff.removed_petal_packages, vec![digest(1)]);
        assert_eq!(diff.added_destinations[0].destination, "new");
        assert_eq!(diff.removed_destinations[0].destination, "old");
        assert!(diff.added_required_verifiers.is_empty());
        assert_eq!(diff.removed_required_verifiers.len(), 1);
        assert_eq!(
            diff.digest().unwrap(),
            canonical_policy_authority_diff(&current, &proposed)
                .digest()
                .unwrap()
        );
    }
}
