//! Honest-runtime pre-sign freshness evaluation.
//!
//! Given the staged observation (blockhash, last-valid height, staging time,
//! commitment) and one or two current network observations, decide whether
//! signing may proceed or which first-class freshness refusal applies.
//!
//! These checks detect **lagging or inconsistent** providers. A single
//! provider that lies consistently about blockhash, height, and validity
//! defeats them; containing that adversary requires endpoint quorum, a
//! separately trusted attestor, or a light-client mechanism, all of which are
//! deliberately out of scope for v1.

use bloom_chain_action::FreshnessReason;
use serde::{Deserialize, Serialize};

/// What the driver staged: the blockhash it embedded in the payload and the
/// liveness metadata it observed at staging time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StagedObservation {
    pub blockhash: String,
    pub last_valid_block_height: u64,
    pub staged_at_ms: u64,
    pub commitment: String,
}

/// A current network observation (e.g. from `getLatestBlockhash`,
/// `getBlockHeight`, `isBlockhashValid`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkObservation {
    pub latest_blockhash: String,
    pub latest_block_height: u64,
    /// `None` when `isBlockhashValid` was not called.
    pub blockhash_valid: Option<bool>,
    pub observed_at_ms: u64,
    pub commitment: String,
}

/// The honest-runtime policy knobs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FreshnessPolicy {
    /// Maximum age of the staged blockhash before restaging is required.
    pub max_staleness_ms: u64,
    /// Minimum remaining block-height window required to sign.
    pub min_remaining_blocks: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreshnessVerdict {
    Fresh,
    Refused(FreshnessReason),
}

/// Evaluate freshness. Inconsistency is checked first: if the observations
/// disagree with each other or with the staged commitment, nothing else they
/// report can be trusted.
pub fn evaluate_freshness(
    staged: &StagedObservation,
    a: &NetworkObservation,
    b: Option<&NetworkObservation>,
    policy: &FreshnessPolicy,
) -> FreshnessVerdict {
    // Commitment/context must match the staged profile.
    if a.commitment != staged.commitment || b.is_some_and(|b| b.commitment != staged.commitment) {
        return FreshnessVerdict::Refused(FreshnessReason::NetworkObservationInconsistent);
    }

    // Two observations must agree with each other.
    if let Some(b) = b
        && (a.latest_blockhash != b.latest_blockhash
            || a.latest_blockhash_height_disagreement(b)
            || matches!((a.blockhash_valid, b.blockhash_valid), (Some(x), Some(y)) if x != y))
    {
        return FreshnessVerdict::Refused(FreshnessReason::NetworkObservationInconsistent);
    }

    // An explicit isBlockhashValid(false) for the staged hash forces restage.
    if a.blockhash_valid == Some(false) || b.is_some_and(|b| b.blockhash_valid == Some(false)) {
        return FreshnessVerdict::Refused(FreshnessReason::BlockhashRefreshRequired);
    }

    // The staged blockhash must not be older than policy allows.
    let age = a.observed_at_ms.saturating_sub(staged.staged_at_ms).max(
        b.map(|b| b.observed_at_ms.saturating_sub(staged.staged_at_ms))
            .unwrap_or(0),
    );
    if age > policy.max_staleness_ms {
        return FreshnessVerdict::Refused(FreshnessReason::BlockhashRefreshRequired);
    }

    // Enough of the validity window must remain to sign into.
    let remaining = staged.last_valid_block_height.saturating_sub(
        a.latest_block_height
            .max(b.map(|b| b.latest_block_height).unwrap_or(0)),
    );
    if remaining < policy.min_remaining_blocks {
        return FreshnessVerdict::Refused(FreshnessReason::InsufficientValidityWindow);
    }

    FreshnessVerdict::Fresh
}

impl NetworkObservation {
    /// Heights within a few blocks are normal drift; hundreds apart is
    /// disagreement. The tolerance matches a typical validator lag bound.
    const HEIGHT_DRIFT_TOLERANCE: u64 = 32;

    fn latest_blockhash_height_disagreement(&self, other: &NetworkObservation) -> bool {
        self.latest_block_height.abs_diff(other.latest_block_height) > Self::HEIGHT_DRIFT_TOLERANCE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn staged(commitment: &str) -> StagedObservation {
        StagedObservation {
            blockhash: "b1".into(),
            last_valid_block_height: 250,
            staged_at_ms: 1_000,
            commitment: commitment.into(),
        }
    }

    fn obs(height: u64, at_ms: u64, commitment: &str) -> NetworkObservation {
        NetworkObservation {
            latest_blockhash: "b1".into(),
            latest_block_height: height,
            blockhash_valid: None,
            observed_at_ms: at_ms,
            commitment: commitment.into(),
        }
    }

    fn policy() -> FreshnessPolicy {
        FreshnessPolicy {
            max_staleness_ms: 90_000,
            min_remaining_blocks: 32,
        }
    }

    #[test]
    fn fresh_when_window_is_wide() {
        let verdict = evaluate_freshness(
            &staged("confirmed"),
            &obs(100, 2_000, "confirmed"),
            None,
            &policy(),
        );
        assert_eq!(verdict, FreshnessVerdict::Fresh);
    }

    #[test]
    fn stale_staging_requires_refresh() {
        let verdict = evaluate_freshness(
            &staged("confirmed"),
            &obs(100, 500_000, "confirmed"),
            None,
            &policy(),
        );
        assert_eq!(
            verdict,
            FreshnessVerdict::Refused(FreshnessReason::BlockhashRefreshRequired)
        );
    }

    #[test]
    fn explicit_invalid_blockhash_requires_refresh() {
        let mut a = obs(100, 2_000, "confirmed");
        a.blockhash_valid = Some(false);
        assert_eq!(
            evaluate_freshness(&staged("confirmed"), &a, None, &policy()),
            FreshnessVerdict::Refused(FreshnessReason::BlockhashRefreshRequired)
        );
    }

    #[test]
    fn narrow_window_refuses() {
        let verdict = evaluate_freshness(
            &staged("confirmed"),
            &obs(240, 2_000, "confirmed"),
            None,
            &policy(),
        );
        assert_eq!(
            verdict,
            FreshnessVerdict::Refused(FreshnessReason::InsufficientValidityWindow)
        );
    }

    #[test]
    fn commitment_mismatch_is_inconsistent() {
        let verdict = evaluate_freshness(
            &staged("confirmed"),
            &obs(100, 2_000, "finalized"),
            None,
            &policy(),
        );
        assert_eq!(
            verdict,
            FreshnessVerdict::Refused(FreshnessReason::NetworkObservationInconsistent)
        );
    }

    #[test]
    fn disagreeing_providers_are_inconsistent() {
        let a = obs(100, 2_000, "confirmed");
        let b = NetworkObservation {
            latest_blockhash: "b2".into(),
            ..obs(100, 2_001, "confirmed")
        };
        assert_eq!(
            evaluate_freshness(&staged("confirmed"), &a, Some(&b), &policy()),
            FreshnessVerdict::Refused(FreshnessReason::NetworkObservationInconsistent)
        );

        let b_height = obs(100 + 500, 2_001, "confirmed");
        assert_eq!(
            evaluate_freshness(&staged("confirmed"), &a, Some(&b_height), &policy()),
            FreshnessVerdict::Refused(FreshnessReason::NetworkObservationInconsistent)
        );

        // Normal drift (a few blocks) is fine.
        let b_close = obs(105, 2_001, "confirmed");
        assert_eq!(
            evaluate_freshness(&staged("confirmed"), &a, Some(&b_close), &policy()),
            FreshnessVerdict::Fresh
        );
    }
}
