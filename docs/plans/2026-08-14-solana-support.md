# Solana Support as a Verified Chain Petal

**Status:** proposed

**Issue:** [bloom#156](https://github.com/bloom-directory/bloom/issues/156)

**Branch:** `feat/solana-support`

**Depends on:** [BIP-39 multi-curve HD wallets](./2026-08-14-bip39-multicurve-hd-wallets.md)
([bloom#163](https://github.com/bloom-directory/bloom/issues/163))

**Architecture:** [Verified Chain Petals](../architecture/Verified%20Chain%20Petals.md)

## Goal

Ship Solana as Bloom's first verified chain Petal. A default BIP-39 wallet
allocates a hardened SLIP-10 Ed25519 child, the content-addressed Solana driver
Petal constructs and operates a native SOL transfer, and a narrow independent
Broker verifier proves the destination, amount, fee payer, program, signer, and
exact message before any signature is authorized.

Users keep one Bloom wallet identity and the same passkeys used for EVM. Solana
must not provision an unrelated root, expose another mnemonic/private key, or
place Solana RPC/transaction logic in Signer.

The first observable proof is a legacy, single-signer System Program transfer
on a local validator. Devnet is the release smoke test. Mainnet broadcast is
disabled by default in the first slice.

## Why a verified Petal

Putting the driver in a Petal gives Bloom a real extension seam for future
chains and lets Solana RPC, codecs, simulation, transaction construction,
broadcast, status parsing, VFS routes, and application UX evolve outside the
Machine/Broker/Signer release cycle.

An ordinary Petal is insufficient because Broker does not independently infer
economic meaning from a `PetalUseClaim`. A compromised driver could otherwise
label malicious bytes as a harmless transfer. The verified profile contains
that risk:

- Machine injects exact package/route provenance and owns durable lifecycle;
- Broker runs a network-free, digest-pinned semantic verifier over the exact
  payload and selected public `KeyRef`;
- Broker renders and policy-checks independently extracted economic facts;
- Signer binds the exact Ed25519 child, suite, payload, approval, counters, and
  replay identity; and
- the Petal can withhold or misreport an external effect, but cannot change a
  verified destination or amount without invalidating the signature.

## Architectural decisions

### 1. Split driver, platform, verifier, and signer

```text
Solana driver Petal
  codecs, configured RPC, construction, simulation,
  advisory plan, broadcast, status interpretation
        │
        ▼
Machine verified-driver host
  provenance, generic chain action, durable outbox,
  scheduler, network mediation, audit, VFS projection
        │
        ▼
Broker solana-system-transfer-v1 verifier
  independent strict parser, verified economic facts,
  authoritative review and policy inputs
        │
        ▼
Signer
  BIP-39/SLIP-10 child custody, exact Ed25519 signing,
  counters, replay, revocation, receipt
```

The Broker verifier is a reviewed Rust crate compiled into Broker. It is not a
second ordinary Petal in Machine. Signer has no Solana RPC, transaction parser,
program policy, or broadcast code.

### 2. Use the shared HD wallet root

This project consumes bloom#163:

- one non-signable `WalletSeedRef` contains encrypted BIP-39 entropy;
- every passkey independently wraps the same WKEK and unlocks the same accounts;
- Solana uses `bip44-solana-slip10-ed25519-v1` at
  `m/44'/501'/<account>'/0'`;
- Signer owns the derivation registry and allocates a typed child `KeyRef`;
- the child is pinned to `KeySpec::Ed25519`, its public key/fingerprint, allowed
  suites, wallet, root relationship, and derivation profile/path; and
- the root never signs and the Petal cannot submit an arbitrary path.

Adding/replacing a passkey, restarting, restoring a backup, or enabling another
chain must reproduce the same registered Solana address.

### 3. Sign serialized message bytes

Solana requires a 64-byte Ed25519 signature over the serialized transaction
message. Use the existing triad contract:

- `KeySpec::Ed25519`;
- `CryptoSuite::Ed25519Message`;
- raw message input; and
- `Ed25519Raw64` output.

The message digest is bound for operation identity, verification, and audit,
but Signer signs the complete message bytes. There is no hash-only compatibility
route or Petal-owned private signer.

### 4. Independent semantic verification is mandatory

The MVP operation class `solana.native-transfer` requires
`proof_verified` with the exact compiled verifier ID and digest for both exact
and reusable selectors. Exact review prevents later byte substitution; the
verifier prevents a deceptive driver/Machine plan from defining what those
bytes mean.

`solana-system-transfer-v1` establishes:

- canonical complete legacy-message encoding with no trailing bytes;
- signed transaction size within Solana's packet limit;
- exactly one required signer;
- selected Ed25519 child public key as fee payer and transfer source;
- exactly one instruction targeting the System Program;
- exactly the native transfer opcode and canonical instruction length;
- destination public key and lamport debit;
- exact payload digest/ordered signing commitment; and
- rejection of v0/lookup tables, multisigner/partial signing, nonce operations,
  compute-budget instructions, unknown programs, extra instructions, ambiguous
  account roles, or malformed short-vector encodings.

It does not establish cluster identity, blockhash freshness, last-valid height,
fee quote, balance, simulation result, broadcast acceptance, or finality. Those
require current network observations and remain visibly `machine_asserted` in
v1 unless a separate network attestor is added. Policy and UI may not treat
them as verifier-proven.

### 5. Configured RPC, never arbitrary Petal networking

The driver uses a new chain-neutral `bloom:chain/rpc` host interface. It names a
configured Solana profile and allowed JSON-RPC method; Machine owns endpoints,
credentials, failover, genesis-hash checks, response caps, redaction, and
network audit. The Petal never receives endpoint credentials or broad URL
authority.

Read and broadcast capabilities are separate. Broadcast requires a staged
operation ID and exact pinned driver route. Local-validator HTTP is permitted
only by an explicit loopback development profile, not by weakening general
Petal HTTPS policy.

Machine's genesis check protects the honest runtime but is not independent
ClaimAssurance. The Solana message contains a recent blockhash, not a chain ID.

### 6. Machine owns a generic durable chain-action outbox

Do not extend the EVM-specific `bloom:tx.outbox` with optional Solana fields.
Add a chain-neutral versioned operation envelope and driver lifecycle. Machine
persists immutable payload/provenance/verifier commitments, public state,
signature/signed artifact, broadcast attempts, ambiguity, callbacks, and
terminal receipts.

The staged action pins the exact package hash. A driver upgrade cannot take
over pending actions unless an installer-approved successor migration contract
explicitly admits the old package and state schema. Route `write_async` is not
a scheduler; Machine schedules restart-safe, idempotent callbacks.

### 7. Solana-native types stay outside generic authority contracts

The driver and verifier use validated Solana public keys, signatures, hashes,
lamports, slots, block heights, commitments, and message structures. Generic
Machine/Broker envelopes carry versioned bounded bytes and canonical facts;
they do not pretend Solana values are EVM `Address`, numeric `ChainId`, or alloy
transactions.

At generic presentation boundaries use explicit chain family plus CAIP-2 and
CAIP-10 identifiers. Never infer address encoding from a string.

## MVP scope

Included:

- deterministic Ed25519 child allocation through bloom#163 custody;
- canonical base58 account projection;
- signed/content-addressed first-party Solana driver Petal;
- compiled `solana-system-transfer-v1` Broker verifier;
- generic configured-RPC and durable chain-action Petal host services;
- local-validator and devnet profiles with expected genesis hashes;
- health, slot/block-height, fee, SOL balance, latest blockhash, simulation,
  broadcast, and signature-status RPC;
- one legacy, single-signer native SOL transfer through the System Program;
- exact and verifier-backed policy/review binding;
- recent-blockhash and `lastValidBlockHeight` tracking;
- pending/signed/sent/ambiguous/terminal outbox states;
- restart-safe reconciliation and identical-byte retry; and
- existing wallet/chain/outbox public UX plus driver-specific routes.

Deferred:

- SPL Token and Token-2022 balances/transfers;
- arbitrary program instructions or opaque raw transactions;
- version-0 messages and address lookup tables;
- multisigner and partial-signature transactions;
- durable nonce accounts and offline signing;
- compute-budget/priority-fee instructions;
- staking, Solana Pay, NFTs/Metaplex, Jupiter, and other protocol Petals;
- independently attested RPC/genesis/fee/simulation/finality facts;
- additional accounts/paths, hardware wallets, or non-BIP-39 roots; and
- mainnet broadcast by default.

Protocol Petals later build on the same generic driver platform, but each new
operation class needs a verifier contract before Bloom treats its economic
claim as independently established.

## Canonical staged action

The generic outbox stores an immutable envelope equivalent to:

```text
schema = bloom.chain-action/1
operation_id and idempotency_key
driver package_hash, route, ABI version, state schema
wallet_id and exact derived KeyRef
chain profile, family, claimed CAIP-2 identity
operation_class and CryptoSuite
unsigned message bytes and digest
canonical PetalUseClaim
verifier ID, artifact digest, evidence digest
verifier result digest after Broker validation
advisory plan bytes/digest
recent blockhash and claimed lastValidBlockHeight
creation/expiry observations
```

Display files are never execution inputs. Confirm, retry, and reconcile reread
the immutable envelope and signed-artifact record.

## Work sequence

### Phase 0 — Freeze contracts and vectors

1. Ratify the verified chain-Petal architecture and threat model.
2. Freeze driver manifest/catalog fields, WIT versions, chain-action envelope,
   callback lifecycle, state transitions, error classes, and size limits.
3. Freeze verifier input/evidence/output and Broker capability advertisement.
4. Record golden vectors for:
   - Ed25519 public key, canonical SPKI, and base58 address;
   - BIP-39/SLIP-10 path and child key;
   - canonical legacy native-transfer message;
   - 64-byte signature and complete signed transaction;
   - `PetalUseClaim`, verifier evidence/result, operation digest, approval
     digest, signed-artifact digest, and receipt.
5. Add negative vectors for changed package/route, path/KeyRef, suite, fee payer,
   destination, lamports, program, instruction count/data, message/blockhash,
   verifier ID/digest/result/evidence, claim, cluster profile, signature slot,
   and signed artifact.
6. Define verified and asserted review fields explicitly.

Gate: Petal/Machine/Broker/Signer share canonical envelope vectors; verifier
vectors use an independent parser and reject every mutation.

### Phase 1 — Release the HD/Ed25519 Signer edge

This is delivered through bloom#163 and released before Solana integration.

1. Implement `bip39-multicurve-v1` and hardened
   `bip44-solana-slip10-ed25519-v1`.
2. Allocate/retire registry-bound children and expose public descriptors.
3. Activate through the existing WKEK/passkey/recovery path.
4. Support exact Ed25519 message signing, local verification, backup/restore,
   deletion, restart, and audit.
5. Advertise Ed25519 capabilities only when the backend implements them.
6. Deny root signing, arbitrary/unhardened/out-of-namespace paths, tombstones,
   cross-profile/suite requests, and changed public fingerprints.

Gate: official vectors, same-address multi-passkey/restore tests, cross-suite
denials, backend conformance, and zeroization-oriented tests pass.

### Phase 2 — Broker/Signer derived-account integration

1. Allocate the Solana child through bloom#163's authority-changing custody
   ceremony or explicit bounded allocation policy.
2. Bind wallet/root relationship, derivation profile/path, Ed25519 key spec,
   suites, namespace, and committed public projection.
3. Project base58 and CAIP account identities without secret material.
4. Render “Add Solana account / Ed25519” and fail on any returned mismatch.
5. Publish synchronized API releases and advance exact pinned edges.

Gate: Machine can request/project the deterministic child and Broker can
exact-sign a known message; second passkey and restored backup reproduce the
same address.

### Phase 3 — Generic verified chain-driver platform

#### Petal package and WIT

1. Add installer-validated driver metadata: family/networks, ABI/state schemas,
   operation classes, required verifiers, account specs/suites, RPC method
   classes, callback routes, and all size ceilings.
2. Add a `bloom:chain/rpc` component interface for configured reads,
   simulation/fees, broadcast, and status calls with separate capabilities.
3. Add a chain-neutral outbox interface for stage/inspect/confirm/cancel and
   bounded reconciliation callbacks. Do not expose Broker URLs to the guest.
4. Preserve host-injected package/route provenance on every call.

#### Machine lifecycle

5. Implement immutable chain-action persistence, central-outbox identity,
   exact retry, package pinning, signed-artifact storage, transition audit,
   expiry jobs, callback scheduling, and public projection.
6. Make restart recovery deterministic at pre-approval, awaiting ceremony,
   approved, pre-sign, post-sign, pre-broadcast, ambiguous, and reconciling
   boundaries.
7. Require identical signed bytes for retry and retain ambiguous reservations.
8. Define explicit old-package/successor migration behavior.

#### Broker verifier edge

9. Extend verifier input with exact payload, claim, provenance, selected public
   `KeyRef`, operation class, suite, and evidence.
10. Return canonical verified/asserted fact sets and result digest.
11. Build authoritative review and policy inputs only from verified output.
12. Bind verifier ID/artifact/result digests into approval/use/receipts.
13. Require a verifier for an operation class without fallback, including exact
   selectors.

Gate: a fixture driver cannot obtain a signature after lying about a verifier-
covered field; driver/Machine crashes and package replacement cannot duplicate
authority, lose a signed effect, or change the pending package.

### Phase 4 — Solana driver Petal and verifier

#### Driver Petal

1. Create a first-party signed Petal package/repository with routes for account,
   balance, stage, plan, confirm, inspect, and receipt workflows.
2. Use a minimal WASM-compatible Solana codec/build dependency set. Complete a
   spike before pinning Anza crates to prove component target compatibility,
   Rust/toolchain support, deterministic builds, dependency cost, and release
   targets.
3. Implement configured RPC calls:
   `getGenesisHash`, `getHealth`, `getSlot`, `getBlockHeight`, `getBalance`,
   `getLatestBlockhash`, `getFeeForMessage`, `simulateTransaction`,
   `sendTransaction`, and `getSignatureStatuses`.
4. Build and assemble only legacy, one-signer System Program transfers.
5. Emit canonical claim/evidence and clearly label the plan advisory.
6. Handle block-height expiry, exact-byte retry, ambiguous send, status polling,
   and terminal receipt through durable host callbacks.

#### Broker verifier

7. Implement an independent minimal strict parser; do not reuse the driver's
   construction parser as the verification implementation.
8. Establish exactly the v1 fields listed in the architecture contract.
9. Reject unsupported message versions, non-canonical short vectors, trailing
   bytes, oversized artifacts, integer overflow, account-role ambiguity, extra
   instructions/signers, unknown programs, and claim/evidence mismatch.
10. Differential-test accepted messages against the pinned Solana reference
    implementation and mutation-test every byte/field boundary.
11. Register ID/digest/capabilities and written verifier contract in Broker.

Gate: driver-built vectors verify; independently malformed and adversarial
payloads fail; no driver-only fact appears as verifier-established review.

### Phase 5 — Native SOL transfer flow

#### Stage

1. Parse typed cluster, destination, and lamports/SOL input in the Petal.
2. Resolve the exact projected Ed25519 child; validate base58 destination.
3. Query latest blockhash/height and fee through configured RPC.
4. Construct the message, estimate signed size, simulate without signature
   verification, and emit claim/evidence/advisory plan.
5. Machine freezes the generic staged envelope before approval preparation.
6. Broker verifier extracts fee payer/source, destination, lamports, program,
   signer count, and message commitment; Broker renders authoritative facts and
   labels cluster/fee/simulation/freshness as asserted.
7. Policy enforces exact or reusable limits only over verified economic fields;
   fee handling follows the explicitly asserted v1 policy posture.

#### Confirm and sign

1. Reread the immutable staged envelope; never execute display files.
2. In the honest runtime, check current block height against claimed
   `lastValidBlockHeight`. Expiry restages; blockhash is never refreshed behind
   an approval.
3. Resubmit exact payload/claim/evidence to Broker; rerun the verifier on use.
4. Sign with `Ed25519Message`; validate receipts and locally verify the raw
   signature against the pinned child public key.
5. Assemble the complete signed transaction and persist its digest before any
   broadcast.

#### Broadcast and reconcile

1. Broadcast only through the staged Solana profile and pinned driver callback.
2. Persist adjacent intent/result audit; a post-dispatch timeout is ambiguous.
3. Retry only identical signed bytes. Never request another signature for a
   provider timeout.
4. Reconcile by transaction signature through `getSignatureStatuses` until
   terminal, known expired/non-effect, or quarantined.
5. Project processed/confirmed/finalized observations without conflating them.
6. Keep Broker reservations charged for ambiguous effects.

Gate: local-validator tests cover success, verifier/policy denial, cancellation,
expiry, wrong signature, driver crash at every transition, timeout before/after
submission, exact replay, restart, package update, and reconciliation.

### Phase 6 — UX, packaging, and release

1. Add explicit account enablement such as
   `bloom wallet account add <name> --network solana-devnet`; a new default
   wallet may request its first Solana child during registration.
2. Install the driver from an immutable signed release/catalog pin. Never build
   floating Petal source during daemon startup.
3. Add operator-configured local/devnet/mainnet profiles with genesis hashes,
   endpoint bindings, commitments, broadcast policy, and response limits.
4. Expose generic wallet/chain/outbox projections plus driver routes; structured
   accounts carry explicit key/address encoding and CAIP identity.
5. Render verified and asserted facts separately in Broker ceremony, VFS plan,
   CLI, receipt, and audit surfaces.
6. Update root/agent/Petal authoring guidance and capability inventory.
7. Run local-validator CI, opt-in devnet smoke, EVM regressions, release builds,
   dependency/license/advisory scans, and production-artifact verifier scans.

Gate: a fresh install can enable the derived account, install/discover the
driver, fund locally, stage/read/approve/confirm a transfer, and observe a
terminal receipt through documented operations without secret exposure.

## Security and failure tests

Required adversarial cases:

- driver claim says Alice/1 SOL while message says Mallory/all funds;
- advisory plan differs from both claim and verifier extraction;
- selected `KeyRef` differs from fee payer/source;
- wrong package, route, operation class, suite, verifier ID/digest/result, or
  evidence;
- changed message after stage, preparation, approval, or signature;
- unknown/multiple programs or instructions, extra signer, v0/lookup table,
  nonce/compute-budget instruction, malformed short vector, trailing bytes,
  and oversized transaction;
- cluster binding points at a wrong genesis hash;
- false fee/simulation/freshness reports remain labeled asserted;
- driver tries raw `net.fetch`, direct Broker/Signer access, arbitrary path
  derivation, or root signing;
- driver crashes or upgrades before/after every durable transition;
- broadcast returns success then timeout, timeout then inclusion, duplicate
  submission, expiration without inclusion, or conflicting provider status;
- verifier feature omitted from Broker build or digest changed; and
- Machine/Petal tampering cannot cause assurance downgrade or a second
  signature.

## First implementation slice

Do not begin with broad RPC/VFS features. Build one cross-boundary vertical:

1. ratify chain-action, driver manifest/WIT, and verifier contracts;
2. publish golden Solana message/claim/verifier/receipt vectors;
3. release the bloom#163 Ed25519 child edge;
4. implement a fixture chain driver plus generic durable Machine outbox;
5. compile a strict `solana-system-transfer-v1` verifier into Broker;
6. prove a fixture can exact-sign the valid vector and cannot sign any semantic
   mutation; and
7. only then add real Solana RPC, broadcast, reconciliation, and public routes.

This proves the security model before dependency-heavy chain code grows around
an unverified or EVM-specific host seam.

## Completion criteria

Solana MVP is complete when:

- a default BIP-39 wallet enables one deterministic derived Solana account;
- every active passkey and restored backup reproduce the same address;
- the signed/content-addressed Solana Petal owns chain-specific orchestration;
- Machine owns generic durable driver state, scheduling, RPC mediation, audit,
  and public projections;
- Broker independently verifies every authoritative economic fact and labels
  network-only observations asserted;
- exact and reusable approvals cannot bypass or downgrade the required verifier;
- Signer signs only the exact verified legacy message with the exact child;
- a native transfer broadcasts/reconciles restart-safely through a local
  validator and opt-in devnet;
- changed semantics, unsupported forms, wrong cluster in the honest runtime,
  unavailable verifier, ambiguous effects, and package upgrades fail closed;
- mnemonic/seed/WKEK/PRF/private child never enter Machine, Broker, or Petal;
  and
- existing EVM, Sealed Approval, Petal, outbox, packaging, and release behavior
  remains green.

## References

- [Verified Chain Petals](../architecture/Verified%20Chain%20Petals.md)
- [BIP-39 multi-curve HD wallets](./2026-08-14-bip39-multicurve-hd-wallets.md)
- [Triad process architecture](../specs/2026-07-23-triad-process-architecture.md)
- [Sealed Approvals](../architecture/Sealed%20Approvals.md)
- [Wallet architecture](../architecture/Wallet.md)
- [Petal authoring](../petals/authoring-petals.md)
- [Solana transaction structure](https://solana.com/docs/core/transactions)
- [Solana `getLatestBlockhash`](https://solana.com/docs/rpc/http/getlatestblockhash)
- [Solana `simulateTransaction`](https://solana.com/docs/rpc/http/simulatetransaction)
- [Solana `sendTransaction`](https://solana.com/docs/rpc/http/sendtransaction)
- [Solana `getSignatureStatuses`](https://solana.com/docs/rpc/http/getsignaturestatuses)
