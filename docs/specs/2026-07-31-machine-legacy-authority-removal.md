# Machine Legacy Authority Removal

**Status:** proposed goal specification; review required before implementation

**Date:** 2026-07-31

**Audience:** Bloom engineers, security reviewers, release engineers, and implementation agents

**Scope:** removal of legacy wallet authority, signing, approval, and key-bearing state from Bloom Machine

**Normative parent:** [Bloom Machine, Broker, and Signer Architecture](./2026-07-23-triad-process-architecture.md)

**Inventory baseline:** `triad-architecture` at `2767153bfab6`

## 1. Purpose and precedence

The triad architecture requires production Machine to contain no embedded
signer or authorization fallback. Broker is the only Machine-facing authority
service. Signer is the only owner of wallet and delegated signing keys and the
only producer of wallet-controlled signatures.

The authority plane now exists, but Machine extraction is incomplete. Machine
still opens the pre-triad keystore and approval database, exposes legacy
approval services, uses legacy wallet records as public projections, and owns
a Hyperliquid delegated signing key. Deleting the old crates before replacing
those consumers would break production reads, staging, simulation, requests,
outbox handling, and venue integrations. Leaving them in place would fail the
target architecture even where current signing calls happen to route through
Broker.

This document specifies the remaining extraction work. It refines W7, W8, and
AC-04 of the parent architecture; it does not change the Machine-to-Broker or
Broker-to-Signer method inventories in parent section 17. If this document and
the parent conflict, the parent controls unless this document is explicitly
ratified as an amendment.

The superseded triad implementability review remains historical inventory
only. It is not normative.

## 2. Goal

After this goal is complete, the production Machine:

1. discovers wallets, keys, credentials, and policy only through authenticated
   Broker public methods or a Machine-owned cache populated from those methods;
2. routes every wallet-controlled signature and every custody mutation through
   Broker, which routes cryptographic effects to Signer;
3. contains no root, child, delegated, venue, payment, recovery, PRF-derived,
   or debug wallet signing key;
4. opens no legacy keystore or daemon approval database;
5. implements no legacy grant, policy-session, ceremony, registration,
   decrypted-signer cache, or hash-only signing path;
6. remains able to read cached public projections and to stage and simulate
   unsigned work when Broker or Signer is unavailable, while signing and
   custody fail closed; and
7. cannot link legacy authority code into any production feature combination
   or release artifact.

The end state is:

```text
Petal / CLI / mounted VFS
          |
          v
       Machine
  public projections,
  staging, simulation,
  execution, broadcast
          |
          v
       Broker
  policy, approval, review,
  authorization, budgets
          |
          v
       Signer
  custody, delegated keys,
  cryptographic signing
```

There is no second authority path hidden behind a feature, degraded startup,
handler-specific adapter, or local developer mode.

## 3. Definitions

### 3.1 Authority code

Authority code is code capable of any of the following:

- generating, importing, decrypting, wrapping, caching, or exporting a wallet
  or delegated private key;
- receiving plaintext PRF output or recovery material;
- deciding that a Machine- or Petal-originated payload is approved;
- minting or consuming an approval, grant, standing session, or signing budget;
- producing a wallet-controlled or delegated-wallet signature; or
- mutating canonical custody or wallet policy state.

Authority code belongs in Broker or Signer according to the parent
responsibility matrix. It must not execute in Machine.

### 3.2 Key material

Key material includes root and child wallet keys, imported keys, Hyperliquid
API-wallet keys, payment keys, decrypted signers, wrapping keys, PRF output,
recovery secrets, and any secret from which those values can be recovered.

The following are not wallet key material and remain permitted in Machine:

- Machine's own authenticated-transport application identity;
- public keys, addresses, `KeyRef` values, signatures, signed payloads, and
  signed public policy snapshots;
- TLS or package-verification material required for Machine-owned networking;
  and
