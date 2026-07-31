# Machine authority removal M2 execution consumer extraction

**Status:** implemented, locally verified, and independently reviewed clean

**Date:** 2026-07-31

**Specification:** [Machine Legacy Authority Removal](../specs/2026-07-31-machine-legacy-authority-removal.md)

## 1. Machine-owned execution state

The central outbox and paid-request paths now allocate stable action identities
through a purpose-specific Machine operation index instead of the legacy
approval database. The index is atomically persisted under a cross-process
lock and binds surface, local operation ID, wallet, and creation time. Its
schema cannot contain grants, challenges, credentials, signing nonces, or key
material. Conflicting or altered mappings fail closed.

Transaction staging, confirmation, cancellation, replacement, Petal outbox
handling, and paid-request planning obtain wallet addresses and advisory policy
inputs from authenticated public projections. Projected policy can produce an
early denial or an unsigned plan, but the Broker independently authorizes every
signature. No wire method was added.

## 2. Exact payload signing

Production paid HTTP, Hyperliquid owner actions, transaction execution, and
Petal execution use payload-bearing Machine-to-Broker signing calls. A durable
Machine exact-signing record binds the action and wallet identities, operation
class, exact preimage digest, claimed signing digest, trusted installer
provenance, plan facts, Broker operation IDs, approval ID, request nonce, and
expiry. Retries reuse the immutable identity; payload, digest, provenance, or
facts drift is rejected before another Broker call.

The x402 adapter freezes its unsigned credential draft, reconstructs the exact
EIP-712 preimage for EIP-3009 and Permit2, and replaces only the draft
signature. The MPP adapter likewise freezes its unsigned draft, reconstructs
the exact Tempo transaction or voucher preimage for charge and session
operations, and replaces only its signature. Draft writes are atomic, and
filesystem errors other than an absent draft fail closed.

Hyperliquid `usdSend` and `approveAgent` construction exposes the exact EIP-712
preimage alongside its digest. The production `usdSend` mounted path persists
its nonce and exact-signing identity across the Broker ceremony and never uses
the legacy hash-only host. Delegated agent-key creation and agent-order signing
remain explicitly assigned to M3.

## 3. Legacy-store isolation

A production daemon construction no longer opens or creates
`auth/auth.sqlite`; the old verifier, grant-backed paid-request signer, and auth
database setup compile only for tests or the transitional local-integration
feature. Production paid requests have no `AuthServices` input. The remaining
legacy authority inputs are confined to the policy-session and delegated-agent
consumers scheduled for M3 and M4 rather than being deleted ahead of their
replacement coverage.

## 4. Coverage

Local coverage includes operation-index durability and tamper rejection,
exact-signing retry and drift rejection, x402 and MPP exact-preimage recovery,
Hyperliquid owner-action preimage fidelity, wallet/request/Petal execution
regressions, production daemon construction with no legacy authority store,
and the existing transaction and Machine-client signing suites. An external
`bloom-vfs` integration test compiles the library through its production
`cfg(not(test))` branches and exercises x402 plus Hyperliquid `usdSend` and
`approveAgent` through Broker prepare/sign calls.

The complete affected package test suite passes with 436 `bloom-vfs` tests and
all tests for `bloom-daemon`, `bloom-hyperliquid`, `bloom-machine-client`,
`bloom-paid-http`, `bloom-paid-mpp`, `bloom-paid-x402`, and `bloom-tx`. Both
`bloom-daemon --no-default-features` and `bloom-daemon
--features local-integration` compile locally. The signed provenance enrollment
test, formatter, diff check, and M0 Machine authority ratchet are green.

## 5. Independent review

The required pragmatic reviewer found and verified fixes for three substantive
classes of issue: production `approveAgent` reachability, immutable semantic
identity and tamper binding for paid-protocol drafts, and missing production
route coverage. The final pass re-ran the production-route x402 and
Hyperliquid tests plus the x402 and MPP adapter suites and reported no remaining
material M2 spec-adherence or test-rigor findings. Delegated agent-key custody
and deletion of the global legacy policy-session authority surface remain
explicitly assigned to M3 and M4.
