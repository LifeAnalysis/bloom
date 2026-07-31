#![allow(clippy::too_many_arguments)]

use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest as _, Sha256};

wit_bindgen::generate!({
    path: "wit",
    world: "route-file",
    generate_all,
});

use bloom::key::derive;
use bloom::route::types::EntryKind;
use bloom::sign::signing::{self, PayloadSignRequest, Selector, SignResult};
use bloom::store::kv;

const STORE_NAMESPACE: &str = "fixture-public";
const STORE_KEY: &str = "latest.json";
const OPERATION_CLASS: &str = "fixture.payload";
const CRYPTO_SUITE: &str = "secp256k1-sha256-recoverable";
// This package intentionally contains one route, so its stable route index is
// r000001. The host independently binds and checks trusted route provenance.
const ROUTE_ID: &str = "r000001";

struct Fixture;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MountedRequest {
    request_id: String,
    wallet_id: String,
    purpose: String,
    maximum_lifetime_ms: u64,
    preimage_hex: String,
    nonce_hex: String,
    approval_hint: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
enum KeyOutcome {
    Pending {
        operation_id: String,
        scope_digest: String,
    },
    Ready {
        operation_id: String,
        scope_digest: String,
        key_ref_jcs: Vec<u8>,
        addresses: Vec<String>,
    },
}

impl Guest for Fixture {
    fn metadata(_ctx: Ctx) -> Result<RouteMeta, RouteError> {
        Ok(RouteMeta {
            kind: EntryKind::File,
            mode: 0o666,
            cache_ttl_ms: None,
            side_effecting_read: false,
            write_async: false,
            description: Some("Signer-owned Petal key and scoped signing fixture".into()),
            consent_summary: Some(
                "Requests a Petal-scoped child key and signs the supplied full payload".into(),
            ),
            required_caps: vec![
                "bloom:key.derive".into(),
                "bloom:sign".into(),
                "bloom:store".into(),
            ],
            sign_intent: Some(OPERATION_CLASS.into()),
            executable: false,
        })
    }

    fn lookup(_ctx: Ctx) -> Result<Entry, RouteError> {
        Ok(Entry {
            name: "session.json".into(),
            kind: EntryKind::File,
            mode: 0o666,
            size: None,
            link_target: None,
        })
    }

    fn list(_ctx: Ctx) -> Result<Vec<Entry>, RouteError> {
        Err(RouteError::NotADir("session.json".into()))
    }

    fn read(_ctx: Ctx) -> Result<Vec<u8>, RouteError> {
        match kv::get(STORE_NAMESPACE, STORE_KEY).map_err(RouteError::Backend)? {
            Some(bytes) => Ok(bytes),
            None => serde_json::to_vec(&json!({
                "schema": "bloom.triad-authority-fixture.result.v1",
                "state": "empty"
            }))
            .map_err(|error| RouteError::Backend(error.to_string())),
        }
    }

