//! Translate per-wallet policy.toml into a list of `PolicyCheck` entries
//! attached to a staged tx.
//!
//! Rules covered:
//! - global + per-chain caps (the *more restrictive* of the two wins).
//! - allow / deny lists for contracts, tokens, recipients (case-
//!   insensitive address compares).
//! - automation: `auto_confirm_below_eth`.

use alloy::primitives::{Address, U256};
use beth_proto::policy::{PolicyCaps, PolicyLists};
use beth_proto::{format_units, Policy, PolicyCheck, PolicyOutcome};

const ETHER: u128 = 1_000_000_000_000_000_000;

/// Addresses involved in a tx, used by allow/deny list checks.
#[derive(Debug, Clone, Default)]
pub struct AddressContext {
    /// The contract being called (if any).
    pub contract: Option<Address>,
    /// The token being moved (for ERC-20 transfers).
    pub token: Option<Address>,
    /// The user-facing recipient. For ERC-20 sends this is the
    /// address inside the calldata, not the contract `to`.
    pub recipient: Option<Address>,
}

/// Run policy checks against a staged tx.
///
/// `chain_name` is the filesystem-friendly chain name; it is matched
/// against `policy.per_chain` for any per-chain overrides.
pub fn evaluate(
    policy: &Policy,
    chain_name: &str,
    value_wei: U256,
    native_decimals: u8,
    ctx: AddressContext,
) -> Vec<PolicyCheck> {
    let mut out = Vec::new();
    let value_human = format_units(value_wei, native_decimals);
    let value_f = value_human.parse::<f64>().unwrap_or(0.0);

    // Effective caps = most restrictive of (global, per-chain).
    let effective_caps = match policy.per_chain.get(chain_name) {
        Some(per) => PolicyCaps::most_restrictive(&policy.caps, per),
        None => policy.caps.clone(),
    };

    if let Some(max) = effective_caps.max_value_eth {
        if value_f > max {
            out.push(PolicyCheck {
                rule: "caps.max_value_eth".into(),
                outcome: PolicyOutcome::Deny,
                message: format!("value {} > max {}", value_human, max),
            });
        } else {
            out.push(PolicyCheck {
                rule: "caps.max_value_eth".into(),
                outcome: PolicyOutcome::Pass,
                message: format!("value {} <= max {}", value_human, max),
            });
        }
    }

    if let Some(soft) = effective_caps.require_override_above_eth {
        if value_f > soft {
            out.push(PolicyCheck {
                rule: "caps.require_override_above_eth".into(),
                outcome: PolicyOutcome::Warn,
                message: format!(
                    "value {} > soft {} — write `override` to confirm",
                    value_human, soft
                ),
            });
        }
    }

    if let Some(auto_below) = policy.automation.auto_confirm_below_eth {
        if value_f <= auto_below {
            out.push(PolicyCheck {
                rule: "automation.auto_confirm_below_eth".into(),
                outcome: PolicyOutcome::Pass,
                message: "value within auto-confirm threshold".into(),
            });
        }
    }

    // ----- allow / deny lists -------------------------------------------------
    check_lists(
        &mut out,
        "denylists.contracts",
        &policy.denylists.contracts,
        ctx.contract,
        ListMode::Deny,
    );
    check_lists(
        &mut out,
        "denylists.tokens",
        &policy.denylists.tokens,
        ctx.token,
        ListMode::Deny,
    );
    check_lists(
        &mut out,
        "denylists.recipients",
        &policy.denylists.recipients,
        ctx.recipient,
        ListMode::Deny,
    );

    check_lists(
        &mut out,
        "allowlists.contracts",
        &policy.allowlists.contracts,
        ctx.contract,
        ListMode::Allow,
    );
    check_lists(
        &mut out,
        "allowlists.tokens",
        &policy.allowlists.tokens,
        ctx.token,
        ListMode::Allow,
    );
    check_lists(
        &mut out,
        "allowlists.recipients",
        &policy.allowlists.recipients,
        ctx.recipient,
        ListMode::Allow,
    );

    let _ = ETHER;
    let _ = PolicyLists::default; // make import deterministic
    out
}

#[derive(Copy, Clone)]
enum ListMode {
    Allow,
    Deny,
}

fn check_lists(
    out: &mut Vec<PolicyCheck>,
    rule: &str,
    list: &std::collections::BTreeSet<String>,
    addr: Option<Address>,
    mode: ListMode,
) {
    if list.is_empty() {
        return;
    }
    let target = match addr {
        Some(a) => a,
        None => {
            // Allowlist with nothing to check is a hard miss — we can't
            // confirm the tx falls inside the list.
            if matches!(mode, ListMode::Allow) {
                out.push(PolicyCheck {
                    rule: rule.into(),
                    outcome: PolicyOutcome::Deny,
                    message: "allowlist set but tx has no relevant address".into(),
                });
            }
            return;
        }
    };
    let target_lc = format!("{target:#x}").to_ascii_lowercase();
    let hit = list
        .iter()
        .any(|s| s.trim().to_ascii_lowercase() == target_lc);
    match (mode, hit) {
        (ListMode::Deny, true) => out.push(PolicyCheck {
            rule: rule.into(),
            outcome: PolicyOutcome::Deny,
            message: format!("{} is denylisted", target_lc),
        }),
        (ListMode::Deny, false) => {}
        (ListMode::Allow, true) => out.push(PolicyCheck {
            rule: rule.into(),
            outcome: PolicyOutcome::Pass,
            message: format!("{} is on allowlist", target_lc),
        }),
        (ListMode::Allow, false) => out.push(PolicyCheck {
            rule: rule.into(),
            outcome: PolicyOutcome::Deny,
            message: format!("{} not on allowlist", target_lc),
        }),
    }
}

