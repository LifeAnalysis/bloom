//! Intent representations.
//!
//! Three input forms accepted:
//! 1. JSON  — structured tx (`to`, `value`, `data`, `chain`, …)
//! 2. TOML  — same fields in toml syntax
//! 3. Shell — single line: `send 1 ETH to 0xabc on ethereum`
//!
//! The `intent` module parses all three into a [`RawIntent`]; the
//! [`tx_engine`](crate::plan) downstream converts that into a concrete
//! [`crate::plan::StagedTx`].

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(untagged)]
pub enum ValueOrToken {
    /// e.g. "0.5 eth", "100 gwei", "10 usdc"
    String(String),
    #[default]
    Empty,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum GasStrategy {
    #[default]
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "fast")]
    Fast,
    #[serde(rename = "standard")]
    Standard,
    #[serde(rename = "slow")]
    Slow,
}

/// Raw intent, with all fields optional except those that distinguish the
/// kind. The tx engine fills defaults.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RawIntentBody {
    /// Plain native or token transfer.
    Send {
        to: String,
        #[serde(default)]
        value: String,
        #[serde(default)]
        token: Option<String>,
        #[serde(default)]
        data: Option<String>,
    },
    /// Contract method call.
    Call {
        contract: String,
        method: String,
        #[serde(default)]
        args: Vec<serde_json::Value>,
        #[serde(default)]
        value: String,
    },
    /// Pre-encoded raw transaction.
    Raw {
        to: String,
        #[serde(default)]
        value: String,
        data: String,
    },
    /// ERC-20 approval. `amount` accepts a decimal integer or `"max"`
    /// (shorthand for 2^256 - 1 — the conventional infinite-allowance
    /// value). The tx engine encodes `approve(address,uint256)`; the
    /// approval is to `spender` against the token contract `token`.
    Approve {
        token: String,
        spender: String,
        amount: String,
    },
    /// Enso DeFi intent.
    Enso { intent: String },
}

/// What the user wrote (after normalising format).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawIntent {
    pub body: RawIntentBody,
    /// Filesystem chain name, e.g. "ethereum" or "anvil".
    pub chain: Option<String>,
    /// Gas strategy hint.
    #[serde(default)]
    pub gas: GasStrategy,
    /// Optional explicit nonce override (rarely useful).
    #[serde(default)]
    pub nonce: Option<u64>,
}

/// A normalised concrete tx-intent (post-resolution): addresses parsed,
/// values resolved to wei.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxIntent {
    pub from: String,
    pub to: String,
    pub value_wei: String,
    pub data_hex: String,
    pub chain_id: u64,
    pub chain: String,
    pub gas_limit: Option<u64>,
    pub max_fee_per_gas: Option<String>,
    pub max_priority_fee_per_gas: Option<String>,
    pub nonce: Option<u64>,
}

/// Shell-style intent. Parsed manually with a tiny grammar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellIntent {
    pub verb: String,   // "send"
    pub amount: String, // "0.5"
    pub unit: String,   // "eth" / "usdc"
    pub to: String,     // address or alias
    pub chain: Option<String>,
    pub priority: Option<String>,
}

impl ShellIntent {
    /// Parse `send <amount> <unit> to <addr> [on <chain>] [--priority <p>]`
    pub fn parse(line: &str) -> Result<Self, String> {
        let toks: Vec<&str> = line.split_whitespace().collect();
        if toks.len() < 5 {
            return Err(format!("too short: '{}'", line));
        }
        if toks[0] != "send" {
            return Err(format!("only 'send' supported, got '{}'", toks[0]));
        }
        let amount = toks[1].to_string();
        let unit = toks[2].to_string();
        if toks[3] != "to" {
            return Err("expected 'to' after amount".into());
        }
        let to = toks[4].to_string();
        let mut i = 5;
        let mut chain = None;
        let mut priority = None;
        while i < toks.len() {
            match toks[i] {
                "on" if i + 1 < toks.len() => {
                    chain = Some(toks[i + 1].to_string());
                    i += 2;
                }
                "--priority" if i + 1 < toks.len() => {
                    priority = Some(toks[i + 1].to_string());
                    i += 2;
                }
                other => return Err(format!("unexpected token '{}'", other)),
            }
        }
        Ok(ShellIntent {
            verb: "send".into(),
            amount,
            unit,
            to,
            chain,
            priority,
        })
    }
}

/// Enso intent body (placeholder; full client lives in `beth-defi`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnsoIntent {
    pub intent: String,
    pub chain: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_intent_basic() {
        let i = ShellIntent::parse("send 0.5 eth to 0xabc on ethereum").unwrap();
        assert_eq!(i.amount, "0.5");
        assert_eq!(i.unit, "eth");
        assert_eq!(i.to, "0xabc");
        assert_eq!(i.chain.as_deref(), Some("ethereum"));
        assert!(i.priority.is_none());
    }

    #[test]
    fn shell_intent_with_priority() {
        let i = ShellIntent::parse("send 10 usdc to vitalik.eth --priority fast").unwrap();
        assert_eq!(i.unit, "usdc");
        assert_eq!(i.priority.as_deref(), Some("fast"));
        assert!(i.chain.is_none());
    }

    #[test]
    fn json_send_round_trip() {
        let s = r#"{"kind":"send","to":"0xabc","value":"0.1 eth"}"#;
        let body: RawIntentBody = serde_json::from_str(s).unwrap();
        match body {
            RawIntentBody::Send { to, value, .. } => {
                assert_eq!(to, "0xabc");
                assert_eq!(value, "0.1 eth");
            }
            _ => panic!("wrong variant"),
        }
    }
}
