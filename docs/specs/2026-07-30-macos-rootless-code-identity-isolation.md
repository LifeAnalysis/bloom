# macOS rootless code-identity isolation profile

**Status:** Candidate architecture; production claim prohibited until the
mandatory disposable-host gates pass

**Applies to:** Bloom triad architecture, local macOS placement

**Normative base:** `2026-07-23-triad-process-architecture.md`

**Minimum operating system:** macOS 15

**Installation privilege:** interactive login user only; no root or
administrator authorization

## 1. Purpose

This profile defines a rootless alternative to
`2026-07-29-macos-unix-principal-isolation.md`.

It aims to retain the custody and authorization properties of the Unix-UID
profile against a compromised Machine without creating local users, installing
LaunchDaemons, writing system directories, or changing the system packet
filter.

The profile relies on current macOS security mechanisms:

- Developer ID code identity and notarization;
- macOS 15 app-data and App Group container protection;
- service-private Data Protection Keychain access groups;
- App Sandbox for Broker and Signer;
- Hardened Runtime and launch environment constraints;
- rootless `SMAppService` LaunchAgents;
- application-key authenticated RPC in addition to container membership.

Apple documents that macOS 15 protects App Group containers even for
non-sandboxed apps, limiting access to apps outside the group, and that app
container association is code-signature based. Apple also documents that an
explicit user decision can grant another app access. The design therefore does
not place plaintext custody or authentication secrets in a container and does
not treat container denial as an unoverrideable Unix permission.

References:

- <https://developer.apple.com/documentation/xcode/protecting-local-app-data-using-containers>
- <https://developer.apple.com/documentation/security/accessing-files-from-the-macos-app-sandbox>
- <https://developer.apple.com/documentation/servicemanagement/smappservice>
- <https://developer.apple.com/documentation/security/defining-launch-environment-and-library-constraints>

## 2. Scope and explicit prerequisites

A conforming rootless release requires:

1. Apple Developer Program membership and a stable Team ID.
2. Developer ID Application signatures on every executable.
3. Hardened Runtime, secure timestamps, and notarization.
4. Stable, distinct code-signing identifiers for Machine, Broker, and Signer.
5. Registered App Groups for the container topology and Developer ID
   provisioning profiles that authorize the exact restricted Keychain group
   set for each service executable.
6. macOS 15 or later.
7. User approval for Bloom background items in System Settings.
8. A notarized application bundle installed at a user-writable location such
   as `~/Applications/Bloom.app`.

It does not require:

- a privileged installer;
- a Developer ID Installer certificate or `.pkg`;
- local service accounts or groups;
- LaunchDaemons;
- App Store distribution;
- a Network Extension or system packet-filter rule.

The distributed object is either a stapled, notarized DMG or a notarized ZIP
containing a stapled app bundle. A ZIP is not itself a stapling target. A
rootless CLI shim is a symlink in `~/.local/bin` pointing to the signed Machine
executable inside the app bundle. The shim contains no code or trust decision.

## 3. Threat model adjustment

The protected adversary remains a fully compromised Machine process. Machine
may invoke every API and system capability granted by its signed executable,
delete user-owned files it can reach, terminate same-UID processes, unregister
background items, bind local ports, and deceive the user.

The profile must retain:

- Machine cannot recover Broker or Signer authentication keys.
- Machine cannot recover local wallet keys, WKEK material, PRF output,
  recovery secrets, or backend credentials.
- Machine cannot authenticate as Broker.
- Machine cannot obtain a signing operation outside Broker and Signer policy,
  exact-request, rate, revocation, and replay enforcement.
- Machine cannot silently substitute executable code for Broker or Signer.
- State tampering, rollback, or deletion fails closed.
- Listener conflict is fatal and never chooses another origin.

The profile accepts two availability differences from distinct Unix UIDs:

- the login user can disable, unregister, signal, or delete the same-user
  background services;
- the login user can explicitly grant an app access to another protected
  container when macOS presents a consent prompt.

Neither action grants custody authority. Disabling services makes signing
unavailable. Container access reveals only encrypted/authenticated state;
service-private Keychain material remains inaccessible to code outside the
corresponding access group.

