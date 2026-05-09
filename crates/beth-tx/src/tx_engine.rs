//! Tx engine: turn a parsed RawIntent into a StagedTx, simulate it,
//! then on confirm sign and broadcast. Also handles same-nonce
//! replacement / cancel txs and a legacy (non-1559) build path.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use alloy::network::EthereumWallet;
use alloy::network::{NetworkTransactionBuilder, TransactionBuilder};
use alloy::primitives::{Address, Bytes, U256};
use alloy::rpc::types::eth::TransactionRequest;
use alloy::signers::local::PrivateKeySigner;
use beth_chain::{ChainClient, ChainError};
use beth_proto::{
    parse_amount, parse_eth, parse_units, AddressBook, ChainSpec, Policy, RawIntent, RawIntentBody,
    StagedTx, TokenRef, TxStatus,
};
use parking_lot::RwLock;
use thiserror::Error;
use tracing::{debug, info};

use crate::intent_parser::ParseError;
use crate::outbox::{Outbox, OutboxError, OutboxState};
use crate::policy_engine;

/// Pluggable name resolver. Implemented by an ENS adapter outside the
/// engine to keep beth-tx free of beth-ens dependency.
#[async_trait::async_trait]
pub trait RecipientResolver: Send + Sync {
    async fn resolve_name(&self, name: &str) -> Result<Address, String>;
}

