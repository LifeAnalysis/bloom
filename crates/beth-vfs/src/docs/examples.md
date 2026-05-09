# Examples

## Local Anvil round-trip

```sh
# 1. Start anvil
anvil --port 8545

# 2. In another terminal, create a wallet
beth wallet new alice --passphrase 'devonly'

# 3. Inspect chain
cat /eth/chains/anvil/head/number
cat /eth/chains/anvil/chain_id

# 4. Stage a send
echo '{"to":"0x70997970C51812dc3A010C7d01b50e0d17dc79C8","value":"0.1 eth","chain":"anvil"}' \
  > /eth/wallets/alice/chains/anvil/outbox/new.tx

# 5. Inspect plan
ls /eth/wallets/alice/chains/anvil/outbox/pending/
cat /eth/wallets/alice/chains/anvil/outbox/pending/0001-*/plan.md

# 6. Confirm
echo y > /eth/wallets/alice/chains/anvil/outbox/pending/0001-*/confirm

# 7. Inspect receipt
ls /eth/wallets/alice/chains/anvil/outbox/sent/
```

## Tools

```sh
cat /eth/tools/keccak/abc                     # hex digest
cat /eth/tools/address/checksum/0xabc...      # EIP-55 form
cat /eth/tools/unit/parse/1.5/eth             # → 1500000000000000000
cat /eth/tools/unit/format/1500000000000000000/18  # → 1.5
```

## NFTs (ERC-721 / ERC-1155)

```sh
# CryptoPunks #5822 — collection view (RPC-only, no etherscan needed):
cat /eth/chains/ethereum/contracts/0xb47e3cd837ddf8e4c57f05d70ab865de6e193bbb/nft/kind
cat /eth/chains/ethereum/contracts/0xb47e3cd837ddf8e4c57f05d70ab865de6e193bbb/nft/name
cat /eth/chains/ethereum/contracts/0xb47e3cd837ddf8e4c57f05d70ab865de6e193bbb/nft/owner_of/5822

# BoredApe #1 — per-holder view (history needs an etherscan API key):
cat /eth/chains/ethereum/addresses/0xd8da6bf26964af9d7eed9e03e53415d37aa96045/nfts/erc721_txs
cat /eth/chains/ethereum/addresses/0xd8da6bf26964af9d7eed9e03e53415d37aa96045/nfts/owned.json

# Per-token detail (auto-detects ERC-1155 and substitutes the {id}
# placeholder in the metadata URI):
cat /eth/chains/ethereum/addresses/0x.../nfts/0x.../1/owner
cat /eth/chains/ethereum/addresses/0x.../nfts/0x.../1/uri
cat /eth/chains/ethereum/addresses/0x.../nfts/0x.../1/metadata.json
cat /eth/chains/ethereum/addresses/0x.../nfts/0x.../1/is_owner       # true/false
cat /eth/chains/ethereum/addresses/0x.../nfts/0x.../1/balance         # always 1 for ERC-721
```

`metadata.json` follows `data:`, `ipfs://`, and `http(s)://` URIs (1 MiB
ceiling, 5s timeout). For ERC-1155 contracts the `{id}` placeholder in
the returned URI is substituted with the lowercase 64-char hex form of
the token id, per spec.
