//! Adapter wiring `beth_ens::EnsClient` into the `RecipientResolver` trait
//! that `beth_tx::TxEngine` consumes. Lives in the daemon crate (not beth-tx)
//! to avoid pulling beth-ens into beth-tx and creating a dep cycle.

use alloy::primitives::Address;
use async_trait::async_trait;
use beth_ens::EnsClient;
use beth_tx::tx_engine::RecipientResolver;

pub struct EnsAdapter {
    client: EnsClient,
}

impl EnsAdapter {
    pub fn new(client: EnsClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl RecipientResolver for EnsAdapter {
    async fn resolve_name(&self, name: &str) -> Result<Address, String> {
        self.client.resolve(name).await.map_err(|e| e.to_string())
    }
}
