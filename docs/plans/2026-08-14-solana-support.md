# Solana Support on the Triad Architecture

**Status:** proposed

**Branch:** `feat/solana-support`

**Supersedes:** the pre-triad implementation assumptions in
[bloom#156](https://github.com/bloom-directory/bloom/issues/156)

## Goal

Ship a first-party Solana wallet path that can create or import an Ed25519
wallet through Broker/Signer custody, read SOL state, stage and simulate a
native SOL transfer, obtain an exact payload-bound approval, sign the frozen
Solana message inside Signer, broadcast it, and reconcile its result through
the existing wallet outbox UX.

The first usable proof is a local-validator end-to-end transfer. Devnet is the
release smoke test. Mainnet broadcast is not enabled by default in the first
slice.

## Architectural decisions

### 1. Solana is a native chain adapter, not a Petal

Chain connectivity, wallet balance, transaction staging, policy evaluation,
signing orchestration, broadcast, reconciliation, and audit are first-party
wallet responsibilities. Protocol applications such as Jupiter, Metaplex, or
staking can be Petals later, built on generic Solana read/signing capabilities.

Putting the base chain in a Petal would make fundamental wallet behavior depend
on a package and would duplicate the root wallet/outbox surface. It would also
blur the rule that Petals receive neither root keys nor Broker credentials.

### 2. Keep the triad authority split intact

- Machine constructs, simulates, displays, and stores only public or unsigned
  state.
- Broker binds the exact Ed25519 key, serialized message, plan facts, policy,
  approval, operation identity, and expiry.
- Signer alone generates/imports/stores the Ed25519 secret and signs the exact
  raw message using `ed25519-message`.
- Machine verifies the returned signature against the projected public key
  before assembling and broadcasting the signed transaction.

There will be no Machine keystore, raw secret import, direct Signer connection,
hash-only compatibility path, or signing fallback.

### 3. One wallet has one root key family in the first release

The current triad contract has one authoritative `root_key_ref` and Broker
fails closed if Signer projects zero or multiple wallet roots. Preserve that
invariant:

- an existing EVM wallet has a secp256k1 root;
- a Solana wallet has an Ed25519 root;
- a wallet exposes only chains compatible with its root key suite.

Do not turn one wallet into a multi-root identity in this project. A later
identity/account grouping layer can associate independently recoverable wallets
without weakening root selection or making suite selection ambiguous.

### 4. Sign serialized Solana message bytes, not a digest

Solana requires 64-byte Ed25519 signatures over the serialized transaction
message. The triad APIs already define `KeySpec::Ed25519`,
`CryptoSuite::Ed25519Message`, raw-message input, and `Ed25519Raw64`; use those
contracts directly.

The staged subject is the exact serialized message. Its review also binds a
SHA-256 digest for operation identity and audit, but Signer signs the message
bytes, not that digest.

### 5. Do not force Solana through EVM types

The current numeric `ChainId`, `ChainSpec`, `alloy::Address`, EVM `ChainClient`,
and EVM transaction engine are not generic chain abstractions. Add explicit
chain-family dispatch and Solana-native types rather than making invalid states
representable with optional EVM fields.

Use a separate `SolanaClusterSpec` initially. It must include a configured
genesis hash, RPC endpoints, commitment, and broadcast policy. Connection setup
must compare `getGenesisHash` with the configured value so an endpoint cannot
silently switch mainnet/devnet/local identity.

## MVP scope

Included:

- managed Ed25519 wallet generation and import through custody ceremonies;
- canonical base58 public key/address projection;
- local validator and devnet cluster configuration;
- cluster health, slot/block-height, fee, SOL balance, and signature status;
- native SOL transfer with one Bloom-managed fee payer/signer;
- recent-blockhash staging with `lastValidBlockHeight` tracking;
- pre-sign simulation, human-readable plan, policy check, exact signing,
  broadcast, and confirmation reconciliation;
- existing pending/sent/failed outbox shape and audit integration;
- EVM behavior and config compatibility.

Deferred:

- SPL Token and Token-2022 balances or transfers;
- arbitrary program instructions or opaque raw transactions;
- version-0 messages and address lookup tables;
- multisigner and partial-signature transactions;
- durable nonce accounts and offline signing;
- staking, Solana Pay, NFTs/Metaplex, Jupiter, and other protocol surfaces;
- Ed25519 hierarchical derivation, hardware wallets, and multi-root wallets;
- mainnet broadcast by default.

These boundaries keep the first signing policy reviewable. Solana transactions
can contain arbitrary program calls, so a generic raw route must not precede
program/account-aware policy and plan rendering.

## Work sequence

### Phase 0 — Freeze contracts and test vectors

1. Record golden vectors for an Ed25519 public key, canonical SPKI, base58
   address, serialized legacy SOL-transfer message, 64-byte signature, signed
   transaction, message digest, and operation digest.
2. Define maximum accepted message size and enforce Solana's complete
   transaction size limit before signing.
3. Define stable cluster identity, commitment, outbox artifact schemas, and
   terminal/retryable RPC error classes.
4. Add cross-suite negative vectors: secp256k1 key with Ed25519 suite, Ed25519
   key with secp256k1 suite, altered message, altered fee payer, altered
   blockhash, wrong signature position, and wrong cluster.

Gate: Broker, Signer, and Machine share the same vectors and reject every
cross-suite or altered-payload case.

### Phase 1 — Signer local Ed25519 backend

This work belongs in `bloom-directory/bloom-signer` and must release before
Bloom pins the new edge.

1. Extend `LocalSignerBackend` root material with a zeroizing 32-byte Ed25519
   seed variant; do not reuse the service/policy Ed25519 identity keys.
2. Implement generate/import, activation, encrypted backup/restore, deletion,
   public description, and exact raw-message signing.
3. Advertise `KeySpec::Ed25519`, `CryptoSuite::Ed25519Message`, message input,
   and raw-64 output only when the backend actually supports them.
4. Verify every produced signature against the pinned public key before
   journaling it, matching the existing backend contract.
5. Project a base58 Solana address from the canonical Ed25519 public key;
   replace the EVM-only address helper with key-spec dispatch.
6. Keep derivation unsupported for Ed25519 in the MVP. Generation and import
   are sufficient and avoid inventing a derivation contract prematurely.

Gate: backend conformance, custody round-trip, backup/restore, restart,
cross-suite denial, and zeroization-oriented tests pass.

### Phase 2 — Broker/Signer custody protocol binding

This requires synchronized API changes in `bloom-broker-api` and
`bloom-signer-api`, followed by Broker and Signer releases.

1. Add an explicit requested root `key_spec` (or a versioned wallet profile
   containing it) to wallet registration/import preparation.
2. Include that choice in exact terms, operation identity, ceremony review,
   signer contribution, result validation, audit, and replay/idempotency keys.
3. Preserve plain-name registration as the legacy secp256k1 request. New
   clients send a versioned structured request for Ed25519; never infer the
   suite from a wallet name or selected RPC network inside Broker/Signer.
4. Render “Solana / Ed25519” in the owner ceremony and fail if the returned
   root key spec differs from the reviewed request.
5. Keep exactly one `WalletRoot`. Do not model the Solana key as a derived key
   under a secp256k1 root.
6. Add edge golden vectors and compatibility tests before advancing pinned
   dependency revisions in Bloom.

Gate: a Machine request can create/import an Ed25519-root wallet, project its
public address, and exact-sign a known message without any secret entering
Machine or Broker.

### Phase 3 — Solana protocol and RPC crate

Add a focused `bloom-solana` crate. Keep Solana dependency versions behind this
crate so the rest of the workspace does not absorb SDK types.

1. Define validated newtypes for public keys, signatures, hashes, lamports,
   slots, block heights, commitment, and cluster identity.
2. Implement endpoint failover and the minimum RPC set:
   `getGenesisHash`, `getHealth`, `getSlot`, `getBlockHeight`, `getBalance`,
   `getLatestBlockhash`, `getFeeForMessage`, `simulateTransaction`,
   `sendTransaction`, and `getSignatureStatuses`.
3. Build and decode only legacy, single-signer System Program transfers in the
   MVP. Reject v0, address-table, multisigner, and unknown-program messages at
   the boundary.
4. Select and pin the modular Anza/Solana crates only after an isolated
   compatibility spike proves Rust 1.85, supported release targets, locked
   builds, and acceptable dependency/compile cost.
5. Keep RPC transport behavior consistent with Bloom's retry and audit model,
   but do not pretend EVM and Solana response/error types are interchangeable.

Gate: deterministic vectors and mock-RPC tests cover every method, malformed
base58/binary input, cluster mismatch, failover, and error classification.

### Phase 4 — Solana transaction engine and policy

Add a chain-family dispatcher in the daemon/VFS. Retain `bloom-tx` as the EVM
engine and introduce a separate Solana engine instead of immediately rewriting
the mature EVM engine behind a large generic trait.

Stage:

1. Parse a typed native-transfer request containing cluster, destination, and
   lamports/SOL amount.
2. Validate the destination public key and use the projected Ed25519 wallet
   root as fee payer and sole signer.
3. Fetch `(blockhash, lastValidBlockHeight)`, build the exact message, estimate
   its fee, and simulate it without signature verification.
4. Freeze message bytes, message digest, blockhash boundary, instruction/program
   facts, fee, balance deltas, simulation result, policy snapshot, and plan.
5. Require policy approval using chain-qualified destinations/assets. At
   minimum support per-transaction and rolling SOL/USD caps plus destination
   allow/deny rules. The only allowed MVP program is the System Program.

Confirm:

1. Re-read the immutable staged subject; display files are never execution
   inputs.
2. Fail as expired when current block height exceeds `lastValidBlockHeight`.
   Never refresh the blockhash behind an approval because that changes the
   signed and reviewed payload; restage instead.
3. Ask Broker to sign the exact message using `Ed25519Message`, validate the
   operation response, and locally verify the signature.
4. Insert the signature in the correct signer slot, revalidate the complete
   transaction and size, and submit the frozen bytes.
5. Reconcile processed/confirmed/finalized status and durable outbox state.
   Retrying the same signed bytes/signature is idempotent; constructing a new
   message is a new operation.

Gate: restart-safe local-validator tests cover successful transfer, simulation
failure, policy denial, user cancellation, expired blockhash, wrong signature,
RPC timeout before/after submission, exact retry, and reconciliation.

### Phase 5 — VFS, CLI, configuration, and docs

1. Add structured registration/import input and CLI UX such as
   `bloom wallet new <name> --network solana-devnet`; resolve the network to an
   explicit Ed25519 profile before preparing the custody request.
2. Preserve existing EVM config and routes. Add Solana cluster config and merge
   both registries only at the `/chains` presentation layer.
3. Expose the compatible subset of:
   - `/chains/<cluster>/{kind,genesis_hash,connected,slot,block_height}`
   - `/wallets/<wallet>/chains/<cluster>/{balance,balance.raw,balance.json}`
   - `/wallets/<wallet>/chains/<cluster>/outbox/...`
4. Make public-key encoding explicit in structured projection data. The text
   `address` route may remain the primary chain address, but consumers must be
   able to distinguish EIP-55 hex from base58 Ed25519 without heuristics.
5. Update root guidance, wallet architecture, examples, audit inventory,
   release packaging, and triad compatibility/release pins.

Gate: a fresh install can discover the Solana routes, create the wallet through
the Broker URL, fund it on a local validator, stage/read/confirm a transfer,
and observe the terminal receipt using only documented CLI/VFS operations.

### Phase 6 — Release verification

Required checks:

- formatting, workspace tests, Clippy, and locked builds;
- Broker/Signer/Machine edge vectors at the exact pinned revisions;
- local-validator end-to-end test in CI;
- opt-in devnet smoke for balance, airdrop-funded transfer, and confirmation;
- all existing EVM wallet, approval, outbox, Petal, and packaging tests;
- x86_64/aarch64 Linux and macOS release builds, including the musl target;
- dependency/license/advisory review for the selected Solana crates;
- audit proving Machine/Broker never persist Ed25519 private material.

## First implementation slice

Do not start with the RPC/VFS surface. Start with the cross-repository custody
vertical:

1. golden Ed25519/Solana message vectors;
2. local Signer Ed25519 generate/import/sign/backup support;
3. versioned custody request binding the requested root key spec;
4. Broker projection and Machine exact-sign integration test;
5. pin the released Broker/Signer edges in Bloom.

Only after this passes should `bloom-solana` and transaction staging land. It
proves the security-critical prerequisite early and prevents a chain adapter
from growing around a temporary or legacy signing path.

## Completion criteria

Solana support is complete for the MVP when all of the following are observable:

- a user creates or imports an Ed25519-root wallet only through a Broker-hosted
  ceremony;
- the projected base58 address is derived from Signer-authenticated public
  material;
- Machine reads SOL balance and stages a native transfer with exact instruction,
  fee, simulation, policy, blockhash, and expiry facts;
- approval and signature bind the exact serialized message bytes;
- Signer returns a valid raw Ed25519 signature and never releases the secret;
- Machine broadcasts the assembled transaction and reconciles a durable receipt;
- an expired blockhash, altered message, wrong suite/key, unknown program,
  cluster mismatch, or unavailable authority fails closed;
- existing EVM workflows remain unchanged.

## References

- [Triad process architecture](../specs/2026-07-23-triad-process-architecture.md)
- [Wallet architecture](../architecture/Wallet.md)
- [Interaction modes](../architecture/Interaction%20Modes.md)
- [Solana transaction structure](https://solana.com/docs/core/transactions)
- [Solana `getLatestBlockhash`](https://solana.com/docs/rpc/http/getlatestblockhash)
- [Solana `simulateTransaction`](https://solana.com/docs/rpc/http/simulatetransaction)
- [Solana `sendTransaction`](https://solana.com/docs/rpc/http/sendtransaction)
- [Solana `getSignatureStatuses`](https://solana.com/docs/rpc/http/getsignaturestatuses)
