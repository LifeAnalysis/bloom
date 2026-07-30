#!/bin/bash
set -Eeuo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
main_root="$(cd "$script_dir/../../../.." && pwd)"
workspace_root="$(dirname "$main_root")"
broker_root="${BLOOM_TART_BROKER_ROOT:-$workspace_root/bloom-broker}"
signer_root="${BLOOM_TART_SIGNER_ROOT:-$workspace_root/bloom-signer}"
development_base="${BLOOM_TART_DEVELOPMENT_BASE:-bloom-macos-w0-dev-base}"
guest_password="${BLOOM_TART_GUEST_PASSWORD:-admin}"
keep_failed="${BLOOM_TART_KEEP_FAILED:-false}"
run_id="$(date -u +%Y%m%dT%H%M%SZ)-$$"
run_name="bloom-macos-w0-run-$run_id"
local_output_root="${BLOOM_TART_OUTPUT_ROOT:-$workspace_root/.w0-local/runs/$run_id}"
build_vm_log="$local_output_root/build-vm.log"
run_vm_log="$local_output_root/run-vm.log"
build_log="$local_output_root/build.log"
w0_log="$local_output_root/w0.log"

for command_name in tart jq sshpass ssh nc; do
  command -v "$command_name" >/dev/null 2>&1 || {
    echo "missing local Tart W0 dependency: $command_name" >&2
    exit 69
  }
done

for repository_root in "$main_root" "$broker_root" "$signer_root"; do
  [[ -d "$repository_root/.git" ]] || {
    echo "missing local Bloom repository: $repository_root" >&2
    exit 69
  }
done

vm_exists() {
  local vm_name="$1"
  tart list --format json |
    jq -e --arg name "$vm_name" \
      'any(.[]; .Source == "local" and .Name == $name)' >/dev/null
}

vm_running() {
  local vm_name="$1"
  tart list --format json |
    jq -e --arg name "$vm_name" \
      'any(.[]; .Source == "local" and .Name == $name and .Running)' >/dev/null
}

if ! vm_exists "$development_base"; then
  echo "missing local Tart W0 development base: $development_base" >&2
  echo "run $script_dir/provision-tart-local.sh first" >&2
  exit 69
fi
if vm_running "$development_base"; then
  echo "local Tart W0 development base is already running: $development_base" >&2
  exit 69
fi

mkdir -p "$local_output_root"

ssh_options=(
  -o StrictHostKeyChecking=no
  -o UserKnownHostsFile=/dev/null
  -o ConnectTimeout=10
  -o PreferredAuthentications=password
  -o PubkeyAuthentication=no
  -o IdentitiesOnly=yes
  -o NumberOfPasswordPrompts=1
  -o ServerAliveInterval=15
  -o ServerAliveCountMax=4
)

active_vm=""
run_pid=""
completed=false

stop_active_vm() {
  if [[ -n "$active_vm" ]] && vm_running "$active_vm"; then
    tart stop "$active_vm" >/dev/null 2>&1 || true
  fi
  if [[ -n "$run_pid" ]]; then
    wait "$run_pid" >/dev/null 2>&1 || true
  fi
  run_pid=""
  active_vm=""
}

cleanup() {
  local status=$?
  trap - EXIT
  stop_active_vm
  if vm_exists "$run_name"; then
    if [[ "$status" -eq 0 || "$keep_failed" != true ]]; then
      tart delete "$run_name" >/dev/null 2>&1 || true
    else
      echo "preserved failed disposable VM: $run_name" >&2
    fi
  fi
  if [[ "$completed" == true ]]; then
    echo "local macOS W0 passed; evidence: $local_output_root"
  else
    echo "local macOS W0 failed; diagnostics: $local_output_root" >&2
  fi
  exit "$status"
}
trap cleanup EXIT

start_vm() {
  local vm_name="$1"
  local log_path="$2"
  active_vm="$vm_name"
  tart run \
    --no-graphics \
    --no-audio \
    --no-clipboard \
    --dir="bloom:$main_root:ro" \
    --dir="bloom-broker:$broker_root:ro" \
    --dir="bloom-signer:$signer_root:ro" \
    --dir="output:$local_output_root" \
    "$vm_name" >"$log_path" 2>&1 &
  run_pid=$!

  guest_ip=""
  for _ in {1..90}; do
    guest_ip="$(tart ip "$vm_name" 2>/dev/null || true)"
    if [[ -n "$guest_ip" ]] &&
      nc -z -w 1 "$guest_ip" 22 >/dev/null 2>&1 &&
      sshpass -p "$guest_password" \
        ssh "${ssh_options[@]}" "admin@$guest_ip" /usr/bin/true \
        >/dev/null 2>&1
    then
      return 0
    fi
    sleep 2
  done
  echo "Tart VM did not expose SSH: $vm_name" >&2
  return 1
}

run_guest() {
  local guest_ip="$1"
  local guest_script="$2"
  local log_path="$3"
  sshpass -p "$guest_password" \
    ssh "${ssh_options[@]}" "admin@$guest_ip" \
    /bin/bash -s <"$guest_script" 2>&1 |
    tee "$log_path"
}

echo "building W0 candidate in local Tart base $development_base"
start_vm "$development_base" "$build_vm_log"
builder_ip="$guest_ip"
run_guest \
  "$builder_ip" \
  "$script_dir/tart-build-guest.sh" \
  "$build_log"
stop_active_vm

echo "creating disposable local macOS W0 clone $run_name"
tart clone "$development_base" "$run_name"
tart set "$run_name" --random-mac
start_vm "$run_name" "$run_vm_log"
runner_ip="$guest_ip"
run_guest \
  "$runner_ip" \
  "$script_dir/tart-run-guest.sh" \
  "$w0_log"

completed=true
