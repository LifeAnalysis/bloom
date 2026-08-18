//! Solana-typed durable-state records, mirroring `bloom-proto::StagedTx`'s
//! role in the EVM outbox but without any `alloy` fields.

use serde::{Deserialize, Serialize};

/// Lifecycle status of a staged Solana transfer, mirroring `TxStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SolanaTxStatus {
    Pending,
    Sent,
    Success,
    Failed,
    Cancelled,
}

/// The write-once staged record, persisted as `intent.json`.
///
/// A Solana transfer's durable identity is its message bytes and the
/// blockhash/last-valid-height freshness window — there is no nonce. The
/// fields are exactly what construction pins before approval; signing and
/// broadcast only ever *consume* this record, never rewrite it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StagedSolanaTransfer {
    /// Outbox id (e.g. `"0001-12345"`), not the signature.
    pub id: String,
    pub wallet: String,
    /// Chain profile name (e.g. `"solana-devnet"`).
    pub chain: String,
    /// Fee payer and transfer source, base58.
    pub fee_payer: String,
    /// Transfer destination, base58.
    pub destination: String,
    /// Native SOL debit in lamports.
    pub lamports: u64,
    /// Recent blockhash, base58 — the freshness anchor.
    pub blockhash: String,
    /// The block height at which `blockhash` stops being valid.
    pub last_valid_block_height: u64,
    /// Serialized legacy message (base64) — the exact bytes to be signed.
    pub message_b64: String,
    /// SHA-256 of the message bytes (hex) — Bloom's payload commitment.
    pub payload_digest_hex: String,
    /// The transaction signature (base58), stamped at signing time. Absent
    /// until the signing step runs; scanners skip entries without it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    pub created_ms: u128,
    /// 0 means no expiry.
    pub expires_ms: u128,
    pub status: SolanaTxStatus,
    /// Central-outbox `action_id`, stamped when a projection is attached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_id: Option<String>,
}

/// A parsed view of a `sent/<id>/intent.json` entry, for background scanners.
#[derive(Debug, Clone)]
pub struct SolanaSentEntry {
    pub wallet: String,
    pub chain: String,
    pub id: String,
    /// The transaction signature (base58), parsed from the broadcast marker;
    /// entries without a recorded signature are skipped by scanners.
    pub signature: String,
    pub fee_payer: String,
    pub destination: String,
    pub lamports: u64,
    pub blockhash: String,
    pub last_valid_block_height: u64,
    /// `intent.json` mtime — the stable "sent at" proxy (the directory mtime
    /// is unreliable because scanners write sibling artefacts into it).
    pub sent_at: std::time::SystemTime,
    /// `true` once a `receipt.json` has been written by the reconciler.
    pub mined: bool,
}

/// Filename of the mined-outcome sibling, written by the reconciliation loop.
pub const RECEIPT_FILE: &str = "receipt.json";

/// The persistent record of a sent transfer's on-chain outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SolanaReceipt {
    /// `"success"` or `"failed"`.
    pub outcome: String,
    /// The transaction signature (base58).
    pub signature: String,
    /// Slot in which the transaction landed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<u64>,
    /// The node's `err` object, when the transaction failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub err: Option<serde_json::Value>,
    /// The node's confirmation status (`processed`/`confirmed`/`finalized`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmation_status: Option<String>,
}

impl SolanaReceipt {
    pub fn is_success(&self) -> bool {
        self.outcome == "success"
    }
}
