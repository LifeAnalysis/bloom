# bloom-eth

`bloom-eth` (binary: `beth`) presents Ethereum and EVM L2s as a virtual
filesystem: reads are blockchain queries, writes are transaction intents,
and `tail -f` is a live event stream. A single Rust daemon owns the
plumbing — RPC, signing, broadcast, caching — and exposes it as POSIX
paths so an agent can drive onchain workflows with `cat`, `ls`, and
`echo` instead of a Web3 SDK.

**Status:** v1 in progress, single-node, NFS mount stub deferred.

## Architecture

The workspace is split into 11 crates:

| Crate | Responsibility |
|-------|----------------|
| `beth` | CLI binary; in-process driver of the daemon. |
| `beth-daemon` | Wires the daemon: home dir, config, chains, keystore, VFS. |
| `beth-vfs` | Path router, handler trait, per-path caching, vendored docs. |
| `beth-chain` | RPC pool and per-chain engine (head, blocks, addresses). |
| `beth-tx` | Tx engine: stage, simulate, sign, broadcast, nonce manager. |
| `beth-keystore` | Encrypted local key storage and signer. |
| `beth-defi` | Enso Shortcuts / DeFi client (stub). |
| `beth-watch` | Subscriptions and live tails (stub). |
| `beth-mount` | NFSv4 mount adapter (feature-gated stub). |
| `beth-tools` | Etherscan and pure helpers (keccak, units, abi). |
| `beth-proto` | Shared types: paths, configs, audit records. |

See `docs/specs/2026-05-08-bloom-eth-design.md` for the full design.

## Build and run

```sh
cargo build
cargo test
cargo run -p beth -- status
```

The CLI is the daemon: every invocation builds the in-process daemon,
performs the requested VFS op, and exits. A `beth serve` subcommand
exists as a placeholder for the eventual long-running NFS-mounted
daemon.

## Filesystem layout

The VFS is rooted at `/eth/` (default mount path) with these top-level
trees:

- `chains/<chain>/` — read-only chain views (head, blocks, addresses).
- `wallets/<name>/` — managed wallets and the `outbox/` write surface.
- `tools/` — pure helpers (`keccak`, address checksum, unit parse).
- `status/` — daemon health, RPC pool, version.
- `docs/` — in-tree help, vendored from `crates/beth-vfs/src/docs/`.

See [QUICKSTART.md](./QUICKSTART.md) for an Anvil-backed walkthrough.

## Security defaults

- **Mainnet broadcasts disabled by default.** Toggle via
  `~/.bloom-eth/config.toml` (`block_mainnet_broadcast`).
- **Private keys are never readable through the FS.** The keystore
  lives outside the mount; only `address` and `public_key` are
  exposed.
- **Encrypted at rest** with argon2id KDF + chacha20poly1305.
- **Hash-chained audit log** at `status/audit.jsonl`: every write and
  side-effecting read is appended; entries reference the prior hash so
  tampering is detectable.
- **Stage-confirm is the only write mode in v1.** A staged tx becomes
  a transaction only when a non-empty confirm file is written.

## What's deferred from the spec

This repo implements the design in `docs/specs/2026-05-08-bloom-eth-design.md`
incrementally. As of now:

- **NFS mount adapter** (`beth-mount`) is a feature-gated stub. Drive
  the FS via `beth vfs cat|ls|write` instead.
- **Watch executor** (`beth-watch`) — live tails and subscription
  fan-out are not yet wired up.
- **Embedded indexer** is deferred; activity backfill relies on
  Etherscan-style APIs only.
- **Etherscan and Enso clients** in `beth-tools` / `beth-defi` are
  stubs and not yet routed through the VFS.
- **Hardware wallets**, smart accounts (4337), multi-user mode, and
  distributed sync remain stretch goals.

## License

Licensed under either of MIT or Apache-2.0 at your option.
