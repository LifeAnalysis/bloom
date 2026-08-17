//! Scripted fault injection over any [`RpcTransport`] — the adversarial RPC
//! harness required before mainnet enablement.
//!
//! Each scripted fault applies **once** to the next matching call; calls that
//! don't match the optional method filter pass through honestly. Once the
//! script is exhausted the proxy is fully honest. This mirrors the plan's
//! required adversarial cases: old-but-valid blockhashes, expired blockhashes,
//! conflicting status responses, wrong genesis hash, timeouts before and
//! after submission, selective transaction drops, inclusion followed by false
//! "not found," and provider disagreement.

use std::sync::Mutex;

use serde_json::{Value, json};
use thiserror::Error;

use crate::sim::SimChain;
use crate::transport::{RpcError, RpcTransport};

/// One scripted fault. `method` scopes the fault to a specific RPC method;
/// `None` applies to the next call regardless of method.
#[derive(Debug, Clone)]
pub struct ScriptedFault {
    pub method: Option<&'static str>,
    pub fault: Fault,
}

impl ScriptedFault {
    pub fn on(method: &'static str, fault: Fault) -> Self {
        Self {
            method: Some(method),
            fault,
        }
    }

    /// Fire on the `occurrence`-th matching call by consuming the earlier
    /// matching calls honestly — lets tests target a stage-time or
    /// finalize-time observation of the same method precisely.
    pub fn on_nth(method: &'static str, occurrence: usize, fault: Fault) -> Vec<Self> {
        let mut script = Vec::with_capacity(occurrence);
        for _ in 1..occurrence {
            script.push(Self::on(method, Fault::Honest));
        }
        script.push(Self::on(method, fault));
        script
    }

    pub fn any(fault: Fault) -> Self {
        Self {
            method: None,
            fault,
        }
    }
}

#[derive(Debug, Clone, Error)]
pub enum FaultError {
    #[error("fault requires a SimChain transport, got '{0}'")]
    NotSimulated(String),
}

/// The fault a scripted call exhibits.
#[derive(Debug, Clone)]
pub enum Fault {
    /// `getLatestBlockhash` returns an older blockhash that is still valid.
    OldButValidBlockhash { skew_blocks: u64 },
    /// `getLatestBlockhash` returns a blockhash whose validity already
    /// expired.
    ExpiredBlockhash,
    /// `getGenesisHash` returns a different cluster's genesis.
    WrongGenesis { genesis: String },
    /// `getSignatureStatuses` alternates between a confirmed report and
    /// not-found across calls.
    ConflictingStatus,
    /// `sendTransaction` times out and the transaction was never accepted.
    TimeoutBeforeSubmit,
    /// `sendTransaction` times out but the transaction lands on chain.
    TimeoutAfterSubmit,
    /// `sendTransaction` succeeds but the transaction never lands.
    SelectiveDrop,
    /// `getSignatureStatuses` reports not-found even for landed transactions.
    FalseNotFound,
    /// `getBlockHeight` reports a wildly different height than the chain, to
    /// construct disagreeing providers.
    ProviderDisagreement { offset_blocks: u64 },
    /// `getFeeForMessage` returns `value: null` — the provider cannot or
    /// will not quote a fee.
    FeeNull,
    /// `getFeeForMessage` returns the honest fee plus a skew, producing an
    /// over-limit quote or an inconsistency between two observations.
    FeeSkew { extra_lamports: u64 },
    /// No fault: consume a script slot honestly. Used to target a later
    /// call of a repeated method.
    Honest,
}

/// A transport wrapper that plays a scripted sequence of faults over an
/// honest [`SimChain`], then falls back to honest behavior.
#[derive(Debug)]
pub struct FaultProxy {
    chain: std::sync::Arc<SimChain>,
    script: Mutex<Vec<ScriptedFault>>,
    status_flip: Mutex<bool>,
}

impl FaultProxy {
    pub fn new(chain: SimChain, script: Vec<ScriptedFault>) -> Self {
        Self::shared(std::sync::Arc::new(chain), script)
    }

    /// Build a proxy over a chain shared with the test/production caller, so
    /// state control (advancing, landing) and mediated calls hit one chain.
    pub fn shared(chain: std::sync::Arc<SimChain>, script: Vec<ScriptedFault>) -> Self {
        Self {
            chain,
            script: Mutex::new(script),
            status_flip: Mutex::new(false),
        }
    }

    /// The wrapped honest chain, for direct state control (advancing,
    /// landing).
    pub fn chain(&self) -> &SimChain {
        &self.chain
    }

    fn take_fault(&self, method: &str) -> Option<Fault> {
        let mut script = self.script.lock().unwrap();
        script
            .iter()
            .position(|s| s.method.is_none_or(|m| m == method))
            .map(|idx| script.remove(idx).fault)
    }
}

impl RpcTransport for FaultProxy {
    fn name(&self) -> &str {
        "fault-proxy"
    }

    fn call(&self, method: &str, params: &Value) -> Result<Value, RpcError> {
        if let Some(fault) = self.take_fault(method) {
            return self.apply(fault, method, params);
        }
        self.chain.call(method, params)
    }
}

