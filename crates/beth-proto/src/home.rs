//! On-disk layout under `~/.bloom-eth/`.

use std::path::{Path, PathBuf};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum HomeError {
    #[error("could not determine home directory; set BETH_HOME or --home")]
    NoHome,
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Resolves and creates the bloom-eth home directory tree.
///
/// Layout:
/// ```text
/// ~/.bloom-eth/
/// ├── config.toml
/// ├── audit.jsonl
/// ├── beth.sock           # admin socket
/// ├── events.sock         # streaming events socket
/// ├── keystore/
/// │   └── <wallet>/
/// │       ├── address
/// │       ├── pubkey
/// │       ├── kind
/// │       ├── encrypted.key
/// │       └── policy.toml
/// ├── cache/
/// │   └── cache.db
/// ├── blobs/
/// ├── outbox/             # daemon's persisted outbox state
/// │   └── <wallet>/<chain>/{pending,sent,failed}/<id>/...
/// ├── watch/
/// └── logs/
/// ```
#[derive(Debug, Clone)]
pub struct HomeDir {
    root: PathBuf,
}

impl HomeDir {
    /// Resolve `raw` (`~/.bloom-eth` style) into a concrete path.
    pub fn resolve(raw: &str) -> Result<Self, HomeError> {
        let root = if raw == "~/.bloom-eth" {
            let home = dirs::home_dir().ok_or(HomeError::NoHome)?;
            home.join(".bloom-eth")
        } else {
            PathBuf::from(shellexpand_local(raw))
        };
        Ok(Self { root })
    }

    /// Use a pre-resolved path (mostly for tests).
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Create the on-disk subdirectories. Idempotent.
    pub fn ensure(&self) -> Result<(), HomeError> {
        let dirs = [
            self.root.clone(),
            self.keystore_dir(),
            self.cache_dir(),
            self.blobs_dir(),
            self.outbox_dir(),
            self.watch_dir(),
            self.logs_dir(),
        ];
        for d in dirs {
            std::fs::create_dir_all(&d).map_err(|source| HomeError::Io {
                path: d.clone(),
                source,
            })?;
        }
        Ok(())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn config_path(&self) -> PathBuf {
        self.root.join("config.toml")
    }
    pub fn audit_path(&self) -> PathBuf {
        self.root.join("audit.jsonl")
    }
    pub fn admin_socket(&self) -> PathBuf {
        self.root.join("beth.sock")
    }
    pub fn events_socket(&self) -> PathBuf {
        self.root.join("events.sock")
    }
    pub fn keystore_dir(&self) -> PathBuf {
        self.root.join("keystore")
    }
    pub fn wallet_dir(&self, name: &str) -> PathBuf {
        self.keystore_dir().join(name)
    }
    pub fn cache_dir(&self) -> PathBuf {
        self.root.join("cache")
    }
    pub fn blobs_dir(&self) -> PathBuf {
        self.root.join("blobs")
    }
    pub fn outbox_dir(&self) -> PathBuf {
        self.root.join("outbox")
    }
    pub fn watch_dir(&self) -> PathBuf {
        self.root.join("watch")
    }
    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }
}

fn shellexpand_local(raw: &str) -> String {
    if let Some(stripped) = raw.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(stripped).to_string_lossy().into_owned();
        }
    }
    raw.to_string()
}
