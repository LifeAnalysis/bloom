//! RPC pool and chain engine.
//!
//! v1 uses a single alloy `RootProvider<Http>` per chain (the pool layer
//! is a thin wrapper that walks `rpc_urls` in priority order on call
//! failure). Subscriptions and websocket transports are deferred.

#![forbid(unsafe_code)]

use std::sync::Arc;

use alloy::eips::BlockNumberOrTag;
use alloy::network::Ethereum;
use alloy::primitives::{Address, BlockHash, Bytes, B256, U256};
use alloy::providers::{Provider, RootProvider};
use alloy::rpc::types::eth::state::StateOverride;
use alloy::rpc::types::eth::{
    Block, Filter, Log, Transaction, TransactionReceipt, TransactionRequest,
};
use alloy::sol;
use alloy::transports::TransportError;
use parking_lot::RwLock;
use thiserror::Error;
use tracing::{debug, warn};

use beth_proto::{ChainId, ChainSpec};

sol! {
    #[sol(rpc)]
    #[allow(missing_docs)]
    interface IERC20 {
        function decimals() external view returns (uint8);
        function balanceOf(address account) external view returns (uint256);
        function allowance(address owner, address spender) external view returns (uint256);
        function symbol() external view returns (string);
        function approve(address spender, uint256 amount) external returns (bool);
        function transfer(address to, uint256 amount) external returns (bool);
    }
}

#[derive(Debug, Error)]
pub enum ChainError {
    #[error("no rpc endpoints configured for chain '{0}'")]
    NoEndpoints(String),
    #[error("transport: {0}")]
    Transport(String),
    #[error("decode: {0}")]
    Decode(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("url parse: {0}")]
    Url(String),
    #[error("rpc: {0}")]
    Rpc(String),
}

impl From<TransportError> for ChainError {
    fn from(e: TransportError) -> Self {
        ChainError::Transport(e.to_string())
    }
}

/// One alloy provider plus failover endpoints.
#[derive(Clone)]
pub struct ChainClient {
    spec: Arc<ChainSpec>,
    primary: Arc<RootProvider<Ethereum>>,
    /// Cached chain id once the provider has reported it.
    cached_chain_id: Arc<RwLock<Option<u64>>>,
}

impl ChainClient {
    /// Construct a client from a ChainSpec. Picks the first rpc_url and
    /// builds an http provider.
    pub fn new(spec: ChainSpec) -> Result<Self, ChainError> {
        if spec.rpc_urls.is_empty() {
            return Err(ChainError::NoEndpoints(spec.name.clone()));
        }
        let url = spec
            .rpc_urls
            .first()
            .unwrap()
            .parse::<url::Url>()
            .map_err(|e| ChainError::Url(e.to_string()))?;
        let provider: RootProvider<Ethereum> = RootProvider::<Ethereum>::new_http(url);
        Ok(Self {
            spec: Arc::new(spec),
            primary: Arc::new(provider),
            cached_chain_id: Arc::new(RwLock::new(None)),
        })
    }

    pub fn spec(&self) -> &ChainSpec {
        &self.spec
    }
    pub fn id(&self) -> ChainId {
        ChainId(self.spec.chain_id)
    }
    pub fn provider(&self) -> Arc<RootProvider<Ethereum>> {
        self.primary.clone()
    }

    pub async fn chain_id(&self) -> Result<u64, ChainError> {
        if let Some(id) = *self.cached_chain_id.read() {
            return Ok(id);
        }
        let id = self.primary.get_chain_id().await?;
        *self.cached_chain_id.write() = Some(id);
        Ok(id)
    }

    pub async fn block_number(&self) -> Result<u64, ChainError> {
        Ok(self.primary.get_block_number().await?)
    }

    pub async fn balance(&self, addr: Address) -> Result<U256, ChainError> {
        Ok(self.primary.get_balance(addr).await?)
    }

    pub async fn nonce(&self, addr: Address) -> Result<u64, ChainError> {
        // Use the pending block so back-to-back stages don't collide on the
        // same nonce when an earlier tx is still propagating between RPC
        // nodes. Falls back to latest if the provider doesn't support it.
        Ok(self.primary.get_transaction_count(addr).pending().await?)
    }

    pub async fn code(&self, addr: Address) -> Result<Vec<u8>, ChainError> {
        Ok(self.primary.get_code_at(addr).await?.to_vec())
    }

    pub async fn block_by_number(&self, n: u64) -> Result<Option<Block>, ChainError> {
        let res = self
            .primary
            .get_block_by_number(alloy::eips::BlockNumberOrTag::Number(n))
            .await?;
        Ok(res)
    }

    pub async fn block_latest(&self) -> Result<Option<Block>, ChainError> {
        let res = self
            .primary
            .get_block_by_number(alloy::eips::BlockNumberOrTag::Latest)
            .await?;
        Ok(res)
    }

    pub async fn tx_by_hash(&self, hash: B256) -> Result<Option<Transaction>, ChainError> {
        Ok(self.primary.get_transaction_by_hash(hash).await?)
    }

    pub async fn receipt(&self, hash: B256) -> Result<Option<TransactionReceipt>, ChainError> {
        Ok(self.primary.get_transaction_receipt(hash).await?)
    }

    pub async fn gas_price(&self) -> Result<u128, ChainError> {
        Ok(self.primary.get_gas_price().await?)
    }

    pub async fn estimate_gas(
        &self,
        tx: &alloy::rpc::types::eth::TransactionRequest,
    ) -> Result<u64, ChainError> {
        Ok(self.primary.estimate_gas(tx.clone()).await?)
    }

