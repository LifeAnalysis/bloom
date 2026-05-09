//! `defi/intents/<wallet>/...` — Enso-mediated DeFi intent surface.
//!
//! Writes go through the wallet's normal stage→confirm pipeline; this
//! handler is an "intent compiler" that turns natural-language or JSON
//! Enso intents into a concrete `RawIntent::Raw` and forwards confirms
//! to [`TxEngine::stage`]. The actual broadcast still happens via the
//! wallet outbox.
//!
//! Paths handled:
//! - `defi/`                                        — `[ "intents" ]`
//! - `defi/intents/`                                — wallets with sessions
//! - `defi/intents/<wallet>/`                       — `new` + session ids
//! - `defi/intents/<wallet>/new`                    — write to begin a session
//! - `defi/intents/<wallet>/<session>/intent.txt`   — original intent
//! - `defi/intents/<wallet>/<session>/route.json`   — full Enso response
//! - `defi/intents/<wallet>/<session>/plan.md`      — human narrative
//! - `defi/intents/<wallet>/<session>/tx.json`      — prepared RawIntent
//! - `defi/intents/<wallet>/<session>/simulation.json` — eth_call sim
//! - `defi/intents/<wallet>/<session>/confirm`      — write to stage into outbox
//!
//! Sessions live in memory (RwLock<HashMap<id, DefiSession>>) only; they
//! evaporate on daemon restart by design — the staged outbox entry is
//! the durable artefact.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use alloy::primitives::Address;
#[cfg(test)]
use alloy::primitives::U256;
use alloy::rpc::types::eth::TransactionRequest;
use async_trait::async_trait;
use beth_chain::{ChainClient, ChainRegistry};
use beth_defi::{
    parse_natural_intent, resolve_token_symbol, EnsoClient, EnsoError, RouteRequest, RouteResponse,
    RoutingStrategy,
};
use beth_keystore::Keystore;
use beth_proto::{checksum_address, AddressBook, RawIntent, RawIntentBody, StagedTx};
use beth_tx::tx_engine::TxEngine;
use parking_lot::RwLock;
use serde::Deserialize;

use crate::handler::{Entry, Handler, HandlerError};
use crate::path::VfsPath;

/// Cached session: original intent, the Enso route response, and the
/// derived RawIntent we'd hand to [`TxEngine::stage`] on confirm.
#[derive(Debug, Clone)]
pub struct DefiSession {
    pub id: String,
    pub wallet: String,
    pub chain: String,
    pub intent_text: String,
    pub route: Option<RouteResponse>,
    pub plan_md: String,
    pub tx_intent: Option<RawIntent>,
    pub staged_id: Option<String>,
    pub created_ms: u128,
}

/// Body of `new` writes — accepts either a JSON `{intent, chain}` or
/// a single-line natural-language string.
#[derive(Debug, Clone, Deserialize)]
struct NewIntentBody {
    #[serde(default)]
    #[allow(dead_code)]
    kind: Option<String>,
    intent: String,
    #[serde(default)]
    chain: Option<String>,
    #[serde(default)]
    slippage_bps: Option<u16>,
}

#[derive(Clone)]
pub struct DefiHandler {
    enso: EnsoClient,
    chains: ChainRegistry,
    keystore: Keystore,
    tx_engine: TxEngine,
    address_book: Arc<AddressBook>,
    sessions: Arc<RwLock<HashMap<String, DefiSession>>>,
    /// Default chain when an intent omits one.
    default_chain: String,
    next_id: Arc<RwLock<u64>>,
}

impl DefiHandler {
    pub fn new(
        enso: EnsoClient,
        chains: ChainRegistry,
        keystore: Keystore,
        tx_engine: TxEngine,
        address_book: Arc<AddressBook>,
    ) -> Self {
        Self {
            enso,
            chains,
            keystore,
            tx_engine,
            address_book,
            sessions: Arc::new(RwLock::new(HashMap::new())),
            default_chain: "ethereum".into(),
            next_id: Arc::new(RwLock::new(1)),
        }
    }

