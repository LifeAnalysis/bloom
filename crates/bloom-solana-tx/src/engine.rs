//! The Solana transfer engine: stage → simulate-free confirm/sign → broadcast,
//! mirroring `bloom-tx`'s `TxEngine` shape for the native-transfer MVP.
//!
//! The engine owns the orchestration: it fetches the recent blockhash, builds
//! the canonical legacy message, stages it in the durable outbox, signs it
//! through the Broker/Signer triad (the [`crate::signing::SolanaTransferSigner`]),
//! records the signature, assembles the signed transaction, and broadcasts it
//! via the read client's gated `sendTransaction`. Reconciliation is separate
//! (see [`crate::reconcile`]).

use base64::Engine as _;
use bloom_broker_api::Digest32;
use bloom_solana::{SolanaClient, SolanaRpcError};
use sha2::{Digest as _, Sha256};
use thiserror::Error;

use crate::outbox::{OutboxError, SolanaOutbox, SolanaOutboxEntry, SolanaOutboxState};
use crate::signing::{SolanaSignOutcome, SolanaTransferSigner};
use crate::types::{SolanaTxStatus, StagedSolanaTransfer};
use crate::{assemble_transaction, build_transfer_message};

/// Default approval/signing TTL for a staged transfer (ms).
const SIGN_TTL_MS: u64 = 60_000;

/// Conservative estimate of Solana's per-slot duration, used only to turn a
/// block-height-denominated blockhash validity window into a wall-clock
/// expiry. Solana's target slot time; real slot times are frequently at or
/// above this under load, so converting at this rate slightly
/// *underestimates* how long the blockhash actually stays valid — erring
/// toward reaping a stage a little early rather than letting one that has
/// truly gone stale linger, which was the actual bug (Fix D,
/// PLAN-SOLANA-PR-FIXES.md).
const APPROX_SLOT_MS: u128 = 400;

/// A native SOL transfer intent as supplied by the write surface
/// (`wallets/<wallet>/chains/<chain>/outbox/new.tx`).
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SolanaTransferIntent {
    /// Destination public key, base58.
    pub destination: String,
    /// Native SOL debit in lamports.
    pub lamports: u64,
}

