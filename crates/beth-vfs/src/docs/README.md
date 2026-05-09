# bloom-eth virtual filesystem

This is the help file vendored into the daemon.

## Top-level layout

- `chains/<chain>/` — read-only chain views: head, blocks, tx, addresses,
  contracts, gas oracle.
- `wallets/<name>/` — managed wallets, outbox write surface, history,
  allowances, ENS reverse, sign / EIP-712 surfaces.
- `defi/intents/` — Enso-mediated DeFi intents (write `quote` / `execute`).
- `watch/` — long-running subscriptions (head, addr, log) executed by the
  daemon and persisted to JSONL.
- `simulate/` — out-of-band tx simulation with state overrides
  (`eth_call` / `debug_traceCall`).
- `tools/` — pure helpers: keccak, address checksum, units, ABI encode /
  decode, EIP-712 hash, RLP, hex, base64.
- `status/` — daemon health, RPC pool, audit head, cache stats, version.
- `docs/` — this file and examples.
- `ens/` — forward / reverse / text / contenthash resolution.
- `prices/` — DefiLlama price oracle (current / historical).
- `addressbook/` — local petname directory.

## Reading

Reads are RPC / API queries. Examples:

```sh
cat /eth/chains/anvil/head/number
cat /eth/chains/ethereum/blocks/19000000/json
cat /eth/wallets/alice/chains/anvil/balance.eth
cat /eth/wallets/alice/chains/ethereum/history.json
cat /eth/tools/keccak/hello
cat /eth/tools/abi/decode/<sig>/<hex>
cat /eth/ens/vitalik.eth/address
cat /eth/ens/vitalik.eth/avatar
cat /eth/ens/vitalik.eth/text/url
cat /eth/prices/spot/eth-usd
cat /eth/addressbook/alice
```

## Writing (stage-confirm)

Native send (canonical):

```sh
echo 'send 0.01 eth to 0xabc... on anvil' \
  > /eth/wallets/alice/chains/anvil/outbox/new.tx
ls /eth/wallets/alice/chains/anvil/outbox/pending/
cat /eth/wallets/alice/chains/anvil/outbox/pending/<id>/plan.md
echo y > /eth/wallets/alice/chains/anvil/outbox/pending/<id>/confirm
```

ERC-20 send (token symbol resolved via address book / token registry):

```sh
echo 'send 100 USDC to alice on ethereum' \
  > /eth/wallets/alice/chains/ethereum/outbox/new.tx
```

Replace / cancel pending tx:

```sh
echo replace > /eth/wallets/alice/chains/ethereum/outbox/pending/<id>/replace
echo cancel  > /eth/wallets/alice/chains/ethereum/outbox/pending/<id>/cancel
```

DeFi intent (Enso):

```sh
cat <<'EOF' > /eth/defi/intents/new.json
{ "from": "alice", "chain": "ethereum",
  "swap": { "in": "1 ETH", "out": "USDC" } }
EOF
ls /eth/defi/intents/pending/
```

Subscribe + read:

```sh
echo '{"kind":"head","chain":"anvil"}' > /eth/watch/new.json
tail -f /eth/watch/<id>/events.jsonl
```

Sign arbitrary message / EIP-712:

```sh
echo 'hello world' > /eth/wallets/alice/sign/personal
cat /eth/wallets/alice/sign/last.sig
echo "$(cat eip712.json)" > /eth/wallets/alice/sign/eip712
```

Address book petnames:

```sh
echo '0x000000000000000000000000000000000000beef' \
  > /eth/addressbook/alice
cat /eth/addressbook/alice
```

Mainnet broadcasts are **disabled by default**. Configure via
`~/.bloom-eth/config.toml` (`block_mainnet_broadcast = false` is required
to allow live broadcasts).

See `examples.md` for end-to-end demos.