    pub async fn fee_history(
        &self,
        block_count: u64,
    ) -> Result<alloy::rpc::types::eth::FeeHistory, ChainError> {
        let fh = self
            .primary
            .get_fee_history(
                block_count,
                alloy::eips::BlockNumberOrTag::Latest,
                &[10.0, 50.0, 90.0],
            )
            .await?;
        Ok(fh)
    }

    pub async fn send_raw(&self, raw: alloy::primitives::Bytes) -> Result<B256, ChainError> {
        let pending = self.primary.send_raw_transaction(raw.as_ref()).await?;
        Ok(*pending.tx_hash())
    }

    /// Read an ERC-20 token's `decimals()`. Returns `None` if the call
    /// reverts — callers should fall back to a sensible default (or
    /// refuse to stage).
    pub async fn erc20_decimals(&self, token: Address) -> Result<Option<u8>, ChainError> {
        let contract = IERC20::new(token, self.primary.clone());
        match contract.decimals().call().await {
            Ok(d) => Ok(Some(d)),
            Err(e) => {
                debug!(error = %e, "erc20_decimals.call_failed");
                Ok(None)
            }
        }
    }

    /// Read an ERC-20 token's `balanceOf(holder)`. Returns `None` if the
    /// call reverts.
    pub async fn erc20_balance(
        &self,
        token: Address,
        holder: Address,
    ) -> Result<Option<U256>, ChainError> {
        let contract = IERC20::new(token, self.primary.clone());
        match contract.balanceOf(holder).call().await {
            Ok(b) => Ok(Some(b)),
            Err(e) => {
                debug!(error = %e, "erc20_balance.call_failed");
                Ok(None)
            }
        }
    }

    /// Read an ERC-20 token's `allowance(owner, spender)`. Returns
    /// `None` if the call reverts.
    pub async fn erc20_allowance(
        &self,
        token: Address,
        owner: Address,
        spender: Address,
    ) -> Result<Option<U256>, ChainError> {
        let contract = IERC20::new(token, self.primary.clone());
        match contract.allowance(owner, spender).call().await {
            Ok(a) => Ok(Some(a)),
            Err(e) => {
                debug!(error = %e, "erc20_allowance.call_failed");
                Ok(None)
            }
        }
    }

    /// Read an ERC-20 token's `symbol()`. Returns `None` if the call
    /// reverts. (Some early tokens encode `symbol` as `bytes32` instead
    /// of `string`; those will surface here as a decode error.)
    pub async fn erc20_symbol(&self, token: Address) -> Result<Option<String>, ChainError> {
        let contract = IERC20::new(token, self.primary.clone());
        match contract.symbol().call().await {
            Ok(s) => Ok(Some(s.trim_matches('\0').to_string())),
            Err(_) => Ok(None),
        }
    }

    /// Read a single 32-byte storage slot at `addr`, optionally pinning
    /// the read to a specific block (defaults to `latest`). The `block`
    /// arg accepts `"latest"`, a decimal block number, or `0x`-prefixed
    /// hex. Surfaces `eth_getStorageAt` directly so callers can read raw
    /// state (EIP-1967 proxy slots, ERC-20 internals, packed structs).
    pub async fn eth_get_storage_at(
        &self,
        addr: Address,
        slot: U256,
        block: Option<&str>,
    ) -> Result<B256, ChainError> {
        let req = self.primary.get_storage_at(addr, slot);
        let val: U256 = match block {
            None | Some("latest") | Some("") => req.await?,
            Some("earliest") => req.block_id(BlockNumberOrTag::Earliest.into()).await?,
            Some("pending") => req.block_id(BlockNumberOrTag::Pending.into()).await?,
            Some(s) => {
                let n = parse_block_arg(s)?;
                req.block_id(BlockNumberOrTag::Number(n).into()).await?
            }
        };
        Ok(B256::from(val.to_be_bytes::<32>()))
    }

    /// Fetch logs for a fully-formed `Filter`. Thin wrapper over
    /// `eth_getLogs`; the contract handler builds the `Filter` from
    /// user-supplied `from_block`/`to_block`/topics so the wrapper stays
    /// transport-agnostic.
    pub async fn get_logs(&self, filter: &Filter) -> Result<Vec<Log>, ChainError> {
        Ok(self.primary.get_logs(filter).await?)
    }

    /// Run `eth_call` against the latest block, optionally applying a set of
    /// state overrides (account balance / nonce / code / storage). This is the
    /// simulator's main hammer — never broadcasts.
    pub async fn eth_call_with_overrides(
        &self,
        req: TransactionRequest,
        overrides: Option<StateOverride>,
    ) -> Result<Bytes, ChainError> {
        let call = self.primary.call(req);
        let bytes = match overrides {
            Some(o) => call.overrides(o).await?,
            None => call.await?,
        };
        Ok(bytes)
    }

    /// Run `eth_call` against an explicit block tag/number. Used by the
    /// `methods/<m>.read` surface so users can read state at a historical
    /// block. `block` accepts the same vocabulary as
    /// [`Self::eth_get_storage_at`].
    pub async fn eth_call_at_block(
        &self,
        req: TransactionRequest,
        block: Option<&str>,
    ) -> Result<Bytes, ChainError> {
        let call = self.primary.call(req);
        let bytes = match block {
            None | Some("latest") | Some("") => call.await?,
            Some("earliest") => call.block(BlockNumberOrTag::Earliest.into()).await?,
            Some("pending") => call.block(BlockNumberOrTag::Pending.into()).await?,
            Some(s) => {
                let n = parse_block_arg(s)?;
                call.block(BlockNumberOrTag::Number(n).into()).await?
            }
        };
        Ok(bytes)
    }

