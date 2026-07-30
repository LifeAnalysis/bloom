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

The lane currently proves account/group shape, non-transitive membership,
root/service filesystem ownership, negative private-state reads, system-domain
LaunchDaemon registration, numeric launchd socket ownership, and loaded
UID-scoped `pf` rules. It also requires the authenticated session socket to
appear with the login UID and revoke group, then verifies the canonical
listener's Broker marker. It removes the live anchor, waits for the root-owned
containment attestation to turn unavailable, proves authenticated triad health
fails, and then restores and re-verifies the anchor. It also constructs a
durable interrupted-enrollment intent plus its exact partial Directory Service
record and proves the next installer invocation removes both without adopting
the record. When the optional bundles are supplied, it also proves a
complete-version upgrade, activation-failure rollback, `SIGKILL` during the
activating phase, stale PID-lock reclamation, journal recovery, and restoration
of the exact prior healthy digest. Foreign/cross-login listener conflict,
actual logout handoff, network attempts, and hostile session authentication
remain required before the W0 claim can graduate.
