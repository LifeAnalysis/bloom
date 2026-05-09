//! Daemon configuration loaded from `~/.bloom-eth/config.toml`.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::chain::ChainSpec;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("toml parse error: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("toml serialise error: {0}")]
    TomlSer(#[from] toml::ser::Error),
    #[error("invalid config: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Default mount path (informational; the kernel mount is opt-in).
    #[serde(default = "default_mount_path")]
    pub mount_path: String,
    /// Address the NFS server listens on. Loopback only by default.
    #[serde(default = "default_nfs_listen")]
    pub nfs_listen_addr: String,
    /// Default chain to use when an intent omits `chain`.
    #[serde(default = "default_chain_name")]
    pub default_chain: String,
    /// Outbox stage TTL.
    #[serde(default = "default_stage_ttl", with = "humantime_serde")]
    pub stage_ttl: std::time::Duration,
    /// Map of chain name -> spec.
    #[serde(default)]
    pub chains: BTreeMap<String, ChainSpec>,
    #[serde(default)]
    pub etherscan: Option<EtherscanConfig>,
    #[serde(default)]
    pub enso: Option<EnsoConfig>,
    /// Kill-switch: never permit broadcast to mainnet chain ids regardless
    /// of per-chain `allow_broadcast`.
    #[serde(default = "default_mainnet_block")]
    pub block_mainnet_broadcast: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EtherscanConfig {
    /// Etherscan v2 multi-chain API key.
    pub api_key: String,
    #[serde(default = "default_etherscan_url")]
    pub api_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnsoConfig {
    pub api_key: String,
    #[serde(default = "default_enso_url")]
    pub api_url: String,
}

fn default_mount_path() -> String {
    "/eth".to_string()
}
fn default_nfs_listen() -> String {
    "127.0.0.1:12049".to_string()
}
fn default_chain_name() -> String {
    "anvil".to_string()
}
fn default_stage_ttl() -> std::time::Duration {
    std::time::Duration::from_secs(3600)
}
fn default_etherscan_url() -> String {
    "https://api.etherscan.io/v2/api".to_string()
}
fn default_enso_url() -> String {
    "https://api.enso.finance".to_string()
}
fn default_mainnet_block() -> bool {
    true
}

impl Config {
    /// A safe local-dev default: Anvil only, no broadcast on mainnet ids.
    pub fn local_default() -> Self {
        let mut chains = BTreeMap::new();
        chains.insert("anvil".to_string(), ChainSpec::anvil_default());
        Config {
            mount_path: default_mount_path(),
            nfs_listen_addr: default_nfs_listen(),
            default_chain: "anvil".to_string(),
            stage_ttl: default_stage_ttl(),
            chains,
            etherscan: None,
            enso: None,
            block_mainnet_broadcast: true,
        }
    }

    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let s = std::fs::read_to_string(path)?;
        let cfg: Self = toml::from_str(&s)?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        let s = toml::to_string_pretty(self)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, s)?;
        Ok(())
    }

    pub fn load_or_init(path: &Path) -> Result<Self, ConfigError> {
        if path.exists() {
            Self::load(path)
        } else {
            let cfg = Self::local_default();
            cfg.save(path)?;
            Ok(cfg)
        }
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.chains.is_empty() {
            return Err(ConfigError::Invalid(
                "config.chains must contain at least one entry".into(),
            ));
        }
        if !self.chains.contains_key(&self.default_chain) {
            return Err(ConfigError::Invalid(format!(
                "default_chain={} not in chains",
                self.default_chain
            )));
        }
        for (k, c) in &self.chains {
            if k != &c.name {
                return Err(ConfigError::Invalid(format!(
                    "chain key '{}' != name '{}'",
                    k, c.name
                )));
            }
            if c.rpc_urls.is_empty() {
                return Err(ConfigError::Invalid(format!(
                    "chain '{}' has no rpc_urls",
                    k
                )));
            }
        }
        Ok(())
    }

    pub fn chain(&self, name: &str) -> Option<&ChainSpec> {
        self.chains.get(name)
    }

    /// Is this chain id one we *consider* mainnet for the kill-switch?
    pub fn is_mainnet_id(chain_id: u64) -> bool {
        matches!(
            chain_id,
            1 | 10 | 137 | 8453 | 42161 | 56 | 43114 | 100 | 250 | 324 | 59144 | 534352
        )
    }

    /// Whether broadcast is ultimately allowed on this chain.
    pub fn broadcast_permitted(&self, c: &ChainSpec) -> bool {
        if self.block_mainnet_broadcast && Self::is_mainnet_id(c.chain_id) {
            return false;
        }
        c.allow_broadcast
    }
}