The UI must describe a request by Machine to access Broker or Signer containers
as a security-boundary violation. Bloom never instructs the user to approve
such a request.

## 4. Code identities and bundle structure

The bundle contains exactly three production executables:

| Role | Signing identifier | Packaging | Sandbox |
|---|---|---|---|
| Machine | `com.bloom.machine` | Main app executable | No |
| Broker | `com.bloom.broker` | Embedded app-like helper bundle | Yes |
| Signer | `com.bloom.signer` | Embedded app-like helper bundle | Yes |

```text
Bloom.app/
  Contents/MacOS/bloom
  Contents/Library/HelperTools/BloomBroker.app/
    Contents/MacOS/bloom-broker
    Contents/embedded.provisionprofile
  Contents/Library/HelperTools/BloomSigner.app/
    Contents/MacOS/bloom-signer
    Contents/embedded.provisionprofile
  Contents/Library/LaunchAgents/
    com.bloom.broker.plist
    com.bloom.signer.plist
```

Broker and Signer are app-like bundles because macOS standalone executables
have nowhere to embed the provisioning profile required to authorize a
restricted `keychain-access-groups` entitlement. The LaunchAgent
`BundleProgram` paths name the signed executable inside each helper bundle.

Machine remains non-sandboxed because Bloom CLI, VFS, mount, package, and
user-file workflows are incompatible with the App Sandbox restriction on
running tools and accessing arbitrary user paths. Apple explicitly documents
that a sandboxed app cannot run programs outside its bundle, container, or App
Group containers merely by receiving user-selected file access.

Broker and Signer use:

- Developer ID validation category;
- the exact Bloom Team ID;
- exact signing identifiers;
- Hardened Runtime;
- no `get-task-allow`;
- library validation;
- no JIT, unsigned executable memory, debugger, DYLD environment, or library
  validation exceptions.

Security configuration is not accepted from arguments, environment variables,
the current directory, or unsigned files. Each service derives its outer app
bundle, verifies the release tuple and complete nested-code seal, resolves
container URLs through the system APIs, and uses compiled endpoint/group
identifiers. A copied genuine helper therefore either runs the same closed
service under the same release floor or refuses; it does not become a
configurable secret-reading utility.

Signer has no network entitlement. Broker has
`com.apple.security.network.server` only and no network-client entitlement.

Every production launch is constrained by:

- an embedded self constraint for Team ID, signing identifier, Developer ID
  validation category, and required entitlements;
- an embedded parent constraint requiring launchd for service execution;
- a matching `SpawnConstraint` in the signed LaunchAgent plist.

The kernel refuses a process whose launch constraints do not match. The
release gate independently extracts and compares the embedded constraints,
entitlements, Team ID, signing identifier, and Code Directory hashes.

## 5. App Groups and Keychain groups

Every identifier is prefixed by the Bloom Team ID. App Groups define protected
container and IPC paths:

| App Group | Members | Permitted contents |
|---|---|---|
| `TEAM.bloom.machine-broker` | Machine, Broker | Machine-to-Broker socket and public projections |
| `TEAM.bloom.broker-signer` | Broker, Signer | Broker-to-Signer socket and public receipts |
| `TEAM.bloom.revoke` | Machine, Broker, Signer | control sockets and public revocation state |
| `TEAM.bloom.broker-private` | Broker only | Broker encrypted state |
| `TEAM.bloom.signer-private` | Signer only | Signer encrypted state |

On macOS, `com.apple.security.application-groups` is an unrestricted
entitlement: its claims are sealed into the notarized code signature but are
not authorized by a provisioning profile. The release gate therefore rejects
an extra or missing App Group, any other Bloom Team-signed executable that
claims these groups, or any membership that creates a Machine-to-Signer edge.
Release-signing policy treats issuing another Team-signed binary with one of
these groups as a security-boundary change.

App Group containers hold no reusable plaintext secret. Groups used for IPC
contain sockets and bounded public projections only.

