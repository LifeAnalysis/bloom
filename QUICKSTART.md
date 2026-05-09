# Quickstart

A short Anvil-backed walkthrough of the `beth` CLI. The CLI's `vfs`
subcommands are the v1 substitute for the (deferred) NFS mount: every
`cat` / `ls` / `write` here is what an agent would otherwise do
through the mounted `/eth/` tree.

## Prerequisites

- Rust toolchain — pinned via `rust-toolchain.toml` (installed
  automatically by `rustup` when you run `cargo` in this repo).
- Foundry's [`anvil`](https://book.getfoundry.sh/anvil/) for the
  local devnet used below.

## 1. Initialise a fresh home

`BETH_HOME` overrides the home directory (default `~/.bloom-eth`).
Using a tmp path keeps the demo isolated.

```sh
BETH_HOME=/tmp/beth-demo cargo run -p beth -- init
```

This prints the home dir, config path, and configured chains. The
default config registers an `anvil` chain at `http://127.0.0.1:8545`
and leaves mainnet broadcasts disabled.

## 2. Start a local devnet

In a second terminal:

```sh
anvil --port 8545
```

Leave it running. Anvil prints ten funded test accounts; we'll mostly
ignore them and create our own wallet.

## 3. Create a wallet

The keystore encrypts the key with `BETH_PASSPHRASE` (argon2id +
chacha20poly1305). For a demo, any passphrase works.

The CLI is a thin shortcut, but wallets are first-class VFS citizens
— the same operation is a write to `wallets/new`:

```sh
# CLI shortcut
BETH_HOME=/tmp/beth-demo BETH_PASSPHRASE=devonly \
  cargo run -p beth -- wallet new alice

# Equivalent VFS write (what an agent would do over the mount)
BETH_HOME=/tmp/beth-demo BETH_PASSPHRASE=devonly \
  cargo run -p beth -- vfs write /wallets/new --data 'alice'

# Full TOML form for import/watch:
#   name = "alice"
#   kind = "import"        # or "local" (default) | "watch"
#   private_key = "0x..."  # required for import
#   address = "0x..."      # required for watch
#   passphrase = "..."     # optional; falls back to BETH_PASSPHRASE
```

You'll get back something like `created wallet 'alice': 0x...`. List
wallets to confirm:

```sh
BETH_HOME=/tmp/beth-demo cargo run -p beth -- wallet list
```

## 4. Inspect the chain through the VFS

The same paths an agent would `cat` over NFS work via `beth vfs cat`:

```sh
BETH_HOME=/tmp/beth-demo \
  cargo run -p beth -- vfs cat /chains/anvil/head/number
```

```sh
BETH_HOME=/tmp/beth-demo \
  cargo run -p beth -- vfs ls /chains/anvil/head
```

Status and docs are also reachable:

```sh
BETH_HOME=/tmp/beth-demo cargo run -p beth -- vfs cat /docs/README.md
BETH_HOME=/tmp/beth-demo cargo run -p beth -- vfs cat /status/daemon.json
```

## 5. Stage a transaction

Writing into the wallet's outbox starts the stage-confirm flow.
Through an NFS mount this would be:

```sh
echo 'send 0.01 eth to 0xabc... on anvil' \
  > /eth/wallets/alice/chains/anvil/outbox/new.tx
```

In v1 the equivalent is:

```sh
BETH_HOME=/tmp/beth-demo BETH_PASSPHRASE=devonly \
  cargo run -p beth -- wallet unlock alice

BETH_HOME=/tmp/beth-demo cargo run -p beth -- vfs write \
  /wallets/alice/chains/anvil/outbox/new.tx \
  --data 'send 0.01 eth to 0x70997970C51812dc3A010C7d01b50e0d17dc79C8 on anvil'
```

The daemon parses the intent, fills defaults, simulates, runs policy
checks, and writes a `pending/<id>/` directory.

## 6. Inspect the plan, then confirm

List pending entries and read the human-readable plan:

```sh
BETH_HOME=/tmp/beth-demo \
  cargo run -p beth -- vfs ls /wallets/alice/chains/anvil/outbox/pending

BETH_HOME=/tmp/beth-demo \
  cargo run -p beth -- vfs cat /wallets/alice/chains/anvil/outbox/pending/<id>/plan.md
```

Confirm by writing any non-empty content to the `confirm` file. The
daemon signs, broadcasts, moves the directory to `sent/<txhash>/`,
and links the tx into `chains/anvil/tx/<hash>/`.

```sh
BETH_HOME=/tmp/beth-demo cargo run -p beth -- vfs write \
  /wallets/alice/chains/anvil/outbox/pending/<id>/confirm --data y
```

Removing the pending directory (or letting it expire after the
configured TTL) cancels the stage.

## What's actually shipped (v2)

- **Long-running daemon** via `beth serve` — UDS JSON-RPC at
  `~/.bloom-eth/run/beth.sock`. Talk to it with `beth ipc call <method>`.
- **Watch executor** — write a spec to `watch/new`, tail
  `watch/<id>/live` for the in-process state, or `watch/<id>/events.json`
  for the rotated event log.
- **Simulate** — write to `simulate/new` to get an `eth_call` + state
  override result and a rendered plan, all without staging.
- **Etherscan-backed history** — `chains/<c>/addresses/<a>/{txs,
  internal_txs,erc20_txs,erc721_txs}` and contract `source` / `abi`.
- **ENS** — recipient names like `vitalik.eth` in intents resolve via
  the canonical mainnet registry.
- **DeFi intents** — `defi/...` (Enso shortcuts) when an Enso API key is
  configured in `~/.bloom-eth/config.toml`.
- **Prices** — keyless DefiLlama at `prices/spot/<coin>(.usd)` and
  `prices/change_24h/<coin>`.
- **Address book** — `addressbook/<alias>` round-trips via FS.
- **EIP-712 / personal_sign / raw-hash signing** — write to
  `wallets/<w>/sign/{message,hash,typed_data}`.
- **NFS mount adapter** — feature-gated; build with
  `cargo build --features beth-daemon/mount` and call `Daemon::mount(path)`
  to expose the VFS over NFSv4.1.

See [docs/AUDIT.md](./docs/AUDIT.md) for the full prompt-to-artifact
checklist mapping every spec section to its implementation and tests.

## End-to-end acceptance

`scripts/acceptance.sh` boots Anvil, imports the funded test key, and
drives both a native ETH send and an ERC-20 transfer through the
stage-confirm-broadcast loop. Optional Uniswap V2 / Enso scenarios on a
mainnet fork run when `BETH_MAINNET_RPC` is set.
