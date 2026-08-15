# Solana Native Transfer Verifier — Wire Format and Golden Vectors

**Status:** frozen for `solana-system-transfer-v1`

**Crate:** `crates/bloom-solana`

**Normative authority:** [Verified Chain Petals](../architecture/Verified%20Chain%20Petals.md)

This document freezes the wire-format facts and golden vectors for the first
Solana verified chain Petal. The golden vectors are the shared contract across
Petal/Machine/Broker/Signer: the driver Petal constructs the message, Broker's
verifier independently parses it, and Signer signs the raw serialized message
bytes (while the SHA-256 digest serves only as Bloom's payload commitment).
The vectors are checked against the pinned Anza reference crates
(`solana-message 4.5.0`, `solana-system-interface 3.3.0`,
`solana-transaction 4.2.0`) by differential and reference-verification tests,
and every single-byte mutation of the golden message breaks the digest binding.

## Legacy message wire format

A legacy `Message` serializes, in order, with no framing:

```text
header        3 bytes: num_required_signatures, num_readonly_signed_accounts,
                       num_readonly_unsigned_accounts
account_keys  short-vec of 32-byte public keys
blockhash     32 bytes
instructions  short-vec of CompiledInstruction {
                  program_id_index: u8
                  accounts:         short-vec of u8 (indices into account_keys)
                  data:             short-vec of u8
              }
```

A `short-vec` length is a `ShortU16`: a little-endian 7-bit-per-byte encoding,
at most three bytes, with the high bit set as a continuation marker. The
decoder rejects alias encodings (a redundant zero continuation byte), more than
three bytes, and a continuation marker on the third byte.

The first byte (`num_required_signatures`) must have the version bit clear:
values `>= 0x80` denote a versioned (v0 / address-lookup-table) message and are
rejected. There must be no trailing bytes after the instructions.

### System Program native transfer

A native SOL transfer invokes the System Program
(`11111111111111111111111111111111`) with the `Transfer` variant. Its data is
exactly 12 bytes: a little-endian `u32` opcode `2` followed by a little-endian
`u64` lamport amount.

The canonical compiled form of a single-signer transfer has:

```text
header       { num_required_signatures: 1, num_readonly_signed_accounts: 0,
               num_readonly_unsigned_accounts: 1 }
account_keys [fee_payer, destination, system_program]
instructions [ { program_id_index: 2, accounts: [0, 1],
                 data: transfer(lamports) } ]
```

## Signing input and payload digest

**Solana signatures are Ed25519 over the raw serialized message bytes — there
is no pre-hash.** Anza's `Transaction::try_partial_sign_unchecked` passes
`message_data()` (the raw serialized message) directly to the signer, and
`Transaction::verify` checks each signature against those same raw bytes.

The ordered signing input is therefore the serialized message bytes themselves.
Bloom's `CryptoSuite::Ed25519Message` signer must feed the raw message bytes to
the Ed25519 primitive — never a SHA-256 pre-image — or the signature is
rejected by the network.

`payload_digest` (and `PetalUseClaim.ordered_hashes[0]`) remains SHA-256 of the
serialized message, used solely as Bloom's payload commitment for operation
identity, review, and audit. It is never the signing input. The golden
signature below verifies against the raw message bytes and fails against the
SHA-256 digest, and it is confirmed by constructing a real Anza `Transaction`
and calling `Transaction::verify`.

## Golden vectors

Deterministic inputs:

- fee-payer seed: `00 01 02 ... 1f` (32 bytes, `0x00..=0x1f`)
- lamports: `1_000_000_000` (1 SOL)
- recent blockhash: `42` repeated 32 times

Outputs:

```text
fee_payer (base58)  FAe4sisG95oZ42w7buUn5qEE4TAnfTTFPiguZUHmhiF
fee_payer (hex)     03a107bff3ce10be1d70dd18e74bc09967e4d6309ba50d5f1ddc8664125531b8
destination (base58) CZ8YUVdk7znjrUmnb5n7kgySk9yRAsQDYmyCxzfSky9t
destination (hex)    abababababababababababababababababababababababababababababababab
lamports             1000000000

message (hex)        0100010303a107bff3ce10be1d70dd18e74bc09967e4d6309ba50d5f1ddc8664125531b8abababababababababababababababababababababababababababababababab0000000000000000000000000000000000000000000000000000000000000000424242424242424242424242424242424242424242424242424242424242424201020200010c0200000000ca9a3b00000000

signing input: the raw serialized message bytes (signed directly)
payload digest (hex) d7770e6c7f805e94d5ed24b4b0d8ca93bdd7de4081ccb230fa257096b7dc5ec5
signature (hex)      cb7ccec4699662f08de156e8322e71e00abcf88506055ecdd849e5749f15b8590a65883e433069bad539fc8206781f4d9ec56c2bbd15c061cbf5570ce9ebbf0e
```

The message is 150 bytes; the signed transaction is 215 bytes (1-byte signature
count + 64-byte signature + message), well within the 1232-byte packet limit.

## Verifier contract

`verify_native_transfer(message_bytes, fee_payer, destination, lamports,
claimed_digest)` establishes:

| Fact | Check |
|---|---|
| Payload format | Canonical legacy message, strict short-vecs, no trailing bytes, version bit clear |
| Size | `message_len <= 1232 - 65` |
| Signers | `num_required_signatures == 1`, `num_readonly_signed_accounts == 0` |
| Account layout | exactly `[fee_payer, destination, system_program]`, all distinct |
| Fee payer / source | `account_keys[0] == fee_payer` (the selected Ed25519 child) |
| Program | `program_id_index == 2`, `account_keys[2] == system_program` |
| Instruction | single instruction, `accounts == [0, 1]`, opcode `2`, data length 12 |
| Destination | `account_keys[1] == destination` |
| Debit | decoded lamports `== lamports` |
| Message commitment | `claimed_digest == SHA-256(message_bytes)` (when supplied) |

It does not establish cluster/genesis identity, blockhash freshness, last-valid
height, fee quote, balance, simulation result, broadcast acceptance, or
finality. Those remain `machine_asserted`.

## Test coverage

- Differential tests against `solana-message` / `solana-system-interface`
  prove byte-identical message serialization.
- The golden signature verifies against the raw message bytes and is confirmed
  by constructing a real Anza `Transaction` and calling `Transaction::verify`;
  it fails against the SHA-256 digest.
- Exact digest binding: with the original payload digest held fixed, a
  150-byte × 8-bit mutation sweep proves every single-byte mutation breaks the
  commitment.
- Economic-field mutations (destination, amount, fee payer) fail even when the
  digest is recomputed; a blockhash change remains structurally valid; a
  different valid transfer with a matching claim passes.
- Versioned messages, multisigner/partial-signing forms, extra instructions,
  duplicate/overlapping account roles, oversized input, and malformed
  short-vec encodings are rejected.
