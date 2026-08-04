# Hyperliquid Petal

Bloom's Hyperliquid HyperCore integration is maintained as the standalone
`bloom-petal-hyperliquid` package. Bloom no longer mounts a native
`/hyperliquid` VFS subtree or exposes a dedicated `bloom hyperliquid` command.

`bloom init` provisions the pinned default release. Inspect its versioned
documentation:

```sh
bloom init
bloom vfs cat /petals/hyperliquid/README.md
bloom vfs ls /petals/hyperliquid/mainnet
```

Market and account reads, signed exchange actions, and agent sessions live
under `/petals/hyperliquid/<network>/...`. HyperEVM remains a built-in EVM
chain, and the DeFi deposit intent remains part of Bloom's generic transaction
surface.

## Upgrade notes

Legacy `[hyperliquid] mainnet_url` and `testnet_url` values are migrated to the
Petal's `mainnet` and `testnet` endpoint bindings when they are valid HTTPS
origins. Explicit `petals.runtime.hyperliquid.endpoints` values take
precedence. Insecure legacy origins fail with an actionable configuration
error rather than silently changing the destination.

Bloom does not automatically replace an existing Hyperliquid package at a
different catalog commit. Petal store state and secret agent keys are scoped by
package hash, so an owner swap would strand live session state while venue
authority or open activity could remain. Before upgrading, stop session writes,
cancel or close live activity, revoke the agent at Hyperliquid, then explicitly
uninstall the old package and rerun `bloom init`.

The pinned v0.1.4 Petal enforces configured session asset, per-write notional,
and leverage limits when a route is written. It does not continuously monitor
loss or positions, auto-flatten on a breach, or revoke venue authority when a
session expires or is stopped.

Bloom cannot safely import native ephemeral agent-session keys into the Petal.
If startup reports legacy sealed key material, verify that venue authority has
been stopped or revoked before removing the reported legacy directory.
