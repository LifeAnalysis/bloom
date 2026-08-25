# Machine authority removal M1 projection implementation

**Status:** implemented and independently reviewed; clean

**Date:** 2026-07-31

**Specification:** [Machine Legacy Authority Removal](../specs/2026-07-31-machine-legacy-authority-removal.md)

## 1. Projection boundary

`bloom-machine-client` now provides the Machine-internal
`WalletProjectionReader`, `CachedWalletProjectionReader`, and
`FileProjectionStore`. They compose only the existing authenticated Broker
methods for wallet, key, credential, and policy public reads. No wire method was
added.

A wallet generation contains the Broker public wallet record, public keys,
public credential descriptors, signed canonical policy snapshot, source
protocol, response digest, observation time, verification provenance, and
freshness. It contains no approval secret, credential wrap, recovery material,
private signer, or encrypted key blob.

Refreshes take a cross-process generation lock before asking Broker for the
full wallet list, then replace the generation and file atomically. Serializing
both observation and commit prevents an older full-list response from
tombstoning a wallet observed by a newer process. The durable generation is the
monotonic baseline, so a stale CLI process cannot overwrite a newer daemon
refresh. A lower policy version is rejected for live and tombstoned wallets.
Deletion creates a durable tombstone carrying the prior version, revocation
epoch, digest, and observation time. A tombstoned wallet cannot reappear unless
Broker advances its revocation epoch.

## 2. Degraded behavior

Live authenticated reads return `fresh`. Broker transport unavailability falls
back only to a validated cached generation and returns it as `stale`. An
uncached wallet returns `SERVICE_UNAVAILABLE`; a legacy keystore record is never
consulted as a projection fallback. Partial or internally inconsistent cache
files fail closed. Projections remain display, routing, staging, and simulation
inputs only and are not accepted by any signing or custody method as authority.

## 3. Migrated consumers

- CLI wallet list, address, QR source address, and Hyperliquid portfolio use
  projections.
- Mounted wallet discovery, address, address roles, public key, kind, canonical
  policy rendering, balance, nonce, and mempool public reads use projections.
- Status wallet counts and CLI status use projections and distinguish an
  unavailable projection from an authenticated empty wallet set. CLI status
  has a projection-only composition and does not construct the daemon or open
  legacy keystore/auth/outbox state.
- `/next.md` uses projection snapshots and renders stale or unavailable state.
- The bump scanner no longer opens legacy wallet policy files.
- Petal EVM staging resolves its public wallet address from the projection;
  removal of its remaining legacy planning-policy input belongs to M2.
- Watch execution has no wallet-keystore lookup to migrate.

The canonical Broker policy has no Machine-local bump timing fields. The bump
scanner therefore uses its existing explicit global defaults for current or
visibly stale projected wallets. Unknown or unavailable projection state now
suppresses advisory output instead of silently treating the wallet as
authenticated. This is advisory scheduling only and does not weaken Broker
signing or policy enforcement.

## 4. Coverage

The projection unit suite covers atomic persistence, stale restart reads,
uncached failure, same-process and cross-process-style policy rollback,
serialized observation during wallet creation, deletion tombstones,
policy-version and epoch resurrection rejection, and altered/partial cache
rejection. The mounted VFS integration fixture proves that a Broker/Signer-only
wallet with no legacy keystore record appears in the wallet directory and
public files, then keeps both addresses and canonical policy readable as
visibly stale data when Broker is stopped. CLI regression tests prove legacy
keystore records are ignored rather than treated as public projections, and
that status creates no legacy authority directories.

The M0 source ratchet remains green and its legacy marker counts do not expand.

## 5. Independent review

The required pragmatic follow-up review verified the complete M1 diff after
the refresh-ordering, offline-policy, status-composition, and bump-scanner
findings were repaired. The reviewer reran the focused regressions and reported
no remaining material spec-compliance or test-rigor finding.
