# Private Input Ceremonies

**Status:** implemented (`bloom:private-input/ceremony@0.2.0`)
**Audience:** Bloom engineers, Petal authors, and implementation agents

A private-input ceremony lets a Petal collect a value-bearing destination
(currently: an EVM address) from its owner through a local, passkey-gated
browser step, without ever seeing the value pass through VFS or the agent
session that drove the Petal. It is a narrower, purpose-built sibling of
Sealed Approval, not a replacement for it: see
[`Sealed Approvals.md`](./Sealed%20Approvals.md) for the general
signing-authorization model this reuses (challenge issuance, WebAuthn
assertion verification, grant minting and immediate revocation).

## Three identifiers, not one

The contract and this host implementation deliberately separate three
values that a first design pass collapsed into fewer, each time
reintroducing a version of the same problem:

- **`operation-id`** — non-secret, deterministic per distinct request
  *content* (the same fingerprint the host uses to decide whether a
  repeated `request-input` call is an idempotent resume, not the
  caller-chosen `request.id`, which is explicitly not guaranteed unique).
  Returned to the Petal while a ceremony is pending. Safe to persist in the
  Petal's own public VFS state — it carries no authority and cannot be used
  to complete, inspect, or tamper with the ceremony.
- **token** — secret, host-internal only. The loopback ceremony URL's path
  component and the daemon's session lock. Never returned to guest Wasm in
  any form.
- **handle** — secret, single-use. Minted only once a session completes,
  returned to the Petal solely alongside the released value itself. Holding
  it never grants access to anything the holder doesn't already have, which
  is why it's safe to hand to the same caller that already has the value.

A Petal that needs to show its owner where to complete a pending ceremony
does so by displaying `operation-id`; Bloom's own ceremony server resolves
that back to the real URL at `GET /private-input/by-operation/{operation-id}`,
reachable only from a local process. A Petal's sanctioned `bloom:http`
capability cannot reach it: `net.fetch` enforces HTTPS on port 443 only,
and this server is plain HTTP on the loopback ceremony port.

## Authorization

`bloom:private-input` is capability-gated like any other host interface,
but capability alone only proves a Petal's manifest declared it wants the
import — it says nothing about *which* Petal is asking. The host
additionally requires the request's `PetalRouteContext.package_hash`
(runner-injected from the verified installed package, never the component
itself) to match an explicit, per-name trusted-hash allowlist. Bare name
matching (`petal_root == "privacy-pools"`) is not sufficient on its own:
`petal_root` is a self-declared install-time label, and any component
installed or reinstalled under a colliding name would otherwise pass.

The allowlist is empty, and therefore fail-closed, by default. Wiring an
actual trusted hash for a given Petal is a separate, deliberate provenance
decision for whoever constructs the daemon — analogous to how other
preinstalled Petals are pinned to a specific reviewed commit and hash —
and is out of scope for the host runtime itself.

## Value-transfer context

Any request that releases a value-bearing destination must supply a
`transfer-context`: network, asset, an integer `amount-base-units` string
(never a display amount), `decimals`, and a source identifier. The host
renders this with its own labels in the ceremony page — using
`amount-base-units` next to a bare asset symbol without applying `decimals`
would misrepresent the amount by orders of magnitude, which is exactly what
the split exists to prevent — and binds it into the sealed approval both as
descriptive subject/plan text and as enforceable grant terms
(`terms.extra`). An owner approving a private-input ceremony is approving
an exact amount moving to an exact destination, not merely a destination.
