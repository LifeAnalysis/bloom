# Edge-Owned Protocol Migration

**Status:** Approved for implementation
**Date:** 2026-08-04
**Scope:** Repository and type ownership of the Machine--Broker and
Broker--Signer contracts
**Supersedes:** The deferred TODO in §20 of
`2026-07-31-machine-legacy-authority-removal.md`

## 1. Objective

Replace the monolithic `bloom-triad-protocol` crate with two independently
owned and versioned API packages:

```text
Machine -> bloom-broker-api  (owned and released by bloom-broker)
Broker  -> bloom-signer-api  (owned and released by bloom-signer)
```

The receiving service owns its API. Broker is the only component that consumes
both packages and must explicitly translate between distinct nominal types.
The completed migration deletes `bloom-triad-protocol`.

This is an ownership and boundary-hardening migration. It must not add or
remove product methods, change custody or signing semantics, or introduce a
new universal domain crate.

## 2. Current state

At approval time:

- `bloom-triad-protocol` contains approximately 5,400 lines spanning both
  authority edges, shared domain objects, framing, errors, versioning and
  service traits.
- `bloom-broker-api` exists in the Broker repository but merely re-exports
  `bloom-triad-protocol`.
- No `bloom-signer-api` package exists.
- Machine packages import `bloom-triad-protocol` directly.
- `bloom-triad-local-transport` contains generic Unix transport work as well as
  edge-specific methods, dispatch and protocol types.
- Broker, Signer and their tests use sibling path dependencies into the Bloom
  repository.

The migration must improve this state without duplicating the entire existing
crate into both repositories.

## 3. Normative dependency rules

The final dependency graph must satisfy all of the following:

1. Machine depends on `bloom-broker-api` and must not depend directly or
   transitively on `bloom-signer-api`, Signer backends, or Signer
   implementation packages.
2. Signer depends on `bloom-signer-api` and must not depend on
   `bloom-broker-api` or Broker implementation packages.
3. Broker is the only product component that depends on both edge APIs.
4. Neither API may depend on or re-export the other API.
5. `bloom-broker-api` must not re-export Signer request, response, receipt,
   policy, key, ceremony, error, or capability types.
6. Shared transport code may contain only mechanical wire and local IPC
   concerns. It must not contain wallet, Petal, approval, policy, custody,
   ceremony, credential, key-registry, or signing semantics.
7. No replacement shared package may reconstruct a universal triad domain
   model under another name.
8. Cross-repository production dependencies must use immutable published
   versions or immutable Git revisions/tags. Committed sibling path
   dependencies are forbidden in the completed migration.

## 4. Edge ownership

### 4.1 `bloom-broker-api`

The Broker repository owns the complete Machine--Broker contract, including:

- Machine--Broker method inventory, request and response enums, and typed
  service seam;
- Machine-facing wallet, key and credential projections;
- Machine-facing key references;
- Sealed Approval proposals, public status, renewal, revocation and limits;
- Petal claims, provenance and assurance evidence;
- Machine signing requests, operation status and public results;
- policy read, validation, review and commit projections;
- public ceremony and custody preparation/status/result projections;
- Broker readiness, capabilities, version negotiation and closed errors.

These are Broker API concepts even where their serialized fields resemble
Signer concepts.

### 4.2 `bloom-signer-api`

The Signer repository owns the complete Broker--Signer contract, including:

- Broker--Signer method inventory, request and response enums, and typed
  service seam;
- Signer-facing `KeyRef`, registry operations and derivation descriptions;
- structural signing-enforcement requests and normalized results;
- approval enforcement terms, activation contributions and receipts;
- Signer custody and ceremony prepare/complete/status contracts;
- WebAuthn proof material consumed by Signer;
- Signer policy storage/read/compare-and-swap contracts;
- revocation reconciliation and control contracts;
- Signer readiness, capabilities, version negotiation, backend projections and
  closed errors.

Signer API types must not mention Petal manifests, Machine projections, Broker
policy decisions, or other northbound concepts unless the architecture
explicitly requires Signer to enforce that exact structure.

### 4.3 Mechanical wire and transport

A shared transport layer is permitted only where sharing is genuinely
mechanical. It may own:

- Unix socket IO and OS peer credentials;
- generic authenticated envelopes and canonical envelope bytes;
- frame encoding and hard byte/depth/count limits;
- replay/session mechanics required to authenticate a generic message;
- generic transport failures and connection admission.

It must accept generic serializable request and response bodies. Edge method
inventories, domain error construction, typed dispatch, capability contents and
journal semantics specific to one edge belong with that edge's API or service.
If a proposed shared utility begins accumulating domain types, the utility must
instead be split or the edge types duplicated nominally.

## 5. Broker translation boundary

Broker must translate between the two APIs using explicit, typed conversion
functions grouped by domain, for example:

```text
translation/
  approval.rs
  ceremony.rs
  custody.rs
  error.rs
  key.rs
  policy.rs
  signing.rs
```

The following rules are mandatory:

- Machine-facing and Signer-facing objects are distinct nominal types.
- Type aliases across the boundary are forbidden for domain objects.
- Serde round-tripping is not conversion logic.
- Wildcard matches over security-relevant closed enums are forbidden.
- Broker must validate and canonicalize before constructing Signer enforcement
  terms.
- Broker must not blindly forward Machine-supplied terms to Signer.
- Signer responses must be checked and deliberately projected before crossing
  the Machine-facing boundary.
- Conversions need not be bidirectional when the architecture defines only one
  direction.
- Every security-critical field must be exercised by conversion tests so that
  omission, substitution or unintended defaulting fails.

Representative transformation:

```text
MachineApprovalProposal
        |
        | Broker validates, canonicalizes and renders
        v
SignerApprovalEnforcementTerms
```

## 6. Compatibility and versioning

### 6.1 Frozen v1 behaviour

Before cutting consumers over, freeze the existing serialized behaviour for
both edges:

- every method name and request/response variant;
- canonical JSON and framed bytes;
- digest and signature domains;
- unknown-field rejection and closed-enum behaviour;
- error codes and durable/retry classifications;
- size, depth and collection limits;
- capability and version-negotiation behaviour.

Golden fixtures must cover every request and response variant. The extraction
must preserve those fixtures unless a separate, explicitly approved protocol
change increments the applicable edge version.

### 6.2 Independent edge versions

The single triad protocol version must be replaced by independent negotiation:

```text
Machine--Broker: Broker API major and compatible minor range
Broker--Signer:  Signer API major and compatible minor range
```

Broker holds and negotiates both ranges independently. A Signer API release
must not require a Machine release unless Broker's Machine-facing behaviour
also changes.

## 7. Implementation work chunks

### C0 — ownership inventory and compatibility freeze

1. Classify every public symbol in `bloom-triad-protocol` as Broker API, Signer
   API, mechanical wire, internal implementation detail, or an intentional pair
   of nominal edge types.
2. Record all consumers and eliminate ambiguous ownership before moving code.
3. Complete per-edge golden vectors and fake-peer coverage for v1.
4. Add baseline dependency assertions that describe the current and target
   graphs.

Exit gate: every exported symbol has one recorded disposition, and both edges
have a compatibility oracle.

### C1 — protocol-neutral transport

1. Make local transport generic over serialized bodies.
2. Move edge request enums, method tables, typed service traits and dispatch
   adapters out of the transport package.
3. Map transport failures to each edge's domain error at its adapter boundary.
4. Preserve authentication, peer-credential, replay, framing and journal-head
   security tests.

Exit gate: the transport package has no dependency on either domain API and no
edge-specific method or domain type.

### C2 — Signer-owned API

1. Create `bloom-signer-api` in the Signer repository.
2. Move the Broker--Signer contract and its owned transitive types.
3. Convert Signer implementation and backend APIs where applicable.
4. Convert Broker's Signer client.
5. Prove compatibility against the frozen Broker--Signer vectors.
6. Publish or immutably tag the API and pin Broker to it.

Exit gate: Signer builds and tests without `bloom-triad-protocol` or
`bloom-broker-api`.

### C3 — Broker-owned API

1. Replace the existing universal re-export in `bloom-broker-api` with owned
   Machine--Broker definitions.
2. Move the Machine-facing contract and owned transitive types.
3. Convert Broker's northbound service adapter.
4. Prove compatibility against the frozen Machine--Broker vectors.
5. Publish or immutably tag the API.

