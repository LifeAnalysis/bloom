#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
broker_repo="$(cd "${repo_root}/../bloom-broker" && pwd -P)"
signer_repo="$(cd "${repo_root}/../bloom-signer" && pwd -P)"
developer_root=""
machine_home=""
mount_dir=""
machine_socket=""
log_dir=""
ready_file=""

die() { printf 'triad developer launcher: %s\n' "$*" >&2; exit 1; }
need_value() { [ "$#" -ge 2 ] || die "$1 requires a value"; }
while [ "$#" -gt 0 ]; do
  case "$1" in
    --developer-root) need_value "$@"; developer_root="$2"; shift 2 ;;
    --machine-home) need_value "$@"; machine_home="$2"; shift 2 ;;
    --mount) need_value "$@"; mount_dir="$2"; shift 2 ;;
    --machine-socket) need_value "$@"; machine_socket="$2"; shift 2 ;;
    --log-dir) need_value "$@"; log_dir="$2"; shift 2 ;;
    --ready-file) need_value "$@"; ready_file="$2"; shift 2 ;;
    *) die "unknown argument: $1" ;;
  esac
done
for value in "$developer_root" "$machine_home" "$mount_dir" "$machine_socket" "$log_dir" "$ready_file"; do
  [ -n "$value" ] || die "all launcher paths are required"
done

command -v jq >/dev/null 2>&1 || die "jq is required"
umask 077
mkdir -p "$developer_root"
chmod 0700 "$developer_root"
developer_root="$(cd "$developer_root" && pwd -P)"
mkdir -p "$machine_home" "$mount_dir" "$log_dir" \
  "$(dirname "$machine_socket")" "$(dirname "$ready_file")"
machine_home="$(cd "$machine_home" && pwd -P)"
mount_dir="$(cd "$mount_dir" && pwd -P)"
log_dir="$(cd "$log_dir" && pwd -P)"
machine_socket="$(cd "$(dirname "$machine_socket")" && pwd -P)/$(basename "$machine_socket")"
ready_file="$(cd "$(dirname "$ready_file")" && pwd -P)/$(basename "$ready_file")"

bloom_bin="${BLOOM_INTEGRATION_MACHINE_BIN:-${repo_root}/target/debug/bloom}"
broker_bin="${BLOOM_INTEGRATION_BROKER_BIN:-${broker_repo}/target/debug/bloom-broker}"
signer_bin="${BLOOM_INTEGRATION_SIGNER_BIN:-${signer_repo}/target/debug/bloom-signer}"
if [ -z "${BLOOM_INTEGRATION_MACHINE_BIN:-}" ]; then
  (cd "$repo_root" && cargo build -p bloom --no-default-features --features mount,triad-dev-harness)
fi
if [ -z "${BLOOM_INTEGRATION_BROKER_BIN:-}" ]; then
  (cd "$broker_repo" && cargo build -p bloom-broker --features triad-dev-harness)
fi
if [ -z "${BLOOM_INTEGRATION_SIGNER_BIN:-}" ]; then
  (cd "$signer_repo" && cargo build -p bloom-signer --features triad-dev-harness)
fi
for binary in "$bloom_bin" "$broker_bin" "$signer_bin"; do
  [ -x "$binary" ] || die "required binary is not executable: $binary"
done

release_digest="$(
  shasum -a 256 "$bloom_bin" "$broker_bin" "$signer_bin" |
    awk '{print $1}' | shasum -a 256 | awk '{print $1}'
)"
config_dir="${developer_root}/config"
fixture_root="${repo_root}/tests/fixtures/triad-authority-petal"
fixture_hash="$($bloom_bin --home "${developer_root}/package-scan" petals build "$fixture_root" |
  sed -n 's/^hash: //p')"
case "$fixture_hash" in
  [0-9a-f][0-9a-f]*) [ "${#fixture_hash}" -eq 64 ] || die "fixture package hash is malformed" ;;
  *) die "could not determine fixture package hash" ;;
esac
if [ ! -f "${config_dir}/edge-manifest.json" ]; then
  [ ! -e "$config_dir" ] || die "incomplete developer config already exists: $config_dir"
  template_dir="$(mktemp -d "${developer_root}/templates.XXXXXX")"
  chmod 0700 "$template_dir"
  for name in edge-manifest.json.in broker.json.in signer.json.in provenance-catalog.unsigned.json; do
    cp "${repo_root}/packaging/triad/macos/config/${name}" "${template_dir}/${name}"
    chmod 0600 "${template_dir}/${name}"
  done
  catalog="${template_dir}/provenance-catalog.unsigned.json"
  catalog_new="${catalog}.new"
  jq --arg package_hash "$fixture_hash" '.records += [{
    subject: {kind:"petal", package_hash:$package_hash, route:"r000001"},
    publisher:"bloom-developer-fixture",
    operation_classes:[{operation_class:"fixture.payload", fee_asset:null}],
    installer_key_id:"unsigned-template",
    installer_signature:""
  }]' "$catalog" > "$catalog_new"
  chmod 0600 "$catalog_new"
  mv -f "$catalog_new" "$catalog"
  mkdir "$config_dir"
  chmod 0700 "$config_dir"
  "$bloom_bin" --triad-render-developer-enrollment \
    "$template_dir" "$config_dir" "$release_digest"
  rm -rf -- "$template_dir"
