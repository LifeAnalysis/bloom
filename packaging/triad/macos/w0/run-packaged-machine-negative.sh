#!/usr/bin/env bash
set -Eeuo pipefail

usage() {
  echo "usage: run-packaged-machine-negative.sh MACHINE_BINARY LOGIN_UID LOGIN_USER BROKER_UID SIGNER_UID MACHINE_IDENTITY EDGE_MANIFEST" >&2
  exit 64
}

[[ $# -eq 7 ]] || usage
machine_binary="$1"
login_uid="$2"
login_user="$3"
broker_uid="$4"
signer_uid="$5"
machine_identity="$6"
edge_manifest="$7"
[[ "$EUID" -eq 0 && "$(uname -s)" == "Darwin" ]] || exit 77
[[ "$login_uid" =~ ^[1-9][0-9]*$ ]] || usage
[[ "$broker_uid" =~ ^[1-9][0-9]*$ ]] || usage
[[ "$signer_uid" =~ ^[1-9][0-9]*$ ]] || usage
[[ -x "$machine_binary" && ! -L "$machine_binary" ]] || exit 65
[[ -f "$machine_identity" && ! -L "$machine_identity" ]] || exit 65
[[ -f "$edge_manifest" && ! -L "$edge_manifest" ]] || exit 65

marker="/private/var/db/bloom-w0-disposable-host"
if [[ "${BLOOM_RUN_MACOS_UNIX_W0:-}" != "true" ]] ||
  [[ ! -f "$marker" || -L "$marker" ]] ||
  ! grep -Fx 'bloom-macos-unix-w0-disposable-v1' "$marker" >/dev/null
then
  echo "packaged Machine runtime negative requires a disposable W0 host" >&2
  exit 77
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
work="$(mktemp -d /private/tmp/bloom-ma13-runtime.XXXXXX)"
runtime="$work/runtime"
clean_home="$work/clean-home"
broker_socket="$runtime/machine-broker/broker.sock"
signer_socket="/private/var/run/bloom/$login_uid/broker-signer/signer.sock"
signer_socket_dir="$(dirname "$signer_socket")"
machine_socket="$runtime/machine.sock"
broker_connected="$runtime/machine-broker/connected"
signer_connected="$runtime/hostile-signer/connected"
broker_user="bloom-broker-$login_uid"
signer_user="bloom-signer-$login_uid"
broker_label="system/com.bloom.broker.$login_uid"
broker_plist="/Library/LaunchDaemons/com.bloom.broker.$login_uid.plist"
signer_label="system/com.bloom.signer.$login_uid"
signer_plist="/Library/LaunchDaemons/com.bloom.signer.$login_uid.plist"
broker_listener_pid=""
signer_listener_pid=""
machine_service_pid=""
broker_was_loaded=false
signer_was_loaded=false
signer_socket_dir_owner=""
signer_socket_dir_group=""
signer_socket_dir_mode=""

cleanup() {
  status=$?
  trap - EXIT INT TERM
  for pid in "$machine_service_pid" "$broker_listener_pid" "$signer_listener_pid"; do
    if [[ -n "$pid" ]]; then
      kill "$pid" 2>/dev/null || true
      wait "$pid" 2>/dev/null || true
    fi
  done
  if [[ -n "$signer_socket_dir_mode" ]]; then
    chown "$signer_socket_dir_owner:$signer_socket_dir_group" "$signer_socket_dir" \
      2>/dev/null || true
    chmod "$signer_socket_dir_mode" "$signer_socket_dir" 2>/dev/null || true
  fi
  if [[ "$signer_was_loaded" == true ]] &&
    ! launchctl print "$signer_label" >/dev/null 2>&1
  then
    launchctl bootstrap system "$signer_plist" >/dev/null 2>&1 || true
  fi
  if [[ "$broker_was_loaded" == true ]] &&
    ! launchctl print "$broker_label" >/dev/null 2>&1
  then
    launchctl bootstrap system "$broker_plist" >/dev/null 2>&1 || true
  fi
  rm -rf -- "$work"
  exit "$status"
}
trap cleanup EXIT INT TERM

/usr/bin/xcrun --sdk macosx clang \
  -std=c11 -Wall -Wextra -Werror \
  "$script_dir/hostile-unix-listener.c" \
  -o "$work/hostile-unix-listener"
chmod 0755 "$work/hostile-unix-listener"

mkdir -p "$runtime/machine-broker" "$runtime/hostile-signer" "$clean_home"
chown "$broker_uid" "$runtime/machine-broker"
chown "$signer_uid" "$runtime/hostile-signer"
chown -R "$login_uid" "$clean_home"
chmod 0755 "$work" "$runtime" "$runtime/machine-broker" "$runtime/hostile-signer"
chmod 0700 "$clean_home"
# Keep the clean test home intentionally small, but valid. Degraded authority
# operation means Broker/Signer may be unavailable; it does not bypass normal
# Machine configuration validation. The unreachable local RPC keeps this test
# deterministic and prevents external network dependence.
printf '%s\n' \
  'default_chain = "anvil"' \
  '' \
  '[petals]' \
  'preinstalled = []' \
  '' \
  '[chains.anvil]' \
  'name = "anvil"' \
  'chain_id = 31337' \
  'rpc_urls = ["http://127.0.0.1:1"]' \
  'allow_broadcast = false' |
  sudo -u "$login_user" /usr/bin/tee "$clean_home/config.toml" >/dev/null
chmod 0600 "$clean_home/config.toml"

# The real installed Broker must be stopped. The replacement has the Broker
# OS principal but deliberately cannot authenticate a triad response.
launchctl print "$broker_label" >/dev/null
launchctl print "$signer_label" >/dev/null
broker_was_loaded=true
signer_was_loaded=true
launchctl bootout "$broker_label"
launchctl bootout "$signer_label"
deadline=$((SECONDS + 15))
while { pgrep -u "$broker_uid" -x bloom-broker >/dev/null 2>&1 ||
  pgrep -u "$signer_uid" -x bloom-signer >/dev/null 2>&1; } &&
  [[ $SECONDS -lt $deadline ]]
do
  sleep 0.1
done
if pgrep -u "$broker_uid" -x bloom-broker >/dev/null 2>&1 ||
  pgrep -u "$signer_uid" -x bloom-signer >/dev/null 2>&1
then
  echo "installed Broker/Signer did not stop for the packaged Machine negative" >&2
  exit 1
fi

# The installed 0710 parent normally makes a direct Machine attempt fail at
# pathname traversal before accept(2), which would make a zero-connection
# sentinel ambiguous. On this disposable host only, record its exact metadata
# and add other-execute while both real services are stopped. The hostile
# socket is then reachable, so a zero accept marker proves no direct connector
# attempt rather than merely re-proving the OS ACL. Cleanup restores metadata
# before either LaunchDaemon is bootstrapped.
[[ -d "$signer_socket_dir" && ! -L "$signer_socket_dir" ]] || exit 65
signer_socket_dir_owner="$(stat -f '%u' "$signer_socket_dir")"
signer_socket_dir_group="$(stat -f '%g' "$signer_socket_dir")"
signer_socket_dir_mode="$(stat -f '%Lp' "$signer_socket_dir")"
chmod 0711 "$signer_socket_dir"

sudo -u "$broker_user" \
  "$work/hostile-unix-listener" "$broker_socket" "$broker_connected" &
broker_listener_pid=$!
sudo -u "$signer_user" \
  "$work/hostile-unix-listener" "$signer_socket" "$signer_connected" &
signer_listener_pid=$!
deadline=$((SECONDS + 5))
while [[ (! -S "$broker_socket" || ! -S "$signer_socket") && $SECONDS -lt $deadline ]]; do
  sleep 0.05
done
[[ -S "$broker_socket" && -S "$signer_socket" ]] || {
  echo "hostile runtime listeners did not become ready" >&2
  exit 1
}

run_machine_with_deadline() {
  local output="$1"
  local command_pid deadline machine_status
  shift
  sudo -H -u "$login_user" env \
    BLOOM_HOME="$clean_home" \
    BLOOM_BROKER_SOCKET="$broker_socket" \
    BLOOM_MACHINE_IDENTITY="$machine_identity" \
    BLOOM_EDGE_MANIFEST="$edge_manifest" \
    "$machine_binary" "$@" >"$output" 2>&1 &
  command_pid=$!
  deadline=$((SECONDS + 8))
  while kill -0 "$command_pid" 2>/dev/null && [[ $SECONDS -lt $deadline ]]; do
    sleep 0.05
  done
  if kill -0 "$command_pid" 2>/dev/null; then
    kill "$command_pid" 2>/dev/null || true
    wait "$command_pid" 2>/dev/null || true
    echo "packaged Machine hung with its authority service unavailable" >&2
    return 124
  fi
  set +e
  wait "$command_pid"
  machine_status=$?
  set -e
  return "$machine_status"
}

# Launch the installed production executable in its long-running Machine
# service mode. macOS packages `bloom`; `serve` is its Machine service mode
# (there is no separately installed bloom-machine payload or Machine plist).
sudo -H -u "$login_user" env \
  BLOOM_HOME="$clean_home" \
  BLOOM_BROKER_SOCKET="$broker_socket" \
  BLOOM_MACHINE_IDENTITY="$machine_identity" \
  BLOOM_EDGE_MANIFEST="$edge_manifest" \
  "$machine_binary" --home "$clean_home" serve \
    --endpoint "unix:$machine_socket" >"$work/machine-service.log" 2>&1 &
machine_service_pid=$!
deadline=$((SECONDS + 8))
while [[ ! -S "$machine_socket" && $SECONDS -lt $deadline ]]; do
  kill -0 "$machine_service_pid" 2>/dev/null || {
    cat "$work/machine-service.log" >&2
    echo "packaged production Machine service exited during degraded startup" >&2
    exit 1
  }
  sleep 0.05
done
[[ -S "$machine_socket" ]] || {
  cat "$work/machine-service.log" >&2
  echo "packaged production Machine service did not publish its IPC socket" >&2
  exit 1
}

# A key-free read path remains usable through that exact packaged service with
# Broker stopped. This exercises a clean production home before authority
# negatives and proves no legacy authority file remains open in the process.
run_machine_with_deadline \
  "$work/status.log" \
  --home "$clean_home" --connect "unix:$machine_socket" status || {
  cat "$work/status.log" >&2
  echo "packaged Machine did not preserve its degraded read/status path" >&2
  exit 1
}
if lsof -nP -a -p "$machine_service_pid" -Fn | grep -E \
  '/(keystore|auth|challenges?|grants?|policy-session|signer-cache)(/|$)' \
  >/dev/null
then
  echo "packaged production Machine service opened legacy authority state" >&2
  lsof -nP -a -p "$machine_service_pid" >&2 || true
  exit 1
fi

# Force the packaged Machine service itself across its configured Broker edge.
# The hostile same-principal endpoint closes without an authenticated reply;
# the VFS projection request must fail promptly rather than hang or fall back.
set +e
run_machine_with_deadline \
  "$work/projection.log" \
  --home "$clean_home" --connect "unix:$machine_socket" vfs ls /wallets
projection_status=$?
set -e
[[ "$projection_status" -ne 0 && "$projection_status" -ne 124 ]] || {
  cat "$work/projection.log" >&2
  echo "packaged Machine service did not fail the hostile Broker projection promptly" >&2
  exit 1
}

set +e
run_machine_with_deadline \
  "$work/custody.log" \
  --home "$clean_home" wallet new ma13-runtime-negative
custody_status=$?
set -e
[[ "$custody_status" -ne 0 && "$custody_status" -ne 124 ]] || {
  cat "$work/custody.log" >&2
  echo "packaged Machine did not fail the Broker-hostile custody request promptly" >&2
  exit 1
}
deadline=$((SECONDS + 3))
while [[ ! -f "$broker_connected" && $SECONDS -lt $deadline ]]; do sleep 0.05; done
[[ -f "$broker_connected" ]] || {
  echo "packaged production Machine service did not exercise the hostile Broker socket" >&2
  exit 1
}
[[ ! -e "$signer_connected" ]] || {
  echo "packaged Machine connected directly to the hostile Signer sentinel" >&2
  exit 1
}

for state_root in "$clean_home" "$runtime"; do
  forbidden="$(find "$state_root" \
    \( -name keystore -o -name auth -o -name auth.sqlite -o -name challenge -o \
       -name challenges -o -name grant -o -name grants -o \
       -name policy-session -o -name signer-cache \) -print -quit)"
  [[ -z "$forbidden" ]] || {
    echo "packaged Machine created legacy authority state: $forbidden" >&2
    exit 1
  }
done

# Keep the Signer sentinel alive until every Machine command and state check
# has finished, then prove it observed no direct connector attempt.
kill -0 "$signer_listener_pid"
kill -0 "$machine_service_pid"
echo "packaged Machine runtime negative passed"
