//! Builds the layered alloy transport stack from a `ChainSpec`.
//!
//! Stack per endpoint:
//!
//! ```text
//! [RetryBackoffLayer with BethRetryPolicy]
//!  ↓
//! [ThrottleLayer (only if endpoint.max_rps is Some)]
//!  ↓
//! [Http transport]
//! ```
//!
//! The fallback fan-out wraps the per-endpoint stacks in a single
//! `FallbackLayer` with `active_transport_count = min(2, endpoints.len())`
//! so two endpoints race for each call. A two-endpoint chain therefore
//! always queries both in parallel; bigger lists rotate the top two by
//! score.
//!
//! WS/WSS endpoints are recognised but not yet attached to the
//! transport stack — `WsConnect` requires an async open and
//! `RpcEngine::build` is intentionally sync (so `ChainClient::new`
//! stays sync). WP-4 plumbs in the async WS hand-off; until then, ws
//! endpoints contribute to `supports_subscriptions()` reporting only.

use std::num::NonZeroUsize;
use std::sync::Arc;

use alloy::network::Ethereum;
use alloy::providers::RootProvider;
use alloy::rpc::client::RpcClient;
use alloy::transports::http::{reqwest, Http};
use alloy::transports::layers::{FallbackLayer, RetryBackoffLayer, ThrottleLayer};
use alloy::transports::{BoxTransport, IntoBoxTransport};
use beth_proto::{ChainSpec, EndpointSpec};
use tower::ServiceBuilder;
use tracing::{debug, info, warn};

use crate::endpoint::{classify_endpoint, is_subscription_capable, EndpointScheme};
use crate::error::BethRpcError;
use crate::health::EndpointHealthSnapshot;
use crate::policy::BethRetryPolicy;

/// Default compute-units-per-second when the operator hasn't set
/// `cu_per_sec`. Mirrors alloy's own internal default for the retry
/// layer (Alchemy free-tier 330 CU/s × 2 ≈ 660). The cost is a
/// conservative throttle on retry pacing only — it doesn't reject
/// calls.
const DEFAULT_CU_PER_SEC: u64 = 660;

/// Maximum retries applied by `RetryBackoffLayer`. Three is the value
/// the spec recommends: one for transient network blips, one for a
/// rate-limit cooldown, one for a tail latency outlier. Anything more
/// risks masking a genuinely-broken endpoint.
const MAX_RETRIES: u32 = 3;

/// Initial backoff in milliseconds. The retry layer doubles this on
/// each attempt; 200 ms keeps the worst-case retry chain inside the
/// per-call budget at our scale (200 → 400 → 800).
const INITIAL_BACKOFF_MS: u64 = 200;

/// The layered RPC engine. Built once per `ChainSpec` and shared via
/// `Arc` by every consumer (the wallet, watch executor, ENS resolver,
/// VFS handlers). The `provider()` accessor exposes the alloy
/// `RootProvider<Ethereum>` so existing call sites that depend on the
/// trait surface continue to compile unchanged.
pub struct RpcEngine {
    chain_name: String,
    endpoints: Vec<EndpointSpec>,
    provider: Arc<RootProvider<Ethereum>>,
    supports_subscriptions: bool,
}