impl FaultProxy {
    fn apply(&self, fault: Fault, method: &str, params: &Value) -> Result<Value, RpcError> {
        match fault {
            Fault::OldButValidBlockhash { skew_blocks } => {
                let height = self.chain.height();
                let mint = height.saturating_sub(skew_blocks);
                let (bhash_hex, last_valid) = self.chain.blockhash_at(mint);
                let b58 = bs58::encode(hex::decode(&bhash_hex).unwrap_or_default()).into_string();
                Ok(json!({ "context": { "slot": self.chain.height() },
                          "value": { "blockhash": b58, "lastValidBlockHeight": last_valid } }))
            }
            Fault::ExpiredBlockhash => {
                let height = self.chain.height();
                let mint = height.saturating_sub(SimChain::VALIDITY + 10);
                let (bhash_hex, last_valid) = self.chain.blockhash_at(mint);
                let b58 = bs58::encode(hex::decode(&bhash_hex).unwrap_or_default()).into_string();
                Ok(json!({ "context": { "slot": self.chain.height() },
                          "value": { "blockhash": b58, "lastValidBlockHeight": last_valid } }))
            }
            Fault::WrongGenesis { genesis } => Ok(json!(genesis)),
            Fault::ConflictingStatus => {
                let mut flip = self.status_flip.lock().unwrap();
                *flip = !*flip;
                if *flip {
                    self.chain.call(method, params)
                } else {
                    let sigs = params.as_array().cloned().unwrap_or_default();
                    Ok(json!({ "value": vec![Value::Null; sigs.len()] }))
                }
            }
            Fault::TimeoutBeforeSubmit => Err(RpcError::Timeout),
            Fault::TimeoutAfterSubmit => {
                // The provider accepted and landed the transaction, then the
                // response was lost.
                let sig = self.chain.call("sendTransaction", params)?;
                if let Some(wire) = sig.as_str() {
                    self.chain.land(wire);
                }
                Err(RpcError::Timeout)
            }
            Fault::SelectiveDrop => {
                // Accepted but silently dropped: never lands.
                self.chain.call("sendTransaction", params)
            }
            Fault::FalseNotFound => {
                let sigs = params.as_array().cloned().unwrap_or_default();
                Ok(json!({ "value": vec![Value::Null; sigs.len()] }))
            }
            Fault::ProviderDisagreement { offset_blocks } => match method {
                "getBlockHeight" => Ok(json!(self.chain.height() + offset_blocks)),
                _ => self.chain.call(method, params),
            },
            Fault::FeeNull => {
                Ok(json!({ "context": { "slot": self.chain.height() }, "value": null }))
            }
            Fault::FeeSkew { extra_lamports } => {
                let mut skewed = self.chain.call(method, params)?;
                if let Some(lamports) = skewed
                    .pointer("/value")
                    .and_then(|v| v.as_u64())
                    .map(|l| l + extra_lamports)
                    && let Some(value) = skewed.get_mut("value")
                {
                    *value = json!(lamports);
                }
                Ok(skewed)
            }
            Fault::Honest => self.chain.call(method, params),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proxy(faults: Vec<ScriptedFault>) -> FaultProxy {
        FaultProxy::new(SimChain::new("aa"), faults)
    }

    #[test]
    fn one_shot_faults_fall_back_to_honest() {
        let p = proxy(vec![ScriptedFault::on(
            "getLatestBlockhash",
            Fault::OldButValidBlockhash { skew_blocks: 30 },
        )]);
        let honest_height = p.chain().height();

        let skewed: Value = p.call("getLatestBlockhash", &Value::Null).unwrap();
        let (skewed_hash, skewed_last) = (
            skewed
                .pointer("/value/blockhash")
                .unwrap()
                .as_str()
                .unwrap(),
            skewed
                .pointer("/value/lastValidBlockHeight")
                .unwrap()
                .as_u64()
                .unwrap(),
        );
        assert_eq!(skewed_last, honest_height - 30 + SimChain::VALIDITY);

        // Still a valid blockhash from the chain's perspective (the fault
        // emits base58 like the real RPC boundary).
        let valid: Value = p.call("isBlockhashValid", &json!(skewed_hash)).unwrap();
        assert_eq!(valid["valid"], json!(true));

        // Script exhausted: the next call is honest.
        let honest: Value = p.call("getLatestBlockhash", &Value::Null).unwrap();
        assert_eq!(
            honest
                .pointer("/value/lastValidBlockHeight")
                .unwrap()
                .as_u64()
                .unwrap(),
            honest_height + SimChain::VALIDITY
        );
    }

    #[test]
    fn timeout_after_submit_lands_the_transaction() {
        let p = proxy(vec![ScriptedFault::on(
            "sendTransaction",
            Fault::TimeoutAfterSubmit,
        )]);
        let sig = SimChain::signature_for("probe");
        // The faulted call times out, but its transaction landed.
        assert!(matches!(
            p.call("sendTransaction", &json!("probe")).unwrap_err(),
            RpcError::Timeout
        ));
        let status: Value = p.call("getSignatureStatuses", &json!([sig])).unwrap();
        assert_eq!(status["value"][0]["confirmationStatus"], json!("confirmed"));
    }

    #[test]
    fn method_filter_scopes_the_fault() {
        let p = proxy(vec![ScriptedFault::on(
            "getGenesisHash",
            Fault::WrongGenesis {
                genesis: "ff".into(),
            },
        )]);
        // Unrelated methods pass through honestly.
        assert_eq!(p.call("getHealth", &Value::Null).unwrap(), json!("ok"));
        // The scoped method is faulted once.
        assert_eq!(p.call("getGenesisHash", &Value::Null).unwrap(), json!("ff"));
        assert_eq!(p.call("getGenesisHash", &Value::Null).unwrap(), json!("aa"));
    }
}