The actual secret and state-anchor boundary uses the restricted
`keychain-access-groups` entitlement. Each service helper embeds a Developer ID
provisioning profile that authorizes exactly the applicable groups:

| Keychain access group | Members |
|---|---|
| `TEAM.com.bloom.broker.keys` | Broker only |
| `TEAM.com.bloom.signer.keys` | Signer only |
| `TEAM.com.bloom.broker-signer.floor` | Broker, Signer |

The release gate rejects a signature/profile entitlement disagreement,
expired or wrong-Team profile, extra group, missing group, or a profile whose
application identifier does not exactly match its helper. Machine has none of
these restricted groups.

### Broker-private Keychain

- Broker application-identity seed;
- Broker request-signing key;
- Broker audit-signing key;
- review-manifest signing key;
- Broker state-encryption and state-authentication keys.

### Signer-private Keychain

- Signer application-identity seed;
- ceremony and revocation signing keys;
- local-backend state keys;
- WKEK/recovery protection material permitted by the custody format;
- Signer audit-signing key;
- Signer state-encryption and state-authentication keys.

### Broker-Signer shared Keychain

- signed monotonic minimum release sequence;
- cross-service enrollment generation;
- no signing, custody, recovery, or backend key.

Machine is not a member of any access group containing Broker or Signer secret
material. Keychain queries set `kSecUseDataProtectionKeychain` and the exact
access group, set `kSecAttrSynchronizable` false, and use a device-only
`kSecAttrAccessibleWhenUnlockedThisDeviceOnly` accessibility class. An
implicit/default Keychain search is forbidden. If W0 shows that exact class is
incompatible with service restart after a normal interactive login, rootless
release is blocked; it is not silently weakened. Changing the class is a
reviewed security-profile change.

Apple documents that an item belongs to exactly one access group and that a
query for a group outside the caller's entitlements fails. The gate tests this
with the installed code identities:

<https://developer.apple.com/documentation/security/sharing-access-to-keychain-items-among-a-collection-of-apps>

Apple documents that `keychain-access-groups` is restricted and must be
authorized by a provisioning profile, while
`com.apple.security.application-groups` is unrestricted on macOS:

<https://developer.apple.com/documentation/technotes/tn3125-inside-code-signing-provisioning-profiles>

Developer ID provisioning profiles are evaluated at every app launch. Release
operations renew and ship replacement profiles well before expiry. Expiry,
revocation, or profile-validation failure makes the affected service
unavailable; it never falls back to an unprofiled or less-entitled executable.

## 6. Persistent state

Broker and Signer private state is stored only in their private protected App
Group containers. Each logical record is encrypted and authenticated with a
service-private Keychain key.

At minimum, encryption/authentication covers:

- database pages or complete atomic snapshots;
- application identities and cross-pins not already in Keychain;
- policies and approval records;
- journals, ledger state, replay state, and trusted-time state;
- local-backend state and custody metadata;
- audit checkpoint records.

Unencrypted filenames, lengths, and bounded public projection data are not
confidential. Any decryption, authentication, schema, sequence, ownership,
replacement, or rollback failure enters the existing fail-closed fault state.

Each service also keeps a compact state anchor in its private Data Protection
Keychain group:

```text
(service_id, enrollment_generation, committed_sequence, committed_root,
 pending_sequence?, pending_root?, format_version)
```

Persistent mutations use immutable, content-addressed encrypted snapshots and
the following two-phase protocol:

1. write and durably sync the next complete snapshot;
2. compare-and-swap the Keychain anchor from the exact committed tuple to an
   exact pending tuple naming that snapshot;
3. durably publish any bounded public projection;
4. compare-and-swap the anchor from that pending tuple to the new committed
   tuple.

On restart, no pending tuple permits only the exact committed snapshot. An
exact pending tuple plus its matching authenticated snapshot is completed
idempotently. A missing snapshot, an unexpected snapshot, a root mismatch, a
sequence gap, or any other combination enters recovery-required fault state.
Garbage collection never removes the committed or pending snapshot. This
protocol, including crashes at every boundary, is part of RUI-01 and the
existing AC-07 suite.