- installer/enrollment tooling credentials when that tooling is not reachable
  from the running Machine or Petals and cannot sign wallet payloads.

### 3.3 Public projection

A public projection is a non-authoritative Machine view built from existing
Broker responses such as `WalletPublic`, `KeyPublic`, `CredentialPublic`, and
`SignedPolicySnapshot`. A projection may support display, routing, staging, and
simulation. It never authorizes signing or custody and cannot override a live
Broker decision.

## 4. Current verified baseline

At the inventory baseline:

- `Daemon::from_home_inner` unconditionally constructs
  `bloom_keystore::Keystore`.
- `Daemon` publicly exposes both `keystore` and `signer_cache`.
- Machine opens `auth/auth.sqlite`, constructs
  `StoreApprovalVerifier<KeystoreApprovalSignatureVerifier>`, and passes it to
  `TxEngine` and VFS handlers.
- `InMemoryGrantStore`, `SignerCache`, and legacy `AuthServices` are built even
  though the key-bearing Petal host is compile-gated.
- the CLI wallet list, address, portfolio, and related reads use the old
  keystore rather than `MachineBrokerClient::{wallets,wallet,keys,key}`;
- the wallet VFS lists wallets and renders address, public key, kind, policy,
  and outbox preparation from legacy keystore records;
- `RequestsHandler`, `HyperliquidHandler`, `StatusHandler`, `PetalTxOutbox`,
  `/next.md`, and background policy consumers receive the old keystore or old
  approval services;
- `wallets/<wallet>/policy-session/*` remains a compiled daemon-owned approval
  and standing-session surface;
- `EvmOutboxProjection` uses the old approval database as an action-ID index;
- `build_write_daemon` responds to Broker configuration failure by constructing
  the legacy daemon composition;
- `local-integration` and `unsafe-debug-signer` retain Machine-owned signer
  paths; and
- `HyperliquidHandler` generates, persists, decrypts, and uses
  `EphemeralAgentKey` inside Machine. This is signing authority even though the
  master-wallet `approveAgent` signature already passes through Broker.

The normal and `--no-default-features` production dependency graphs still
contain `bloom-keystore`, `bloom-auth`, `bloom-auth-api`, `bloom-vfs`, and
`bloom-tx`. The first three therefore cannot be treated as test-only today.

This inventory is a starting point, not a closed list. Phase M0 must regenerate
it from source and Cargo metadata before edits begin.

## 5. Normative end-state invariants

### MI-01: one authority path

Every production signing or custody request originating from CLI, VFS, Petal,
request execution, transaction execution, venue integration, background job,
or recovery tooling goes Machine to Broker and, where a cryptographic effect
is required, Broker to Signer.

Machine has no Signer client, Signer credential, direct Signer socket path, or
backend handle.

### MI-02: no key-bearing Machine

No production Machine object, field, closure, task, handler, store, or feature
may contain key material as defined in section 3.2. This includes scoped and
short-lived delegated keys. `EphemeralAgentKey`, `PrivateKeySigner`, local
signer implementations, PRF decryption, and equivalents are forbidden in the
production Machine dependency graph.

### MI-03: Broker and Signer are authoritative

Signer is the sole writer of key registry, custody, credential wraps,
derivation registry, and canonical policy. Broker is the sole owner of Sealed
Approval policy evaluation, lifecycle, limits, reservations, and ceremony
review. Machine stores projections only.

### MI-04: projections cannot authorize

Machine may use a current or cached policy projection to produce advisory
plans and early denials. Final authorization always uses Broker's independently
verified snapshot. A stale, altered, missing, or rolled-back Machine cache can
damage availability or presentation but cannot produce a signature or custody
effect.

### MI-05: unavailable means degraded, not legacy

Broker or Signer unavailability preserves public cached reads, unsigned
staging, and simulation where inputs are available. It denies signing,
broadcast that requires a new signature, approval mutation, policy mutation,
and custody. Machine reports the unavailable authority edge explicitly. It
never constructs an in-process verifier, signer, keystore, or approval store.

