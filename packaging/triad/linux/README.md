# Bloom triad Linux packaging source

This directory is source input for the privileged installer. It is not
installed directly from a source checkout.

The installer creates one system-owned instance for each interactive login UID
that has Bloom enabled. For login UID `1000`, the effective principals are
`bloom-broker-1000` and `bloom-signer-1000`; the Machine continues to run as
the interactive login principal. The generated `sysusers.d` and `tmpfiles.d`
files form two deliberately non-transitive data-plane groups:

- the login principal and Broker share `bloom-machine-broker-1000`;
- Broker and Signer share `bloom-broker-signer-1000`;
- the login principal is not a member of the Broker--Signer group.

The separate `bloom-revoke-1000` group reaches only the two control sockets.
It grants no access to either data-plane socket.

Broker and Signer own their four authenticated Unix sockets so Linux
`SO_PEERCRED` reports the actual security-service UID in both directions.
Systemd owns only the canonical `127.0.0.1:18734` ceremony listener, where Unix
peer credentials are unavailable, and passes that TCP listener to Broker. The
authenticated login-session sentinel owns its Unix socket while the user's
systemd session is active. A path unit starts Broker and Signer only after that
session socket exists. A canonical-listener conflict therefore fails the
`bloom-broker-ceremony@UID.socket` unit and never selects another port.

State roots and service configuration live below principal-owned mode-0700
directories. The edge manifest and binaries are root-owned and not writable by
any product principal. The local Signer service permits only `AF_UNIX`.
The AWS KMS drop-in is a separate installer-rendered, instance-specific
profile. For UID `1000`, the installer renders it to
`bloom-signer@1000.service.d/50-aws-kms.conf`; it enables IP sockets but
retains `IPAddressDeny=any`. The installer must render the reviewed KMS
endpoint CIDRs as `IPAddressAllow` entries. Empty or wildcard egress is an
installer error.

The templates use `@...@` placeholders where packaging must supply an absolute
binary path, login identity, or reviewed egress list. `%i` is the systemd
instance specifier and is intentionally left for systemd.

The installer publishes `/usr/bin/bloom-uninstall`. Running it through `sudo`
without `--purge` removes runtime integration but retains the selected login's
configuration and custody state. `--purge` additionally requires the exact
`delete-bloom-login-LOGIN_UID` token before deleting that material. Shared
runtime files remain while any active enrollment needs them, and the
uninstaller remains while retained custody is present.

For the login account that invoked `sudo`, the commands are:

```sh
sudo bloom-uninstall
sudo bloom-uninstall --purge "delete-bloom-login-$(id -u)"
```

The root-owned edge manifest pins `trusted_time_source` to
`linux-system-clock`. Bloom reads the ordinary host wall clock and protects
rolling windows with its own durable nondecreasing floor. On the same boot it
also compares wall-clock progress with a suspend-aware monotonic anchor, so a
large unexpected forward step degrades rate-limited signing. A later boot may
legitimately advance by more than the same-boot bound while the machine was
powered off. Bloom does not require Chrony or another time daemon.

Packaging creates a mode-0700 `audit-checkpoints` directory below each
service's state root and passes its absolute path through
`BLOOM_BROKER_AUDIT_CHECKPOINT_DIR` or
`BLOOM_SIGNER_AUDIT_CHECKPOINT_DIR`. Only that service principal can read or append
its checkpoint records. In particular, the Machine login and Broker cannot
read the Signer checkpoint root. Runtime checkpoint writes reject symlinks,
non-owned directories, sequence rollback, and replacement.