fi

for name in edge-manifest.json machine-identity.json broker-identity.json \
  signer-identity.json session-identity.json broker.json signer.json \
  provenance-catalog.json
do
  [ -f "${config_dir}/${name}" ] || die "developer config is missing ${name}"
  [ ! -L "${config_dir}/${name}" ] || die "developer config contains a symlink: ${name}"
  chmod 0600 "${config_dir}/${name}"
done
jq -e --arg package_hash "$fixture_hash" '
  any(.records[]; .subject.kind == "petal" and
      .subject.package_hash == $package_hash and .subject.route == "r000001")
' "${config_dir}/provenance-catalog.json" >/dev/null ||
  die "developer enrollment predates the current fixture; create a fresh developer root"
mkdir -p "${developer_root}/state/broker" "${developer_root}/state/signer"
chmod 0700 "${developer_root}/state" \
  "${developer_root}/state/broker" "${developer_root}/state/signer"

runtime_dir="$(mktemp -d "${developer_root}/runtime.XXXXXX")"
runtime_dir="$(cd "$runtime_dir" && pwd -P)"
chmod 0700 "$runtime_dir"
for endpoint in session signer broker; do
  mkdir "${runtime_dir}/${endpoint}"
  chgrp "$(id -g)" "${runtime_dir}/${endpoint}"
  chmod 0710 "${runtime_dir}/${endpoint}"
done
session_socket="${runtime_dir}/session/session.sock"
signer_socket="${runtime_dir}/signer/signer.sock"
signer_control_socket="${runtime_dir}/signer/control.sock"
broker_socket="${runtime_dir}/broker/broker.sock"
broker_control_socket="${runtime_dir}/broker/control.sock"

rewrite_broker_config() {
  source="${config_dir}/broker.json"
  temporary="${source}.new.$$"
  jq --arg signer_socket "$signer_socket" --arg digest "$release_digest" \
    '.signer_socket_path = $signer_socket | .build_digest = $digest | .network_containment = null' \
    "$source" > "$temporary"
  chmod 0600 "$temporary"
  mv -f "$temporary" "$source"
}
rewrite_signer_config() {
  source="${config_dir}/signer.json"
  temporary="${source}.new.$$"
  jq --arg digest "$release_digest" \
    '.build_digest = $digest | .network_containment = null' "$source" > "$temporary"
  chmod 0600 "$temporary"
  mv -f "$temporary" "$source"
}
rewrite_broker_config
rewrite_signer_config

env_file="${log_dir}/triad.env"
{
  printf 'export BLOOM_TRIAD_DEVELOPER_ROOT=%q\n' "$developer_root"
  printf 'export BLOOM_TRIAD_DEVELOPER_RUNTIME=%q\n' "$runtime_dir"
  printf 'export BLOOM_HOME=%q\n' "$machine_home"
  printf 'export BLOOM_BROKER_SOCKET=%q\n' "$broker_socket"
  printf 'export BLOOM_MACHINE_IDENTITY=%q\n' "${config_dir}/machine-identity.json"
  printf 'export BLOOM_EDGE_MANIFEST=%q\n' "${config_dir}/edge-manifest.json"
  printf 'export BLOOM_PROVENANCE_CATALOG=%q\n' "${config_dir}/provenance-catalog.json"
} > "$env_file"
chmod 0600 "$env_file"
session_pid=""; signer_pid=""; broker_pid=""; machine_pid=""
cleanup() {
  status=$?
  trap - EXIT INT TERM
  for pid in "$machine_pid" "$broker_pid" "$signer_pid" "$session_pid"; do
    if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then kill "$pid" 2>/dev/null || true; fi
  done
  for pid in "$machine_pid" "$broker_pid" "$signer_pid" "$session_pid"; do
    if [ -n "$pid" ]; then wait "$pid" 2>/dev/null || true; fi
  done
  case "$runtime_dir" in
    "${developer_root}/runtime."*) rm -rf -- "$runtime_dir" ;;
    *) printf 'refusing to remove unexpected runtime: %s\n' "$runtime_dir" >&2 ;;
  esac
  exit "$status"
}
trap cleanup EXIT INT TERM

