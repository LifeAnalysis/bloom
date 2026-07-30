#!/usr/bin/env bash
set -Eeuo pipefail

usage() {
  echo "usage: run-disposable.sh PAYLOAD_DIR LOGIN_UID LOGIN_USER" >&2
  exit 64
}

[[ $# -eq 3 ]] || usage
payload="$(cd "$1" && pwd -P)"
login_uid="$2"
login_user="$3"
[[ "$login_uid" =~ ^[1-9][0-9]*$ ]] || usage
[[ "$login_user" =~ ^[a-z_][a-z0-9_-]*$ ]] || usage

[[ "$EUID" -eq 0 && "$(uname -s)" == "Darwin" ]] || {
  echo "W0 requires root on a disposable macOS host" >&2
  exit 77
}
marker="/private/var/db/bloom-w0-disposable-host"
if [[ "${BLOOM_RUN_MACOS_UNIX_W0:-}" != "true" ]] ||
  [[ ! -f "$marker" || -L "$marker" ]] ||
  ! grep -Fx 'bloom-macos-unix-w0-disposable-v1' "$marker" >/dev/null
then
  echo "W0 host is not explicitly marked disposable" >&2
  exit 77
fi
[[ "$(<"$payload/PLATFORM_CLAIM")" == "macos-unix-principals-w0" ]] || {
  echo "W0 payload has the wrong platform claim" >&2
  exit 65
}
[[ "$(id -u "$login_user")" == "$login_uid" ]] || {
  echo "W0 login name and UID do not match" >&2
  exit 65
}
launchctl print "gui/$login_uid" >/dev/null 2>&1 || {
  echo "W0 requires an active GUI login for the selected user" >&2
  exit 69
}

triad_source="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
installer="$triad_source/release/install-macos.sh"
enrollment="/Library/Application Support/BloomTriad/enrollments/$login_uid.json"

cleanup() {
  status=$?
  if [[ -f "$enrollment" ]]; then
    "$installer" uninstall / "$login_uid" "delete-bloom-login-$login_uid" || true
  fi
  exit "$status"
}
trap cleanup EXIT

for kind_and_name in \
  "Users bloom-broker-$login_uid" \
  "Users bloom-signer-$login_uid" \
  "Groups bloom-broker-$login_uid" \
  "Groups bloom-signer-$login_uid" \
  "Groups bloom-machine-broker-$login_uid" \
  "Groups bloom-broker-signer-$login_uid" \
  "Groups bloom-revoke-$login_uid"
do
  kind="${kind_and_name%% *}"
  name="${kind_and_name#* }"
  if dscl . -read "/$kind/$name" >/dev/null 2>&1; then
    echo "W0 refuses to adopt pre-existing Directory Service record $kind/$name" >&2
    exit 65
  fi
done

"$installer" install / "$login_uid" "$login_user" "$payload"

field() {
  plutil -extract "$1" raw -o - "$enrollment"
}

broker_uid="$(field broker_uid)"
signer_uid="$(field signer_uid)"
broker_gid="$(field broker_gid)"
signer_gid="$(field signer_gid)"
machine_broker_gid="$(field machine_broker_gid)"
broker_signer_gid="$(field broker_signer_gid)"
revoke_gid="$(field revoke_gid)"

assert_record() {
  kind="$1"
  name="$2"
  attribute="$3"
  expected="$4"
  observed="$(
    dscl . -read "/$kind/$name" "$attribute" |
      sed -n "s/^$attribute: //p"
  )"
  [[ "$observed" == "$expected" ]] || {
    echo "$kind/$name $attribute: expected $expected, observed $observed" >&2
    exit 1
  }
}

assert_record Users "bloom-broker-$login_uid" UniqueID "$broker_uid"
assert_record Users "bloom-broker-$login_uid" PrimaryGroupID "$broker_gid"
assert_record Users "bloom-broker-$login_uid" IsHidden 1
assert_record Users "bloom-broker-$login_uid" UserShell /usr/bin/false
assert_record Users "bloom-signer-$login_uid" UniqueID "$signer_uid"
assert_record Users "bloom-signer-$login_uid" PrimaryGroupID "$signer_gid"
assert_record Users "bloom-signer-$login_uid" IsHidden 1
assert_record Users "bloom-signer-$login_uid" UserShell /usr/bin/false

dseditgroup -o checkmember -m "$login_user" "bloom-machine-broker-$login_uid" >/dev/null
dseditgroup -o checkmember -m "bloom-broker-$login_uid" "bloom-machine-broker-$login_uid" >/dev/null
dseditgroup -o checkmember -m "bloom-broker-$login_uid" "bloom-broker-signer-$login_uid" >/dev/null
dseditgroup -o checkmember -m "bloom-signer-$login_uid" "bloom-broker-signer-$login_uid" >/dev/null
if dseditgroup -o checkmember -m "$login_user" "bloom-broker-signer-$login_uid" >/dev/null 2>&1; then
  echo "Machine login unexpectedly belongs to the Broker-Signer group" >&2
  exit 1
fi

assert_metadata() {
  path="$1"
  expected="$2"
  observed="$(stat -f '%u:%g:%Lp' "$path")"
  [[ "$observed" == "$expected" ]] || {
    echo "$path: expected $expected, observed $observed" >&2
    exit 1
  }
}

assert_metadata "/private/var/db/bloom/$login_uid/broker" "$broker_uid:$broker_gid:700"
assert_metadata "/private/var/db/bloom/$login_uid/signer" "$signer_uid:$signer_gid:700"
assert_metadata \
  "/private/var/run/bloom/$login_uid/machine-broker" \
  "0:$machine_broker_gid:710"
assert_metadata \
  "/private/var/run/bloom/$login_uid/broker-signer" \
  "0:$broker_signer_gid:710"
assert_metadata "/private/var/run/bloom/$login_uid/revoke" "0:$revoke_gid:710"
assert_metadata \
  "/private/var/run/bloom/$login_uid/session" \
  "$login_uid:$revoke_gid:710"

broker_probe="/private/var/db/bloom/$login_uid/broker/w0-private"
signer_probe="/private/var/db/bloom/$login_uid/signer/w0-private"
install -o "bloom-broker-$login_uid" -g "bloom-broker-$login_uid" -m 0600 /dev/null "$broker_probe"
install -o "bloom-signer-$login_uid" -g "bloom-signer-$login_uid" -m 0600 /dev/null "$signer_probe"
sudo -u "$login_user" test ! -r "$broker_probe"
sudo -u "$login_user" test ! -r "$signer_probe"
sudo -u "bloom-broker-$login_uid" test ! -r "$signer_probe"
sudo -u "$login_user" test ! -r \
  "/Library/Application Support/BloomTriad/config/$login_uid/installer/identity.json"

launchctl print "system/com.bloom.broker.$login_uid" >/dev/null
launchctl print "system/com.bloom.signer.$login_uid" >/dev/null
launchctl print "gui/$login_uid/com.bloom.session" >/dev/null

pf_rules="$(pfctl -a "com.bloom.triad/$login_uid" -sr)"
grep -E "user (<?$broker_uid|bloom-broker-$login_uid)" <<<"$pf_rules" >/dev/null
grep -E "user (<?$signer_uid|bloom-signer-$login_uid)" <<<"$pf_rules" >/dev/null

for socket in \
  "/private/var/run/bloom/$login_uid/machine-broker/broker.sock" \
  "/private/var/run/bloom/$login_uid/broker-signer/signer.sock" \
  "/private/var/run/bloom/$login_uid/revoke/broker-control.sock" \
  "/private/var/run/bloom/$login_uid/revoke/signer-control.sock" \
  "/private/var/run/bloom/$login_uid/session/session.sock"
do
  deadline=$((SECONDS + 20))
  while [[ ! -S "$socket" && $SECONDS -lt $deadline ]]; do
    sleep 1
  done
  [[ -S "$socket" ]] || {
    echo "launchd did not create $socket" >&2
    exit 1
  }
done

assert_metadata \
  "/private/var/run/bloom/$login_uid/machine-broker/broker.sock" \
  "$broker_uid:$machine_broker_gid:660"
assert_metadata \
  "/private/var/run/bloom/$login_uid/broker-signer/signer.sock" \
  "$signer_uid:$broker_signer_gid:660"
assert_metadata \
  "/private/var/run/bloom/$login_uid/revoke/broker-control.sock" \
  "$broker_uid:$revoke_gid:660"
assert_metadata \
  "/private/var/run/bloom/$login_uid/revoke/signer-control.sock" \
  "$signer_uid:$revoke_gid:660"
assert_metadata \
  "/private/var/run/bloom/$login_uid/session/session.sock" \
  "$login_uid:$revoke_gid:660"

ceremony_headers="$(curl --silent --show-error --max-time 2 --dump-header - \
  --output /dev/null http://127.0.0.1:18734/)"
grep -Fi 'x-bloom-ceremony-owner: bloom-broker-v1' <<<"$ceremony_headers" >/dev/null

containment_status="/private/var/run/bloom/$login_uid/containment/status.json"
assert_metadata "$containment_status" "0:0:644"
release_digest="$(field release_digest)"
machine_binary="/usr/local/libexec/bloom/current/bloom"
sudo -u "$login_user" \
  "$machine_binary" \
  --triad-health-check \
  "$release_digest"

pfctl -a "com.bloom.triad/$login_uid" -F rules
deadline=$((SECONDS + 10))
while [[ $SECONDS -lt $deadline ]]; do
  if [[ -f "$containment_status" ]] &&
    [[ "$(plutil -extract available raw -o - "$containment_status")" == "false" ]]
  then
    break
  fi
  sleep 1
done
[[ "$(plutil -extract available raw -o - "$containment_status")" == "false" ]] || {
  echo "packet-filter monitor did not report the removed anchor" >&2
  exit 1
}
if sudo -u "$login_user" \
  "$machine_binary" \
  --triad-health-check \
  "$release_digest"
then
  echo "Broker remained ready after its packet-filter anchor disappeared" >&2
  exit 1
fi
pfctl \
  -a "com.bloom.triad/$login_uid" \
  -f "/etc/pf.anchors/com.bloom.triad.$login_uid"
"$machine_binary" --triad-pf-monitor-once
sudo -u "$login_user" \
  "$machine_binary" \
  --triad-health-check \
  "$release_digest"

echo "Bloom macOS Unix-principal disposable W0 isolation checks passed"
