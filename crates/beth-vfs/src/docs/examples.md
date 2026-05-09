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