Exit gate: `bloom-broker-api` has no dependency on Signer code and exposes no
Signer API type.

After C0, C1, C2 and C3 may be developed in parallel where their files and
release ordering permit. Their final integration still observes the exit gates.

### C4 — explicit Broker translations

1. Introduce domain-grouped conversion modules in Broker.
2. Remove implicit sharing, aliases and re-exports between edge types.
3. Add exhaustive enum and field-level conversion tests.
4. Add negative tests for malformed, inconsistent and security-field-omitting
   inputs and responses.

Exit gate: Broker is the sole dependency join and every cross-edge domain
object crosses through a reviewed explicit conversion.

### C5 — Machine cutover

1. Move `bloom-machine-client`, `bloom-machine`, daemon orchestration, VFS,
   transaction code, Petal integration, CLI and tests onto
   `bloom-broker-api`.
2. Remove direct protocol imports throughout the production Machine graph.
3. Pin Bloom to an immutable Broker API release.
4. Run real-process triad integration tests.

Exit gate: Machine has no direct or transitive Signer API or implementation
dependency and no `bloom-triad-protocol` import.

### C6 — independent version negotiation and mixed-version tests

1. Split version constants and capability negotiation by edge.
2. Test previous/current service-release combinations on both edges without
   weakening the exact authority-wire requirement in §11.3. The generic wire
   range primitive may use synthetic overlapping minor ranges to prove its
   compatibility algorithm, but production Machine--Broker and Broker--Signer
   ranges remain exactly 1.1 and must reject 1.0 as a downgrade.
3. Test incompatible major versions fail before durable work.
4. Ensure a Signer-only compatible release does not force a Machine release.

Exit gate: both edges negotiate independently and fail closed.

### C7 — deletion and release gates

1. Delete `bloom-triad-protocol` and all migration adapters.
2. Remove sibling path dependencies and stale packaging/documentation.
3. Add repository checks rejecting reintroduction of the monolithic crate,
   Broker re-export of Signer types, Machine-to-Signer dependencies, and shared
   domain packages spanning both edges.
4. Run all repository and full-triad test suites.
5. After every C0--C7 exit gate and pragmatic review has passed, commit the
   completed migration in `bloom-signer`, `bloom-broker`, and `bloom`, then
   push all three repositories. Do not publish intermediate broken states.

Release order is Signer API, Broker pinned to Signer API, Broker API and Broker,
then Bloom pinned to Broker API.

Exit gate: the final acceptance criteria in §9 all pass.

## 8. Pragmatic review gate after every chunk

Every completed major chunk C0--C7 must be reviewed by a reviewer sub-agent
before the next dependent chunk is accepted.

The reviewer is instructed to:

1. inspect the actual diff and relevant tests for that chunk;
2. report concrete deviations from this specification with file/line evidence;
3. report test failures, missing negative coverage, accidental compatibility
   changes, dependency leaks and unreviewed security-field conversions;
4. distinguish blocking findings from small follow-ups;
5. avoid speculative redesign, unrelated cleanup, new abstractions, vendoring,
   generated source trees, provenance inventories, or expansion in lines of
   code that is not required by this specification;
6. prefer deletion, direct types and straightforward conversions over framework
   construction;
7. approve the chunk when it meets the specification and its tests are
   proportionate, even if unrelated opportunities for improvement remain.

Concrete findings must be fixed and the affected tests rerun before the chunk
is closed. If review asks for work outside this specification, log and reject
that request unless the user separately approves the scope change.

## 9. Final acceptance criteria

The migration is complete only when:

1. Machine consumes `bloom-broker-api` and no Signer API or implementation.
2. Signer consumes `bloom-signer-api` and no Broker API or implementation.
3. Broker is the only product component consuming both edge APIs.
4. Neither API depends on or re-exports the other.
5. Broker uses explicit nominal conversions for every cross-edge domain object.
6. Shared transport contains no domain semantics.
7. Both edges negotiate versions independently and fail closed.
8. Existing v1 wire and digest vectors remain valid except for separately
   approved versioned changes.
