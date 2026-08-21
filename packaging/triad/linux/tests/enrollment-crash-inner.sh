#!/usr/bin/env bash
set -euo pipefail

groupadd -g 31001 bloom-machine-broker-1000
groupadd -g 31002 bloom-broker-signer-1000
groupadd -g 31003 bloom-revoke-1000
useradd -u 1000 -g users -G bloom-machine-broker-1000,bloom-revoke-1000 alice
useradd -u 30001 -g users -G bloom-machine-broker-1000,bloom-broker-signer-1000 bloom-broker-1000
useradd -u 30002 -g users -G bloom-broker-signer-1000 bloom-signer-1000

mkdir -p /payload/bin /payload/installer/linux /payload/installer/release /fake-bin
cp /tested-bloom /payload/bin/bloom
cp -R /source/packaging/triad/linux/config /payload/installer/linux/config
cp /source/packaging/triad/release/enroll-linux.sh /payload/installer/release/enroll-linux.sh
printf '%s\n' fixture > /payload/SHA256SUMS
printf '#!/bin/sh\nexit 0\n' >/fake-bin/systemctl
printf '#!/bin/sh\nexit 0\n' >/fake-bin/runuser
chmod 0755 /fake-bin/systemctl /fake-bin/runuser /payload/installer/release/enroll-linux.sh
export PATH="/fake-bin:$PATH"

case "$BOUNDARY" in
  journal-temp) marker='bloom.linux-enrollment-transaction.1'; occurrence=2 ;;
  prepared) marker='sync_path "$transactions"'; occurrence=4 ;;
  published) marker='write_phase published' ;;
  activated) marker='"bloom-broker-ceremony@$login_uid.socket"'; occurrence=2 ;;
  committed) marker='write_phase committed' ;;
esac
awk -v marker="$marker" -v wanted="${occurrence:-1}" '
  { print }
  index($0, marker) { seen++; if (seen == wanted) print "kill -STOP $$" }
' /payload/installer/release/enroll-linux.sh >/enroll-killed
chmod 0755 /enroll-killed

/enroll-killed 1000 alice /payload &
pid=$!
for unused in $(seq 1 400); do
  state="$(awk '/^State:/ {print $2}' "/proc/$pid/status" 2>/dev/null || true)"
  [[ "$state" == T ]] && break
  sleep 0.025
done
[[ "${state:-}" == T ]] || { echo "process did not stop at $BOUNDARY" >&2; exit 1; }
kill -KILL "$pid"
wait "$pid" 2>/dev/null || true

/payload/installer/release/enroll-linux.sh 1000 alice /payload
[[ -f /etc/bloom/1000/edge-manifest.json ]]
[[ -f /etc/bloom/1000/machine/identity.json ]]
[[ -f /etc/bloom/1000/machine/revoke-identity.json ]]
[[ -f /etc/bloom/1000/session/identity.json ]]
[[ -f /etc/bloom/1000/installer/identity.json ]]
[[ -f /etc/bloom/1000/provenance-catalog.json ]]
[[ ! -e /etc/bloom/.transactions/1000 ]]
[[ -z "$(find /etc/bloom/.transactions -name '.new-1000.*' -print -quit)" ]]
[[ -z "$(find /etc/bloom/.transactions -name '.committed-1000.*' -print -quit)" ]]
[[ "$(stat -c %a /etc/bloom/1000)" == 711 ]]
[[ "$(stat -c %u /etc/bloom/1000/broker/identity.json)" == 30001 ]]
[[ "$(stat -c %u /etc/bloom/1000/signer/identity.json)" == 30002 ]]
echo "Linux enrollment recovered safely at $BOUNDARY"
