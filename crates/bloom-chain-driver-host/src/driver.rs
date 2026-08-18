//! One configured chain profile's mediator + durable outbox, and the
//! registry of them a daemon dispatches Petal `chain_read` calls through.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use bloom_chain_action::ChainActionOutbox;
use bloom_chain_rpc::mediator::Mediator;

use crate::profiles::{ProfileConfig, ProfileError, resolve};

/// One configured chain-driver profile, wired: mediated RPC + its own
/// durable outbox. Everything here is generic — chain identity lives
/// entirely in the wrapped [`Mediator`]'s [`bloom_chain_rpc::mediator::ChainRpcProfile`].
pub struct ChainDriverHost {
    mediator: Arc<Mediator>,
    outbox: Arc<ChainActionOutbox>,
}

impl ChainDriverHost {
    pub fn new(mediator: Arc<Mediator>, outbox: Arc<ChainActionOutbox>) -> Self {
        Self { mediator, outbox }
    }

    pub fn mediator(&self) -> &Arc<Mediator> {
        &self.mediator
    }

    pub fn outbox(&self) -> &Arc<ChainActionOutbox> {
        &self.outbox
    }

    /// The dispatch key `daemon_petal_chain_read`-style routing checks
    /// before ever calling into this host (e.g. `"solana"`).
    pub fn family(&self) -> &str {
        &self.mediator.profile().family
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("profile: {0}")]
    Profile(#[from] ProfileError),
    #[error("mediator construction for '{0}' failed: {1}")]
    Mediator(String, String),
    #[error("outbox construction for '{0}' failed: {1}")]
    Outbox(String, #[source] bloom_chain_action::OutboxError),
}

/// Registry of configured chain-driver hosts, keyed by profile name.
/// Cheap to clone: the inner map is shared.
#[derive(Clone, Default)]
pub struct ChainDriverRegistry(Arc<HashMap<String, Arc<ChainDriverHost>>>);

impl ChainDriverRegistry {
    pub fn get(&self, profile_name: &str) -> Option<Arc<ChainDriverHost>> {
        self.0.get(profile_name).cloned()
    }

    pub fn profile_names(&self) -> Vec<String> {
        self.0.keys().cloned().collect()
    }
}

/// Build a registry from configured profiles, one `Mediator` (with the
/// caller-supplied transport) and one on-disk outbox (under
/// `<outbox_root>/<profile name>`) per profile. Broadcast capability on each
/// mediator is exactly the profile's own configured flag — a further
/// per-request release gate, if wanted, is the caller's responsibility
/// above this registry.
pub fn build_registry(
    profiles: &[ProfileConfig],
    outbox_root: &Path,
    mut transport_for: impl FnMut(&ProfileConfig) -> Result<Box<dyn bloom_chain_rpc::transport::RpcTransport>, String>,
) -> Result<ChainDriverRegistry, BuildError> {
    let mut hosts = HashMap::with_capacity(profiles.len());
    for config in profiles {
        let (config, chain_profile) = resolve(profiles, &config.name, config.allow_broadcast)?;
        let transport = transport_for(&config)
            .map_err(|e| BuildError::Mediator(config.name.clone(), e))?;
        let mediator = Mediator::new(chain_profile, vec![transport])
            .map_err(|e| BuildError::Mediator(config.name.clone(), e.to_string()))?;
        let outbox = ChainActionOutbox::new(outbox_root.join(&config.name))
            .map_err(|e| BuildError::Outbox(config.name.clone(), e))?;
        hosts.insert(
            config.name.clone(),
            Arc::new(ChainDriverHost::new(Arc::new(mediator), Arc::new(outbox))),
        );
    }
    Ok(ChainDriverRegistry(Arc::new(hosts)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bloom_chain_rpc::transport::{RpcError, RpcTransport};
    use serde_json::Value;

    struct StubTransport;
    impl RpcTransport for StubTransport {
        fn name(&self) -> &str {
            "stub"
        }
        fn call(&self, _method: &str, _params: &Value) -> Result<Value, RpcError> {
            Ok(Value::Null)
        }
    }

    fn profile(name: &str, family: &str) -> ProfileConfig {
        ProfileConfig {
            name: name.to_string(),
            family: family.to_string(),
            expected_genesis_hex: "ab".repeat(32),
            http_endpoint: "http://127.0.0.1:1".into(),
            allowed_read_methods: vec!["getGenesisHash".into()],
            allow_broadcast: false,
            max_response_bytes: 65536,
        }
    }

    #[test]
    fn builds_one_host_per_profile_dispatchable_by_family() {
        let dir = tempfile::tempdir().unwrap();
        let profiles = vec![profile("solana-devnet", "solana"), profile("test-evm", "evm")];
        let registry =
            build_registry(&profiles, dir.path(), |_| Ok(Box::new(StubTransport))).unwrap();

        assert_eq!(registry.get("solana-devnet").unwrap().family(), "solana");
        assert_eq!(registry.get("test-evm").unwrap().family(), "evm");
        assert!(registry.get("nonexistent").is_none());
        let mut names = registry.profile_names();
        names.sort();
        assert_eq!(names, vec!["solana-devnet".to_string(), "test-evm".to_string()]);
    }

    #[test]
    fn each_host_gets_its_own_outbox_directory() {
        let dir = tempfile::tempdir().unwrap();
        let profiles = vec![profile("a", "solana"), profile("b", "solana")];
        let registry =
            build_registry(&profiles, dir.path(), |_| Ok(Box::new(StubTransport))).unwrap();
        assert!(dir.path().join("a").exists());
        assert!(dir.path().join("b").exists());
        // Independent outboxes: staging in one never touches the other.
        assert!(registry.get("a").unwrap().outbox().list().unwrap().is_empty());
    }
}