    pub fn with_default_chain(mut self, chain: impl Into<String>) -> Self {
        self.default_chain = chain.into();
        self
    }

    fn allocate_id(&self) -> String {
        let mut g = self.next_id.write();
        let n = *g;
        *g = n.wrapping_add(1);
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() % 100_000)
            .unwrap_or(0);
        format!("{:04}-{:05}", n, suffix)
    }

    fn session_key(wallet: &str, id: &str) -> String {
        format!("{wallet}/{id}")
    }

    fn parse_new_body(body: &str) -> Result<NewIntentBody, HandlerError> {
        let s = body.trim();
        if s.is_empty() {
            return Err(HandlerError::invalid("empty intent body"));
        }
        if s.starts_with('{') {
            let v: NewIntentBody =
                serde_json::from_str(s).map_err(|e| HandlerError::invalid(format!("json: {e}")))?;
            if v.intent.trim().is_empty() {
                return Err(HandlerError::invalid("missing 'intent' field"));
            }
            Ok(v)
        } else {
            Ok(NewIntentBody {
                kind: Some("enso".into()),
                intent: s.to_string(),
                chain: None,
                slippage_bps: None,
            })
        }
    }

    fn list_session_wallets(&self) -> Vec<String> {
        let mut s: Vec<String> = self
            .sessions
            .read()
            .values()
            .map(|sess| sess.wallet.clone())
            .collect();
        s.sort();
        s.dedup();
        s
    }

    fn list_sessions_for_wallet(&self, wallet: &str) -> Vec<String> {
        let mut out: Vec<String> = self
            .sessions
            .read()
            .values()
            .filter(|s| s.wallet == wallet)
            .map(|s| s.id.clone())
            .collect();
        out.sort();
        out
    }

    fn get_session(&self, wallet: &str, id: &str) -> Result<DefiSession, HandlerError> {
        let key = Self::session_key(wallet, id);
        self.sessions
            .read()
            .get(&key)
            .cloned()
            .ok_or_else(|| HandlerError::not_found(format!("session {wallet}/{id}")))
    }

    fn put_session(&self, sess: DefiSession) {
        let key = Self::session_key(&sess.wallet, &sess.id);
        self.sessions.write().insert(key, sess);
    }

    fn chain_client(&self, name: &str) -> Result<ChainClient, HandlerError> {
        self.chains
            .get(name)
            .ok_or_else(|| HandlerError::not_found(format!("chain '{name}'")))
    }

    /// Build a RouteRequest from a natural-language intent.
    async fn build_route_request(
        chain: &ChainClient,
        chain_id: u64,
        from: Address,
        intent: &str,
    ) -> Result<RouteRequest, HandlerError> {
        let nat = parse_natural_intent(intent).ok_or_else(|| {
            HandlerError::invalid(format!(
                "could not parse intent '{intent}' (expected `swap <amount> <tok> to <tok>`)"
            ))
        })?;
        // For raw integer amounts against a hex token, prefer the raw value
        // verbatim — that matches our balance views. Otherwise look up
        // decimals: known symbols come from the static table; unknown hex
        // addresses go through an on-chain decimals() call so users can
        // specify amounts in human units (e.g. "1.5 0xabc...").
        let is_hex = nat.token_in.starts_with("0x") || nat.token_in.starts_with("0X");
        let decimals = if is_hex {
            if !nat.amount.contains('.') {
                0
            } else {
                let token_in = resolve_token_symbol(chain_id, &nat.token_in).ok_or_else(|| {
                    HandlerError::invalid(format!("unknown token '{}'", nat.token_in))
                })?;
                chain
                    .erc20_decimals(token_in)
                    .await
                    .map_err(|e| HandlerError::backend(e.to_string()))?
                    .ok_or_else(|| {
                        HandlerError::backend(format!(
                            "could not read decimals for {}",
                            checksum_address(&token_in)
                        ))
                    })?
            }
        } else {
            decimals_for_symbol(chain_id, &nat.token_in)
        };
        Self::compose_route_request(chain_id, from, &nat, decimals)
    }

    /// Pure builder: turns a parsed intent + known decimals into a RouteRequest.
    /// Split out so unit tests can exercise the symbol path without an RPC.
    fn compose_route_request(
        chain_id: u64,
        from: Address,
        nat: &beth_defi::NaturalIntent,
        decimals_in: u8,
    ) -> Result<RouteRequest, HandlerError> {
        let token_in = resolve_token_symbol(chain_id, &nat.token_in)
            .ok_or_else(|| HandlerError::invalid(format!("unknown token '{}'", nat.token_in)))?;
        let token_out = resolve_token_symbol(chain_id, &nat.token_out)
            .ok_or_else(|| HandlerError::invalid(format!("unknown token '{}'", nat.token_out)))?;
        let amount = beth_proto::parse_units(&nat.amount, decimals_in)
            .map_err(|e| HandlerError::invalid(format!("amount: {e}")))?;
        Ok(RouteRequest {
            from_address: from,
            chain_id,
            token_in,
            token_out,
            amount_in: amount,
            slippage_bps: 50,
            routing_strategy: Some(RoutingStrategy::Router),
            receiver: None,
        })
    }

    async fn create_session(
        &self,
        wallet: &str,
        body: NewIntentBody,
    ) -> Result<DefiSession, HandlerError> {
        let info = self
            .keystore
            .info(wallet)
            .map_err(|e| HandlerError::backend(e.to_string()))?;
        let chain_name = body
            .chain
            .clone()
            .unwrap_or_else(|| self.default_chain.clone());
        let chain = self.chain_client(&chain_name)?;
        let chain_id = chain
            .chain_id()
            .await
            .map_err(|e| HandlerError::backend(e.to_string()))?;
        let mut req =
            Self::build_route_request(&chain, chain_id, info.address, &body.intent).await?;
        if let Some(bps) = body.slippage_bps {
            req.slippage_bps = bps;
        }

        let route = self.enso.route(req.clone()).await.map_err(map_enso_err)?;

        // Build a `Raw` intent that the wallet outbox can stage directly.
        let raw_intent = RawIntent {
            body: RawIntentBody::Raw {
                to: checksum_address(&route.tx.to),
                value: route.tx.value.to_string(),
                data: format!("0x{}", hex::encode(route.tx.data.as_ref())),
            },
            chain: Some(chain_name.clone()),
            gas: beth_proto::GasStrategy::Auto,
            nonce: None,
        };

        let plan = render_plan_md(&body.intent, &chain_name, &req, &route);
        let id = self.allocate_id();
        let now_ms = now_ms();
        let sess = DefiSession {
            id,
            wallet: wallet.to_string(),
            chain: chain_name,
            intent_text: body.intent,
            route: Some(route),
            plan_md: plan,
            tx_intent: Some(raw_intent),
            staged_id: None,
            created_ms: now_ms,
        };
        self.put_session(sess.clone());
        Ok(sess)
    }

    /// Run an `eth_call` simulation of the staged Enso tx.
    async fn simulate_session(
        &self,
        sess: &DefiSession,
    ) -> Result<serde_json::Value, HandlerError> {
        let route = sess
            .route
            .as_ref()
            .ok_or_else(|| HandlerError::backend("session has no route"))?;
        let chain = self.chain_client(&sess.chain)?;
        let req = TransactionRequest::default()
            .from(route.tx.from)
            .to(route.tx.to)
            .value(route.tx.value)
            .input(route.tx.data.clone().into());
        match chain.eth_call_with_overrides(req, None).await {
            Ok(bytes) => Ok(serde_json::json!({
                "success": true,
                "return_data": format!("0x{}", hex::encode(bytes.as_ref())),
                "gas_estimate": route.gas,
            })),
            Err(e) => Ok(serde_json::json!({
                "success": false,
                "error": e.to_string(),
                "gas_estimate": route.gas,
            })),
        }
    }

    async fn confirm_session(&self, wallet: &str, id: &str) -> Result<StagedTx, HandlerError> {
        let sess = self.get_session(wallet, id)?;
        let info = self
            .keystore
            .info(wallet)
            .map_err(|e| HandlerError::backend(e.to_string()))?;
        let chain = self.chain_client(&sess.chain)?;
        let intent = sess
            .tx_intent
            .clone()
            .ok_or_else(|| HandlerError::backend("session has no prepared intent"))?;
        let staged = self
            .tx_engine
            .stage(
                wallet,
                info.address,
                intent,
                &chain,
                &info.policy,
                Some(&self.address_book),
            )
            .await
            .map_err(|e| HandlerError::backend(e.to_string()))?;
        // Update session with staged id.
        let mut updated = sess;
        updated.staged_id = Some(staged.id.clone());
        self.put_session(updated);
        Ok(staged)
    }
}

