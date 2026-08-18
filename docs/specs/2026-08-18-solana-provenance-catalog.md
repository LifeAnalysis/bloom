# Solana provenance-catalog entry (operator config)

**Status:** ready-to-use config draft — no code dependency. Consumed once the
§4 pin bump + `root_key_ref` reconciliation lands and the Solana signing flow
is wired.

The Machine/Broker provenance catalog is an installer-signed operator config
file (loaded via `bloom_machine_client::load_provenance_catalog`, path from
`BLOOM_PROVENANCE_CATALOG` or the site config). Each record authorizes one
Machine action class. The checked-in template lives at
`packaging/triad/macos/config/provenance-catalog.unsigned.json`; the macOS
enrollment signs it locally and installs it as `provenance-catalog.json`.

## The Solana record

```json
{
  "subject": {
    "kind": "system",
    "component_id": "bloom-machine",
    "operation_class": "solana.transfer.confirm"
  },
  "publisher": "bloom-installer",
  "operation_classes": [
    {
      "operation_class": "solana.native-transfer",
      "fee_asset": { "chain": "solana", "asset": "native" }
    }
  ],
  "installer_key_id": "<installer key id>",
  "installer_signature": "<base64url Ed25519 signature>"
}
```

## Field semantics

- **`subject.operation_class` — the action class** (`solana.transfer.confirm`):
  what the Machine's signing flow names when it looks up the record (mirrors
  EVM's `transaction.confirm`). A `solana.transfer.cancel` entry will follow
  the same shape when the cancel path lands (Solana has no `replace` — no
  nonce).

- **`operation_classes[].operation_class` — the claim's operation class**
  (`solana.native-transfer`): the value the claim carries and the verifier
  contract pins (the verifier's `REQUIRED_OPERATION_CLASS`). Broker matches
  `claim.operation_class` against this list (`account_claim_values`).

- **`fee_asset: { chain: "solana", asset: "native" }`**: Solana's fee is
  denominated in native lamports. Declaring a fee asset makes this a
  *fee-bearing* operation class, which means the claim **must** carry
  `DeclaredFee::Fee { chain: "solana", asset: "native", amount: <lamports> }`
  (`FEE_REQUIRED` otherwise). The declared fee is *machine-asserted* (a
  `getFeeForMessage` / prioritization-fee reading), not verifier-proven — the
  `solana-system-transfer-v1` assurance verifier independently establishes
  `declared_destinations`/`declared_debits`/`payload_digest`, never the fee.

- **`installer_signature`**: Ed25519 over the record's `unsigned_canonical_bytes()`
  (JCS of the record with `installer_signature` set to empty), in the
  `bloom-provenance-record/v1` signing domain. The unsigned template uses
  `"unsigned-template"` / `""`; the enrollment pipeline substitutes the real
  installer key id and signature. Broker verifies the signature and current
  catalog membership independently — Machine never supplies a record or
  signature for Broker to accept.

## Open alignment point (decided at signing-flow build, not now)

Whether the Solana transfer uses EVM's `System`-subject exact-signing path
(no `PetalUseClaim`) or a claim-bearing `ProofVerified` path determines how
`subject.operation_class` is consumed. The entry above is correct for the
`ProofVerified` claim path (which is the whole point of keeping the
independent verifier); if the signing flow instead lands on a System-subject
exact path, the record still authorizes the action but the verifier would
need an explicit invocation — reconcile this when the flow is wired.
