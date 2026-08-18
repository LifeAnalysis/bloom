//! Deterministic fixture chain driver.
//!
//! A test double for exercising the chain-action outbox without any chain
//! SDK, network, or real cryptography. It produces byte-deterministic
//! payloads and "signatures" derived from SHA-256 over the payload and a
//! caller-provided secret, so identical inputs always produce identical
//! staged actions across processes and restarts.
//!
//! **The fixture signature is not a cryptographic signature.** It exists only
//! to give the outbox bounded bytes to persist, digest, and retry. Real
//! signing flows through the triad (Broker approval, Signer custody) and is
//! out of scope for this crate.

use crate::{
    ArtifactSegment, ArtifactTemplate, BroadcastOutcome, ChainBinding, DriverBinding, NewAction,
};

/// Scripted broadcast behavior, consumed in order and then repeated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptedOutcome {
    Accept,
    Timeout,
    Reject,
}

/// A deterministic fixture driver bound to one package hash and secret.
#[derive(Debug, Clone)]
pub struct FixtureDriver {
    pub package_hash: String,
    secret: Vec<u8>,
    script: Vec<ScriptedOutcome>,
    steps: usize,
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().into()
}

impl FixtureDriver {
    pub fn new(package_hash: &str, secret: &[u8]) -> Self {
        Self {
            package_hash: package_hash.to_string(),
            secret: secret.to_vec(),
            script: Vec::new(),
            steps: 0,
        }
    }

    /// Script the broadcast outcomes. The last entry repeats forever.
    pub fn with_script(mut self, script: Vec<ScriptedOutcome>) -> Self {
        self.script = script;
        self
    }

    /// Build a deterministic staging request. The payload embeds the
    /// operation id and amount, so identical inputs stage identical bytes.
    #[allow(clippy::too_many_arguments)] // test-fixture builder
    pub fn stage_request(
        &self,
        operation_id: &str,
        wallet_id: &str,
        key_ref: &str,
        destination: &str,
        amount: u64,
        created_at_ms: u64,
        expires_at_ms: u64,
    ) -> NewAction {
        let mut payload = Vec::new();
        payload.extend_from_slice(b"fixture-transfer/v1\x00");
        payload.extend_from_slice(operation_id.as_bytes());
        payload.push(0);
        payload.extend_from_slice(destination.as_bytes());
        payload.push(0);
        payload.extend_from_slice(&amount.to_be_bytes());
        NewAction {
            operation_id: operation_id.to_string(),
            idempotency_key: format!("idem-{operation_id}"),
            driver: DriverBinding {
                package_hash: self.package_hash.clone(),
                route: "transfer.stage".to_string(),
                abi_version: 1,
                state_schema: 1,
            },
            wallet_id: wallet_id.to_string(),
            key_ref: key_ref.to_string(),
            chain: ChainBinding {
                family: "fixture".to_string(),
                profile: "fixture-local".to_string(),
                claimed_caip2: "fixture:local".to_string(),
            },
            operation_class: "fixture.native-transfer".to_string(),
            crypto_suite: "fixture-message".to_string(),
            // Artifact plan: `payload || fixture-signature` — one payload
            // literal, one signature slot, matching `assemble_artifact`.
            artifact_template: ArtifactTemplate {
                segments: vec![
                    ArtifactSegment::Literal {
                        bytes_hex: hex::encode(&payload),
                    },
                    ArtifactSegment::Signature { index: 0 },
                ],
            },
            payload,
            created_at_ms,
            expires_at_ms,
        }
    }

    /// Deterministic fixture "signature" (NOT cryptographic): SHA-256 over
    /// `secret || payload`, repeated to 64 bytes.
    pub fn fixture_sign(&self, payload: &[u8]) -> Vec<u8> {
        let mut input = Vec::with_capacity(self.secret.len() + payload.len());
        input.extend_from_slice(&self.secret);
        input.extend_from_slice(payload);
        let mut sig = Vec::with_capacity(64);
        let mut round = input.clone();
        while sig.len() < 64 {
            let d = sha256(&round);
            sig.extend_from_slice(&d);
            round = d.to_vec();
        }
        sig.truncate(64);
        sig
    }

    /// Assemble the signed artifact: `payload || signature`.
    pub fn assemble_artifact(&self, payload: &[u8]) -> Vec<u8> {
        let sig = self.fixture_sign(payload);
        let mut artifact = payload.to_vec();
        artifact.extend_from_slice(&sig);
        artifact
    }

    /// The next scripted broadcast outcome, consuming the script in order and
    /// repeating the final entry.
    pub fn next_broadcast_outcome(&mut self) -> BroadcastOutcome {
        let outcome = if self.script.is_empty() {
            ScriptedOutcome::Accept
        } else {
            let idx = self.steps.min(self.script.len() - 1);
            self.steps += 1;
            self.script[idx]
        };
        match outcome {
            ScriptedOutcome::Accept => BroadcastOutcome::Accepted,
            ScriptedOutcome::Timeout => BroadcastOutcome::Ambiguous,
            ScriptedOutcome::Reject => BroadcastOutcome::Rejected {
                reason: "fixture: rejected before dispatch".to_string(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_is_deterministic() {
        let a = FixtureDriver::new(&"a".repeat(64), b"secret-1");
        let b = FixtureDriver::new(&"a".repeat(64), b"secret-1");
        let ra = a.stage_request(&"0".repeat(64), "w", "k", "dest-1", 42, 1, 0);
        let rb = b.stage_request(&"0".repeat(64), "w", "k", "dest-1", 42, 1, 0);
        assert_eq!(ra.payload, rb.payload);
        assert_eq!(a.fixture_sign(&ra.payload), b.fixture_sign(&rb.payload));
        assert_eq!(
            a.assemble_artifact(&ra.payload),
            b.assemble_artifact(&rb.payload)
        );
        // Different amount changes the payload.
        let rc = a.stage_request(&"0".repeat(64), "w", "k", "dest-1", 43, 1, 0);
        assert_ne!(ra.payload, rc.payload);
    }

    #[test]
    fn script_repeats_last_entry() {
        let mut d = FixtureDriver::new(&"a".repeat(64), b"s")
            .with_script(vec![ScriptedOutcome::Timeout, ScriptedOutcome::Accept]);
        assert_eq!(d.next_broadcast_outcome(), BroadcastOutcome::Ambiguous);
        assert_eq!(d.next_broadcast_outcome(), BroadcastOutcome::Accepted);
        assert_eq!(d.next_broadcast_outcome(), BroadcastOutcome::Accepted);
    }
}