#[async_trait]
impl Handler for DefiHandler {
    async fn lookup(&self, path: &VfsPath) -> Result<Entry, HandlerError> {
        let segs = path.segments();
        if segs.is_empty() {
            return Ok(Entry::dir(""));
        }
        match segs[0].as_str() {
            "intents" => match segs.len() {
                1 => Ok(Entry::dir("intents")),
                2 => Ok(Entry::dir(&segs[1])),
                3 if segs[2] == "new" => Ok(Entry::writable_file("new")),
                3 => {
                    // /<wallet>/<session>
                    let _ = self.get_session(&segs[1], &segs[2])?;
                    Ok(Entry::dir(&segs[2]))
                }
                4 => {
                    let _ = self.get_session(&segs[1], &segs[2])?;
                    if is_session_file(&segs[3]) {
                        if segs[3] == "confirm" {
                            Ok(Entry::writable_file(&segs[3]))
                        } else {
                            Ok(Entry::file(&segs[3]))
                        }
                    } else {
                        Err(HandlerError::not_found(path.to_string_path()))
                    }
                }
                _ => Err(HandlerError::not_found(path.to_string_path())),
            },
            _ => Err(HandlerError::not_found(path.to_string_path())),
        }
    }

    async fn read(&self, path: &VfsPath) -> Result<Vec<u8>, HandlerError> {
        let segs = path.segments();
        if segs.len() != 4 || segs[0] != "intents" {
            return Err(HandlerError::NotAFile(path.to_string_path()));
        }
        let wallet = &segs[1];
        let id = &segs[2];
        let fname = segs[3].as_str();
        let sess = self.get_session(wallet, id)?;
        match fname {
            "intent.txt" => Ok(format!("{}\n", sess.intent_text).into_bytes()),
            "route.json" => {
                let r = sess
                    .route
                    .as_ref()
                    .ok_or_else(|| HandlerError::backend("no route"))?;
                Ok(serde_json::to_vec_pretty(r).unwrap())
            }
            "plan.md" => Ok(sess.plan_md.clone().into_bytes()),
            "tx.json" => {
                let i = sess
                    .tx_intent
                    .as_ref()
                    .ok_or_else(|| HandlerError::backend("no tx intent"))?;
                Ok(serde_json::to_vec_pretty(i).unwrap())
            }
            "simulation.json" => {
                let v = self.simulate_session(&sess).await?;
                Ok(serde_json::to_vec_pretty(&v).unwrap())
            }
            "confirm" => Ok(b"# write any non-empty content to stage into outbox\n".to_vec()),
            _ => Err(HandlerError::NotAFile(path.to_string_path())),
        }
    }

