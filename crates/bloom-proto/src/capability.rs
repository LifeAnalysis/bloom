//! Capability model: a read-only projection for non-authoritative service
//! capabilities.
//!
//! [`CapabilityViewEntry`] is the serialisable snapshot each venue handler
//! projects into, rendered at `/wallets/<w>/capabilities/active.json`.
//!
//! `signing_model` is **load-bearing security truth** — agents and humans
//! must know whether the owner key is still in the loop for every action
//! inside this capability.

use serde::Serialize;

/// Which venue a capability governs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Venue {
    /// DeFi intent routes via Enso shortcuts.
    Defi,
}

/// Who signs actions authorised by this capability.
///
/// This is **not** an implementation detail. It tells the agent (and the
/// human at review time) whether the owner key is still resident and needed
/// for every action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SigningModel {
    /// The capability is a service credential only (HMAC / API key). It never
    /// moves funds — the owner must still sign value-moving operations
    /// separately (e.g. Enso API keys).
    ServiceAuthOnly,
}

/// Lifecycle status of a capability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityStatus {
    /// Active and accepting actions.
    Active,
    /// Time-bound expiry reached; no new actions accepted.
    Expired,
    /// Risk breach or breach-action trigger; trading halted.
    Halted,
    /// Explicitly revoked by owner or agent.
    Revoked,
    /// The most recent external status read failed, so reported values are
    /// last-known.
    Stale,
}

/// A serializable snapshot of a capability — what gets rendered in
/// `/wallets/<w>/capabilities/active.json`.  Each venue handler constructs
/// this directly from its native store.
#[derive(Debug, Clone, Serialize)]
pub struct CapabilityViewEntry {
    pub id: String,
    pub wallet: String,
    pub venue: Venue,
    pub signing_model: SigningModel,
    pub created_ms: u128,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_ms: Option<u128>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_in_secs: Option<u64>,
    pub status: CapabilityStatus,
    /// Venue-structured limits blob (max_order_usd, max_position_usd, etc.).
    pub limits: serde_json::Value,
    pub next_write_path: String,
    pub revoke_path: String,
    pub audit_ref: String,
    pub review_ref: String,
    /// Prose summary of what is allowed.
    pub allowed: Vec<String>,
    /// Prose summary of what is explicitly excluded.
    pub denied: Vec<String>,
}

pub fn now_ms_u128() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}