### MI-06: no production debug authority

The production Machine has no `local-integration` or `unsafe-debug-signer`
feature capable of restoring authority. Deterministic test credentials and
real-passkey developer workflows run as separate harnesses against the real
Broker and Signer protocols.

### MI-07: clean break from legacy authority state

Production Machine does not open, create, migrate, or write the old keystore,
`auth/auth.sqlite`, approval challenge artifacts, signer cache, or
policy-session state. There are no deployed users requiring an authority-state
migration. Old files may be ignored or diagnosed as obsolete, but their
presence must not alter runtime behavior.

## 6. Machine public projection architecture

### 6.1 Internal interfaces

Machine introduces key-free internal interfaces, with concrete names chosen by
implementation but responsibilities fixed here:

```text
WalletProjectionReader
  list_wallets()
  get_wallet(wallet_id)
  list_keys(wallet_id)
  get_key(wallet_id, key_id)
  list_credentials(wallet_id)
  get_policy(wallet_id)

AuthorityClient
  existing MachineBrokerClient signing, approval, policy,
  ceremony, custody, key, and credential operations

ProjectionStore
  cache authenticated public responses with source version,
  digest, observed time, and freshness state
```

These are Machine-internal seams, not new wire methods. Implementations use the
existing parent section 17.1 Broker methods. Missing convenience methods on
`MachineBrokerClient` are wrappers over existing request variants; this goal
adds no Broker or Signer RPC methods.

### 6.2 Projection contents

The cache may contain only:

- wallet ID, public address, wallet kind, backend ID, and public status;
- public keys, public derivation metadata, and opaque `KeyRef` values;
- public credential descriptors with no PRF, wrap, authenticator secret, or
  recovery secret;
- signed canonical policy bytes, version, digest, signing key ID, and
  verification status;
- public ceremony/status fields already permitted by the parent; and
- source service version, response digest, observation time, and stale marker.

The cache must not contain Broker approval secrets, Signer locators not already
present in a public `KeyRef`, encrypted private-key blobs, credential wraps,
recovery ciphertext, signing nonces, or material useful to reconstruct a key.

### 6.3 Refresh and offline behavior

On an authenticated Broker connection, Machine refreshes projections and
atomically replaces a wallet's cached generation. It never merges a lower
policy version over a higher one. Deleted Broker wallets become tombstoned in
the projection rather than silently resurrected from cache.

When Broker is unavailable:

- cached entries are readable and visibly marked stale;
- an uncached wallet or key returns the existing `SERVICE_UNAVAILABLE` rather
  than falsely reporting that the entity does not exist;
- staging and simulation may continue using public data, but results are
  labelled advisory and unsigned; and
- all authority mutations and signing fail closed.

## 7. Surface migration contract

| Surface or consumer | Required target |
|---|---|
| CLI wallet list/address/portfolio/public key | `WalletProjectionReader`; live Broker refresh when available |
| Wallet VFS discovery and public files | public projection only; no `Keystore` field or fallback |
| Wallet policy read/update | `policy.read`, `policy.validate_update`, completed ceremony receipt, then `policy.commit_update`; no direct policy writer |
| Transaction staging and outbox | explicit public wallet/policy projections for planning; `signing.sign` or `signing.sign_batch` for final bytes |
| Petal payload signing | payload-bearing Broker signing only; v0.1 remains fail-closed |
| Petal transaction outbox | key-free wallet projection and Broker authority client |
| Paid HTTP/x402/MPP requests | Broker approval and signing; no `PetalHost::sign_hash`, old grant, or keystore lookup |
| Hyperliquid master-wallet actions | exact Broker approval and Signer signature |
| Hyperliquid API-wallet session actions | Signer-owned delegated `KeyRef`; Machine never generates or opens an API-wallet key |
| `/next.md`, status, bump scanner, watchers | projection reader; stale/unavailable states explicit |
| Central outbox action-ID allocation | Machine-owned operation index with no approval or grant semantics |
| Custody CLI/VFS | existing Broker custody prepares, shared ceremony status/cancel, and custody result |
| Approval lifecycle CLI/VFS | existing Broker Sealed Approval methods only |