wait_for_socket() {
  path="$1"; pid="$2"; label="$3"
  attempts=0
  while [ ! -S "$path" ]; do
    kill -0 "$pid" 2>/dev/null || {
      tail -n 80 "${log_dir}/${label}.log" >&2 || true
      die "$label exited before publishing its socket"
    }
    attempts=$((attempts + 1))
    [ "$attempts" -lt 300 ] || die "$label did not publish its socket"
    sleep 0.1
  done
}

BLOOM_TRIAD_DEVELOPER_ROOT="$developer_root" \
BLOOM_TRIAD_DEVELOPER_RUNTIME="$runtime_dir" \
  "$bloom_bin" --session-sentinel >"${log_dir}/session.log" 2>&1 &
session_pid=$!
wait_for_socket "$session_socket" "$session_pid" session

BLOOM_TRIAD_DEVELOPER_ROOT="$developer_root" \
BLOOM_SIGNER_IDENTITY="${config_dir}/signer-identity.json" \
BLOOM_EDGE_MANIFEST="${config_dir}/edge-manifest.json" \
BLOOM_SIGNER_CONFIG="${config_dir}/signer.json" \
BLOOM_SIGNER_SOCKET="$signer_socket" \
BLOOM_SIGNER_CONTROL_SOCKET="$signer_control_socket" \
BLOOM_SESSION_SOCKET="$session_socket" \
  "$signer_bin" >"${log_dir}/signer.log" 2>&1 &
signer_pid=$!
wait_for_socket "$signer_socket" "$signer_pid" signer

BLOOM_TRIAD_DEVELOPER_ROOT="$developer_root" \
BLOOM_BROKER_IDENTITY="${config_dir}/broker-identity.json" \
BLOOM_EDGE_MANIFEST="${config_dir}/edge-manifest.json" \
BLOOM_BROKER_CONFIG="${config_dir}/broker.json" \
BLOOM_BROKER_SOCKET="$broker_socket" \
BLOOM_BROKER_CONTROL_SOCKET="$broker_control_socket" \
BLOOM_SESSION_SOCKET="$session_socket" \
  "$broker_bin" >"${log_dir}/broker.log" 2>&1 &
broker_pid=$!
wait_for_socket "$broker_socket" "$broker_pid" broker

BLOOM_TRIAD_DEVELOPER_ROOT="$developer_root" \
BLOOM_BROKER_SOCKET="$broker_socket" \
BLOOM_MACHINE_IDENTITY="${config_dir}/machine-identity.json" \
BLOOM_EDGE_MANIFEST="${config_dir}/edge-manifest.json" \
BLOOM_PROVENANCE_CATALOG="${config_dir}/provenance-catalog.json" \
  "$bloom_bin" --home "$machine_home" petals install "$fixture_root" \
  >"${log_dir}/fixture-install.log" 2>&1

# This harness exercises Polymarket plus the local authority fixture.  Keep the
# production Machine's ordinary provisioning path, but do not make readiness
# depend on downloading unrelated preinstalled Petals.
machine_config="${machine_home}/config.toml"
[ -f "$machine_config" ] || die "Machine did not create its configuration"
machine_config_new="${machine_config}.new.$$"
awk '
  $0 == "[petals]" { in_petals = 1; print; next }
  in_petals && $0 ~ /^preinstalled = \[/ {
    print "preinstalled = [\"polymarket\"]"
    skipping = 1
    next
  }
  skipping {
    if ($0 == "]") skipping = 0
    next
  }
  { print }
' "$machine_config" > "$machine_config_new"
chmod 0600 "$machine_config_new"
mv -f "$machine_config_new" "$machine_config"

BLOOM_TRIAD_DEVELOPER_ROOT="$developer_root" \
BLOOM_BROKER_SOCKET="$broker_socket" \
BLOOM_MACHINE_IDENTITY="${config_dir}/machine-identity.json" \
BLOOM_EDGE_MANIFEST="${config_dir}/edge-manifest.json" \
BLOOM_PROVENANCE_CATALOG="${config_dir}/provenance-catalog.json" \
  "$bloom_bin" --home "$machine_home" serve \
    --endpoint "unix:${machine_socket}" --mount "$mount_dir" \
    >"${log_dir}/machine.log" 2>&1 &
machine_pid=$!
wait_for_socket "$machine_socket" "$machine_pid" machine
mount_attempts=0
while ! mount | grep -F " on ${mount_dir} " >/dev/null 2>&1 || ! command ls "$mount_dir" >/dev/null 2>&1; do
  kill -0 "$machine_pid" 2>/dev/null || die "Machine exited before its kernel mount became ready"
  mount_attempts=$((mount_attempts + 1))
  [ "$mount_attempts" -lt 300 ] || die "Machine socket became ready but its kernel mount did not"
  sleep 0.1
done
printf 'ready\n' > "$ready_file"
wait "$machine_pid"
