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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_id_display_and_from() {
        let id: ChainId = 8453u64.into();
        assert_eq!(id.0, 8453);
        assert_eq!(id.to_string(), "8453");
    }

    #[test]
    fn chain_ref_new_and_display() {
        let r = ChainRef::new("ethereum");
        assert_eq!(r.as_str(), "ethereum");
        assert_eq!(r.to_string(), "ethereum");
    }

    #[test]
    fn chain_id_serializes_transparently_as_number() {
        let id = ChainId(1);
        let s = serde_json::to_string(&id).unwrap();
        assert_eq!(s, "1");
        let back: ChainId = serde_json::from_str("1").unwrap();
        assert_eq!(back, id);
    }

    #[test]
    fn chain_ref_serializes_transparently_as_string() {
        let r = ChainRef::new("base");
        let s = serde_json::to_string(&r).unwrap();
        assert_eq!(s, "\"base\"");
        let back: ChainRef = serde_json::from_str("\"base\"").unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn anvil_default_shape() {
        let c = ChainSpec::anvil_default();
        assert_eq!(c.name, "anvil");
        assert_eq!(c.chain_id, 31337);
        assert_eq!(c.id(), ChainId(31337));
        assert_eq!(c.r#ref().as_str(), "anvil");
        assert!(c.allow_broadcast);
        assert_eq!(c.rpc_urls, vec!["http://127.0.0.1:8545".to_string()]);
        assert_eq!(c.native_symbol, "ETH");
        assert_eq!(c.native_decimals, 18);
        assert!(!c.legacy_tx);
        assert!(c.etherscan_api_url.is_none());
    }

    #[test]
    fn chain_spec_toml_round_trip() {
        let original = ChainSpec::anvil_default();
        let s = toml::to_string(&original).unwrap();
        let back: ChainSpec = toml::from_str(&s).unwrap();
        assert_eq!(back, original);
    }

    #[test]
    fn chain_spec_json_round_trip() {
        let original = ChainSpec {
            name: "ethereum".to_string(),
            chain_id: 1,
            rpc_urls: vec!["https://rpc.example".to_string()],
            allow_broadcast: false,
            etherscan_api_url: Some("https://api.etherscan.io/v2/api".to_string()),
            display_name: Some("Ethereum Mainnet".to_string()),
            native_symbol: "ETH".to_string(),
            native_decimals: 18,
            legacy_tx: false,
        };
        let s = serde_json::to_string(&original).unwrap();
        let back: ChainSpec = serde_json::from_str(&s).unwrap();
        assert_eq!(back, original);
    }

    #[test]
    fn chain_spec_defaults_apply_when_missing_fields() {
        let toml_text = r#"
            name = "minimal"
            chain_id = 42
            rpc_urls = ["http://x"]
        "#;
        let c: ChainSpec = toml::from_str(toml_text).unwrap();
        assert_eq!(c.name, "minimal");
        assert_eq!(c.chain_id, 42);
        // serde defaults should fill in everything else
        assert!(!c.allow_broadcast);
        assert_eq!(c.native_symbol, "ETH");
        assert_eq!(c.native_decimals, 18);
        assert!(!c.legacy_tx);
        assert!(c.etherscan_api_url.is_none());
        assert!(c.display_name.is_none());
    }
}
