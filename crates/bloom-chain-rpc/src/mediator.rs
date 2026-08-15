//! The Machine-owned RPC mediator: profile resolution, method allowlists,
//! genesis-hash binding, response caps, and read/broadcast separation, with
//! an adjacent audit trail for every mediated call.

use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::transport::{RpcError, RpcTransport};

/// Default maximum serialized response size accepted from a provider.
pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 64 * 1024;

#[derive(Debug, Error)]
pub enum MediationError {
    #[error("method '{0}' is not allowed for profile '{1}'")]
    MethodNotAllowed(String, String),
    #[error("broadcast is disabled for profile '{0}'")]
    BroadcastDisabled(String),
    #[error(
        "cluster genesis mismatch for profile '{profile}': expected {expected}, observed {observed}"
    )]
    ClusterGenesisMismatch {
        profile: String,
        expected: String,
        observed: String,
    },
    #[error("response of {0} bytes exceeds the {1}-byte cap")]
    ResponseTooLarge(usize, usize),
    #[error("rpc: {0}")]
    Rpc(#[from] RpcError),
    #[error("profile '{0}' has no endpoints")]
    NoEndpoints(String),
}

/// An operator-configured chain profile. The driver names the profile and a
/// method; it never sees endpoints or credentials.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainRpcProfile {
    pub name: String,
    pub family: String,
    /// Expected cluster genesis hash (hex); enforced on first mediated call.
    pub expected_genesis_hex: String,
    /// Allowed read methods (exact JSON-RPC method names).
    pub allowed_read_methods: Vec<String>,
    /// Whether `sendTransaction` is permitted at all on this profile.
    pub allow_broadcast: bool,
    #[serde(default = "default_max_response")]
    pub max_response_bytes: usize,
}

fn default_max_response() -> usize {
    DEFAULT_MAX_RESPONSE_BYTES
}

/// One audit entry per mediated call: network intent adjacent to result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEntry {
    pub at_ms: u64,
    pub endpoint: String,
    pub method: String,
    /// `"read"` or `"broadcast"`.
    pub kind: String,
    pub outcome: String,
    /// Present for broadcast: the staged operation and pinned artifact digest.
    pub operation_id: Option<String>,
    pub artifact_digest_hex: Option<String>,
}

/// The receipt returned for a mediated broadcast.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BroadcastReceipt {
    /// Provider-assigned transaction signature (or a provider-equivalent id).
    pub signature: String,
    /// Whether the response arrived at all. A timeout is NOT a receipt; the
    /// caller records ambiguity at the outbox level.
    pub accepted: bool,
}

/// Mediates every driver RPC call through the configured transports.
pub struct Mediator {
    profile: ChainRpcProfile,
    transports: Vec<Box<dyn RpcTransport>>,
    genesis_verified: Mutex<bool>,
    audit: Mutex<Vec<AuditEntry>>,
}

impl Mediator {
    pub fn new(
        profile: ChainRpcProfile,
        transports: Vec<Box<dyn RpcTransport>>,
    ) -> Result<Self, MediationError> {
        if transports.is_empty() {
            return Err(MediationError::NoEndpoints(profile.name));
        }
        Ok(Self {
            profile,
            transports,
            genesis_verified: Mutex::new(false),
            audit: Mutex::new(Vec::new()),
        })
    }

    /// The configured profile (public projection; no credentials exist to leak).
    pub fn profile(&self) -> &ChainRpcProfile {
        &self.profile
    }

    /// Every audit entry recorded so far.
    pub fn audit(&self) -> Vec<AuditEntry> {
        self.audit.lock().unwrap().clone()
    }