9. `bloom-triad-protocol` and committed sibling path dependencies are gone.
10. No new authority method or behaviour was introduced by the extraction.
11. All tests in Bloom, Broker and Signer pass, including fake-peer,
    compatibility, dependency, mixed-version and real-process triad tests.
12. Repository gates prevent the old ownership structure from returning.
13. The completed, reviewed migration is committed and pushed in all three
    repositories.

## 10. Non-goals and implementation discipline

This migration does not authorize:

- new custody, signing, approval, Petal, policy or ceremony behaviour;
- redesign of cryptography, WebAuthn, persistence, packaging or service
  activation;
- vendoring or copying third-party source;
- generated compatibility layers checked in as large source trees;
- broad formatting or unrelated cleanup;
- retaining the monolithic crate as a compatibility facade after consumers
  have moved;
- creating a large shared `core`, `types`, `common` or `wire` domain package.

Make the smallest direct changes that establish correct ownership. Stop only
for a true contradiction that prevents this specification from being
implemented as written; ordinary extraction details are implementation
decisions to make and record.

## 11. C0 public-symbol disposition

This inventory is normative for the extraction. “Paired” means two distinct
nominal edge definitions with explicit Broker conversion; it does not mean a
shared type alias. “Internal” means the symbol leaves both public APIs.

