//! Fail-closed compatibility mount for the removed native Hyperliquid surface.
//!
//! Hyperliquid signing now belongs in a Petal using generic Petal-scoped
//! Signer keys. Keeping this tiny projection lets existing installations emit
//! a useful migration diagnostic without compiling the legacy agent signer.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use bloom_hyperliquid::HyperliquidClient;
use bloom_proto::CapabilityViewEntry;

use crate::handler::{Entry, Handler, HandlerError};
use crate::path::VfsPath;

const README: &str = "# Hyperliquid native surface removed\n\nThe native Bloom Hyperliquid agent and signing surface has been removed. Install and use the Hyperliquid Petal; Petal sub-keys are held and scoped by Bloom Signer.\n";

#[derive(Clone, Default)]
pub struct HyperliquidHandler;

impl HyperliquidHandler {
    pub fn new(_mainnet: HyperliquidClient, _testnet: HyperliquidClient) -> Self {
        Self
    }

    pub fn with_store_root(self, _root: PathBuf) -> Self {
        self
    }

    pub fn start_monitoring(self: Arc<Self>) {}

    pub fn capability_views_for(&self, _wallet: &str) -> Vec<CapabilityViewEntry> {
        Vec::new()
    }
}

#[async_trait]
impl Handler for HyperliquidHandler {
    async fn lookup(&self, path: &VfsPath) -> Result<Entry, HandlerError> {
        match path.segments() {
            [] => Ok(Entry::dir("")),
            [name] if name == "README.md" => Ok(Entry::read_only_file("README.md")),
            _ => Err(HandlerError::NotFound(path.to_string_path())),
        }
    }

    async fn read(&self, path: &VfsPath) -> Result<Vec<u8>, HandlerError> {
        match path.segments() {
            [name] if name == "README.md" => Ok(README.as_bytes().to_vec()),
            _ => Err(HandlerError::NotFound(path.to_string_path())),
        }
    }

    async fn write(&self, _path: &VfsPath, _data: &[u8]) -> Result<(), HandlerError> {
        Err(HandlerError::Unsupported(
            "native Hyperliquid writes were removed; use the Hyperliquid Petal".into(),
        ))
    }

    async fn prepare_write_open(&self, _path: &VfsPath) -> Result<(), HandlerError> {
        Err(HandlerError::Unsupported(
            "native Hyperliquid writes were removed; use the Hyperliquid Petal".into(),
        ))
    }

    async fn list(&self, path: &VfsPath) -> Result<Vec<Entry>, HandlerError> {
        if path.is_root() {
            Ok(vec![Entry::read_only_file("README.md")])
        } else {
            Err(HandlerError::NotADir(path.to_string_path()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn native_writes_fail_closed_with_migration_diagnostic() {
        let handler = HyperliquidHandler;
        let error = handler
            .write(
                &VfsPath::parse("/mainnet/agent_sessions/wallet/new.json").unwrap(),
                b"{}",
            )
            .await
            .unwrap_err();
        assert!(
            matches!(error, HandlerError::Unsupported(message) if message.contains("Hyperliquid Petal"))
        );
    }
}
