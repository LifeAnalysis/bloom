//! Read-only Solana chain client.
//!
//! The in-tree analogue of `bloom-evm::ChainClient` for Solana: a typed,
//! genesis-bound read surface over the layered [`SolanaRpcClient`] transport.
//! It performs no signing, no broadcasting, and no account custody — those
//! belong to the `bloom-solana-tx` engine and the Broker/Signer triad.
//!
//! Unlike EVM, Solana has no `alloy` equivalent worth adopting here; this
//! crate's transport is `reqwest`-based (see [`transport`]) and reuses the
//! chain-neutral [`bloom_rpc_common::HealthRegistry`] for endpoint health.

#![forbid(unsafe_code)]

pub mod error;
pub mod retry;
pub mod transport;

use std::sync::Arc;

pub use error::SolanaRpcError;
pub use transport::SolanaRpcClient;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub use bloom_proto::EndpointSpec;

/// Operator configuration for one Solana cluster.
///
/// Chain-neutral endpoint config is reused from `bloom_proto::EndpointSpec`;
/// the Solana-specific fields (genesis binding, broadcast posture) live here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SolanaSpec {
    /// Filesystem-friendly name, e.g. `"solana-devnet"`.
    pub name: String,
    /// Configured RPC endpoints, in preference order.
    #[serde(default)]
    pub endpoints: Vec<EndpointSpec>,
    /// Expected genesis hash (base58). When set, the client refuses to talk
    /// to a node whose `getGenesisHash` differs — the Solana analogue of
    /// EVM's chain-id binding (a message carries a blockhash, not a chain id).
    #[serde(default)]
    pub expected_genesis_hex: Option<String>,
    /// Whether broadcasting is enabled on this cluster.
    #[serde(default)]
    pub allow_broadcast: bool,
}

impl SolanaSpec {
    /// Endpoints usable for HTTP reads.
    pub fn endpoints(&self) -> impl Iterator<Item = &EndpointSpec> {
        self.endpoints.iter()
    }
}

/// `getLatestBlockhash` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LatestBlockhash {
    /// The recent blockhash, base58-encoded.
    pub blockhash: String,
    /// The last block height at which the blockhash is still valid.
    pub last_valid_block_height: u64,
}

/// One entry of `getSignatureStatuses`'s `value` array (the `null` case is
/// represented by the outer `Option`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignatureStatus {
    pub slot: u64,
    pub confirmations: Option<u64>,
    #[serde(default)]
    pub err: Option<Value>,
    #[serde(default)]
    pub confirmation_status: Option<String>,
}

/// `simulateTransaction`'s `value` object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Simulation {
    #[serde(default)]
    pub err: Option<Value>,
    #[serde(default)]
    pub logs: Option<Vec<String>>,
    #[serde(default)]
    pub units_consumed: Option<u64>,
    #[serde(default)]
    pub return_data: Option<Value>,
}

/// A registry of Solana clients keyed by chain name, mirroring
/// `bloom-evm::ChainRegistry`.
#[derive(Clone, Default)]
pub struct SolanaChainRegistry {
    inner: std::sync::Arc<parking_lot::RwLock<std::collections::BTreeMap<String, SolanaClient>>>,
}

impl SolanaChainRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&self, client: SolanaClient) {
        let name = client.chain_name().to_string();
        self.inner.write().insert(name, client);
    }

    pub fn get(&self, name: &str) -> Option<SolanaClient> {
        self.inner.read().get(name).cloned()
    }

    pub fn list_names(&self) -> Vec<String> {
        self.inner.read().keys().cloned().collect()
    }

    pub fn from_specs<I: IntoIterator<Item = SolanaSpec>>(
        specs: I,
    ) -> Result<Self, SolanaRpcError> {
        let r = Self::new();
        for spec in specs {
            r.add(SolanaClient::build(&spec)?);
        }
        Ok(r)
    }
}

/// The read-only chain client. Clone is cheap (Arc inside).
#[derive(Clone)]
pub struct SolanaClient {
    inner: Arc<Inner>,
}

struct Inner {
    rpc: Arc<SolanaRpcClient>,
    expected_genesis_hex: Option<String>,
}

impl SolanaClient {
    /// Build a client over `spec`. Fails on an empty endpoint list.
    pub fn build(spec: &SolanaSpec) -> Result<Self, SolanaRpcError> {
        let rpc = Arc::new(SolanaRpcClient::build(spec)?);
        Ok(Self {
            inner: Arc::new(Inner {
                rpc,
                expected_genesis_hex: spec.expected_genesis_hex.clone(),
            }),
        })
    }

    /// The underlying transport's endpoint-health snapshot.
    pub fn endpoints_snapshot(&self) -> Vec<bloom_rpc_common::EndpointHealthSnapshot> {
        self.inner.rpc.endpoints_snapshot()
    }

    /// Chain name this client was built for.
    pub fn chain_name(&self) -> &str {
        self.inner.rpc.chain_name()
    }

