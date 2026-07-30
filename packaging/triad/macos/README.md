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
domain. Broker must require its authenticated sentinel before serving
ceremonies.

## Filesystem and network boundaries

The installer renders the root-owned release, edge manifest, account/group
record, LaunchDaemon definitions, session LaunchAgent, and packet-filter
anchor. Broker and Signer state/checkpoint roots remain owned by their
respective service UIDs and mode `0700`.

The packet-filter template denies new Broker IP flows and all Signer TCP/UDP
flows by numeric effective UID. Production activation is prohibited until the
disposable macOS W0 lane proves IPv4/IPv6, TCP/UDP, loopback, accepted Broker
responses, anchor drift, Fast User Switching, and removal behavior. Local
Signer is the only initial backend.

Static template and staged-root tests are conformance inputs, not proof of an
operating-system boundary. Tests that create accounts, load LaunchDaemons,
change `pf`, or exercise multiple GUI users run only on disposable macOS VMs.