Machine obtaining user-authorized access to a service container can cause
denial of service by deleting ciphertext. It cannot construct accepted state,
recover plaintext, lower monotonic values, or authenticate as the service.
Deleting private Keychain items, whether through explicit user action or
Keychain reset, is also destructive denial of service and enters recovery
rather than creating a fresh identity.

Broker and Signer exchange signed journal heads. Each stores the peer head in
its own private container and commits its own head to its Keychain-protected
state. Deleting or truncating one container is detected against the surviving
peer head or Keychain-protected sequence.

## 7. Enrollment without a root-owned manifest

The Unix profile's root-owned edge manifest cannot be reproduced rootlessly.
Initial trust is instead established by the installed code identities.

On first launch:

1. Machine verifies the complete app bundle, notarization ticket, release
   manifest signature, exact executable inventory, and current compatibility
   matrix.
2. Machine registers both signed LaunchAgents through `SMAppService`.
3. User approval must reach `SMAppService.Status.enabled`.
4. Broker and Signer generate their application keys directly into their
   private Keychain access groups.
5. Machine connects to Broker through the Machine-Broker group.
6. Broker connects to Signer through the Broker-Signer group.
7. Each connection authenticates the normal protocol envelope and the fixed
   expected application key.
8. The services exchange code-identity-bound enrollment contributions and
   cross-signed public keys.
9. Broker and Signer independently persist the same enrollment generation in
   their private authenticated state.
10. Machine receives only a signed public edge projection.

Enrollment is permitted only when both service private stores and private
Keychain groups have no prior identity. Presence on one side and absence on
the other is recovery-required, never fresh enrollment.

Rotation requires the existing application keys, a completed ceremony where
normatively required, and two-phase cross-signing. Machine cannot reset or
replace an enrollment by deleting its own projection.

## 8. RPC and control endpoints

The current authenticated framed protocol remains the domain protocol.
Same-UID peer credentials are recorded but are not treated as service
identity.

Authorization requires all of:

1. socket path in the exact expected protected App Group container;
2. caller access through the fixed signed App Group entitlement;
3. application-key challenge and signed envelope;
4. exact service ID, enrollment generation, boot epoch, method, and nonce;
5. closed per-endpoint method authorization.

Socket layout:

```text
TEAM.bloom.machine-broker/broker.sock
TEAM.bloom.broker-signer/signer.sock
TEAM.bloom.revoke/broker-control.sock
TEAM.bloom.revoke/signer-control.sock
```

Machine is not a member of the Broker-Signer group. If the user explicitly
grants Machine filesystem access to that container, Signer's application-key
authentication still rejects it.

Control sockets accept only the revoke-client application identity and expose
only the closed revocation/status method inventory. A compromised Machine may
revoke and cause availability loss but cannot sign or mutate custody.

As a defense-in-depth W0 candidate, an XPC transport may use
`NSXPCConnection.setCodeSigningRequirement` to enforce the exact peer Team ID
and signing identifier. This API is available from macOS 13. It is not required
for the first implementation if App Group path isolation plus application-key
authentication passes all negative tests. A partial XPC proxy that weakens
method, payload, or receipt binding is prohibited.

## 9. Rootless service activation

Broker and Signer are LaunchAgents embedded in:

```text
Bloom.app/Contents/Library/LaunchAgents/
```

Their executables remain inside the signed app bundle and are referenced using
`BundleProgram`. Machine registers them with `SMAppService`.

Apple documents that:

- `SMAppService` controls helpers inside the app bundle;
- apps using it must be code signed;
- a registered LaunchAgent bootstraps immediately and on later logins, subject
  to user approval;
- changing the executable or plist requires re-registration.

References:

- <https://developer.apple.com/documentation/servicemanagement/smappservice>
- <https://developer.apple.com/documentation/servicemanagement/smappservice/register()>

Both jobs use:

- failure-only KeepAlive;
- bounded restart throttling;
- core dumps disabled;
- private logs containing no secrets;
- no secret-bearing arguments or environment variables;
- exact App Group container URLs resolved at runtime rather than hard-coded
  paths.

