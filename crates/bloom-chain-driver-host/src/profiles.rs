//! Operator-configured chain-driver profiles.
//!
//! Only configured profiles are accepted — a driver Petal never supplies an
//! RPC endpoint. This is the one generic parser that turns operator JSON
//! into [`ChainRpcProfile`]s, with the family-agnostic policy gates applied
//! once here instead of once per chain family: mainnet is refused outright
//! (no chain family may declare a mainnet profile in this release posture),
//! and broadcast capability is always the AND of the profile's own flag and
//! the caller's explicit release request — never implicit either way.
//!
//! This crate ships no built-in profile for any chain: cluster/network
//! identity (genesis hashes, default endpoints) is chain-family knowledge
//! that belongs to that family's driver Petal, not to the generic Machine
//! host. See the crate-level doc for the planned migration of this config
//! source from an operator file to installer-validated Petal metadata.

use std::path::Path;

use bloom_chain_rpc::mediator::{ChainRpcProfile, DEFAULT_MAX_RESPONSE_BYTES};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const PROFILES_SCHEMA: &str = "bloom.chain-driver.profiles/1";

#[derive(Debug, Error)]
pub enum ProfileError {
    #[error("unknown chain-driver profile '{0}' (configured: {1})")]
    Unknown(String, String),
    #[error("mainnet profiles are disabled in this release posture: '{0}'")]
    MainnetDisabled(String),
    #[error("profile file schema '{0}' is not {PROFILES_SCHEMA}")]
    BadSchema(String),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileConfig {
    pub name: String,
    /// Dispatch key for `daemon_petal_chain_read`-style routing (e.g.
    /// `"solana"`, `"evm"`). Never inferred from `name`.
    pub family: String,
    pub expected_genesis_hex: String,
    pub http_endpoint: String,
    #[serde(default)]
    pub allowed_read_methods: Vec<String>,
    /// Whether broadcast is permitted at all on this profile. Actual
    /// broadcast capability additionally requires the caller's explicit
    /// release request; see [`resolve`].
    #[serde(default)]
    pub allow_broadcast: bool,
    #[serde(default = "default_max_response")]
    pub max_response_bytes: usize,
}

fn default_max_response() -> usize {
    DEFAULT_MAX_RESPONSE_BYTES
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ProfilesFile {
    schema: String,
    #[serde(default)]
    profiles: Vec<ProfileConfig>,
}

fn is_mainnet(profile: &ProfileConfig) -> bool {
    profile.name.to_lowercase().contains("mainnet")
        || profile.family.to_lowercase().contains("mainnet")
}

/// Load the operator-configured profile set from `<state>/chain-profiles.json`.
/// A missing file is not an error — it means no chain-driver profile is
/// configured. Mainnet profiles are refused at load time.
pub fn load_profiles(state_root: &Path) -> Result<Vec<ProfileConfig>, ProfileError> {
    let path = state_root.join("chain-profiles.json");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file: ProfilesFile = serde_json::from_slice(&std::fs::read(&path)?)?;
    if file.schema != PROFILES_SCHEMA {
        return Err(ProfileError::BadSchema(file.schema));
    }
    for profile in &file.profiles {
        if is_mainnet(profile) {
            return Err(ProfileError::MainnetDisabled(profile.name.clone()));
        }
    }
    Ok(file.profiles)
}

/// Resolve a configured profile by name into the mediator's chain profile.
/// Broadcast capability is the AND of the profile config and the caller's
/// explicit release request; never implicit.
pub fn resolve(
    profiles: &[ProfileConfig],
    name: &str,
    broadcast_requested: bool,
) -> Result<(ProfileConfig, ChainRpcProfile), ProfileError> {
    let config = profiles.iter().find(|p| p.name == name).ok_or_else(|| {
        ProfileError::Unknown(
            name.to_string(),
            profiles
                .iter()
                .map(|p| p.name.clone())
                .collect::<Vec<_>>()
                .join(", "),
        )
    })?;
    if is_mainnet(config) {
        return Err(ProfileError::MainnetDisabled(config.name.clone()));
    }
    let chain = ChainRpcProfile {
        name: config.name.clone(),
        family: config.family.clone(),
        expected_genesis_hex: config.expected_genesis_hex.clone(),
        allowed_read_methods: config.allowed_read_methods.clone(),
        allow_broadcast: config.allow_broadcast && broadcast_requested,
        max_response_bytes: config.max_response_bytes,
    };
    Ok((config.clone(), chain))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_profiles(dir: &Path, profiles: &[ProfileConfig]) {
        let file = ProfilesFile {
            schema: PROFILES_SCHEMA.to_string(),
            profiles: profiles.to_vec(),
        };
        std::fs::write(
            dir.join("chain-profiles.json"),
            serde_json::to_vec(&file).unwrap(),
        )
        .unwrap();
    }

    fn devnet() -> ProfileConfig {
        ProfileConfig {
            name: "solana-devnet".into(),
            family: "solana".into(),
            expected_genesis_hex: "ab".repeat(32),
            http_endpoint: "https://api.devnet.solana.com".into(),
            allowed_read_methods: vec!["getGenesisHash".into()],
            allow_broadcast: false,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
        }
    }

    #[test]
    fn missing_file_is_an_empty_set_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_profiles(dir.path()).unwrap(), Vec::new());
    }