    /// Verify cluster identity once per mediator lifetime: the first
    /// endpoint's genesis hash must equal the profile's expectation.
    pub fn verify_cluster(&self) -> Result<(), MediationError> {
        let mut done = self.genesis_verified.lock().unwrap();
        if *done {
            return Ok(());
        }
        let observed = self.transports[0].call("getGenesisHash", &Value::Null)?;
        let observed = observed.as_str().unwrap_or_default().to_string();
        if observed != self.profile.expected_genesis_hex {
            return Err(MediationError::ClusterGenesisMismatch {
                profile: self.profile.name.clone(),
                expected: self.profile.expected_genesis_hex.clone(),
                observed,
            });
        }
        *done = true;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)] // audit-record builder
    fn record(
        &self,
        at_ms: u64,
        endpoint: &str,
        method: &str,
        kind: &str,
        outcome: &str,
        operation_id: Option<&str>,
        artifact_digest_hex: Option<&str>,
    ) {
        self.audit.lock().unwrap().push(AuditEntry {
            at_ms,
            endpoint: endpoint.to_string(),
            method: method.to_string(),
            kind: kind.to_string(),
            outcome: outcome.to_string(),
            operation_id: operation_id.map(str::to_string),
            artifact_digest_hex: artifact_digest_hex.map(str::to_string),
        });
    }

    /// Perform a mediated read call. Enforces cluster binding, the read
    /// allowlist, and the response-size cap.
    pub fn read(&self, at_ms: u64, method: &str, params: &Value) -> Result<Value, MediationError> {
        self.verify_cluster()?;
        if !self
            .profile
            .allowed_read_methods
            .iter()
            .any(|m| m == method)
        {
            self.record(
                at_ms,
                self.transports[0].name(),
                method,
                "read",
                "denied:method",
                None,
                None,
            );
            return Err(MediationError::MethodNotAllowed(
                method.to_string(),
                self.profile.name.clone(),
            ));
        }
        let result = self.transports[0].call(method, params);
        match &result {
            Ok(value) => {
                let size = serde_json::to_vec(value).map_or(usize::MAX, |v| v.len());
                if size > self.profile.max_response_bytes {
                    self.record(
                        at_ms,
                        self.transports[0].name(),
                        method,
                        "read",
                        "denied:oversize",
                        None,
                        None,
                    );
                    return Err(MediationError::ResponseTooLarge(
                        size,
                        self.profile.max_response_bytes,
                    ));
                }
                self.record(
                    at_ms,
                    self.transports[0].name(),
                    method,
                    "read",
                    "ok",
                    None,
                    None,
                );
            }
            Err(e) => {
                self.record(
                    at_ms,
                    self.transports[0].name(),
                    method,
                    "read",
                    &format!("error:{e}"),
                    None,
                    None,
                );
            }
        }
        result.map_err(MediationError::Rpc)
    }

    /// Perform a mediated broadcast. Requires the staged operation id and the
    /// exact pinned artifact digest; both are recorded in the audit trail.
    /// A transport timeout returns `Err(RpcError::Timeout)` — ambiguity is
    /// the caller's (and the outbox's) responsibility, never silently folded
    /// into a receipt.
    pub fn broadcast(
        &self,
        at_ms: u64,
        operation_id: &str,
        artifact_digest_hex: &str,
        wire_hex: &str,
    ) -> Result<BroadcastReceipt, MediationError> {
        self.verify_cluster()?;
        if !self.profile.allow_broadcast {
            self.record(
                at_ms,
                self.transports[0].name(),
                "sendTransaction",
                "broadcast",
                "denied:disabled",
                Some(operation_id),
                Some(artifact_digest_hex),
            );
            return Err(MediationError::BroadcastDisabled(self.profile.name.clone()));
        }
        let outcome =
            self.transports[0].call("sendTransaction", &Value::String(wire_hex.to_string()));
        match outcome {
            Ok(sig) => {
                let signature = sig.as_str().unwrap_or_default().to_string();
                self.record(
                    at_ms,
                    self.transports[0].name(),
                    "sendTransaction",
                    "broadcast",
                    "accepted",
                    Some(operation_id),
                    Some(artifact_digest_hex),
                );
                Ok(BroadcastReceipt {
                    signature,
                    accepted: true,
                })
            }
            Err(e) => {
                self.record(
                    at_ms,
                    self.transports[0].name(),
                    "sendTransaction",
                    "broadcast",
                    &format!("error:{e}"),
                    Some(operation_id),
                    Some(artifact_digest_hex),
                );
                Err(MediationError::Rpc(e))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::SimChain;
    use serde_json::json;

    fn profile() -> ChainRpcProfile {
        ChainRpcProfile {
            name: "sim-local".into(),
            family: "sim".into(),
            expected_genesis_hex: "aa".repeat(4),
            allowed_read_methods: vec![
                "getGenesisHash".into(),
                "getBlockHeight".into(),
                "getLatestBlockhash".into(),
                "isBlockhashValid".into(),
                "getSignatureStatuses".into(),
            ],
            allow_broadcast: true,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
        }
    }

    #[test]
    fn genesis_binding_is_enforced() {
        let chain = SimChain::new(&"bb".repeat(4));
        let mediator = Mediator::new(profile(), vec![Box::new(chain)]).unwrap();
        let err = mediator
            .read(1, "getBlockHeight", &Value::Null)
            .unwrap_err();
        assert!(matches!(err, MediationError::ClusterGenesisMismatch { .. }));
        // And the denial is audited.
        assert!(
            mediator.audit().is_empty(),
            "genesis failure precedes any mediated call"
        );
    }

    #[test]
    fn method_allowlist_is_enforced() {
        let mediator =
            Mediator::new(profile(), vec![Box::new(SimChain::new(&"aa".repeat(4)))]).unwrap();
        assert!(mediator.read(1, "getBlockHeight", &Value::Null).is_ok());
        let err = mediator
            .read(1, "getBalance", &json!("someone"))
            .unwrap_err();
        assert!(matches!(err, MediationError::MethodNotAllowed(m, _) if m == "getBalance"));
        let audit = mediator.audit();
        assert_eq!(audit.last().unwrap().outcome, "denied:method");
    }

    #[test]
    fn broadcast_gating_and_audit() {
        let mut p = profile();
        p.allow_broadcast = false;
        let mediator = Mediator::new(p, vec![Box::new(SimChain::new(&"aa".repeat(4)))]).unwrap();
        let err = mediator.broadcast(1, "op", "digest", "wire").unwrap_err();
        assert!(matches!(err, MediationError::BroadcastDisabled(_)));

        let mut p2 = profile();
        p2.allow_broadcast = true;
        let mediator2 = Mediator::new(p2, vec![Box::new(SimChain::new(&"aa".repeat(4)))]).unwrap();
        let receipt = mediator2
            .broadcast(1, "op-1", "digest-1", "wire-1")
            .unwrap();
        assert!(receipt.accepted);
        let audit = mediator2.audit();
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].kind, "broadcast");
        assert_eq!(audit[0].operation_id.as_deref(), Some("op-1"));
        assert_eq!(audit[0].artifact_digest_hex.as_deref(), Some("digest-1"));
    }

    #[test]
    fn response_cap_is_enforced() {
        let mut p = profile();
        p.max_response_bytes = 8;
        let mediator = Mediator::new(p, vec![Box::new(SimChain::new(&"aa".repeat(4)))]).unwrap();
        let err = mediator
            .read(1, "getLatestBlockhash", &Value::Null)
            .unwrap_err();
        assert!(matches!(err, MediationError::ResponseTooLarge(_, 8)));
    }
}
