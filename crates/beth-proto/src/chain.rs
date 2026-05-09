//! Chain identifiers and per-chain config.

use serde::{Deserialize, Serialize};

/// The numeric EVM chainId. `1`, `10`, `8453`, …
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ChainId(pub u64);

impl std::fmt::Display for ChainId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<u64> for ChainId {
    fn from(n: u64) -> Self {
        ChainId(n)
    }
}

/// A user-facing chain name (e.g. "ethereum", "base", "anvil").
///
/// Stored alongside the numeric chainId so the FS path layout is the
/// human-readable name while the JSON-RPC layer uses the number.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ChainRef(pub String);

impl ChainRef {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ChainRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Per-chain configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainSpec {
    /// Filesystem-friendly name (e.g. "ethereum", "anvil", "base").
    pub name: String,
    /// EVM chainId.
    pub chain_id: u64,
    /// RPC endpoints in priority order. The first is preferred; later
    /// entries are failovers.
    pub rpc_urls: Vec<String>,
    /// Whether broadcasts are allowed on this chain. **False by default
    /// on mainnet/L2** to enforce the "never broadcast to mainnet
    /// without explicit operator config" rule.
    #[serde(default)]
    pub allow_broadcast: bool,
    /// Etherscan-compatible API base URL (optional).
    #[serde(default)]
    pub etherscan_api_url: Option<String>,
    /// Display name for plan.md (e.g. "Ethereum Mainnet").
    #[serde(default)]
    pub display_name: Option<String>,
    /// Symbol of the native token, e.g. "ETH", "MATIC". Defaults to "ETH".
    #[serde(default = "default_native")]
    pub native_symbol: String,
    /// Decimals of the native token. Defaults to 18.
    #[serde(default = "default_native_decimals")]
    pub native_decimals: u8,
    /// When true, build legacy (pre-EIP-1559) transactions on this
    /// chain — populating `gas_price` instead of `max_fee_per_gas` /
    /// `max_priority_fee_per_gas`. Defaults to false (EIP-1559).
    #[serde(default)]
    pub legacy_tx: bool,
}

fn default_native() -> String {
    "ETH".to_string()
}
fn default_native_decimals() -> u8 {
    18
}

impl ChainSpec {
    /// A safe default Anvil chain spec for local development.
    pub fn anvil_default() -> Self {
        ChainSpec {
            name: "anvil".to_string(),
            chain_id: 31337,
            rpc_urls: vec!["http://127.0.0.1:8545".to_string()],
            allow_broadcast: true,
            etherscan_api_url: None,
            display_name: Some("Anvil (local)".to_string()),
            native_symbol: "ETH".to_string(),
            native_decimals: 18,
            legacy_tx: false,
        }
    }

    pub fn id(&self) -> ChainId {
        ChainId(self.chain_id)
    }
    pub fn r#ref(&self) -> ChainRef {
        ChainRef::new(&self.name)
    }
}
