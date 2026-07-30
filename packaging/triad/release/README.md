# Bloom triad release package

`compatibility-v1.toml` is the closed v1 service matrix. The first release
supports only the listed current/current combination; every adjacent or
downgrade combination fails closed.

`build-bundle.sh` accepts three already-built production binaries and a
reviewed Ed25519 release key. It verifies semantic versions, scans every
staged and generated bundle file for release-blocking markers, records all
three Git revisions, embeds both platform installers, signs the internal
payload manifest for post-elevation verification, normalizes metadata, and
emits a deterministic archive with checksum, signature, and public key.

`verify-bundle.sh` verifies the detached signature and both the outer and
internal checksums before accepting the compatibility matrix or installers.
Production verification currently accepts Linux ELF bundles. The
non-production `macos-unix-principals-w0` claim accepts Mach-O binaries only
in its explicitly enabled disposable Darwin lane. The `test-unclaimed` marker requires the explicit
`BLOOM_ALLOW_TEST_UNCLAIMED=true` override at build, verification, and install;
neither test claim can be advertised as production.

Production `macos-unix-principals` bundles are accepted only on Darwin and
only with a signed `bloom.macos-unix-conformance.1` report. The builder
requires an out-of-band SHA-256 pin for the conformance public key; the report
must bind the canonical release-subject digest, all three source revisions,
MUI-01 through MUI-12, installed AC-01 through AC-35, negative-access tests,
and the two-login lifecycle suite. The subject digest covers every packaged
binary, installer, compatibility input, plist, ACL, and packet-filter source
while excluding only the platform-claim value and the release/conformance
signature envelope. This avoids a self-referential archive digest while still
invalidating evidence after any security-relevant packaged input changes.
The final archive and internal release signature then bind the report and its
public key into the distributed artifact.

`macos-conformance-subject.sh` computes the canonical subject.
`sign-macos-conformance-report.sh` refuses to sign until each required
criterion has a regular `CRITERION.pass` evidence file containing that exact
subject digest; it never overwrites an existing report. The release operator
reviews those suite outputs and signs with the separately controlled
conformance key. `verify-macos-conformance.sh` verifies that signature,
criterion completeness, source revisions, subject binding, and—during
production assembly—the out-of-band conformance-key fingerprint.

For one candidate payload `C`, the disposable evidence matrix is:

- run the single-login W0 with `C` as its primary payload to produce MUI-01,
  MUI-02, MUI-03, MUI-04, MUI-07, MUI-08, MUI-10, MUI-11, MUI-12,
  `installed_ac_01_35`, and `negative_access`;
- on a two-GUI-login disposable VM, run the two-login W0 with an older valid
  payload as the baseline, `C` as `UPGRADE_PAYLOAD`, and a distinct
  deliberately activation-failing payload as `FAILING_UPGRADE_PAYLOAD` to
  produce MUI-05, MUI-06, MUI-09, and `two_login_lifecycle`;
- merge only `.pass` files whose contents equal `C`'s canonical subject
  digest, then review and sign them with
  `sign-macos-conformance-report.sh`.

The signer refuses a mixture of evidence from different candidates.

`triad-release-gate.sh` rejects modified or untracked release inputs, runs
locked fmt, clippy, and tests in all three sibling workspaces, builds release
binaries, assembles the bundle twice, verifies both, requires byte-identical
archives, matches the signed source revisions back to the three clean
workspaces, executes each extracted production binary, then reruns all three
workspace suites with the verified bundle bound as acceptance input.
`--test-signing-key` is CI-only; production invocation must set
`TRIAD_RELEASE_SIGNING_KEY`.

Fault-injection acceptance tests remain separately linked test executables:
putting fault hooks into the production services would violate AC-20. Their
post-extraction rerun is bound to the exact clean source revisions recorded in
the signed bundle; process/artifact acceptance additionally executes and
inspects the extracted production binaries.

Linux instance configuration fixtures are site-specific security inputs and
are deliberately not reusable release credentials. Test-only staged
installer fixtures use the following `config/` layout beside the extracted binaries:
`edge-manifest.json`, `broker.json`, `signer.json`,
`machine-identity.json`, `broker-identity.json`, `signer-identity.json`,
`revoke-identity.json`, `session-identity.json`, `installer-identity.json`,
and `provenance-catalog.json`. The macOS W0 bundle deliberately contains none
of these private files: its guarded live installer uses the same fresh
root-owned identity-generation path as the production Unix-principal claim.
On Linux,
`nts-servers.conf`. The last file contains at least two distinct reviewed NTS
host names, one per line. AWS credentials and `aws-kms-ip-allow.conf` are an
optional paired site overlay.

Production macOS enrollment does not accept that private fixture layout. Its
installed Machine binary generates fresh per-login Machine, Broker, Signer,
revoke-client, session, installer, audit, review, ceremony, and revocation
keys from the OS CSPRNG in a root-owned empty staging directory. It renders
only signed public templates from `installer/macos/config`, cross-pins the
public keys, signs the provenance catalog locally, atomically installs the
private outputs under their final principals, and removes the staging
directory. Bundle build and verification reject concrete private seeds and
identity-shaped JSON for a production macOS claim.

The installers stop an existing instance before replacement, atomically
replace each file, and reactivate only after the complete set is present. An
interrupted upgrade is unavailable rather than mixed-version. They also
support service-config rotation and confirmation-bound per-login uninstall.
Live macOS config rotation uses a root-only transaction, validates both the
input and its root-staged copy against immutable identity and containment
fields, and rolls back the prior config and loaded-job set on failed health or
interruption. Transport identity rotation atomically replaces the complete
five-identity/edge-manifest set generated by the installed Machine binary,
without changing persisted custody/signing authorities. Permanent macOS
uninstall records exact deletion intent before
unpublishing Machine access and resumes idempotently after interruption; its
confirmation token is deliberately distinct from retain-custody uninstall.
The retain token removes runtime integration while preserving the exact
service principals, private configuration, and custody state; restoration
requires the same signed release and publishes Machine access only after
authenticated health succeeds.
The Linux AWS KMS profile requires credentials and a non-wildcard reviewed
CIDR allowlist together; reinstall without that pair removes any prior
instance credential and egress drop-in.

The root-requiring macOS Unix-principal templates remain conformance inputs,
not a production platform claim. A release may claim
`macos-unix-principals` only after the disposable W0 lane proves the effective
UID/group, filesystem, launchd, listener, network, lifecycle, and rollback
boundaries and a digest-bound conformance report is included. The rootless
code-identity architecture remains a separate future profile.
