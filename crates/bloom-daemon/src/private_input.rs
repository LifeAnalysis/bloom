//! Daemon-owned, capability-gated private input sessions for Petal routes.
//!
//! Session values are held only in daemon memory. Guest Wasm code never
//! receives a browser-openable ceremony URL or any other bearer credential
//! for the ceremony itself: it receives a non-secret `operation_id` while
//! pending, and (only once approved) a one-time `handle` alongside the
//! released value. The ceremony is surfaced to its owner exclusively
//! through [`crate::ceremony_server`]'s owner-facing launch surface,
//! correlated by `operation_id` -- never returned to, or reconstructable
//! by, the calling component.

use std::collections::HashMap;
use std::sync::Mutex;

use base64::Engine as _;
use bloom_auth_api::ApprovalChallenge;
use bloom_petals::HostError;
use bloom_petals::abi::{
    PendingPrivateInput, PetalRouteContext, PrivateInputKind, PrivateInputOutcome,
    PrivateInputRequest, PrivateInputTransferContext, ReadyPrivateInput,
};
use rand::RngCore;
use zeroize::Zeroizing;

pub(crate) const PRIVATE_INPUT_TTL_MS: u64 = 10 * 60 * 1000;
const MAX_ACTIVE_PRIVATE_INPUTS: usize = 64;

/// Petal names, and the exact package hash trusted to request private-input
/// ceremonies under that name. Bare-name matching alone (`petal_root ==
/// "privacy-pools"`) is not authorization: `petal_root` is a self-declared
/// install-time label, and any component installed/mounted under a
/// colliding name would otherwise pass. Binding to `package_hash` -- which
/// the runner injects from the verified installed package, never the
/// component itself -- means a same-named-but-different-build component is
/// rejected.
///
/// Empty (and therefore fail-closed) by default: private-input is denied
/// for every petal until a deployment explicitly trusts a specific,
/// verified Privacy Pools build. Wiring an actual hash requires a real
/// provenance decision (e.g. once Privacy Pools has a reviewed, tagged
/// release pinned the way other preinstalled petals are) that does not
/// belong in this host-runtime change -- see
/// [`PrivateInputManager::with_trusted_hash`].
#[derive(Default, Clone)]
pub(crate) struct TrustedPrivateInputPetals(HashMap<String, String>);

impl TrustedPrivateInputPetals {
    pub fn is_trusted(&self, petal_root: &str, package_hash: &str) -> bool {
        self.0
            .get(petal_root)
            .is_some_and(|hash| hash == package_hash)
    }
}

#[derive(Clone)]
pub(crate) struct PrivateInputMetadata {
    /// Secret bearer token: the loopback ceremony URL's path component and
    /// the daemon-internal session key. Never returned to guest code.
    pub token: String,
    pub id: String,
    pub wallet: String,
    pub approval_wallet: String,
    pub title: String,
    pub prompt: String,
    pub kind: PrivateInputKind,
    pub transfer: PrivateInputTransferContext,
    pub context: PetalRouteContext,
    pub expires_ms: u64,
}

enum PrivateInputState {
    Awaiting,
    Prepared {
        value: Zeroizing<String>,
        challenge: Box<ApprovalChallenge>,
    },
    Ready {
        value: Zeroizing<String>,
        /// One-time consume handle, minted only once the ceremony
        /// completes. Never generated (or exposed) before then.
        handle: String,
    },
}

struct PrivateInputSession {
    metadata: PrivateInputMetadata,
    fingerprint: [u8; 32],
    /// Non-secret, deterministic per fingerprint. Safe to hand to guest
    /// code and to persist in a Petal's own public state.
    operation_id: String,
    state: PrivateInputState,
}

pub(crate) struct PrivateInputManager {
    sessions: Mutex<HashMap<String, PrivateInputSession>>,
    trusted: TrustedPrivateInputPetals,
}

impl Default for PrivateInputManager {
    fn default() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            trusted: TrustedPrivateInputPetals::default(),
        }
    }
}