Handlers receive the narrow interfaces they need. Passing a broad `Daemon`,
`Keystore`, `AuthServices`, signer cache, or private signer into a production
handler is prohibited.

## 8. Legacy policy-session disposition

The pre-triad `wallets/<wallet>/policy-session/*` model is removed. It is not
retained as an alias and is not migrated into Broker state. Its `use` operation
has no target equivalent: Broker consumes a Sealed Approval as part of the
normal signing operation.

If the mounted VFS continues to expose approval management, its canonical
projection is:

```text
/wallets/<wallet>/sealed-approvals/new.json
/wallets/<wallet>/sealed-approvals/active.json
/wallets/<wallet>/sealed-approvals/<approval_id>/status.json
/wallets/<wallet>/sealed-approvals/<approval_id>/limits.json
/wallets/<wallet>/sealed-approvals/<approval_id>/renew
/wallets/<wallet>/sealed-approvals/<approval_id>/revoke
/wallets/<wallet>/sealed-approvals/revoke_all
```

Each file is a projection or adapter over the existing Broker methods
`sealed_approval.prepare`, `status`, `list`, `limit_state`, `renew`, `revoke`,
and `revoke_all`. Ceremony URLs and expiry follow parent AC-26. Machine stores
no independent approval state.

The CLI follows the same vocabulary. “Grant,” “standing session,” and
“policy-session” disappear from production help, docs, errors, and artifacts.

## 9. Delegated venue keys

An exchange API wallet or other delegated signer is still a private signing
key and therefore belongs to Signer.

For Hyperliquid, the target flow is:

1. Machine requests a unique, policy-allowed Signer-owned child key through
   the existing key preparation/derivation surface and receives only its
   public `KeyRef` and address. A venue agent address is never reused after
   deregistration; each session receives a fresh derivation path.
2. Machine constructs the exact `approveAgent` payload naming that address.
3. Broker obtains the user-approved owner signature through the normal exact
   signing path.
4. Subsequent agent actions use Broker `signing.sign` with the delegated
   `KeyRef`, the active Sealed Approval, operation identity, and exact digest.
5. Session stop or expiry revokes the approval at both Broker and Signer, so
   the delegated `KeyRef` cannot sign another operation without a new explicit
   authorization. Venue-side same-name replacement or expiry removes standing
   venue authority according to the venue contract; replacing a named venue
   slot uses a fresh derived address and must not cause Machine to reuse an old
   private signer.
6. Machine may persist public session metadata and signed venue responses, but
   never the delegated private key or a decryptable blob containing it.

If the existing key derivation and signing contracts cannot represent a
short-lived delegated key without weakening the parent invariants, this is a
real protocol blocker and requires a reviewed parent-spec amendment. It is not
permission to leave `EphemeralAgentKey` in Machine.

The same rule applies to x402, MPP, exchange agents, builder keys with signing
authority, and future venue-specific delegated credentials.

## 10. Transaction and request engine extraction

`bloom-tx` may continue to construct unsigned payloads, calculate signing
digests, attach returned signatures, serialize signed transactions, and
broadcast. Its production code must not:

- import or construct `alloy::signers::local::PrivateKeySigner` or an
  equivalent private signer;
- call a local `sign_hash_sync` path;
- accept an `ApprovalVerifier`, `AuthStoreWriter`, legacy `PetalHost`, or
  policy-session store; or
- select an unsafe signing fallback when Broker is unavailable.

Tests may use private signers from dev-dependencies to verify encoding and
vectors. Such test helpers must not be reachable from a production feature or
binary.

