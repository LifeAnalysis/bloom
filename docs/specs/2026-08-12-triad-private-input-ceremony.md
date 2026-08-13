# Triad private-input ceremony

**Status:** research draft; not approved for implementation or merge  
**Replaces:** the pre-Triad prototype in Bloom PR #158  
**First consumer:** Privacy Pools private withdrawal

## Problem

Privacy Pools needs an owner-supplied withdrawal destination that must not pass
through agent-visible chat, command output, logs, or public VFS state. The owner
must see and approve the exact amount, asset, network, source note, and proposed
destination before the destination is released to the Petal.

This is an owner-input ceremony, not a signing approval or custody operation.
It must reuse Triad's ceremony infrastructure without weakening its process
boundaries or pretending to enforce properties it cannot enforce.

## Security claim

The ceremony protects the input from the agent-facing product surface and
requires fresh owner presence before release. It does **not** hide the input
from Bloom Machine or the requesting Petal: both necessarily handle the value
after approval. It does not contain a compromised Machine or a compromised
trusted Petal, and it does not by itself prove that the eventual withdrawal
matches the reviewed context.

Consequently:

- only an installer-pinned package hash and route may request the ceremony,
  and Broker independently verifies that signed provenance rather than trusting
  Machine's assertion;
- Broker owns the browser application, token, WebAuthn verification, expiry,
  rate limits, session state, and audit record;
- Machine never binds or proxies ceremony HTTP and never reconstructs a URL;
- the guest receives a non-secret operation ID, never a ceremony URL or token;
- the approved value is released only to the same package hash and route;
- the approval receipt binds a hash of the value and the exact transfer context;
- a consumer-specific verifier is required before claiming that execution is
  cryptographically bound to the approved value.

## Flow

1. A Petal calls `request-input` with a typed input kind, exact transfer
   context, subject reference, and caller request ID. It cannot supply HTML,
   ceremony copy, or choose the credential that counts as owner approval.
2. Machine injects installed package provenance and resolves the approval
   identity from owner policy. Broker independently verifies that the signed
   provenance catalog authorizes the exact package and route for `owner-input`.
3. Machine sends an idempotent `owner_input.prepare` request to Broker over the
   authenticated Machine–Broker channel.
4. Broker returns a public operation ID and keeps the single-use ceremony URL
   in its owner-facing projection. Machine may expose that URL only through a
   deliberate owner CLI/UI surface; it is never returned to guest Wasm.
5. Broker obtains a narrow, signed verification contribution from Signer for
   the policy-selected approval identity, then renders host-owned wording plus
   the exact amount, decimals, asset, network, source, and entered value. The
   passkey assertion confirms owner presence and binds the review digest.
   Signer verifies or records the assertion and credential counter against
   that digest, but receives only the digest—not the entered value—and performs
   no transaction signing or custody mutation.
6. Machine polls Broker by operation ID. Once ready, Broker releases the value
   and an acknowledgement token only to the same authenticated Machine request
   and package provenance.
7. Machine returns both to the requesting Petal through the host call. The
   Petal durably stores the value in its secret namespace, then acknowledges
   receipt. Broker deletes the value and rejects replay.

Crashes before acknowledgement are at-least-once delivery to the same trusted
origin. Acknowledgement is idempotent. Terminal sessions retain only public
status, the value hash, review digest, provenance, timestamps, and audit data.

## Ownership

| Concern | Owner |
|---|---|
| WIT types and guest lifecycle | Petal contract |
| Package/route provenance injection | Machine |
| Machine–Broker adapter and owner projection | Machine |
| Ceremony HTTP, WebAuthn, value retention, replay and audit | Broker |
| Approval identity, passkey authority, and credential counter | Signer, using a digest-only contribution without value access, signing, or custody mutation |
| Wallet keys or transaction signatures | Not involved |
| Persisting the approved private value | Trusted Petal secret store |
| Binding the final withdrawal to the receipt | Privacy Pools verifier |

## Minimal protocol

The Broker API needs three typed operations:

```text
owner_input.prepare(request + provenance) -> pending(operation_id)
owner_input.result(operation_id + provenance) -> pending | ready(value, ack)
owner_input.ack(operation_id + ack + provenance) -> consumed
```

Machine creates the Triad operation ID; the Petal's caller-provided ID is only
request metadata. Broker binds the operation ID to a canonical digest of the
full request and provenance, resumes an identical retry, and rejects reuse with
different content. The ceremony URL is deliberately absent from Petal-facing
results. Broker's existing owner projection carries it.

The initial input kind is an EVM address. The initial transfer context is:
network, asset, amount in base units, decimals, and source ID. New kinds require
their own canonical validation and host-owned rendering; arbitrary prompts,
HTML, schemas, labels, or scripts from a Petal are prohibited.

## Contract consequences

The provisional Petal #18 contract is not the final contract. In particular,
guest-controlled `title`, `prompt`, and `approval-wallet` fields violate the
ownership split above and must be removed or replaced with closed, host-owned
types and owner-policy resolution. Its subject reference and transfer context
also need canonical validation rules shared by Machine and Broker. A version
number on that draft does not make these decisions stable.

## Implementation shape

The Bloom-side successor to PR #158 should contain only:

- the private-input capability and WIT adapter;
- provenance-catalog authorization;
- a thin `MachineBrokerClient` adapter;
- owner projection plumbing that never enters guest state; and
- contract and cross-process tests.

Session management, HTML, WebAuthn, expiry, rate limiting, and audit belong in
`bloom-broker` and must extend its existing ceremony framework. The old daemon
manager, daemon ceremony routes, embedded HTML, and direct keystore access from
PR #158 must not be ported.

## Merge gates

- Human approval of this threat model and ownership split.
- Triad architecture rebased and its Machine–Broker protocol stable.
- Broker API and implementation reviewed independently.
- Petal contract reviewed against the final Broker lifecycle.
- Installer-signed provenance authorizes the exact Privacy Pools package/route.
- End-to-end test proves: no URL in guest state, no value in public VFS/logs,
  exact review rendering, origin binding, crash recovery, expiry, and replay
  rejection.
- Privacy Pools verifier demonstrates whatever execution-binding claim the
  product intends to make; otherwise documentation must state that the trusted
  Petal enforces the reviewed context.

Until these gates pass, PR #158, Petal #18, and Privacy Pools #4 remain research
drafts and must not merge.
