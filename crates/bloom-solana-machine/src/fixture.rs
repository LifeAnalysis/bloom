//! Fixture signing and approval authorities.
//!
//! Both stand in for triad components until the BIP-39 agent publishes the
//! real Ed25519 Signer edge and Broker ceremonies:
//!
//! - [`FixtureEd25519Signer`] performs **real** Ed25519 over the raw message
//!   bytes (the Solana convention — no pre-hash) with a fixed seed, and
//!   locally verifies every signature against its pinned public key.
//! - [`ExactApprovalLedger`] records one-shot exact approvals binding the
//!   payload digest; replay and divergence fail closed.

use std::collections::HashSet;
use std::sync::Mutex;

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

use crate::{
    ApprovalAuthority, ApprovalDenied, ApprovalToken, ExactApprovalFacts, SigningAuthority,
    SigningError,
};
use async_trait::async_trait;

/// A fixture signer with a fixed 32-byte seed. Produces genuine Ed25519
/// signatures over raw bytes; never used outside tests and the local loop.
pub struct FixtureEd25519Signer {
    key: SigningKey,
}

impl FixtureEd25519Signer {
    pub fn from_seed(seed: [u8; 32]) -> Self {
        Self {
            key: SigningKey::from_bytes(&seed),
        }
    }

    pub fn verifying_key(&self) -> VerifyingKey {
        self.key.verifying_key()
    }
}

impl SigningAuthority for FixtureEd25519Signer {
    fn public_key_bytes(&self) -> [u8; 32] {
        self.key.verifying_key().to_bytes()
    }

    fn sign_raw(&self, message: &[u8]) -> Result<[u8; 64], SigningError> {
        // Ed25519 over the raw serialized message bytes — the Anza-verified
        // convention. No SHA-256 pre-hash anywhere.
        let signature: Signature = self.key.sign(message);
        let bytes = signature.to_bytes();
        self.key
            .verifying_key()
            .verify(message, &Signature::from(bytes))
            .map_err(|_| SigningError::VerificationFailed)?;
        Ok(bytes)
    }
}

/// Scriptable exact-approval authority. `deny_all` flips the ledger into
/// fail-closed mode (no ceremony available).
#[derive(Default)]
pub struct ExactApprovalLedger {
    approved: Mutex<HashSet<String>>,
    deny_all: bool,
}

impl ExactApprovalLedger {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn denying() -> Self {
        Self {
            deny_all: true,
            ..Self::default()
        }
    }

    pub fn approval_count(&self) -> usize {
        self.approved.lock().unwrap().len()
    }
}

#[async_trait]
impl ApprovalAuthority for ExactApprovalLedger {
    async fn approve_exact(
        &self,
        facts: &ExactApprovalFacts,
    ) -> Result<ApprovalToken, ApprovalDenied> {
        if self.deny_all {
            return Err(ApprovalDenied::Denied("ceremony unavailable".into()));
        }
        let mut guard = self.approved.lock().unwrap();
        // An exact approval is one-shot: the same digest cannot be approved
        // twice (replay) and nothing else is approvable implicitly.
        if !guard.insert(facts.payload_digest_hex.clone()) {
            return Err(ApprovalDenied::Denied(
                "exact approval already consumed for this payload".into(),
            ));
        }
        Ok(ApprovalToken {
            approval_id: format!("approval-{}", facts.payload_digest_hex),
            approved_payload_digest_hex: facts.payload_digest_hex.clone(),
        })
    }
}