Paid-request and transaction state stores remain Machine-owned because they
describe intended and external effects. Approval state does not. The old
`auth.sqlite` action-ID mapping is replaced with a purpose-specific
Machine-owned operation index whose schema contains no approval, challenge,
credential, grant, or signing-secret fields.

## 11. Developer and integration workflows

Removing embedded authority must not remove the ability to test mounted VFS,
Petals, real passkeys, and bounded mainnet flows locally.

The replacement is an out-of-process triad development harness that:

- starts the production Machine, Broker, and Signer protocol implementations
  under temporary developer-owned roots;
- may run all three under one login UID and therefore makes no production
  principal-isolation claim;
- uses the real Broker ceremony origin and real Signer custody/signing path;
- supports a genuine browser passkey for manual tests;
- mounts the VFS and drives Bloom only through that mounted filesystem where a
  mounted test is requested;
- preserves the bounded Hyperliquid and Polymarket mainnet safety profile; and
- never compiles private-key custody or approval verification into Machine.

`bloom-broker-debug-driver` remains the deterministic software-credential
harness required by parent section 24. It does not replace the manual real
passkey harness, and neither harness may be packaged in production.

Only after both deterministic and real-passkey replacement workflows pass may
Machine's `local-integration`, `unsafe-debug-signer`, ceremony server,
registration coordinator, sealed ceremony, signer cache, and hash-signing
modules be deleted.

## 12. Work packages

### M0 — freeze and inventory

- Record the source revision and every production feature combination.
- Generate reverse dependency and source-use inventories for
  `bloom-keystore`, `bloom-auth`, `bloom-auth-api`, private signer types,
  `AuthServices`, legacy `PetalHost`, and `EphemeralAgentKey`.
- Add failing release gates before removing code.

Completion: the inventory is exhaustive enough that adding a new forbidden
dependency or marker fails CI/local release validation.

### M1 — public projection extraction

- Implement the key-free projection interfaces and cache.
- Add missing `MachineBrokerClient` convenience wrappers over existing RPCs.
- Move CLI wallet reads and Wallet VFS public reads to projections.
- Move status, `/next.md`, portfolio, watchers, bump scanner, and Petal public
  lookup to projections.

Completion: a wallet created in Signer appears in CLI and mounted VFS without
any legacy keystore record; stale-cache behavior passes.

### M2 — execution consumer extraction

- Remove keystore and legacy auth inputs from transaction staging, outbox,
  Petal transaction handling, paid requests, and Hyperliquid master actions.
- Replace the auth-backed action-ID map with the Machine operation index.
- Prove every final signing digest uses Broker.

Completion: request, transaction, Petal, and background execution suites pass
with no legacy authority store present on disk.

### M3 — delegated-key extraction

- Move Hyperliquid API-wallet keys and every other delegated signer to Signer
  `KeyRef` ownership.
- Route delegated signing through Broker.
- Remove decryptable delegated-key blobs from Machine storage.

Completion: bounded Hyperliquid session and order tests pass while memory,
filesystem, dependency, and fault-injection tests prove Machine never receives
the delegated private key.

### M4 — approval and policy-session removal

- Add canonical Sealed Approval VFS/CLI projections where product surfaces are
  retained.
- Delete `policy-session/*`, old challenges, grant stores, `AuthServices`,
  `StoreApprovalVerifier`, `KeystoreApprovalSignatureVerifier`, and
  `auth.sqlite` use.
- Remove legacy approval branches from `TxEngine` and request handlers.

Completion: Broker restart/reconciliation tests preserve approval state while
Machine restart loses no authority state because Machine owns none.

### M5 — developer harness migration

- Make deterministic Broker debug-driver coverage replace daemon signer tests.
- Make the out-of-process real-passkey mounted integration pass for Petals,
  Hyperliquid, and Polymarket.