    async fn write(&self, path: &VfsPath, data: &[u8]) -> Result<(), HandlerError> {
        let segs = path.segments();
        if segs.is_empty() || segs[0] != "intents" {
            return Err(HandlerError::PermissionDenied);
        }
        match segs.len() {
            // intents/<wallet>/new
            3 if segs[2] == "new" => {
                let body = std::str::from_utf8(data)
                    .map_err(|_| HandlerError::invalid("non-utf8 intent body"))?;
                let parsed = Self::parse_new_body(body)?;
                let sess = self.create_session(&segs[1], parsed).await?;
                tracing::info!(wallet = %sess.wallet, session = %sess.id, "defi.session.created");
                Ok(())
            }
            // intents/<wallet>/<session>/confirm
            4 if segs[3] == "confirm" => {
                let trimmed = std::str::from_utf8(data).unwrap_or("").trim();
                if trimmed.is_empty() {
                    return Err(HandlerError::invalid("empty confirm"));
                }
                let staged = self.confirm_session(&segs[1], &segs[2]).await?;
                tracing::info!(
                    wallet = %segs[1],
                    session = %segs[2],
                    staged = %staged.id,
                    "defi.session.confirmed"
                );
                Ok(())
            }
            _ => Err(HandlerError::PermissionDenied),
        }
    }

