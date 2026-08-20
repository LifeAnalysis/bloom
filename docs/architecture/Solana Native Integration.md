# Solana Native Integration

**Status:** architecture decision; triad-aligned
**Audience:** Bloom engineers, Petal authors, and implementation agents

## The decision

Solana is a first-class part of `bloom` itself — integrated **the same way
EVM is integrated**: in-tree crates (`bloom-solana`, `bloom-solana-tx`), not a
sandboxed, content-addressed Petal package. This supersedes an earlier
approach (`feat/solana-support`, PR #166, the `bloom-petal-solana` repo) that
shipped Solana as a **verified-chain-Petal**: a driver Petal plus a parallel
mini-Machine (`bloom-solana-machine`), a second CLI (`bloom-solana-cli`), a
bespoke signed catalog, and an RPC mediator (`bloom-chain-rpc`).

That work is **parked as reference, not deleted, not developed further** —
it stays available for the reusable pieces it produced (see "What carried
forward" below), but it is not the pattern for adding Solana, or any future
chain, to Bloom.

## Why the Petal shape was rejected

The verified-chain-Petal design duplicated facilities the Bloom Petal
runtime already provides: `bloom:http`'s `net.allow` policy, daemon-mediated
`chain_read`, `bloom:sign@0.2.0`'s `sign_payload_outcome`, durable
`petal_signing_requests`, and `PreinstalledPetal` pinning. Standing up a
parallel mini-Machine and a bespoke signed catalog to reimplement those,
just for one chain, was scope the Petal boundary exists specifically to
avoid. The Petal sandbox is the right shape for domain plugins that need
isolation from Bloom Machine's own trust boundary — one more base-layer
blockchain is not that; it belongs in the same crate family as every other
chain Bloom talks to directly.

## What "native" means concretely

- **Crate shape, not custody mechanism.** `bloom-solana` (read-only chain
  client) and `bloom-solana-tx` (transfer engine, outbox, reconciliation)
  mirror `bloom-evm` and `bloom-tx`'s shape and directory conventions
  exactly: `<home>/outbox/<wallet>/<chain>/{pending,sent,failed}/<id>/...`,
  write-once `intent.json`, a `receipt.json` sibling once mined. EVM's own
  crates are `alloy`-typed throughout with no chain-agnostic trait to
  implement against, so this is a parallel crate family, not a shared
  abstraction over EVM's — see `crates/bloom-solana-tx/src/outbox.rs` vs
  `crates/bloom-tx/src/outbox.rs` for how close the mirroring is today (real
  duplication, tracked as a deliberate, separately-scoped follow-up rather
  than a bad shared abstraction rushed to avoid it).
- **Custody: the same Signer+Broker triad EVM uses**, not a bespoke Solana
  keystore. Solana keys come from the BIP-39 multicurve derivation
  (`wallet.accounts`, `AccountAllocate`) — the same seed phrase / passkeys
  that produce the EVM address produce the Solana one. Signing goes through
  the same `TriadSigningService`/`MachineBrokerClient` pattern EVM's
  `transaction.confirm` exact-signing already uses; Solana didn't invent a
  parallel signing seam.
- **RPC transport is a parallel, not reused, stack.** `bloom-rpc`'s
  Ethereum-typed layers (`RootProvider<Ethereum>`, the `alloy` transport
  stack) don't fit Solana's JSON-RPC shape. What *is* shared, via
  `bloom-rpc-common` (chain-agnostic by design): `HealthRegistry` (endpoint
  cooldown/scoring) and the retry-classification rule table. Solana's own
  `reqwest`-based transport reimplements alloy's retry/throttle/fallback/probe
  pattern on top of those shared pieces rather than the `alloy` stack itself.
- **Mount surface unchanged.** Solana chains route through the existing
  `wallets/<wallet>/chains/<chain>/...` VFS family alongside EVM chains —
  same outbox route shape (`outbox/new.tx`, `outbox/pending/<id>/{confirm,cancel}`,
  `outbox/{pending,sent,failed}/<id>/...`), dispatched to a Solana transfer
  engine instead of the EVM `TxEngine` when the chain name resolves to one.
  See `crates/bloom-vfs/src/handlers/wallets.rs`'s `lookup_chain`/`list_chain`
  for the dispatch point, and `docs/examples-domain/01-chains.md` for the
  general (currently EVM-focused) walkthrough of that surface's read side.

## Registered semantic verifier: not yet on the native signing path

Broker contains the Anza-based, golden-vector-pinned
`solana-system-transfer-v1` verifier carried forward from the parked Petal
work. That verifier is currently invoked only for a Petal claim. The native
Solana engine signs under `ProvenanceSubject::System` with
`petal_use_claim: None`, so Broker treats the request as
`ClaimAssurance::MachineAsserted`; the verifier is not authoritative on the
real native signing path. EVM's current exact-signing path has the same
baseline limitation.

This is a release blocker, not a realized security property. Before mainnet,
the protocol must put an independently established, exact Solana transfer
claim on the real signing path and have Broker invoke the verifier for it. The
claim must bind the economic facts, message digest, intended genesis, and
blockhash; cluster and blockhash provenance cannot rely only on
Machine-supplied assertions. See Release Blocker #1 in the production-readiness
ledger at the workspace root.

## Provenance and operator configuration

The provenance catalog gains a `solana.transfer.confirm` entry
authorizing the native-transfer operation class, documented in
[`2026-08-18-solana-provenance-catalog.md`](../specs/2026-08-18-solana-provenance-catalog.md).
Operator-facing chain configuration lives in `[solana_chains.<name>]`,
parallel to `[chains.<name>]` for EVM — `Config::validate()` refuses a
config where a chain name is configured in both.

## Mainnet posture

This release posture never talks to Solana mainnet at all. The gate is the
cluster's live genesis hash (`bloom_solana::is_mainnet_beta_blocking`),
checked against the known mainnet-beta hash at chain construction — never
the operator's config-key name, which proves nothing about a cluster's real
identity. See `crates/bloom-solana/src/mainnet_guard.rs`.

## What carried forward from the parked Petal work

Not everything from PR #166 was Petal-hosting machinery with nothing left
to host once the shape changed. Reused directly:

- The Anza-based semantic verifier (`solana-system-transfer-v1`) described
  above, retained but not yet authoritative on the native signing path.
- BIP-39 SLIP-10 Ed25519 derivation — never Petal-shaped; it's the same
  derivation infrastructure EVM's HD wallets use.
- CAIP-2 chain-identity mapping and golden test vectors.

Not reused: the mini-Machine, the second CLI, the bespoke signed catalog,
the `bloom-chain-rpc` mediator, and `bloom-chain-action`'s outbox/artifact
template system — all superseded by mirroring EVM's existing outbox/
reconciliation pattern instead of inventing a Petal-hosting equivalent.