- Delete Machine embedded ceremony, registration, signer-cache, local PRF, and
  unsafe debug signer code and features.

Completion: both developer workflows pass without a Machine key-bearing
dependency.

### M6 — dependency and artifact purge

- Remove `bloom-keystore`, `bloom-auth`, and `bloom-auth-api` from every normal
  dependency path into production Machine binaries and libraries.
- Move any still-useful key-free DTOs to neutral crates rather than retaining
  a legacy authority dependency.
- Strengthen AC-04 release validation with Cargo metadata, feature-matrix,
  symbol/marker, and runtime negative tests.
- Delete obsolete state writers and documentation.

Completion: all acceptance criteria in section 14 pass on the packaged bundle.

Dependencies:

```text
M0 -> M1 -> M2 -> M3 -> M4 -> M6
                 \         /
                  -> M5 --
```

M1 and M2 may migrate consumers incrementally, but no legacy authority
component is deleted until its final consumer has moved. M6 is the point at
which absence, rather than routing preference, becomes mechanically enforced.

## 13. Explicit removals

Subject to source drift, completion deletes or production-gates away all of
the following from Machine:

- `Daemon.keystore`, `Daemon.signer_cache`, and `Daemon.auth_services`;
- unconditional `Keystore`, `AuthStore`, `StoreApprovalVerifier`,
  `InMemoryGrantStore`, and `SignerCache` construction;
- `registration.rs`, `sealed_ceremony.rs`, `sign_hash.rs`, and the Machine
  ceremony server;
- `local-integration` and `unsafe-debug-signer` authority features;
- legacy `PetalHost::sign_hash` and hash-only production adapters;
- `wallets/<wallet>/policy-session/*` and its documentation;
- `auth/auth.sqlite` and old challenge/grant/session artifacts;
- `EphemeralAgentKey` and sealed delegated-key storage in Hyperliquid VFS;
- private signer fields and local signing branches in `bloom-tx`;
- Keystore parameters on Wallets, Requests, Hyperliquid, status, Petal outbox,
  and background consumers; and
- the Broker-unavailable construction branch that restores the legacy
  composition.

This list does not authorize removal of unsigned transaction construction,
chain RPC, simulation, broadcast, public signature handling, Machine audit,
Petal execution, VFS, request state, or public projection caching.

## 14. Acceptance criteria

- **MA-01 — dependency graph:** Every packaged production Machine feature set
  has no normal or build dependency path to `bloom-keystore`, `bloom-auth`, or
  `bloom-auth-api`. Dev-dependencies are absent from production artifacts.
- **MA-02 — no private signer types:** Production Machine source and symbols
  contain no local private signer, `EphemeralAgentKey`, signer cache, PRF
  decryptor, recovery secret, or key-wrap implementation.
- **MA-03 — projection fidelity:** Wallet registration, import, deletion,
  credential changes, key derivation, and policy updates become visible through
  CLI and mounted VFS solely from Broker projections, including across Machine
  restart.
- **MA-04 — projection rollback safety:** Altered, stale, rolled-back, missing,
  and partially written Machine projections cannot authorize signing or
  custody and are visibly stale or rejected.
- **MA-05 — degraded operation:** With Broker stopped, cached public reads,
  staging, and simulation work where their public inputs exist; every signing,
  approval, policy, and custody mutation fails promptly and reports the
  unavailable authority edge.
- **MA-06 — no fallback construction:** Broker authentication/configuration
  failure never opens a keystore, auth database, verifier, signer, or ceremony
  listener in Machine.
- **MA-07 — signing route matrix:** Automated tests cover transactions, batch
  transactions, Petal payloads, paid HTTP/x402/MPP, Hyperliquid owner actions,
  Hyperliquid agent actions, outbox confirmation, background jobs, and every
  CLI/VFS signing entry point. Each observes a Broker operation ID and Signer
  receipt; direct or hash-only alternatives fail.
