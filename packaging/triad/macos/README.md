# Bloom triad macOS Unix-principal packaging source

This directory implements the root-requiring Unix-principal profile in
`docs/specs/2026-07-29-macos-unix-principal-isolation.md`. It is source input
for the signed installer and is never installed directly from a checkout.

The rootless code-identity architecture remains documented as a future target
in `docs/specs/2026-07-30-macos-rootless-code-identity-isolation.md`. Nothing
in this directory may emit its `macos-rootless-code-identity` platform claim or
substitute App Groups, same-UID LaunchAgents, or Keychain groups for the Unix
principal boundaries.

## Service topology

For each enrolled login UID, the installer renders two system-domain
LaunchDaemons:

- `com.bloom.broker.LOGIN_UID`, running as `bloom-broker-LOGIN_UID`;
- `com.bloom.signer.LOGIN_UID`, running as `bloom-signer-LOGIN_UID`.

The daemon templates use numeric `SockPathOwner` and `SockPathGroup` values for
each launchd-owned Unix socket. This permits the Broker data, Broker-Signer,
and revocation edges to use different groups without making either service
group transitive. Socket mode is decimal `432`, equivalent to octal `0660`.

Broker owns the canonical ceremony listener by direct exclusive bind to
`127.0.0.1:18734`. The LaunchDaemon does not declare or pre-bind that TCP
socket. A conflict is fatal, reported, retried by failure-only `KeepAlive`, and
never selects a fallback address or port.

The global `com.bloom.session` LaunchAgent invokes only Machine's
`--session-sentinel` mode. It exits successfully for an unenrolled login,
keeps no custody or signing authority, and is destroyed with its GUI login
domain. It owns `session/session.sock` as the login UID and authenticates a
separately pinned `bloom-session` identity. The socket reuses the already
declared revoke group, whose membership contains the login, Broker, and
Signer, while mutual application-key authentication distinguishes the two
service channels. Broker authenticates before binding the canonical ceremony
listener; Signer authenticates before accepting RPC. Both drain and exit
successfully on disconnect.

## Filesystem and network boundaries

The installer renders the root-owned release, edge manifest, account/group
record, LaunchDaemon definitions, session LaunchAgent, and packet-filter
anchor. Broker and Signer state/checkpoint roots remain owned by their
respective service UIDs and mode `0700`.

Version upgrades are global because every enrollment executes through the one
root-owned `current` link. The installer first publishes a complete immutable
release directory, then writes a root-only transaction containing exact
backups and staged Broker/Signer configurations, LaunchDaemon definitions,
and packet-filter anchors for every enrollment, plus the global session
LaunchAgent. It stops all loaded instances, swaps the complete set, repoints
`current`, reloads the root packet-filter ruleset, and restores only the jobs
that were loaded beforehand.
Machine's private installer health mode asks Broker for the existing
`broker.readiness`; Broker in turn requires the existing `signer.readiness` to
report `ready` on the same build. A failed check restores the complete old set.
An installer invocation that finds a non-committed transaction performs that
same rollback before doing new work.

Production enrollment invokes the installed Machine binary's root-only
enrollment-material mode against the signed public templates in `config/`.
Five application identities and the Broker/Signer signing authorities are
fresh per login; only their public cross-pins enter the root-owned manifest.
The temporary root-only generation directory is removed on success or error.

The packet-filter template denies new Broker IP flows and all Signer TCP/UDP
flows by numeric effective UID. A root/wheel one-shot monitor is launched once
per second with no socket, RPC, custody, or signing surface. It verifies the
loaded per-UID anchors and atomically publishes short-lived root-owned status
records. Broker and Signer require the exact login UID, release digest,
ownership, mode, availability bit, and freshness before readiness or any
signing/custody/policy mutation; revocation and public status remain
available. Production activation is prohibited until the disposable macOS W0
lane proves IPv4/IPv6, TCP/UDP, loopback, accepted Broker responses, anchor
drift, Fast User Switching, and removal behavior. Local Signer is the only
initial backend.

Static template and staged-root tests are conformance inputs, not proof of an
operating-system boundary. Tests that create accounts, load LaunchDaemons,
change `pf`, or exercise multiple GUI users run only on disposable macOS VMs.
The guarded harness and its current coverage are documented under `w0/`.
