//! The PetalHost bridge: the Petal's `bloom:chain/read` calls land on the
//! Machine's [`Mediator`], so every driver RPC is profile-mediated,
//! allowlisted, genesis-bound, and audited. Every other host authority is
//! denied — the driver Petal declares no other capability.

use async_trait::async_trait;
use bloom_chain_rpc::mediator::Mediator;
use bloom_petals::{ChainRequest, ChainResponse, DenyHost, HostError, HostVfsEntry, PetalHost};
use std::sync::Arc;

pub struct MediatorHost {
    mediator: Arc<Mediator>,
    deny: DenyHost,
    clock_ms: Box<dyn Fn() -> u64 + Send + Sync>,
}

impl MediatorHost {
    pub fn new(
        mediator: Arc<Mediator>,
        clock_ms: impl Fn() -> u64 + Send + Sync + 'static,
    ) -> Self {
        Self {
            mediator,
            deny: DenyHost,
            clock_ms: Box::new(clock_ms),
        }
    }
}

#[async_trait]
impl PetalHost for MediatorHost {
    async fn vfs_lookup(&self, path: &str) -> Result<HostVfsEntry, HostError> {
        self.deny.vfs_lookup(path).await
    }
    async fn vfs_read(&self, path: &str) -> Result<Vec<u8>, HostError> {
        self.deny.vfs_read(path).await
    }
    async fn vfs_list(&self, path: &str) -> Result<Vec<HostVfsEntry>, HostError> {
        self.deny.vfs_list(path).await
    }
    async fn vfs_write(&self, path: &str, bytes: &[u8]) -> Result<(), HostError> {
        self.deny.vfs_write(path, bytes).await
    }

    async fn chain_read(&self, req: ChainRequest) -> Result<ChainResponse, HostError> {
        // Trusted route provenance must have been injected by the runner;
        // a component cannot originate a mediated read without it.
        if req.context.is_none() {
            return Err(HostError::Denied(
                "chain_read requires trusted route provenance".into(),
            ));
        }
        let params: serde_json::Value =
            serde_json::from_str(&req.params_json).unwrap_or(serde_json::Value::Null);
        let at = (self.clock_ms)();
        self.mediator
            .read(at, &req.method, &params)
            .map(|result| ChainResponse {
                result_json: result.to_string(),
            })
            .map_err(|e| HostError::Denied(format!("mediated chain read: {e}")))
    }
}
