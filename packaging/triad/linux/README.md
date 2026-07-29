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

All five Unix sockets and the canonical `127.0.0.1:18734` listener are owned by
systemd. The installer enables every socket instance before exposing the
Machine client. The service processes consume the named descriptors and have
no self-bind fallback. A canonical-listener conflict therefore fails the
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

The root-owned edge manifest pins `trusted_time_source` to
`linux-chrony-nts`. The installer renders `chrony/bloom-nts.conf.in` with at
least two independently operated NTS servers and refuses an unauthenticated
source. Broker and Signer query the kernel synchronization status; loss of
synchronization produces an untrusted reading and degraded rate-limited
signing rather than a wall-clock fallback.
