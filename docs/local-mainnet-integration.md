# Manual local mainnet integration

This is the developer path for exercising the real Bloom binary, installed
Petals, an existing passkey wallet, both venue APIs, policy evaluation,
the kernel-mounted NFS VFS, interactive browser ceremonies, signing,
submission, and receipts without
running Machine, Broker, and Signer as separate services.

It is deliberately not a deployment mode. The `local-integration` Cargo
feature compiles the custody adapters into the Machine process, and
`serve --local-integration` must also be supplied at runtime. A normal build
does not contain those adapters. A special build refuses to serve without the
runtime flag.

## What the runner protects

`scripts/local-mainnet-integration.sh` defaults to non-spending preflight. Live
mode:

- requires an explicit opt-in for each selected venue; either venue can be run
  separately so retrying one failure never repeats a successful order;
- requires exact venue, market, side, size, price/slippage bound, and order
  type;
- accepts only Hyperliquid perp limit orders with a $10–$25 notional;
- creates a developer-only Hyperliquid policy envelope for exactly one asset,
  at most $25 notional/position/loss, and at most five minutes;
- accepts only Polymarket `FAK` or `FOK` orders with at most $25 maximum
  consideration;
- displays the Polymarket plan, policy result, revalidated quote, and final
  review intent before either order is submitted;
- requires an exact terminal acknowledgement, then a Hyperliquid passkey
  approval and a separate Polymarket passkey approval;
- cancels a resting Hyperliquid `Alo` order, and also attempts cancellation
  and session stop if the runner exits abnormally;
- accesses wallet identity, venue state, Petal routes, plans, ceremonies, and
  receipts exclusively through ordinary reads and writes beneath the temporary
  kernel mount; it never uses the `bloom vfs` fallback or IPC operations.

The local Hyperliquid policy is in memory and is accepted only if the stored
wallet has no configured Hyperliquid policy. It never edits `policy.toml`.
If a wallet already has a policy, that policy remains authoritative and the
developer overlay is refused.

Venue acceptance is not proof of a fill. `Ioc`/`FAK` with a marketable bound
normally fills immediately up to that bound and cancels the remainder;
`FOK` either fills completely or cancels. Use exact bounds you are prepared
to trade at.

## Prerequisites

- macOS with the passkey available to the current login's browser/keychain;
- Rust/Cargo and `jq` (`brew install jq`);
- an existing Bloom passkey wallet under `~/.bloom/keystore`;
- the pinned Polymarket Petal enabled in Bloom's preinstalled Petal config;
- Hyperliquid mainnet collateral and Polymarket onboarding/funding sufficient
  for the chosen orders;
- no other local process listening on `127.0.0.1:18734`.

The last rule is fail-closed. If the root-installed Broker is active, unload
only its job before the test:

```bash
uid="$(id -u)"
sudo launchctl bootout "system/com.bloom.broker.${uid}"
```

Restore it after the test:

```bash
uid="$(id -u)"
sudo launchctl bootstrap system \
  "/Library/LaunchDaemons/com.bloom.broker.${uid}.plist"
```

The runner does not perform either root action. If the port is owned, startup
fails before any order is staged. An installed enrollment may remain on disk;
the local process does not connect to it.

## 1. Run preflight

The smallest invocation is:

```bash
scripts/local-mainnet-integration.sh \
  --wallet hl-mainnet-validation
```

No ceremony opens and no order is created. Serving may install/update the
pinned Petal, but custody material and venue positions are not changed.
Preflight verifies:

- the mounted wallet exists, reports passkey kind, and exposes its address;
- the pinned Polymarket Petal loads;
- Hyperliquid mainnet metadata and the wallet account snapshot are readable;
- the Polymarket route contract and onboarding, account, and trade directories
  are reachable through the kernel-mounted filesystem.

Polymarket's authoritative onboarding, funding, market, and policy checks run
when the mounted `trade/<wallet>/new` file is written in live mode. Preflight
does not invoke its refresh-on-read status leaves through a non-filesystem
fallback.

Supply candidate markets to get their current metadata during preflight:

```bash
scripts/local-mainnet-integration.sh \
  --wallet hl-mainnet-validation \
  --hl-coin BTC \
  --pm-slug YOUR-POLYMARKET-SLUG
```

This prints the Hyperliquid asset ID that must be pinned in the live command
and echoes the explicitly selected Polymarket slug. Live mounted draft creation
refuses unavailable markets, incomplete onboarding, insufficient funding, or
policy failures before any passkey ceremony or order submission.

## 2. Run bounded mainnet submissions

Choose current values yourself. This is a shape example, not a price
recommendation:

```bash
scripts/local-mainnet-integration.sh \
  --wallet hl-mainnet-validation \
  --execute-hyperliquid \
  --hl-coin BTC \
  --hl-asset-id 0 \
  --hl-side buy \
  --hl-price YOUR_MAXIMUM_PRICE \
  --hl-size YOUR_SIZE \
  --hl-tif Ioc \
  --execute-polymarket \
  --pm-slug YOUR-POLYMARKET-SLUG \
  --pm-outcome Yes \
  --pm-side buy \
  --pm-amount YOUR_USD_AMOUNT \
  --pm-price-bound YOUR_MAXIMUM_PRICE_FROM_0_TO_1 \
  --pm-order-type FAK
```

The command validates all numeric bounds before touching venue state. It then:

1. starts the special Bloom process on a private Unix socket;
2. mounts its VFS over NFS at a private temporary directory;
3. rechecks the wallet, Hyperliquid state, and Polymarket Petal surface
   exclusively with ordinary filesystem reads through that mount;
4. creates and revalidates an unsigned Polymarket draft through filesystem
   writes;
5. prints both pinned requests and all available review artifacts;
6. asks for an exact acknowledgement naming the selected venue or venues;
7. opens the real passkey ceremony for the exact Hyperliquid five-minute
   session, retries session creation, and submits the exact order;
8. asks for a draft-specific Polymarket acknowledgement;
9. opens the real passkey ceremony for the exact Polymarket order, retries the
   exact post, and reads the receipt;
10. cancels applicable resting orders, stops the session, unmounts, and exits.

The runner retains its temporary log directory on any failure and prints its
path. Do not retry blindly after an ambiguous network failure: inspect the
persisted Hyperliquid response/session audit and Polymarket receipt first.
If one venue succeeds and the other fails, retry only the failed venue by
omitting the successful venue's `--execute-*` flag and arguments.

## Local deterministic verification

These do not contact mainnet or open a passkey prompt:

```bash
scripts/test-local-mainnet-integration.sh
cargo test -p bloom-vfs --features local-integration local_integration_bounds
cargo check -p bloom --no-default-features
cargo check -p bloom --no-default-features --features local-integration
```

The actual passkey and live venue submissions are intentionally manual because
only the wallet owner can approve them and they can move real money.
