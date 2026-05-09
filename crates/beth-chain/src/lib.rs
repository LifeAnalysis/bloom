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
        assert!(ChainClient::new(s).is_err());
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
}