If background-item approval is missing or revoked, Machine reports
`SERVICE_APPROVAL_REQUIRED` with instructions to open the Login Items settings.
It never falls back to an embedded service or forks a security service.

Logout destroys the GUI launchd domain and terminates its agents. The next
login restores enabled agents. Fast User Switching creates an independent
per-login agent set, subject to the single canonical listener limitation.

## 10. Canonical ceremony listener

Broker directly binds exactly `127.0.0.1:18734`:

- no address reuse;
- no port reuse;
- no fallback port, IPv6 listener, wildcard listener, or proxy;
- close-on-exec;
- exact post-bind local-address verification.

App Sandbox grants Broker network-server authority only. Signer has no network
authority.

A bind conflict is fatal. Before exit, Broker writes an authenticated bounded
startup diagnostic to the Machine-Broker public projection distinguishing:

- another Bloom login owns the listener;
- a foreign or unverifiable process owns the listener.

The classification is diagnostic, not authorization. On `EADDRINUSE`, Broker
performs a bounded loopback owner-probe challenge. An honest Bloom listener
returns the expected versioned response; no response or any malformed response
is reported as foreign or unverifiable. A hostile listener can imitate the
public probe and mislabel its denial of service, but gains no session, secret,
or signing authority. Every conflict remains fatal regardless of the label.

Failure-only KeepAlive retries. When an owning GUI login ends, its Broker exits
and releases the port; a waiting Broker can acquire it without user action.
There is no fallback and no fairness claim.

The disposable test lane must prove the actual launchd retry behavior with two
different login UIDs. A helper-process simulation is insufficient.

## 11. Network containment

Signer has App Sandbox enabled and no network client or server entitlement.
Negative tests cover IPv4, IPv6, loopback, DNS, TCP, UDP, and Unix-to-network
proxy attempts.

Broker has network-server entitlement only. It may accept the canonical
loopback listener and return responses on accepted connections. It cannot
initiate arbitrary network connections.

The first rootless release supports `LocalSignerBackend` only. AWS KMS is
unsupported because App Sandbox network-client permission is not an
endpoint-specific egress control. Rootless AWS KMS requires a future mechanism
that proves exact endpoint restrictions without a privileged packet filter.

Machine retains its normal network and filesystem authority; Broker and Signer
remain independent enforcement boundaries.

## 12. Process and executable integrity

Hardened Runtime, library validation, absence of `get-task-allow`, and launch
constraints protect Broker and Signer from same-UID code injection and
unapproved launch contexts. The mandatory tests attempt:

- `task_for_pid`;
- debugger attach;
- process sampling with memory access;
- `DYLD_*` injection;
- unsigned library injection;
- direct execution outside launchd;
- copied executable launch;
- changed plist or executable;
- ad-hoc, development, wrong-Team, wrong-identifier, and wrong-entitlement
  signatures.

All must fail before service code handles a request.

The user owns the app bundle and can delete or overwrite it. Deletion is
availability loss. An altered or unsigned replacement fails code-signature,
launch-constraint, and notarization checks.

### 12.1 Signed-version rollback

A prior legitimate Developer-ID-signed bundle would also satisfy Team and
identifier requirements. Every rootless release therefore implements the
release-floor protocol from its first shippable version.

Broker and Signer:

1. verify a Bloom-release-key-signed tuple containing release sequence,
   component versions, protocol range, bundle digest, and expiry policy;
2. store the highest accepted sequence in the Broker-Signer shared Data
   Protection Keychain group;
3. refuse startup below that sequence;
4. refuse any operation while their observed floor values disagree;
5. increase the floor only after both new services pass compatibility and
   state-migration preparation;
6. never expose an RPC that lowers or deletes the floor.

Replacing the app with an older genuine release then produces
`UNSUPPORTED_VERSION` before RPC service. Deleting the app remains denial of
service.

## 13. Rootless install and update

### 13.1 Install

1. User downloads either the stapled, notarized DMG or the notarized ZIP whose
   app bundle carries the stapled ticket.
