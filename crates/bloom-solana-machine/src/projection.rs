//! Typed, read-only VFS projections.
//!
//! Every field is derived from durable outbox records — the immutable
//! envelope and the journal — never from mutable Petal output or display
//! files. Verified facts (extracted by the independent verifier) are kept in
//! a separate struct from machine-asserted observations so no surface can
//! render them as one undifferentiated category.
//!
//! Digests are preferred over payload bytes: projections carry the message
//! digest, artifact digest, and verifier result digest. Raw signed bytes are
//! intentionally not projected; the transaction signature (base58) is.

use bloom_chain_action::{Action, ActionState, AttemptOutcome, Transition};
use serde::{Deserialize, Serialize};

use crate::MachineError;

/// Fields established by the independent `solana-system-transfer-v1`
/// verifier at staging, replayed from the journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifiedFacts {
    pub fee_payer_base58: String,
    pub destination_base58: String,
    pub lamports: u64,
    pub verifier_id: String,
    pub verifier_result_digest_hex: String,
    pub message_digest_hex: String,
}

/// Machine-asserted (never verifier-proven) observations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssertedFacts {
    /// The network fee quoted for the exact message.
    pub fee_lamports: u64,
    /// The approved hard fee ceiling.
    pub max_fee_lamports: u64,
    /// Transfer + fee, what the payer loses in the worst honest case.
    pub total_debit_lamports: u64,
    /// The blockhash embedded in the payload (base58).
    pub blockhash_base58: String,
    /// Observed last-valid block height for that blockhash.
    pub last_valid_block_height: u64,
    /// When the liveness observation was recorded (ms epoch).
    pub observed_at_ms: u64,
    /// The claimed cluster identity; visible, never verified.
    pub claimed_caip2: String,
    /// Age of the blockhash observation at projection time (ms).
    pub blockhash_age_ms: u64,
    /// Blocks remaining in the observed validity window at projection time;
    /// `None` when no current height was supplied.
    pub remaining_validity_blocks: Option<u64>,
}

/// Binding identities for the driver and provenance chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingProjections {
    pub package_hash: String,
    pub route: String,
    pub abi_version: u32,
    pub state_schema: u32,
    pub operation_class: String,
    pub crypto_suite: String,
    pub payload_digest_hex: String,
    /// Digest of the assembled signed artifact, once one exists.
    pub artifact_digest_hex: Option<String>,
}

/// One broadcast attempt and its recorded outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptProjection {
    pub attempt: u64,
    pub artifact_digest_hex: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
}

/// Reconciliation/finality observation, once terminal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinalityProjection {
    /// processed/confirmed/finalized as last observed.
    pub confirmation: Option<String>,
    /// Terminal failure reason, if the operation failed.
    pub failure_reason: Option<String>,
    /// Quarantine reason, if quarantined.
    pub quarantine_reason: Option<String>,
    /// Freshness refusal reason, if refused.
    pub freshness_reason: Option<String>,
}

/// The complete typed projection for one operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationProjection {
    pub schema: String,
    pub operation_id: String,
    pub state: String,
    pub terminal: bool,
    pub wallet_id: String,
    /// Operator-pinned cluster identity for this operation.
    pub cluster: ClusterProjection,
    /// Blockhash age and validity-window accounting (honest-runtime).
    pub freshness: FreshnessSummary,
    /// Verified (verifier-extracted) economic facts.
    pub verified: VerifiedFacts,
    /// Machine-asserted observations, explicitly labeled as such.
    pub asserted: AssertedFacts,
    pub bindings: BindingProjections,
    /// Base58 of the single Ed25519 signature, once signed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature_base58: Option<String>,
    pub attempts: Vec<AttemptProjection>,
    pub finality: FinalityProjection,
}

pub const PROJECTION_SCHEMA: &str = "bloom.solana.operation-projection/2";