| Current module | Disposition | Public symbols |
|---|---|---|
| `approval` | Paired Broker/Signer domain types | `ApprovalSubject`, `ClaimAssuranceLevel`, `ApprovalSelector`, `SlidingWindow`, `AssetId`, `ValueLimit`, `ValueWindow`, `ApprovalLimits`, `ActivationMode`, `SealedApprovalTerms` |
| `audit` | Mechanical authenticated-wire type | `SignedJournalHead` |
| `ceremony` | Broker API/public projection | `SealedApprovalPrepareResponse`, `ApprovalPrepareState`, `CustodyPrepareResponse`, `CustodyPrepareState` |
| `ceremony` | Broker implementation internal | `ReviewManifest`, `CeremonySession`, `SignerSessionContribution` |
| `ceremony` | Signer API | `CeremonyPrepareRequest`, `SignerCeremonyContribution`, `SignerPreparedApproval`, `WebAuthnAssertion`, `WebAuthnAttestation`, `WebAuthnCredential`, `CredentialPrfInput`, `CeremonyWebAuthnOptions`, `WebAuthnCeremonyProof`, `CeremonyCompleteRequest`, `SignerActivationReceipt`, `CustodySignerContribution`, `SignerPreparedCustody`, `SignerCeremonyStatus`, `SignerCeremonyPrepareRequest`, `SignerCeremonyPrepareResponse`, `SignerCeremonyCompleteRequest`, `SignerCeremonyCompleteResponse`, `CustodyCompleteRequest`, `CeremonyChallenge`, `CeremonyPhase`, `LocalPrfHpkeAad`, `CustodyHpkeAad`, `CustodyOutputHpkeAad` |
| `ceremony` | Paired Broker/Signer domain types | `CeremonyKind`, `CustodyPrepareRequest`, `CustodyResult`, `CredentialSummary` |
| `claims` | Broker API | `DeclaredDebit`, `DeclaredDestination`, `DeclaredFee`, `ClaimAssurance`, `PetalUseClaim` |
| `codec` | Mechanical wire | `FRAME_MAX_BYTES`, `JSON_MAX_DEPTH`, `JSON_MAX_STRING_BYTES`, `JSON_MAX_LIST_LENGTH`, `encode_frame`, `decode_frame`, `Base64UrlBytes` |
| `codec` | Paired Broker/Signer signing limits and payload | `SINGLE_PAYLOAD_MAX_BYTES`, `BATCH_CHILD_MAX_BYTES`, `BATCH_AGGREGATE_MAX_BYTES`, `BATCH_CHILD_MAX_COUNT`, `SigningPayloads` |
| `codec` | Signer API | `HPKE_ENVELOPE_MAX_BYTES`, `HpkeEnvelope` |
| `crypto` | Paired Broker/Signer domain types | `KEYREF_LOCATOR_MAX_BYTES`, `KeySpec`, `CryptoSuite`, `CryptoInputKind`, `SignatureEncoding`, `DerivationRef`, `KeyRef` |
| `crypto` | Signer API | `EnrolledKeyBinding` |
| `envelope` | Mechanical authenticated wire | `RPC_ENVELOPE_SCHEMA_V1`, `ProtocolVersion`, `EnvelopeKind`, `UnsignedEnvelope`, `SignedEnvelope`, `AuthenticatedPeer`, `TypedRequestMethod` |
| `error` | Paired edge-specific closed contracts | `RetryClass`, `DurableEffect`, `ProtocolErrorCode`, `ErrorContract`, `ProtocolError`, `UnknownPeerErrorCode` |
| `ids` | Mechanical validated wire primitives | `Token`, `Digest32`, `OperationId`, `BootEpoch`, `RequestNonce`, `DecimalU64`, `DecimalU256` |
| `lib` | Paired independent edge versions | `PROTOCOL_MAJOR`, `PROTOCOL_MINOR_MIN`, `PROTOCOL_MINOR_MAX` |
| `methods` | Mechanical transport seam | `ServiceRole`, `ServiceFuture`, `RpcService` |
| `methods` | Broker API | `MachineBrokerMethod` |
| `methods` | Signer API | `BrokerSignerMethod`, `ControlMethod` |
| `petal_key` | Paired Machine proposal/Signer enforcement types | `PetalKeyScope` |
| `policy` | Paired Broker/Signer domain types | `CanonicalWalletPolicy`, `PolicyDestination`, `RequiredVerifier`, `SignedPolicySnapshot`, `PolicyUpdateRequest`, `PolicyCommitReceipt` |
| `policy` | Broker API | `PolicyUpdatePrepareResponse`, `PolicyCommitUpdateRequest` |
| `policy` | Broker implementation internal | `canonical_policy_authority_diff`, `PolicyAuthorityDiff`, `PolicyAuthorityDestination`, `PolicyAuthorityVerifier`, `PolicyUpdateReviewManifest` |
| `policy` | Signer API | `PolicyValidationReceipt`, `PolicyCompareAndSwapRequest`, `PolicyUpdateCeremonyPrepareRequest`, `PolicyUpdateCeremonyCompleteRequest` |
| `provenance` | Broker API | `PROVENANCE_RECORD_SIGNATURE_DOMAIN`, `PROVENANCE_CATALOG_SCHEMA`, `ProvenanceSubject`, `ProvenanceRecord`, `ProvenanceOperationClass`, `ProvenanceFeeAsset`, `ProvenanceCatalog` |
| `revocation` | Paired public projection/Signer authority types | `ApprovalTombstone`, `WalletTombstone`, `RevocationState`, `RevocationSnapshot` |
| `service` | Paired edge-local utility/projection types | `Empty`, `HelloChallenge`, `ReadinessState`, `Readiness`, `BackendPublicCapability`, `VerifierPublicCapability`, `ServiceCapabilities`, `IdRequest`, `WalletRequest`, `WalletOperationRequest`, `KeyRequest`, `RevokeRequest`, `ApprovalLifecycleState`, `ApprovalPublicStatus`, `OperationRequest`, `OperationState`, `OperationPublicStatus`, `WalletPublic`, `KeyPublic`, `CredentialState`, `CredentialPublic`, `CeremonyState`, `CeremonyPublicStatus` |
| `service` | Broker API | `ApprovalPrepareRequest`, `ApprovalRenewRequest`, `ApprovalLimitState`, `MachineBrokerRequest`, `MachineBrokerResponse`, `MachineBrokerService` |
| `service` | Signer API | `CustodyBindOutputRecipientRequest`, `BrokerSignerRequest`, `BrokerSignerResponse`, `ControlRequest`, `ControlResponse`, `BrokerSignerService`, `RevocationControlService` |
| `signing` | Broker API | `MachineSignRequest`, `PetalSignSelector` |
| `signing` | Signer API | `SelectorKind`, `SignOperationIdentity`, `UnsignedSignRequest`, `SignRequest`, `BrokerValidationReceipt`, `BROKER_VALIDATION_RECEIPT_SIGNATURE_DOMAIN` |
| `signing` | Paired public/Signer result | `NormalizedSignature`, `SigningResult` |
| `webauthn` | Signer implementation, with constants available southbound as required | `CEREMONY_ORIGIN`, `CEREMONY_RP_ID`, `VerifiedAssertion`, `verify_webauthn_assertion`, `verify_webauthn_attestation` |