impl PrivateInputManager {
    /// Trust a specific package hash to request private-input ceremonies
    /// under the given petal name. See [`TrustedPrivateInputPetals`] for why
    /// this must be explicit rather than inferred from the name alone.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn with_trusted_hash(
        mut self,
        petal_root: impl Into<String>,
        package_hash: impl Into<String>,
    ) -> Self {
        self.trusted
            .0
            .insert(petal_root.into(), package_hash.into());
        self
    }

    pub fn request(
        &self,
        request: PrivateInputRequest,
        now_ms: u64,
    ) -> Result<PrivateInputOutcome, HostError> {
        let context = validate_request(&request, &self.trusted)?;
        let fingerprint = request_fingerprint(&request, &context)?;
        let operation_id = operation_id_for_fingerprint(&fingerprint);
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| HostError::Backend("private-input session lock poisoned".into()))?;
        sessions.retain(|_, session| session.metadata.expires_ms > now_ms);

        if let Some(session) = sessions
            .values()
            .find(|session| session.fingerprint == fingerprint)
        {
            return Ok(outcome(session));
        }
        if sessions.len() >= MAX_ACTIVE_PRIVATE_INPUTS {
            return Err(HostError::Backend(
                "too many active private-input ceremonies".into(),
            ));
        }

        let expires_ms = now_ms
            .checked_add(PRIVATE_INPUT_TTL_MS)
            .ok_or_else(|| HostError::Backend("private-input expiry overflow".into()))?;
        let token = random_token();
        let metadata = PrivateInputMetadata {
            token: token.clone(),
            id: request.id,
            wallet: request.wallet,
            approval_wallet: request.approval_wallet.ok_or_else(|| {
                HostError::Invalid("private-input approval wallet was not resolved".into())
            })?,
            title: request.title,
            prompt: request.prompt,
            kind: request.kind,
            transfer: request.transfer,
            context,
            expires_ms,
        };
        sessions.insert(
            token,
            PrivateInputSession {
                metadata,
                fingerprint,
                operation_id: operation_id.clone(),
                state: PrivateInputState::Awaiting,
            },
        );
        Ok(PrivateInputOutcome::Pending(PendingPrivateInput {
            operation_id,
            expires_ms,
        }))
    }

    /// Resolves the non-secret, guest-visible `operation_id` to the actual
    /// (secret) session token, for Bloom's own owner-facing launch surface.
    /// This is the *only* place that translation happens: no guest code
    /// path has access to it, since none holds a reference to the
    /// [`PrivateInputManager`] -- guest calls are mediated entirely through
    /// [`crate::PetalHost`], which never exposes this method.
    pub fn token_for_operation(
        &self,
        operation_id: &str,
        now_ms: u64,
    ) -> Result<String, HostError> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| HostError::Backend("private-input session lock poisoned".into()))?;
        sessions.retain(|_, session| session.metadata.expires_ms > now_ms);
        sessions
            .iter()
            .find(|(_, session)| session.operation_id == operation_id)
            .map(|(token, _)| token.clone())
            .ok_or_else(|| HostError::NotFound("private-input ceremony".into()))
    }

    pub fn metadata(&self, token: &str, now_ms: u64) -> Result<PrivateInputMetadata, HostError> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| HostError::Backend("private-input session lock poisoned".into()))?;
        let expired = sessions
            .get(token)
            .is_some_and(|session| session.metadata.expires_ms <= now_ms);
        if expired {
            sessions.remove(token);
            return Err(HostError::Denied("private-input ceremony expired".into()));
        }
        sessions
            .get(token)
            .map(|session| session.metadata.clone())
            .ok_or_else(|| HostError::NotFound("private-input ceremony".into()))
    }

    pub fn set_prepared(
        &self,
        token: &str,
        value: String,
        challenge: ApprovalChallenge,
        now_ms: u64,
    ) -> Result<(), HostError> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| HostError::Backend("private-input session lock poisoned".into()))?;
        let session = sessions
            .get_mut(token)
            .ok_or_else(|| HostError::NotFound("private-input ceremony".into()))?;
        if session.metadata.expires_ms <= now_ms {
            return Err(HostError::Denied("private-input ceremony expired".into()));
        }
        if matches!(session.state, PrivateInputState::Ready { .. }) {
            return Err(HostError::Denied(
                "private-input ceremony already completed".into(),
            ));
        }
        session.state = PrivateInputState::Prepared {
            value: Zeroizing::new(value),
            challenge: Box::new(challenge),
        };
        Ok(())
    }

    pub fn prepared_challenge(
        &self,
        token: &str,
        now_ms: u64,
    ) -> Result<ApprovalChallenge, HostError> {
        let sessions = self
            .sessions
            .lock()
            .map_err(|_| HostError::Backend("private-input session lock poisoned".into()))?;
        let session = sessions
            .get(token)
            .ok_or_else(|| HostError::NotFound("private-input ceremony".into()))?;
        if session.metadata.expires_ms <= now_ms {
            return Err(HostError::Denied("private-input ceremony expired".into()));
        }
        match &session.state {
            PrivateInputState::Prepared { challenge, .. } => Ok(challenge.as_ref().clone()),
            PrivateInputState::Awaiting => Err(HostError::Invalid(
                "private-input value has not been prepared".into(),
            )),
            PrivateInputState::Ready { .. } => Err(HostError::Denied(
                "private-input ceremony already completed".into(),
            )),
        }
    }

    /// Completes the ceremony, minting the one-time consume handle. Only
    /// generated here, at the moment the value first becomes releasable --
    /// never earlier, and never derivable from the (non-secret, public
    /// throughout) operation id.
    pub fn complete(&self, token: &str, action_id: &str, now_ms: u64) -> Result<(), HostError> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| HostError::Backend("private-input session lock poisoned".into()))?;
        let session = sessions
            .get_mut(token)
            .ok_or_else(|| HostError::NotFound("private-input ceremony".into()))?;
        if session.metadata.expires_ms <= now_ms {
            return Err(HostError::Denied("private-input ceremony expired".into()));
        }
        let state = std::mem::replace(&mut session.state, PrivateInputState::Awaiting);
        match state {
            PrivateInputState::Prepared { value, challenge }
                if challenge.action_id == action_id =>
            {
                session.state = PrivateInputState::Ready {
                    value,
                    handle: random_token(),
                };
                Ok(())
            }
            other => {
                session.state = other;
                Err(HostError::Denied(
                    "private-input approval does not match the prepared value".into(),
                ))
            }
        }
    }

    /// Consumes a completed session by its one-time handle. The handle is
    /// unique per completed session by construction (freshly random,
    /// minted only in [`Self::complete`]), so lookup is authoritative on
    /// its own; the route-context match is defense in depth, not the
    /// primary disambiguator the way it had to be for the old id-keyed
    /// design.
    pub fn consume(
        &self,
        handle: &str,
        context: Option<PetalRouteContext>,
        now_ms: u64,
    ) -> Result<(), HostError> {
        let context = context.ok_or_else(|| {
            HostError::Denied("private-input consume requires trusted route context".into())
        })?;
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| HostError::Backend("private-input session lock poisoned".into()))?;
        let token = sessions
            .iter()
            .find(|(_, session)| {
                same_origin(&session.metadata.context, &context)
                    && session.metadata.expires_ms > now_ms
                    && matches!(
                        &session.state,
                        PrivateInputState::Ready { handle: session_handle, .. }
                            if session_handle == handle
                    )
            })
            .map(|(token, _)| token.clone())
            .ok_or_else(|| HostError::NotFound("ready private-input session".into()))?;
        sessions.remove(&token);
        Ok(())
    }
}

fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Deterministic per distinct request *content*: identical retries (same
/// fingerprint) resolve to the same operation id; requests that differ in
/// content get a different one, even if they reuse the same caller-chosen
/// `request.id` -- `id` is explicitly not guaranteed unique (see
/// [`PrivateInputManager::consume`]), so deriving the public correlation id
/// from it would let two unrelated concurrent ceremonies collide under
/// Bloom's own owner-facing launch surface.
fn operation_id_for_fingerprint(fingerprint: &[u8; 32]) -> String {
    let hex = blake3::Hash::from_bytes(*fingerprint).to_hex().to_string();
    format!("op_{}", &hex[..32])
}

fn validate_request(
    request: &PrivateInputRequest,
    trusted: &TrustedPrivateInputPetals,
) -> Result<PetalRouteContext, HostError> {
    let context = request.context.clone().ok_or_else(|| {
        HostError::Denied("private-input request requires trusted route context".into())
    })?;
    if !trusted.is_trusted(&context.petal_root, &context.package_hash) {
        return Err(HostError::Denied(
            "private-input ceremonies require an explicitly trusted package".into(),
        ));
    }
    if request.id.is_empty()
        || request.id.len() > 256
        || request.id.starts_with('/')
        || request.id.contains('\0')
        || request
            .id
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(HostError::Invalid("invalid private-input id".into()));
    }
    if request.wallet.is_empty() || request.wallet.len() > 128 {
        return Err(HostError::Invalid("invalid private-input wallet".into()));
    }
    if request
        .approval_wallet
        .as_ref()
        .is_none_or(|wallet| wallet.is_empty() || wallet.len() > 64)
    {
        return Err(HostError::Invalid(
            "private-input approval wallet must name a passkey wallet".into(),
        ));
    }
    if request.title.is_empty() || request.title.len() > 120 {
        return Err(HostError::Invalid("invalid private-input title".into()));
    }
    if request.prompt.is_empty() || request.prompt.len() > 500 {
        return Err(HostError::Invalid("invalid private-input prompt".into()));
    }
    validate_transfer(&request.transfer)?;
    Ok(context)
}

