#!/bin/bash
set -Eeuo pipefail

readonly upstream_image="${BLOOM_TART_UPSTREAM_IMAGE:-ghcr.io/cirruslabs/macos-tahoe-base:latest}"
readonly upstream_base="${BLOOM_TART_UPSTREAM_BASE:-bloom-macos-w0-base}"
readonly development_base="${BLOOM_TART_DEVELOPMENT_BASE:-bloom-macos-w0-dev-base}"
readonly guest_password="${BLOOM_TART_GUEST_PASSWORD:-admin}"
readonly tart_cpu="${BLOOM_TART_CPU:-8}"
readonly tart_memory_mb="${BLOOM_TART_MEMORY_MB:-16384}"

if [[ "$(uname -s)" != "Darwin" || "$(uname -m)" != "arm64" ]]; then
  echo "local Tart W0 provisioning requires Apple Silicon macOS" >&2
  exit 69
fi

for command_name in tart jq sshpass ssh nc; do
  command -v "$command_name" >/dev/null 2>&1 || {
    echo "missing local Tart W0 dependency: $command_name" >&2
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

if ! vm_exists "$upstream_base"; then
  echo "pulling immutable Tart macOS base $upstream_image"
  tart clone "$upstream_image" "$upstream_base"
fi

if vm_exists "$development_base"; then
  echo "local Tart W0 development base already exists: $development_base"
  exit 0
fi

echo "creating local Tart W0 development base $development_base"
tart clone "$upstream_base" "$development_base"
tart set \
  "$development_base" \
  --cpu "$tart_cpu" \
  --memory "$tart_memory_mb" \
  --random-mac

run_log="$(mktemp "${TMPDIR:-/tmp}/bloom-tart-provision.XXXXXX.log")"
run_pid=""
provisioned=false

cleanup() {
  local status=$?
  trap - EXIT
  if vm_running "$development_base"; then
    tart stop "$development_base" >/dev/null 2>&1 || true
  fi
  if [[ -n "$run_pid" ]]; then
    wait "$run_pid" >/dev/null 2>&1 || true
  fi
  if [[ "$provisioned" != true ]]; then
    echo "Tart W0 base provisioning failed; VM log: $run_log" >&2
    if vm_exists "$development_base"; then
      tart delete "$development_base" >/dev/null 2>&1 || true
    fi
  fi
  exit "$status"
}
trap cleanup EXIT

tart run \
  --no-graphics \
  --no-audio \
  --no-clipboard \
  "$development_base" >"$run_log" 2>&1 &
run_pid=$!

guest_ip=""
for _ in {1..90}; do
  guest_ip="$(tart ip "$development_base" 2>/dev/null || true)"
  if [[ -n "$guest_ip" ]] && nc -z -w 1 "$guest_ip" 22 >/dev/null 2>&1; then
    break
  fi
  sleep 2
done
if [[ -z "$guest_ip" ]]; then
  echo "Tart W0 development base did not expose SSH" >&2
  exit 1
fi

ssh_options=(
  -o StrictHostKeyChecking=no
  -o UserKnownHostsFile=/dev/null
  -o ConnectTimeout=10
  -o PreferredAuthentications=password
  -o PubkeyAuthentication=no
  -o IdentitiesOnly=yes
)

sshpass -p "$guest_password" \
  ssh "${ssh_options[@]}" "admin@$guest_ip" /bin/bash -s <<'GUEST'
set -Eeuo pipefail
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin"
/usr/bin/sudo -n /usr/bin/true
/bin/launchctl print "gui/$(/usr/bin/id -u)" >/dev/null
/usr/bin/xcrun --find clang >/dev/null
if ! command -v cargo >/dev/null 2>&1; then
  /usr/bin/curl \
    --proto "=https" \
    --tlsv1.2 \
    -sSf \
    https://sh.rustup.rs |
    /bin/sh -s -- -y --profile minimal --default-toolchain stable
fi
cargo --version
rustc --version
GUEST

tart stop "$development_base"
wait "$run_pid"
run_pid=""
provisioned=true
echo "local Tart W0 development base is ready: $development_base"
