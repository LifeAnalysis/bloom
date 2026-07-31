# Machine authority removal M0 inventory

**Status:** frozen implementation inventory

**Date:** 2026-07-31

**Specification:** [Machine Legacy Authority Removal](../specs/2026-07-31-machine-legacy-authority-removal.md)

**Baseline revision:** `triad-architecture` at `2767153bfab6`

## 1. Purpose

This is the M0 inventory required by the Machine legacy-authority removal
specification. It records the production Machine feature matrix, forbidden
dependency reachability, and the exact pre-removal source footprint. It is not
an acceptance of that footprint.

The machine-readable per-file ceilings are in
`packaging/triad/release/machine-authority-baseline.tsv`. The checker rejects a
new file containing a tracked marker or an occurrence count above the frozen
ceiling. Counts may only fall during M1--M6. Source roots are derived from the
complete local crate closure of every production feature set rather than a
hand-maintained crate list. The three forbidden implementation crates are
excluded from source counting because their entire presence is rejected by the
resolved dependency gate; all other local production dependencies are covered.

## 2. Production feature matrix

The allowed production Machine configurations are frozen in
`packaging/triad/release/machine-production-feature-sets.tsv`:

| Label | Package | Default features | Additional features |
|---|---|---:|---|
| `bloom-default` | `bloom` | yes | none |
| `bloom-portable` | `bloom` | no | none |
| `bloom-decompile` | `bloom` | yes | `bytecode-decompile` |
| `bloom-portable-decompile` | `bloom` | no | `bytecode-decompile` |
| `bloom-machine` | `bloom-machine` | yes | none |

`unsafe-debug-signer` and `local-integration` are not production feature sets.
Their declarations remain a strict-gate failure until M5 removes the embedded
authority workflows.

## 3. Forbidden dependency reachability

All four `bloom` feature combinations currently reach all three forbidden
legacy authority crates:

```text
bloom-keystore
  <- bloom
  <- bloom-daemon
  <- bloom-vfs

bloom-auth
  <- bloom-daemon

bloom-auth-api
  <- bloom
  <- bloom-auth
  <- bloom-daemon
  <- bloom-keystore
  <- bloom-proto
  <- bloom-tx
  <- bloom-vfs
```

The standalone `bloom-machine` package does not currently reach those crates,
but it is not yet the full VFS/Petal/transaction Machine composition shipped by
the release gate. M1--M6 must clean the actual `bloom` production graph rather
than substituting the thin binary as evidence.

## 4. Frozen source footprint

| Marker | Occurrences | Files |
|---|---:|---:|
| `ApprovalVerifier` | 28 | 8 |
| `AuthServices` | 57 | 9 |
| `AuthStore` | 50 | 8 |
| `AuthStoreWriter` | 20 | 6 |
| `EphemeralAgentKey` | 14 | 1 |
| `GrantStore` | 45 | 8 |
| `InMemoryGrantStore` | 11 | 4 |
| `KeystoreApprovalSignatureVerifier` | 2 | 1 |
| `KeystorePetalHost` | 12 | 4 |
| `PetalHost::sign_hash` | 10 | 5 |
| `PrivateKeySigner` | 29 | 10 |
| `RegistrationCoordinator` | 23 | 5 |
| `SignerCache` | 10 | 2 |
| `StoreApprovalVerifier` | 8 | 4 |
| `bloom_auth` | 121 | 19 |
| `bloom_keystore` | 38 | 13 |
| `ceremony_server` | 7 | 3 |
| `local-integration` | 77 | 7 |
| `policy-session` | 74 | 9 |
| `sign_hash_sync` | 10 | 4 |
| `unsafe-debug-signer` | 34 | 8 |

The exact file/count tuple is the enforcement source. This summary is for
review and planning only.

## 5. Gates installed by M0

`check-machine-authority-boundary.sh --check-baseline`:

- passes at the frozen baseline;
- rejects an increased occurrence count;
- rejects a tracked marker appearing in a new file anywhere in the resolved
  local production dependency closure; and
- permits removal without requiring a baseline rewrite after every edit.

`check-machine-authority-boundary.sh --require-clean`:

- evaluates every allowed production feature set with `cargo tree` normal and
  build edges;
- rejects any path to `bloom-keystore`, `bloom-auth`, or `bloom-auth-api`;
- rejects authority-restoring production feature declarations; and
- intentionally fails at M0. The triad release gate invokes this mode before
  building or packaging, so no current bundle can claim the clean Machine
  boundary prematurely.

The bundle scanner also rejects the frozen legacy crate, type, feature, and
policy-session markers. This is defense in depth; the resolved dependency graph
is the primary absence proof.

`test-machine-authority-boundary.sh` proves baseline success, ratchet failure
after a lowered ceiling, inventory generation, and strict release failure. A
Rust integration test runs that script as part of the service-activation test
suite and asserts the strict checker remains wired into the release gate.

## 6. M0 disposition

M0 removes no legacy code. Its purpose is to make the current violation
explicit, reproducible, and non-expandable before M1 starts moving consumers.
The strict gate may turn green only when M6 satisfies MA-01, MA-02, MA-10, and
MA-13; deleting or weakening the checker to obtain a release is nonconforming.
