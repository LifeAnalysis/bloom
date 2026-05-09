//! Staged-tx artefacts: the human-readable plan, policy_check, and the
//! tx state model.

use serde::{Deserialize, Serialize};

use crate::policy::PolicyCheck;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TxStatus {
    Pending,
    Sent,
    Success,
    Reverted,
    Failed,
    Cancelled,
}

impl std::fmt::Display for TxStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            TxStatus::Pending => "pending",
            TxStatus::Sent => "sent",
            TxStatus::Success => "success",
            TxStatus::Reverted => "reverted",
            TxStatus::Failed => "failed",
            TxStatus::Cancelled => "cancelled",
        };
        f.write_str(s)
    }
}

/// A staged tx ready to be confirmed. `id` is unique per wallet.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StagedTx {
    pub id: String,
    pub wallet: String,
    pub chain: String,
    pub chain_id: u64,
    pub from: String,
    pub to: String,
    pub value_wei: String,
    pub data_hex: String,
    pub gas_limit: u64,
    pub max_fee_per_gas: Option<String>,
    pub max_priority_fee_per_gas: Option<String>,
    /// For legacy (non-1559) chains. Mutually exclusive with the
    /// EIP-1559 fields above.
    #[serde(default)]
    pub gas_price: Option<String>,
    pub nonce: u64,
    pub policy_checks: Vec<PolicyCheck>,
    pub created_ms: u128,
    pub expires_ms: u128,
    pub status: TxStatus,
    /// Tx hash once broadcast.
    #[serde(default)]
    pub tx_hash: Option<String>,
    /// Optional ERC-20 token metadata for plan rendering. Set when the
    /// tx encodes a `transfer(address,uint256)` call against a token
    /// contract.
    #[serde(default)]
    pub token: Option<TokenRef>,
    /// USD-denominated value at stage time, when a price oracle was
    /// available. Persisted so the per-day rolling-window enforcement
    /// can sum historical sends without re-querying prices.
    #[serde(default)]
    pub usd_value: Option<f64>,
}

/// Lightweight token reference embedded in a `StagedTx` for display.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenRef {
    pub address: String,
    pub symbol: String,
    pub decimals: u8,
    /// Recipient (decoded from the transfer call) — convenient for
    /// rendering plan.md.
    pub recipient: String,
    /// Human-readable amount in token units (e.g. "100" for 100 USDC).
    pub amount: String,
}

/// Helpers to render plan.md from a StagedTx.
pub struct PlanRender;

impl PlanRender {
    pub fn render(staged: &StagedTx, native_symbol: &str, native_decimals: u8) -> String {
        use crate::units::format_units;
        let value_u256 = alloy::primitives::U256::from_str_radix(&staged.value_wei, 10)
            .unwrap_or(alloy::primitives::U256::ZERO);
        let value_human = format_units(value_u256, native_decimals);
        let mut s = String::new();
        s.push_str(&format!("# Staged tx {}\n\n", staged.id));
        s.push_str(&format!("Wallet: {}\n", staged.wallet));
        s.push_str(&format!("From:   {}\n", staged.from));
        if let Some(tok) = &staged.token {
            // ERC-20 transfer view.
            s.push_str(&format!("To:     {} (token contract)\n", staged.to));
            s.push_str(&format!("Token:  {} ({})\n", tok.symbol, tok.address));
            s.push_str(&format!(
                "Action: Transfer {} {} to {}\n",
                tok.amount, tok.symbol, tok.recipient
            ));
        } else {
            s.push_str(&format!("To:     {}\n", staged.to));
        }
        s.push_str(&format!(
            "Chain:  {} (id {})\n",
            staged.chain, staged.chain_id
        ));
        s.push_str(&format!(
            "Value:  {} {} ({} wei)\n",
            value_human, native_symbol, staged.value_wei
        ));
        s.push_str(&format!("Nonce:  {}\n", staged.nonce));
        if let Some(gp) = staged.gas_price.as_deref() {
            s.push_str(&format!(
                "Gas:    limit={} gas_price={} (legacy)\n",
                staged.gas_limit, gp
            ));
        } else {
            let max_fee = staged.max_fee_per_gas.as_deref().unwrap_or("auto");
            let prio = staged.max_priority_fee_per_gas.as_deref().unwrap_or("auto");
            s.push_str(&format!(
                "Gas:    limit={} max_fee={} prio={}\n",
                staged.gas_limit, max_fee, prio
            ));
        }
        if !staged.data_hex.is_empty() && staged.data_hex != "0x" {
            s.push_str(&format!(
                "Data:   {} bytes\n",
                staged.data_hex.trim_start_matches("0x").len() / 2
            ));
        } else {
            s.push_str("Data:   (none)\n");
        }
        s.push_str("\n## Policy\n");
        if staged.policy_checks.is_empty() {
            s.push_str("- No policy rules configured.\n");
        } else {
            for c in &staged.policy_checks {
                s.push_str(&format!("- [{:?}] {}: {}\n", c.outcome, c.rule, c.message));
            }
        }
        s.push_str("\n## Confirm\n");
        s.push_str(
            "Write `y` to `confirm` to broadcast, `cancel` to discard, \
             `override` to bypass soft policy warnings.\n",
        );
        s
    }
}
