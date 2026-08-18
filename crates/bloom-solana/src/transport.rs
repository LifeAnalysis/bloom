//! Layered JSON-RPC transport for Solana.
//!
//! Mirrors `bloom-rpc`'s `transport.rs` shape for the parts that translate:
//! a `reqwest`-based JSON-RPC client over a weighted endpoint list, with
//! retry-on-transient, failover across endpoints, and an active `getHealth`
//! probe loop feeding the shared [`HealthRegistry`]. The `alloy` transport
//! stack (`RootProvider<Ethereum>`, `FallbackLayer`) is deliberately not
//! reused — Solana's JSON-RPC methods and response shapes do not fit it.

use std::time::{Duration, Instant};

use bloom_rpc_common::HealthRegistry;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::watch;

use crate::error::SolanaRpcError;
use crate::retry::{RetrySignal, should_retry};

/// Maximum retry passes across the endpoint list. Matches `bloom-rpc`'s
/// `MAX_RETRIES` so both transports budget transient recovery identically.
const MAX_ATTEMPTS: usize = 3;

/// Initial backoff; doubles per attempt (200 → 400 → 800 ms).
const INITIAL_BACKOFF_MS: u64 = 200;

/// Active probe interval, matching `bloom-rpc`'s cadence.
const PROBE_INTERVAL: Duration = Duration::from_secs(15);

/// One configured endpoint and its pre-built client.
struct Endpoint {
    url: String,
    weight: u32,
    client: reqwest::Client,
}

/// A collapsed transport failure with its retry classification.
struct CallFailure {
    error: SolanaRpcError,
    retryable: bool,
}

/// The shared Solana RPC transport. Built once per [`crate::SolanaSpec`] and
/// shared via `Arc`.
pub struct SolanaRpcClient {
    chain_name: String,
    endpoints: Vec<Endpoint>,
    health: HealthRegistry,
    shutdown_tx: Option<watch::Sender<bool>>,
}

impl SolanaRpcClient {
    /// Build the transport from a spec. Fails with
    /// [`SolanaRpcError::NoEndpoints`] when no usable endpoint is configured.
    pub fn build(spec: &crate::SolanaSpec) -> Result<Self, SolanaRpcError> {
        let mut endpoints = Vec::new();
        for ep in spec.endpoints() {
            if ep.url.starts_with("ws://") || ep.url.starts_with("wss://") {
                continue; // read client is HTTP-only for now
            }
            let client = reqwest::Client::builder()
                .build()
                .map_err(|e| SolanaRpcError::Transport(e.to_string()))?;
            endpoints.push(Endpoint {
                url: ep.url.clone(),
                weight: ep.weight,
                client,
            });
        }
        if endpoints.is_empty() {
            return Err(SolanaRpcError::NoEndpoints(spec.name.clone()));
        }
        endpoints.sort_by_key(|e| std::cmp::Reverse(e.weight));

        let health = HealthRegistry::new(endpoints.iter().map(|e| e.url.clone()));
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let client = Self {
            chain_name: spec.name.clone(),
            endpoints,
            health,
            shutdown_tx: Some(shutdown_tx),
        };

        client.spawn_probe_loop(shutdown_rx);
        Ok(client)
    }

    /// Per-endpoint health snapshot, mirroring `bloom-rpc`'s accessor.
    pub fn endpoints_snapshot(&self) -> Vec<bloom_rpc_common::EndpointHealthSnapshot> {
        self.health.snapshot()
    }

    /// Number of endpoints currently in cooldown.
    pub fn cooled_down_count(&self) -> usize {
        self.health.cooled_down_count()
    }

    /// The chain name this transport was built for.
    pub fn chain_name(&self) -> &str {
        &self.chain_name
    }

    /// One JSON-RPC call with retry + failover. Returns the decoded `result`.
    pub async fn call<T: DeserializeOwned>(
        &self,
        method: &str,
        params: &Value,
    ) -> Result<T, SolanaRpcError> {
        let value = self.call_raw(method, params).await?;
        serde_json::from_value::<T>(value)
            .map_err(|e| SolanaRpcError::Decode(format!("{method}: {e}")))
    }

