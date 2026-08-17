//! The Bloom Solana driver Petal.
//!
//! Constructs canonical legacy, single-signer System Program transfers and
//! assembles signed transactions. Chain observations flow exclusively through
//! the Machine-mediated `bloom:chain/read` host interface — this component
//! imports no network, signing, key, or VFS capability. Signing authority is
//! exercised outside the Petal (Broker approval, Signer custody);
//! `transfer.assemble.json` consumes an externally produced signature.
//!
//! Message construction uses the pinned Anza crates; the independent
//! `solana-system-transfer-v1` verifier in `bloom-solana` (compiled into
//! Broker, not here) re-parses these bytes. Host-integration tests prove the
//! two implementations agree through the real Wasmtime execution path.

#![allow(clippy::too_many_arguments)]

use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest as _, Sha256};
use std::str::FromStr;

wit_bindgen::generate!({
    path: "wit",
    world: "route-file",
    generate_all,
});

use bloom::chain::read;
use bloom::route::types::EntryKind;
use bloom::store::kv;

const STORE_NAMESPACE: &str = "solana-driver";
const STORE_KEY: &str = "latest.json";
const OPERATION_CLASS: &str = "solana.native-transfer";
const CRYPTO_SUITE: &str = "ed25519-message";

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    let out: [u8; 32] = h.finalize().into();
    hex::encode(out)
}

/// `transfer.stage.json` request body.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StageRequest {
    /// Operator-configured chain profile name used for mediated reads.
    chain_profile: String,
    /// The projected Ed25519 fee-payer account (base58).
    fee_payer_base58: String,
    destination_base58: String,
    lamports: u64,
    /// Explicit recent blockhash (hex). When absent the Petal performs one
    /// mediated `getLatestBlockhash` read on `chain_profile`.
    blockhash_hex: Option<String>,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct StageResponse {
    schema: &'static str,
    message_hex: String,
    payload_digest_hex: String,
    fee_payer_base58: String,
    destination_base58: String,
    lamports: u64,
    blockhash_base58: String,
    last_valid_block_height: u64,
    operation_class: &'static str,
    crypto_suite: &'static str,
}

/// `transfer.assemble.json` request body.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AssembleRequest {
    message_hex: String,
    /// 64-byte Ed25519 signature over the raw message bytes (hex).
    signature_hex: String,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct AssembleResponse {
    schema: &'static str,
    /// Complete legacy transaction: short-vec signature count, the 64-byte
    /// signature, then the message bytes.
    transaction_hex: String,
    transaction_digest_hex: String,
    payload_digest_hex: String,
}

struct SolanaDriver;

impl Guest for SolanaDriver {
    fn metadata(_ctx: Ctx) -> Result<RouteMeta, RouteError> {
        Ok(RouteMeta {
            kind: EntryKind::File,
            mode: 0o666,
            cache_ttl_ms: None,
            side_effecting_read: false,
            write_async: false,
            description: Some("Solana native SOL transfer driver".into()),
            consent_summary: Some(
                "Constructs legacy single-signer System Program transfers through mediated chain reads"
                    .into(),
            ),
            required_caps: vec!["bloom:chain".into(), "bloom:store".into()],
            // This driver never signs: signing authority lives in the triad
            // outside the Petal, so no sign intent is declared.
            sign_intent: None,
            executable: false,
        })
    }

    fn lookup(ctx: Ctx) -> Result<Entry, RouteError> {
        let name = route_name(&ctx)?;
        Ok(Entry {
            name,
            kind: EntryKind::File,
            mode: 0o666,
            size: None,
            link_target: None,
        })
    }

    fn list(_ctx: Ctx) -> Result<Vec<Entry>, RouteError> {
        Err(RouteError::NotADir("transfer routes are files".into()))
    }

    fn read(_ctx: Ctx) -> Result<Vec<u8>, RouteError> {
        match kv::get(STORE_NAMESPACE, STORE_KEY).map_err(RouteError::Backend)? {
            Some(bytes) => Ok(bytes),
            None => serde_json::to_vec(&json!({
                "schema": "bloom.solana-driver.result.v1",
                "state": "empty"
            }))
            .map_err(|e| RouteError::Backend(e.to_string())),
        }
    }

    fn write(ctx: Ctx, body: Vec<u8>) -> Result<(), RouteError> {
        let value = match route_name(&ctx)?.as_str() {
            "transfer.stage.json" => {
                let response = stage(body)?;
                serde_json::to_value(&response).map_err(|e| RouteError::Backend(e.to_string()))?
            }
            "transfer.assemble.json" => {
                let response = assemble(body)?;
                serde_json::to_value(&response).map_err(|e| RouteError::Backend(e.to_string()))?
            }
            other => return Err(RouteError::NotFound(other.to_string())),
        };
        let mut wrapped = json!({ "schema": "bloom.solana-driver.result.v1", "state": "ok" });
        if let Some(obj) = wrapped.as_object_mut()
            && let Some(inner) = value.as_object()
        {
            for (k, v) in inner {
                obj.insert(k.clone(), v.clone());
            }
        }
        store_json(&wrapped)
    }
}

