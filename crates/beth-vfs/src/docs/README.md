# bloom-eth virtual filesystem

This is the help file vendored into the daemon.

## Top-level layout

- `chains/<chain>/` — read-only chain views: head, blocks, tx, addresses,
  contracts (`source`/`abi`/`methods`/`events`/`storage`/`proxy`),
  gas oracle, ERC-20 balances under `addresses/<a>/tokens/<token>/...`.
- `wallets/<name>/` — managed wallets, outbox write surface, history,
  allowances, ENS reverse, sign / EIP-712 surfaces.
- `defi/intents/` — Enso-mediated DeFi intents (write `quote` / `execute`).
- `watch/` — long-running subscriptions (head, addr, log) executed by the
  daemon and persisted to JSONL.
- `simulate/` — out-of-band tx simulation with state overrides
  (`eth_call` / `debug_traceCall`).
- `tools/` — pure helpers: `keccak`, `selector`, `address/checksum`,
  `sha256`, `blake3`, `hex`, `base64`, `unit/{parse,format}`, `abi`,
  `rlp`, `eip712`.
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
cat /eth/prices/spot/eth.usd
cat /eth/addressbook/alice
```

NFT reads (auto-detects ERC-721 vs ERC-1155 via ERC-165):

```sh
# Per-holder views (transfer history requires etherscan-backed history).
cat /eth/chains/ethereum/addresses/0xd8da.../nfts/erc721_txs
cat /eth/chains/ethereum/addresses/0xd8da.../nfts/erc1155_txs
cat /eth/chains/ethereum/addresses/0xd8da.../nfts/owned.json   # best-effort

# Per-token reads (RPC-only):
cat /eth/chains/ethereum/addresses/0xd8da.../nfts/<contract>/<id>/owner
cat /eth/chains/ethereum/addresses/0xd8da.../nfts/<contract>/<id>/uri
cat /eth/chains/ethereum/addresses/0xd8da.../nfts/<contract>/<id>/metadata.json
cat /eth/chains/ethereum/addresses/0xd8da.../nfts/<contract>/<id>/balance
cat /eth/chains/ethereum/addresses/0xd8da.../nfts/<contract>/<id>/is_owner
cat /eth/chains/ethereum/addresses/0xd8da.../nfts/<contract>/<id>/approved

# Collection-level views:
cat /eth/chains/ethereum/contracts/<contract>/nft/kind          # erc721 | erc1155 | unknown
cat /eth/chains/ethereum/contracts/<contract>/nft/name
cat /eth/chains/ethereum/contracts/<contract>/nft/symbol
cat /eth/chains/ethereum/contracts/<contract>/nft/total_supply
cat /eth/chains/ethereum/contracts/<contract>/nft/owner_of/<id>
cat /eth/chains/ethereum/contracts/<contract>/nft/token_uri/<id>
cat /eth/chains/ethereum/contracts/<contract>/nft/is_approved_for_all/<owner>/<operator>
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

DeFi intent (Enso shortcuts) — natural language is the canonical input;
JSON works too. Requires an `[enso]` block in `~/.bloom-eth/config.toml`
with an API key (`BETH_ENSO_KEY`):

```sh
echo 'swap 0.1 eth to USDC on base' \
  > /eth/defi/intents/alice/new
ls /eth/defi/intents/alice/
cat /eth/defi/intents/alice/<sess>/plan.md
echo y > /eth/defi/intents/alice/<sess>/confirm
```

ERC-20 token-in routes auto-prepend an `approve(spender, max)` ahead
of the swap when the current allowance is insufficient.

Subscribe + read (TOML body, kinds: `block`, `balance`, `gas_price`,
`event`):

```sh
cat <<'EOF' > /eth/watch/new
kind = "block"
chain = "anvil"
EOF
tail -f /eth/watch/<id>/live              # in-process running state
cat    /eth/watch/<id>/history.jsonl     # rotated archive (1 MiB each)
```

Sign arbitrary message / raw hash / EIP-712 typed data:

```sh
echo 'hello world' > /eth/wallets/alice/sign/message
cat /eth/wallets/alice/sign/message.sig
cat eip712.json     > /eth/wallets/alice/sign/typed_data
cat /eth/wallets/alice/sign/typed_data.sig
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
