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

## Remaining validation

1. Repeat the local-validator flow from a clean triad root and prove the signed
   transaction reaches `sent`, then poll it through `processed`, `confirmed`,
   and `finalized`. The last run's shell ended after signing while the item was
   still `pending`; it is not evidence of broadcast or confirmation.
2. Restart all three services after wallet creation and verify the locked
   Signer can still describe and use the same BIP-39 Solana child without
   re-importing the mnemonic.
3. Run the complete fmt, clippy, and test matrices for Signer, Broker, and
   Machine on the final pinned commits. Targeted suites and feature builds pass,
   but the full cross-repository matrix was not repeated after the final pin.
4. Run `cargo test -p bloom-solana-tx --test reconcile` outside the restricted
   sandbox. Its five tests require binding a loopback TCP listener; the latest
   in-sandbox run failed at bind with `Operation not permitted`.
5. Add a process-boundary fault test that forces BIP-39 `AccountAllocate` to
   fail during a real ceremony and proves both Signer and Broker retry cleanly.
6. Clarify user-facing policy documentation: the stable claim chain family is
   `solana`, while `solana-local` is a Machine connection/profile name.

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