    async fn list(&self, path: &VfsPath) -> Result<Vec<Entry>, HandlerError> {
        let segs = path.segments();
        match segs.len() {
            0 => Ok(vec![Entry::dir("intents")]),
            1 if segs[0] == "intents" => Ok(self
                .list_session_wallets()
                .into_iter()
                .map(|w| Entry::dir(&w))
                .collect()),
            2 if segs[0] == "intents" => {
                // List "new" + sessions for this wallet (does not require
                // wallet to exist if no sessions, but if it doesn't exist
                // we show only `new`).
                let mut out = vec![Entry::writable_file("new")];
                for id in self.list_sessions_for_wallet(&segs[1]) {
                    out.push(Entry::dir(&id));
                }
                Ok(out)
            }
            3 if segs[0] == "intents" => {
                let _ = self.get_session(&segs[1], &segs[2])?;
                Ok(vec![
                    Entry::file("intent.txt"),
                    Entry::file("route.json"),
                    Entry::file("plan.md"),
                    Entry::file("tx.json"),
                    Entry::file("simulation.json"),
                    Entry::writable_file("confirm"),
                ])
            }
            _ => Err(HandlerError::NotADir(path.to_string_path())),
        }
    }
}

fn is_session_file(s: &str) -> bool {
    matches!(
        s,
        "intent.txt" | "route.json" | "plan.md" | "tx.json" | "simulation.json" | "confirm"
    )
}

fn map_enso_err(e: EnsoError) -> HandlerError {
    match e {
        EnsoError::Disabled | EnsoError::MissingKey => {
            HandlerError::Unsupported("Enso is disabled (no API key)".into())
        }
        EnsoError::InvalidIntent(s) => HandlerError::invalid(s),
        other => HandlerError::backend(other.to_string()),
    }
}

fn decimals_for_symbol(chain_id: u64, sym: &str) -> u8 {
    let upper = sym.to_ascii_uppercase();
    if matches!(
        upper.as_str(),
        "ETH" | "ETHER" | "WETH" | "MATIC" | "BNB" | "AVAX"
    ) {
        return 18;
    }
    match (chain_id, upper.as_str()) {
        (_, "USDC" | "USDT") => 6,
        (1, "DAI") => 18,
        (1, "WBTC") => 8,
        _ => 18,
    }
}

