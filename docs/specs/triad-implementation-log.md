# Triad implementation log

This log records fail-closed implementation choices where
`2026-07-23-triad-process-architecture.md` is intentionally silent.

## W1 contracts

- **UTC wire representation:** Protocol timestamps use canonical unsigned
  decimal milliseconds since the Unix epoch. This avoids JSON floating-point
  ambiguity and follows section 18's canonical-decimal rule for large
  integers. Services still obtain and validate time according to section 10.3;
  this choice defines serialization only.
- **Initial CryptoSuite registry:** W1 reserves
  `secp256k1-keccak256-recoverable`,
  `secp256k1-sha256-recoverable`, and `ed25519-message`. Each identifier fixes
  its key specification, digest/message input kind, and normalized output.
  Unknown suites fail closed. Enrollment may advertise any strict subset.
- **Service-application signatures:** Local protocol envelopes use Ed25519
  application signatures in addition to OS peer credentials. The application
  signing key remains service-owned; the shared contract exposes only the
  pinned public verification key.
- **Protocol crate placement:** The neutral `bloom-triad-protocol` crate lives
  in the Machine repository during extraction. Broker and Signer use a local
  sibling path while the repositories are developed together. It contains no
  wallet private-key, PRF, WKEK, backend-credential, or custody-plaintext
  representation. W9 must replace local development paths with the frozen
  released artifact before either extracted repository is releasable.
- **Unknown provider errors:** Backend errors not explicitly mapped by a
  reviewed backend become `IndeterminateAcceptance`, never retryable-before-
  acceptance.
- **Signing identity domains:** Stable signing operation IDs hash the JCS
  `SignOperationIdentity` after the ASCII domain
  `bloom-sign-operation/v1`. Attempt digests hash the JCS unsigned
  `bloom.sign-request/1` with only `attempt_digest` omitted. The reviewed
  vector freezes both preimages and demonstrates that boot, validity, and
  attempt-ID changes affect only the attempt digest.
- **Closed public lifecycle registries:** W1 freezes approval and operation
  states directly from sections 10.4 and 16, and custody ceremony states from
  section 13.6. The initial public credential registry is deliberately limited
  to `ACTIVE` and `REVOKED`; adding a state requires a protocol-minor change
  rather than silently accepting an unknown token.
- **W1 cross-repository baseline:** Machine protocol commit `d01e82f` is
  consumed by Signer commit `5c09a6a` and Broker commit `6e3985b`. The sibling
  repositories intentionally have no remotes during extraction.

## W2–W3 implementation commits

- Broker durable journals and reservation ledgers are frozen at `ac7ef23`.
- The Signer security seam is frozen at `2fe667a` and consumes the protocol
  validity correction at Machine commit `bb09c32`. It includes compiled
  backend-instance routing, encrypted local custody, signed derivation grants,
  self-contained monotonic backup/restore, exact structural SignRequest
  validation, per-wallet policy signing, and signed/idempotent revocation
  reconciliation.
- W0 packaging remains an explicit parallel release gate; no W2/W3 service
  code claims that host-level isolation has already passed.

## W4 Broker seam

- Broker commit `0bf0e81` consumes Machine protocol commit `f30340c` and
  Signer seam commit `2fe667a`.
- Canonical approval metadata and lifecycle activation share the Broker
  journal transaction. Authorization and revocation reconciliation additionally
  share one in-process linearization barrier; the reservation transaction
  independently requires the canonical approval to be exactly `ACTIVE`.
- Installer-signed provenance is required for Petal, CLI, and System subjects
  at prepare and on every use. Runtime provenance must equal the frozen record.
- Petal value accounting aggregates duplicate debit and fee lines with checked
  unsigned 256-bit arithmetic. Native fees consume the same atomic asset
  ledger while retaining a distinct fee-exhaustion status.
- Assurance implementations are compile-time registry entries pinned by
  artifact digest. A proof or attestation can satisfy required assurance only
  when its reviewed contract explicitly establishes every selector and
  accounting field used for that decision.
- Signer approval-tombstone unions are independently signature-checked,
  digest/count checked, monotonically persisted, and enforced even if journal
  state application is interrupted. Wallet epoch divergence blocks prepare
  and activation until reconciliation converges.
- The W4 review gate passed with 12 W2 journal tests and 7 W4 authority tests,
  all-feature workspace tests, and Clippy with warnings denied.