fn route_name(ctx: &Ctx) -> Result<String, RouteError> {
    let path = ctx.path.as_str();
    let name = path.rsplit('/').next().unwrap_or(path);
    match name {
        "transfer.stage.json" | "transfer.assemble.json" => Ok(name.to_string()),
        other => Err(RouteError::NotFound(other.to_string())),
    }
}

fn store_json(value: &serde_json::Value) -> Result<(), RouteError> {
    let bytes =
        serde_json::to_vec_pretty(value).map_err(|error| RouteError::Backend(error.to_string()))?;
    kv::put(STORE_NAMESPACE, STORE_KEY, &bytes, false).map_err(RouteError::Backend)
}

fn invalid(msg: &str) -> RouteError {
    RouteError::Invalid(msg.to_string())
}

fn stage(body: Vec<u8>) -> Result<StageResponse, RouteError> {
    let req: StageRequest = serde_json::from_slice(&body).map_err(|e| invalid(&e.to_string()))?;

    let from = solana_message::Address::from_str(&req.fee_payer_base58)
        .map_err(|e| invalid(&format!("fee_payer_base58: {e}")))?;
    let to = solana_message::Address::from_str(&req.destination_base58)
        .map_err(|e| invalid(&format!("destination_base58: {e}")))?;
    if from == to {
        return Err(invalid("destination equals fee payer"));
    }
    if req.lamports == 0 {
        return Err(invalid("lamports must be greater than zero"));
    }

    let (blockhash, last_valid) = match req.blockhash_hex.as_deref() {
        Some(hex_str) => {
            let bytes =
                hex::decode(hex_str).map_err(|e| invalid(&format!("blockhash_hex: {e}")))?;
            let arr: [u8; 32] = bytes.try_into().map_err(|v: Vec<u8>| {
                invalid(&format!("blockhash must be 32 bytes, got {}", v.len()))
            })?;
            (
                solana_message::Hash::new_from_array(arr),
                // An explicit blockhash carries no observed height; the
                // response reports zero and the caller tracks liveness.
                0,
            )
        }
        None => mediated_latest_blockhash(&req.chain_profile)?,
    };

    let ix = solana_system_interface::instruction::transfer(&from, &to, req.lamports);
    let message = solana_message::Message::new_with_blockhash(&[ix], Some(&from), &blockhash);
    let bytes = message.serialize();

    Ok(StageResponse {
        schema: "bloom.solana-driver.stage.v1",
        message_hex: hex::encode(&bytes),
        payload_digest_hex: sha256_hex(&bytes),
        fee_payer_base58: from.to_string(),
        destination_base58: to.to_string(),
        lamports: req.lamports,
        blockhash_base58: blockhash.to_string(),
        last_valid_block_height: last_valid,
        operation_class: OPERATION_CLASS,
        crypto_suite: CRYPTO_SUITE,
    })
}

/// One mediated read: the Petal never sees endpoints or credentials.
fn mediated_latest_blockhash(profile: &str) -> Result<(solana_message::Hash, u64), RouteError> {
    let resp = read::call(&read::Request {
        chain: profile.to_string(),
        method: "getLatestBlockhash".to_string(),
        params_json: "[]".to_string(),
    })
    .map_err(|e| RouteError::Denied(format!("mediated chain read: {e}")))?;

    let parsed: serde_json::Value =
        serde_json::from_str(&resp.result_json).map_err(|e| RouteError::Backend(e.to_string()))?;
    // Real Solana RPC nests getLatestBlockhash under "value".
    let blockhash = parsed
        .pointer("/value/blockhash")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RouteError::Backend("missing blockhash".into()))?;
    let last_valid = parsed
        .pointer("/value/lastValidBlockHeight")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| RouteError::Backend("missing lastValidBlockHeight".into()))?;
    let hash = solana_message::Hash::from_str(blockhash)
        .map_err(|e| RouteError::Backend(format!("blockhash: {e}")))?;
    Ok((hash, last_valid))
}

fn assemble(body: Vec<u8>) -> Result<AssembleResponse, RouteError> {
    let req: AssembleRequest =
        serde_json::from_slice(&body).map_err(|e| invalid(&e.to_string()))?;

    let message =
        hex::decode(&req.message_hex).map_err(|e| invalid(&format!("message_hex: {e}")))?;
    let signature =
        hex::decode(&req.signature_hex).map_err(|e| invalid(&format!("signature_hex: {e}")))?;
    if signature.len() != 64 {
        return Err(invalid(&format!(
            "signature must be 64 bytes, got {}",
            signature.len()
        )));
    }

    let mut tx = Vec::with_capacity(1 + 64 + message.len());
    tx.push(1u8); // short-vec length for exactly one signature
    tx.extend_from_slice(&signature);
    tx.extend_from_slice(&message);

    Ok(AssembleResponse {
        schema: "bloom.solana-driver.assemble.v1",
        transaction_hex: hex::encode(&tx),
        transaction_digest_hex: sha256_hex(&tx),
        payload_digest_hex: sha256_hex(&message),
    })
}

export!(SolanaDriver);
