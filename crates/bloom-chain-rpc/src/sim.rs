//! A deterministic in-memory chain for mediation, freshness, and
//! reconciliation tests.
//!
//! The simulator mints one blockhash per block height
//! (`SHA-256("sim/blockhash/<height>")`), keeps each valid for
//! [`SimChain::VALIDITY`] further blocks, and tracks submitted/landed
//! transactions keyed by their derived "signature" (SHA-256 of the wire
//! bytes). Heights only move when the test moves them, so freshness windows
//! are fully controllable.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::transport::{RpcError, RpcTransport};

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    let out: [u8; 32] = h.finalize().into();
    hex::encode(out)
}

#[derive(Debug, Default)]
struct SimState {
    height: u64,
    minted: HashMap<String, u64>,
    submitted: HashSet<String>,
    landed: HashMap<String, u64>,
}

/// Deterministic simulated chain behind an [`RpcTransport`].
#[derive(Debug)]
pub struct SimChain {
    genesis_hex: String,
    state: Mutex<SimState>,
}

impl SimChain {
    /// Blocks a minted blockhash stays valid after its height.
    pub const VALIDITY: u64 = 150;

    /// Lamports charged per required signature (Solana's base fee).
    pub const FEE_PER_SIGNATURE: u64 = 5_000;

    pub fn new(genesis_hex: &str) -> Self {
        Self {
            genesis_hex: genesis_hex.to_string(),
            state: Mutex::new(SimState {
                height: 100,
                ..SimState::default()
            }),
        }
    }

    pub fn blockhash_for(height: u64) -> String {
        sha256_hex(format!("sim/blockhash/{height}").as_bytes())
    }

    /// The deterministic "signature" the sim assigns to submitted wire bytes.
    pub fn signature_for(wire: &str) -> String {
        sha256_hex(format!("sim/sig/{wire}").as_bytes())
    }

    fn hex_to_base58(hex_str: &str) -> String {
        let bytes = hex::decode(hex_str).unwrap_or_default();
        bs58::encode(bytes).into_string()
    }

    fn base58_to_hex(b58: &str) -> Result<String, RpcError> {
        let bytes = bs58::decode(b58)
            .into_vec()
            .map_err(|e| RpcError::Transport(format!("blockhash base58: {e}")))?;
        Ok(hex::encode(bytes))
    }

    /// Mint (or look up) the blockhash for an explicit height.
    pub fn blockhash_at(&self, height: u64) -> (String, u64) {
        let mut st = self.state.lock().unwrap();
        let bhash = Self::blockhash_for(height);
        st.minted.insert(bhash.clone(), height);
        (bhash, height + Self::VALIDITY)
    }

    pub fn advance(&self, blocks: u64) {
        self.state.lock().unwrap().height += blocks;
    }

    pub fn height(&self) -> u64 {
        self.state.lock().unwrap().height
    }

    /// Simulate inclusion of a previously submitted transaction at the
    /// current height.
    pub fn land(&self, signature: &str) {
        let mut st = self.state.lock().unwrap();
        let height = st.height;
        // Unconditional by design: reconciliation keys by whatever signature
        // string the caller will query, which for single-signer Solana
        // transactions is the transaction's own signature.
        st.landed.insert(signature.to_string(), height);
    }