fn validate_transfer(transfer: &PrivateInputTransferContext) -> Result<(), HostError> {
    if transfer.network.is_empty() || transfer.network.len() > 64 {
        return Err(HostError::Invalid(
            "invalid private-input transfer network".into(),
        ));
    }
    if transfer.asset.is_empty() || transfer.asset.len() > 32 {
        return Err(HostError::Invalid(
            "invalid private-input transfer asset".into(),
        ));
    }
    if transfer.amount_base_units.is_empty()
        || transfer.amount_base_units.len() > 78
        || !transfer
            .amount_base_units
            .bytes()
            .all(|b| b.is_ascii_digit())
    {
        return Err(HostError::Invalid(
            "private-input transfer amount must be a non-negative decimal integer".into(),
        ));
    }
    if transfer.source.is_empty() || transfer.source.len() > 256 {
        return Err(HostError::Invalid(
            "invalid private-input transfer source".into(),
        ));
    }
    Ok(())
}

fn request_fingerprint(
    request: &PrivateInputRequest,
    context: &PetalRouteContext,
) -> Result<[u8; 32], HostError> {
    let encoded = serde_json::to_vec(&serde_json::json!({
        "domain": "bloom.private_input.request.v2",
        "id": request.id,
        "wallet": request.wallet,
        "approval_wallet": request.approval_wallet,
        "title": request.title,
        "prompt": request.prompt,
        "kind": match request.kind { PrivateInputKind::EvmAddress => "evm_address" },
        "transfer": {
            "network": request.transfer.network,
            "asset": request.transfer.asset,
            "amount_base_units": request.transfer.amount_base_units,
            "decimals": request.transfer.decimals,
            "source": request.transfer.source,
        },
        "petal_root": context.petal_root,
        "package_hash": context.package_hash,
        "route_id": context.route_id,
        "op": context.op,
        "path": context.path,
        "params": context.params,
        "actor": context.actor,
    }))
    .map_err(|error| HostError::Backend(format!("encode private-input request: {error}")))?;
    Ok(*blake3::hash(&encoded).as_bytes())
}

