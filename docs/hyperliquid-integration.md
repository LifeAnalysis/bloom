# Hyperliquid Petal

Bloom's Hyperliquid HyperCore integration is maintained as the standalone
`bloom-petal-hyperliquid` package. Bloom no longer mounts a native
`/hyperliquid` VFS subtree or exposes a dedicated `bloom hyperliquid` command.

Install a local checkout and inspect its versioned documentation:

```sh
bloom petals install ../bloom-petal-hyperliquid
bloom vfs cat /petals/hyperliquid/README.md
bloom vfs ls /petals/hyperliquid/mainnet
```

Market and account reads, signed exchange actions, and agent sessions live
under `/petals/hyperliquid/<network>/...`. HyperEVM remains a built-in EVM
chain, and the DeFi deposit intent remains part of Bloom's generic transaction
surface.
