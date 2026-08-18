# Verified Chain Petals

**Status:** architecture contract for first-party chain drivers

**First consumer:** [Solana support](../plans/2026-08-14-solana-support.md)

**Normative authority:**
[`2026-07-23-triad-process-architecture.md`](../specs/2026-07-23-triad-process-architecture.md)

## Decision

A non-EVM chain integration may be delivered as a content-addressed Petal, but
the Petal is not trusted to define the economic meaning of bytes it asks Bloom
to sign. A **verified chain Petal** combines three independently bounded parts:

1. a chain-driver Petal that implements chain-specific construction, RPC,
   simulation, presentation, broadcast, and receipt parsing;
2. Machine-owned generic driver services for capability mediation, durable
   operation state, scheduling, audit, and public projections; and
3. a reviewed semantic verifier compiled into Broker that independently parses
   the final signing payload and establishes a documented subset of the
   `PetalUseClaim` and review facts.

Signer remains chain-agnostic above its closed cryptographic-suite registry. It
owns keys and signs the exact payload authorized by Broker. A Petal never sees
mnemonic entropy, WKEK, WebAuthn PRF output, or private child keys.

This design keeps protocol velocity in Petals without allowing a compromised
Petal or Machine to lie about verified destinations and amounts under a
reusable approval.

## Authority boundary

| Responsibility | Owner | Security meaning |
|---|---|---|
| HD seed, child derivation, credentials, private keys | Signer | Never available to Petals or Machine |
| Approval, policy, budgets, review, verifier registry | Broker | Only authority that may authorize a Machine/Petal signing request |
| Exact signing, structural counters, replay, revocation | Signer | Independently bounds every authorized signature |
| Petal execution and provenance injection | Machine | Package hash and route are injected, not guest-controlled |
| Durable chain-action state and scheduling | Machine | Availability and public operation history; cannot create signing authority |
| Chain message construction and RPC interpretation | Chain-driver Petal | Untrusted until checked by a verifier; network facts may remain asserted |
| Payload semantic extraction | Broker verifier | Establishes only the fields listed in its reviewed verifier contract |
| Broadcast and reconciliation | Petal through Machine host calls | External effects are audited and restart-safe; they do not widen the signature |

The verifier is not another ordinary Petal in the same Machine process. That
would share the compromise domain with the driver and provide no stronger
ClaimAssurance. It is a reviewed Rust artifact compiled into Broker under the
assurance-verifier registry, or a separately keyed invariant attestor with an
equivalent pinned contract.

Compile-time, feature-gated Broker verifiers are the MVP posture. A future
separately keyed verifier/attestor module is a deliberate extension point, but
it may not become runtime-loadable until its installation authority, sandbox
and resource limits, rollback behavior, digest/version compatibility, and trust
root are independently ratified. Evidence from such a module remains bound to
an exact advertised verifier contract and cannot downgrade a required proof.

## Canonical flow

```text
client writes a typed chain intent
        │
        ▼
chain-driver Petal
  resolves public account
  queries configured RPC binding
  constructs and simulates unsigned payload
  emits payload + claim + evidence + advisory plan
        │
        ▼
Machine generic chain-action host
  injects package/route provenance
  persists immutable staged envelope
  asks Broker to prepare or use approval
        │
        ▼
Broker semantic verifier
  parses exact payload independently
  checks selected KeyRef/public account
  extracts verified facts
  compares claim and evidence
  builds authoritative review/policy inputs
        │
        ▼
Broker policy and Sealed Approval
  binds package, route, operation class,
  verifier ID/digest, exact payload and verified facts
        │
        ▼
Signer
  checks suite/key/approval/replay/counters
  signs exact payload
        │
        ▼
chain-driver Petal through Machine
  assembles signed artifact
  broadcasts through configured binding
  reconciles via durable callbacks
```

No stage, approval, or signature implies broadcast. Broadcast is a separate,
audited Machine operation over the persisted staged action.

## Chain-driver Petal contract

### Identity and installation

A driver is an ordinary signed, content-addressed Petal package with additional
driver metadata committed by its installer-signed catalog record:

