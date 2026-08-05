# Neutral Service Runtime Extraction

**Status:** Approved for implementation
**Date:** 2026-08-05

## 1. Purpose

Broker and Signer already compile without Machine, daemon, wallet, transaction,
Petal, legacy-auth, or legacy-keystore packages. The remaining isolation defect
is narrower: both repositories fetch security-critical mechanical crates from
the Bloom product monorepo, and Signer CI still explicitly checks that repo out.

This change makes the shared local-service substrate independently fetchable,
buildable, and auditable without the Machine/daemon repository. It must not
change protocol bytes, authority semantics, authentication, checkpointing,
trusted-time behavior, listener behavior, or platform-containment decisions.

## 2. Target ownership

Create a public `bloom-directory/bloom-service-runtime` repository containing:

- `bloom-rpc-wire`;
- `bloom-triad-local-transport`;
- `bloom-audit-checkpoint`;
- `bloom-service-activation`;
- `bloom-trusted-time`; and
- after a separately reviewed mechanical split, `bloom-platform-containment`.

The repository is a mechanical service substrate. It must not own wallet,
approval, policy, custody, ceremony, credential, Petal, transaction, or signing
authority contracts. Machine--Broker contracts remain in `bloom-broker-api`;
Broker--Signer contracts remain in `bloom-signer-api`.

## 3. Scope and non-goals

### 3.1 In scope

1. Remove the vestigial Bloom sibling checkout from Signer CI.
2. Relocate Bloom product-packaging tests out of the extracted runtime crate.
3. Import relevant history into the new repository and preserve source,
   fixtures, generic tests, authors, dates, and commit messages as far as a
   filtered cross-repository extraction permits.
4. Split macOS platform-containment verification out of transport in its own
   reviewable commit without changing behavior.
5. Pin Signer, Broker, and Bloom to one immutable neutral-runtime revision.
6. Remove the extracted crate copies and Git-source patch from Bloom.
7. Run all affected repository and real-process tests locally.
8. Commit and push every affected repository only after its dependency order
   permits a locally verified immutable pin.

### 3.2 Deferred follow-through

These are documented future changes and must not hold up this extraction:

- per-service manifest schemas generated from one packaging model; and
- publishing checksummed crates from signed neutral-repository releases.

The later manifest change will remove Signer's unused Machine entry. The later
registry publication will replace repository fetches with bounded archives.
Neither belongs in the semantically null extraction review.

### 3.3 Explicit non-goals

- no duplicate edge-owned transport implementations;
- no authority-domain migration;
- no wire or digest changes;
- no compatibility layer for the deferred manifest split;
- no vendored or generated source tree;
- no dependency/provenance inventory framework;
- no broad vocabulary scanner or exhaustive third-party allowlist; and
- no unrelated cleanup or line-count expansion.

## 4. Work chunks and exit gates

### C0 — baseline and Signer CI cleanup

Remove Signer's explicit `bloom-directory/bloom` checkout and sibling move.
Run the locked Signer workspace from a standalone checkout/layout. Record the
current source revision and current passing behavior for the six target units.

Exit gate: Signer needs no sibling path, and removing the explicit checkout does
not change its build or tests. Cargo may still fetch Bloom until C4.

### C1 — test ownership cleanup

Move these unchanged product tests from `bloom-service-activation/tests` to
`bloom-it/tests`, retaining their assertions and workspace-relative behavior:

- `linux_packaging.rs`;
- `macos_launchagent.rs`; and
- `triad_release.rs`.

Generic activation and privileged runtime tests remain with the extracted
crate. Do not delete or weaken any acceptance assertion.

Exit gate: Bloom's full affected suites pass, while the five extraction-source
crates no longer require `packaging/triad` to test in isolation.

### C2 — history-preserving neutral extraction

Create the public neutral repository using filtered history, not a claimed
cross-repository `git mv`. Include relevant predecessor history where practical.
Commit hashes may change; authors, dates, messages, and useful path history must
be retained. Identify Bloom `8466712fcab78b079b5e50ae4269e6c3ed9d6e5a`
as the extraction source without generating a provenance inventory.

