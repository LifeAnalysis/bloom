#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
broker_repo="$(cd "${repo_root}/../bloom-broker" && pwd -P)"
launcher="${BLOOM_TRIAD_DEV_LAUNCHER:-${repo_root}/scripts/triad-dev-launch.sh}"
startup_timeout_secs="${BLOOM_INTEGRATION_STARTUP_TIMEOUT_SECS:-300}"

die() { printf 'MA-03 projection fidelity: %s\n' "$*" >&2; exit 1; }
command -v jq >/dev/null 2>&1 || die "jq is required"
[ -x "$launcher" ] || die "triad developer launcher is not executable: $launcher"
case "$startup_timeout_secs" in *[!0-9]*|'') die "startup timeout must be an integer" ;; esac

# Keep Unix-domain socket paths below macOS SUN_LEN even when TMPDIR expands to
# a long per-login /var/folders path.
run_root="$(mktemp -d "${BLOOM_MA03_TMPDIR:-/tmp}/bloom-ma03.XXXXXX")"
developer_root="${run_root}/developer"
machine_home="${run_root}/machine-home"
mount_dir="${run_root}/mount"
log_dir="${run_root}/logs"
machine_socket="${run_root}/run/machine.sock"
ready_file="${run_root}/run/ready"
launcher_log="${run_root}/launcher.log"
launcher_pid=""
mkdir -p "$machine_home" "$mount_dir" "$log_dir" "$(dirname "$machine_socket")"

cleanup() {
  status=$?
  trap - EXIT INT TERM
  stop_stack || true
  if [ "$status" -eq 0 ]; then
    rm -rf -- "$run_root"
  else
    printf 'MA-03 diagnostics retained at: %s\n' "$run_root" >&2
  fi
  exit "$status"
}
trap cleanup EXIT INT TERM

stop_stack() {
  if [ -n "$launcher_pid" ] && kill -0 "$launcher_pid" 2>/dev/null; then
    kill "$launcher_pid" 2>/dev/null || true
    wait "$launcher_pid" 2>/dev/null || true
  fi
  launcher_pid=""
  rm -f -- "$ready_file"
  attempts=0
  while mount | grep -F " on ${mount_dir} " >/dev/null 2>&1; do
    attempts=$((attempts + 1))
    [ "$attempts" -lt 100 ] || die "Machine mount remained active after shutdown"
    sleep 0.1
  done
}

start_stack() {
  : > "$launcher_log"
  "$launcher" \
    --developer-root "$developer_root" \
    --machine-home "$machine_home" \
    --mount "$mount_dir" \
    --machine-socket "$machine_socket" \
    --log-dir "$log_dir" \
    --ready-file "$ready_file" >"$launcher_log" 2>&1 &
  launcher_pid=$!
  deadline=$(( $(date +%s) + startup_timeout_secs ))
  while [ ! -f "$ready_file" ]; do
    kill -0 "$launcher_pid" 2>/dev/null || {
      cat "$launcher_log" >&2
      die "triad developer stack exited during startup"
    }
    [ "$(date +%s)" -lt "$deadline" ] || {
      cat "$launcher_log" >&2
      die "triad developer stack did not become ready"
    }
    sleep 0.1
  done
  # shellcheck disable=SC1090
  source "${log_dir}/triad.env"
  bloom_bin="${repo_root}/target/debug/bloom"
  driver_bin="${broker_repo}/target/debug/bloom-broker-debug-driver"
  [ -x "$bloom_bin" ] && [ -x "$driver_bin" ] || die "integration binaries are missing"
}

cli() {
  "$bloom_bin" --home "$machine_home" "$@"
}