- chain family and supported CAIP-2 networks;
- driver ABI version;
- operation classes;
- required cryptographic suites and account key specs;
- required verifier IDs and digests by operation class;
- RPC bindings and allowed method classes;
- durable callback routes and compatible state-schema versions; and
- maximum request, response, payload, evidence, and signed-artifact sizes.

The installed package hash and route are injected by Machine and bound into the
staged action, approval provenance, `PetalUseClaim`, verifier input, audit, and
receipt. A package update cannot silently take over pending actions.

### Guest responsibilities

The driver may:

- encode and strictly decode its chain's public types;
- query only declared daemon-owned RPC bindings;
- construct an unsigned payload from typed input;
- request simulation and fee/block-lifetime observations;
- emit an advisory plan and a canonical `PetalUseClaim`;
- provide verifier evidence in the verifier's versioned schema;
- request a signature through `bloom:sign/signing@0.2.0` using an exact typed
  `KeyRef` and full preimage;
- assemble the signed wire artifact;
- broadcast only through the staged action's binding; and
- interpret status responses during Machine-scheduled reconciliation.

The driver may not:

- create/import/export/unlock a wallet or select a private derivation path;
- supply its own package or route identity;
- contact Broker or Signer;
- downgrade the required verifier or ClaimAssurance;
- mutate an immutable staged payload after approval preparation;
- treat its own plan, simulation, fee quote, or claim as Broker-verified; or
- run an untracked background task outside the durable driver lifecycle.

## Generic Machine services

The current Petal ABI has payload-bearing signing, host-injected provenance,
scoped HTTP, private storage, and an EVM-specific transaction outbox. Verified
chain Petals require two additional chain-neutral services.

### Configured RPC transport

Add a versioned `bloom:chain/rpc` host interface. A driver names a configured
chain profile and an RPC method; it never supplies endpoint credentials or an
arbitrary URL. Machine resolves the profile to operator-configured endpoints,
verifies the expected genesis/network identity in the honest runtime, applies
method and response-size allowlists, redacts configured credentials, and
records adjacent network-intent/network-result audit entries.

Read methods and broadcast methods are separate capabilities. Broadcast takes
a staged operation ID and signed artifact, checks that the caller is the exact
pinned driver route, and records the artifact digest before network dispatch.
Local-validator HTTP is allowed only through an explicit loopback development
profile; it is not a general relaxation of Petal `net.fetch` HTTPS policy.

Machine endpoint checks improve honest-runtime safety but do not raise
ClaimAssurance, because Machine is inside the compromise domain being
contained. Network identity needs separate attestation if policy requires it
to be independently established.

### Durable chain-action outbox

Add a versioned chain-neutral outbox instead of copying the EVM transaction
engine into each Petal. Its immutable staged envelope contains at least:

```text
operation_id
driver package hash and route
driver ABI/state schema versions
wallet and exact KeyRef
chain profile and claimed CAIP-2 identity
operation class and CryptoSuite
unsigned payload bytes and digest
canonical PetalUseClaim
verifier ID, digest, and evidence digest
advisory plan and digest
expiry/liveness observations
creation time and idempotency key
```

Machine owns pending/signed/sent/ambiguous/terminal state, exact retry identity,
expiry scheduling, callback invocation, signed-artifact persistence, network
audit, and public VFS projection. The Petal owns chain-specific parsing but not
the state transition journal.

Pending operations pin the exact package hash. Upgrade uses one of:

- finish/reconcile with the old installed artifact;
- an installer-approved successor whose migration contract explicitly accepts
  the old driver/state schema; or
- fail closed and require an operator-visible recovery action.

Machine owns the migration state machine and invariant enforcement; the
installer/catalog authority signs each admitted old-package-to-successor
relationship. Only drafts that have not reached approval preparation may pass
through a successor's deterministic state adapter. Awaiting-ceremony,
approval-prepared, approved, pre-sign, signed, sent, or ambiguous actions remain
pinned to the old package and verifier commitments; they complete or cancel
where safe with the old artifact, or enter operator-visible quarantine. Broker
must continue to advertise every verifier ID/digest required by an in-flight
approval or signing action, and migration never changes immutable payloads,
signatures, reservations, provenance, or verifier commitments.

