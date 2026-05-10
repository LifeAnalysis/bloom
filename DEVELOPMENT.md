# Development guide

Operator's manual for working on `bloom-eth`: building, running, testing, and
debugging the daemon. The user-facing tour lives in [README.md](./README.md)
and [QUICKSTART.md](./QUICKSTART.md); this file covers the dev loop.

## Contents

1. [Toolchain and prerequisites](#toolchain-and-prerequisites)
2. [Building](#building)
3. [Running locally](#running-locally)
4. [Test suites](#test-suites)
   - [Rust unit tests](#rust-unit-tests)
   - [Rust integration tests](#rust-integration-tests)
   - [Dockerized tests (`tests/docker/`)](#dockerized-tests-testsdocker)
   - [Acceptance script (`scripts/acceptance.sh`)](#acceptance-script-scriptsacceptancesh)
   - [Playground (`scripts/play.sh`)](#playground-scriptsplaysh)
5. [Debugging](#debugging)
6. [Lint and format](#lint-and-format)
7. [Coverage map](#coverage-map): which suite tests which crate

## Toolchain and prerequisites

| Tool | Why |
|------|-----|
| Rust ≥ 1.85 | Pinned via `rust-toolchain.toml`. `rustup` installs it on first `cargo` run. |
| Foundry (`anvil`, `cast`, `forge`) | All anvil-backed integration tests, the acceptance script, and the playground. Override the binary paths with `BETH_ANVIL_BIN` / `BETH_CAST_BIN`. |
| `jq` | Acceptance script and Docker drivers. |
| Docker (compose v1 or v2) | Dockerized tests and `scripts/play.sh`. |
| Linux kernel NFS client | `--mount`/`--fork`/`--enso(-live)` Docker tests (requires `SYS_ADMIN`, `apparmor=unconfined`, `/dev/fuse`). |
| Optional API keys | `BETH_ETHERSCAN_KEY`, `BETH_ENSO_KEY`, `BETH_MAINNET_RPC` — populate `test.env` (gitignored) and `source` it. |

## Building

The workspace contains 17 crates (`Cargo.toml` `[workspace]`). Default builds
exclude the optional NFS mount adapter; opt in with the `mount` feature when
you need it.

```sh
# Debug build of every crate
cargo build --workspace

# Release binary (used by acceptance.sh and play.sh — lands at target/release/beth)
cargo build --release -p beth

# Daemon with the embedded NFS server (pulls embednfs as a git dep)
cargo build --release -p beth --features beth-daemon/mount

# Daemon with the heimdall bytecode-decompile fallback for revert decoding
# (heavy build; only needed if you're working on revert decoding)
cargo build --release -p beth --features bytecode-decompile
```

Release tuning (`Cargo.toml`): `lto = "thin"`, `codegen-units = 1`. Expect
release builds to be slow but cache well.

## Running locally

There are three execution modes. They share the same home directory layout
under `$BETH_HOME` (default `~/.bloom-eth`).

```sh
# Mode 1 — one-shot CLI. Each invocation builds the in-process daemon, runs
# the op, and exits. No socket. Good for scripts and CI.
BETH_HOME=/tmp/beth-demo cargo run -p beth -- init
BETH_HOME=/tmp/beth-demo cargo run -p beth -- vfs cat /chains/anvil/head/number

# Mode 2 — long-running daemon. Binds a UDS JSON-RPC socket; later `beth vfs`
# calls auto-detect it and route through (sharing unlock cache, watches, etc.).
beth serve                                     # foreground; logs to stderr
beth ipc call lookup --params '{"path":"/status/version"}'

# Mode 3 — NFS mount. Build with the `mount` feature, then run the example
# binary the docker tests use (or `Daemon::mount(path).await` from your own
# binary). The kernel-mounted tree appears at the path you supply.
cargo build --release -p beth-mount --features mount --example mount_demo
./target/release/examples/mount_demo /tmp/eth                  # mounts at /tmp/eth
```

The IPC socket lives at `$BETH_HOME/run/beth.sock` (mode 0600, created on
first `beth serve`). The same path is the IPC fallback target for `beth vfs`
calls when a daemon is up.

### Environment variables

Every `BETH_*` variable used by the binary, scripts, or test harness:

| Variable | Used by | Notes |
|----------|---------|-------|
| `BETH_HOME` | binary, all scripts | Override home dir. Default `~/.bloom-eth`. |
| `BETH_PASSPHRASE` | binary, scripts | Argon2id-derived KEK for the keystore. |
| `BETH_ETHERSCAN_KEY` | beth-etherscan, live tests | Etherscan v2 API key. |
| `BETH_ENSO_KEY` | beth-defi, docker `--enso*` | Enso Shortcuts key. |
| `BETH_MAINNET_RPC` | beth-ens live test, acceptance.sh §3/§4 | Optional; scenarios skip cleanly when unset. |
| `BETH_LIVE_HOME` | docker `--enso-live` | Path to a real keystore (mounted **read-only** into container). |
| `BETH_LIVE_DEST1/2/3` | docker `--enso-live` | Base mainnet sender + sweep targets. |
| `BETH_BASE_USDC`, `BETH_BASE_AUSDC` | docker `--enso*` | Canonical Base token addresses. |
| `BETH_BASE_RPC_URL` | docker `--enso-live` | Defaults to `https://base.publicnode.com`. |
| `BETH_SWAP_AMOUNT_ETH` | docker `--enso-live` | Default `0.001` — **real funds**. |
| `BETH_ANVIL_BIN`, `BETH_CAST_BIN` | beth-it, beth-watch | Override Foundry binary paths. |
| `BETH_TEST_WALLET_NAME/KEY/PASSPHRASE` | docker drivers | Pre-seeds the daemon wallet. |
| `BETH_PLAY_HOME`, `BETH_PLAY_PERSIST`, `BETH_PLAY_DAEMON_LOG` | scripts/play.sh | Playground knobs. |
| `RUST_LOG` | binary | `tracing-subscriber` env-filter. Default `info`. |

`test.env` (gitignored) is the canonical place to keep these. `source test.env`
before invoking the docker drivers.

## Test suites

### Rust unit tests

Standard `#[cfg(test)] mod tests` blocks, ~572 across the workspace. None
require external services. Run them all with:

```sh
cargo test --workspace --lib
```

Or scope to a single crate:

```sh
cargo test -p beth-vfs              # 219 tests — path router, handlers, caches
cargo test -p beth-proto            # 71  tests — config, intent, policy, units
cargo test -p beth-chain            # 62  tests — RPC client, blocks, balances
cargo test -p beth-tx               # 61  tests — staging, simulation, fee logic
cargo test -p beth-mount --features mount  # 43  tests — NFSv4 server (feature-gated)
cargo test -p beth-revert           # 27  tests — Error/Panic/custom decoders
cargo test -p beth-etherscan        # 23  tests — v2 client, ABI parser, cache
cargo test -p beth-tools            # 22  tests — keccak/sha/abi/rlp helpers
cargo test -p beth-prices           # 21  tests — DefiLlama oracle
cargo test -p beth-rpc              # 17  tests — failover, health, sessions
cargo test -p beth-watch            # 17  tests — watch executor & log rotation
cargo test -p beth-defi             # 10  tests — Enso route + intent parser
cargo test -p beth-daemon           # 7   tests — IPC dispatch, lifecycle
cargo test -p beth-ens              # 6   tests — namehash, encoder
cargo test -p beth-keystore         # 5   tests — argon2id + chacha20poly1305
```

### Rust integration tests

Ten `tests/*.rs` files. All but `crates/beth/tests/cli.rs` are gated with
`#[ignore]` because they spawn an anvil or hit the network — pass `-- --ignored`
to opt in.

```sh
# Always-on: CLI smoke tests (no anvil, no network)
cargo test -p beth --test cli

# Anvil-backed end-to-end suite (Foundry must be on $PATH)
cargo test -p beth-it -- --ignored
cargo test -p beth-watch --test anvil_watch -- --ignored

# Live Ethereum mainnet (skips cleanly if BETH_MAINNET_RPC is unset)
BETH_MAINNET_RPC=https://eth.example.com cargo test -p beth-ens -- --ignored

# Heimdall decompile fallback (feature-gated, heavy build)
cargo test -p beth-it --test revert_decoding_fallbacks \
  --features bytecode-decompile -- --ignored --nocapture
```

What each integration test covers:

| Test file | Covers |
|-----------|--------|
| `crates/beth/tests/cli.rs` | Subcommand routing, `init`, `status`, `vfs ls/cat/write`, IPC socket fallback, keystore wallet creation. |
| `crates/beth-it/tests/anvil_e2e.rs` | Full stage → confirm → broadcast for native ETH. Funds a wallet, writes `outbox/new.tx`, confirms, asserts the receipt. |
| `crates/beth-it/tests/erc20_e2e.rs` | ERC-20 transfer staging incl. fee-bump replacement; surfaces a known failure when token decimals are unreadable. |
| `crates/beth-it/tests/revert_decoding.rs` | Deploys a `Reverter` contract and asserts the decoder produces correct output for `Error(string)`, `Panic(uint)`, and custom errors. |
| `crates/beth-it/tests/revert_decoding_fallbacks.rs` | Same contract, no Etherscan ABI — exercises the heimdall bytecode decompile path. **Requires `--features bytecode-decompile`.** |
| `crates/beth-it/tests/rpc_failover.rs` | Two anvils; kills one mid-loop and asserts subsequent reads succeed within 1s on the survivor. |
| `crates/beth-it/tests/rpc_health_probe.rs` | Live anvil + dead endpoint; waits ~17s and asserts the health snapshot reflects success rate and cooldown. |
| `crates/beth-it/tests/rpc_state_drift.rs` | Two anvils at different heights; opens a session and asserts cross-provider hash mismatch is degraded-and-retried, not surfaced. |
| `crates/beth-it/tests/rpc_ws_subscriptions.rs` | Anvil WS endpoint: `subscribe_blocks`, mines 3 blocks, asserts 3 headers arrive. |
| `crates/beth-it/tests/rpc_ws_watch_handover.rs` | Watch executor block-watch survives anvil restart by handing over from WS to polling. |
| `crates/beth-watch/tests/anvil_watch.rs` | Balance watch: anvil_setBalance triggers a transition recorded to the live event log. |
| `crates/beth-ens/tests/live_mainnet.rs` | `vitalik.eth` round-trip (forward + reverse + text). Skips with a print if `BETH_MAINNET_RPC` is unset. |

The `beth-it` crate (`crates/beth-it/src/lib.rs`) is the harness shared by
those nine integration tests: `spawn_anvil()`, `cast_send()`, `pick_free_port()`,
and an `AnvilGuard` RAII wrapper that kills the child on drop.

### Dockerized tests (`tests/docker/`)

The Docker harness exists for two reasons: kernel NFS mounts work on Linux
but not on macOS host, and live-network DeFi flows need a controlled wallet.
The host orchestrator is `tests/docker/run.sh`; it builds a Linux `rust:bookworm`
image once (`Dockerfile`), caches the cargo target dir in the
`bloom-eth-cargo-cache` named volume, and dispatches into one of five
in-container drivers.

```sh
# Default — NFS mount surface regression test (no chain, no wallet)
bash tests/docker/run.sh                       # → tests/docker/test.sh

# `cargo test --workspace --lib` inside the Linux container
bash tests/docker/run.sh --workspace           # → tests/docker/test_workspace.sh

# Wallet staging + chain reads against an anvil fork of Base
bash tests/docker/run.sh --fork                # → tests/docker/test_fork_mount.sh

# DeFi intent (Enso → Aave) on an anvil fork — needs BETH_ENSO_KEY
bash tests/docker/run.sh --enso                # → tests/docker/test_enso_aave.sh

# Same flow against live Base mainnet — broadcasts and spends real funds
source test.env
bash tests/docker/run.sh --enso-live           # → tests/docker/test_enso_aave.sh

# Force a no-cache rebuild of the test image
bash tests/docker/run.sh --rebuild --mount
```

Coverage per mode:

| Mode | Compose stack | Verifies |
|------|---------------|----------|
| `--workspace` | single container | The unit-test suite passes on Linux as well as macOS. CI-shape regression for OS-specific code. |
| `--mount` (default) | single privileged container | NFS server + kernel mount: `ls`, `cat /status/version`, `cat /tools/keccak/abc`, `write /watch/new`. Regression-tests the WRITE-stability bug that returned EREMOTEIO. |
| `--fork` | compose: anvil-fork sidecar + driver | End-to-end wallet flow over the mount: stage → confirm → broadcast → poll receipt → fee-bump replace; chain reads under `/eth/chains/base/{head,tx,gas,blocks}`. |
| `--enso` | compose: anvil-fork + driver | Full DeFi intent: post NL intent → confirm session → poll outbox → broadcast → assert aBaseUSDC > 0. Generous 5% slippage and 300s gas-estimation budget to absorb fork drift. |
| `--enso-live` | single privileged container | Same flow against real Base mainnet, plus a balance-neutral unwind (redeem aBaseUSDC → ETH). Mounts `$BETH_LIVE_HOME` read-only and copies the keystore to a throwaway home. |

In-container drivers and their helpers all live in `tests/docker/`:

- `Dockerfile` — `rust:bookworm` base; installs `nfs-common` (for `mount.nfs4`),
  `ca-certificates`, `procps`, `curl`, `jq`. Pins rustfmt + clippy to dodge
  transient registry hiccups.
- `docker-compose.yml` — anvil-fork sidecar (Base mainnet at chain_id 8453,
  port 8545, healthcheck via `cast chain-id`); two driver profiles (`enso`,
  `fork`) sharing the sidecar.
- `lib.sh` — bash helpers (`prepare_home_dir`, `build_mount_demo`,
  `start_mount_demo`, `wait_for_mount`, `wait_tx_success`,
  `top_up_anvil_balance`, etc.) plus the deterministic Anvil fixtures.
- `test.sh`, `test_workspace.sh`, `test_fork_mount.sh`, `test_enso_aave.sh` —
  the per-mode drivers invoked by `run.sh`.

Common gotchas (more in each script's header comment):

- The `--mount`, `--fork`, and `--enso(-live)` containers run with
  `--cap-add SYS_ADMIN`, `--device /dev/fuse`, and `--security-opt
  apparmor=unconfined`. The `--workspace` mode does not.
- `CARGO_TARGET_DIR=/tmp/cargo-target` is set in-container so Linux artifacts
  don't trample the macOS host's `target/`. The `bloom-eth-cargo-cache` named
  volume persists this between runs; `docker volume rm bloom-eth-cargo-cache`
  to nuke.
- Public Base RPC has 1–2 block lag across replicas. `--enso-live` polls final
  balances for 60s and accepts ≤5 raw aBaseUSDC dust as success.
- `--enso-live` mounts the live home **read-only**; the daemon runs from a
  throwaway copy of the keystore. A bad test cannot corrupt the canonical home.

### Acceptance script (`scripts/acceptance.sh`)

Host-side end-to-end suite that doesn't need Docker. Drives the four happy
paths from §11.4 of the design doc using `beth` CLI calls (which exercise the
same code as VFS writes).

```sh
cargo build --release -p beth                  # build first
./scripts/acceptance.sh                        # exit 0 = pass, 1 = fail, 2 = missing tools
```

Prereqs: `anvil`, `cast`, `forge`, `jq`, and a built `target/release/beth`
(override with `BETH_BIN`).

| # | Scenario | Skipped when |
|---|----------|--------------|
| 1 | Native ETH send on a local anvil | (always runs) |
| 2 | ERC-20 transfer (deploys `MockERC20` via `forge`) | (always runs) |
| 3 | Uniswap V2 swap on a mainnet fork | `BETH_MAINNET_RPC` unset |
| 4 | Enso intent on a mainnet fork | `BETH_MAINNET_RPC` or `BETH_ENSO_KEY` unset |

Anvil and the temp home dir are torn down on exit via `trap`.

### Playground (`scripts/play.sh`)

Interactive REPL — not a test, but the fastest way to drive the daemon by
hand against a real anvil.

```sh
./scripts/play.sh                              # builds beth, boots anvil, drops you into a subshell
BETH_PLAY_PERSIST=1 ./scripts/play.sh          # keep the play home between runs
```

What it sets up: anvil at `127.0.0.1:8545` (chain_id 31337) via
`docker/playground/docker-compose.yml`; a fresh `~/.bloom-eth-play` with two
chains (`anvil` broadcasts enabled, `base` mainnet read-only); three wallets
(`alice`, `bob`, `carol`) imported from anvil's deterministic mnemonic with
passphrase `play`; a backgrounded `beth serve` whose logs go to
`/tmp/beth-play-daemon.log`. Cleanup on subshell exit kills the daemon and
the anvil container.

## Debugging

### Tracing logs

The binary configures `tracing-subscriber` with `EnvFilter` from `RUST_LOG`
(default `info`, output to stderr). Useful filters:

```sh
RUST_LOG=info beth serve
RUST_LOG=beth_daemon=debug,beth_vfs=debug,info beth serve
RUST_LOG=beth_rpc=trace,info beth serve              # endpoint health & failover
RUST_LOG=error beth status                           # quiet for scripts
```

For `scripts/play.sh` the daemon log lands at
`${BETH_PLAY_DAEMON_LOG:-/tmp/beth-play-daemon.log}`.

### Audit log

Every side-effecting operation is appended to `$BETH_HOME/audit.jsonl` as a
hash-chained record (`{ts_ms, kind, wallet?, chain?, data, prev, digest}`,
all blake3). Tampering is detectable via `AuditLog::verify()`. The live
fingerprint is exposed under the status surface:

```sh
beth vfs cat /status/audit/head      # current blake3 digest
beth vfs cat /status/audit/count     # total entries
beth vfs cat /status/audit/last      # last 10 records as JSON
```

### Status VFS surface

The fastest read-only diagnostic. Backed by
`crates/beth-vfs/src/handlers/status.rs`; per-path TTLs keep these calls
cheap (chain probes 5s, version 1d, audit live).

```sh
beth vfs cat /status/daemon.json                        # version, uptime, home, chains
beth vfs cat /status/chains/base/connected              # true / false (750ms RPC ping)
beth vfs cat /status/chains/base/block_number           # head height (or backend error)
beth vfs ls  /status/chains/base/endpoints              # health snapshots, 0-indexed
beth vfs cat /status/chains/base/endpoints/0/success_rate
beth vfs cat /status/policies/block_mainnet_broadcast   # safety flag
beth vfs cat /status/outbox/pending_count               # pending tx count
beth vfs cat /status/backends/summary.json              # which data source each surface uses
```

### IPC introspection

Once `beth serve` is up, the JSON-RPC dispatcher
(`crates/beth-daemon/src/ipc.rs`) exposes `lookup`, `read`, `write`, `list`,
`version`, `chains`, `shutdown`. They are addressable directly:

```sh
beth ipc call version
beth ipc call chains
beth ipc call list   --params '{"path":"/wallets"}'
beth ipc call read   --params '{"path":"/status/daemon.json"}'
beth ipc call write  --params '{"path":"/wallets/alice/chains/anvil/outbox/new.tx","text":"send 0.01 eth to 0x..."}'
beth ipc call shutdown
```

Useful when you suspect the CLI shim is hiding a daemon-side error.

### Home directory layout

```
$BETH_HOME/
├── config.toml          # chain config, etherscan/enso keys, broadcast policy
├── addressbook.toml     # local petname directory
├── audit.jsonl          # hash-chained audit log
├── run/beth.sock        # UDS JSON-RPC socket (mode 0600)
├── keystore/<wallet>/   # encrypted.key, address, pubkey, kind, policy.toml
├── cache/cache.db       # etherscan / ABI cache (TTL-gated)
├── blobs/               # large response storage
├── outbox/<wallet>/<chain>/{pending,sent,failed}/<id>/
├── watch/<id>/          # subscription state + rotated history.jsonl[.n]
└── logs/                # daemon log files (when running detached)
```

## Lint and format

No custom `rustfmt.toml` or `clippy.toml` — defaults apply.

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
```

CI expects both clean.

## Coverage map

Quick "if I changed X, what should I run?" matrix.

| If you touched… | Run, in order |
|-----------------|---------------|
| `beth-proto` (config, intents, units) | `cargo test -p beth-proto` |
| `beth-vfs` handlers | `cargo test -p beth-vfs` then `bash tests/docker/run.sh --mount` |
| `beth-rpc` failover/health/WS | `cargo test -p beth-rpc` then `cargo test -p beth-it -- --ignored` |
| `beth-chain` | `cargo test -p beth-chain` then `cargo test -p beth-it --test anvil_e2e -- --ignored` |
| `beth-tx` staging / nonce / replace | `cargo test -p beth-tx` then `cargo test -p beth-it --test anvil_e2e -- --ignored` then `bash tests/docker/run.sh --fork` |
| `beth-keystore` | `cargo test -p beth-keystore` |
| `beth-revert` | `cargo test -p beth-revert` then `cargo test -p beth-it --test revert_decoding -- --ignored` (and `revert_decoding_fallbacks` with `--features bytecode-decompile` if you touched the heimdall path) |
| `beth-watch` | `cargo test -p beth-watch -- --ignored` then `cargo test -p beth-it --test rpc_ws_watch_handover -- --ignored` |
| `beth-mount` | `cargo test -p beth-mount --features mount` then `bash tests/docker/run.sh --mount` (and `--fork` if you touched plumbing the wallet flow uses) |
| `beth-defi` (Enso client + parser) | `cargo test -p beth-defi` then `bash tests/docker/run.sh --enso` (needs `BETH_ENSO_KEY`) |
| `beth-etherscan` | `cargo test -p beth-etherscan` (and the live test if you have a key) |
| `beth-ens` | `cargo test -p beth-ens` then the live test with `BETH_MAINNET_RPC` if applicable |
| `beth-prices` | `cargo test -p beth-prices` |
| `beth-daemon` IPC / lifecycle | `cargo test -p beth-daemon` then `cargo test -p beth --test cli` |
| `beth` CLI | `cargo test -p beth --test cli` then `./scripts/acceptance.sh` |
| Anything load-bearing for live use | `bash tests/docker/run.sh --enso-live` (sources `test.env`, real funds) |