mounted() {
  case "$1" in
    /*) printf '%s%s\n' "$mount_dir" "$1" ;;
    *) die "internal VFS path is not absolute: $1" ;;
  esac
}

complete_launch() {
  launch_output="$1"
  seed="$2"
  shift 2
  ceremony_url="$(printf '%s\n' "$launch_output" | sed -n 's/^ceremony_url: //p')"
  [ -n "$ceremony_url" ] || die "custody launch omitted ceremony_url"
  "$driver_bin" complete "$ceremony_url" "$seed" "$@"
}

assert_projection_pair() {
  wallet_id="$1"
  label="$2"
  attempts=0
  vfs_projection=""
  while [ "$attempts" -lt 100 ]; do
    vfs_projection="$(cat "$(mounted "/wallets/${wallet_id}/projection.json")" 2>/dev/null || true)"
    if printf '%s' "$vfs_projection" | jq -e . >/dev/null 2>&1; then
      break
    fi
    attempts=$((attempts + 1))
    sleep 0.05
  done
  # Observe through the long-lived Machine first. A separate CLI process uses
  # the same atomic cache file, so putting it second avoids turning the test
  # itself into an external-writer race against the mounted reader.
  cli_projection="$(cli wallet projection "$wallet_id")"
  for projection in "$cli_projection" "$vfs_projection"; do
    printf '%s' "$projection" | jq -e --arg wallet "$wallet_id" '
      .wallet.wallet_id == $wallet and
      .source_protocol == "bloom.machine-broker.v1" and
      .verification == "authenticated_broker" and
      (.keys | type == "array") and (.credentials | type == "array")
    ' >/dev/null || die "${label}: invalid public projection"
  done
  cli_normalized="$(printf '%s' "$cli_projection" | jq -cS 'del(.observed_at_ms, .freshness)')"
  vfs_normalized="$(printf '%s' "$vfs_projection" | jq -cS 'del(.observed_at_ms, .freshness)')"
  [ "$cli_normalized" = "$vfs_normalized" ] ||
    die "${label}: CLI and mounted VFS disagree"
  printf '%s\n' "$cli_projection"
}

assert_no_legacy_record() {
  for wallet_id in "$@"; do
    [ ! -e "${machine_home}/keystore/${wallet_id}" ] ||
      die "Machine created a legacy keystore record for ${wallet_id}"
  done
  [ ! -e "${machine_home}/auth/auth.sqlite" ] || die "Machine created legacy auth.sqlite"
  [ ! -e "${machine_home}/signer-cache" ] || die "Machine created a legacy signer cache"
}

wait_for_fixture_record() {
  request_id="$1"
  attempts=0
  while [ "$attempts" -lt 200 ]; do
    while IFS= read -r record_name; do
      [ -n "$record_name" ] || continue
      record_path="$(mounted "/petal-key-requests/${record_name}")"
      record="$(cat "$record_path" 2>/dev/null || true)"
      if printf '%s' "$record" | jq -e --arg request_id "$request_id" '
        .request_id == $request_id and .status == "awaiting_user" and
        (.ceremony_url | type == "string")
      ' >/dev/null 2>&1; then
        printf '%s\n' "$record"
        return 0
      fi
    done < <(LC_ALL=C command ls -1 "$(mounted /petal-key-requests)" 2>/dev/null || true)
    attempts=$((attempts + 1))
    sleep 0.05
  done
  die "Petal key derivation ceremony did not appear through the mounted VFS"
}

printf 'Building deterministic ceremony driver...\n'
(cd "$broker_repo" && cargo build -p bloom-broker-debug-driver)
start_stack

# The frozen protocol contains credential add/remove prepare variants for
# future consumers, but Machine currently retains exactly one credential-change
# surface: `wallet rebind-passkey` (credential replacement).  Prove that from
# the actual user-visible CLI and mounted namespace instead of inventing a new
# Machine command merely to exercise otherwise-unexposed protocol variants.
wallet_help="$(cli wallet --help)"
credential_commands="$(printf '%s\n' "$wallet_help" |
  sed -n 's/^  \([a-z][a-z-]*\)  *.*/\1/p' |
  grep -E '(credential|passkey|authenticator)' || true)"
[ "$credential_commands" = "rebind-passkey" ] ||
  die "credential-change CLI inventory is not exactly rebind-passkey: ${credential_commands:-<none>}"

printf 'MA-03: registering wallet through Broker/Signer...\n'
registration_launch="$(cli wallet new ma03-registration)"
registration_result="$(complete_launch "$registration_launch" registration-auth --sign-count 1)"
registered_wallet="$(printf '%s' "$registration_result" | jq -er '.wallet_id')"
registered_projection="$(assert_projection_pair "$registered_wallet" registration)"
printf '%s' "$registered_projection" | jq -e '
  (.credentials | length) == 1 and (.keys | length) == 1
