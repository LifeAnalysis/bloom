# Disposable macOS W0 lane

`run-disposable.sh PAYLOAD UID USER [UPGRADE_PAYLOAD
[FAILING_UPGRADE_PAYLOAD]]` is destructive integration testing for the
`macos-unix-principals-w0` bundle claim. It creates Directory Service users and
groups, installs system LaunchDaemons, modifies the dedicated Bloom block in
`/etc/pf.conf`, and removes them afterward.

It refuses to run unless all of the following hold:

- the effective UID is root;
- the host is Darwin;
- `BLOOM_RUN_MACOS_UNIX_W0=true`;
- `/private/var/db/bloom-w0-disposable-host` is a regular root-created file
  containing exactly `bloom-macos-unix-w0-disposable-v1`;
- the selected login has an active GUI launchd domain;
- no target Bloom account or group already exists;
- the payload claim is `macos-unix-principals-w0`.

The marker is deliberately not created by this repository. Disposable VM
provisioning owns it. Never create it on a developer workstation.

The manually dispatched `macOS Unix-principal W0` workflow is the repository's
disposable-host provisioner. It runs only on a fresh GitHub-hosted macOS VM,
first proves that the runner login has a GUI launchd domain, builds all three
public repositories from the selected refs, creates a non-production W0
bundle, installs an ephemeral root-owned release pin and host marker, runs this
harness, and removes those markers in an unconditional cleanup step. The
workflow does not produce or advertise a production platform claim.

The lane currently proves account/group shape, non-transitive membership,
root/service filesystem ownership, negative private-state reads, system-domain
LaunchDaemon registration, numeric launchd socket ownership, and loaded
UID-scoped `pf` rules. It also requires the authenticated session socket to
appear with the login UID and revoke group, then verifies the canonical
listener's Broker marker. It pre-binds the canonical port with a foreign
process, verifies Broker's specific fatal/no-fallback diagnostic and Machine
failure, proves Broker opened no fallback listener, then verifies failure-only
KeepAlive acquires the port after it is released. It removes the live anchor,
waits for the root-owned containment attestation to turn unavailable, proves
authenticated triad health fails, and then restores and re-verifies the
anchor. Real service-UID probes prove Signer cannot emit IPv4 or IPv6 loopback
TCP/UDP, and that neither Broker nor Signer can create non-loopback IPv4
TCP/UDP flows; authenticated Broker responses remain covered by the triad
health check. It also constructs a durable interrupted-enrollment intent plus
its exact partial Directory Service record and proves the next installer
invocation removes both without adopting the record. When the optional bundles
are supplied, it also proves a complete-version upgrade, activation-failure
rollback, `SIGKILL` during the activating phase, stale PID-lock reclamation,
journal recovery, and restoration of the exact prior healthy digest.
It also rotates the complete transport-identity/edge-manifest set, verifies
that service configs are unchanged, and requires authenticated health with the
new cross-pins. An unauthorized connection from the login UID must be rejected
by the session sentinel without disrupting authenticated health. A guarded
session-domain bootout/rebootstrap cycle proves Broker and Signer drain, the
ceremony listener closes, and socket activation restores authenticated health
without reinstalling or manually restarting either service.

Cross-login listener conflict and actual Fast User Switching/logout on a
two-login disposable VM remain required before the W0 claim can graduate.