impl SolanaTransferIntent {
    /// The destination as its raw 32-byte public key.
    pub fn destination_bytes(&self) -> Result<[u8; 32], String> {
        let bytes = bs58::decode(&self.destination)
            .into_vec()
            .map_err(|e| e.to_string())?;
        bytes
            .try_into()
            .map_err(|_| "destination must be a 32-byte base58 public key".to_string())
    }
}

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("outbox: {0}")]
    Outbox(#[from] OutboxError),
    #[error("chain: {0}")]
    Chain(#[from] SolanaRpcError),
    #[error("signing: {0}")]
    Signer(String),
    #[error("invalid transfer: {0}")]
    Invalid(String),
    #[error("broadcasting is disabled for chain '{0}' (operator release posture)")]
    BroadcastDisabled(String),
}

/// Orchestrates the native SOL transfer lifecycle.
pub struct SolanaTransferEngine {
    outbox: SolanaOutbox,
    client: SolanaClient,
    signer: SolanaTransferSigner,
    chain: String,
}

impl SolanaTransferEngine {
    pub fn new(
        outbox: SolanaOutbox,
        client: SolanaClient,
        signer: SolanaTransferSigner,
        chain: impl Into<String>,
    ) -> Self {
        Self {
            outbox,
            client,
            signer,
            chain: chain.into(),
        }
    }

    pub fn outbox(&self) -> &SolanaOutbox {
        &self.outbox
    }

    pub fn chain(&self) -> &str {
        &self.chain
    }

    /// Turn a fetched blockhash's `last_valid_block_height` into a
    /// wall-clock expiry, so a staged-but-abandoned transfer is eventually
    /// reaped by the sweep guard instead of lingering forever
    /// (`stage()` previously hardcoded `expires_ms: 0`, which the sweep's
    /// `!= 0` guard treats as "never expires").
    ///
    /// Best-effort: if the current block height can't be fetched (a
    /// transient RPC hiccup right after the blockhash call that just
    /// succeeded), falls back to the full nominal validity window
    /// (`MAX_RECENT_BLOCKHASHES` worth of slots) from now rather than
    /// leaving the entry unexpirable again.
    async fn blockhash_expiry_ms(&self, last_valid_block_height: u64, now_ms: u128) -> u128 {
        // Solana's standard blockhash validity window.
        const NOMINAL_VALID_BLOCKS: u64 = 150;
        let blocks_remaining = match self.client.get_block_height().await {
            Ok(current_height) => last_valid_block_height.saturating_sub(current_height),
            Err(_) => NOMINAL_VALID_BLOCKS,
        };
        now_ms + u128::from(blocks_remaining) * APPROX_SLOT_MS
    }

    /// Stage a native transfer: fetch a recent blockhash, build the canonical
    /// legacy message, and persist the write-once intent in `pending/<id>/`.
    pub async fn stage(
        &self,
        wallet: &str,
        fee_payer: &[u8; 32],
        destination: &[u8; 32],
        lamports: u64,
        now_ms: u128,
    ) -> Result<StagedSolanaTransfer, EngineError> {
        // Establish cluster identity before fetching the blockhash. Keeping
        // both observations on the same client makes endpoint failover safe:
        // every endpoint must identify as the configured cluster.
        let genesis_hash = self.client.verify_genesis().await?;
        let blockhash = self.client.get_latest_blockhash().await?;
        let blockhash_bytes: [u8; 32] = bs58::decode(&blockhash.blockhash)
            .into_vec()
            .map_err(|e| EngineError::Invalid(format!("blockhash base58: {e}")))?
            .try_into()
            .map_err(|_| EngineError::Invalid("blockhash must be 32 bytes".into()))?;
        let message = build_transfer_message(fee_payer, destination, lamports, &blockhash_bytes)
            .map_err(|e| EngineError::Invalid(e.to_string()))?;
        let payload_digest_hex = hex::encode(Sha256::digest(&message));
        let id = self.outbox.allocate_id();
        let expires_ms = self
            .blockhash_expiry_ms(blockhash.last_valid_block_height, now_ms)
            .await;
        let staged = StagedSolanaTransfer {
            id,
            wallet: wallet.to_string(),
            chain: self.chain.clone(),
            fee_payer: bs58::encode(fee_payer).into_string(),
            destination: bs58::encode(destination).into_string(),
            lamports,
            genesis_hash,
            blockhash: blockhash.blockhash,
            last_valid_block_height: blockhash.last_valid_block_height,
            message_b64: base64::engine::general_purpose::STANDARD.encode(&message),
            payload_digest_hex,
            signature: None,
            created_ms: now_ms,
            expires_ms,
            status: SolanaTxStatus::Pending,
            action_id: None,
        };
        self.outbox.write_pending(
            &staged,
            &format!(
                "Solana native transfer: {} → {} ({lamports} lamports)\nblockhash {} valid through block {}\n",
                staged.fee_payer,
                staged.destination,
                staged.blockhash,
                staged.last_valid_block_height,
            ),
        )?;
        Ok(staged)
    }

    /// Confirm and sign a staged transfer. On `Signed` the signature is
    /// recorded in the outbox, but the entry **stays `pending`** — it only
    /// moves to `sent` once [`Self::broadcast`] actually succeeds. A signed
    /// message with no broadcast attempt yet is still exactly as
    /// retryable/cancellable as an unsigned one (mirrors `bloom-tx`'s EVM
    /// `confirm` path, which gates its `Sent` transition on the broadcast
    /// RPC call itself succeeding, never on signing alone). On
    /// `ApprovalRequired` the ceremony details are returned and the entry
    /// stays pending for a retry with the returned `approval_id`.
    pub async fn sign(
        &self,
        wallet: &str,
        id: &str,
        fee_payer: &[u8; 32],
        approval_id: Option<Digest32>,
        now_ms: u128,
    ) -> Result<SolanaSignOutcome, EngineError> {
        let entry =
            self.outbox
                .read_in_state(wallet, &self.chain, id, SolanaOutboxState::Pending)?;
        let message = base64::engine::general_purpose::STANDARD
            .decode(&entry.staged.message_b64)
            .map_err(|e| EngineError::Invalid(format!("message base64: {e}")))?;
        validate_staged_message(&entry.staged, fee_payer, &message)?;
        let canonical_plan_facts = serde_jcs::to_vec(&entry.staged)
            .map_err(|e| EngineError::Invalid(format!("canonical plan facts: {e}")))?;
        let plan_facts_digest = Digest32::from_bytes(Sha256::digest(&canonical_plan_facts).into());

        let outcome = self
            .signer
            .sign_transfer(
                wallet,
                fee_payer,
                &message,
                approval_id,
                now_ms.min(u128::from(u64::MAX)) as u64,
                (now_ms + u128::from(SIGN_TTL_MS)).min(u128::from(u64::MAX)) as u64,
                plan_facts_digest,
            )
            .await
            .map_err(EngineError::Signer)?;

        if let SolanaSignOutcome::Signed { signature } = &outcome {
            let signature_b58 = bs58::encode(signature).into_string();
            self.outbox
                .record_signature(wallet, &self.chain, id, &signature_b58)?;
        }
        Ok(outcome)
    }

    /// Broadcast a signed transfer: assemble the transaction from the
    /// recorded signature + message and submit it. The entry transitions
    /// `pending` → `sent` only *after* the broadcast RPC call actually
    /// succeeds — if `send_transaction` fails (RPC timeout, node error, a
    /// normal and expected failure mode), the entry is left exactly where
    /// it was, still `pending` and so still retryable (call `sign`+
    /// `broadcast` again — signing is idempotent, it just re-records the
    /// same signature) or cancellable, never permanently stranded in a
    /// `sent` state that reflects a broadcast that never happened.
    /// Returns the transaction signature.
    pub async fn broadcast(
        &self,
        wallet: &str,
        id: &str,
        now_ms: u128,
    ) -> Result<String, EngineError> {
        if !self.client.allow_broadcast() {
            return Err(EngineError::BroadcastDisabled(self.chain.clone()));
        }
        let entry =
            self.outbox
                .read_in_state(wallet, &self.chain, id, SolanaOutboxState::Pending)?;
        let observed_genesis = self.client.verify_genesis().await?;
        if observed_genesis != entry.staged.genesis_hash {
            return Err(EngineError::Invalid(format!(
                "live cluster genesis {} differs from staged genesis {}",
                observed_genesis, entry.staged.genesis_hash
            )));
        }
        let signature_b58 = entry
            .staged
            .signature
            .as_ref()
            .ok_or_else(|| EngineError::Invalid("entry has no recorded signature".into()))?;
        let signature: [u8; 64] = bs58::decode(signature_b58)
            .into_vec()
            .map_err(|e| EngineError::Invalid(format!("signature base58: {e}")))?
            .try_into()
            .map_err(|_| EngineError::Invalid("signature must be 64 bytes".into()))?;
        let message = base64::engine::general_purpose::STANDARD
            .decode(&entry.staged.message_b64)
            .map_err(|e| EngineError::Invalid(format!("message base64: {e}")))?;
        let fee_payer: [u8; 32] = bs58::decode(&entry.staged.fee_payer)
            .into_vec()
            .map_err(|e| EngineError::Invalid(format!("fee payer base58: {e}")))?
            .try_into()
            .map_err(|_| EngineError::Invalid("fee payer must be 32 bytes".into()))?;
        validate_staged_message(&entry.staged, &fee_payer, &message)?;
        let tx_bytes = assemble_transaction(&message, &signature)
            .map_err(|e| EngineError::Invalid(e.to_string()))?;
        let tx_b64 = base64::engine::general_purpose::STANDARD.encode(&tx_bytes);

        // Broadcast first, while the entry is still `pending`: on failure
        // it must stay exactly there, never move to `sent` for a broadcast
        // that never actually happened.
        let submitted = self.client.send_transaction(&tx_b64).await?;

        // Only a confirmed-successful broadcast earns the `sent` transition.
        // `staged.status` and the on-disk directory must agree, so derive
        // the target state from the status via `from_status` rather than
        // hardcoding a state literal that could drift from it.
        let mut staged = entry.staged.clone();
        staged.status = SolanaTxStatus::Sent;
        let target_state = SolanaOutboxState::from_status(&staged.status);
        let sent_dir = self.outbox.transition(&entry, target_state)?;
        let sent_entry = SolanaOutboxEntry {
            state: target_state,
            staged,
            dir: sent_dir,
        };
        self.outbox.rewrite_intent(&sent_entry)?;
        self.outbox
            .write_broadcast_attempt(&sent_entry, &submitted, &tx_bytes, now_ms)?;
        Ok(submitted)
    }
}

fn validate_staged_message(
    staged: &StagedSolanaTransfer,
    expected_fee_payer: &[u8; 32],
    message: &[u8],
) -> Result<(), EngineError> {
    let stored_fee_payer: [u8; 32] = bs58::decode(&staged.fee_payer)
        .into_vec()
        .map_err(|e| EngineError::Invalid(format!("fee payer base58: {e}")))?
        .try_into()
        .map_err(|_| EngineError::Invalid("fee payer must be 32 bytes".into()))?;
    if &stored_fee_payer != expected_fee_payer {
        return Err(EngineError::Invalid(
            "resolved signing key differs from the staged fee payer".into(),
        ));
    }
    let destination: [u8; 32] = bs58::decode(&staged.destination)
        .into_vec()
        .map_err(|e| EngineError::Invalid(format!("destination base58: {e}")))?
        .try_into()
        .map_err(|_| EngineError::Invalid("destination must be 32 bytes".into()))?;
    let blockhash: [u8; 32] = bs58::decode(&staged.blockhash)
        .into_vec()
        .map_err(|e| EngineError::Invalid(format!("blockhash base58: {e}")))?
        .try_into()
        .map_err(|_| EngineError::Invalid("blockhash must be 32 bytes".into()))?;
    let expected =
        build_transfer_message(&stored_fee_payer, &destination, staged.lamports, &blockhash)
            .map_err(|e| EngineError::Invalid(e.to_string()))?;
    if expected != message {
        return Err(EngineError::Invalid(
            "serialized message differs from immutable staged transfer facts".into(),
        ));
    }
    let digest = hex::encode(Sha256::digest(message));
    if digest != staged.payload_digest_hex {
        return Err(EngineError::Invalid(
            "serialized message digest differs from staged payload digest".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn staged_fixture() -> (StagedSolanaTransfer, [u8; 32], Vec<u8>) {
        let payer = [0x11; 32];
        let destination = [0x22; 32];
        let blockhash = [0x42; 32];
        let message = build_transfer_message(&payer, &destination, 1_000_000, &blockhash).unwrap();
        let staged = StagedSolanaTransfer {
            id: "0001-test".into(),
            wallet: "wallet".into(),
            chain: "solana-devnet".into(),
            fee_payer: bs58::encode(payer).into_string(),
            destination: bs58::encode(destination).into_string(),
            lamports: 1_000_000,
            genesis_hash: "test-genesis".into(),
            blockhash: bs58::encode(blockhash).into_string(),
            last_valid_block_height: 100,
            message_b64: base64::engine::general_purpose::STANDARD.encode(&message),
            payload_digest_hex: hex::encode(Sha256::digest(&message)),
            signature: None,
            created_ms: 1,
            expires_ms: 2,
            status: SolanaTxStatus::Pending,
            action_id: None,
        };
        (staged, payer, message)
    }

    #[test]
    fn staged_message_validation_binds_economic_and_freshness_facts() {
        let (staged, payer, message) = staged_fixture();
        validate_staged_message(&staged, &payer, &message).unwrap();

        let mut changed_amount = staged.clone();
        changed_amount.lamports += 1;
        assert!(validate_staged_message(&changed_amount, &payer, &message).is_err());

        let mut changed_blockhash = staged.clone();
        changed_blockhash.blockhash = bs58::encode([0x43; 32]).into_string();
        assert!(validate_staged_message(&changed_blockhash, &payer, &message).is_err());

        let mut changed_digest = staged;
        changed_digest.payload_digest_hex = "00".repeat(32);
        assert!(validate_staged_message(&changed_digest, &payer, &message).is_err());
    }
}
