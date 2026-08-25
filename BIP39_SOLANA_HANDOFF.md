# BIP-39 and native Solana integration checkpoint

This checkpoint records the remaining verification work after the 2026-08-25
Signer, Broker, and Machine integration commits. It is intentionally a test
handoff, not a list of known architectural gaps.

## Completed

- Signer persists and restores BIP-39 child public descriptions, rolls back
  failed allocations, honors tombstones, and signs raw Ed25519 messages rather
  than their SHA-256 digests.
- Broker forwards raw Ed25519 messages to Signer and exercises native Solana
  `SystemUseClaim` authorization with proof-verified success and denials for
  weak assurance, mismatched evidence, and disallowed destinations.
- Machine builds native System Program transfers, binds approval value limits
  to lamports plus fees, uses deterministic retry identities, validates the
  returned signature, simulates before send, and persists/reconciles outbox
  state.
- A real local-validator ceremony created and funded the BIP-39 wallet
  `fundedwallet`, produced Solana address
  `3Cy3YNTFywCmxoxt8n7UH6hg6dLo5uACowX3CFceaSnx`, passed policy approval, and
  produced a signature for restaged transaction
  `sol-3755e1bc41accf283ce052a9a68a0d97`.

## Validation status (updated 2026-08-25, second pass)

Done this pass:

1. DONE. Full local-validator flow driven end-to-end through the real
   separate-process triad against agave v3.0.0: BIP-39 wallet imported,
   BIP-44/SLIP-10 Ed25519 Solana child allocated
   (`3Cy3YNTFywCmxoxt8n7UH6hg6dLo5uACowX3CFceaSnx`), policy committed, transfer
   staged, approval ceremony completed, signed, broadcast, and reconciled to a
   durable `receipt.json` with `outcome: success`, `confirmation_status:
   finalized`. Verified on-chain (`solana confirm` = Finalized; destination
   balance credited).
2. DONE. All three services restarted after wallet creation; the locked Signer
   still described and used the same BIP-39 Solana child (allocation
   `ACTIVATED`, no mnemonic re-import) and produced a second finalized transfer.
3. DONE. Full matrices on the final pins: Signer (`2308ff4`) fmt+clippy+tests
   35/35 suites; Broker (`fa72dfe`) fmt+clippy+tests 20/20; Machine
   `cargo test --workspace` green except the documented env exceptions below
   (the two packaging tests pass with `TAR=bsdtar`; four macOS tests need macOS).
4. DONE. `cargo test -p bloom-solana-tx --test reconcile` passes 5/5 outside the
   sandbox (fixtures were missing the broadcast-attempt marker; now seeded).
6. DONE (confirmed empirically). Policy `allowed_destinations` uses the stable
   claim chain family `"solana"`; `solana-local` is only the connection/profile
   name.

Fixes applied this pass (uncommitted working-tree changes):
- Machine: re-pinned service-runtime `155560173…`→`06104c7…` and signer-derive
  `35d2832`→`2308ff4` to match the Broker/Signer pins (lockfile had a split
  service-runtime rev); added the `route_grants` field to
  `bloom-machine-client`'s `ApprovalSelector::Petal` (Broker-api drift); fixed
  `reconcile`/daemon/`signing` test fixtures and a `solana_workflow` fixture bug
  (child pubkey must be canonical Ed25519 **SPKI DER**, not the raw 32 bytes);
  clippy `needless_borrow` cleanups in `bloom-solana-tx/src/outbox.rs`.
- Broker: fixed a bricking bug — a permanently-rejected ceremony adoption
  (`retry: never`, e.g. a stale Signer activation receipt) left the session
  `WALLET_COMMITTED` and re-fired on every sweep/restart, latching the audit
  chain degraded and denying ALL subsequent mutations; such sessions are now
  terminalized as `FAILED` (`sweep_committed_session`). Added latch-cause
  diagnostics at the three checkpoint-append sites.

Done this pass (third pass, 2026-08-25):

5. DONE (behavior fixed + validated e2e). A real process-boundary
   `AccountAllocate` fault — completing the ceremony with a stale (non-advancing)
   WebAuthn signature counter — is now rejected cleanly with `HTTP 400
   UNAUTHENTICATED_PEER "authenticator signature counter did not advance"`, the
   ceremony session is terminalized `FAILED`, the Signer leaves NO orphaned
   derived child (0 active Solana children), and an immediate retry with an
   advancing counter SUCCEEDS (1 active child) and goes on to a finalized
   transfer. Proven in the automated smoke run and a focused repro.

Two Broker robustness bugs found and fixed this pass (both were sources of the
intermittent `HTTP 500 (empty body)` on back-to-back ceremonies):

A. **Readiness rollback-latch.** On Machine reconnect the non-mutating
   `broker.readiness` handshake could hit `audit checkpoint sequence would roll
   back` when a peer presented an older (validly-signed) head; the Broker
   latched audit degradation and then denied ALL mutations until restart. Fixed
   in `signer_client.rs::checkpoint_append_fatal`: a `SequenceRollback` is
   benign (the crate rejects bad signatures / forks as `SequenceConflict`
   first, so a rollback only means we already retain a newer head) and no longer
   latches. Unit-tested; the three checkpoint-append sites use the classifier.
B. **Faulted ceremony not terminalized.** In `ceremony.rs::complete_session`,
   an unauthenticated completion (rejected proof / stale counter) ran a
   best-effort `signer.cancel`, and a cancel failure short-circuited to a
   bodyless 500 that SKIPPED terminalization — leaving the session `VERIFYING`
   ("live"), which blocked every retry of the same wallet with `QUOTA_EXCEEDED:
   wallet already has a live ceremony` until it expired. Fixed so a failed
   cleanup-cancel logs but still terminalizes the session `FAILED`.

Full fmt/clippy/test matrices stay green after both fixes (one pre-existing
parallel-execution flake in the `*_over_real_transport` suite, unrelated).

## Known CI environment exceptions

- `triad_release` needs `TAR=bsdtar` on the current Linux runner because its
  `/usr/bin/tar` reports GNU 1.35 but rejects `--uid=0`.
- Four macOS installer tests cannot execute on Linux and need a macOS runner.

## Useful commands

```sh
# Signer
cargo test -p bloom-signer-api -p bloom-signer-backend-local -p bloom-signer

# Broker
cargo test -p bloom-broker --test w4_authority
cargo build -p bloom-broker --features triad-dev-harness

# Machine
cargo test -p bloom-machine-client
cargo test -p bloom-solana-tx
cargo test -p bloom-solana-tx --test reconcile
cargo build -p bloom --no-default-features --features mount,triad-dev-harness
```
