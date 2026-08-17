//! Operator-configured cluster profiles.
//!
//! Only configured profiles are accepted — neither the Petal nor CLI input
//! may supply an RPC endpoint. Built-in devnet/localnet profiles exist for
//! the devnet-first UX; additional profiles come from
//! `<state>/profiles.json`. Mainnet is refused outright in this release
//! posture: no built-in profile, and file-based mainnet profiles are
//! rejected at load.

use std::path::Path;

use bloom_chain_rpc::mediator::{ChainRpcProfile, DEFAULT_MAX_RESPONSE_BYTES};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Devnet genesis hash (base58) published by Anza; pinned here so a
/// misconfigured endpoint pointing elsewhere fails the mediator's binding.
pub const DEVNET_GENESIS: &str = "4S2Gga9vVKnB3EwKgpNz2GuppUcLDFup9pAiV5WVZK6k";
pub const LOCALNET_GENESIS: &str = "BloomLocalValidatorGenesis1111111111111111111111111";
pub const DEVNET_HTTP: &str = "https://api.devnet.solana.com";
pub const LOCALNET_HTTP: &str = "http://127.0.0.1:8899";

#[derive(Debug, Error)]
pub enum ProfileError {
    #[error("unknown cluster profile '{0}' (configured: {1})")]
    Unknown(String, String),
    #[error("mainnet profiles are disabled in this release posture")]
    MainnetDisabled,
    #[error("profile file: {0}")]
    File(String),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileConfig {
    pub name: String,
    pub family: String,
    pub expected_genesis_hex: String,
    pub http_endpoint: String,
    /// Broadcast stays off unless the operator explicitly enables it in the
    /// profile file AND the command is a broadcast command with its own
    /// release flag.
    #[serde(default)]
    pub allow_broadcast: bool,
    #[serde(default)]
    pub max_fee_lamports: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfilesFile {
    pub schema: String,
    #[serde(default)]
    pub profiles: Vec<ProfileConfig>,
}

pub const PROFILES_SCHEMA: &str = "bloom.solana.profiles/1";

/// Built-in devnet profile: read/stage/simulation only. Broadcast requires
/// the profile's `allow_broadcast` AND the command-level release flag.
pub fn builtin_profiles() -> Vec<ProfileConfig> {
    vec![
        ProfileConfig {
            name: "devnet".into(),
            family: "solana".into(),
            expected_genesis_hex: DEVNET_GENESIS.into(),
            http_endpoint: DEVNET_HTTP.into(),
            allow_broadcast: false,
            max_fee_lamports: Some(100_000),
        },
        ProfileConfig {
            name: "localnet".into(),
            family: "solana".into(),
            expected_genesis_hex: LOCALNET_GENESIS.into(),
            http_endpoint: LOCALNET_HTTP.into(),
            allow_broadcast: true,
            max_fee_lamports: Some(100_000),
        },
    ]
}

/// Load the effective profile set: built-ins plus any operator file.
/// Mainnet family/profiles are refused at load time.
pub fn load_profiles(state_root: &Path) -> Result<Vec<ProfileConfig>, ProfileError> {
    let mut profiles = builtin_profiles();
    let path = state_root.join("profiles.json");
    if path.exists() {
        let file: ProfilesFile = serde_json::from_slice(&std::fs::read(&path)?)?;
        if file.schema != PROFILES_SCHEMA {
            return Err(ProfileError::File(format!(
                "schema {} != {PROFILES_SCHEMA}",
                file.schema
            )));
        }
        for profile in file.profiles {
            if is_mainnet(&profile) {
                return Err(ProfileError::MainnetDisabled);
            }
            profiles.retain(|p| p.name != profile.name);
            profiles.push(profile);
        }
    }
    Ok(profiles)
}

fn is_mainnet(profile: &ProfileConfig) -> bool {
    let name = profile.name.to_lowercase();
    let family = profile.family.to_lowercase();
    name.contains("mainnet") || family.contains("mainnet")
}

/// Resolve a profile by name into the mediator's chain profile.
pub fn resolve(
    state_root: &Path,
    name: &str,
    broadcast_requested: bool,
) -> Result<(ProfileConfig, ChainRpcProfile), ProfileError> {
    let profiles = load_profiles(state_root)?;
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
        return Err(ProfileError::MainnetDisabled);
    }
    // Broadcast capability is the AND of the profile config and the
    // command-level release flag; never implicit.
    let allow_broadcast = config.allow_broadcast && broadcast_requested;
    let chain = ChainRpcProfile {
        name: format!("solana-{}", config.name),
        family: "solana".into(),
        expected_genesis_hex: config.expected_genesis_hex.clone(),
        allowed_read_methods: vec![
            "getGenesisHash".into(),
            "getHealth".into(),
            "getBlockHeight".into(),
            "getLatestBlockhash".into(),
            "getFeeForMessage".into(),
            "isBlockhashValid".into(),
            "getSignatureStatuses".into(),
            "getBalance".into(),
        ],
        allow_broadcast,
        max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
    };
    Ok((config.clone(), chain))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_never_include_mainnet() {
        assert!(!builtin_profiles().iter().any(is_mainnet));
    }

    #[test]
    fn file_profiles_reject_mainnet() {
        let dir = tempfile::tempdir().unwrap();
        let file = ProfilesFile {
            schema: PROFILES_SCHEMA.into(),
            profiles: vec![ProfileConfig {
                name: "mainnet-beta".into(),
                family: "solana".into(),
                expected_genesis_hex: "ff".repeat(21),
                http_endpoint: "https://api.mainnet-beta.solana.com".into(),
                allow_broadcast: true,
                max_fee_lamports: None,
            }],
        };
        std::fs::write(
            dir.path().join("profiles.json"),
            serde_json::to_vec(&file).unwrap(),
        )
        .unwrap();
        assert!(matches!(
            load_profiles(dir.path()),
            Err(ProfileError::MainnetDisabled)
        ));
    }

    #[test]
    fn broadcast_requires_both_profile_and_command_flag() {
        let dir = tempfile::tempdir().unwrap();
        let (_, off) = resolve(dir.path(), "devnet", true).unwrap();
        assert!(!off.allow_broadcast, "devnet profile has broadcast off");
        let (_, local_on) = resolve(dir.path(), "localnet", true).unwrap();
        assert!(local_on.allow_broadcast);
        let (_, local_off) = resolve(dir.path(), "localnet", false).unwrap();
        assert!(!local_off.allow_broadcast, "no command flag, no broadcast");
    }

    #[test]
    fn unknown_profile_names_list_the_configured_set() {
        let dir = tempfile::tempdir().unwrap();
        let err = resolve(dir.path(), "westnet", false).unwrap_err();
        assert!(err.to_string().contains("devnet"));
        assert!(err.to_string().contains("localnet"));
    }
}
