# Bloom triad release package

`compatibility-v1.toml` is the closed v1 service matrix. The first release
supports only the listed current/current combination; every adjacent or
downgrade combination fails closed.

`build-bundle.sh` accepts three already-built production binaries and a
reviewed Ed25519 release key. It verifies semantic versions, scans every
staged and generated bundle file for release-blocking markers, records all
three Git revisions, embeds both platform installers, normalizes metadata, and
emits a deterministic archive with checksum, signature, and public key.

`verify-bundle.sh` verifies the detached signature and both the outer and
internal checksums before accepting the compatibility matrix or installers.
Production verification accepts Linux ELF bundles only. The
`test-unclaimed` marker requires the explicit
`BLOOM_ALLOW_TEST_UNCLAIMED=true` override at build, verification, and install;
macOS cannot be asserted by these scripts.

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

Instance configuration and identities are site-specific security inputs and
are deliberately not reusable release credentials. They must be placed in the
following `config/` layout beside the extracted binaries before installation:
`edge-manifest.json`, `broker.json`, `signer.json`,
`broker-identity.json`, `signer-identity.json`, and (on Linux)
`nts-servers.conf`. The last file contains at least two distinct reviewed NTS
host names, one per line. AWS credentials and `aws-kms-ip-allow.conf` are an
optional paired site overlay.

The installers stop an existing instance before replacement, atomically
replace each file, and reactivate only after the complete set is present. An
interrupted upgrade is unavailable rather than mixed-version. They also
support service-config rotation and confirmation-bound per-login uninstall.
The Linux AWS KMS profile requires credentials and a non-wildcard reviewed
CIDR allowlist together; reinstall without that pair removes any prior
instance credential and egress drop-in.

The macOS templates remain conformance inputs, not a production platform
claim. A release may claim macOS only after the three binaries are signed with
the rendered App Sandbox entitlements and the disposable launchd lane proves
the effective boundaries. Raw Cargo binaries do not satisfy that gate.
