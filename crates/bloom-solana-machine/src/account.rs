//! Enabled Solana account registry — public projections only.
//!
//! The Machine's account registry records which derived child each wallet
//! has enabled for Solana, on which cluster profile. It stores public key
//! material exclusively (locator, Ed25519 public key, base58 address, CAIP-10
//! identity). Private keys, mnemonics, entropy, and WKEK material never exist
//! in the Machine process at all.
//!
//! File-backed JSON under the machine state root, atomically written,
//! validated on load. Registry entries are append-oriented: enabling the same
//! `(wallet, cluster, public key)` twice is idempotent; enabling a *different*
//! key for the same wallet on the same cluster is refused — one active Solana
//! account per wallet per cluster until the BIP-39 registry lands.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::projection::AccountProjection;

pub const ACCOUNT_REGISTRY_SCHEMA: &str = "bloom.solana.account-registry/1";

#[derive(Debug, Error)]
pub enum AccountError {
    #[error("account for wallet '{0}' on cluster '{1}' already enabled with a different key")]
    KeyConflict(String, String),
    #[error("invalid public key hex: {0}")]
    BadPublicKey(String),
    #[error("account not found for wallet '{0}' on cluster '{1}'")]
    NotFound(String, String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnabledAccount {
    pub wallet_id: String,
    /// Fixture-era identity: opaque key-ref locator. Swapped for the real
    /// derived-account descriptor at the BIP-39 integration checkpoint.
    pub key_ref_locator: String,
    pub public_key_hex: String,
    pub address_base58: String,
    pub caip10: String,
    pub cluster_profile: String,
    pub enabled_at_ms: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RegistryFile {
    schema: String,
    /// (wallet, cluster) -> account.
    accounts: BTreeMap<String, EnabledAccount>,
}

/// The durable account registry.
#[derive(Debug)]
pub struct AccountRegistry {
    path: PathBuf,
    lock: Mutex<RegistryFile>,
}

impl AccountRegistry {
    pub fn open(state_root: &Path) -> Result<Self, AccountError> {
        let path = state_root.join("solana-accounts.json");
        let file = if path.exists() {
            let file: RegistryFile = serde_json::from_slice(&std::fs::read(&path)?)?;
            if file.schema != ACCOUNT_REGISTRY_SCHEMA {
                return Err(AccountError::BadPublicKey(format!(
                    "registry schema {}",
                    file.schema
                )));
            }
            file
        } else {
            RegistryFile {
                schema: ACCOUNT_REGISTRY_SCHEMA.to_string(),
                accounts: BTreeMap::new(),
            }
        };
        Ok(Self {
            path,
            lock: Mutex::new(file),
        })
    }

    fn key(wallet: &str, cluster: &str) -> String {
        format!("{wallet}\u{1f}{cluster}")
    }

    /// Enable (or idempotently re-confirm) a Solana account. The base58
    /// address and CAIP-10 are derived here from the supplied Ed25519 public
    /// key and cluster CAIP-2 — callers never supply them, so the projection
    /// cannot disagree with the key.
    pub fn enable(
        &self,
        wallet_id: &str,
        key_ref_locator: &str,
        public_key_hex: &str,
        cluster_profile: &str,
        cluster_caip2: &str,
        now_ms: u64,
    ) -> Result<EnabledAccount, AccountError> {
        let bytes = hex::decode(public_key_hex)
            .map_err(|e| AccountError::BadPublicKey(format!("{public_key_hex}: {e}")))?;
        let arr: [u8; 32] = bytes.try_into().map_err(|v: Vec<u8>| {
            AccountError::BadPublicKey(format!("need 32 bytes, got {}", v.len()))
        })?;
        let address_base58 = bs58::encode(arr).into_string();
        // CAIP-10: blockchain_id:address per CAIP-10 (namespace-profile
        // truncated reference).
        let caip10 = format!("{cluster_caip2}:{address_base58}");

        let mut file = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let key = Self::key(wallet_id, cluster_profile);
        let account = EnabledAccount {
            wallet_id: wallet_id.to_string(),
            key_ref_locator: key_ref_locator.to_string(),
            public_key_hex: public_key_hex.to_lowercase(),
            address_base58,
            caip10,
            cluster_profile: cluster_profile.to_string(),
            enabled_at_ms: now_ms,
        };
        if let Some(existing) = file.accounts.get(&key) {
            if existing.public_key_hex != account.public_key_hex {
                return Err(AccountError::KeyConflict(
                    wallet_id.to_string(),
                    cluster_profile.to_string(),
                ));
            }
            return Ok(existing.clone());
        }
        file.accounts.insert(key, account.clone());
        self.persist(&file)?;
        Ok(account)
    }

    pub fn get(
        &self,
        wallet_id: &str,
        cluster_profile: &str,
    ) -> Result<EnabledAccount, AccountError> {
        self.lock
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .accounts
            .get(&Self::key(wallet_id, cluster_profile))
            .cloned()
            .ok_or_else(|| {
                AccountError::NotFound(wallet_id.to_string(), cluster_profile.to_string())
            })
    }

    pub fn list(&self) -> Vec<EnabledAccount> {
        self.lock
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .accounts
            .values()
            .cloned()
            .collect()
    }

    /// Public projections for VFS surfaces.
    pub fn projections(&self) -> Vec<AccountProjection> {
        self.list()
            .into_iter()
            .map(|a| AccountProjection {
                schema: "bloom.solana.account-projection/1".to_string(),
                wallet_id: a.wallet_id,
                key_ref_locator: a.key_ref_locator,
                public_key_hex: a.public_key_hex,
                address_base58: a.address_base58,
                caip10: a.caip10,
                cluster_profile: a.cluster_profile,
                enabled_at_ms: a.enabled_at_ms,
            })
            .collect()
    }

    fn persist(&self, file: &RegistryFile) -> Result<(), AccountError> {
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(file)?)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> (tempfile::TempDir, AccountRegistry) {
        let dir = tempfile::tempdir().unwrap();
        let reg = AccountRegistry::open(dir.path()).unwrap();
        (dir, reg)
    }

    const KEY: &str = "03a107bff3ce10be1d70dd18e74bc09967e4d6309ba50d5f1ddc8664125531b8";

    #[test]
    fn enable_is_idempotent_and_derives_identity() {
        let (_d, reg) = registry();
        let a = reg
            .enable("w", "solana-child-0", KEY, "devnet", "solana:devnet", 1)
            .unwrap();
        let b = reg
            .enable("w", "solana-child-0", KEY, "devnet", "solana:devnet", 2)
            .unwrap();
        assert_eq!(a, b);
        assert_eq!(
            a.address_base58,
            "FAe4sisG95oZ42w7buUn5qEE4TAnfTTFPiguZUHmhiF"
        );
        assert_eq!(
            a.caip10,
            "solana:devnet:FAe4sisG95oZ42w7buUn5qEE4TAnfTTFPiguZUHmhiF"
        );
    }

    #[test]
    fn conflicting_key_is_refused() {
        let (_d, reg) = registry();
        reg.enable("w", "k0", KEY, "devnet", "solana:devnet", 1)
            .unwrap();
        assert!(matches!(
            reg.enable("w", "k1", &"11".repeat(32), "devnet", "solana:devnet", 2),
            Err(AccountError::KeyConflict(_, _))
        ));
        // A different cluster accepts a different key.
        reg.enable("w", "k1", &"11".repeat(32), "local", "solana:localnet", 3)
            .unwrap();
    }

    #[test]
    fn registry_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        {
            let reg = AccountRegistry::open(dir.path()).unwrap();
            reg.enable("w", "k0", KEY, "devnet", "solana:devnet", 1)
                .unwrap();
        }
        let reg = AccountRegistry::open(dir.path()).unwrap();
        assert_eq!(reg.list().len(), 1);
        assert_eq!(
            reg.get("w", "devnet").unwrap().address_base58,
            "FAe4sisG95oZ42w7buUn5qEE4TAnfTTFPiguZUHmhiF"
        );
        assert!(reg.get("w", "local").is_err());
    }
}