    /// Verify the node's genesis hash matches the spec, at most once per
    /// client (result cached). A mismatch is a hard refusal.
    pub async fn verify_genesis(&self) -> Result<String, SolanaRpcError> {
        let observed = self.get_genesis_hash().await?;
        if let Some(expected) = &self.inner.expected_genesis_hex
            && expected != &observed
        {
            return Err(SolanaRpcError::GenesisMismatch {
                chain: self.chain_name().to_string(),
                expected: expected.clone(),
                observed,
            });
        }
        Ok(observed)
    }

    /// Node health (`getHealth`). Ok when the node reports `"ok"`.
    pub async fn get_health(&self) -> Result<(), SolanaRpcError> {
        let result = self.inner.rpc.call_raw("getHealth", &json!([])).await?;
        if result.as_str() == Some("ok") {
            Ok(())
        } else {
            Err(SolanaRpcError::Decode(format!(
                "getHealth returned {result}"
            )))
        }
    }

    /// Cluster genesis hash, base58-encoded.
    pub async fn get_genesis_hash(&self) -> Result<String, SolanaRpcError> {
        self.inner.rpc.call("getGenesisHash", &json!([])).await
    }

    /// Current slot.
    pub async fn get_slot(&self) -> Result<u64, SolanaRpcError> {
        self.inner.rpc.call("getSlot", &json!([])).await
    }

    /// Current block height (processed blocks, not necessarily finalized).
    pub async fn get_block_height(&self) -> Result<u64, SolanaRpcError> {
        self.inner.rpc.call("getBlockHeight", &json!([])).await
    }

    /// Native SOL balance in lamports for a base58 account address.
    pub async fn get_balance(&self, account: &str) -> Result<u64, SolanaRpcError> {
        let result: Value = self.inner.rpc.call("getBalance", &json!([account])).await?;
        result
            .get("value")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| SolanaRpcError::Decode(format!("getBalance: {result}")))
    }

    /// A recent blockhash and its last-valid block height.
    pub async fn get_latest_blockhash(&self) -> Result<LatestBlockhash, SolanaRpcError> {
        let result: Value = self
            .inner
            .rpc
            .call("getLatestBlockhash", &json!([]))
            .await?;
        let value = result
            .get("value")
            .ok_or_else(|| SolanaRpcError::Decode(format!("getLatestBlockhash: {result}")))?;
        let blockhash = value
            .get("blockhash")
            .and_then(|v| v.as_str())
            .ok_or_else(|| SolanaRpcError::Decode(format!("getLatestBlockhash: {value}")))?;
        let last_valid_block_height = value
            .get("lastValidBlockHeight")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| SolanaRpcError::Decode(format!("getLatestBlockhash: {value}")))?;
        Ok(LatestBlockhash {
            blockhash: blockhash.to_string(),
            last_valid_block_height,
        })
    }

    /// Fee for a serialized message (base64), if the node can quote it.
    pub async fn get_fee_for_message(
        &self,
        message_b64: &str,
    ) -> Result<Option<u64>, SolanaRpcError> {
        let result: Value = self
            .inner
            .rpc
            .call("getFeeForMessage", &json!([message_b64]))
            .await?;
        Ok(result.get("value").and_then(|v| v.as_u64()))
    }

    /// Simulate a signed transaction (base64) without committing it.
    pub async fn simulate_transaction(&self, tx_b64: &str) -> Result<Simulation, SolanaRpcError> {
        let result: Value = self
            .inner
            .rpc
            .call("simulateTransaction", &json!([tx_b64]))
            .await?;
        let value = result.get("value").cloned().unwrap_or(Value::Null);
        serde_json::from_value::<Simulation>(value)
            .map_err(|e| SolanaRpcError::Decode(format!("simulateTransaction: {e}")))
    }

    /// Submit a signed transaction (base64) to the cluster. Returns the
    /// transaction signature. This is the write path the transaction engine
    /// gates — the read client itself performs no gating beyond the transport.
    pub async fn send_transaction(&self, tx_b64: &str) -> Result<String, SolanaRpcError> {
        self.inner
            .rpc
            .call("sendTransaction", &json!([tx_b64]))
            .await
    }

    /// Confirmation status for a list of transaction signatures. The outer
    /// `Option` mirrors the node's `null` entries (signature not seen).
    pub async fn get_signature_statuses(
        &self,
        signatures: &[String],
    ) -> Result<Vec<Option<SignatureStatus>>, SolanaRpcError> {
        let result: Value = self
            .inner
            .rpc
            .call("getSignatureStatuses", &json!([signatures]))
            .await?;
        let values = result
            .get("value")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        values
            .into_iter()
            .map(|v| {
                if v.is_null() {
                    Ok(None)
                } else {
                    serde_json::from_value::<SignatureStatus>(v)
                        .map(Some)
                        .map_err(|e| SolanaRpcError::Decode(format!("getSignatureStatuses: {e}")))
                }
            })
            .collect()
    }
}