The extraction commit must keep production source, generic fixtures, vectors,
and generic tests byte-identical. Only root workspace/build scaffolding and the
C1 test ownership move may differ. The new repository must build and test
without Bloom, Broker, or Signer checkouts.

Exit gate: full neutral workspace tests, strict Clippy, formatting, and a
review-time source/fixture comparison pass.

### C3 — platform-containment crate split

In a separate neutral-repository commit, create
`bloom-platform-containment`. Move `NetworkContainmentGuard`, its status schema,
and its tests out of transport without behavioral changes. Broker and Signer
will consume this crate directly in C4. Do not combine this with `EdgeManifest`
work or rename the transport package in this migration.

Exit gate: neutral tests prove identical accepted/rejected containment status
behavior, and transport no longer exports platform policy.

### C4 — immutable consumer repinning

Update in dependency order:

1. Signer pins all six packages to one neutral revision, imports containment
   directly, regenerates its lockfile, passes its full local suite, commits, and
   pushes.
2. Broker pins that neutral revision and the new Signer revision, imports
   containment directly, regenerates its lockfile, passes its full local suite,
   commits, and pushes.
3. Bloom pins the same neutral revision and the new Broker revision, deletes the
   six local crate copies, removes the Bloom Git `[patch]`, relocates any source
   inventory references, and passes its full local and release suites before
   committing and pushing `triad-architecture`.

No consumer may resolve two identities for a neutral type-bearing crate.

Exit gate: clean standalone checkouts resolve only immutable cross-repository
sources, have no sibling paths, and pass locked builds and tests.

### C5 — basic isolation checks and final audit

Keep permanent checking deliberately small:

- Signer and Broker CI perform a direct lockfile assertion that the exact source
  `git+https://github.com/bloom-directory/bloom.git` is absent.
- Neutral CI asserts that its lockfile has no Git source from Bloom, Broker, or
  Signer.
- Existing process-boundary tests continue to reject Machine/daemon packages in
  Broker and Signer production graphs.
- A simple dependency-tree check confirms one resolved `bloom-rpc-wire`.

Do not add a new gate framework, generated allowlist, transitive dependency
inventory, or raw grep ban on ordinary cryptographic words.

Exit gate: all four repositories are independently buildable, documented,
reviewed, committed, pushed, and their remote heads are verified.

## 5. Pragmatic review rule

After each C0--C5 chunk, a reviewer sub-agent must inspect the actual diff and
test evidence before the next dependent chunk is accepted. The reviewer should
report or fix only concrete semantic changes, missing files/tests, dependency
leaks, source-identity duplication, history/extraction mistakes, or real test
failures. It must not propose speculative redesign, general cleanup, new
frameworks, exhaustive allowlists, or unrelated hardening.

## 6. Acceptance criteria

1. Signer and Broker contain no dependency source pointing at the Bloom product
   repository.
2. Their production graphs contain no Machine or daemon implementation package.
3. A Signer audit/build does not clone or check out the Bloom product repo.
4. The neutral repository contains only the six mechanical/platform packages.
5. `NetworkContainmentGuard` is no longer part of the transport package.
6. All consumers resolve the same neutral revision and one wire type identity.
7. Bloom contains no local copy of the extracted crates and no self-Git patch.
8. Existing wire vectors, authentication, checkpoint, trusted-time, activation,
   containment, release, and process-boundary behavior remain passing.
9. Product packaging tests remain in Bloom and retain their assertions.
10. No sibling checkout is required in any repository.
11. Every major chunk has pragmatic review approval.
12. All affected repositories are committed, pushed, and remotely verified.

## 7. Deferred architecture

The next standalone specification should replace the shared `EdgeManifest` with
service-owned closed schemas while retaining generic `ManifestPeer`, `PeerAcl`,
and identity verification in the neutral substrate. After that boundary has
stabilized, publish the neutral crates from signed releases and use controlled
compatible versions plus lockfiles and a single-version assertion.
