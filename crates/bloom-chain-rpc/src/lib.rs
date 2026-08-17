//! Chain-neutral Machine RPC mediation for verified chain Petals.
//!
//! This crate is the in-process core of the plan's `bloom:chain/rpc` host
//! interface: a driver names a configured chain profile and an allowed
//! JSON-RPC method; Machine owns endpoints, genesis-hash checks, method and
//! response-size allowlists, read/broadcast capability separation, and audit.
//! No endpoint credentials, arbitrary URLs, or chain SDKs are involved.
//!
//! Everything is defined against the [`RpcTransport`] trait so the
//! deterministic [`sim::SimChain`] and the scripted [`fault::FaultProxy`]
//! drive the same code paths a real HTTP transport would.
//!
//! The [`freshness`] module implements the honest-runtime pre-sign freshness
//! evaluation (staging age, remaining validity window, observation
//! consistency) and maps to the outbox's first-class freshness refusal
//! states. It detects lagging or inconsistent providers; a consistently
//! malicious RPC requires endpoint quorum or a separate attestor and is out
//! of scope for v1.

#![forbid(unsafe_code)]

pub mod fault;
pub mod freshness;
#[cfg(feature = "http")]
pub mod http;
pub mod mediator;
pub mod sim;
pub mod transport;

pub use freshness::{
    FreshnessPolicy, FreshnessVerdict, NetworkObservation, StagedObservation, evaluate_freshness,
};
pub use mediator::{AuditEntry, BroadcastReceipt, ChainRpcProfile, MediationError, Mediator};
pub use sim::SimChain;
pub use transport::{RpcError, RpcTransport};
