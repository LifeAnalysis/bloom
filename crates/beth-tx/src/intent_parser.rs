//! Parse JSON / TOML / shell-style intents into a normalised
//! [`beth_proto::RawIntent`].
//!
//! Heuristic: if input starts with `{`, it's JSON; if it parses as TOML
//! and has at least one of {`to`, `kind`}, it's TOML; otherwise it's
//! treated as a shell-style line.

use beth_proto::{RawIntent, RawIntentBody, ShellIntent};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("empty input")]
    Empty,
    #[error("json parse: {0}")]
    Json(#[from] serde_json::Error),
    #[error("toml parse: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("shell parse: {0}")]
    Shell(String),
    #[error("ambiguous intent")]
    Ambiguous,
}

#[derive(serde::Deserialize, Debug, Clone)]
struct LooseIntent {
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    to: Option<String>,
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    data: Option<String>,
    #[serde(default)]
    contract: Option<String>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    args: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    intent: Option<String>,
    #[serde(default)]
    chain: Option<String>,
    #[serde(default)]
    gas: Option<String>,
    #[serde(default)]
    nonce: Option<u64>,
    #[serde(default)]
    priority: Option<String>,
}

impl LooseIntent {
    fn into_raw(self) -> Result<RawIntent, ParseError> {
        let kind = self
            .kind
            .clone()
            .or_else(|| {
                if self.contract.is_some() && self.method.is_some() {
                    Some("call".into())
                } else if self.intent.is_some() {
                    Some("enso".into())
                } else if self.to.is_some() {
                    if self
                        .data
                        .as_deref()
                        .filter(|d| !d.is_empty() && *d != "0x")
                        .is_some()
                    {
                        Some("raw".into())
                    } else {
                        Some("send".into())
                    }
                } else {
                    None
                }
            })
            .ok_or(ParseError::Ambiguous)?;
        let body = match kind.as_str() {
            "send" => RawIntentBody::Send {
                to: self.to.ok_or(ParseError::Ambiguous)?,
                value: self.value.unwrap_or_default(),
                token: self.token,
                data: self.data,
            },
            "call" => RawIntentBody::Call {
                contract: self.contract.ok_or(ParseError::Ambiguous)?,
                method: self.method.ok_or(ParseError::Ambiguous)?,
                args: self.args.unwrap_or_default(),
                value: self.value.unwrap_or_default(),
            },
            "raw" => RawIntentBody::Raw {
                to: self.to.ok_or(ParseError::Ambiguous)?,
                value: self.value.unwrap_or_default(),
                data: self.data.ok_or(ParseError::Ambiguous)?,
            },
            "enso" => RawIntentBody::Enso {
                intent: self.intent.ok_or(ParseError::Ambiguous)?,
            },
            _ => return Err(ParseError::Ambiguous),
        };
        let gas = match self.gas.or(self.priority).as_deref() {
            Some("auto") | None => beth_proto::GasStrategy::Auto,
            Some("fast") => beth_proto::GasStrategy::Fast,
            Some("standard") => beth_proto::GasStrategy::Standard,
            Some("slow") => beth_proto::GasStrategy::Slow,
            Some(other) => {
                return Err(ParseError::Shell(format!(
                    "unknown gas strategy '{}'",
                    other
                )))
            }
        };
        Ok(RawIntent {
            body,
            chain: self.chain,
            gas,
            nonce: self.nonce,
        })
    }
}

/// Parse a textual intent in any of the accepted forms.
pub fn parse(input: &str) -> Result<RawIntent, ParseError> {
    let s = input.trim();
    if s.is_empty() {
        return Err(ParseError::Empty);
    }
    if s.starts_with('{') {
        let loose: LooseIntent = serde_json::from_str(s)?;
        return loose.into_raw();
    }
    if s.starts_with("send ") {
        let shell = ShellIntent::parse(s).map_err(ParseError::Shell)?;
        return Ok(RawIntent {
            body: RawIntentBody::Send {
                to: shell.to,
                value: format!("{} {}", shell.amount, shell.unit),
                token: if shell.unit.eq_ignore_ascii_case("eth")
                    || shell.unit.eq_ignore_ascii_case("ether")
                    || shell.unit.eq_ignore_ascii_case("wei")
                    || shell.unit.eq_ignore_ascii_case("gwei")
                {
                    None
                } else {
                    Some(shell.unit.clone())
                },
                data: None,
            },
            chain: shell.chain,
            gas: match shell.priority.as_deref() {
                Some("fast") => beth_proto::GasStrategy::Fast,
                Some("standard") => beth_proto::GasStrategy::Standard,
                Some("slow") => beth_proto::GasStrategy::Slow,
                _ => beth_proto::GasStrategy::Auto,
            },
            nonce: None,
        });
    }
    // Try TOML.
    let loose: LooseIntent = toml::from_str(s)?;
    loose.into_raw()
}

#[cfg(test)]
mod tests {
    use super::*;
    use beth_proto::RawIntentBody;

    #[test]
    fn json_send() {
        let r = parse(r#"{"to":"0xabc","value":"0.1 eth"}"#).unwrap();
        assert!(matches!(r.body, RawIntentBody::Send { .. }));
    }

    #[test]
    fn toml_send_with_chain() {
        let r = parse(
            r#"
to = "0xabc"
value = "10 usdc"
chain = "ethereum"
"#,
        )
        .unwrap();
        assert_eq!(r.chain.as_deref(), Some("ethereum"));
        if let RawIntentBody::Send { value, token, .. } = r.body {
            assert_eq!(value, "10 usdc");
            assert!(token.is_none());
        } else {
            panic!("wrong variant");
        }
    }

    #[test]
    fn shell_send() {
        let r = parse("send 0.5 eth to 0xabc on anvil").unwrap();
        assert_eq!(r.chain.as_deref(), Some("anvil"));
    }

    #[test]
    fn shell_token_send_sets_token() {
        let r = parse("send 10 usdc to vitalik.eth").unwrap();
        if let RawIntentBody::Send { token, .. } = r.body {
            assert_eq!(token.as_deref(), Some("usdc"));
        } else {
            panic!("wrong variant");
        }
    }

    #[test]
    fn json_call() {
        let r = parse(
            r#"{"contract":"0xabc","method":"transfer(address,uint256)","args":["0xdef","1"]}"#,
        )
        .unwrap();
        assert!(matches!(r.body, RawIntentBody::Call { .. }));
    }

    #[test]
    fn json_enso() {
        let r = parse(r#"{"kind":"enso","intent":"swap 1 ETH to USDC"}"#).unwrap();
        assert!(matches!(r.body, RawIntentBody::Enso { .. }));
    }

    #[test]
    fn empty_input_errors() {
        assert!(matches!(parse(""), Err(ParseError::Empty)));
    }
}
