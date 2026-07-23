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

Bloom cannot safely import native ephemeral agent-session keys into the Petal.
If startup reports legacy sealed key material, verify that venue authority has
been stopped or revoked before removing the reported legacy directory.