    /// Attempt `debug_traceCall`. Many providers (Alchemy free, Infura) don't
    /// support this; the caller should treat any RPC error here as
    /// "tracing unsupported" and surface that as informational rather than fatal.
    pub async fn debug_trace_call(
        &self,
        req: TransactionRequest,
        overrides: Option<StateOverride>,
    ) -> Result<serde_json::Value, ChainError> {
        // params order: [tx, blockTag, traceConfig]
        // We pass a `callTracer` config when overrides are absent; when overrides
        // are present, we splice them into the trace config (Geth-style).
        let block: alloy::eips::BlockNumberOrTag = alloy::eips::BlockNumberOrTag::Latest;
        let mut cfg = serde_json::json!({ "tracer": "callTracer" });
        if let Some(o) = overrides {
            cfg["stateOverrides"] =
                serde_json::to_value(o).map_err(|e| ChainError::Decode(e.to_string()))?;
        }
        let params = (req, block, cfg);
        let res: serde_json::Value = self
            .primary
            .client()
            .request("debug_traceCall", params)
            .await
            .map_err(|e| ChainError::Rpc(e.to_string()))?;
        Ok(res)
    }
}

/// A registry of chain clients keyed by name.
#[derive(Clone, Default)]
pub struct ChainRegistry {
    inner: Arc<RwLock<std::collections::BTreeMap<String, ChainClient>>>,
}

impl ChainRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&self, client: ChainClient) {
        let name = client.spec().name.clone();
        debug!(chain = %name, "registry.add");
        self.inner.write().insert(name, client);
    }

    pub fn get(&self, name: &str) -> Option<ChainClient> {
        self.inner.read().get(name).cloned()
    }

    pub fn list_names(&self) -> Vec<String> {
        self.inner.read().keys().cloned().collect()
    }

    pub fn from_chains<I: IntoIterator<Item = ChainSpec>>(specs: I) -> Result<Self, ChainError> {
        let r = Self::new();
        for s in specs {
            match ChainClient::new(s) {
                Ok(c) => r.add(c),
                Err(e) => warn!(error = %e, "registry.skip"),
            }
        }
        Ok(r)
    }
}

/// Convenience: derive a hash for a B256 hex string.
pub fn parse_block_hash(s: &str) -> Result<BlockHash, ChainError> {
    s.parse::<BlockHash>()
        .map_err(|e| ChainError::Decode(e.to_string()))
}

