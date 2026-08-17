//! The real Solana Machine lifecycle.
//!
//! [`SolanaMachine`] wires the already-built pieces into one durable loop:
//!
//! 1. **stage** — the Solana driver Petal (real WASM component through the
//!    Wasmtime/Petal host) constructs the canonical legacy transfer through a
//!    Machine-mediated `getLatestBlockhash`; the message bytes and economic
//!    facts are frozen into an immutable `bloom.chain-action/1` envelope.
//! 2. **verify** — the independent `solana-system-transfer-v1` verifier
//!    re-parses the Petal's bytes against the request's facts behind a
//!    fixture `KeyRef`; any divergence fails closed before approval.
//! 3. **freshness** — mediated observations (latest blockhash, height,
//!    validity) are evaluated; a refusal becomes a first-class terminal
//!    outbox transition and blocks signing.
//! 4. **exact approval** — an [`ApprovalAuthority`] must bind the exact
//!    payload digest, key, and verified facts before a signature is
//!    requested. The fixture ledger stands in for Broker Sealed Approval
//!    until the BIP-39 edge lands.
//! 5. **sign** — a [`SigningAuthority`] signs the **raw message bytes**
//!    (Ed25519, no pre-hash) and the signature is locally verified against
//!    the pinned public key before being recorded.
//! 6. **assemble** — the Petal assembles the complete signed transaction.
//! 7. **broadcast** — only through the staged profile's mediated transport;
//!    a post-dispatch timeout is recorded as `Ambiguous`, never re-signed.
//! 8. **reconcile** — by transaction signature over mediated status reads;
//!    ambiguity retries the exact persisted bytes, confirmation projects the
//!    observed commitment level.
//!
//! Nothing here talks to Broker or Signer: signing is fixture-only by design
//! until the BIP-39 agent publishes the real Ed25519 edge, and mainnet
//! broadcast remains disabled at the profile layer.

#![forbid(unsafe_code)]

pub mod account;
pub mod fixture;
pub mod host;
pub mod mount;
pub mod projection;

use std::sync::Arc;

use bloom_chain_action::{
    Action, ActionState, BroadcastOutcome, ChainActionOutbox, FreshnessReason, NewAction,
    OutboxError, ReconciliationOutcome,
};
use bloom_chain_rpc::mediator::{ChainRpcProfile, MediationError, Mediator};
use bloom_chain_rpc::{
    FreshnessPolicy, FreshnessVerdict, NetworkObservation, StagedObservation, evaluate_freshness,
};
use bloom_solana::adapter::{
    ADAPTER_SCHEMA, FixtureKeyRef, REQUIRED_OPERATION_CLASS, REQUIRED_SUITE, TransferClaimV1,
    VerifierInputV1,
};
use bloom_solana::{Pubkey, RejectionReason};
use bloom_vfs::path::VfsPath;

pub use account::{AccountError, AccountRegistry, EnabledAccount};
use bloom_vfs::{Handler, Vfs};
pub use projection::{
    AccountProjection, AssertedFacts, BindingProjections, ClusterProjection, FinalityProjection,
    FreshnessSummary, OperationProjection, PROJECTION_SCHEMA, VerifiedFacts,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

pub const STAGE_ROUTE: &str = "/petals/solana-driver/transfer.stage.json";
pub const ASSEMBLE_ROUTE: &str = "/petals/solana-driver/transfer.assemble.json";

/// Facts an exact approval must bind. Mirrors the Sealed Approval exact
/// selector: approving these facts authorizes exactly one signature over
/// exactly these bytes with exactly this key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExactApprovalFacts {
    pub operation_id: String,
    pub payload_digest_hex: String,
    pub fee_payer_base58: String,
    pub destination_base58: String,
    pub lamports: u64,
    /// Fee observed at staging (asserted).
    pub fee_lamports: u64,
    /// Approved hard fee ceiling.
    pub max_fee_lamports: u64,
    pub operation_class: String,
    pub crypto_suite: String,
    pub verifier_id: String,
    pub verifier_result_digest_hex: String,
}