    fn write(ctx: Ctx, body: Vec<u8>) -> Result<(), RouteError> {
        let request: MountedRequest = serde_json::from_slice(&body)
            .map_err(|error| RouteError::Invalid(format!("decode mounted request: {error}")))?;
        validate_request(&request)?;

        let derive_request = serde_json::to_vec(&json!({
            "request_id": request.request_id,
            "wallet_id": request.wallet_id,
            "purpose": request.purpose,
            "allowed_crypto_suites": [CRYPTO_SUITE],
            "maximum_lifetime_ms": request.maximum_lifetime_ms
        }))
        .map_err(|error| RouteError::Backend(error.to_string()))?;
        let key_bytes = match derive::request(&derive_request) {
            Ok(bytes) => bytes,
            Err(error) => return store_error("key_request_failed", error),
        };
        let key: KeyOutcome = serde_json::from_slice(&key_bytes)
            .map_err(|error| RouteError::Backend(format!("decode key outcome: {error}")))?;

        let KeyOutcome::Ready {
            operation_id,
            scope_digest,
            key_ref_jcs,
            addresses,
        } = key
        else {
            return store_json(&json!({
                "schema": "bloom.triad-authority-fixture.result.v1",
                "stage": "key",
                "outcome": key
            }));
        };

        let preimage = hex::decode(&request.preimage_hex)
            .map_err(|error| RouteError::Invalid(format!("decode preimage_hex: {error}")))?;
        let ordered_hash = Sha256::digest(&preimage);
        let payload_digest = Sha256::digest(&preimage);
        let claim = json!({
            "package_hash": ctx.package_hash,
            "route": ROUTE_ID,
            "operation_class": OPERATION_CLASS,
            "crypto_suite": CRYPTO_SUITE,
            "payload_digest": hex::encode(payload_digest),
            "ordered_hashes": [hex::encode(ordered_hash)],
            "declared_debits": [],
            "declared_destinations": [],
            "declared_fee": {"kind": "none"},
            "nonce": request.nonce_hex,
            "claim_assurance": {"kind": "machine_asserted"}
        });
        let claim_jcs = serde_jcs::to_vec(&claim)
            .map_err(|error| RouteError::Backend(format!("canonicalize claim: {error}")))?;
        let sign_request = PayloadSignRequest {
            wallet: request.wallet_id,
            preimage,
            claimed_hash: ordered_hash.to_vec(),
            signature_algorithm: CRYPTO_SUITE.into(),
            operation_class: OPERATION_CLASS.into(),
            petal_use_claim_jcs: claim_jcs,
            claim_assurance_evidence: None,
            approval_hint: request.approval_hint,
            action: None,
            advisory: None,
            selector: Selector::Reusable,
            key_ref_jcs: Some(key_ref_jcs.clone()),
        };
        let sign = match signing::sign_payload(&sign_request) {
            Ok(result) => result,
            Err(error) => return store_error("signing_failed", error),
        };
        let public_key = json!({
            "operation_id": operation_id,
            "scope_digest": scope_digest,
            "key_ref_jcs": key_ref_jcs,
            "addresses": addresses
        });
        match sign {
            SignResult::Signature(signature) => store_json(&json!({
                "schema": "bloom.triad-authority-fixture.result.v1",
                "stage": "complete",
                "public_key": public_key,
                "signature_hex": hex::encode(signature)
            })),
            SignResult::ApprovalRequired(approval) => store_json(&json!({
                "schema": "bloom.triad-authority-fixture.result.v1",
                "stage": "signing",
                "public_key": public_key,
                "approval_required": {
                    "action_id": approval.action_id,
                    "ceremony_url": approval.ceremony_url,
                    "expires_ms": approval.expires_ms
                }
            })),
        }
    }
}

fn validate_request(request: &MountedRequest) -> Result<(), RouteError> {
    if request.request_id.is_empty()
        || request.wallet_id.is_empty()
        || request.purpose.is_empty()
        || request.maximum_lifetime_ms == 0
    {
        return Err(RouteError::Invalid(
            "request identity and lifetime must be non-empty".into(),
        ));
    }
    if request.nonce_hex.len() != 32
        || !request
            .nonce_hex
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(RouteError::Invalid(
            "nonce_hex must be exactly 16 bytes of lowercase hex".into(),
        ));
    }
    if let Some(approval) = &request.approval_hint
        && (approval.len() != 64
            || !approval
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()))
    {
        return Err(RouteError::Invalid(
            "approval_hint must be exactly 32 bytes of lowercase hex".into(),
        ));
    }
    Ok(())
}

fn store_error(stage: &str, message: String) -> Result<(), RouteError> {
    store_json(&json!({
        "schema": "bloom.triad-authority-fixture.result.v1",
        "stage": stage,
        "error": message
    }))
}

fn store_json(value: &serde_json::Value) -> Result<(), RouteError> {
    let bytes =
        serde_json::to_vec_pretty(value).map_err(|error| RouteError::Backend(error.to_string()))?;
    kv::put(STORE_NAMESPACE, STORE_KEY, &bytes, false).map_err(RouteError::Backend)
}

export!(Fixture);
