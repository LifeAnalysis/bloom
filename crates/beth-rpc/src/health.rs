//! Endpoint health observability — stub for now.
//!
//! WP-3 owns the live implementation: scoring, cooldowns, the active
//! probe loop. This module exists in the WP-2 cut so the public type
//! `EndpointHealthSnapshot` is stable and can be referenced from
//! `RpcEngine` without churn when the real probe lands.

use std::time::Duration;

/// A point-in-time view of one endpoint's health, suitable for the VFS
/// `status/chains/<n>/endpoints/<idx>/*` leaves the next WP will add.
///
/// All fields are `Option`/`f64` / `Duration`-shaped so a freshly-built
/// engine that has not yet observed any traffic can still serialise
/// something consistent. WP-3 fills in the real values; today every
/// `RpcEngine::endpoints_snapshot` returns an empty `Vec`.
#[derive(Debug, Clone, PartialEq)]
pub struct EndpointHealthSnapshot {
    /// The configured URL.
    pub url: String,
    /// Observed success rate over the rolling sample window. `None`
    /// before any sample is recorded.
    pub success_rate: Option<f64>,
    /// EWMA latency. `None` before any sample is recorded.
    pub avg_latency: Option<Duration>,
    /// Cooldown end timestamp (ms since UNIX epoch) if the endpoint is
    /// currently parked.
    pub cooldown_until_ms: Option<u64>,
}
