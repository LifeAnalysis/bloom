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

Each daemon explicitly binds its Unix sockets inside endpoint directories
owned by that service UID. The directories use distinct edge groups and mode
`0710`; a service validates this metadata before publishing a `0660` socket.
Broker and Signer have separate revocation subdirectories. This construction
is required because a launchd-created Unix socket reports launchd's UID to the
connecting peer on macOS, which cannot satisfy the protocol's mutual kernel
peer-UID check. It does not fall back from failed launchd activation or create
endpoints outside the signed profile.

Broker owns the canonical ceremony listener by direct exclusive bind to
`127.0.0.1:18734`. The LaunchDaemon does not declare or pre-bind that TCP
socket. A conflict is fatal, reported, retried by failure-only `KeepAlive`, and
never selects a fallback address or port. Before exiting, Broker atomically
writes a Broker-owned, Machine-readable `broker-startup.json`. Machine accepts
only its exact owner, group, mode, schema, address, incident, and message, so a
bind failure is reported promptly as either another Bloom login or a foreign
or unverifiable listener. A successful retry removes the stale diagnostic.

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
`current`, and reloads the root packet-filter ruleset. Because concurrent
Brokers cannot both own the canonical listener, activation restores session
and Signer jobs first, then bootstraps and health-checks each recorded Broker
whose login-session job was active, exclusively in turn. Logged-out
enrollments have no authenticated session to test; their root-owned staged
files and release bindings still participate in the same atomic transaction.
After each active Broker is stopped, the installer restores the complete prior
loaded-job set. Rollback validates the old release the same way.
Machine's private installer health mode asks Broker for the existing
`broker.readiness`; Broker in turn requires the existing `signer.readiness` to
report `ready` on the same build. A failed check restores the complete old set.
An installer invocation that finds a non-committed transaction performs that
same rollback before doing new work.

Live Broker and Signer config rotation is separately root-journaled. The
installer first validates the caller's replacement and then validates the
root-staged copy again, rejecting changes to release identity, containment,
state paths, cross-pinned keys, or service-principal identity. It records the
previous config and exact loaded-job set, stops both services, atomically swaps
the one service-owned config, restores only those jobs, and requires the
authenticated triad health check when Broker was active. Failure or a later
invocation after interruption restores the byte-identical prior config and
job set. The disposable W0 lane exercises valid rotation, immutable-field
rejection, and SIGKILL recovery.

`rotate-identities` rotates only the five application identities used on the
authenticated Unix edges (Machine, Broker, Signer, revoke client, and session)
and their root-owned edge manifest. The installed, digest-bound Machine binary
generates the replacement set from the OS CSPRNG; the root installer journals
the complete old/new sets, stops the session agent and both services, swaps all
cross-pins while they are unavailable, then restores them and requires
authenticated health. It deliberately does not rotate Broker or Signer
custody/signing authorities embedded in service config, whose persisted-key
rollover is a separate semantic operation.

Permanent per-login uninstall is also a root-journaled operation. The exact
verified enrollment record is copied into a durable transaction before the
public enrollment state becomes `uninstalling`. Teardown is idempotent: a
later installer invocation resumes stopping integration, removing only the
recorded login's files, deleting Directory Service records only when their
numeric IDs still match, and finally removing global integration when no
enrollment remains. The disposable W0 lane kills an uninstall after its
journal becomes durable and verifies forward recovery.

The distinct `retain-bloom-login-LOGIN_UID` confirmation removes the jobs,
packet-filter integration, and public enrollment while preserving the exact
service accounts, private configuration, and service-owned custody state. A
root-only retained record carries the original numeric identities and release
digest. Reinstalling that exact signed release verifies the retained
filesystem and Directory Service boundaries, publishes only an `activating`
record, and removes the retained record only after authenticated Broker and
Signer health succeeds. Interrupted or failed restoration returns to the
retained, unavailable state. When other enrollments are active, restoration
also requires that their one global release already matches the retained
release; the installer never downgrades or mixes the active set to satisfy a
restore. `delete-bloom-login-LOGIN_UID` remains the separate irreversible path
and can permanently delete an already-retained enrollment.

Production enrollment invokes the installed Machine binary's root-only
enrollment-material mode against the signed public templates in `config/`.
Five application identities and the Broker/Signer signing authorities are
fresh per login; only their public cross-pins enter the root-owned manifest.
The temporary root-only generation directory is removed on success or error.
Fresh enrollment is journaled before the first Directory Service mutation.
Each record intent is durable before creation, so an interrupted installer can
remove only names it first proved absent. The root-owned enrollment record is
published as `activating`; only the session sentinel, PF monitor, and private
installer health probe accept that state. Ordinary Machine discovery requires
`active`, which is atomically published only after authenticated Broker and
Signer health succeeds. A later installer invocation resumes a health-passed
transaction or rolls an incomplete one back exactly.

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