2. User copies `Bloom.app` to `~/Applications`.
3. First launch verifies Gatekeeper assessment, notarization ticket, nested
   signatures, release signature, exact Team ID and entitlements.
4. Bloom creates `~/.local/bin/bloom` as a symlink if requested.
5. Bloom registers Broker and Signer LaunchAgents using `SMAppService`.
6. User enables the Bloom background items when macOS requests approval.
7. Enrollment from section 7 completes.
8. Machine publishes ready status only after both service health handshakes.

No file is written outside locations writable by the login user and the
system-managed App/Keychain containers.

### 13.2 Update

1. Download to a Machine-private staging directory.
2. Verify the complete new app and signed release-floor tuple.
3. Ask Broker and Signer to prepare state migrations and floor advancement.
4. Drain operations and unregister both `SMAppService` agents.
5. Atomically rename the old and new app bundles within the same filesystem.
6. Re-register both agents, as Apple requires after executable/plist changes.
7. Wait for user approval if macOS returns `requiresApproval`.
8. Both services verify the new bundle digest and commit the shared release
   floor.
9. Complete state migrations and health checks.
10. Delete the retained prior bundle only after success.

Failure before floor commitment restores the old complete bundle. Failure
after floor commitment leaves services unavailable and requires reinstalling
the new-or-later release; it never permits rollback.

### 13.3 Uninstall

Rootless uninstall:

1. confirms the exact user and whether custody state is retained or destroyed;
2. drains and unregisters both agents;
3. removes the CLI symlink and app bundle;
4. optionally deletes public App Group containers;
5. for permanent deletion, asks each service to erase its private Keychain
   items and private container before unregistering;
6. reports retained recovery material and remaining background-item status.

Machine cannot independently erase Signer-private Keychain items. If Signer
cannot run, permanent deletion requires documented user action in Keychain and
container privacy settings; uninstall must not falsely report deletion.

## 14. Security-equivalence matrix

| Property | Unix-principal profile | Rootless profile | Result |
|---|---|---|---|
| Machine cannot read Signer keys | Signer UID files | Signer-private Data Protection Keychain | Equivalent for compromised Machine |
| Machine cannot forge Broker | Broker UID identity | Broker-private Keychain identity | Equivalent |
| Machine cannot open Signer data RPC | Unix group + UID | App Group exclusion + application key | Equivalent after negative tests |
| Machine cannot alter accepted state | owner-only files + signatures | encrypted/MACed containers + Keychain keys | Equivalent integrity; deletion remains DoS |
| Machine cannot inject service process | distinct UID | Hardened Runtime + launch constraints | Similar; requires same-UID attack tests |
| Machine cannot replace accepted executable | root-owned install | Developer ID + spawn constraints + release floor | Equivalent integrity; deletion remains DoS |
| Signer has no network | UID-scoped `pf` | App Sandbox without network entitlement | Equivalent for local backend |
| Independent audit checkpoint | service UID directory | service-private authenticated container + peer head | Similar; user-authorized container access can delete but not forge |
| Service availability against Machine | different UID | same UID, user-controllable agents/signals | Weaker; explicit availability limitation |
| User cannot bypass state boundary without admin | Unix ownership | macOS container consent can grant access | Weaker confidentiality unless state remains encrypted |
| Multi-user RPC separation | UID/group | per-user GUI domain + App Group + app key | Similar; mandatory two-login tests |
| Canonical listener fail-closed | exclusive bind/KeepAlive | exclusive bind/KeepAlive | Equivalent after real launchd test |
| Root compromise resistance | Not claimed | Not applicable/no root component | Neither profile claims root compromise |

The rootless profile may claim "similar custody and authorization containment,"
not "identical Unix isolation." Documentation must prominently disclose the
same-UID availability and explicit-container-consent differences.

### 14.1 Unix-profile release-gate parity

The Unix profile's macOS-specific criteria are dispositioned as follows:

| Unix criterion | Rootless disposition |
|---|---|
| MUI-01, no Apple dependency | Intentionally not preserved; Developer ID is a rootless prerequisite |
| MUI-02, effective-principal separation | Three code identities; Broker/Signer sandbox and process-integrity gates |
| MUI-03, only declared socket edges | RUI-05 App Group and application-key negatives |
| MUI-04, private state/checkpoints | RUI-01/RUI-02 plus Keychain anchors and AC-18 |
| MUI-05, listener conflict behavior | RUI-07 and AC-31 |
| MUI-06, logout and retry lifecycle | RUI-03/RUI-07 |
| MUI-07, packet-filter drift fails closed | Replaced by signed App Sandbox profile; missing/drifted sandbox is fatal in RUI-06 |
| MUI-08, Signer has no IP authority | RUI-06 |
| MUI-09, atomic compatible upgrade | Sections 12.1 and 13.2, RUI-03/RUI-08, and AC-19 |
| MUI-10, stop-before-delete uninstall | Section 13.3 and RUI-02/RUI-08 |
| MUI-11, digest-bound release evidence | Section 16 |
| MUI-12, installed full-suite pass | Section 16 requires AC-01 through AC-35 |

The rootless release report records both this table and the security-equivalence
matrix. A row marked equivalent or similar without its named installed-bundle
test evidence is a release failure.

## 15. Mandatory W0 rootless spike

The spike runs only on disposable macOS 15 and current-major macOS VMs using a
real Developer ID/notarized test release. Ad-hoc signatures are not evidence.

### RUI-01 Container isolation

- Machine cannot read or modify Broker-private or Signer-private containers.
- An unrelated same-UID process cannot access them without a macOS consent
  prompt.
- Denying the prompt preserves denial.
- Granting the prompt exposes only ciphertext/public metadata and cannot access
  private Keychain items.
- Machine never prompts during normal operation.

### RUI-02 Keychain isolation

- each role reads only its declared access groups;
- Machine queries for Broker/Signer groups return missing-entitlement or no
  match;
- Broker cannot read Signer-private items;
- copied, re-signed, ad-hoc, old-Team, and wrong-identifier binaries fail;
- Keychain items are local-only, use
  `kSecAttrAccessibleWhenUnlockedThisDeviceOnly`, and never enter iCloud
  Keychain;
- deletion or Keychain reset causes recovery/availability failure and never
  fresh enrollment or accepted rollback;
- uninstall and update behavior matches sections 13.2 and 13.3.

### RUI-03 Launch integrity

- `SMAppService` registration, approval, logout/login, update re-registration,
  and user-revoked approval behave exactly as specified;
- every wrong executable, entitlement, Team ID, identifier, parent, and spawn
  context fails;
- direct launch of Broker or Signer fails;
- alternate arguments, environment, working directory, and unsigned
  configuration cannot change an endpoint, group, identity, release, backend,
  policy, or state root;
- a copied exact helper/full bundle cannot expose a new method or bypass the
  application-key and release-floor checks;
- expired, revoked, missing, altered, and wrong-helper provisioning profiles
  fail launch without an unprofiled fallback;
- a copied old release fails after floor advancement.

### RUI-04 Same-UID process attacks

- debugger, task-port, memory sampling, signal/crash, DYLD, library injection,
  environment, working-directory, and executable replacement cases run;
- confidentiality and integrity survive;
- permitted signal/unregister/delete cases are recorded as availability-only.

### RUI-05 IPC

- exact three group edges exist and no transitive data endpoint appears;
- every wrong application key, generation, boot epoch, method, and nonce fails;
- Machine remains unable to use Signer data RPC even after user-authorized
  filesystem access to its container;
- an unrelated local UID cannot use another login's sockets, containers,
  application keys, or Keychain items;
- control sockets expose only revocation/status.

### RUI-06 Network

- Signer cannot open IPv4, IPv6, loopback, DNS, TCP, or UDP;
- Broker can serve the canonical listener and cannot initiate network client
  connections;
- absence of a sandbox at runtime is fatal and reported;
- Broker and Signer can read only the approved macOS managed-time status;
  inability to obtain trusted-time evidence fails rate-limited signing closed.

### RUI-07 Listener and multi-user lifecycle

