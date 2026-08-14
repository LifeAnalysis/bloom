# Solana Support on the Triad Architecture

**Status:** proposed

**Issue:** [bloom#156](https://github.com/bloom-directory/bloom/issues/156)

**Branch:** `feat/solana-support`

**Depends on:** [BIP-39 multi-curve HD wallets](./2026-08-14-bip39-multicurve-hd-wallets.md)
([bloom#163](https://github.com/bloom-directory/bloom/issues/163))

## Goal

Ship a first-party Solana wallet path that allocates a hardened SLIP-10
Ed25519 child from Bloom's default BIP-39 wallet root, reads SOL state, stages
and simulates a native SOL transfer, obtains an exact payload-bound approval,
signs the frozen Solana message inside Signer, broadcasts it, and reconciles
its result through the existing wallet outbox UX.

Users keep one Bloom wallet identity and the same passkeys they use for EVM.
Solana support must not provision an unrelated Ed25519 root, introduce another
mnemonic/private key, or require a second wallet-recovery workflow.

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

### 3. Solana is a derived account under the shared wallet seed

This project consumes the wallet model ratified and implemented by bloom#163:

- one non-signable `WalletSeedRef` contains the encrypted BIP-39 root;
- every passkey independently wraps the same WKEK and therefore unlocks the
  same EVM and Solana accounts;
- EVM accounts use the versioned BIP-32 secp256k1 profile;
- Solana accounts use `bip44-solana-slip10-ed25519-v1` with the canonical path
  `m/44'/501'/<account>'/0'`;
- the Signer-owned derivation registry allocates the path and issues a typed
  child `KeyRef` pinned to its Ed25519 public key;
- only the derived child is signable. The seed root never satisfies a signing
  request and never crosses the Signer boundary.

Broker and Machine select the exact child account; neither supplies an
arbitrary path. Adding or replacing a passkey, restoring the wallet, or adding
another chain must reproduce the same registered Solana address.

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

- allocation and projection of a managed Ed25519 child through the versioned
  HD-wallet custody ceremony from bloom#163;
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
- additional Solana accounts/paths, hardware wallets, and non-BIP-39 roots;
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

### Phase 1 — Release the derived Ed25519 signing edge

This work belongs in `bloom-directory/bloom-signer` and must release before
Bloom pins the new edge.

1. Consume the `bip39-multicurve-v1` root and
   `bip44-solana-slip10-ed25519-v1` child profile from bloom#163. Do not add a
   second Ed25519 root variant or reuse service/policy Ed25519 identity keys.
2. Implement child allocation, public description, activation through the
   existing WKEK/passkey path, registry-aware backup/restore, retirement, and
   exact raw-message signing.
3. Advertise `KeySpec::Ed25519`, `CryptoSuite::Ed25519Message`, message input,
   and raw-64 output only when the backend actually supports them.
4. Verify every produced signature against the pinned public key before
   journaling it, matching the existing backend contract.
5. Project a base58 Solana address from the canonical Ed25519 public key;
   replace the EVM-only address helper with key-spec dispatch.
6. Reject arbitrary, unhardened, out-of-namespace, unregistered, tombstoned, or
   cross-profile paths and refuse signing through the `WalletSeedRef` itself.

Gate: bloom#163's root/registry vectors plus Ed25519 allocation, backup/restore,
restart, cross-suite/path denial, signature, and zeroization-oriented tests pass.

### Phase 2 — Broker/Signer derived-account binding

This requires synchronized API changes in `bloom-broker-api` and
`bloom-signer-api`, followed by Broker and Signer releases.

1. Use bloom#163's explicit child-allocation request. Bind the wallet seed
   profile, Solana derivation profile, semantic role, namespace, expected
   Ed25519 key spec, and allowed suites.
2. Include those fields in exact terms, operation identity, ceremony review,
   signer contribution, result validation, audit, and replay/idempotency keys.
3. Render “Add Solana account / Ed25519” in the owner ceremony and commit to
   the expected public projection before accepting the resulting child.
4. Fail if Signer returns a different path, profile, key spec, public key,
   wallet root relationship, or address.
5. Project the registered child and its CAIP account identity without exposing
   entropy, WKEK, PRF output, or derived private material.
6. Add edge golden vectors and compatibility tests before advancing pinned
   dependency revisions in Bloom.

Gate: a Machine request can allocate/project the deterministic Solana child and
exact-sign a known message without any secret entering Machine or Broker; a
second passkey and restored backup reproduce the same child address.

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
2. Validate the destination public key and use the projected Ed25519 child
   account as fee payer and sole signer.
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

1. Add explicit account-enablement UX such as
   `bloom wallet account add <name> --network solana-devnet`; resolve the
   network to the versioned Solana child profile before preparing the custody
   request. A new default BIP-39 wallet may enable its first Solana child during
   registration through the same typed request.
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

Do not start with the RPC/VFS surface. Start with the cross-repository derived
account vertical after bloom#163's BIP-39 root edge is released:

1. golden Ed25519/Solana message vectors;
2. local Signer hardened SLIP-10 child allocation/sign/backup support;
3. versioned custody request binding the derivation profile and child key spec;
4. Broker projection, same-address multi-passkey/restore proof, and Machine
   exact-sign integration test;
5. pin the released Broker/Signer edges in Bloom.

Only after this passes should `bloom-solana` and transaction staging land. It
proves the security-critical prerequisite early and prevents a chain adapter
from growing around a temporary or legacy signing path.

## Completion criteria

Solana support is complete for the MVP when all of the following are observable:

- a default BIP-39 wallet enables a deterministic Solana child only through a
  Broker-hosted account-allocation ceremony;
- every active passkey and a restored backup reproduce the same Solana address;
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
