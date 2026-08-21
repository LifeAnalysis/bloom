#!/usr/bin/env bash
set -euo pipefail

useradd -u 1000 -m alice
mkdir -p /payload/bin /payload/config /payload/installer/linux \
  /payload/installer/release /fake-bin
cp /tested-bloom /payload/bin/bloom
for binary in bloom-broker bloom-signer bloom-signer-migrate; do
  printf '#!/bin/sh\nexit 0\n' >"/payload/bin/$binary"
  chmod 0755 "/payload/bin/$binary"
done
cp -R /source/packaging/triad/linux/config /payload/installer/linux/config
cp /source/packaging/triad/release/enroll-linux.sh /payload/installer/release/enroll-linux.sh
chmod 0755 /payload/installer/release/enroll-linux.sh
printf '%s\n' linux >/payload/PLATFORM_CLAIM
printf '%s\n' fixture >/payload/SHA256SUMS
printf 'time.cloudflare.com\ntime.nist.gov\n' >/payload/config/nts-servers.conf
for command in systemctl runuser systemd-sysusers systemd-tmpfiles; do
  printf '#!/bin/sh\nexit 0\n' >"/fake-bin/$command"
done
chmod 0755 /fake-bin/systemctl /fake-bin/runuser /fake-bin/systemd-sysusers /fake-bin/systemd-tmpfiles
export PATH="/fake-bin:$PATH"
export BLOOM_ALLOW_TEST_UNCLAIMED=true

if /source/packaging/triad/release/install-linux.sh install / 1000 alice /payload \
  >/tmp/unsigned.out 2>/tmp/unsigned.err
then
  echo 'unsigned production Linux payload was accepted' >&2
  exit 1
fi
grep -F 'signed release metadata' /tmp/unsigned.err >/dev/null
[[ ! -e /etc/bloom/1000 ]]
printf '%s\n' test-unclaimed >/payload/PLATFORM_CLAIM
/source/packaging/triad/release/install-linux.sh install / 1000 alice /payload
for relative in edge-manifest.json provenance-catalog.json \
  machine/identity.json machine/revoke-identity.json session/identity.json \
  installer/identity.json broker/config.json broker/identity.json \
  signer/config.json signer/identity.json
do
  [[ -f "/etc/bloom/1000/$relative" ]] || {
    echo "fresh Linux installer omitted $relative" >&2
    exit 1
  }
done
[[ ! -e /etc/bloom/.transactions/1000 ]]
[[ "$(stat -c %a /etc/bloom/1000)" == 711 ]]
[[ "$(stat -c %a /etc/bloom/1000/machine/identity.json)" == 600 ]]
echo 'Fresh root-only Linux installation produced a complete enrollment'
