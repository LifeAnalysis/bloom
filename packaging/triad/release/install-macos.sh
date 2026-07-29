#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat >&2 <<'EOF'
usage:
  install-macos.sh install ROOT LOGIN_UID LOGIN_USER PAYLOAD_DIR
  install-macos.sh rotate-config ROOT LOGIN_UID PRINCIPAL CONFIG_JSON
  install-macos.sh uninstall ROOT LOGIN_UID CONFIRM_TOKEN
EOF
  exit 64
}

[[ $# -ge 1 ]] || usage
action="$1"
shift
triad_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"

validate() {
  root="$1"
  login_uid="$2"
  [[ -d "$root" && "$login_uid" =~ ^[1-9][0-9]*$ ]] || usage
  root="$(cd "$root" && pwd -P)"
}

atomic_install() {
  source_file="$1"
  destination="$2"
  mode="$3"
  mkdir -p "$(dirname "$destination")"
  temporary="${destination}.new.$$"
  install -m "$mode" "$source_file" "$temporary"
  mv -f "$temporary" "$destination"
}

case "$action" in
  install)
    [[ $# -eq 4 ]] || usage
    validate "$1" "$2"
    login_user="$3"
    payload="$(cd "$4" && pwd -P)"
    platform_claim="$(<"$payload/PLATFORM_CLAIM")"
    if [[ ! ("$platform_claim" == "test-unclaimed" &&
      "${BLOOM_ALLOW_TEST_UNCLAIMED:-}" == "true") ]]
    then
      echo "this source installer has no production macOS platform claim" >&2
      exit 65
    fi
    [[ "$login_user" =~ ^[a-z_][a-z0-9_-]*$ ]] || usage
    for required in \
      bin/bloom \
      bin/bloom-broker \
      bin/bloom-signer \
      config/edge-manifest.json \
      config/broker.json \
      config/signer.json \
      config/broker-identity.json \
      config/signer-identity.json
    do
      test -f "$payload/$required" || {
        echo "payload is missing $required" >&2
        exit 66
      }
    done
    if [[ "$root" == "/" ]]; then
      launch_root="$root/Library/LaunchAgents"
      launchctl bootout \
        "gui/$login_uid" \
        "$launch_root/com.bloom.broker.$login_uid.plist" 2>/dev/null || true
      launchctl bootout \
        "gui/$login_uid" \
        "$launch_root/com.bloom.signer.$login_uid.plist" 2>/dev/null || true
    fi

    install_root="$root/Library/Application Support/BloomTriad"
    binary_root="$install_root/bin"
    state_root="$install_root/logins/$login_uid"
    launch_root="$root/Library/LaunchAgents"
    mkdir -p "$binary_root" "$state_root/broker" "$state_root/signer" "$launch_root"
    chmod 0700 "$state_root/broker" "$state_root/signer"
    for binary in bloom bloom-broker bloom-signer; do
      atomic_install "$payload/bin/$binary" "$binary_root/$binary" 0755
    done
    if [[ "$root" == "/" ]]; then
      for binary in bloom bloom-broker bloom-signer; do
        codesign --verify --strict "$binary_root/$binary"
      done
    fi
    atomic_install "$payload/config/edge-manifest.json" "$state_root/edge-manifest.json" 0600
    atomic_install "$payload/config/broker.json" "$state_root/broker/config.json" 0600
    atomic_install "$payload/config/signer.json" "$state_root/signer/config.json" 0600
    atomic_install \
      "$payload/config/broker-identity.json" \
      "$state_root/broker/identity.json" \
      0600
    atomic_install \
      "$payload/config/signer-identity.json" \
      "$state_root/signer/identity.json" \
      0600
    mkdir -p \
      "$state_root/broker/audit-checkpoints" \
      "$state_root/signer/audit-checkpoints" \
      "$state_root/sockets"
    chmod 0700 \
      "$state_root/broker/audit-checkpoints" \
      "$state_root/signer/audit-checkpoints" \
      "$state_root/sockets"

    broker_plist="$launch_root/com.bloom.broker.$login_uid.plist"
    signer_plist="$launch_root/com.bloom.signer.$login_uid.plist"
    sed \
      -e "s|@BLOOM_BROKER_BINARY@|$binary_root/bloom-broker|g" \
      -e "s|@BLOOM_BROKER_IDENTITY@|$state_root/broker/identity.json|g" \
      -e "s|@BLOOM_BROKER_CONFIG@|$state_root/broker/config.json|g" \
      -e "s|@BLOOM_EDGE_MANIFEST@|$state_root/edge-manifest.json|g" \
      -e "s|@BLOOM_BROKER_SOCKET@|$state_root/sockets/broker.sock|g" \
      -e "s|@BLOOM_BROKER_CONTROL_SOCKET@|$state_root/sockets/broker-control.sock|g" \
      -e "s|@BLOOM_BROKER_LOG@|$state_root/broker/broker.log|g" \
      -e "s|@BLOOM_BROKER_AUDIT_CHECKPOINT_DIR@|$state_root/broker/audit-checkpoints|g" \
      "$triad_root/macos/launchagents/com.bloom.broker.plist.in" > "$broker_plist.new"
    chmod 0600 "$broker_plist.new"
    mv -f "$broker_plist.new" "$broker_plist"
    sed \
      -e "s|@BLOOM_SIGNER_BINARY@|$binary_root/bloom-signer|g" \
      -e "s|@BLOOM_SIGNER_IDENTITY@|$state_root/signer/identity.json|g" \
      -e "s|@BLOOM_SIGNER_CONFIG@|$state_root/signer/config.json|g" \
      -e "s|@BLOOM_EDGE_MANIFEST@|$state_root/edge-manifest.json|g" \
      -e "s|@BLOOM_SIGNER_SOCKET@|$state_root/sockets/signer.sock|g" \
      -e "s|@BLOOM_SIGNER_CONTROL_SOCKET@|$state_root/sockets/signer-control.sock|g" \
      -e "s|@BLOOM_SIGNER_LOG@|$state_root/signer/signer.log|g" \
      -e "s|@BLOOM_SIGNER_AUDIT_CHECKPOINT_DIR@|$state_root/signer/audit-checkpoints|g" \
      "$triad_root/macos/launchagents/com.bloom.signer.plist.in" > "$signer_plist.new"
    chmod 0600 "$signer_plist.new"
    mv -f "$signer_plist.new" "$signer_plist"
    if [[ "$root" == "/" ]]; then
      chown -R "$login_user" "$state_root" "$broker_plist" "$signer_plist"
      launchctl bootstrap "gui/$login_uid" "$signer_plist"
      launchctl bootstrap "gui/$login_uid" "$broker_plist"
    fi
    ;;
  rotate-config)
    [[ $# -eq 4 ]] || usage
    validate "$1" "$2"
    principal="$3"
    replacement="$4"
    [[ "$principal" == "broker" || "$principal" == "signer" ]] || usage
    destination="$root/Library/Application Support/BloomTriad/logins/$login_uid/$principal/config.json"
    test -d "$(dirname "$destination")" || {
      echo "principal is not installed" >&2
      exit 66
    }
    atomic_install "$replacement" "$destination" 0600
    if [[ "$root" == "/" ]]; then
      launchctl kickstart -k "gui/$login_uid/com.bloom.$principal"
    fi
    ;;
  uninstall)
    [[ $# -eq 3 ]] || usage
    validate "$1" "$2"
    expected="delete-bloom-login-$login_uid"
    [[ "$3" == "$expected" ]] || {
      echo "uninstall confirmation must equal $expected" >&2
      exit 64
    }
    launch_root="$root/Library/LaunchAgents"
    if [[ "$root" == "/" ]]; then
      launchctl bootout "gui/$login_uid" "$launch_root/com.bloom.broker.$login_uid.plist" || true
      launchctl bootout "gui/$login_uid" "$launch_root/com.bloom.signer.$login_uid.plist" || true
    fi
    rm -f -- \
      "$launch_root/com.bloom.broker.$login_uid.plist" \
      "$launch_root/com.bloom.signer.$login_uid.plist"
    state_target="$root/Library/Application Support/BloomTriad/logins/$login_uid"
    rm -rf -- "$state_target"
    ;;
  *)
    usage
    ;;
esac