## W5 ceremony

- Machine protocol commit `0bd5a9c` freezes canonical ceremony challenges,
  raw WebAuthn proofs, HPKE AAD, signed Signer contributions and custody
  results, public credential summaries, and reviewed vectors.
- Signer commit `94170be` consumes protocol commit `0bd5a9c`. It independently
  verifies raw WebAuthn bytes, keeps PRF and custody plaintext behind
  single-use HPKE, atomically commits wallet/credential/policy/backend effects,
  provisions signed public root KeyRefs, preserves exact imported secp256k1
  scalar identity, delivers optional recovery material only to Browser, and
  reconciles interrupted key derivation without path reuse.
- Broker commit `88cde7c` consumes protocol commit `0bd5a9c` and Signer commit
  `94170be`. It owns the canonical listener and review manifest, enforces
  origin/host/token/rate limits, independently verifies WebAuthn, keeps opaque
  custody ciphertext out of public status, and retains authenticated
  Browser-only results in `AWAITING_RECOVERY_ACK` until durable acknowledgement.
- The W5 review gate passed after three read-only review rounds. All-feature
  workspace tests and Clippy with warnings denied pass in Machine, Signer, and
  Broker; the shipped Browser asset additionally executes its ChaCha20-
  Poly1305 and X25519/HPKE self-tests under Node.

## W6 AWS KMS backend

- Signer commit `a3ce379` implements the compile-time AWS KMS backend and
  consumes Machine protocol commit `0bd5a9c` and Signer ceremony commit
  `94170be`.
- Production construction uses the AWS SDK with a statically configured
  WebIdentity role, token file, and session name. It does not instantiate the
  ambient credential chain. Regional KMS and STS hosts are declared exactly,
  SDK retries are disabled, and provider calls are deadline- and quota-bound.
- Enrollment pins the immutable key ARN, account, region, usage, spec,
  algorithm, canonical SPKI, fingerprint, and secp256k1 address. Enrollment
  and provider-audit state is atomically persisted under HMAC-SHA256 using a
  Signer-owned key and is bound to the canonical full backend configuration.
- KMS signing uses `ECDSA_SHA_256` with `DIGEST`, DER-decodes signatures,
  normalizes low `s`, derives recovery parity against the pinned public key,
  and returns Bloom's recoverable encoding. Unknown, internal, dispatch, and
  timeout outcomes remain indeterminate; only a narrow reviewed rejection
  allowlist is definitive, and the backend never retries a signing call.
- The W6 review gate passed after three read-only review rounds. The AWS SDK
  replay test and ten backend fault/restart/normalization tests pass, as do the
  all-feature Signer workspace tests and Clippy with warnings denied.
- W0 remains the release gate for OS-enforced egress and installed IAM policy,
  CloudTrail, principal, and sandbox configuration. W6 validates and binds the
  declarations but does not claim those host controls have been provisioned.

## W7 Machine integration

- Machine commit `b2e4f6a` adds the keyless typed Broker client and the
  payload-bearing Petal signing ABI. Trusted runtime provenance, final
  preimage bytes, claim assurance evidence, frozen action/advisory bindings,
  operation identity, and response identity are preserved end to end.
- Petal signing v0.1 hash-only single and batch calls fail closed with
  `UNSUPPORTED_VERSION`. Production-reachable legacy VFS and transaction
  hash-signing adapters also fail closed; their direct keystore-backed
  implementations are test-only.
- The production CLI write composition root constructs Machine with a bounded,
  canonical Unix Broker connector. Broker ceremony projections retain URL and
  expiry only for the exact originating approval/custody operation while it is
  awaiting the user, and clear them on mismatch, expiry, or terminal state.
- Broker commit `d55c3c1` moves current package/route provenance into an
  installer-owned catalog. Approval preparation no longer accepts provenance
  records from Machine, and every authorization compares the current verified
  catalog entry with the approval-frozen record.
- Catalog rotation, policy/revocation mutation, and signing authorization use
  one serialization barrier through reservation. The concurrency regression
  proves rotation cannot race between current-provenance validation and the
  durable reservation boundary.
- The W7 review gate passed after four read-only review rounds. The focused
  protocol, Machine client, Petal, daemon, CLI compile, full Broker workspace,
  and Signer workspace checks pass.
