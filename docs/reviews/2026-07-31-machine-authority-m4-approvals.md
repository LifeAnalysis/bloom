# Machine authority removal M4 approvals and legacy-session removal

**Status:** implemented, locally verified, and independently reviewed clean

**Date:** 2026-07-31

**Specification:** [Machine Legacy Authority Removal](../specs/2026-07-31-machine-legacy-authority-removal.md)

## 1. Canonical Sealed Approval surface

The mounted wallet VFS now projects the existing Broker Sealed Approval
lifecycle at `sealed-approvals/new.json`, `active.json`, per-approval status,
limits, renew and revoke controls, and `revoke_all`. The Machine client adds
only convenience wrappers over the frozen `sealed_approval.list`,
`limit_state`, `renew`, `revoke`, and `revoke_all` methods. Requests are
wallet- and approval-bound before dispatch; cross-wallet responses, duplicate
list entries, mismatched approval identities, and overlong ceremony expiry
fail closed.

Prepare and renew persist only a mode-0600 public operation projection. Reads
reconcile the recorded operation through `ceremony.status`, require the
Sealed Approval ceremony kind and exact operation, URL, and expiry, and expose
the Broker launch URL only while the ceremony is awaiting the user. Success,
completion, cancellation, expiry, and failure remove the launch projection.
Approval authority and limits remain solely in Broker and Signer.

## 2. Legacy authority removal

The `wallets/<wallet>/policy-session/*` tree, its marker store, challenges,
grant/session handlers, and implementation tests were deleted. Wallet policy
updates now use the normal Broker `policy.validate_update` preparation,
Signer policy-update ceremony receipt, and Broker `policy.commit_update` path
in both production and tests.

TxEngine no longer accepts an approval verifier, auth writer, grant store, or
legacy PetalHost. Its owner-session and sealed-action execution APIs and all
local hash-signing branches were removed. Paid requests now always consume a
Machine-owned public execution snapshot and use Broker exact payload signing;
there is no AuthServices, grant, PetalHost, or keystore authorization branch.
The Daemon no longer composes AuthServices or SignerCache in default or
no-default production builds. The remaining embedded developer objects are
strictly test/`local-integration` gated for deletion in M5.

The deferred embedded ceremony feature still compiles, but every
broadcast-shaped legacy completion fails closed for both approval-only and
approval-and-execute requests, immediately revokes the newly minted grant,
drops its cached signer, and directs the developer to the out-of-process triad
harness. No deleted TxEngine API or developer-only Cargo feature was restored.

## 3. Isolation and documentation

Petal guest VFS authorization parses and normalizes paths before denying every
owner ceremony-token projection under Sealed Approval prepare/renew and policy
update pending/latest status. Tests cover lookup, list, read, write, `..`
normalization, denied-write non-delivery, owner access, and adjacent public
wallet reads.

Mounted help and the mount adapter use only the canonical `sealed-approvals`
vocabulary and routing. The removed `policy-session` name remains only in a
negative absence assertion and is not present in production help or VFS
discovery.

## 4. Coverage

Local verification passed:

- `bloom-machine-client`: 19 tests;
- `bloom-tx`: 138 tests;
- `bloom-vfs`: 333 tests and 8 production triad route tests;
- `bloom-daemon`: 80 tests plus default, no-default, and both
  `local-integration` feature checks;
- `bloom-mount`: 70 feature tests plus mounted write tests;
- Broker authority/journal restart persistence for an active approval;
- formatting, diff checks, and the tightened Machine authority source ratchet.

The production route suite proves x402 and Tempo MPP preparation, activation,
exact payload signing, approval identity, Signer receipt, durable exact slot,
and sent state. It also proves policy prepare-response recovery, receipt-only
commit, terminal URL clearing, and fail-closed native Hyperliquid writes.

## 5. Independent review

The initial pragmatic review found three material issues: Petal guest VFS
could read owner ceremony URLs; production Daemon still carried empty broad
legacy authority/cache fields; and mounted help still advertised the removed
policy-session API. The fixes added normalized guest-path isolation, excluded
legacy containers from production composition, and replaced all mounted help
and adapter vocabulary with canonical Broker-backed Sealed Approvals.

The first follow-up found that the deferred `local-integration` feature no
longer compiled because its ceremony server called deleted TxEngine APIs. The
fix removed those calls and made legacy broadcast completion explicitly
fail-closed without restoring a signing fallback. The final independent
follow-up reran both local-integration feature matrices, ceremony tests,
production policy/x402/MPP routes, guest isolation, canonical docs, approval
lifecycle, and Broker restart persistence and reported no remaining material
M4 findings.