impl RpcEngine {
    /// Build the layered transport stack for `spec`.
    ///
    /// Returns `BethRpcError::NoEndpoints` if `spec.endpoints()` is
    /// empty. Each endpoint URL is classified into HTTP or WS;
    /// HTTP-shaped endpoints are wrapped in retry + optional throttle
    /// and joined under a fallback layer. WS-shaped endpoints are
    /// recorded for `supports_subscriptions()` reporting only — the
    /// active probe loop and WS subscription hand-off arrive in WP-3
    /// and WP-4.
    pub fn build(spec: &ChainSpec) -> Result<Self, BethRpcError> {
        let endpoints = spec.endpoints();
        if endpoints.is_empty() {
            return Err(BethRpcError::NoEndpoints(spec.name.clone()));
        }

        let mut http_transports: Vec<BoxTransport> = Vec::new();
        let mut supports_subscriptions = false;

        for ep in &endpoints {
            let (url, scheme) = classify_endpoint(ep)?;
            if is_subscription_capable(ep) {
                supports_subscriptions = true;
            }
            match scheme {
                EndpointScheme::Http => {
                    let transport = build_http_stack(&url, ep);
                    http_transports.push(transport);
                    debug!(
                        chain = %spec.name,
                        url = %redacted(&ep.url),
                        weight = ep.weight,
                        cu_per_sec = ep.cu_per_sec,
                        max_rps = ep.max_rps,
                        "rpc.transport.http_stack_built"
                    );
                }
                EndpointScheme::Ws => {
                    // WP-4 will replace this branch with a real
                    // WS-backed transport. For now we keep the engine
                    // sync-buildable.
                    warn!(
                        chain = %spec.name,
                        url = %redacted(&ep.url),
                        "rpc.transport.ws_endpoint_skipped_until_wp4"
                    );
                }
            }
        }

        if http_transports.is_empty() {
            return Err(BethRpcError::NoEndpoints(spec.name.clone()));
        }

        let active = NonZeroUsize::new(http_transports.len().min(2)).unwrap_or(
            // SAFETY: `http_transports.is_empty()` was rejected above,
            // so `len() >= 1` and `min(2) >= 1`. The unwrap_or branch
            // is unreachable; we still write the fallback to avoid an
            // explicit unwrap that future readers might mis-edit.
            NonZeroUsize::new(1).unwrap(),
        );
        let fallback = FallbackLayer::default().with_active_transport_count(active);

        let service = ServiceBuilder::new().layer(fallback).service(http_transports);
        // Loopback URLs are tagged "local" by alloy's HTTP transport;
        // we don't have visibility into per-endpoint locality at this
        // outer layer, so default to false (the most conservative
        // choice for retry/timeout heuristics).
        let client: RpcClient = RpcClient::new(service, false);
        let provider: RootProvider<Ethereum> = RootProvider::<Ethereum>::new(client);

        info!(
            chain = %spec.name,
            endpoints = endpoints.len(),
            http_endpoints = active.get(),
            supports_subscriptions,
            "rpc.engine.built"
        );

        Ok(Self {
            chain_name: spec.name.clone(),
            endpoints,
            provider: Arc::new(provider),
            supports_subscriptions,
        })
    }

    /// The alloy provider backed by the layered stack. Cloning the
    /// `Arc` is the intended way for downstream crates to share the
    /// engine.
    pub fn provider(&self) -> Arc<RootProvider<Ethereum>> {
        self.provider.clone()
    }

    /// True if any configured endpoint declared a ws/wss URL and was
    /// not flagged `http_only`. WP-4 wires this into the watch
    /// executor's WS fast path.
    pub fn supports_subscriptions(&self) -> bool {
        self.supports_subscriptions
    }

    /// Per-endpoint health view. Stub for WP-2: the probe loop that
    /// fills it lives in WP-3, so today this returns an empty Vec.
    pub fn endpoints_snapshot(&self) -> Vec<EndpointHealthSnapshot> {
        Vec::new()
    }

    /// Chain name this engine was built for. Used by error variants
    /// that surface "all endpoints failed" with operator context.
    pub fn chain_name(&self) -> &str {
        &self.chain_name
    }

    /// Configured endpoints, in source order.
    pub fn endpoints(&self) -> &[EndpointSpec] {
        &self.endpoints
    }
}

/// Build the HTTP transport stack for one endpoint. The result is
/// box-erased so all endpoints in the fallback layer share a single
/// concrete type.
fn build_http_stack(url: &url::Url, ep: &EndpointSpec) -> BoxTransport {
    let cu = ep.cu_per_sec.unwrap_or(DEFAULT_CU_PER_SEC);
    let retry = RetryBackoffLayer::new_with_policy(
        MAX_RETRIES,
        INITIAL_BACKOFF_MS,
        cu,
        BethRetryPolicy::default(),
    );

    // ServiceBuilder applies layers outermost-first when reading
    // top-to-bottom; conceptually retry sits *outside* throttle so
    // throttled-and-rate-limited responses still get retried. For
    // endpoints without a configured `max_rps` we skip the throttle
    // layer entirely (the throttle layer panics on `rps == 0`).
    let http: Http<reqwest::Client> = Http::new(url.clone());
    if let Some(rps) = ep.max_rps {
        let throttle = ThrottleLayer::new(rps);
        let stack = ServiceBuilder::new().layer(retry).layer(throttle).service(http);
        stack.into_box_transport()
    } else {
        let stack = ServiceBuilder::new().layer(retry).service(http);
        stack.into_box_transport()
    }
}

/// Best-effort URL redaction for log lines. Strips the path / query so
/// vendor API keys baked into the path don't end up in stdout.
fn redacted(url: &str) -> String {
    if let Ok(u) = url::Url::parse(url) {
        let host = u.host_str().unwrap_or("?");
        let port = u
            .port()
            .map(|p| format!(":{p}"))
            .unwrap_or_default();
        format!("{}://{}{}", u.scheme(), host, port)
    } else {
        url.to_string()
    }
}
