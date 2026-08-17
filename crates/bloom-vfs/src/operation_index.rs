//! Machine-owned identity index for intended operations.
//!
//! This store has no approval, challenge, grant, credential, or signing
//! semantics. It only preserves the idempotent mapping from a venue-local
//! operation identity to Bloom's central action ID.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use fs2::FileExt as _;
use serde::{Deserialize, Serialize};

const SCHEMA: &str = "bloom.machine_operation_index.v1";
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub trait OperationIndex: Send + Sync {
    fn allocate(
        &self,
        surface: &str,
        venue_local_id: &str,
        wallet: &str,
        created_ms: u64,
    ) -> Result<String, String>;
}

#[derive(Debug, Clone)]
pub struct FileOperationIndex {
    path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredIndex {
    schema: String,
    operations: BTreeMap<String, OperationRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OperationRecord {
    surface: String,
    venue_local_id: String,
    action_id: String,
    wallet: String,
    created_ms: u64,
}

impl FileOperationIndex {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn load(&self) -> Result<StoredIndex, String> {
        match fs::read(&self.path) {
            Ok(bytes) => {
                let index: StoredIndex = serde_json::from_slice(&bytes)
                    .map_err(|error| format!("read Machine operation index: {error}"))?;
                index.validate()?;
                Ok(index)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(StoredIndex {
                schema: SCHEMA.to_owned(),
                operations: BTreeMap::new(),
            }),
            Err(error) => Err(format!("read Machine operation index: {error}")),
        }
    }

    fn save(&self, index: &StoredIndex) -> Result<(), String> {
        index.validate()?;
        let parent = self
            .path
            .parent()
            .ok_or_else(|| "Machine operation index path has no parent".to_owned())?;
        fs::create_dir_all(parent)
            .map_err(|error| format!("create Machine operation index directory: {error}"))?;
        let name = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| "Machine operation index filename is invalid".to_owned())?;
        let temporary = parent.join(format!(
            ".{name}.{}.{}.tmp",
            std::process::id(),
            TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let bytes = serde_json::to_vec(index)
            .map_err(|error| format!("encode Machine operation index: {error}"))?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| format!("create Machine operation index update: {error}"))?;
        let result = file
            .write_all(&bytes)
            .and_then(|()| file.sync_all())
            .and_then(|()| fs::rename(&temporary, &self.path));
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result.map_err(|error| format!("commit Machine operation index: {error}"))
    }
}

impl OperationIndex for FileOperationIndex {
    fn allocate(
        &self,
        surface: &str,
        venue_local_id: &str,
        wallet: &str,
        created_ms: u64,
    ) -> Result<String, String> {
        validate_component("surface", surface, true)?;
        validate_component("venue-local ID", venue_local_id, false)?;
        validate_component("wallet", wallet, false)?;
        let parent = self
            .path
            .parent()
            .ok_or_else(|| "Machine operation index path has no parent".to_owned())?;
        fs::create_dir_all(parent)
            .map_err(|error| format!("create Machine operation index directory: {error}"))?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(parent.join("operation-index.lock"))
            .map_err(|error| format!("open Machine operation index lock: {error}"))?;
        lock.lock_exclusive()
            .map_err(|error| format!("lock Machine operation index: {error}"))?;
        let result = (|| {
            let mut index = self.load()?;
            let key = mapping_key(surface, venue_local_id);
            if let Some(existing) = index.operations.get(&key) {
                if existing.wallet != wallet {
                    return Err(format!(
                        "operation identity {surface}/{venue_local_id} is already bound to wallet {}",
                        existing.wallet
                    ));
                }
                return Ok(existing.action_id.clone());
            }
            let action_id = derive_action_id(surface, venue_local_id);
            if index
                .operations
                .values()
                .any(|record| record.action_id == action_id)
            {
                return Err(format!(
                    "action ID collision for {surface}/{venue_local_id}"
                ));
            }
            index.operations.insert(
                key,
                OperationRecord {
                    surface: surface.to_owned(),
                    venue_local_id: venue_local_id.to_owned(),
                    action_id: action_id.clone(),
                    wallet: wallet.to_owned(),
                    created_ms,
                },
            );
            self.save(&index)?;
            Ok(action_id)
        })();
        let _ = lock.unlock();
        result
    }
}

impl StoredIndex {
    fn validate(&self) -> Result<(), String> {
        if self.schema != SCHEMA {
            return Err("Machine operation index schema is unsupported".to_owned());
        }
        let mut action_ids = std::collections::BTreeSet::new();
        for (key, record) in &self.operations {
            validate_component("surface", &record.surface, true)?;
            validate_component("venue-local ID", &record.venue_local_id, false)?;
            validate_component("wallet", &record.wallet, false)?;
            if key != &mapping_key(&record.surface, &record.venue_local_id)
                || record.action_id != derive_action_id(&record.surface, &record.venue_local_id)
                || !action_ids.insert(record.action_id.clone())
            {
                return Err("Machine operation index contains an invalid identity binding".into());
            }
        }
        Ok(())
    }
}

fn mapping_key(surface: &str, venue_local_id: &str) -> String {
    format!("{surface}\u{1f}{venue_local_id}")
}

fn derive_action_id(surface: &str, venue_local_id: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"bloom.action_id.v1");
    hasher.update(surface.as_bytes());
    hasher.update(&[0x1f]);
    hasher.update(venue_local_id.as_bytes());
    format!("{surface}-{}", &hasher.finalize().to_hex()[..32])
}

fn validate_component(label: &str, value: &str, safe_segment: bool) -> Result<(), String> {
    if value.is_empty() || value.len() > 512 || value.chars().any(char::is_control) {
        return Err(format!("invalid {label}"));
    }
    if safe_segment
        && !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(format!("invalid {label}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocation_is_idempotent_durable_and_wallet_bound() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("operations.json");
        let first = FileOperationIndex::new(&path);
        let action = first.allocate("evm", "tx-1", "alice", 10).unwrap();
        assert_eq!(first.allocate("evm", "tx-1", "alice", 20).unwrap(), action);

        let restarted = FileOperationIndex::new(&path);
        assert_eq!(
            restarted.allocate("evm", "tx-1", "alice", 30).unwrap(),
            action
        );
        assert!(restarted.allocate("evm", "tx-1", "bob", 30).is_err());
        let bytes = fs::read(&path).unwrap();
        for forbidden in ["approval", "challenge", "grant", "credential", "secret"] {
            assert!(!String::from_utf8_lossy(&bytes).contains(forbidden));
        }
    }

    #[test]
    fn altered_index_fails_closed() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("operations.json");
        let index = FileOperationIndex::new(&path);
        index.allocate("requests", "req-1", "alice", 10).unwrap();
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        value["operations"]["requests\u{1f}req-1"]["action_id"] = "requests-tampered".into();
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(index.allocate("requests", "req-1", "alice", 10).is_err());
    }
}