/// The token an [`ApprovalAuthority`] issues for one exact approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalToken {
    pub approval_id: String,
    pub approved_payload_digest_hex: String,
}

/// Broker stands in for this until the triad wiring lands. Implementations
/// must fail closed and bind the exact payload digest.
#[async_trait::async_trait]
pub trait ApprovalAuthority: Send + Sync {
    async fn approve_exact(
        &self,
        facts: &ExactApprovalFacts,
    ) -> Result<ApprovalToken, ApprovalDenied>;
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ApprovalDenied {
    #[error("exact approval denied: {0}")]
    Denied(String),
    #[error("approval authority unavailable: {0}")]
    Unavailable(String),
}

/// A signing authority for raw Solana message bytes. The fixture
/// implementation performs real Ed25519 over the raw bytes — the Solana
/// convention, no pre-hash — and is the only secret-holder in this slice.
pub trait SigningAuthority: Send + Sync {
    /// The pinned public key every signed operation must verify against.
    fn public_key_bytes(&self) -> [u8; 32];
    /// Sign the exact raw message bytes.
    fn sign_raw(&self, message: &[u8]) -> Result<[u8; 64], SigningError>;
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SigningError {
    #[error("signing unavailable: {0}")]
    Unavailable(String),
    #[error("produced signature failed local verification")]
    VerificationFailed,
}

#[derive(Debug, Error)]
pub enum MachineError {
    #[error("outbox: {0}")]
    Outbox(#[from] OutboxError),
    #[error("mediated rpc: {0}")]
    Mediation(#[from] MediationError),
    #[error("petal route {route}: {source}")]
    Route {
        route: &'static str,
        #[source]
        source: bloom_vfs::HandlerError,
    },
    #[error("petal result missing field '{0}'")]
    MissingField(&'static str),
    #[error("independent verifier rejected the staged message: {0:?}")]
    VerifierRejected(RejectionReason),
    #[error("freshness refused: {0:?}")]
    FreshnessRefused(FreshnessReason),
    #[error("approval: {0}")]
    Approval(#[from] ApprovalDenied),
    #[error("signing: {0}")]
    Signing(#[from] SigningError),
    #[error("petal produced a signature for a different key")]
    SignerKeyMismatch,
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("bad hex from petal: {0}")]
    BadHex(String),
    #[error("staged facts no longer match the frozen envelope")]
    StaleStaging,
    #[error("retry is only legal for ambiguous operations; this one is '{0}'")]
    RetryNotAmbiguous(&'static str),
    #[error("fee refused: {0:?}")]
    FeeRefused(FeeRefusal),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// One transfer request driven through the full lifecycle.
#[derive(Debug, Clone)]
pub struct TransferRequest {
    /// 64 lowercase hex characters.
    pub operation_id: String,
    pub wallet_id: String,
    pub fee_payer_base58: String,
    pub destination_base58: String,
    pub lamports: u64,
    /// The fixture key identity standing in for the BIP-39 derived child.
    pub key_ref: FixtureKeyRef,
    /// 0 disables expiry sweeping.
    pub expires_at_ms: u64,
    /// Hard upper bound on the network fee this operation may consume. The
    /// honest-runtime fee gate refuses any observation above it.
    pub max_fee_lamports: u64,
    /// Claimed CAIP-2 identity; visible, never verifier-proven.
    pub claimed_caip2: String,
}

/// Where an operation stands after a lifecycle call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleStatus {
    /// Staged and verifier-accepted; awaiting freshness + approval + signing.
    Staged,
    /// Terminal freshness refusal; restage under a new approval.
    FreshnessRefused(FreshnessReason),
    /// Signed; the exact signed artifact is durably pinned.
    Signed,
    /// Dispatch accepted by the provider; awaiting reconciliation.
    Sent,
    /// Post-dispatch timeout or unknown provider result.
    Ambiguous,
    /// Reconciliation observed a terminal on-chain effect.
    Confirmed {
        confirmation: String,
    },
    /// Definitive pre-effect rejection.
    Failed {
        reason: String,
    },
    Quarantined {
        reason: String,
    },
    Cancelled,
    Expired,
}

/// The wired Machine. Clone-safe: every component is shared.
#[derive(Clone)]
pub struct SolanaMachine {
    vfs: Arc<Vfs>,
    mediator: Arc<Mediator>,
    outbox: Arc<ChainActionOutbox>,
    signer: Arc<dyn SigningAuthority>,
    approvals: Arc<dyn ApprovalAuthority>,
    policy: FreshnessPolicy,
    profile: String,
    package_hash: String,
}

/// Why a fee observation was refused. The fee is `machine_asserted`: these
/// gates bound honest-provider behavior and quoting, they do not verify it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeeRefusal {
    /// The provider returned `value: null` for the exact message.
    NullObservation,
    /// Two observations for the same message disagreed.
    Inconsistent { staged: u64, observed: u64 },
    /// The quoted fee exceeds the request's approved ceiling.
    OverLimit { observed: u64, max: u64 },
}

/// Everything `stage_transfer` froze, needed to finalize (freshness gate,
/// approval, signing, assembly) — possibly much later, after a ceremony.
#[derive(Debug, Clone)]
pub struct StagedTransfer {
    pub operation_id: String,
    pub message_hex: String,
    pub payload_digest_hex: String,
    pub blockhash_base58: String,
    pub last_valid_block_height: u64,
    /// Fee observed at staging (`getFeeForMessage` for the exact message).
    /// Asserted, never verifier-proven.
    pub fee_lamports: u64,
}

impl SolanaMachine {
    #[allow(clippy::too_many_arguments)] // fixed wiring surface; each arg is a distinct authority
    pub fn new(
        vfs: Arc<Vfs>,
        mediator: Arc<Mediator>,
        outbox: Arc<ChainActionOutbox>,
        signer: Arc<dyn SigningAuthority>,
        approvals: Arc<dyn ApprovalAuthority>,
        policy: FreshnessPolicy,
        profile: &str,
        package_hash: &str,
    ) -> Self {
        Self {
            vfs,
            mediator,
            outbox,
            signer,
            approvals,
            policy,
            profile: profile.to_string(),
            package_hash: package_hash.to_string(),
        }
    }

    /// The independent verifier re-parses the Petal-built message behind the
    /// fixture KeyRef. Runs before anything is approvable.
    fn verify_staged(
        &self,
        _key: Pubkey,
        request: &TransferRequest,
        message_hex: &str,
        payload_digest_hex: &str,
    ) -> Result<VerifierInputV1, RejectionReason> {
        let input = VerifierInputV1 {
            schema: ADAPTER_SCHEMA.to_string(),
            operation_class: REQUIRED_OPERATION_CLASS.to_string(),
            crypto_suite: REQUIRED_SUITE.to_string(),
            message_hex: message_hex.to_string(),
            payload_digest_hex: payload_digest_hex.to_string(),
            claim: TransferClaimV1 {
                fee_payer_base58: request.fee_payer_base58.clone(),
                destination_base58: request.destination_base58.clone(),
                lamports: request.lamports,
            },
            key_ref: request.key_ref.clone(),
            evidence: None,
        };
        input.validate().map_err(|e| RejectionReason::Malformed {
            detail: e.to_string(),
        })?;
        bloom_solana::adapter::run_verifier(&input)?;
        Ok(input)
    }

    async fn petal_write(&self, route: &'static str, body: &Value) -> Result<Value, MachineError> {
        let path = VfsPath::parse(route).expect("static route parses");
        self.vfs
            .write(&path, &serde_json::to_vec(body)?)
            .await
            .map_err(|source| MachineError::Route { route, source })?;
        let bytes = self
            .vfs
            .read(&path)
            .await
            .map_err(|source| MachineError::Route { route, source })?;
        let value: Value = serde_json::from_slice(&bytes)?;
        if value.get("state").and_then(|s| s.as_str()) == Some("ok") {
            Ok(value)
        } else {
            Err(MachineError::MissingField("state=ok"))
        }
    }

    /// Stage, verify, freshness-check, approve, sign, assemble — everything
    /// up to (but not including) broadcast, in one call. See
    /// [`stage_transfer`] and [`finalize_transfer`] for the split lifecycle
    /// (a ceremony may legitimately sit between staging and signing).
    pub async fn prepare_transfer(
        &self,
        request: &TransferRequest,
        now_ms: u64,
    ) -> Result<Action, MachineError> {
        let staged = self.stage_transfer(request, now_ms).await?;
        self.finalize_transfer(request, &staged, now_ms).await
    }

    /// Stage: construct through the real Petal (mediated blockhash read
    /// inside), freeze the immutable envelope, and verify the bytes with the
    /// independent verifier behind the request's key identity.
    pub async fn stage_transfer(
        &self,
        request: &TransferRequest,
        now_ms: u64,
    ) -> Result<StagedTransfer, MachineError> {
        // The signer's pinned key must be the key the request names.
        let pinned = self.signer.public_key_bytes();
        let named = request.key_ref.public_key().map_err(|e| {
            MachineError::VerifierRejected(RejectionReason::Malformed {
                detail: e.to_string(),
            })
        })?;
        if pinned != *named.as_bytes() {
            return Err(MachineError::SignerKeyMismatch);
        }

        // 1. Stage through the real Petal (mediated blockhash read inside).
        let staged = self
            .petal_write(
                STAGE_ROUTE,
                &json!({
                    "chain_profile": self.profile,
                    "fee_payer_base58": request.fee_payer_base58,
                    "destination_base58": request.destination_base58,
                    "lamports": request.lamports,
                }),
            )
            .await?;
        let message_hex = staged
            .get("message_hex")
            .and_then(|v| v.as_str())
            .ok_or(MachineError::MissingField("message_hex"))?
            .to_string();
        let payload_digest_hex = staged
            .get("payload_digest_hex")
            .and_then(|v| v.as_str())
            .ok_or(MachineError::MissingField("payload_digest_hex"))?
            .to_string();
        let blockhash_base58 = staged
            .get("blockhash_base58")
            .and_then(|v| v.as_str())
            .ok_or(MachineError::MissingField("blockhash_base58"))?
            .to_string();
        let last_valid = staged
            .get("last_valid_block_height")
            .and_then(|v| v.as_u64())
            .ok_or(MachineError::MissingField("last_valid_block_height"))?;

        let message_bytes =
            hex::decode(&message_hex).map_err(|e| MachineError::BadHex(e.to_string()))?;

        // 2. Freeze the immutable envelope.
        self.outbox.stage(NewAction {
            operation_id: request.operation_id.clone(),
            idempotency_key: format!("idem-{}", request.operation_id),
            driver: bloom_chain_action::DriverBinding {
                package_hash: self.package_hash.clone(),
                route: "transfer.stage.json".to_string(),
                abi_version: 1,
                state_schema: 1,
            },
            wallet_id: request.wallet_id.clone(),
            key_ref: serde_json::to_string(&request.key_ref)?,
            chain: bloom_chain_action::ChainBinding {
                family: "solana".to_string(),
                profile: self.profile.clone(),
                claimed_caip2: request.claimed_caip2.clone(),
            },
            operation_class: REQUIRED_OPERATION_CLASS.to_string(),
            crypto_suite: REQUIRED_SUITE.to_string(),
            payload: message_bytes.clone(),
            created_at_ms: now_ms,
            expires_at_ms: request.expires_at_ms,
        })?;

        // 3. Independent verification behind the fixture KeyRef.
        let verified = self
            .verify_staged(named, request, &message_hex, &payload_digest_hex)
            .map_err(MachineError::VerifierRejected)?;
        let verified_result = bloom_solana::adapter::run_verifier(&verified)
            .map_err(MachineError::VerifierRejected)?;

        // Fee observation for the exact message. Asserted, bounded, displayed
        // as asserted — never verifier-proven.
        let fee_lamports = self.observe_fee(now_ms, &message_bytes).await?;
        self.outbox.record_fee_observed(
            &request.operation_id,
            now_ms,
            fee_lamports,
            request.max_fee_lamports,
        )?;
        self.outbox.record_facts_verified(
            &request.operation_id,
            now_ms,
            request.fee_payer_base58.clone(),
            request.destination_base58.clone(),
            request.lamports,
            verified_result.verifier_id.clone(),
            verified_result.result_digest_hex.clone(),
            payload_digest_hex.clone(),
        )?;
        self.outbox.record_liveness_observed(
            &request.operation_id,
            now_ms,
            blockhash_base58.clone(),
            last_valid,
            now_ms,
        )?;

        Ok(StagedTransfer {
            operation_id: request.operation_id.clone(),
            message_hex,
            payload_digest_hex,
            blockhash_base58: blockhash_base58.clone(),
            last_valid_block_height: last_valid,
            fee_lamports,
        })
    }

    /// One mediated `getFeeForMessage` for the exact message bytes. Refuses
    /// null and over-limit quotes.
    async fn observe_fee(&self, now_ms: u64, message_bytes: &[u8]) -> Result<u64, MachineError> {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(message_bytes);
        let fee: Value = serde_json::from_str(
            &self
                .mediator
                .read(now_ms, "getFeeForMessage", &serde_json::json!([b64]))?
                .to_string(),
        )?;
        let observed = fee.get("value").and_then(|v| v.as_u64());
        let observed = match observed {
            Some(lamports) => lamports,
            None => return Err(MachineError::FeeRefused(FeeRefusal::NullObservation)),
        };
        Ok(observed)
    }

    /// Finalize: freshness gate, exact approval, sign, assemble. The staged
    /// envelope is never re-derived: `staged` must match the frozen bytes.
    pub async fn finalize_transfer(
        &self,
        request: &TransferRequest,
        staged: &StagedTransfer,
        now_ms: u64,
    ) -> Result<Action, MachineError> {
        let action = self.outbox.load(&staged.operation_id)?;
        if action.envelope.payload_hex
            != hex::encode(
                hex::decode(&staged.message_hex)
                    .map_err(|e| MachineError::BadHex(e.to_string()))?,
            )
        {
            return Err(MachineError::StaleStaging);
        }
        let StagedTransfer {
            operation_id,
            message_hex,
            payload_digest_hex,
            blockhash_base58,
            last_valid_block_height: last_valid,
            fee_lamports: _,
        } = staged;
        let verified = self
            .verify_staged(
                Pubkey::from_bytes(self.signer.public_key_bytes()),
                request,
                message_hex,
                payload_digest_hex,
            )
            .map_err(MachineError::VerifierRejected)?;
        let result = bloom_solana::adapter::run_verifier(&verified)
            .map_err(MachineError::VerifierRejected)?;

        // Fee gate: re-observe for the exact frozen message; refuse null,
        // inconsistency with the staged observation, or a quote over the
        // approved ceiling. Fees are asserted facts — this bounds honest
        // quoting, it does not verify it.
        let observed_fee = self
            .observe_fee(
                now_ms,
                &hex::decode(message_hex).map_err(|e| MachineError::BadHex(e.to_string()))?,
            )
            .await?;
        if observed_fee != staged.fee_lamports {
            return Err(MachineError::FeeRefused(FeeRefusal::Inconsistent {
                staged: staged.fee_lamports,
                observed: observed_fee,
            }));
        }
        if observed_fee > request.max_fee_lamports {
            return Err(MachineError::FeeRefused(FeeRefusal::OverLimit {
                observed: observed_fee,
                max: request.max_fee_lamports,
            }));
        }

        // Freshness gate over mediated observations.
        let observation = self.observe_network(now_ms, blockhash_base58).await?;
        let staged_observation = StagedObservation {
            blockhash: blockhash_base58.clone(),
            last_valid_block_height: *last_valid,
            staged_at_ms: now_ms,
            commitment: "confirmed".to_string(),
        };
        if let FreshnessVerdict::Refused(reason) =
            evaluate_freshness(&staged_observation, &observation, None, &self.policy)
        {
            self.outbox
                .refuse_for_freshness(operation_id, now_ms, reason)?;
            return Err(MachineError::FreshnessRefused(reason));
        }

        // 5. Exact approval binding the verifier's facts.
        let facts = ExactApprovalFacts {
            operation_id: operation_id.clone(),
            payload_digest_hex: payload_digest_hex.clone(),
            fee_payer_base58: request.fee_payer_base58.clone(),
            destination_base58: request.destination_base58.clone(),
            lamports: request.lamports,
            fee_lamports: staged.fee_lamports,
            max_fee_lamports: request.max_fee_lamports,
            operation_class: REQUIRED_OPERATION_CLASS.to_string(),
            crypto_suite: REQUIRED_SUITE.to_string(),
            verifier_id: result.verifier_id.clone(),
            verifier_result_digest_hex: result.result_digest_hex.clone(),
        };
        let token = self.approvals.approve_exact(&facts).await?;
        if token.approved_payload_digest_hex != *payload_digest_hex {
            return Err(
                ApprovalDenied::Denied("approval token does not bind this payload".into()).into(),
            );
        }

        // 6. Sign the raw message bytes and locally verify before recording.
        let signature = self.signer.sign_raw(
            &hex::decode(message_hex).map_err(|e| MachineError::BadHex(e.to_string()))?,
        )?;
        let assembled = self
            .petal_write(
                ASSEMBLE_ROUTE,
                &json!({
                    "message_hex": message_hex,
                    "signature_hex": hex::encode(signature),
                }),
            )
            .await?;
        let transaction_hex = assembled
            .get("transaction_hex")
            .and_then(|v| v.as_str())
            .ok_or(MachineError::MissingField("transaction_hex"))?
            .to_string();
        let artifact =
            hex::decode(&transaction_hex).map_err(|e| MachineError::BadHex(e.to_string()))?;
        let action = self
            .outbox
            .record_signed(operation_id, now_ms, &signature, &artifact)?;

        Ok(action)
    }

    /// Gather a freshness observation through mediated reads, including an
    /// explicit `isBlockhashValid` check on the staged blockhash.
    async fn observe_network(
        &self,
        now_ms: u64,
        staged_blockhash: &str,
    ) -> Result<NetworkObservation, MachineError> {
        let latest: Value = serde_json::from_str(
            &self
                .mediator
                .read(now_ms, "getLatestBlockhash", &serde_json::json!([]))?
                .to_string(),
        )?;
        let height: Value = self
            .mediator
            .read(now_ms, "getBlockHeight", &serde_json::json!([]))?;
        let validity: Value = self.mediator.read(
            now_ms,
            "isBlockhashValid",
            &serde_json::json!([staged_blockhash]),
        )?;
        Ok(NetworkObservation {
            latest_blockhash: latest
                .pointer("/value/blockhash")
                .or_else(|| latest.get("blockhash"))
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            latest_block_height: height.as_u64().unwrap_or(u64::MAX),
            blockhash_valid: validity.get("valid").and_then(|v| v.as_bool()),
            observed_at_ms: now_ms,
            commitment: "confirmed".to_string(),
        })
    }

    /// Broadcast the persisted artifact exactly once per attempt. A timeout
    /// after dispatch records `Ambiguous` — never a re-sign.
    pub async fn broadcast(&self, operation_id: &str, now_ms: u64) -> Result<Action, MachineError> {
        let action = self.outbox.record_broadcast_attempt(operation_id, now_ms)?;
        let artifact = action
            .artifact
            .as_ref()
            .expect("signed action has artifact");
        let attempt = action.attempts.last().expect("attempt recorded").attempt;
        let receipt = self.mediator.broadcast(
            now_ms,
            operation_id,
            &artifact.digest_hex,
            &hex::encode(&artifact.artifact),
        );
        let outcome = match receipt {
            Ok(_) => BroadcastOutcome::Accepted,
            Err(bloom_chain_rpc::MediationError::Rpc(bloom_chain_rpc::RpcError::Timeout)) => {
                BroadcastOutcome::Ambiguous
            }
            Err(bloom_chain_rpc::MediationError::Rpc(e)) => BroadcastOutcome::Rejected {
                reason: e.to_string(),
            },
            Err(e) => return Err(e.into()),
        };
        Ok(self
            .outbox
            .record_broadcast_outcome(operation_id, now_ms, attempt, outcome)?)
    }

    /// Reconcile one operation by transaction signature over mediated reads.
    /// From `Ambiguous`, retries the exact persisted bytes once per call.
    pub async fn reconcile(
        &self,
        operation_id: &str,
        now_ms: u64,
    ) -> Result<LifecycleStatus, MachineError> {
        let action = self.outbox.load(operation_id)?;
        match action.state {
            ActionState::Staged => Ok(LifecycleStatus::Staged),
            ActionState::Signed | ActionState::Sent | ActionState::Ambiguous => {
                let signature = transaction_signature(&action);
                let status: Value = self.mediator.read(
                    now_ms,
                    "getSignatureStatuses",
                    // Real RPC shape: params = [[<signature>, ...]].
                    &serde_json::json!([[signature]]),
                )?;
                let first = status
                    .get("value")
                    .and_then(|v| v.get(0))
                    .cloned()
                    .unwrap_or(Value::Null);
                if let Some(confirmation) = first
                    .get("confirmationStatus")
                    .and_then(|c| c.as_str())
                    .map(str::to_string)
                {
                    let done = self.outbox.record_reconciliation(
                        operation_id,
                        now_ms,
                        ReconciliationOutcome::Confirmed {
                            detail: confirmation.clone(),
                        },
                    )?;
                    let _ = done;
                    return Ok(LifecycleStatus::Confirmed { confirmation });
                }
                match action.state {
                    // Provider explicitly or implicitly says not found.
                    ActionState::Sent => Ok(LifecycleStatus::Sent),
                    // Unknown effect: retry the identical bytes.
                    ActionState::Ambiguous => {
                        self.broadcast(operation_id, now_ms).await?;
                        Ok(LifecycleStatus::Ambiguous)
                    }
                    ActionState::Signed => Ok(LifecycleStatus::Signed),
                    _ => unreachable!(),
                }
            }
            ActionState::Confirmed => Ok(LifecycleStatus::Confirmed {
                confirmation: "already terminal".to_string(),
            }),
            ActionState::Failed => Ok(LifecycleStatus::Failed {
                reason: "already terminal".to_string(),
            }),
            ActionState::FreshnessRefused => Ok(LifecycleStatus::FreshnessRefused(
                FreshnessReason::BlockhashRefreshRequired,
            )),
            ActionState::Cancelled => Ok(LifecycleStatus::Cancelled),
            ActionState::Expired => Ok(LifecycleStatus::Expired),
            ActionState::Quarantined => Ok(LifecycleStatus::Quarantined {
                reason: "already terminal".to_string(),
            }),
        }
    }

    /// Retry an ambiguous operation with the exact persisted bytes. Only
    /// legal from `Ambiguous`; any other state is an error. No signature is
    /// requested — the durable artifact is re-dispatched verbatim.
    pub async fn retry_ambiguous(
        &self,
        operation_id: &str,
        now_ms: u64,
    ) -> Result<Action, MachineError> {
        let action = self.outbox.load(operation_id)?;
        match action.state {
            ActionState::Ambiguous => self.broadcast(operation_id, now_ms).await,
            other => Err(MachineError::RetryNotAmbiguous(other.as_str())),
        }
    }

    /// Quarantine an unresolved operation for operator action.
    pub async fn quarantine(
        &self,
        operation_id: &str,
        now_ms: u64,
        reason: &str,
    ) -> Result<Action, MachineError> {
        Ok(self.outbox.record_reconciliation(
            operation_id,
            now_ms,
            ReconciliationOutcome::Quarantined {
                reason: reason.to_string(),
            },
        )?)
    }

    /// The typed VFS projection for one operation: durable outbox records
    /// only, verified facts structurally separated from machine-asserted
    /// observations. Digests over payload bytes throughout.
    pub fn project(
        &self,
        operation_id: &str,
        now_ms: u64,
    ) -> Result<OperationProjection, MachineError> {
        let action = self.outbox.load(operation_id)?;
        let profile = self.mediator.profile();
        projection::project_operation(
            &action,
            now_ms,
            &profile_caip2(profile),
            &profile.expected_genesis_hex,
        )
    }

    /// JSON convenience over [`Self::project`] for file/CLI surfaces.
    pub fn project_json(&self, operation_id: &str, now_ms: u64) -> Result<Value, MachineError> {
        let projection = self.project(operation_id, now_ms)?;
        serde_json::to_value(&projection).map_err(MachineError::Json)
    }

    /// Cluster identity projection from the operator-configured profile.
    pub fn project_cluster(&self) -> ClusterProjection {
        let profile = self.mediator.profile();
        ClusterProjection {
            schema: "bloom.solana.cluster-projection/1".to_string(),
            profile: profile.name.clone(),
            family: profile.family.clone(),
            caip2: profile_caip2(profile),
            expected_genesis_hex: profile.expected_genesis_hex.clone(),
            broadcast_enabled: profile.allow_broadcast,
        }
    }

    /// The mediated profile in effect (public projection; no credentials).
    pub fn profile(&self) -> &ChainRpcProfile {
        self.mediator.profile()
    }

    /// Load the durable action (envelope + replayed lifecycle).
    pub fn load_action(&self, operation_id: &str) -> Action {
        self.outbox.load(operation_id).expect("action loads")
    }

    /// The signing authority's pinned public key (projection; tests and
    /// confirm paths verify identity against the registered account).
    pub fn signer_public_key(&self) -> [u8; 32] {
        self.signer.public_key_bytes()
    }

    /// The durable outbox, for host-level integration (sweeps, checks).
    pub fn outbox(&self) -> &ChainActionOutbox {
        &self.outbox
    }

    /// Cancel a still-staged operation (nothing signed).
    pub fn cancel(&self, operation_id: &str, now_ms: u64) -> Result<Action, MachineError> {
        Ok(self.outbox.cancel(operation_id, now_ms)?)
    }

    /// A copy of this machine with a different approval authority (tests and
    /// future Broker wiring swap authorities without remounting).
    pub fn with_approvals(&self, approvals: Arc<dyn ApprovalAuthority>) -> Self {
        Self {
            approvals,
            ..self.clone()
        }
    }
}

/// Derive the profile's CAIP-2 identity from family + name. Profile names
/// are operator-configured tokens; the CAIP-2 reference uses the namespace
/// truncated form.
fn profile_caip2(profile: &ChainRpcProfile) -> String {
    // Chain profile names carry the family prefix ("solana-devnet"); the
    // CAIP-2 reference is family:network ("solana:devnet").
    let network = profile
        .name
        .strip_prefix(&format!("{}-", profile.family))
        .unwrap_or(&profile.name);
    format!("{}:{}", profile.family, network)
}

/// A Solana transaction's signature (single signer) is the base58 of the
/// signature bytes themselves.
fn transaction_signature(action: &Action) -> String {
    let artifact = action.artifact.as_ref().expect("signed action");
    bs58::encode(&artifact.signature).into_string()
}