Best-effort detached route execution is not a scheduler. Reconciliation must be
driven by durable Machine jobs that can resume after restart and invoke bounded
driver callbacks with idempotent inputs.

## Broker semantic verifier contract

Each verifier has a stable ID, compiled-artifact digest, maximum input sizes,
canonical input/evidence schemas, deterministic output schema, and written
security contract. Broker capabilities advertise the exact ID/digest set.

Verifier input includes:

- exact payload bytes and payload digest;
- canonical `PetalUseClaim`;
- host-injected package/route provenance;
- approval operation class and requested CryptoSuite;
- trusted public description of the selected `KeyRef`; and
- versioned evidence supplied by the driver.

Verifier output separates:

```text
verified facts
asserted-but-unverified facts
rejection reasons
canonical verifier result digest
```

Broker derives authoritative review and policy inputs from verified output, not
from Petal Markdown or duplicate claim fields. Every claimed verified field
must equal the verifier's extraction exactly. Unknown fields, non-canonical
encodings, trailing bytes, integer overflow, unsupported variants, ambiguous
account roles, or incomplete evidence deny the operation.

Verifier code should be implementation-independent from the driver where
practical. Sharing one parser between construction and verification creates a
common-mode bug. Golden vectors, mutation tests, differential tests against a
reference implementation, and adversarial parser tests are release gates.

## Solana native-transfer verifier v1

The first verifier is `solana-system-transfer-v1`. It accepts only a legacy,
single-signer message containing exactly one System Program transfer.

### Facts it establishes

| Fact | Verification |
|---|---|
| Payload format | Complete canonical legacy message, no trailing bytes |
| Size | Signed transaction will fit the configured Solana packet limit |
| Signers | Exactly one required signer; no partial/multisigner form |
| Fee payer/source | Selected Ed25519 `KeyRef` public key is fee payer and transfer source |
| Program | Exactly one instruction and it targets the System Program |
| Instruction | Exactly the native transfer opcode and canonical data length |
| Destination | Extracted destination public key equals the claim |
| Debit | Extracted lamports equal the claim's SOL debit |
| Message commitment | Payload digest and ordered signing hash match exact bytes |
| Suite | `ed25519-message` with a raw 64-byte signature output |
| Unsupported forms | v0/lookup tables, nonce operations, compute-budget instructions, unknown programs, extra instructions and account ambiguity are denied |

The verified result supplies Broker's authoritative source, destination,
lamports, program, signer count, and message digest review lines.

### Facts it does not establish

| Fact | Why not |
|---|---|
| Cluster/genesis identity | Solana messages contain a recent blockhash, not a chain ID |
| Blockhash freshness and last-valid height | Requires a trusted current network observation |
| Fee quote | Determined by network state, not fully encoded in the message |
| Balance and simulation result | RPC observations outside the static payload |
| Broadcast acceptance/finality | Occurs after signing as an external effect |

Those facts remain visibly `machine_asserted` for v1 unless a separately pinned
network attestor establishes them. The verifier nevertheless denies fee-altering
instruction families in the MVP, which narrows fee risk. Broker review and
policy must never label unverified network observations as verifier-established.

### Implementation home and parser independence

The verifier and its golden vectors live in the `bloom-broker` repo as its own
`bloom-solana` crate, beside the Broker edge that compiles it — Bloom keeps no
copy. The production verifier parses with the Anza `solana-message` and
`solana-transaction` reference crates and requires re-serialization to equal the
input bytes exactly, so construction and verification never share a hand-rolled
parser: the driver Petal builds with the Anza codec, the verifier checks with the
same reference implementation plus a strict shape contract, and the two cannot
develop a common-mode parsing bug. Golden vectors, mutation tests, and
differential tests against the pinned Anza versions remain release gates, and the
verifier corpus digest is published out-of-band so verifier changes are
detectable independent of the Broker build.

## Approval and policy rules

