# Triad merge readiness and proposed PR description

## Release blockers outside this branch

The following commits exist and are pinned immutably, but are not releases merely
because a feature branch exists. They must be reviewed and merged by their owning
repositories before a production triad release is represented:

| Component | Exact commit | Required external action |
| --- | --- | --- |
| service runtime | `2e402f03814166406ea6489b60422b0865d1f6c2` | merge `agent/macos-host-clock`, then tag through the runtime's release process |
| Petal contract/tooling | `eda6647c523bba161eaa22812aa0e75ec7782404` | merge `agent/triad-signing-api`, publish/tag the compatible toolchain |
| Gasless | `7023efe7a574b4402c407f7c1f75992b6672aa8d` | merge `agent/triad-payload-signing`, build and publish a new immutable Petal release |
| Privacy Pools | `902a2bb580ba0fa68b7cba6c736cea9d2856eb89` | merge `agent/triad-host-abi`, build and publish a new immutable Petal release |
| Venice x402 | `c58b7f97d90db60eee9ba0af60e7ff6e3afb2bad` | merge `agent/triad-payload-signing`, build and publish a new immutable Petal release |

The currently published Gasless `v0.1.1` and Venice x402 `v0.1.0` archives use
the retired hash-signing host route. Privacy Pools `v0.1.2` predates the triad
host ABI. Their catalog records are retained for an auditable immutable history,
but all three are ineligible for default activation. No replacement release hash
has been invented.

macOS production readiness remains blocked until the updated single-login and
two-login W0 suites pass on a privileged disposable macOS host for the exact
release subject. Signing, notarization, release credentials, tags, and a signed
conformance report remain release-owner work. Linux remains a packaging and
conformance input and is not described here as production-ready.

## Proposed corrected PR description

### Summary

This PR establishes the Machine → Broker → Signer authority boundary and a
fail-closed triad release shape. Machine CLI, daemon, VFS, mounted filesystem,
Petal, policy, ceremony, wallet, and signing flows use daemon IPC and Broker
authorization; Machine has no ordinary direct-Signer or local-signing fallback.
The release gate rejects legacy hash-only Machine routes and mutable external Git
dependencies.

The macOS Unix-principal installer now uses immutable digest-named releases,
mandatory compatibility/state-schema preflight, stop-before-switch activation,
an atomic `current` symlink, installed-triad health checks, rollback after failed
activation, and idempotent recovery of an interrupted activation journal.
Upgrades never regenerate enrollment identities or custody state. Operators can
remove runtime integration with `uninstall --retain-custody`, restore with the
exact signed release, or perform a separately confirmed permanent purge. A
restore or new enrollment cannot silently replace the shared release used by
another active login, and a failed restore returns to the retained state.

The compatibility manifest records exact Broker, Signer, service-runtime, and
Petal-contract commits and per-component state downgrade floors. Cross-edge
protocol tests reject incompatible major/minor ranges before authority-bearing
dispatch. The one-time Signer passkey migration is release-bounded and its
staging is retry-safe; partially staged state is not activated.

Default Petal installation verifies the release manifest, source commit, archive
name and SHA-256, package hash, and tooling commit before atomic owner
replacement. Near Intents and Enso remain the only default-eligible published
artifacts. Gasless, Privacy Pools, and Venice x402 stay disabled by default until
their triad-compatible branches are merged and new immutable releases exist.

### Validation status

Static, staged-root, protocol, migration, routing, bundle, formatting, Clippy,
and shell checks pass in the branch workspace. After clearing stale host NFS
mounts, the real mounted projection lane passes registration, import, credential
replacement, policy update, Signer-owned Petal key derivation, scoped payload
signing, wallet deletion, restart persistence, and portable MA-08 secret-artifact
confinement. A production macOS claim is not made by this PR: full live-memory
capture and the exact privileged disposable W0 runs still require the dedicated
host, followed by digest-bound conformance evidence, signing/notarization, and
release owner approval. No sibling branch or live PR is merged or mutated here.