- **MA-08 — delegated-key confinement:** Hyperliquid and future delegated-key
  tests prove Machine receives only public `KeyRef` metadata and signatures.
  Machine filesystem and crash dumps contain no delegated key or decryptable
  key blob.
- **MA-09 — legacy state absent:** A clean production start creates no legacy
  keystore, `auth.sqlite`, approval challenge, grant, policy-session, or signer
  cache state. Pre-existing legacy files are not opened or trusted.
- **MA-10 — canonical approval lifecycle:** Every retained approval management
  surface maps to Broker Sealed Approval methods. `policy-session` paths and
  vocabulary are absent from production help, VFS discovery, and binaries.
- **MA-11 — policy custody:** Policy update uses
  `policy.validate_update` as Broker preparation, a completed ceremony receipt,
  and `policy.commit_update`; no direct commit or Machine policy writer exists.
- **MA-12 — developer parity:** Deterministic Broker debug-driver tests and the
  real-passkey out-of-process mounted integration both pass with Machine built
  from the same key-free production code.
- **MA-13 — production artifact gates:** Release validation rejects forbidden
  Cargo dependencies, feature activation, symbols, strings, files, methods,
  sockets, and runtime connector attempts. A short marker list alone is not
  sufficient.
- **MA-14 — parent conformance:** AC-01 through AC-35 are rerun on the bundle.
  In particular AC-02, AC-04, AC-14, AC-24, AC-26, and AC-35 must pass after
  legacy source and dependencies are removed.

## 15. Release-gate requirements

The release build must evaluate the resolved Cargo graph for every production
binary and allowed production feature set. It must fail if a forbidden crate
is reachable even when no forbidden string appears in the final binary.

It must also scan for at least these classes:

```text
bloom-keystore
bloom-auth
bloom-auth-api
KeystorePetalHost
StoreApprovalVerifier
KeystoreApprovalSignatureVerifier
SignerCache
EphemeralAgentKey
PrivateKeySigner
unsafe-debug-signer
local-integration
policy-session
bloom.sign-hash
```

String matching is defense in depth, not the dependency proof. False positives
from user-facing migration diagnostics must be handled through structured
allowlists tied to exact artifact and symbol provenance, not by weakening the
gate globally.

Runtime negative tests launch the packaged Machine with Broker and Signer
stopped or replaced by hostile sockets and prove Machine neither opens legacy
state nor attempts the Signer endpoint.

## 16. Non-goals

- Removing `bloom-vfs`, `bloom-tx`, Alloy transaction encoding, chain RPC,
  simulation, or broadcast from Machine.
- Preventing Machine from receiving public signatures or signed payloads it
  must broadcast.
- Moving Machine-owned action, request, simulation, execution, or external
  effect state into Broker.
- Claiming production process isolation for the same-UID development harness.
- Adding new Machine-to-Broker or Broker-to-Signer methods.
- Preserving legacy approval, policy-session, keystore, or debug-signer state
  compatibility; the parent architecture permits a clean break.
- Deleting useful test vectors before equivalent Broker/Signer coverage exists.

## 17. Stop conditions and implementation discipline

Implementation stops for review only when a normative section cannot be
implemented as written without weakening a parent invariant, changing a wire
method, or assigning authority to the wrong principal. The delegated-key issue
in section 9 is an example of a legitimate protocol blocker if existing
contracts cannot express it.

An underspecified internal return shape, type name, cache layout, error
wording, unstated inconvenience, or scenario already resolved by the parent
threat model is not a stop condition. The implementer chooses the narrowest
conforming design, records it in the implementation log, and proceeds.

No implementation phase may make a temporary production fallback easier to
activate than the code it replaces. Intermediate commits must fail closed.

## 18. Proposed `/goal` prompt

