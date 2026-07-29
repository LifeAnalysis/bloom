# Bloom triad macOS packaging source

This directory is source input for the signed installer. It is not installed
directly from a source checkout.

`com.bloom.broker.plist.in` deliberately gives launchd ownership of the
Machine→Broker socket, the revocation-control socket, and the canonical
`127.0.0.1:18734` ceremony listener. The installer renders every `@...@`
placeholder to an absolute path inside the signed application or the
appropriate per-login App Group container.

The Broker job requests failure-only `KeepAlive` with a five-second throttle.
A ceremony-listener conflict remains a fatal Broker startup failure and never
causes a fallback bind. The disposable macOS launchd conformance lane, rather
than this static source file, determines whether a failed registration is
retained and retried after the prior owner releases the listener.

The W0 macOS conformance test must exercise the rendered, signed LaunchAgent.
Static plist validation is necessary but is not evidence that socket handover,
cross-login failure, or retry works.

The three entitlements templates form two non-transitive IPC groups:

- Machine and Broker share `bloom.machine-broker`.
- Broker and Signer share `bloom.broker-signer`.
- Machine is not a member of the Broker–Signer group.

Every executable is App Sandbox-enabled. The local Signer entitlement has no
network capability. Broker receives only the server entitlement required for
the loopback ceremony listener. Hardened runtime and the absence of
`get-task-allow` are enforced by the signed-bundle scan rather than expressed
as entitlements here.

The root-owned edge manifest pins `trusted_time_source` to
`macos-managed-timed`, the platform-managed time service. Peer-supplied time
and arbitrary source identifiers are rejected.

The installer renders a distinct audit checkpoint directory for Broker and
Signer and passes it as `BLOOM_AUDIT_CHECKPOINT_DIR`. Each directory is owned
and writable only by its service principal and lies outside the shared socket
container; Machine and Broker cannot read the Signer audit checkpoint.
Runtime checkpoint writes accept only exclusive append creation below the
exact packaging-selected root and reject symlinks, replacement, and sequence
rollback.