' >/dev/null || die "registration projection omitted public authority descriptors"
original_credential="$(printf '%s' "$registered_projection" | jq -er '.credentials[0].credential_id')"
wallet_entries="$(LC_ALL=C command ls -1 "$(mounted "/wallets/${registered_wallet}")")"
if printf '%s\n' "$wallet_entries" | grep -Eiq '(credential|passkey|authenticator|rebind)'; then
  die "unexpected mounted credential mutation surface is exposed"
fi

printf 'MA-03: importing wallet through Broker/Signer...\n'
import_launch="$(cli wallet import ma03-import)"
import_result="$(complete_launch "$import_launch" import-auth \
  --sign-count 1 --raw-private-key ERERERERERERERERERERERERERERERERERERERERERE)"
imported_wallet="$(printf '%s' "$import_result" | jq -er '.wallet_id')"
[ "$imported_wallet" != "$registered_wallet" ] || die "registration and import returned one wallet"
assert_projection_pair "$imported_wallet" import >/dev/null
assert_no_legacy_record "$registered_wallet" "$imported_wallet"

printf 'MA-03: replacing the registered wallet credential...\n'
rebind_launch="$(cli wallet rebind-passkey "$registered_wallet")"
complete_launch "$rebind_launch" registration-auth --sign-count 2 \
  --new-authenticator-seed replacement-auth >/dev/null
rebound_projection="$(assert_projection_pair "$registered_wallet" credential-replace)"
replacement_credential="$(printf '%s' "$rebound_projection" | jq -er '.credentials[0].credential_id')"
[ "$replacement_credential" != "$original_credential" ] ||
  die "credential replacement did not change the public credential descriptor"
printf '%s' "$rebound_projection" | jq -e '(.credentials | length) == 1' >/dev/null ||
  die "credential replacement left an unexpected credential set"
printf '%s' "$rebound_projection" | jq -e \
  --arg original "$original_credential" --arg replacement "$replacement_credential" '
    (.credentials | length) == 1 and
    .credentials[0].credential_id == $replacement and
    all(.credentials[]; .credential_id != $original)
  ' >/dev/null || die "CLI and mounted projection did not expose only the replacement credential"

printf 'MA-03: committing a policy update through its completed ceremony receipt...\n'
fixture_hash="$(jq -er '.records[] | select(.subject.kind == "petal" and .subject.route == "r000001") | .subject.package_hash' \
  "${developer_root}/config/provenance-catalog.json" | head -n 1)"
current_policy="$(cat "$(mounted "/wallets/${registered_wallet}/policy.json")")"
old_policy_version="$(printf '%s' "$rebound_projection" | jq -er '.policy.version | tonumber')"
policy_file="${run_root}/proposed-policy.json"
printf '%s' "$current_policy" | jq -cS --arg package_hash "$fixture_hash" '
  .allowed_petal_packages = ((.allowed_petal_packages + [$package_hash]) | unique | sort)
' > "$policy_file"
policy_launch="$(cli wallet update-policy "$registered_wallet" --file "$policy_file")"
complete_launch "$policy_launch" replacement-auth --sign-count 2 >/dev/null
policy_operation="$(printf '%s\n' "$policy_launch" | sed -n 's/^operation_id: //p')"
cli wallet commit-policy "$policy_operation" >/dev/null
policy_projection="$(assert_projection_pair "$registered_wallet" policy-update)"
new_policy_version="$(printf '%s' "$policy_projection" | jq -er '.policy.version | tonumber')"
[ "$new_policy_version" -eq $((old_policy_version + 1)) ] ||
  die "policy update did not advance the signed projection version exactly once"
printf '%s' "$policy_projection" | jq -e --arg package_hash "$fixture_hash" '
  (.policy.canonical_policy | type == "string") and
  (.wallet.policy_digest == .policy.policy_digest)
' >/dev/null || die "policy projection is internally inconsistent"
mounted_policy="$(cat "$(mounted "/wallets/${registered_wallet}/policy.json")")"
printf '%s' "$mounted_policy" | jq -e --arg package_hash "$fixture_hash" '
  .allowed_petal_packages | index($package_hash) != null
' >/dev/null || die "mounted policy did not expose the committed authority change"

