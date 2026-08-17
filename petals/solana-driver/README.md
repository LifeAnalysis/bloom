# solana-driver

The Bloom Solana driver Petal: a content-addressed first-party chain driver
for native SOL transfers.

## Routes

- `transfer.stage.json` — constructs a canonical legacy, single-signer System
  Program transfer message from typed input (`fee_payer_base58`,
  `destination_base58`, `lamports`, optional `blockhash_hex`). Without an
  explicit blockhash it performs exactly one Machine-mediated
  `getLatestBlockhash` read on the named `chain_profile`. Returns the message
  bytes, SHA-256 payload commitment, and economic facts.
- `transfer.assemble.json` — assembles the complete signed transaction from
  the message bytes and an externally produced 64-byte Ed25519 signature.

## Capabilities

Imports `bloom:chain/read` (Machine-mediated; no endpoints or credentials are
visible to the guest) and `bloom:store` (result persistence). No network,
signing, key-derivation, or VFS authority. Signing happens through the triad
(Broker approval, Signer custody) outside this Petal; the independent
`solana-system-transfer-v1` verifier in `bloom-solana` re-parses every
message this driver constructs.

## Build

`./build-petal.sh` builds for `wasm32-unknown-unknown`, wraps each route as a
component, validates, and records content-addressed artifacts under
`artifacts/`.
