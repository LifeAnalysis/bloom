//! A real Solana JSON-RPC HTTP transport behind the mediated stack.
//!
//! Enabled by the `http` feature. Speaks the actual wire protocol: base58
//! blockhashes, base64-encoded transactions for `sendTransaction`, and the
//! `result`/`error` JSON-RPC envelope. A transport-level timeout maps to
//! [`RpcError::Timeout`] — acceptance unknown — so ambiguity handling stays
//! with the caller. Uses a std-only blocking HTTP client so it can be called
//! from async host code without nested-runtime hazards.

use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use base64::Engine as _;
use serde_json::{Value, json};
use thiserror::Error;

use crate::transport::{RpcError, RpcTransport};

#[derive(Debug, Error)]
pub enum HttpTransportError {
    #[error("invalid endpoint url: {0}")]
    BadUrl(String),
    #[error("http agent construction failed: {0}")]
    Agent(String),
}

pub struct SolanaHttpTransport {
    endpoint: String,
    agent: ureq::Agent,
    next_id: AtomicU64,
}

impl SolanaHttpTransport {
    pub fn new(endpoint: &str) -> Result<Self, HttpTransportError> {
        // Reject obviously malformed endpoints early with a real URL parse.
        url::Url::parse(endpoint)
            .map_err(|e| HttpTransportError::BadUrl(format!("{endpoint}: {e}")))?;
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(5))
            .timeout_read(Duration::from_secs(10))
            .build();
        Ok(Self {
            endpoint: endpoint.to_string(),
            agent,
            next_id: AtomicU64::new(1),
        })
    }
}

impl RpcTransport for SolanaHttpTransport {
    fn name(&self) -> &str {
        "solana-http"
    }

    fn call(&self, method: &str, params: &Value) -> Result<Value, RpcError> {
        // The mediator hands sendTransaction the artifact as hex; the wire
        // protocol wants base64.
        let params = if method == "sendTransaction" {
            // The mediator hands the artifact as a bare hex string; the wire
            // protocol wants params = [<base64 transaction>, config?].
            let wire_hex = params
                .as_str()
                .ok_or_else(|| RpcError::Transport("sendTransaction expects hex wire".into()))?;
            let bytes = hex::decode(wire_hex)
                .map_err(|e| RpcError::Transport(format!("sendTransaction hex: {e}")))?;
            Value::Array(vec![
                Value::String(base64::engine::general_purpose::STANDARD.encode(bytes)),
                json!({ "encoding": "base64" }),
            ])
        } else {
            params.clone()
        };

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let body = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let request = self
            .agent
            .post(&self.endpoint)
            .set("content-type", "application/json");
        let response = request.send_string(&body.to_string()).map_err(|e| {
            let text = e.to_string();
            if text.contains("timed out") || text.contains("Timeout") {
                RpcError::Timeout
            } else {
                RpcError::Transport(text)
            }
        })?;
        let mut text = String::new();
        response
            .into_reader()
            .take(1 << 20)
            .read_to_string(&mut text)
            .map_err(|e| RpcError::Transport(format!("read body: {e}")))?;
        let payload: Value =
            serde_json::from_str(&text).map_err(|e| RpcError::Transport(format!("decode: {e}")))?;
        if let Some(error) = payload.get("error") {
            return Err(RpcError::Transport(error.to_string()));
        }
        Ok(payload.get("result").cloned().unwrap_or(Value::Null))
    }
}