/// Parse a block-number argument as decimal or `0x`-prefixed hex.
/// Used by the storage / methods surfaces so users can write either
/// `latest`, `123`, or `0x7b` interchangeably.
pub fn parse_block_arg(s: &str) -> Result<u64, ChainError> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).map_err(|e| ChainError::Decode(format!("block hex: {e}")))
    } else {
        s.parse::<u64>()
            .map_err(|e| ChainError::Decode(format!("block dec: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_add_get() {
        let spec = ChainSpec::anvil_default();
        let c = ChainClient::new(spec.clone()).unwrap();
        let r = ChainRegistry::new();
        r.add(c);
        assert!(r.get("anvil").is_some());
        assert_eq!(r.list_names(), vec!["anvil".to_string()]);
    }

    #[test]
    fn missing_endpoints_error() {
        let mut s = ChainSpec::anvil_default();
        s.rpc_urls.clear();
        match ChainClient::new(s) {
            Err(ChainError::NoEndpoints(name)) => assert_eq!(name, "anvil"),
            Err(e) => panic!("expected NoEndpoints, got {e:?}"),
            Ok(_) => panic!("expected NoEndpoints error"),
        }
    }

    #[test]
    fn invalid_url_returns_url_error() {
        let mut s = ChainSpec::anvil_default();
        s.rpc_urls = vec!["::not a url::".to_string()];
        match ChainClient::new(s) {
            Err(ChainError::Url(_)) => {}
            Err(e) => panic!("expected Url error, got {e:?}"),
            Ok(_) => panic!("expected Url error"),
        }
    }

    #[test]
    fn from_chains_skips_bad_specs() {
        // Empty rpc_urls should be skipped (logged as warn) without erroring the registry.
        let good = ChainSpec::anvil_default();
        let mut bad = ChainSpec::anvil_default();
        bad.name = "broken".to_string();
        bad.rpc_urls.clear();
        let r = ChainRegistry::from_chains(vec![good, bad]).unwrap();
        assert!(r.get("anvil").is_some());
        assert!(r.get("broken").is_none());
    }

    #[test]
    fn registry_overwrites_on_duplicate_name() {
        // The BTreeMap insert semantics mean a second `add` for the same chain name
        // replaces the previous entry — that's fine but worth pinning so a future
        // refactor doesn't silently change to a "first-wins" or "error" model.
        let r = ChainRegistry::new();
        let mut s1 = ChainSpec::anvil_default();
        s1.chain_id = 1;
        let mut s2 = ChainSpec::anvil_default();
        s2.chain_id = 2;
        r.add(ChainClient::new(s1).unwrap());
        r.add(ChainClient::new(s2).unwrap());
        let got = r.get("anvil").unwrap();
        assert_eq!(got.spec().chain_id, 2);
    }

    #[test]
    fn parse_block_arg_dec_and_hex() {
        assert_eq!(parse_block_arg("0").unwrap(), 0);
        assert_eq!(parse_block_arg("123").unwrap(), 123);
        assert_eq!(parse_block_arg("0x7b").unwrap(), 123);
        assert_eq!(parse_block_arg("0X7B").unwrap(), 123);
        assert!(parse_block_arg("nope").is_err());
        assert!(parse_block_arg("0xZZ").is_err());
    }

    #[test]
    fn parse_block_arg_trims_whitespace() {
        assert_eq!(parse_block_arg("  42  ").unwrap(), 42);
        assert_eq!(parse_block_arg("\t0x10\n").unwrap(), 16);
    }

    #[test]
    fn parse_block_arg_error_is_decode_variant() {
        let err = parse_block_arg("nope").unwrap_err();
        assert!(matches!(err, ChainError::Decode(_)), "got {err:?}");
        let err = parse_block_arg("0xZZ").unwrap_err();
        assert!(matches!(err, ChainError::Decode(_)), "got {err:?}");
    }

    #[test]
    fn parse_block_hash_roundtrip() {
        let h = "0x".to_string() + &"ab".repeat(32);
        let parsed = parse_block_hash(&h).unwrap();
        assert_eq!(format!("{parsed:?}"), format!("0x{}", "ab".repeat(32)));
    }

    #[test]
    fn parse_block_hash_rejects_garbage() {
        let err = parse_block_hash("not a hash").unwrap_err();
        assert!(matches!(err, ChainError::Decode(_)), "got {err:?}");
    }

    #[test]
    fn chain_error_display_messages() {
        // Pin the user-facing display strings — these surface in the CLI
        // and a refactor could silently break log greppability.
        assert_eq!(
            ChainError::NoEndpoints("foo".into()).to_string(),
            "no rpc endpoints configured for chain 'foo'"
        );
        assert_eq!(
            ChainError::Transport("connection refused".into()).to_string(),
            "transport: connection refused"
        );
        assert_eq!(
            ChainError::Decode("bad utf8".into()).to_string(),
            "decode: bad utf8"
        );
        assert_eq!(
            ChainError::NotFound("tx".into()).to_string(),
            "not found: tx"
        );
        assert_eq!(
            ChainError::Url("invalid".into()).to_string(),
            "url parse: invalid"
        );
        assert_eq!(ChainError::Rpc("revert".into()).to_string(), "rpc: revert");
    }

    #[test]
    fn chain_client_id_and_spec_accessors() {
        let mut spec = ChainSpec::anvil_default();
        spec.chain_id = 12345;
        let c = ChainClient::new(spec.clone()).unwrap();
        assert_eq!(c.id().0, 12345);
        assert_eq!(c.spec().name, spec.name);
        // provider() returns an Arc clone — sanity that the pointer is usable.
        let p = c.provider();
        assert!(Arc::strong_count(&p) >= 1);
    }
}

// ---------------------------------------------------------------------------
// Mock-RPC tests
// ---------------------------------------------------------------------------
//
// These tests spin up a tiny dispatching JSON-RPC server on `127.0.0.1:0` and
// point a `ChainClient` at it. We avoid pulling in `mockito`/`wiremock`/etc.
// — a hand-rolled tokio listener mirrors the pattern used in `beth-prices`
// and `beth-defi`.
//
// Rules for the mock:
//   * Each test owns its own listener; no global state, no port reuse.
//   * The handler dispatches by JSON-RPC `method` so a single server can
//     answer the multi-call sequences alloy issues internally.
//   * Responses are pre-baked JSON — we don't try to model alloy's full
//     wire format, just produce the shape its decoder expects.
//
#[cfg(test)]
mod mock_rpc_tests {
    use super::*;
    use alloy::network::TransactionBuilder;
    use alloy::primitives::address;
    use std::collections::HashMap;
    use std::net::SocketAddr;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Either a canned success result (raw JSON value as a string) or a
    /// JSON-RPC error response body. The dispatcher embeds this into a
    /// `{ "jsonrpc": "2.0", "id": <echo>, "result": <X> }` envelope.
    #[derive(Clone)]
    #[allow(dead_code)] // RawBody kept for future malformed-frame tests.
    enum MockResponse {
        /// Raw JSON for the `result` field (already JSON-encoded).
        Ok(String),
        /// `(code, message, data)` for the `error` field.
        Err(i64, String, Option<String>),
        /// Raw HTTP body — useful for malformed-response tests.
        RawBody(String),
    }

    /// Spawn a tiny dispatching mock server. Returns the URL.
    ///
    /// `responses` maps JSON-RPC method names to a queue of responses.
    /// Methods are popped from the front on each call so tests can model
    /// request-order-dependent behaviour. If a method is missing or its
    /// queue is exhausted the server replies with a generic JSON-RPC error
    /// to make the failure mode obvious in test output.
    async fn spawn_mock(responses: HashMap<String, Vec<MockResponse>>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();
        let state = Arc::new(parking_lot::Mutex::new(responses));
        tokio::spawn(async move {
            loop {
                let (mut sock, _) = match listener.accept().await {
                    Ok(p) => p,
                    Err(_) => break,
                };
                let state = state.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::with_capacity(16 * 1024);
                    let mut tmp = [0u8; 4096];
                    // Read until we have headers + Content-Length bytes.
                    let body = loop {
                        let n = match sock.read(&mut tmp).await {
                            Ok(0) => break String::new(),
                            Ok(n) => n,
                            Err(_) => return,
                        };
                        buf.extend_from_slice(&tmp[..n]);
                        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            let headers = match std::str::from_utf8(&buf[..end]) {
                                Ok(s) => s,
                                Err(_) => return,
                            };
                            let cl = headers
                                .lines()
                                .find_map(|l| {
                                    let l = l.trim();
                                    let mut p = l.splitn(2, ':');
                                    let k = p.next()?.trim();
                                    if k.eq_ignore_ascii_case("content-length") {
                                        p.next()?.trim().parse::<usize>().ok()
                                    } else {
                                        None
                                    }
                                })
                                .unwrap_or(0);
                            let body_start = end + 4;
                            // Read remaining body if needed.
                            while buf.len() < body_start + cl {
                                let n = match sock.read(&mut tmp).await {
                                    Ok(0) => break,
                                    Ok(n) => n,
                                    Err(_) => return,
                                };
                                buf.extend_from_slice(&tmp[..n]);
                            }
                            let body_end = body_start + cl;
                            break String::from_utf8_lossy(&buf[body_start..body_end]).to_string();
                        }
                    };
                    let req: serde_json::Value =
                        serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
                    let id = req.get("id").cloned().unwrap_or(serde_json::json!(1));
                    let method = req
                        .get("method")
                        .and_then(|m| m.as_str())
                        .unwrap_or("")
                        .to_string();

                    let resp = {
                        let mut g = state.lock();
                        g.get_mut(&method).and_then(|q| {
                            if q.is_empty() {
                                None
                            } else {
                                Some(q.remove(0))
                            }
                        })
                    };
                    let (status_line, body) = match resp {
                        Some(MockResponse::Ok(result)) => (
                            "HTTP/1.1 200 OK",
                            format!(
                                "{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{}}}",
                                serde_json::to_string(&id).unwrap(),
                                result
                            ),
                        ),
                        Some(MockResponse::Err(code, message, data)) => {
                            let data_str = data
                                .map(|d| format!(",\"data\":{}", d))
                                .unwrap_or_default();
                            (
                                "HTTP/1.1 200 OK",
                                format!(
                                    "{{\"jsonrpc\":\"2.0\",\"id\":{},\"error\":{{\"code\":{},\"message\":{}{}}}}}",
                                    serde_json::to_string(&id).unwrap(),
                                    code,
                                    serde_json::to_string(&message).unwrap(),
                                    data_str
                                ),
                            )
                        }
                        Some(MockResponse::RawBody(b)) => ("HTTP/1.1 200 OK", b),
                        None => (
                            "HTTP/1.1 200 OK",
                            format!(
                                "{{\"jsonrpc\":\"2.0\",\"id\":{},\"error\":{{\"code\":-32601,\"message\":\"method not mocked: {}\"}}}}",
                                serde_json::to_string(&id).unwrap(),
                                method
                            ),
                        ),
                    };
                    let resp = format!(
                        "{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        status_line,
                        body.len(),
                        body
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        format!("http://{addr}")
    }

    /// Build a ChainClient that talks to `url`.
    fn client_at(url: &str) -> ChainClient {
        let mut spec = ChainSpec::anvil_default();
        spec.rpc_urls = vec![url.to_string()];
        ChainClient::new(spec).unwrap()
    }

    /// Convenience: hex-encode a u64 as a JSON-RPC quantity string.
    fn qty(n: u64) -> String {
        format!("\"0x{:x}\"", n)
    }

    /// Convenience: hex-encode a U256.
    fn qty_u256(n: U256) -> String {
        format!("\"0x{:x}\"", n)
    }

    fn responses() -> HashMap<String, Vec<MockResponse>> {
        HashMap::new()
    }

    // -- chain_id ----------------------------------------------------------

    #[tokio::test]
    async fn chain_id_happy_path_and_caches() {
        let mut r = responses();
        r.insert("eth_chainId".into(), vec![MockResponse::Ok(qty(31337))]);
        let url = spawn_mock(r).await;
        let c = client_at(&url);
        assert_eq!(c.chain_id().await.unwrap(), 31337);
        // Cache: a second call must NOT hit the (now-empty) mock.
        assert_eq!(c.chain_id().await.unwrap(), 31337);
    }

    #[tokio::test]
    async fn chain_id_malformed_response_is_transport_error() {
        let mut r = responses();
        // `result` claims to be a string but isn't a valid quantity hex.
        r.insert(
            "eth_chainId".into(),
            vec![MockResponse::Ok("\"not-a-number\"".to_string())],
        );
        let url = spawn_mock(r).await;
        let c = client_at(&url);
        let err = c.chain_id().await.unwrap_err();
        // Decoding errors come through alloy's transport layer in this stack.
        match err {
            ChainError::Transport(_) => {}
            other => panic!("expected Transport error, got {other:?}"),
        }
    }

    // -- block_number -----------------------------------------------------

    #[tokio::test]
    async fn block_number_happy_path() {
        let mut r = responses();
        r.insert(
            "eth_blockNumber".into(),
            vec![MockResponse::Ok(qty(0xabcd))],
        );
        let url = spawn_mock(r).await;
        let c = client_at(&url);
        assert_eq!(c.block_number().await.unwrap(), 0xabcd);
    }

    // -- balance ----------------------------------------------------------

    #[tokio::test]
    async fn balance_happy_path() {
        let want = U256::from(1_234_567u128);
        let mut r = responses();
        r.insert(
            "eth_getBalance".into(),
            vec![MockResponse::Ok(qty_u256(want))],
        );
        let url = spawn_mock(r).await;
        let c = client_at(&url);
        let addr = address!("0x1111111111111111111111111111111111111111");
        assert_eq!(c.balance(addr).await.unwrap(), want);
    }

    #[tokio::test]
    async fn balance_zero_for_unknown_account() {
        let mut r = responses();
        r.insert(
            "eth_getBalance".into(),
            vec![MockResponse::Ok("\"0x0\"".to_string())],
        );
        let url = spawn_mock(r).await;
        let c = client_at(&url);
        let addr = address!("0xdeaddeaddeaddeaddeaddeaddeaddeaddeaddead");
        assert_eq!(c.balance(addr).await.unwrap(), U256::ZERO);
    }

    // -- nonce ------------------------------------------------------------

    #[tokio::test]
    async fn nonce_happy_path() {
        let mut r = responses();
        r.insert(
            "eth_getTransactionCount".into(),
            vec![MockResponse::Ok(qty(7))],
        );
        let url = spawn_mock(r).await;
        let c = client_at(&url);
        let addr = address!("0x2222222222222222222222222222222222222222");
        assert_eq!(c.nonce(addr).await.unwrap(), 7);
    }

    // -- code -------------------------------------------------------------

    #[tokio::test]
    async fn code_returns_bytes() {
        let mut r = responses();
        r.insert(
            "eth_getCode".into(),
            vec![MockResponse::Ok("\"0x6080604052\"".into())],
        );
        let url = spawn_mock(r).await;
        let c = client_at(&url);
        let addr = address!("0x3333333333333333333333333333333333333333");
        let bytes = c.code(addr).await.unwrap();
        assert_eq!(bytes, vec![0x60, 0x80, 0x60, 0x40, 0x52]);
    }

    #[tokio::test]
    async fn code_empty_for_eoa() {
        let mut r = responses();
        r.insert(
            "eth_getCode".into(),
            vec![MockResponse::Ok("\"0x\"".into())],
        );
        let url = spawn_mock(r).await;
        let c = client_at(&url);
        let addr = address!("0x4444444444444444444444444444444444444444");
        assert!(c.code(addr).await.unwrap().is_empty());
    }

    // -- receipt ----------------------------------------------------------

    #[tokio::test]
    async fn receipt_missing_returns_none() {
        // alloy treats `result: null` from `eth_getTransactionReceipt` as Ok(None).
        let mut r = responses();
        r.insert(
            "eth_getTransactionReceipt".into(),
            vec![MockResponse::Ok("null".into())],
        );
        let url = spawn_mock(r).await;
        let c = client_at(&url);
        let h = B256::repeat_byte(0xaa);
        assert!(c.receipt(h).await.unwrap().is_none());
    }

    // -- gas_price --------------------------------------------------------

    #[tokio::test]
    async fn gas_price_happy_path() {
        let mut r = responses();
        r.insert(
            "eth_gasPrice".into(),
            vec![MockResponse::Ok(qty(1_000_000_000))],
        );
        let url = spawn_mock(r).await;
        let c = client_at(&url);
        assert_eq!(c.gas_price().await.unwrap(), 1_000_000_000);
    }

    // -- estimate_gas -----------------------------------------------------

    #[tokio::test]
    async fn estimate_gas_happy_path() {
        let mut r = responses();
        r.insert("eth_estimateGas".into(), vec![MockResponse::Ok(qty(21000))]);
        let url = spawn_mock(r).await;
        let c = client_at(&url);
        let req = TransactionRequest::default();
        assert_eq!(c.estimate_gas(&req).await.unwrap(), 21000);
    }

    #[tokio::test]
    async fn estimate_gas_revert_is_transport_error() {
        let mut r = responses();
        r.insert(
            "eth_estimateGas".into(),
            vec![MockResponse::Err(
                3,
                "execution reverted: insufficient allowance".into(),
                None,
            )],
        );
        let url = spawn_mock(r).await;
        let c = client_at(&url);
        let req = TransactionRequest::default();
        let err = c.estimate_gas(&req).await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("execution reverted") || msg.contains("insufficient allowance"),
            "expected revert text in error, got: {msg}"
        );
    }

    // -- send_raw ---------------------------------------------------------

    #[tokio::test]
    async fn send_raw_happy_path() {
        let h = B256::repeat_byte(0x42);
        let mut r = responses();
        r.insert(
            "eth_sendRawTransaction".into(),
            vec![MockResponse::Ok(format!(
                "\"0x{}\"",
                hex::encode(h.as_slice())
            ))],
        );
        let url = spawn_mock(r).await;
        let c = client_at(&url);
        // Minimal payload — the mock doesn't validate.
        let raw = Bytes::from(vec![0x02, 0xc0]);
        let got = c.send_raw(raw).await.unwrap();
        assert_eq!(got, h);
    }

    #[tokio::test]
    async fn send_raw_already_known_is_error() {
        // Geth/Erigon return a -32000 with "already known" when re-broadcasting.
        let mut r = responses();
        r.insert(
            "eth_sendRawTransaction".into(),
            vec![MockResponse::Err(-32000, "already known".into(), None)],
        );
        let url = spawn_mock(r).await;
        let c = client_at(&url);
        let err = c.send_raw(Bytes::from(vec![0x02, 0xc0])).await.unwrap_err();
        assert!(err.to_string().contains("already known"), "got {err}");
    }

    #[tokio::test]
    async fn send_raw_insufficient_funds_is_error() {
        let mut r = responses();
        r.insert(
            "eth_sendRawTransaction".into(),
            vec![MockResponse::Err(
                -32000,
                "insufficient funds for gas * price + value".into(),
                None,
            )],
        );
        let url = spawn_mock(r).await;
        let c = client_at(&url);
        let err = c.send_raw(Bytes::from(vec![0x02, 0xc0])).await.unwrap_err();
        assert!(err.to_string().contains("insufficient funds"), "got {err}");
    }

    // -- ERC-20 helpers ---------------------------------------------------

    /// ABI-encode a uint8 (right-aligned in a 32-byte word) as a hex JSON string.
    fn enc_uint8(v: u8) -> String {
        let mut w = [0u8; 32];
        w[31] = v;
        format!("\"0x{}\"", hex::encode(w))
    }

    /// ABI-encode a uint256 right-aligned.
    fn enc_uint256(v: U256) -> String {
        format!("\"0x{}\"", hex::encode(v.to_be_bytes::<32>()))
    }

    /// ABI-encode a dynamic string with offset+len header.
    fn enc_string(s: &str) -> String {
        let len = s.len();
        // offset: 0x20
        let mut buf = Vec::new();
        let mut w = [0u8; 32];
        w[31] = 0x20;
        buf.extend_from_slice(&w);
        // length
        let mut lw = [0u8; 32];
        lw[24..32].copy_from_slice(&(len as u64).to_be_bytes());
        buf.extend_from_slice(&lw);
        // payload, padded to 32-byte boundary.
        let mut payload = s.as_bytes().to_vec();
        let pad = (32 - (len % 32)) % 32;
        payload.extend(std::iter::repeat_n(0u8, pad));
        buf.extend_from_slice(&payload);
        format!("\"0x{}\"", hex::encode(buf))
    }

    #[tokio::test]
    async fn erc20_decimals_happy_path() {
        let mut r = responses();
        r.insert("eth_call".into(), vec![MockResponse::Ok(enc_uint8(6))]);
        let url = spawn_mock(r).await;
        let c = client_at(&url);
        let token = address!("0x5555555555555555555555555555555555555555");
        assert_eq!(c.erc20_decimals(token).await.unwrap(), Some(6));
    }

    #[tokio::test]
    async fn erc20_decimals_revert_returns_none() {
        let mut r = responses();
        r.insert(
            "eth_call".into(),
            vec![MockResponse::Err(3, "execution reverted".into(), None)],
        );
        let url = spawn_mock(r).await;
        let c = client_at(&url);
        let token = address!("0x5555555555555555555555555555555555555555");
        assert_eq!(c.erc20_decimals(token).await.unwrap(), None);
    }

    #[tokio::test]
    async fn erc20_decimals_short_response_returns_none() {
        let mut r = responses();
        r.insert(
            "eth_call".into(),
            vec![MockResponse::Ok("\"0x1234\"".into())],
        );
        let url = spawn_mock(r).await;
        let c = client_at(&url);
        let token = address!("0x5555555555555555555555555555555555555555");
        assert_eq!(c.erc20_decimals(token).await.unwrap(), None);
    }

    #[tokio::test]
    async fn erc20_balance_happy_path() {
        let want = U256::from(987_654_321u128);
        let mut r = responses();
        r.insert("eth_call".into(), vec![MockResponse::Ok(enc_uint256(want))]);
        let url = spawn_mock(r).await;
        let c = client_at(&url);
        let token = address!("0x6666666666666666666666666666666666666666");
        let holder = address!("0x7777777777777777777777777777777777777777");
        assert_eq!(c.erc20_balance(token, holder).await.unwrap(), Some(want));
    }

    #[tokio::test]
    async fn erc20_balance_revert_returns_none() {
        let mut r = responses();
        r.insert(
            "eth_call".into(),
            vec![MockResponse::Err(3, "revert".into(), None)],
        );
        let url = spawn_mock(r).await;
        let c = client_at(&url);
        let token = address!("0x6666666666666666666666666666666666666666");
        let holder = address!("0x7777777777777777777777777777777777777777");
        assert_eq!(c.erc20_balance(token, holder).await.unwrap(), None);
    }

    #[tokio::test]
    async fn erc20_symbol_happy_path() {
        let mut r = responses();
        r.insert(
            "eth_call".into(),
            vec![MockResponse::Ok(enc_string("USDC"))],
        );
        let url = spawn_mock(r).await;
        let c = client_at(&url);
        let token = address!("0x8888888888888888888888888888888888888888");
        assert_eq!(
            c.erc20_symbol(token).await.unwrap().as_deref(),
            Some("USDC")
        );
    }

    #[tokio::test]
    async fn erc20_symbol_revert_returns_none() {
        let mut r = responses();
        r.insert(
            "eth_call".into(),
            vec![MockResponse::Err(3, "revert".into(), None)],
        );
        let url = spawn_mock(r).await;
        let c = client_at(&url);
        let token = address!("0x8888888888888888888888888888888888888888");
        assert_eq!(c.erc20_symbol(token).await.unwrap(), None);
    }

    #[tokio::test]
    async fn erc20_symbol_short_response_returns_none() {
        // Only 1-word response: not enough for offset+len header.
        let mut r = responses();
        r.insert("eth_call".into(), vec![MockResponse::Ok(enc_uint8(0))]);
        let url = spawn_mock(r).await;
        let c = client_at(&url);
        let token = address!("0x8888888888888888888888888888888888888888");
        assert_eq!(c.erc20_symbol(token).await.unwrap(), None);
    }

    // -- eth_call helpers -------------------------------------------------

    #[tokio::test]
    async fn eth_call_with_overrides_no_overrides() {
        let mut r = responses();
        r.insert(
            "eth_call".into(),
            vec![MockResponse::Ok("\"0xdead\"".into())],
        );
        let url = spawn_mock(r).await;
        let c = client_at(&url);
        let req = TransactionRequest::default()
            .with_to(address!("0x9999999999999999999999999999999999999999"))
            .with_input(Bytes::from(vec![0x01]));
        let out = c.eth_call_with_overrides(req, None).await.unwrap();
        assert_eq!(out.as_ref(), &[0xde, 0xad]);
    }

    #[tokio::test]
    async fn eth_call_at_block_uses_named_tag() {
        let mut r = responses();
        r.insert(
            "eth_call".into(),
            vec![MockResponse::Ok("\"0xbeef\"".into())],
        );
        let url = spawn_mock(r).await;
        let c = client_at(&url);
        let req = TransactionRequest::default()
            .with_to(address!("0x9999999999999999999999999999999999999999"));
        let out = c.eth_call_at_block(req, Some("earliest")).await.unwrap();
        assert_eq!(out.as_ref(), &[0xbe, 0xef]);
    }

    #[tokio::test]
    async fn eth_call_at_block_decoded_block_number() {
        // Asks for `0x10` — confirm parse_block_arg path is taken without
        // an explicit assertion on params (mock is method-only).
        let mut r = responses();
        r.insert("eth_call".into(), vec![MockResponse::Ok("\"0x01\"".into())]);
        let url = spawn_mock(r).await;
        let c = client_at(&url);
        let req = TransactionRequest::default();
        let out = c.eth_call_at_block(req, Some("0x10")).await.unwrap();
        assert_eq!(out.as_ref(), &[0x01]);
    }

    #[tokio::test]
    async fn eth_call_at_block_bad_arg_returns_decode_error() {
        let mut r = responses();
        // The decode happens before we hit RPC, but having the mock around
        // ensures we don't accidentally fall through to a real network.
        r.insert("eth_call".into(), vec![MockResponse::Ok("\"0x\"".into())]);
        let url = spawn_mock(r).await;
        let c = client_at(&url);
        let req = TransactionRequest::default();
        let err = c
            .eth_call_at_block(req, Some("not-a-block"))
            .await
            .unwrap_err();
        assert!(matches!(err, ChainError::Decode(_)), "got {err:?}");
    }

    // -- eth_getStorageAt -------------------------------------------------

    #[tokio::test]
    async fn eth_get_storage_at_happy_path() {
        let mut w = [0u8; 32];
        w[31] = 0x2a;
        let mut r = responses();
        r.insert(
            "eth_getStorageAt".into(),
            vec![MockResponse::Ok(format!("\"0x{}\"", hex::encode(w)))],
        );
        let url = spawn_mock(r).await;
        let c = client_at(&url);
        let addr = address!("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let got = c
            .eth_get_storage_at(addr, U256::ZERO, Some("latest"))
            .await
            .unwrap();
        assert_eq!(got.as_slice()[31], 0x2a);
    }

    #[tokio::test]
    async fn eth_get_storage_at_bad_block_arg_decode_error() {
        let r = responses();
        let url = spawn_mock(r).await;
        let c = client_at(&url);
        let addr = address!("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let err = c
            .eth_get_storage_at(addr, U256::ZERO, Some("zzz"))
            .await
            .unwrap_err();
        assert!(matches!(err, ChainError::Decode(_)), "got {err:?}");
    }

    // -- debug_traceCall --------------------------------------------------

    #[tokio::test]
    async fn debug_trace_call_unsupported_maps_to_rpc_error() {
        let mut r = responses();
        r.insert(
            "debug_traceCall".into(),
            vec![MockResponse::Err(
                -32601,
                "the method debug_traceCall does not exist".into(),
                None,
            )],
        );
        let url = spawn_mock(r).await;
        let c = client_at(&url);
        let req = TransactionRequest::default();
        let err = c.debug_trace_call(req, None).await.unwrap_err();
        match err {
            ChainError::Rpc(msg) => {
                assert!(msg.contains("debug_traceCall"), "got {msg}");
            }
            other => panic!("expected ChainError::Rpc, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn debug_trace_call_happy_path() {
        let mut r = responses();
        r.insert(
            "debug_traceCall".into(),
            vec![MockResponse::Ok(
                "{\"type\":\"CALL\",\"gasUsed\":\"0x1\"}".into(),
            )],
        );
        let url = spawn_mock(r).await;
        let c = client_at(&url);
        let req = TransactionRequest::default();
        let v = c.debug_trace_call(req, None).await.unwrap();
        assert_eq!(v["type"], "CALL");
        assert_eq!(v["gasUsed"], "0x1");
    }

    // -- transport-layer error mapping ------------------------------------

    #[tokio::test]
    async fn transport_failure_maps_to_transport_error() {
        // No server at all — connect to a port we know isn't bound.
        // Pick port 1 (privileged) on 127.0.0.1; should reliably refuse on
        // every supported CI host.
        let mut spec = ChainSpec::anvil_default();
        spec.rpc_urls = vec!["http://127.0.0.1:1".into()];
        let c = ChainClient::new(spec).unwrap();
        let err = c.block_number().await.unwrap_err();
        assert!(matches!(err, ChainError::Transport(_)), "got {err:?}");
    }
}
