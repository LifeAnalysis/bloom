# VFS/CLI Parity Ledger

**Status:** superseded historical record
**Original date:** 2026-06-28
**Inspected baseline:** `1ea1e65`

The original ledger described a pre-triad baseline and is no longer a product
contract, benchmark definition, or source of executable instructions. Its
rows for Machine-owned wallet state, foreground signing, native Polymarket and
Hyperliquid handlers, and service-local authorization have been removed so an
active documentation search cannot teach those retired flows.

## Current parity rules

| Surface | Current parity requirement |
|---|---|
| Wallet discovery and public keys | CLI and mounted VFS read authenticated Broker projections or an explicitly stale Machine public cache |
| Custody | CLI and retained VFS adapters call existing Broker custody prepares; Browser uses Broker ceremony HTTP and Signer commits the effect |
| Policy update | `policy.validate_update` prepares the `policy_update` custody ceremony; commit requires its completed receipt |
| Transaction and request staging | Machine may stage and simulate unsigned work without authority state |
| Transaction and request signing | Every retained CLI/VFS entry point sends exact payload bytes to Broker and observes Broker operation identity and Signer receipt |
| Approval lifecycle | Retained CLI/VFS management surfaces adapt existing Broker Sealed Approval methods only |
| Petal signing | Payload-bearing host calls only; installer provenance and route scope are injected by Machine and enforced by Broker and Signer |
| Petal delegated identities | Signer-owned Petal-scoped `KeyRef`; Machine and Petal receive public metadata only |
| Polymarket | Installed external Petal route contract; the pinned legacy package remains read-only/fail-closed until it publishes payload signing |
| Hyperliquid | Installed external Petal only; no native CLI or root VFS authority parity requirement |
| Degraded operation | Cached public reads, staging, and simulation may continue; signing, approval, policy, and custody mutations fail closed |

## Evidence expected for new ledger entries

A current parity claim must name:

- the mounted or CLI surface actually retained;
- the existing Machine-to-Broker method it uses;
- the Broker operation ID and, for cryptographic effects, Signer receipt;
- the exact local test or packaged acceptance criterion;
- degraded/unavailable behavior; and
- the immutable Petal package/route contract when the surface is external.

Do not revive rows from the former baseline. Derive any future ledger from the
current implementation and the normative documents:

- [`2026-07-23-triad-process-architecture.md`](../specs/2026-07-23-triad-process-architecture.md)
- [`2026-07-31-machine-legacy-authority-removal.md`](../specs/2026-07-31-machine-legacy-authority-removal.md)