- foreign pre-bind is fatal and reported with no fallback;
- two GUI users produce one owner and one loudly failing waiter;
- owner logout releases the listener;
- waiting KeepAlive acquires it without user action;
- Machine never hangs waiting for a failed Broker.

### RUI-08 Bloom functionality

- CLI, VFS, mount, package execution, browser ceremony, passkeys, local custody,
  backup, recovery, update, and uninstall work from the notarized rootless app;
- no workflow asks for administrator credentials;
- no normal workflow requests access to Broker/Signer private containers.

Failure of any RUI test blocks the production rootless platform claim. It does
not become a documentation exception.

## 16. Release claim

The release matrix may advertise:

```text
platform = "macos-rootless-code-identity"
minimum_os = "15.0"
backend = "local"
```

only when:

- the exact bundle passes RUI-01 through RUI-08;
- the full AC-01 through AC-35 suite passes with installed production
  executables;
- the conformance report is signed and bound to the exact bundle digest;
- notarization acceptance and stapled-ticket validation pass;
- no App Group, Keychain group, entitlement, launch constraint, or executable
  differs from the reviewed manifest.

The release gate rejects generic `macos`, App-Group-only, ad-hoc, unsigned,
unnotarized, macOS 14-or-older, AWS KMS, or test-unclaimed combinations.

## 17. Go/no-go conclusion

The architecture is technically capable of providing similar custody and
authorization containment to Unix principals for the compromised-Machine
threat model, provided every requirement above holds.

It cannot honestly claim identical security:

- service availability remains controllable by the same login UID;
- macOS permits explicit user authorization of another app's container;
- the construction depends on Apple code identity, notarization, Keychain, and
  macOS 15 container enforcement rather than kernel UID separation.

Critical secrets in service-private Keychain groups and authenticated
encryption of all container state convert the container-consent difference
from key compromise into confidentiality exposure of bounded metadata plus
denial of service. If the W0 spike shows any path to private Keychain material,
accepted state forgery, arbitrary Signer networking, unapproved service code,
or direct Machine-to-Signer authority, rootless macOS is nonconforming and the
Unix-principal installer remains the only supported macOS profile.

## 18. Implementation order

Work proceeds in this order. A failed phase blocks later rootless work rather
than producing a weakened fallback:

1. **Signed skeleton:** build the exact nested bundle layout, entitlements,
   embedded profiles, launch constraints, release tuple, and notarized/stapled
   artifact. Prove Gatekeeper launch from `~/Applications` and rejection of
   every altered nested component.
2. **Rootless activation:** register both embedded LaunchAgents with
   `SMAppService` from an ordinary login, exercise approval/revocation and
   logout/login, and prove that no write or authorization requires root.
3. **Isolation spike:** implement only container lookup, private Keychain test
   items, and closed test sockets. Run RUI-01 through RUI-06, including copied
   helpers, same-UID hostile code, another local UID, explicit container
   consent, and profile failure.
4. **Durability spike:** implement immutable encrypted snapshots, exact
   Keychain-anchor compare-and-swap, peer journal heads, release floor, and
   crash injection at every transition. Pass AC-07, AC-18, and AC-19 before
   moving custody state.
5. **Listener lifecycle:** implement direct exclusive bind, the diagnostic
   owner probe, failure-only KeepAlive, and Machine startup reporting. Run
   RUI-07 with two real GUI users.
6. **Triad integration:** move Broker and Signer RPC/state into the proven
   boundaries without adding methods or fallback code. Keep the rootless
   platform claim disabled.
7. **Install/update/uninstall:** implement section 13 and test interrupted
   installs, every update boundary, profile renewal, retained-state uninstall,
   and permanent deletion.
8. **Release qualification:** run RUI-01 through RUI-08 and AC-01 through
   AC-35 against the exact notarized artifact on clean macOS 15 and
   current-major VMs. Sign the digest-bound report and only then enable
   `macos-rootless-code-identity`.

Phases 1 through 5 are a feasibility gate, not production implementation.
They intentionally precede migration of wallet material. No real custody seed
is used until those phases pass.