- Production Solana native-transfer operations require
  `proof_verified { verifier_id = solana-system-transfer-v1, ... }` for the
  economic fields the verifier contract covers.
- The immutable approval and each signing use bind the verifier ID, artifact
  digest, result digest, package hash, route, operation class, exact child
  `KeyRef`, suite, and payload commitment.
- Exact one-shot approval still runs the verifier. Exact bytes prevent
  substitution but do not prevent a deceptive plan; authoritative review must
  come from verified extraction.
- Reusable approval is permitted only when wallet policy requires this verifier
  and its value/destination rules rely solely on fields the verifier establishes.
- Declared fee and network-liveness budgets remain asserted unless a separate
  verifier/attestor contract establishes them. Ceremony UI labels the assurance
  of each field rather than assigning one undifferentiated badge to the action.
- A missing, mismatched, disabled, or changed verifier fails closed. There is no
  fallback to `machine_asserted` for an operation class that requires it.

## Broadcast and reconciliation

After signature, the driver assembles the complete Solana transaction. The
signature cannot authorize different message bytes, so a compromised driver
cannot change verified destination or amount without invalidating it. It can
still withhold broadcast, send identical signed bytes multiple times, lie about
an RPC response, or become unavailable; the durable outbox and audit contain
those availability and external-effect risks.

Broadcast follows these rules:

- persist the signature and signed-artifact digest before dispatch;
- only send through the staged chain profile's broadcast binding;
- retry only identical signed bytes under the same operation ID;
- treat timeout after dispatch as ambiguous and reconcile by signature;
- never refresh a blockhash or reconstruct a message behind an approval;
- persist raw bounded provider evidence needed for later diagnosis;
- keep nonterminal operations scheduled until terminal, expired with proven
  non-effect, or explicitly quarantined for operator resolution; and
- never release Broker value reservations merely because the Petal reports an
  error after an ambiguous broadcast.

## Failure containment

| Failure | Required behavior |
|---|---|
| Petal lies about destination or amount | Broker verifier extracts mismatch and denies before signing |
| Machine changes payload after review | Exact payload/verifier/approval digests mismatch and deny |
| Verifier unavailable or digest differs | Deny; no assurance downgrade |
| Petal sends unsupported Solana form | Verifier denies before policy reservation/signing |
| Petal crashes after signing | Machine retains signed operation and resumes pinned callback |
| Timeout after broadcast | Mark ambiguous, keep budgets charged, reconcile by signature |
| Package upgrades with pending actions | Old hash remains pinned or explicit successor migration is required |
| RPC points at wrong cluster | Honest Machine rejects genesis mismatch; independent assurance is not claimed |
| Petal suppresses broadcast | Availability failure only; no alternate signature is issued |
| Petal replays identical transaction | Same Solana signature/operation identity; outbox records idempotent replay |

## Release gates

- content-addressed driver package and installer-signed catalog provenance;
- WIT validation for only the declared driver, RPC, sign, store, and VFS caps;
- no raw endpoint credentials, Broker credentials, Signer connection, or secret
  wallet material in the component;
- canonical ABI vectors shared across Petal/Machine/Broker, without sharing the
  verifier's implementation parser;
- mutation and differential tests for every verified Solana field;
- proof that every signing route requires the compiled verifier ID/digest;
- restart tests at every durable transition, including pre-sign, post-sign,
  pre-broadcast, ambiguous broadcast, and reconciliation;
- package-upgrade tests with pending actions;
- local-validator end-to-end tests and opt-in devnet smoke tests;
- explicit UI labeling of verified versus Machine/Petal-asserted facts; and
- all existing EVM, approval, Petal, custody, packaging, and release tests.

## Consequences

Solana chain logic can evolve and ship as a Petal without placing Solana RPC or
transaction orchestration in Broker or Signer. The trusted computing base still
gains a small Solana-specific verifier, but that code is intentionally narrow,
network-free, deterministic, and unable to broadcast or sign. Adding a new
chain or operation class does not automatically require a verifier; it does
require one whenever Bloom wants policy or review to treat Petal-declared
economic meaning as independently established.