printf 'MA-03: deriving a Signer-owned Petal key from a mounted Petal request...\n'
request_id="ma03-key-derive-$$"
fixture_request="$(jq -nc \
  --arg request_id "$request_id" --arg wallet_id "$registered_wallet" \
  '{request_id:$request_id,wallet_id:$wallet_id,purpose:"fixture.payload",
    maximum_lifetime_ms:900000,preimage_hex:"6d613033",
    nonce_hex:"11111111111111111111111111111111",approval_hint:null}')"
printf '%s\n' "$fixture_request" > "$(mounted /petals/triad-authority-fixture/session.json)" 2>/dev/null || true
key_record="$(wait_for_fixture_record "$request_id")"
key_ceremony_url="$(printf '%s' "$key_record" | jq -er '.ceremony_url')"
before_key_count="$(printf '%s' "$policy_projection" | jq -er '.keys | length')"
"$driver_bin" complete "$key_ceremony_url" replacement-auth --sign-count 3 >/dev/null
derived_projection="$(assert_projection_pair "$registered_wallet" key-derive)"
after_key_count="$(printf '%s' "$derived_projection" | jq -er '.keys | length')"
[ "$after_key_count" -eq $((before_key_count + 1)) ] ||
  die "derived key did not appear in the public wallet projection"
printf '%s' "$derived_projection" | jq -e '
  any(.keys[]; .key_ref.derivation != null)
' >/dev/null || die "derived key projection omitted public derivation metadata"
printf '%s' "$derived_projection" | jq -e --arg replacement "$replacement_credential" '
  (.credentials | length) == 1 and
  .credentials[0].credential_id == $replacement
' >/dev/null || die "later Broker projection refresh lost the replacement credential"

printf 'MA-03: deleting the imported wallet through Broker/Signer...\n'
delete_launch="$(cli wallet delete "$imported_wallet")"
complete_launch "$delete_launch" import-auth --sign-count 2 >/dev/null
wallet_list="$(cli wallet list)"
printf '%s\n' "$wallet_list" | grep -F "$registered_wallet" >/dev/null ||
  die "retained wallet disappeared after another wallet deletion"
if printf '%s\n' "$wallet_list" | grep -F "$imported_wallet" >/dev/null; then
  die "deleted wallet remained in CLI projection discovery"
fi
[ ! -e "$(mounted "/wallets/${imported_wallet}")" ] ||
  die "deleted wallet remained in mounted VFS discovery"
assert_no_legacy_record "$registered_wallet" "$imported_wallet"

before_restart="$(printf '%s' "$derived_projection" | jq -cS 'del(.observed_at_ms, .freshness)')"
printf 'MA-03: restarting the out-of-process stack over the same authoritative state...\n'
stop_stack
start_stack
after_restart_projection="$(assert_projection_pair "$registered_wallet" restart)"
after_restart="$(printf '%s' "$after_restart_projection" | jq -cS 'del(.observed_at_ms, .freshness)')"
[ "$before_restart" = "$after_restart" ] ||
  die "retained Broker projection changed across Machine restart"
printf '%s' "$after_restart_projection" | jq -e \
  --arg original "$original_credential" --arg replacement "$replacement_credential" '
    (.credentials | length) == 1 and
    .credentials[0].credential_id == $replacement and
    all(.credentials[]; .credential_id != $original)
  ' >/dev/null ||
  die "replacement credential was not preserved solely through Broker projection across restart"
restart_list="$(cli wallet list)"
printf '%s\n' "$restart_list" | grep -F "$registered_wallet" >/dev/null ||
  die "retained wallet did not survive Machine restart"
if printf '%s\n' "$restart_list" | grep -F "$imported_wallet" >/dev/null; then
  die "deleted wallet resurrected across Machine restart"
fi
[ ! -e "$(mounted "/wallets/${imported_wallet}")" ] ||
  die "deleted wallet resurrected in the mounted VFS across restart"
assert_no_legacy_record "$registered_wallet" "$imported_wallet"

printf 'MA-03 projection fidelity passed: the sole retained credential-change surface (replacement), registration, import, policy update, Petal key derivation, deletion, and restart matched through CLI and mounted VFS; no credential add/remove Machine surface exists.\n'