/// Build the projection from durable records alone.
///
/// `now_ms` anchors the blockhash-age computation; `profile_genesis_hex` and
/// `profile_caip2` come from the operator-configured profile the machine was
/// built with (cluster identity is profile-pinned, not envelope-claimed).
pub fn project_operation(
    action: &Action,
    now_ms: u64,
    profile_caip2: &str,
    profile_genesis_hex: &str,
) -> Result<OperationProjection, MachineError> {
    let env = &action.envelope;

    let mut verified: Option<VerifiedFacts> = None;
    let mut fee: Option<(u64, u64)> = None;
    let mut liveness: Option<(String, u64, u64)> = None;
    let mut confirmation: Option<String> = None;
    let mut failure_reason: Option<String> = None;
    let mut quarantine_reason: Option<String> = None;
    let mut freshness_reason: Option<String> = None;

    for record in &action.journal {
        match &record.transition {
            Transition::FactsVerified {
                fee_payer_base58,
                destination_base58,
                lamports,
                verifier_id,
                verifier_result_digest_hex,
                message_digest_hex,
            } => {
                verified = Some(VerifiedFacts {
                    fee_payer_base58: fee_payer_base58.clone(),
                    destination_base58: destination_base58.clone(),
                    lamports: *lamports,
                    verifier_id: verifier_id.clone(),
                    verifier_result_digest_hex: verifier_result_digest_hex.clone(),
                    message_digest_hex: message_digest_hex.clone(),
                });
            }
            Transition::FeeObserved {
                lamports,
                max_lamports,
            } => fee = Some((*lamports, *max_lamports)),
            Transition::LivenessObserved {
                blockhash_base58,
                last_valid_block_height,
                observed_at_ms,
            } => {
                liveness = Some((
                    blockhash_base58.clone(),
                    *last_valid_block_height,
                    *observed_at_ms,
                ))
            }
            Transition::Confirmed { detail } => confirmation = Some(detail.clone()),
            Transition::Failed { reason } => failure_reason = Some(reason.clone()),
            Transition::Quarantined { reason } => quarantine_reason = Some(reason.clone()),
            Transition::FreshnessRefused { reason } => {
                freshness_reason = Some(format!("{reason:?}").to_lowercase())
            }
            _ => {}
        }
    }

    let verified = verified.ok_or(MachineError::MissingField("facts_verified record"))?;
    let (fee_lamports, max_fee_lamports) =
        fee.ok_or(MachineError::MissingField("fee_observed record"))?;
    let (blockhash_base58, last_valid_block_height, observed_at_ms) =
        liveness.ok_or(MachineError::MissingField("liveness_observed record"))?;

    let total_debit_lamports = verified.lamports.saturating_add(fee_lamports);

    let attempts = action
        .attempts
        .iter()
        .map(|a| AttemptProjection {
            attempt: a.attempt,
            artifact_digest_hex: a.artifact_digest_hex.clone(),
            outcome: a.outcome.as_ref().map(|o| match o {
                AttemptOutcome::Accepted => "accepted".to_string(),
                AttemptOutcome::Ambiguous => "ambiguous".to_string(),
                AttemptOutcome::Rejected { reason } => format!("rejected: {reason}"),
            }),
        })
        .collect();

    Ok(OperationProjection {
        schema: PROJECTION_SCHEMA.to_string(),
        operation_id: env.operation_id.clone(),
        state: action.state.as_str().to_string(),
        terminal: action.state.is_terminal(),
        wallet_id: env.wallet_id.clone(),
        verified,
        cluster: ClusterProjection {
            schema: "bloom.solana.cluster-projection/1".to_string(),
            profile: env.chain.profile.clone(),
            family: env.chain.family.clone(),
            caip2: profile_caip2.to_string(),
            expected_genesis_hex: profile_genesis_hex.to_string(),
            broadcast_enabled: true,
        },
        freshness: FreshnessSummary {
            age_ms: now_ms.saturating_sub(observed_at_ms),
            last_valid_block_height,
            remaining_blocks: None,
        },
        asserted: AssertedFacts {
            fee_lamports,
            max_fee_lamports,
            total_debit_lamports,
            blockhash_base58,
            last_valid_block_height,
            observed_at_ms,
            claimed_caip2: env.chain.claimed_caip2.clone(),
            blockhash_age_ms: now_ms.saturating_sub(observed_at_ms),
            remaining_validity_blocks: None,
        },
        bindings: BindingProjections {
            package_hash: env.driver.package_hash.clone(),
            route: env.driver.route.clone(),
            abi_version: env.driver.abi_version,
            state_schema: env.driver.state_schema,
            operation_class: env.operation_class.clone(),
            crypto_suite: env.crypto_suite.clone(),
            payload_digest_hex: env.payload_digest_hex.clone(),
            artifact_digest_hex: action.artifact.as_ref().map(|a| a.digest_hex.clone()),
        },
        signature_base58: action
            .artifact
            .as_ref()
            .map(|a| bs58::encode(&a.signature).into_string()),
        attempts,
        finality: FinalityProjection {
            confirmation,
            failure_reason,
            quarantine_reason,
            freshness_reason,
        },
    })
}

/// Cluster identity projection: the operator-pinned binding. The genesis
/// hash is what the mediator enforces on every call; it is displayed whole
/// so users can cross-check against explorer-published values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClusterProjection {
    pub schema: String,
    pub profile: String,
    pub family: String,
    pub caip2: String,
    pub expected_genesis_hex: String,
    /// Broadcast capability of the profile as configured.
    pub broadcast_enabled: bool,
}

/// Enabled-account projection. Public key material only: never a private
/// key, mnemonic, entropy, or WKEK — none of which the Machine ever holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountProjection {
    pub schema: String,
    pub wallet_id: String,
    pub key_ref_locator: String,
    /// Ed25519 public key (64 lowercase hex).
    pub public_key_hex: String,
    /// Canonical base58 Solana address.
    pub address_base58: String,
    /// CAIP-10 account identity on the enabled cluster.
    pub caip10: String,
    pub cluster_profile: String,
    pub enabled_at_ms: u64,
}

/// Blockhash freshness summary derived from the liveness record.
///
/// Honest-runtime accounting only: it bounds lag, not malice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FreshnessSummary {
    /// Age of the liveness observation in milliseconds.
    pub age_ms: u64,
    pub last_valid_block_height: u64,
    /// Remaining blocks computed against a current observed height.
    pub remaining_blocks: Option<u64>,
}

impl OperationProjection {
    /// Digests preferred over payload bytes: this is the canonical short
    /// identity of the operation for display surfaces.
    pub fn digest_summary(&self) -> String {
        format!(
            "message={} artifact={}",
            self.verified.message_digest_hex,
            self.bindings
                .artifact_digest_hex
                .as_deref()
                .unwrap_or("<unsigned>")
        )
    }
}

/// State helper for `staged operation ID and lifecycle state` projections.
pub fn lifecycle_state(action: &Action) -> &'static str {
    match action.state {
        ActionState::Staged => "staged",
        ActionState::Signed => "signed",
        ActionState::Sent => "sent",
        ActionState::Ambiguous => "ambiguous",
        ActionState::Confirmed => "confirmed",
        ActionState::Failed => "failed",
        ActionState::Cancelled => "cancelled",
        ActionState::Expired => "expired",
        ActionState::Quarantined => "quarantined",
        ActionState::FreshnessRefused => "freshness_refused",
    }
}