    #[test]
    fn configured_profiles_load_and_resolve() {
        let dir = tempfile::tempdir().unwrap();
        write_profiles(dir.path(), &[devnet()]);
        let profiles = load_profiles(dir.path()).unwrap();
        let (config, chain) = resolve(&profiles, "solana-devnet", false).unwrap();
        assert_eq!(config.family, "solana");
        assert_eq!(chain.name, "solana-devnet");
        assert_eq!(chain.family, "solana");
    }

    #[test]
    fn mainnet_by_name_or_family_is_rejected_at_load() {
        let dir = tempfile::tempdir().unwrap();
        let mut mainnet = devnet();
        mainnet.name = "solana-mainnet-beta".into();
        write_profiles(dir.path(), &[mainnet]);
        assert!(matches!(
            load_profiles(dir.path()),
            Err(ProfileError::MainnetDisabled(_))
        ));
    }

    #[test]
    fn broadcast_requires_both_profile_and_caller_request() {
        let dir = tempfile::tempdir().unwrap();
        let mut localnet = devnet();
        localnet.name = "solana-localnet".into();
        localnet.allow_broadcast = true;
        write_profiles(dir.path(), &[devnet(), localnet]);
        let profiles = load_profiles(dir.path()).unwrap();

        let (_, off) = resolve(&profiles, "solana-devnet", true).unwrap();
        assert!(!off.allow_broadcast, "devnet profile has broadcast off");

        let (_, local_on) = resolve(&profiles, "solana-localnet", true).unwrap();
        assert!(local_on.allow_broadcast);

        let (_, local_off) = resolve(&profiles, "solana-localnet", false).unwrap();
        assert!(!local_off.allow_broadcast, "no caller request, no broadcast");
    }

    #[test]
    fn unknown_profile_lists_the_configured_set() {
        let dir = tempfile::tempdir().unwrap();
        write_profiles(dir.path(), &[devnet()]);
        let profiles = load_profiles(dir.path()).unwrap();
        let err = resolve(&profiles, "westnet", false).unwrap_err();
        assert!(err.to_string().contains("solana-devnet"));
    }

    #[test]
    fn wrong_schema_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("chain-profiles.json"),
            serde_json::to_vec(&serde_json::json!({"schema": "wrong", "profiles": []})).unwrap(),
        )
        .unwrap();
        assert!(matches!(
            load_profiles(dir.path()),
            Err(ProfileError::BadSchema(_))
        ));
    }
}
