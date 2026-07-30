#!/usr/bin/env bash
set -Eeuo pipefail

usage() {
  echo "usage: run-disposable.sh PAYLOAD_DIR LOGIN_UID LOGIN_USER [UPGRADE_PAYLOAD [FAILING_UPGRADE_PAYLOAD]]" >&2
  exit 64
}

[[ $# -ge 3 && $# -le 5 ]] || usage
payload="$(cd "$1" && pwd -P)"
login_uid="$2"
login_user="$3"
upgrade_payload=""
failing_upgrade_payload=""
if [[ $# -ge 4 ]]; then
  upgrade_payload="$(cd "$4" && pwd -P)"
fi
if [[ $# -eq 5 ]]; then
  failing_upgrade_payload="$(cd "$5" && pwd -P)"
fi
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
for additional_payload in "$upgrade_payload" "$failing_upgrade_payload"; do
  [[ -z "$additional_payload" ]] && continue
  [[ "$(<"$additional_payload/PLATFORM_CLAIM")" == "macos-unix-principals-w0" ]] || {
    echo "W0 upgrade payload has the wrong platform claim" >&2
    exit 65
  }
done
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
rotation_fixtures="$(mktemp -d /private/tmp/bloom-w0-rotation.XXXXXX)"

cleanup() {
  status=$?
  if [[ -f "$enrollment" ]]; then
    "$installer" uninstall / "$login_uid" "delete-bloom-login-$login_uid" || true
  fi
  rm -rf -- "$rotation_fixtures"
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

recovery_uid=424242
recovery_group="bloom-broker-$recovery_uid"
[[ "$recovery_uid" != "$login_uid" ]] || exit 65
for recovery_path in \
  "/Library/Application Support/BloomTriad/enrollments/$recovery_uid.json" \
  "/Library/Application Support/BloomTriad/config/$recovery_uid" \
  "/private/var/db/bloom/$recovery_uid" \
  "/private/var/run/bloom/$recovery_uid"
do
  [[ ! -e "$recovery_path" ]] || {
    echo "W0 recovery probe UID already has Bloom state" >&2
    exit 65
  }
done
if dscl . -read "/Groups/$recovery_group" >/dev/null 2>&1; then
  echo "W0 recovery probe group already exists" >&2
  exit 65
fi
recovery_gid="$(
  dscl . -list /Groups PrimaryGroupID |
    awk '$NF ~ /^[0-9]+$/ && $NF > maximum { maximum = $NF } END { print maximum + 1 }'
)"
recovery_transaction="/Library/Application Support/BloomTriad/enrollment-transactions/$recovery_uid"
mkdir "$recovery_transaction"
chmod 0700 "$recovery_transaction"
chown root:wheel "$recovery_transaction"
printf '%s\n' 'bloom.macos-enrollment-transaction.1' \
  > "$recovery_transaction/schema"
printf '%s\n' "$recovery_uid" > "$recovery_transaction/login-uid"
printf '%s\n' 'bloom-w0-recovery' > "$recovery_transaction/login-user"
printf '%s\n' provisioning > "$recovery_transaction/phase"
printf '%s\n' Groups "$recovery_group" PrimaryGroupID "$recovery_gid" \
  > "$recovery_transaction/record.001"
chmod 0600 "$recovery_transaction"/*
dscl . -create "/Groups/$recovery_group"
dscl . -create "/Groups/$recovery_group" PrimaryGroupID "$recovery_gid"
sync
"$installer" install / "$login_uid" "$login_user" "$payload"
[[ ! -e "$recovery_transaction" ]] || {
  echo "installer did not consume the interrupted enrollment journal" >&2
  exit 1
}
if dscl . -read "/Groups/$recovery_group" >/dev/null 2>&1; then
  echo "installer did not remove the journal-owned partial group" >&2
  exit 1
fi

broker_uid="$(field broker_uid)"
signer_uid="$(field signer_uid)"
broker_gid="$(field broker_gid)"
signer_gid="$(field signer_gid)"
machine_broker_gid="$(field machine_broker_gid)"
broker_signer_gid="$(field broker_signer_gid)"
revoke_gid="$(field revoke_gid)"
[[ "$(field state)" == "active" ]] || {
  echo "installer published the enrollment before activation completed" >&2
  exit 1
}

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

broker_config="/Library/Application Support/BloomTriad/config/$login_uid/broker/config.json"
rotated_config="$rotation_fixtures/broker-valid.json"
cp "$broker_config" "$rotated_config"
plutil -replace maximum_connections -integer 63 "$rotated_config"
"$installer" rotate-config / "$login_uid" broker "$rotated_config"
[[ "$(plutil -extract maximum_connections raw -o - "$broker_config")" == "63" ]] || {
  echo "valid Broker config rotation did not become active" >&2
  exit 1
}
sudo -u "$login_user" \
  "$machine_binary" \
  --triad-health-check \
  "$release_digest"
rotated_digest="$(shasum -a 256 "$broker_config" | awk '{print $1}')"

immutable_config="$rotation_fixtures/broker-immutable.json"
cp "$broker_config" "$immutable_config"
plutil -replace signer_socket_path -string /private/tmp/forbidden.sock "$immutable_config"
if "$installer" rotate-config / "$login_uid" broker "$immutable_config"; then
  echo "Broker config rotation changed an immutable cross-principal field" >&2
  exit 1
fi
[[ ! -e "/Library/Application Support/BloomTriad/rotation-transaction" ]] || {
  echo "rejected Broker config rotation published a recovery journal" >&2
  exit 1
}
[[ "$(shasum -a 256 "$broker_config" | awk '{print $1}')" == "$rotated_digest" ]]

failing_config="$rotation_fixtures/broker-failing.json"
cp "$broker_config" "$failing_config"
plutil -replace maximum_connections -string invalid "$failing_config"
"$installer" rotate-config / "$login_uid" broker "$failing_config" &
interrupted_rotation_pid=$!
rotation_phase="/Library/Application Support/BloomTriad/rotation-transaction/phase"
deadline=$((SECONDS + 30))
while [[ $SECONDS -lt $deadline ]]; do
  if [[ -f "$rotation_phase" ]] &&
    [[ "$(<"$rotation_phase")" == "activating" ]]
  then
    break
  fi
  if ! kill -0 "$interrupted_rotation_pid" 2>/dev/null; then
    echo "W0 config rotation exited before its interruption point" >&2
    wait "$interrupted_rotation_pid" || true
    exit 1
  fi
  sleep 0.05
done
[[ -f "$rotation_phase" && "$(<"$rotation_phase")" == "activating" ]] || {
  echo "W0 did not observe the config rotation activation phase" >&2
  kill "$interrupted_rotation_pid" 2>/dev/null || true
  wait "$interrupted_rotation_pid" || true
  exit 1
}
kill -9 "$interrupted_rotation_pid"
wait "$interrupted_rotation_pid" 2>/dev/null || true
[[ -d "/Library/Application Support/BloomTriad/rotation-transaction" ]] || {
  echo "interrupted W0 config rotation did not leave a recovery journal" >&2
  exit 1
}
"$installer" rotate-config / "$login_uid" broker "$rotated_config"
[[ ! -e "/Library/Application Support/BloomTriad/rotation-transaction" ]] || {
  echo "W0 config rotation recovery did not consume its journal" >&2
  exit 1
}
[[ "$(shasum -a 256 "$broker_config" | awk '{print $1}')" == "$rotated_digest" ]]
sudo -u "$login_user" \
  "$machine_binary" \
  --triad-health-check \
  "$release_digest"

assert_active_release() {
  expected_digest="$1"
  [[ "$(field state)" == "active" ]]
  [[ "$(field release_digest)" == "$expected_digest" ]]
  current_target="$(readlink /usr/local/libexec/bloom/current)"
  [[ "$current_target" == "releases/$expected_digest" ]]
  sudo -u "$login_user" \
    /usr/local/libexec/bloom/current/bloom \
    --triad-health-check \
    "$expected_digest"
}

current_good_payload="$payload"
if [[ -n "$upgrade_payload" ]]; then
  prior_digest="$(field release_digest)"
  "$installer" install / "$login_uid" "$login_user" "$upgrade_payload"
  upgraded_digest="$(field release_digest)"
  [[ "$upgraded_digest" != "$prior_digest" ]] || {
    echo "W0 upgrade payload did not produce a new release digest" >&2
    exit 65
  }
  assert_active_release "$upgraded_digest"
  current_good_payload="$upgrade_payload"
fi

if [[ -n "$failing_upgrade_payload" ]]; then
  baseline_digest="$(field release_digest)"
  set +e
  "$installer" install / "$login_uid" "$login_user" "$failing_upgrade_payload"
  failed_status=$?
  set -e
  [[ "$failed_status" -ne 0 ]] || {
    echo "failing W0 upgrade unexpectedly activated" >&2
    exit 1
  }
  assert_active_release "$baseline_digest"

  "$installer" \
    install \
    / \
    "$login_uid" \
    "$login_user" \
    "$failing_upgrade_payload" &
  interrupted_pid=$!
  upgrade_phase="/Library/Application Support/BloomTriad/upgrade-transaction/phase"
  deadline=$((SECONDS + 30))
  while [[ $SECONDS -lt $deadline ]]; do
    if [[ -f "$upgrade_phase" ]] &&
      [[ "$(<"$upgrade_phase")" == "activating" ]]
    then
      break
    fi
    if ! kill -0 "$interrupted_pid" 2>/dev/null; then
      echo "W0 upgrade exited before its interruption point" >&2
      wait "$interrupted_pid" || true
      exit 1
    fi
    sleep 0.05
  done
  [[ -f "$upgrade_phase" && "$(<"$upgrade_phase")" == "activating" ]] || {
    echo "W0 did not observe the upgrade activation phase" >&2
    kill "$interrupted_pid" 2>/dev/null || true
    wait "$interrupted_pid" || true
    exit 1
  }
  kill -9 "$interrupted_pid"
  wait "$interrupted_pid" 2>/dev/null || true
  [[ -d "/Library/Application Support/BloomTriad/upgrade-transaction" ]] || {
    echo "interrupted W0 upgrade did not leave a recovery journal" >&2
    exit 1
  }
  "$installer" install / "$login_uid" "$login_user" "$current_good_payload"
  [[ ! -e "/Library/Application Support/BloomTriad/upgrade-transaction" ]] || {
    echo "W0 upgrade recovery did not consume its journal" >&2
    exit 1
  }
  assert_active_release "$baseline_digest"
fi

uninstall_transaction="/Library/Application Support/BloomTriad/uninstall-transactions/$login_uid"
"$installer" uninstall / "$login_uid" "delete-bloom-login-$login_uid" &
interrupted_uninstall_pid=$!
deadline=$((SECONDS + 30))
while [[ $SECONDS -lt $deadline ]]; do
  if [[ -d "$uninstall_transaction" ]]; then
    kill -STOP "$interrupted_uninstall_pid" 2>/dev/null || true
    break
  fi
  if ! kill -0 "$interrupted_uninstall_pid" 2>/dev/null; then
    echo "W0 uninstall exited before its interruption point" >&2
    wait "$interrupted_uninstall_pid" || true
    exit 1
  fi
  sleep 0.01
done
[[ -d "$uninstall_transaction" ]] || {
  echo "W0 did not observe the uninstall transaction" >&2
  kill "$interrupted_uninstall_pid" 2>/dev/null || true
  wait "$interrupted_uninstall_pid" || true
  exit 1
}
kill -9 "$interrupted_uninstall_pid"
wait "$interrupted_uninstall_pid" 2>/dev/null || true
"$installer" uninstall / "$login_uid" "delete-bloom-login-$login_uid"
[[ ! -e "$uninstall_transaction" ]] || {
  echo "W0 uninstall recovery did not consume its journal" >&2
  exit 1
}
[[ ! -e "$enrollment" ]]
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
    echo "W0 uninstall recovery left Directory Service record $kind/$name" >&2
    exit 1
  fi
done

echo "Bloom macOS Unix-principal disposable W0 isolation checks passed"