fn same_origin(left: &PetalRouteContext, right: &PetalRouteContext) -> bool {
    left.petal_root == right.petal_root
        && left.package_hash == right.package_hash
        && left.route_id == right.route_id
        && left.op == right.op
        && left.path == right.path
        && left.params == right.params
        && left.actor == right.actor
}

fn outcome(session: &PrivateInputSession) -> PrivateInputOutcome {
    match &session.state {
        PrivateInputState::Ready { value, handle } => {
            PrivateInputOutcome::Ready(ReadyPrivateInput {
                handle: handle.clone(),
                value: value.to_string(),
            })
        }
        PrivateInputState::Awaiting | PrivateInputState::Prepared { .. } => {
            PrivateInputOutcome::Pending(PendingPrivateInput {
                operation_id: session.operation_id.clone(),
                expires_ms: session.metadata.expires_ms,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bloom_auth_api::{APPROVAL_CHALLENGE_SCHEMA_V1, AssuranceLevel};

    const TRUSTED_HASH: &str = "abababababababababababababababababababababababababababababababab";
    const _: () = assert!(TRUSTED_HASH.len() == 64);

    fn manager() -> PrivateInputManager {
        PrivateInputManager::default().with_trusted_hash("privacy-pools", TRUSTED_HASH)
    }

    fn transfer() -> PrivateInputTransferContext {
        PrivateInputTransferContext {
            network: "ethereum".into(),
            asset: "eth".into(),
            amount_base_units: "1000000000000000000".into(),
            decimals: 18,
            source: "dev/note-1".into(),
        }
    }

    fn request() -> PrivateInputRequest {
        PrivateInputRequest {
            id: "privacy-pools/withdraw/dev/note-1".into(),
            wallet: "dev".into(),
            approval_wallet: Some("owner-passkey".into()),
            title: "Private withdrawal destination".into(),
            prompt: "Enter the destination address".into(),
            kind: PrivateInputKind::EvmAddress,
            transfer: transfer(),
            context: Some(PetalRouteContext {
                petal_root: "privacy-pools".into(),
                package_hash: TRUSTED_HASH.into(),
                route_id: "withdraw-private".into(),
                op: "write".into(),
                path: "private-withdrawals/dev/note-1.json".into(),
                params: vec![],
                actor: None,
            }),
        }
    }

    #[test]
    fn request_is_stable_and_redacted() {
        let manager = manager();
        let first = manager.request(request(), 1).unwrap();
        let second = manager.request(request(), 2).unwrap();
        assert_eq!(first, second);
        let json = format!("{first:?}");
        assert!(!json.contains("0x"));
    }

    #[test]
    fn rejects_untrusted_package_hash_even_with_matching_name() {
        let manager = manager();
        let mut request = request();
        request.context.as_mut().unwrap().package_hash = "ff".repeat(32);
        assert!(matches!(
            manager.request(request, 1),
            Err(HostError::Denied(_))
        ));
    }

    #[test]
    fn rejects_other_petal_names() {
        let manager = manager();
        let mut request = request();
        request.context.as_mut().unwrap().petal_root = "anything-else".into();
        assert!(matches!(
            manager.request(request, 1),
            Err(HostError::Denied(_))
        ));
    }

    #[test]
    fn rejects_when_nothing_is_trusted() {
        let manager = PrivateInputManager::default();
        assert!(matches!(
            manager.request(request(), 1),
            Err(HostError::Denied(_))
        ));
    }

    #[test]
    fn different_content_sharing_the_same_caller_id_gets_different_operation_ids() {
        let manager = manager();
        let first = manager.request(request(), 1).unwrap();
        let mut other = request();
        // Same caller-chosen id, different content -- id is explicitly not
        // a uniqueness guarantee.
        other.prompt = "A completely different prompt".into();
        let second = manager.request(other, 2).unwrap();
        let PrivateInputOutcome::Pending(first) = first else {
            panic!("expected pending");
        };
        let PrivateInputOutcome::Pending(second) = second else {
            panic!("expected pending");
        };
        assert_ne!(first.operation_id, second.operation_id);
    }

    #[test]
    fn transfer_amount_must_be_a_decimal_integer() {
        let manager = manager();
        let mut request = request();
        request.transfer.amount_base_units = "not-a-number".into();
        assert!(matches!(
            manager.request(request, 1),
            Err(HostError::Invalid(_))
        ));
    }

    #[test]
    fn operation_id_resolves_to_the_real_token_for_the_owner_surface_only() {
        let manager = manager();
        let pending = manager.request(request(), 1).unwrap();
        let PrivateInputOutcome::Pending(pending) = pending else {
            panic!("expected pending");
        };
        let resolved = manager
            .token_for_operation(&pending.operation_id, 2)
            .unwrap();
        // The resolved token actually identifies the real session.
        assert!(manager.metadata(&resolved, 2).is_ok());
        assert!(manager.token_for_operation("op_does-not-exist", 2).is_err());
    }

    fn challenge(action_id: &str) -> ApprovalChallenge {
        ApprovalChallenge {
            schema: APPROVAL_CHALLENGE_SCHEMA_V1.into(),
            action_id: action_id.into(),
            wallet: "dev".into(),
            surface: "petal-private-input".into(),
            petal_id: "pkg:privacy-pools".into(),
            petal_digest: TRUSTED_HASH.into(),
            intent_hash: "cd".repeat(32),
            server_nonce: "nonce".into(),
            assurance: AssuranceLevel::Standard,
            daemon_terms_digest: "ef".repeat(32),
            petal_policy_digest: "12".repeat(32),
            policy_version: 0,
            expiry_ms: 600_001,
            ceremony_url: None,
        }
    }

    #[test]
    fn ready_value_is_origin_bound_and_single_use_by_handle() {
        let manager = manager();
        let request = request();
        let context = request.context.clone();
        let pending = manager.request(request.clone(), 1).unwrap();
        let PrivateInputOutcome::Pending(pending) = pending else {
            panic!("expected pending");
        };
        // The public operation id is not a session lookup key: find the
        // internal token the way the ceremony server does, via metadata
        // scan, since the test has no HTTP layer. We reach in through a
        // second `request` call, which returns the same outcome for the
        // same fingerprint, to get at the private token indirectly is not
        // possible here, so we drive completion through the manager's own
        // token by capturing it at insertion. Simplest: request() doesn't
        // expose token, so exercise the flow through a raw lock inspection.
        let token = {
            let sessions = manager.sessions.lock().unwrap();
            sessions.keys().next().unwrap().clone()
        };
        manager
            .set_prepared(
                &token,
                "0x1111111111111111111111111111111111111111".into(),
                challenge("action-1"),
                2,
            )
            .unwrap();
        assert!(manager.complete(&token, "wrong-action", 3).is_err());
        manager.complete(&token, "action-1", 3).unwrap();
        let ready = manager.request(request, 4).unwrap();
        let PrivateInputOutcome::Ready(ready) = ready else {
            panic!("expected ready");
        };
        assert_eq!(ready.value, "0x1111111111111111111111111111111111111111");
        // The operation id shown while pending must never work as the
        // consume handle -- it's public, the handle must not be.
        assert!(
            manager
                .consume(&pending.operation_id, context.clone(), 5)
                .is_err()
        );

        let mut wrong_context = context.clone().unwrap();
        wrong_context.package_hash = "ff".repeat(32);
        assert!(
            manager
                .consume(&ready.handle, Some(wrong_context), 5)
                .is_err()
        );
        manager.consume(&ready.handle, context.clone(), 5).unwrap();
        assert!(manager.metadata(&token, 6).is_err());
        // Single-use: the session is gone, so replaying the exact same
        // consume call with the right handle and context fails too.
        assert!(manager.consume(&ready.handle, context, 6).is_err());
    }
}