fn render_plan_md(intent: &str, chain: &str, req: &RouteRequest, route: &RouteResponse) -> String {
    let mut s = String::new();
    s.push_str("# DeFi intent\n\n");
    s.push_str(&format!("Intent:    {intent}\n"));
    s.push_str(&format!("Chain:     {chain} (id {})\n", req.chain_id));
    s.push_str(&format!(
        "From:      {}\n",
        checksum_address(&req.from_address)
    ));
    s.push_str(&format!(
        "Token in:  0x{:x}  amount={} (raw)\n",
        req.token_in, req.amount_in
    ));
    s.push_str(&format!(
        "Token out: 0x{:x}  amountOut≈{}\n",
        req.token_out, route.amount_out
    ));
    s.push_str(&format!("Slippage:  {} bps\n", req.slippage_bps));
    if let Some(ref g) = route.gas {
        s.push_str(&format!("Gas:       {g}\n"));
    }
    if let Some(p) = route.price_impact {
        s.push_str(&format!("Impact:    {p}%\n"));
    }
    s.push_str(&format!("Tx to:     {}\n", checksum_address(&route.tx.to)));
    s.push_str(&format!("Tx value:  {} wei\n", route.tx.value));
    s.push_str(&format!("Tx data:   {} bytes\n", route.tx.data.len()));
    s.push_str("\n## Confirm\n");
    s.push_str(
        "Write any non-empty content to `confirm` to stage this through \
         the wallet's outbox; review there before broadcasting.\n",
    );
    s
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_new_body_json() {
        let b = DefiHandler::parse_new_body(
            r#"{"kind":"enso","intent":"swap 1 ETH to USDC","chain":"ethereum"}"#,
        )
        .unwrap();
        assert_eq!(b.intent, "swap 1 ETH to USDC");
        assert_eq!(b.chain.as_deref(), Some("ethereum"));
    }

    #[test]
    fn parse_new_body_plain() {
        let b = DefiHandler::parse_new_body("swap 1 ETH to USDC on ethereum").unwrap();
        assert_eq!(b.intent, "swap 1 ETH to USDC on ethereum");
        assert!(b.chain.is_none());
    }

    #[test]
    fn parse_new_body_empty_errors() {
        assert!(DefiHandler::parse_new_body("").is_err());
        assert!(DefiHandler::parse_new_body("{}").is_err());
    }

    #[test]
    fn build_route_request_resolves_eth_to_usdc() {
        let from: Address = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045"
            .parse()
            .unwrap();
        let nat = beth_defi::parse_natural_intent("swap 1 ETH to USDC").unwrap();
        let req = DefiHandler::compose_route_request(1, from, &nat, 18).unwrap();
        assert_eq!(req.chain_id, 1);
        assert_eq!(req.amount_in, U256::from(1_000_000_000_000_000_000u128));
        // USDC mainnet
        assert_eq!(
            req.token_out.to_checksum(None),
            "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
        );
    }

    #[test]
    fn build_route_request_unknown_token_errors() {
        let from = Address::ZERO;
        let nat = beth_defi::parse_natural_intent("swap 1 FOO to BAR").unwrap();
        let err = DefiHandler::compose_route_request(1, from, &nat, 18).unwrap_err();
        assert!(err.to_string().contains("unknown token"));
    }

    #[test]
    fn render_plan_md_includes_key_fields() {
        let req = RouteRequest {
            from_address: Address::ZERO,
            chain_id: 1,
            token_in: Address::ZERO,
            token_out: Address::ZERO,
            amount_in: U256::from(1u64),
            slippage_bps: 50,
            routing_strategy: None,
            receiver: None,
        };
        let route = RouteResponse {
            tx: beth_defi::RouteTx {
                from: Address::ZERO,
                to: Address::ZERO,
                data: Default::default(),
                value: U256::ZERO,
            },
            amount_out: "100".into(),
            gas: Some("21000".into()),
            route: serde_json::Value::Null,
            price_impact: Some(0.1),
        };
        let md = render_plan_md("swap 1 ETH to USDC", "ethereum", &req, &route);
        assert!(md.contains("swap 1 ETH to USDC"));
        assert!(md.contains("Slippage:  50 bps"));
        assert!(md.contains("Confirm"));
    }
}