#[derive(Debug, Error)]
pub enum TxEngineError {
    #[error("parse: {0}")]
    Parse(#[from] ParseError),
    #[error("chain: {0}")]
    Chain(#[from] ChainError),
    #[error("outbox: {0}")]
    Outbox(#[from] OutboxError),
    #[error("address: {0}")]
    Address(String),
    #[error("amount: {0}")]
    Amount(String),
    #[error("policy denied")]
    PolicyDenied,
    #[error("broadcast disabled for chain '{0}' (set allow_broadcast=true)")]
    BroadcastDisabled(String),
    #[error("not yet implemented: {0}")]
    Unimplemented(String),
    #[error("signer: {0}")]
    Signer(String),
    #[error("token: {0}")]
    Token(String),
}

/// In-memory cache for ERC-20 metadata keyed by `(chain_id, address)`.
type TokenCache = Arc<RwLock<HashMap<(u64, Address), TokenMeta>>>;

#[derive(Debug, Clone)]
struct TokenMeta {
    address: Address,
    symbol: String,
    decimals: u8,
}

/// Stage / confirm the lifecycle.
#[derive(Clone)]
pub struct TxEngine {
    pub outbox: Outbox,
    /// Default stage TTL in ms.
    pub stage_ttl_ms: u128,
    /// Mainnet broadcast kill-switch.
    pub block_mainnet_broadcast: bool,
    token_cache: TokenCache,
    resolver: Option<Arc<dyn RecipientResolver>>,
}

impl TxEngine {
    pub fn new(outbox: Outbox, stage_ttl_ms: u128, block_mainnet_broadcast: bool) -> Self {
        Self {
            outbox,
            stage_ttl_ms,
            block_mainnet_broadcast,
            token_cache: Arc::new(RwLock::new(HashMap::new())),
            resolver: None,
        }
    }

    /// Wire a name resolver (typically an ENS adapter) for recipients.
    pub fn with_resolver(mut self, resolver: Arc<dyn RecipientResolver>) -> Self {
        self.resolver = Some(resolver);
        self
    }

    /// Resolve recipient from an intent (`0xabc`, alias, or ENS name).
    async fn resolve_recipient_async(
        &self,
        to: &str,
        book: Option<&AddressBook>,
    ) -> Result<Address, TxEngineError> {
        if to.starts_with("0x") {
            return to
                .parse::<Address>()
                .map_err(|e| TxEngineError::Address(e.to_string()));
        }
        if let Some(b) = book {
            if let Some(addr) = b.resolve(to) {
                return Ok(addr);
            }
        }
        if to.ends_with(".eth") {
            if let Some(r) = &self.resolver {
                return r
                    .resolve_name(to)
                    .await
                    .map_err(|e| TxEngineError::Address(format!("ens '{to}': {e}")));
            }
            return Err(TxEngineError::Unimplemented(format!(
                "ENS resolution for '{}' (no resolver wired)",
                to
            )));
        }
        Err(TxEngineError::Address(format!(
            "unresolved recipient '{}'",
            to
        )))
    }

    /// Resolve a value+token string into wei (when token is the native
    /// asset). Returns `Ok(None)` when the token is non-native and the
    /// caller should route through the ERC-20 path.
    fn resolve_native_value(
        value: &str,
        token: &Option<String>,
    ) -> Result<Option<U256>, TxEngineError> {
        match token.as_deref() {
            Some(t) => match t.to_ascii_lowercase().as_str() {
                "eth" | "ether" | "wei" | "gwei" => Ok(Some(
                    parse_eth(value).map_err(|e| TxEngineError::Amount(e.to_string()))?,
                )),
                _ => Ok(None),
            },
            None => {
                if value.is_empty() {
                    Ok(Some(U256::ZERO))
                } else {
                    Ok(Some(
                        parse_eth(value).map_err(|e| TxEngineError::Amount(e.to_string()))?,
                    ))
                }
            }
        }
    }

    /// Resolve an ERC-20 token symbol or 0x address into a concrete
    /// contract address on the given chain.
    fn resolve_token_address(
        token: &str,
        chain_id: u64,
    ) -> Result<(Address, String), TxEngineError> {
        let t = token.trim();
        if t.starts_with("0x") || t.starts_with("0X") {
            let addr: Address = t
                .parse()
                .map_err(|e: alloy::hex::FromHexError| TxEngineError::Token(e.to_string()))?;
            return Ok((addr, t.to_string()));
        }
        let upper = t.to_ascii_uppercase();
        if let Some(addr_str) = lookup_known_token(chain_id, &upper) {
            let addr: Address = addr_str.parse().map_err(|_| {
                TxEngineError::Token(format!("invalid hardcoded token addr for {upper}"))
            })?;
            return Ok((addr, upper));
        }
        Err(TxEngineError::Token(format!(
            "unknown token '{token}' on chain id {chain_id}"
        )))
    }

    /// Read the ERC-20 metadata for `addr`, caching the result.
    async fn token_meta(
        &self,
        chain: &ChainClient,
        addr: Address,
        symbol_hint: &str,
    ) -> Result<TokenMeta, TxEngineError> {
        let chain_id = chain.chain_id().await?;
        let key = (chain_id, addr);
        if let Some(m) = self.token_cache.read().get(&key).cloned() {
            return Ok(m);
        }
        let decimals = chain.erc20_decimals(addr).await?.ok_or_else(|| {
            TxEngineError::Token(format!(
                "could not read decimals() from {} (not an ERC-20?)",
                beth_proto::checksum_address(&addr)
            ))
        })?;
        let symbol = if symbol_hint.starts_with("0x") || symbol_hint.starts_with("0X") {
            short_addr_label(&addr)
        } else {
            symbol_hint.to_ascii_uppercase()
        };
        let meta = TokenMeta {
            address: addr,
            symbol,
            decimals,
        };
        self.token_cache.write().insert(key, meta.clone());
        Ok(meta)
    }

    /// Stage a tx for a wallet on a chain. The caller is responsible for
    /// looking up the wallet's address.
    pub async fn stage(
        &self,
        wallet: &str,
        from: Address,
        intent: RawIntent,
        chain: &ChainClient,
        policy: &Policy,
        address_book: Option<&AddressBook>,
    ) -> Result<StagedTx, TxEngineError> {
        let spec: &ChainSpec = chain.spec();
        let chain_id = chain.chain_id().await?;

        // (to, value_wei, data_hex, optional token metadata for plan)
        let (to, value_wei, data_hex, token_for_plan): (Address, U256, String, Option<TokenRef>) =
            match &intent.body {
                RawIntentBody::Send {
                    to,
                    value,
                    token,
                    data,
                } => {
                    let to_addr = self.resolve_recipient_async(to, address_book).await?;
                    if let Some(v) = Self::resolve_native_value(value, token)? {
                        // Native send.
                        let data = data.clone().unwrap_or_else(|| "0x".into());
                        (to_addr, v, data, None)
                    } else {
                        // ERC-20 path.
                        let token_str = token.as_deref().unwrap_or("");
                        let (token_addr, sym_hint) =
                            Self::resolve_token_address(token_str, chain_id)?;
                        let meta = self.token_meta(chain, token_addr, &sym_hint).await?;
                        let parsed = parse_amount(value)
                            .map_err(|e| TxEngineError::Amount(e.to_string()))?;
                        let amount = parse_units(&parsed.number, meta.decimals)
                            .map_err(|e| TxEngineError::Amount(e.to_string()))?;
                        let calldata = beth_tools::encode_call(
                            "transfer(address,uint256)",
                            &serde_json::json!([
                                beth_proto::checksum_address(&to_addr),
                                amount.to_string(),
                            ]),
                        )
                        .map_err(|e| TxEngineError::Amount(format!("encode_call: {e}")))?;
                        let token_ref = TokenRef {
                            address: beth_proto::checksum_address(&meta.address),
                            symbol: meta.symbol.clone(),
                            decimals: meta.decimals,
                            recipient: beth_proto::checksum_address(&to_addr),
                            amount: parsed.number.clone(),
                        };
                        (token_addr, U256::ZERO, calldata, Some(token_ref))
                    }
                }
                RawIntentBody::Raw { to, value, data } => {
                    let to_addr = self.resolve_recipient_async(to, address_book).await?;
                    let v = if value.is_empty() {
                        U256::ZERO
                    } else {
                        parse_eth(value).map_err(|e| TxEngineError::Amount(e.to_string()))?
                    };
                    (to_addr, v, data.clone(), None)
                }
                RawIntentBody::Call {
                    contract,
                    method,
                    args,
                    value,
                } => {
                    let contract_addr =
                        self.resolve_recipient_async(contract, address_book).await?;
                    let v = if value.is_empty() {
                        U256::ZERO
                    } else {
                        parse_eth(value).map_err(|e| TxEngineError::Amount(e.to_string()))?
                    };
                    let data = beth_tools::encode_call(method, &serde_json::json!(args))
                        .map_err(|e| TxEngineError::Amount(format!("encode_call: {e}")))?;
                    (contract_addr, v, data, None)
                }
                RawIntentBody::Enso { .. } => {
                    return Err(TxEngineError::Unimplemented(
                        "Enso intents flow through beth-defi (not in v1 stage path)".into(),
                    ));
                }
            };

        // Build a request to estimate gas; choose 1559 vs legacy fields.
        let data_bytes = decode_data(&data_hex)?;
        let nonce = match intent.nonce {
            Some(n) => n,
            None => chain.nonce(from).await?,
        };
        let gas_price = chain.gas_price().await.unwrap_or(1_000_000_000);
        let max_fee = gas_price.saturating_mul(2);
        let prio = (gas_price / 10).max(1);

        let mut req = TransactionRequest::default()
            .with_from(from)
            .with_to(to)
            .with_value(value_wei)
            .with_input(data_bytes.clone())
            .with_nonce(nonce)
            .with_chain_id(chain_id);
        if spec.legacy_tx {
            req = req.with_gas_price(gas_price);
        } else {
            req = req
                .with_max_fee_per_gas(max_fee)
                .with_max_priority_fee_per_gas(prio);
        }
        let gas_limit = match chain.estimate_gas(&req).await {
            Ok(g) => {
                // Add a 25% buffer; estimates can run short under load.
                let buffered = g.saturating_mul(125) / 100;
                buffered.max(21_000)
            }
            Err(e) => {
                tracing::warn!(error = %e, "estimate_gas failed; using 500k fallback");
                500_000
            }
        };

        let now_ms = now_ms();
        let (max_fee_field, prio_field, gas_price_field) = if spec.legacy_tx {
            (None, None, Some(gas_price.to_string()))
        } else {
            (Some(max_fee.to_string()), Some(prio.to_string()), None)
        };

        // For policy evaluation, decide what addresses are involved:
        //   - native send: contract=None,    token=None,         recipient=to
        //   - erc20 send:  contract=token,   token=token,        recipient=token_for_plan.recipient
        //   - call/raw:    contract=to,      token=None,         recipient=to (best-effort)
        //
        // `destination_is_contract` drives the legacy `[contracts]` block
        // from the spec: a plain native send to an EOA bypasses those
        // checks. For native sends with a non-empty data field or where
        // the destination has bytecode we still flag it as a contract
        // call; the heuristic mirrors the spec's "code length > 0 OR data
        // is non-empty" rule.
        let mut policy_ctx = policy_engine::AddressContext::default();
        match &intent.body {
            RawIntentBody::Send { .. } => {
                if let Some(t) = &token_for_plan {
                    policy_ctx.token = Some(to);
                    policy_ctx.contract = Some(to);
                    policy_ctx.destination_is_contract = true;
                    policy_ctx.token_symbol = Some(t.symbol.clone());
                    if let Ok(rec) = t.recipient.parse::<Address>() {
                        policy_ctx.recipient = Some(rec);
                    }
                } else {
                    policy_ctx.recipient = Some(to);
                    // Native send: if data is non-empty or the destination
                    // has bytecode, treat as contract call.
                    let data_nonempty = !data_bytes.is_empty();
                    let to_has_code = chain.code(to).await.map(|c| !c.is_empty()).unwrap_or(false);
                    policy_ctx.destination_is_contract = data_nonempty || to_has_code;
                    if policy_ctx.destination_is_contract {
                        policy_ctx.contract = Some(to);
                    }
                }
            }
            RawIntentBody::Call { .. } | RawIntentBody::Raw { .. } => {
                policy_ctx.contract = Some(to);
                policy_ctx.recipient = Some(to);
                policy_ctx.destination_is_contract = true;
            }
            RawIntentBody::Enso { .. } => {}
        }
        // TODO(policy): wire a price oracle so usd_value is populated for
        // non-zero-value txs. Until then USD caps fire a single Warn check
        // that surfaces the missing oracle in plan.md, instead of silently
        // passing dollar-denominated rules.
        policy_ctx.usd_value = None;

        let mut staged = StagedTx {
            id: self.outbox.allocate_id(),
            wallet: wallet.to_string(),
            chain: spec.name.clone(),
            chain_id,
            from: beth_proto::checksum_address(&from),
            to: beth_proto::checksum_address(&to),
            value_wei: value_wei.to_string(),
            data_hex: data_hex.clone(),
            gas_limit,
            max_fee_per_gas: max_fee_field,
            max_priority_fee_per_gas: prio_field,
            gas_price: gas_price_field,
            nonce,
            policy_checks: vec![],
            created_ms: now_ms,
            expires_ms: now_ms + self.stage_ttl_ms,
            status: TxStatus::Pending,
            tx_hash: None,
            token: token_for_plan,
        };
        staged.policy_checks = policy_engine::evaluate(
            policy,
            &spec.name,
            value_wei,
            spec.native_decimals,
            policy_ctx,
        );

        let plan =
            beth_proto::PlanRender::render(&staged, &spec.native_symbol, spec.native_decimals);
        self.outbox.write_pending(&staged, &plan)?;
        debug!(id=%staged.id, wallet=%staged.wallet, chain=%staged.chain, "tx.stage");
        Ok(staged)
    }

    /// Confirm and broadcast a staged tx. Caller decides whether the
    /// confirm content is "y" (normal) or the policy's override sentinel
    /// (bypass soft warns).
    ///
    /// Refuses any id that is not currently in `pending`: a stale path
    /// like `outbox/<wallet>/<chain>/pending/<sent-id>/confirm` cannot
    /// rebroadcast (fix #2). Refuses any pending entry whose `expires_ms`
    /// has passed (fix #3) — the caller should sweep expired and re-stage.
    #[allow(clippy::too_many_arguments)]
    pub async fn confirm(
        &self,
        wallet: &str,
        chain_name: &str,
        id: &str,
        chain: &ChainClient,
        signer: &PrivateKeySigner,
        policy: &Policy,
        confirm_text: &str,
    ) -> Result<StagedTx, TxEngineError> {
        let entry = self
            .outbox
            .read_in_state(wallet, chain_name, id, OutboxState::Pending)?;
        let mut staged = entry.staged.clone();

        // Expiry check: stage TTL is enforced regardless of whether the
        // sweeper has run yet. We use wall-clock here; sweep_expired is the
        // background mop-up that removes stale dirs.
        let now = now_ms();
        if staged.expires_ms != 0 && now >= staged.expires_ms {
            return Err(TxEngineError::Outbox(OutboxError::StagedExpired {
                id: staged.id.clone(),
                expired_at: staged.expires_ms,
                now,
            }));
        }

        // Policy gate.
        let hard = policy_engine::has_hard_violation(&staged.policy_checks);
        if hard {
            staged.status = TxStatus::Failed;
            self.outbox
                .transition(&entry, crate::outbox::OutboxState::Failed)?;
            return Err(TxEngineError::PolicyDenied);
        }
        let warn = policy_engine::has_warning(&staged.policy_checks);
        let sentinel = policy.override_sentinel();
        let override_text = confirm_text.trim().eq_ignore_ascii_case(sentinel);
        if warn && !override_text {
            return Err(TxEngineError::PolicyDenied);
        }

        // Broadcast gate: never broadcast to mainnet by default.
        let spec = chain.spec();
        let is_mainnet = beth_proto::Config::is_mainnet_id(spec.chain_id);
        if (self.block_mainnet_broadcast && is_mainnet) || !spec.allow_broadcast {
            return Err(TxEngineError::BroadcastDisabled(spec.name.clone()));
        }

        let tx_hash = self.broadcast(&staged, chain, signer).await?;
        info!(id=%staged.id, hash=%format!("{:#x}", tx_hash), "tx.broadcast");

        staged.status = TxStatus::Sent;
        staged.tx_hash = Some(format!("{:#x}", tx_hash));

        let new_dir = self
            .outbox
            .transition(&entry, crate::outbox::OutboxState::Sent)?;
        self.outbox.write_artefact(
            &new_dir,
            "intent.json",
            &serde_json::to_vec_pretty(&staged).unwrap(),
        )?;
        self.outbox.write_artefact(
            &new_dir,
            "tx_hash",
            staged.tx_hash.as_ref().unwrap().as_bytes(),
        )?;

        Ok(staged)
    }

    /// Build, sign and broadcast a single concrete `StagedTx`.
    async fn broadcast(
        &self,
        staged: &StagedTx,
        chain: &ChainClient,
        signer: &PrivateKeySigner,
    ) -> Result<alloy::primitives::B256, TxEngineError> {
        let to_addr: Address = staged
            .to
            .parse()
            .map_err(|e: alloy::hex::FromHexError| TxEngineError::Address(e.to_string()))?;
        let value: U256 = staged
            .value_wei
            .parse()
            .map_err(|_| TxEngineError::Amount("value_wei".into()))?;
        let data = decode_data(&staged.data_hex)?;

        let mut req = TransactionRequest::default()
            .with_from(signer.address())
            .with_to(to_addr)
            .with_value(value)
            .with_input(data)
            .with_nonce(staged.nonce)
            .with_chain_id(staged.chain_id)
            .with_gas_limit(staged.gas_limit);

        if chain.spec().legacy_tx {
            let gp: u128 = staged
                .gas_price
                .as_deref()
                .and_then(|s| s.parse::<u128>().ok())
                .unwrap_or(1_000_000_000);
            req = req.with_gas_price(gp);
        } else {
            let max_fee: u128 = staged
                .max_fee_per_gas
                .as_deref()
                .and_then(|s| s.parse::<u128>().ok())
                .unwrap_or(2_000_000_000);
            let prio: u128 = staged
                .max_priority_fee_per_gas
                .as_deref()
                .and_then(|s| s.parse::<u128>().ok())
                .unwrap_or(1_000_000);
            req = req
                .with_max_fee_per_gas(max_fee)
                .with_max_priority_fee_per_gas(prio);
        }

        let wallet_signer = EthereumWallet::from(signer.clone());
        let tx_envelope = req
            .build(&wallet_signer)
            .await
            .map_err(|e| TxEngineError::Signer(e.to_string()))?;
        let mut buf = Vec::new();
        alloy::eips::Encodable2718::encode_2718(&tx_envelope, &mut buf);
        let raw = Bytes::from(buf);
        Ok(chain.send_raw(raw).await?)
    }

    /// Issue a same-nonce replacement tx with bumped fees. The original
    /// must already be persisted in the outbox **and still in pending**;
    /// already-broadcast txs cannot be replaced through this path
    /// (fix #2 / #10). Floors `bump_pct` at 10 to satisfy the mempool's
    /// >= 10% rule.
    pub async fn replace(
        &self,
        wallet: &str,
        chain_name: &str,
        original_id: &str,
        chain: &ChainClient,
        signer: &PrivateKeySigner,
        bump_pct: u32,
    ) -> Result<StagedTx, TxEngineError> {
        let bump = bump_pct.max(10);
        let entry =
            self.outbox
                .read_in_state(wallet, chain_name, original_id, OutboxState::Pending)?;
        let original = &entry.staged;

        let mut bumped = original.clone();
        bumped.status = TxStatus::Pending;
        bumped.tx_hash = None;
        bump_fees_in_place(&mut bumped, bump);

        let tx_hash = self.broadcast(&bumped, chain, signer).await?;
        bumped.tx_hash = Some(format!("{:#x}", tx_hash));
        bumped.status = TxStatus::Sent;

        self.outbox.write_artefact(
            &entry.dir,
            "replacement_tx_hash",
            bumped.tx_hash.as_ref().unwrap().as_bytes(),
        )?;
        self.outbox.write_artefact(
            &entry.dir,
            "replacement_intent.json",
            &serde_json::to_vec_pretty(&bumped).unwrap(),
        )?;
        info!(
            id = %original.id,
            replacement = %bumped.tx_hash.as_deref().unwrap_or(""),
            "tx.replace"
        );
        Ok(bumped)
    }

    /// Issue a same-nonce self-send to cancel the original. Refuses if the
    /// original is no longer pending (fix #2 / #10).
    pub async fn cancel(
        &self,
        wallet: &str,
        chain_name: &str,
        original_id: &str,
        chain: &ChainClient,
        signer: &PrivateKeySigner,
        bump_pct: u32,
    ) -> Result<StagedTx, TxEngineError> {
        let bump = bump_pct.max(10);
        let entry =
            self.outbox
                .read_in_state(wallet, chain_name, original_id, OutboxState::Pending)?;
        let original = &entry.staged;

        let mut cancel_tx = original.clone();
        cancel_tx.status = TxStatus::Pending;
        cancel_tx.tx_hash = None;
        cancel_tx.to = beth_proto::checksum_address(&signer.address());
        cancel_tx.value_wei = "0".to_string();
        cancel_tx.data_hex = "0x".to_string();
        cancel_tx.token = None;
        bump_fees_in_place(&mut cancel_tx, bump);

        let tx_hash = self.broadcast(&cancel_tx, chain, signer).await?;
        cancel_tx.tx_hash = Some(format!("{:#x}", tx_hash));
        cancel_tx.status = TxStatus::Cancelled;

        self.outbox.write_artefact(
            &entry.dir,
            "cancel_tx_hash",
            cancel_tx.tx_hash.as_ref().unwrap().as_bytes(),
        )?;
        self.outbox.write_artefact(
            &entry.dir,
            "cancel_intent.json",
            &serde_json::to_vec_pretty(&cancel_tx).unwrap(),
        )?;
        if entry.state != OutboxState::Failed {
            let _ = self.outbox.transition(&entry, OutboxState::Failed);
        }
        info!(
            id = %original.id,
            cancel = %cancel_tx.tx_hash.as_deref().unwrap_or(""),
            "tx.cancel"
        );
        Ok(cancel_tx)
    }
}

/// Bump fee fields by `pct%` (rounded up by ≥ 1 wei). Whichever set is
/// populated — 1559 or legacy — gets bumped.
fn bump_fees_in_place(staged: &mut StagedTx, pct: u32) {
    fn bump_one(s: &Option<String>, pct: u32) -> Option<String> {
        s.as_deref().and_then(|x| {
            let v = x.parse::<u128>().ok()?;
            let bump = v.saturating_mul(pct as u128) / 100;
            let bumped = v.saturating_add(bump.max(1));
            Some(bumped.to_string())
        })
    }
    if let Some(b) = bump_one(&staged.max_fee_per_gas, pct) {
        staged.max_fee_per_gas = Some(b);
    }
    if let Some(b) = bump_one(&staged.max_priority_fee_per_gas, pct) {
        staged.max_priority_fee_per_gas = Some(b);
    }
    if let Some(b) = bump_one(&staged.gas_price, pct) {
        staged.gas_price = Some(b);
    }
}

fn decode_data(s: &str) -> Result<Bytes, TxEngineError> {
    let s = s.trim();
    if s.is_empty() || s == "0x" {
        return Ok(Bytes::new());
    }
    let s = s.strip_prefix("0x").unwrap_or(s);
    let v = hex::decode(s).map_err(|e| TxEngineError::Amount(format!("data: {e}")))?;
    Ok(Bytes::from(v))
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn short_addr_label(a: &Address) -> String {
    let s = format!("{a:#x}");
    if s.len() > 10 {
        format!("{}…{}", &s[..6], &s[s.len() - 4..])
    } else {
        s
    }
}

/// Hardcoded list of common ERC-20 addresses. Anvil mainnet forks share
/// chain id 31337 with vanilla Anvil — the lookup is best-effort and a
/// caller can always pass a 0x address explicitly.
fn lookup_known_token(chain_id: u64, symbol_upper: &str) -> Option<&'static str> {
    match (chain_id, symbol_upper) {
        (1, "USDC") | (31337, "USDC") => Some("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
        (1, "USDT") | (31337, "USDT") => Some("0xdAC17F958D2ee523a2206206994597C13D831ec7"),
        (1, "DAI") | (31337, "DAI") => Some("0x6B175474E89094C44Da98b954EedeAC495271d0F"),
        (1, "WETH") | (31337, "WETH") => Some("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        _ => None,
    }
}

const _PARSE_UNITS: fn(&str, u8) -> Result<U256, beth_proto::units::UnitError> = parse_units;

#[cfg(test)]
mod tests {
    use super::*;
    use beth_proto::TxStatus;

    fn fake_staged_1559(id: &str) -> StagedTx {
        StagedTx {
            id: id.into(),
            wallet: "alice".into(),
            chain: "anvil".into(),
            chain_id: 31337,
            from: "0x0000000000000000000000000000000000000001".into(),
            to: "0x0000000000000000000000000000000000000002".into(),
            value_wei: "0".into(),
            data_hex: "0x".into(),
            gas_limit: 21000,
            max_fee_per_gas: Some("100".into()),
            max_priority_fee_per_gas: Some("10".into()),
            gas_price: None,
            nonce: 0,
            policy_checks: vec![],
            created_ms: 0,
            expires_ms: 0,
            status: TxStatus::Pending,
            tx_hash: None,
            token: None,
        }
    }

    #[test]
    fn bump_fees_1559_at_least_10pct() {
        let mut s = fake_staged_1559("a");
        bump_fees_in_place(&mut s, 15);
        // 100 * 1.15 = 115; integer math: 100 + (100*15/100) = 115.
        assert_eq!(s.max_fee_per_gas.as_deref(), Some("115"));
        // 10 * 1.15 = 11.5 — integer math: 10 + (10*15/100=1) = 11.
        // But our bump-one floors the bump at 1 wei.
        assert_eq!(s.max_priority_fee_per_gas.as_deref(), Some("11"));
    }

    #[test]
    fn bump_fees_legacy_path() {
        let mut s = fake_staged_1559("a");
        s.max_fee_per_gas = None;
        s.max_priority_fee_per_gas = None;
        s.gas_price = Some("1000".into());
        bump_fees_in_place(&mut s, 12);
        // 1000 + 1000*12/100 = 1120.
        assert_eq!(s.gas_price.as_deref(), Some("1120"));
    }

    #[test]
    fn lookup_usdc_mainnet() {
        let addr = lookup_known_token(1, "USDC").unwrap();
        assert!(addr.to_ascii_lowercase().starts_with("0xa0b86991"));
    }

    #[test]
    fn resolve_token_address_via_hex() {
        let (a, sym) =
            TxEngine::resolve_token_address("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48", 1)
                .unwrap();
        assert_eq!(
            format!("{a:#x}"),
            "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"
        );
        assert!(sym.starts_with("0x"));
    }

    #[test]
    fn resolve_token_unknown_symbol_errors() {
        let err = TxEngine::resolve_token_address("MOCK", 1).unwrap_err();
        assert!(matches!(err, TxEngineError::Token(_)));
    }

    /// Helpers shared by the confirm-flow regression tests below. They
    /// build a self-contained TxEngine + Outbox + ChainClient pointing at
    /// an unreachable URL — every test below must fail (expectedly!)
    /// before any chain call is attempted, otherwise the assertion
    /// becomes "could not connect" and you can't tell which gate the
    /// confirm flow is meant to be honouring.
    fn fake_engine(stage_ttl_ms: u128) -> (TxEngine, beth_proto::ChainSpec, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let outbox = crate::outbox::Outbox::new(dir.path()).unwrap();
        let engine = TxEngine::new(outbox, stage_ttl_ms, false);
        let spec = beth_proto::ChainSpec {
            name: "anvil".into(),
            chain_id: 31337,
            // Unreachable URL — confirms that fail before broadcast must
            // not depend on this being reachable.
            rpc_urls: vec!["http://127.0.0.1:1".into()],
            allow_broadcast: true,
            etherscan_api_url: None,
            display_name: None,
            native_symbol: "ETH".into(),
            native_decimals: 18,
            legacy_tx: false,
        };
        (engine, spec, dir)
    }

    /// Fix #2: writing `pending/<sent-id>/confirm` must not rebroadcast
    /// — the engine must refuse to confirm an id that no longer lives in
    /// `pending`.
    #[tokio::test]
    async fn confirm_rejects_id_already_in_sent() {
        let (engine, spec, _dir) = fake_engine(60_000);
        let chain = beth_chain::ChainClient::new(spec.clone()).unwrap();
        let signer = alloy::signers::local::PrivateKeySigner::random();
        let policy = beth_proto::Policy::default();

        // Stage manually (write_pending) so we don't need a live RPC.
        let mut staged = fake_staged_1559("0001-test");
        staged.expires_ms = now_ms() + 60_000;
        engine.outbox.write_pending(&staged, "p").unwrap();
        // Move it to sent to simulate a stale path that targets the wrong
        // state.
        let entry = engine.outbox.read("alice", "anvil", "0001-test").unwrap();
        engine
            .outbox
            .transition(&entry, crate::outbox::OutboxState::Sent)
            .unwrap();

        let r = engine
            .confirm("alice", "anvil", "0001-test", &chain, &signer, &policy, "y")
            .await;
        match r {
            Err(TxEngineError::Outbox(OutboxError::StateMismatch { actual, .. })) => {
                assert_eq!(actual, "sent");
            }
            other => panic!("expected StateMismatch, got {other:?}"),
        }
    }

    /// Fix #3: confirm must reject a pending entry whose stage TTL has
    /// expired. The check fires before broadcast, so the (unreachable)
    /// chain URL is never touched.
    #[tokio::test]
    async fn confirm_rejects_expired_stage() {
        let (engine, spec, _dir) = fake_engine(60_000);
        let chain = beth_chain::ChainClient::new(spec.clone()).unwrap();
        let signer = alloy::signers::local::PrivateKeySigner::random();
        let policy = beth_proto::Policy::default();

        let mut staged = fake_staged_1559("0001-test");
        // Already expired the moment this test runs.
        staged.expires_ms = 1;
        engine.outbox.write_pending(&staged, "p").unwrap();

        let r = engine
            .confirm("alice", "anvil", "0001-test", &chain, &signer, &policy, "y")
            .await;
        match r {
            Err(TxEngineError::Outbox(OutboxError::StagedExpired { id, .. })) => {
                assert_eq!(id, "0001-test");
            }
            other => panic!("expected StagedExpired, got {other:?}"),
        }
    }

    /// Fix #11: the override sentinel comes from policy.toml, not a hard
    /// "override" string. A custom token must be honoured.
    #[tokio::test]
    async fn confirm_uses_policy_override_token() {
        use beth_proto::policy::{PolicyAutomation, PolicyCaps, PolicyCheck, PolicyOutcome};

        let (engine, spec, _dir) = fake_engine(60_000);
        let chain = beth_chain::ChainClient::new(spec.clone()).unwrap();
        let signer = alloy::signers::local::PrivateKeySigner::random();

        // Soft-warn on tx, with a non-default override token.
        let policy = beth_proto::Policy {
            automation: PolicyAutomation {
                override_token: Some("yolo".into()),
                ..Default::default()
            },
            caps: PolicyCaps::default(),
            ..Default::default()
        };

        let mut staged = fake_staged_1559("0001-test");
        staged.expires_ms = now_ms() + 60_000;
        // Inject a Warn check directly so the override gate fires.
        staged.policy_checks = vec![PolicyCheck {
            rule: "test.warn".into(),
            outcome: PolicyOutcome::Warn,
            message: "soft cap".into(),
        }];
        engine.outbox.write_pending(&staged, "p").unwrap();

        // "y" must be rejected — needs the policy's override token.
        let r = engine
            .confirm("alice", "anvil", "0001-test", &chain, &signer, &policy, "y")
            .await;
        assert!(matches!(r, Err(TxEngineError::PolicyDenied)));

        // Default sentinel ("override") must NOT bypass when policy
        // overrides it.
        let r = engine
            .confirm(
                "alice",
                "anvil",
                "0001-test",
                &chain,
                &signer,
                &policy,
                "override",
            )
            .await;
        assert!(matches!(r, Err(TxEngineError::PolicyDenied)));

        // The configured token gets past the policy gate; the next gate
        // is broadcast, which fails on the unreachable RPC. We treat any
        // *non*-PolicyDenied error as the policy gate having let us
        // through.
        let r = engine
            .confirm(
                "alice",
                "anvil",
                "0001-test",
                &chain,
                &signer,
                &policy,
                "yolo",
            )
            .await;
        match r {
            Err(TxEngineError::PolicyDenied) => panic!("override token did not bypass warn"),
            Ok(_) => panic!("unexpected broadcast success on unreachable RPC"),
            Err(_) => {}
        }
    }
}
