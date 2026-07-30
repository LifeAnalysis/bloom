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

`run-two-login.sh PAYLOAD UID_A USER_A UID_B USER_B [UPGRADE_PAYLOAD
[FAILING_UPGRADE_PAYLOAD]]` is the separate cross-login lifecycle lane. It has
the same root, Darwin, payload-claim, and external disposable-marker guards,
additionally requires two distinct active GUI launchd domains, and refuses
existing Bloom state or principals for either login. It enrolls both UIDs,
proves the second Broker dies fatally with the specific Machine-visible
cross-login diagnostic and no fallback listener, terminates the owning GUI
domain, then proves failure-only KeepAlive transfers the canonical listener
before another Machine request. When both optional bundles are supplied, it
also proves a successful complete-version upgrade is published to both
enrollments and a deliberately failing subsequent upgrade rolls both back to
the same healthy version. Upgrade validation gives each Broker exclusive use
of the canonical listener in turn before restoring the original loaded-job
set. If
`BLOOM_MACOS_W0_EVIDENCE_DIR` names an existing absolute directory, a
successful run writes digest-bound `mui_05.pass`, `mui_06.pass`, and
`two_login_lifecycle.pass` evidence for the tested payload. It writes
`mui_09.pass` only when both the successful and failing two-login upgrade cases
pass.

`run-installed-acceptance.sh PAYLOAD UID USER MAIN_ROOT BROKER_ROOT SIGNER_ROOT
EVIDENCE_DIR` is invoked by the single-login harness while the installed
services are healthy. It verifies the active enrollment, byte-identical
installed binaries, exact running service UIDs and executable paths, clean
source revisions matching `SOURCE_REVISIONS`, and authenticated health. It
then reruns the triad protocol, transport, activation, checkpoint, Machine
client, policy-update, Broker, and Signer acceptance sources while the real
installed services remain active. Fault injection stays confined to test
executables. It also rejects provisioning profiles, Developer-ID authorities,
or Team IDs and executes the production builder without conformance inputs to
prove that claim generation fails. A final process/health recheck precedes
digest-bound `mui_01.pass`, `mui_11.pass`, `installed_ac_01_35.pass`, and
`mui_12.pass` evidence.

The manually dispatched `macOS Unix-principal W0` workflow is the repository's
disposable-host provisioner. It runs only on a fresh GitHub-hosted macOS VM,
first proves that the runner login has a GUI launchd domain, builds all three
public repositories from the selected refs, creates a non-production W0
bundle, installs an ephemeral root-owned release pin and host marker, runs this
harness including the installed acceptance rerun, uploads only digest-bound
criterion files after success, and removes those markers in an unconditional
cleanup step. The workflow does not produce or advertise a production
platform claim.

The workflow is also reusable. Until its standalone filename reaches the
default branch, manually dispatching the existing `CI` workflow at
`triad-architecture` calls the same disposable W0 job from that exact commit.
Pull requests and ordinary pushes never enter the destructive lane.

The lane currently proves account/group shape, non-transitive membership,
root/service filesystem ownership, explicit checkpoint/config/database
negative reads, immutable release/plist/manifest/packet-filter replacement
denial for every product principal, live installer rejection of mode, owner,
symlink, and hard-link manifest substitutions, process task/sample denial, system-domain
LaunchDaemon registration, numeric launchd socket ownership, unrelated-UID
endpoint denial, Machine denial on the Broker-to-Signer edge, and loaded
UID-scoped `pf` rules. Authenticated triad health also proves both service UIDs
can sample the pinned `macos-managed-timed` source. The lane requires the authenticated session socket to
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

The cross-login harness is intentionally not run on the ordinary
single-login GitHub-hosted lane. A recorded successful run on a disposable
two-login VM remains required before the W0 claim can graduate.

`macos-two-login-w0.yml` defines that run for an ephemeral self-hosted runner
labelled `bloom-two-login-disposable`. The runner itself must be outside both
test UIDs, both supplied users must already have genuine active GUI domains,
and the VM must be destroyed or reverted after the job. The workflow builds a
digest-distinct valid baseline variant, the candidate, and a candidate-derived
activation-failing bundle from the same clean source revisions under one
ephemeral release key. It then invokes the two-login harness and uploads only
candidate-subject evidence. It never creates the two GUI users or treats a
synthetic launchd domain as a login.