The method enums generated by `method_enum!` are included explicitly above
even though their declarations are macro expansions. Private helpers and impl
methods move with their owning public type unless a later chunk demonstrates
that they are implementation-only.

### 11.1 Current consumers and final disposition

This is the C0 package-level consumer baseline. Test-only source consumers move
with the package named in the same row; they do not establish separate API
ownership.

| Repository | Current direct consumer | Uses | Final dependency |
|---|---|---|---|
| Bloom | `bloom-machine-client` | Machine--Broker client, projections and envelopes | `bloom-broker-api` plus mechanical transport |
| Bloom | `bloom-machine` | Machine--Broker requests, responses and ceremony projections | `bloom-broker-api` |
| Bloom | `bloom-daemon` | Machine-facing requests, projections and errors | `bloom-broker-api` |
| Bloom | `bloom-vfs` | Machine-facing policy, approval, signing and custody types | `bloom-broker-api` |
| Bloom | `bloom-tx` | Machine signing requests/results | `bloom-broker-api` |
| Bloom | `bloom-petals` | Machine-facing Petal selectors and key projections | `bloom-broker-api` |
| Bloom | `bloom` | CLI Machine-facing custody and public projections | `bloom-broker-api` |
| Bloom | `bloom-it` | full-triad fixtures for both edges | both APIs as test-only integration dependencies |
| Bloom | `bloom-triad-local-transport` | authenticated envelopes plus both typed dispatch adapters | mechanical transport only; typed adapters move to their edge owners |
| Bloom | `bloom-audit-checkpoint` | signed journal-head wire representation | mechanical authenticated-wire package |
| Broker | `bloom-broker-api` | currently re-exports the monolith | owned Machine--Broker definitions only |
| Broker | `bloom-broker` | northbound service and southbound Signer client | both edge APIs; sole production dependency join |
| Signer | `bloom-signer` | Broker--Signer service and domain types | `bloom-signer-api` |
| Signer | `bloom-signer-backend-api` | Signer cryptographic/backend public types | `bloom-signer-api` or backend-private definitions |
| Signer | `bloom-signer-backend-local` | backend API transitive and test fixtures | `bloom-signer-api` only where the backend contract requires it |
| Signer | `bloom-signer-backend-aws-kms` | backend API transitive and test fixtures | `bloom-signer-api` only where the backend contract requires it |

The `bloom-service-activation` occurrences are packaging tests rather than a
runtime protocol dependency and must be rewritten to assert the final package
names. Broker and Signer debug/integration binaries inherit their owning
workspace's edge API and do not own additional contracts.

### 11.2 Dependency assertions

The current baseline is asserted by the direct dependency set in §11.1 and by
the C0 repository scan. The target assertions, which become executable release
gates as packages cut over, are:

```text
production(Machine) intersect {bloom-signer-api, bloom-signer-*} = empty
dependencies(bloom-broker-api) intersect {bloom-signer-api, bloom-signer-*} = empty
dependencies(bloom-signer-api) intersect {bloom-broker-api, bloom-broker-*} = empty
production(Broker) contains {bloom-broker-api, bloom-signer-api}
production(Signer) contains bloom-signer-api
direct_consumers(both edge APIs) = {bloom-broker}
all production dependency graphs exclude bloom-triad-protocol
all committed cross-repository dependency sources are immutable
```

Until C7, the C0 direct-consumer list must remain exhaustive: removing a
consumer is expected as it cuts over, while adding a new consumer is a test
failure unless this specification assigns it a final disposition.

### 11.3 Frozen initial edge versions

Authority-edge envelopes require protocol 1.1 because signed sender journal
heads are mandatory and protocol 1.0 is explicitly rejected on those edges.
The release compatibility declaration therefore freezes current/current
authority-service bundles at exactly 1.1 with downgrade and adjacent-version
support disabled. Protocol 1.0 remains accepted only on non-authority control
edges; it is not a Machine--Broker or Broker--Signer compatibility claim.