    fn handle(&self, method: &str, params: &Value) -> Result<Value, RpcError> {
        let mut st = self.state.lock().unwrap();
        match method {
            "getGenesisHash" => Ok(json!(self.genesis_hex)),
            "getHealth" => Ok(json!("ok")),
            "getBlockHeight" => Ok(json!(st.height)),
            "getSlot" => Ok(json!(st.height)),
            "getLatestBlockhash" => {
                // Real Solana RPC returns base58 blockhashes; keep the wire
                // protocol-shaped so real clients (the Petal) parse it.
                let height = st.height;
                let bhash_hex = Self::blockhash_for(height);
                st.minted.insert(bhash_hex.clone(), height);
                Ok(json!({
                    "context": { "slot": st.height },
                    "value": {
                        "blockhash": Self::hex_to_base58(&bhash_hex),
                        "lastValidBlockHeight": st.height + Self::VALIDITY,
                    }
                }))
            }
            "getFeeForMessage" => {
                // Real Solana shape: params = [<base64 message>, commitment?];
                // result = { context: { slot }, value: <lamports|null> }.
                // Fee model: FEE_PER_SIGNATURE lamports per required
                // signature, read from the message header's first byte.
                let b64 = params
                    .as_str()
                    .or_else(|| {
                        params
                            .as_array()
                            .and_then(|a| a.first())
                            .and_then(|v| v.as_str())
                    })
                    .ok_or_else(|| RpcError::Transport("message param missing".into()))?;
                let raw = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64)
                    .map_err(|e| RpcError::Transport(format!("message base64: {e}")))?;
                let signatures = raw.first().copied().unwrap_or(0);
                let lamports = u64::from(signatures) * Self::FEE_PER_SIGNATURE;
                Ok(json!({
                    "context": { "slot": st.height },
                    "value": lamports,
                }))
            }
            "isBlockhashValid" => {
                // Real Solana shape: params = [<blockhash>, <commitment>?];
                // accept the bare-string form too for direct callers.
                let bhash = params
                    .as_str()
                    .or_else(|| {
                        params
                            .as_array()
                            .and_then(|a| a.first())
                            .and_then(|v| v.as_str())
                    })
                    .ok_or_else(|| RpcError::Transport("blockhash param missing".into()))?;
                let bhash_hex = Self::base58_to_hex(bhash)?;
                // Reverse lookup over plausible mint heights; test heights
                // are small so this stays trivial.
                let mint = (0..=st.height)
                    .find(|h| Self::blockhash_for(*h) == bhash_hex)
                    .or_else(|| st.minted.get(&bhash_hex).copied());
                let valid = mint.is_some_and(|h| st.height <= h + Self::VALIDITY);
                Ok(json!({ "valid": valid }))
            }
            "sendTransaction" => {
                let wire = params.as_str().unwrap_or("");
                let signature = sha256_hex(format!("sim/sig/{wire}").as_bytes());
                st.submitted.insert(signature.clone());
                Ok(json!(signature))
            }
            "getSignatureStatuses" => {
                // Real RPC shape: params = [[<sig>, ...]]; accept the flat
                // [sig, ...] form for direct callers too.
                let sigs = params
                    .as_array()
                    .ok_or_else(|| RpcError::Transport("signatures param missing".into()))?;
                let sigs: Vec<serde_json::Value> = if sigs.first().is_some_and(|f| f.is_array()) {
                    sigs.first()
                        .and_then(|f| f.as_array())
                        .cloned()
                        .unwrap_or_default()
                } else {
                    sigs.clone()
                };
                let statuses: Vec<Value> = sigs
                    .iter()
                    .map(|s| {
                        let sig = s.as_str().unwrap_or("");
                        match st.landed.get(sig) {
                            Some(slot) => json!({
                                "slot": slot,
                                "confirmationStatus": "confirmed",
                            }),
                            None => Value::Null,
                        }
                    })
                    .collect();
                Ok(json!({ "value": statuses }))
            }
            other => Err(RpcError::MethodUnsupported(other.to_string())),
        }
    }
}

impl RpcTransport for SimChain {
    fn name(&self) -> &str {
        "sim"
    }

    fn call(&self, method: &str, params: &Value) -> Result<Value, RpcError> {
        self.handle(method, params)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex_to_b58(hex_str: &str) -> String {
        bs58::encode(hex::decode(hex_str).unwrap()).into_string()
    }

    #[test]
    fn blockhash_validity_window() {
        let chain = SimChain::new("aa");
        let (bhash, last_valid) = chain.blockhash_at(chain.height());
        assert_eq!(last_valid, 100 + SimChain::VALIDITY);
        let ok: Value = chain
            .call("isBlockhashValid", &json!(hex_to_b58(&bhash)))
            .unwrap();
        assert_eq!(ok["valid"], json!(true));
        chain.advance(SimChain::VALIDITY + 1);
        let stale: Value = chain
            .call("isBlockhashValid", &json!(hex_to_b58(&bhash)))
            .unwrap();
        assert_eq!(stale["valid"], json!(false));
    }

    #[test]
    fn submission_and_landing() {
        let chain = SimChain::new("aa");
        let sig = chain
            .call("sendTransaction", &json!("wire-bytes"))
            .unwrap()
            .as_str()
            .unwrap()
            .to_string();
        let pending: Value = chain.call("getSignatureStatuses", &json!([sig])).unwrap();
        assert_eq!(pending["value"][0], Value::Null);
        chain.land(&sig);
        let landed: Value = chain.call("getSignatureStatuses", &json!([sig])).unwrap();
        assert_eq!(landed["value"][0]["confirmationStatus"], json!("confirmed"));
    }
}