/// Returns true if any check is `Deny`.
pub fn has_hard_violation(checks: &[PolicyCheck]) -> bool {
    checks.iter().any(|c| c.outcome == PolicyOutcome::Deny)
}

/// Returns true if any check is `Warn`.
pub fn has_warning(checks: &[PolicyCheck]) -> bool {
    checks.iter().any(|c| c.outcome == PolicyOutcome::Warn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use beth_proto::policy::{PolicyAutomation, PolicyCaps, PolicyLists};
    use std::collections::BTreeSet;

    fn addr(s: &str) -> Address {
        s.parse().unwrap()
    }

    #[test]
    fn caps_max_value() {
        let p = Policy {
            caps: PolicyCaps {
                max_value_eth: Some(0.5),
                ..Default::default()
            },
            ..Default::default()
        };
        let value = U256::from(700_000_000_000_000_000u128); // 0.7 eth
        let checks = evaluate(&p, "anvil", value, 18, AddressContext::default());
        assert!(has_hard_violation(&checks));
    }

    #[test]
    fn caps_pass() {
        let p = Policy {
            caps: PolicyCaps {
                max_value_eth: Some(1.0),
                ..Default::default()
            },
            ..Default::default()
        };
        let value = U256::from(500_000_000_000_000_000u128);
        let checks = evaluate(&p, "anvil", value, 18, AddressContext::default());
        assert!(!has_hard_violation(&checks));
    }

    #[test]
    fn soft_warn() {
        let p = Policy {
            caps: PolicyCaps {
                require_override_above_eth: Some(0.1),
                ..Default::default()
            },
            ..Default::default()
        };
        let value = U256::from(500_000_000_000_000_000u128);
        let checks = evaluate(&p, "anvil", value, 18, AddressContext::default());
        assert!(has_warning(&checks));
    }

    #[test]
    fn denylist_recipient_is_hard_block() {
        let mut deny = BTreeSet::new();
        deny.insert("0x000000000000000000000000000000000000dead".to_string());
        let p = Policy {
            denylists: PolicyLists {
                recipients: deny,
                ..Default::default()
            },
            ..Default::default()
        };
        let ctx = AddressContext {
            recipient: Some(addr("0x000000000000000000000000000000000000dead")),
            ..Default::default()
        };
        let checks = evaluate(&p, "anvil", U256::ZERO, 18, ctx);
        assert!(
            has_hard_violation(&checks),
            "expected hard violation: {checks:?}"
        );
    }

    #[test]
    fn allowlist_miss_is_hard_block() {
        let mut allow = BTreeSet::new();
        allow.insert("0x0000000000000000000000000000000000001111".to_string());
        let p = Policy {
            allowlists: PolicyLists {
                contracts: allow,
                ..Default::default()
            },
            ..Default::default()
        };
        let ctx = AddressContext {
            contract: Some(addr("0x0000000000000000000000000000000000002222")),
            ..Default::default()
        };
        let checks = evaluate(&p, "anvil", U256::ZERO, 18, ctx);
        assert!(has_hard_violation(&checks), "checks: {checks:?}");
    }

    #[test]
    fn allowlist_hit_passes() {
        let mut allow = BTreeSet::new();
        allow.insert("0x0000000000000000000000000000000000001111".to_string());
        let p = Policy {
            allowlists: PolicyLists {
                contracts: allow,
                ..Default::default()
            },
            ..Default::default()
        };
        let ctx = AddressContext {
            contract: Some(addr("0x0000000000000000000000000000000000001111")),
            ..Default::default()
        };
        let checks = evaluate(&p, "anvil", U256::ZERO, 18, ctx);
        assert!(!has_hard_violation(&checks), "checks: {checks:?}");
    }

    #[test]
    fn per_chain_override_is_more_restrictive() {
        // Global allows 1 ETH, per-chain caps anvil at 0.1.
        let mut per_chain = std::collections::BTreeMap::new();
        per_chain.insert(
            "anvil".to_string(),
            PolicyCaps {
                max_value_eth: Some(0.1),
                ..Default::default()
            },
        );
        let p = Policy {
            caps: PolicyCaps {
                max_value_eth: Some(1.0),
                ..Default::default()
            },
            per_chain,
            ..Default::default()
        };
        // 0.5 ETH passes global but fails per-chain.
        let value = U256::from(500_000_000_000_000_000u128);
        let checks = evaluate(&p, "anvil", value, 18, AddressContext::default());
        assert!(has_hard_violation(&checks), "{checks:?}");

        // On a different chain (no override) it should pass.
        let checks = evaluate(&p, "ethereum", value, 18, AddressContext::default());
        assert!(!has_hard_violation(&checks), "{checks:?}");
    }

    #[test]
    fn override_token_default_is_override() {
        let p = Policy::default();
        assert_eq!(p.override_sentinel(), "override");
        let p2 = Policy {
            automation: PolicyAutomation {
                override_token: Some("yolo".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(p2.override_sentinel(), "yolo");
    }
}