    /// One JSON-RPC call with retry + failover, returning the raw `result`.
    pub async fn call_raw(&self, method: &str, params: &Value) -> Result<Value, SolanaRpcError> {
        let mut last: Option<SolanaRpcError> = None;
        for attempt in 0..MAX_ATTEMPTS {
            for (idx, endpoint) in self.endpoints.iter().enumerate() {
                let started = Instant::now();
                match self.post(endpoint, method, params).await {
                    Ok(value) => {
                        self.health.record_success(idx, started.elapsed(), None);
                        return Ok(value);
                    }
                    Err(failure) => {
                        let backoff = failure.retryable.then(|| backoff_for(attempt));
                        self.health.record_failure(idx, failure.retryable, backoff);
                        if !failure.retryable {
                            return Err(failure.error);
                        }
                        last = Some(failure.error);
                    }
                }
            }
            // Every endpoint failed this pass with a transient error; pause
            // before the next pass.
            if attempt + 1 < MAX_ATTEMPTS {
                tokio::time::sleep(backoff_for(attempt)).await;
            }
        }
        Err(last.unwrap_or_else(|| SolanaRpcError::NoEndpoints(self.chain_name.clone())))
    }

    /// One raw HTTP POST, collapsing transport + node errors into a
    /// classified [`CallFailure`].
    async fn post(
        &self,
        endpoint: &Endpoint,
        method: &str,
        params: &Value,
    ) -> Result<Value, CallFailure> {
        let body =
            serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
        let response = endpoint
            .client
            .post(&endpoint.url)
            .json(&body)
            .send()
            .await
            .map_err(classify_reqwest_error)?;

        let status = response.status();
        if !status.is_success() {
            let retryable = should_retry(RetrySignal::HttpStatus(status.as_u16()));
            return Err(CallFailure {
                error: SolanaRpcError::Transport(format!(
                    "{} returned HTTP {}",
                    endpoint.url,
                    status.as_u16()
                )),
                retryable,
            });
        }

        let payload: Value = response.json().await.map_err(|e| CallFailure {
            error: SolanaRpcError::Decode(e.to_string()),
            retryable: false,
        })?;

        if let Some(error) = payload.get("error") {
            let code = error.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
            let message = error
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or_default()
                .to_string();
            let retryable = should_retry(RetrySignal::RpcError {
                code,
                message: &message,
            });
            return Err(CallFailure {
                error: SolanaRpcError::Rpc { code, message },
                retryable,
            });
        }

        Ok(payload.get("result").cloned().unwrap_or(Value::Null))
    }

    fn spawn_probe_loop(&self, mut shutdown_rx: watch::Receiver<bool>) {
        let chain = self.chain_name.clone();
        let endpoints: Vec<(usize, String, reqwest::Client)> = self
            .endpoints
            .iter()
            .enumerate()
            .map(|(i, e)| (i, e.url.clone(), e.client.clone()))
            .collect();
        let health = self.health.clone();
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            tracing::debug!(chain = %chain, "solana.health.probe_loop_skipped_no_runtime");
            return;
        };
        handle.spawn(async move {
            tracing::info!(
                chain = %chain,
                endpoints = endpoints.len(),
                "solana.health.probe_loop_started"
            );
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(PROBE_INTERVAL) => {}
                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() {
                            return;
                        }
                    }
                }
                for (idx, url, client) in &endpoints {
                    let started = Instant::now();
                    let body = serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "getHealth", "params": [] });
                    let result = client.post(url).json(&body).send().await;
                    match result {
                        Ok(resp) if resp.status().is_success() => {
                            health.record_success(*idx, started.elapsed(), None);
                        }
                        Ok(resp) => {
                            let retryable = should_retry(RetrySignal::HttpStatus(resp.status().as_u16()));
                            health.record_failure(*idx, retryable, None);
                        }
                        Err(_) => {
                            health.record_failure(*idx, true, None);
                        }
                    }
                }
            }
        });
    }
}

impl Drop for SolanaRpcClient {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(true);
        }
    }
}

fn backoff_for(attempt: usize) -> Duration {
    Duration::from_millis(INITIAL_BACKOFF_MS << attempt.min(8))
}

fn classify_reqwest_error(e: reqwest::Error) -> CallFailure {
    if e.is_timeout() || e.is_connect() {
        return CallFailure {
            error: SolanaRpcError::Transport(e.to_string()),
            retryable: true,
        };
    }
    CallFailure {
        error: SolanaRpcError::Transport(e.to_string()),
        retryable: false,
    }
}