```text
/goal Remove all legacy wallet authority, signing, approval, and key-bearing
state from production Bloom Machine according to
docs/specs/2026-07-31-machine-legacy-authority-removal.md and the normative
triad architecture.

Implement M0 through M6 in order. Keep Broker and Signer as the only production
authorization, custody, and signing path. Replace legacy keystore reads with a
key-free Machine public projection over existing Broker methods; remove the old
policy-session/auth database model; move every delegated venue key, including
Hyperliquid API wallets, into Signer-owned KeyRefs; preserve read/stage/simulate
degraded operation without constructing legacy authority; replace embedded
local integration with out-of-process deterministic and real-passkey triad
harnesses; and enforce absence through dependency, artifact, feature, and
runtime release gates.

Treat M0, M1, M2, M3, M4, M5, and M6 as the major work chunks. After completing
the implementation and local tests for each chunk, stop before beginning the
next chunk and assign a pragmatic reviewer sub-agent to inspect that chunk's
diff, tests, and relevant spec requirements. The reviewer must independently
check spec adherence, security-boundary preservation, regressions, and test
rigor. It must not demand speculative abstractions, unrelated cleanup, or
ceremonial over-engineering. It should report only concrete findings with
severity, evidence, and the violated requirement, or state that no material
findings remain. Address every material finding, rerun the affected tests, and
obtain a clean follow-up review before marking the chunk complete.

Do not add Machine-to-Broker or Broker-to-Signer wire methods without a reviewed
spec amendment. Do not preserve a temporary production signing fallback. Do
not delete a legacy component until its consumer has moved and replacement
coverage passes. Do not change the Polymarket Petal merely to accommodate
Machine extraction; exercise it through its mounted production interface.

Stop only when a normative section cannot be implemented as written without
weakening the parent architecture or changing a frozen protocol. Internal type
names, cache schemas, return shapes, error wording, inconvenient consequences,
and scenarios already decided by the parent spec are implementation decisions:
choose the narrowest conforming option, log it, test it, and continue.

Completion requires MA-01 through MA-14 and a packaged rerun of AC-01 through
AC-35. Report progress by work package and acceptance criterion, not by lines
changed. Do not use GitHub CI as a polling dependency; run platform-independent
tests locally and macOS packaging/isolation tests in the local Tart VM.
```

## 19. Review questions

Ratification should explicitly confirm:

1. the clean break from legacy authority state and `policy-session`;
2. the proposed canonical mounted Sealed Approval paths in section 8;
3. Signer-owned derived `KeyRef` as the required model for Hyperliquid and
   other delegated signing keys;
4. replacement of embedded `local-integration` with an out-of-process real
   passkey triad harness; and
5. removal of all three legacy crates from the normal production Machine graph,
   moving any still-useful key-free DTOs to neutral crates.

## 20. Deferred TODO — edge-owned protocol repositories

After the authority-removal work settles the remaining DTO and delegated-key
contracts, replace the monolithic `bloom-triad-protocol` ownership model with
two edge-owned API packages:

```text
Machine -> bloom-broker-api  (owned and released by bloom-broker)
Broker  -> bloom-signer-api  (owned and released by bloom-signer)
```

Broker must perform an explicit, exhaustive translation between distinct
nominal Machine-facing and Signer-facing types. The Broker API must not simply
re-export Signer request or response types. Machine must not depend directly or
transitively on the Signer repository.

The split should keep truly edge-specific concepts on their edge: Petal claims,
provenance, public projections, and approval proposals belong to the Broker
API; structural signing enforcement, custody contributions, key registry,
policy CAS, and Signer receipts belong to the Signer API. Any shared wire
utility must remain small and mechanical and must not recreate a universal
domain protocol crate under another name.

Before extraction, freeze both edge schemas, canonicalization and conversion
vectors, closed errors, fake-peer suites, and version negotiation. Publish and
pin immutable releases rather than retaining sibling path dependencies. Amend
the parent repository policy before treating this TODO as normative work; it is
not part of M0--M6 unless separately ratified.
