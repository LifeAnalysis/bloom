# Machine authority removal M3 Petal-scoped sub-keys

**Status:** implemented, locally verified, and independently reviewed clean

**Date:** 2026-07-31

**Specification:** [Machine Legacy Authority Removal](../specs/2026-07-31-machine-legacy-authority-removal.md)

## 1. Generic scope and unchanged wire surfaces

`PetalKeyScope` canonically binds the wallet, parent root `KeyRef`, installed
package hash, exact route, optional Petal-local agent identity, reviewed
purpose, allowed suites, maximum lifetime, and custody operation. Its
domain-separated digest is bound into the existing `key.derive_prepare`
request and Signer ceremony contribution. This extends the custody DTO without
adding a Machine-to-Broker or Broker-to-Signer method.

Broker verifies the scope against the installer-signed provenance catalog and
wallet policy, renders every scope field in the exact custody review, and
records the resulting child-to-scope binding durably. Signer independently
binds the parent root to the named wallet, chooses the derivation namespace and
path, persists the immutable scope beside the enrolled child, and rechecks the
Petal identity, wallet, suite, and lifetime at approval activation and every
sign authorization.

## 2. Petal and owner surfaces

The component host exposes versioned `bloom:key/derive@0.1.0`. Its guest DTO
contains no provenance fields and rejects unknown fields; the runner injects
the installed package hash and route. Stable request identities reconcile one
custody operation, while changed terms or tampered Machine reconciliation state
fail closed. The guest receives only a pending operation/scope identity or the
public child `KeyRef` and addresses.

Machine atomically stores public reconciliation state as mode `0600` and
projects it read-only at mounted `petal-key-requests/`. The ceremony URL and
expiry are owner-readable only while pending and are never returned to the
guest. Completion clears the URL from the persisted projection.

Versioned `bloom:sign/signing@0.3.0` accepts an optional RFC 8785 canonical
public `KeyRef` and a closed `exact` or `reusable` selector choice, then routes
it through the existing payload-bearing `signing.sign` path. Exact mode
requires an explicit key, binds the choice into the operation identity, and
omits the Petal claim only after Machine validates its payload, purpose, and
trusted provenance; reusable mode forwards the validated claim and assurance
normally. Malformed, noncanonical, substituted, wrong-suite, or out-of-scope
keys and unknown selector values fail before signing. The `@0.2.0` interface
retains its root-key and reusable-selector behavior.

## 3. Native Hyperliquid retirement

The native Hyperliquid VFS authority handler and its local ephemeral-agent
implementation were deleted. Its compatibility mount contains only a
read-only migration notice and rejects every write without calling Broker.
The old private Hyperliquid signer API is absent from production compilation.
Public read-only Hyperliquid helpers may remain, but all future Hyperliquid
custody and signing belongs to an ordinary Petal using the generic scope.

Neither Broker nor Signer contains a Hyperliquid ceremony, input class,
namespace, policy branch, metadata type, or other venue-specific behavior.

## 4. Coverage

A real Broker-to-Signer integration registers a passkey wallet, commits policy
and signed fixture-Petal provenance, completes scoped child custody through the
Browser/HPKE ceremony, activates reusable and exact Petal approvals, and signs
with the child through each selector. It rejects cross-package, cross-route,
cross-wallet, System, CLI, replay, payload drift, revocation, and expiry
attempts. Reopening both authority databases preserves the public child and
immutable scope while boot deactivation denies signing until a new activation.

Component-fixture and fake-Broker tests cover exact import linking, provenance
injection, public-only results, retry, tamper, owner projection, canonical
explicit-key signing, and `@0.2.0` compatibility. Signer tests cover fresh
allocation, cross-principal isolation, restart, backup/restore, missing scope,
and browser attempts to choose a namespace.

Local verification passed for the full `bloom-broker` and `bloom-signer`
workspaces; all targets in `bloom-petals`, `bloom-vfs`, `bloom-daemon`,
`bloom-hyperliquid`, `bloom-machine-client`, and `bloom-triad-protocol`; normal
and no-default-feature production checks; formatting and diff checks; and the
Machine authority source ratchet.

## 5. Independent review

The initial pragmatic review found three concrete gaps. Guest VFS access was
not filtering the owner-only ceremony projection; Signer did not yet compare a
reusable selector's operation classes with the durable scope purpose; and the
executable component fixture linked the new import without invoking it.

The fixes deny every normalized guest VFS operation under
`petal-key-requests` while retaining owner access, enforce the durable purpose
both at activation and before every sign reservation, and execute an actual
Wasmtime component through `derive(Pending)`, `derive(Ready)`, and scoped
`sign@0.3`. Regression tests include normalized traversal, corrupted durable
approval terms, exact authority-call ordering, exact/reusable operation-ID
separation, and real exact child signing.

The final follow-up found no remaining material M3 issue. It independently
confirmed both selector modes, owner-only ceremony isolation, durable purpose
enforcement, executable fixture coverage, real Broker-to-Signer exact and
reusable signing, replay and payload-drift rejection, and the absence of new
wire methods or venue-specific Broker/Signer behavior.
