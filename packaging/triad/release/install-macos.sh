#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat >&2 <<'EOF'
usage:
  install-macos.sh install ROOT LOGIN_UID LOGIN_USER PAYLOAD_DIR
  install-macos.sh rotate-config ROOT LOGIN_UID PRINCIPAL CONFIG_JSON
  install-macos.sh uninstall ROOT LOGIN_UID CONFIRM_TOKEN

Staged-root tests must supply:
  BLOOM_MACOS_BROKER_UID
  BLOOM_MACOS_SIGNER_UID
  BLOOM_MACOS_MACHINE_BROKER_GID
  BLOOM_MACOS_BROKER_SIGNER_GID
  BLOOM_MACOS_REVOKE_GID
  BLOOM_RELEASE_DIGEST
EOF
  exit 64
}

[[ $# -ge 1 ]] || usage
action="$1"
shift
source_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"

validate_root_uid() {
  root="$1"
  login_uid="$2"
  [[ -d "$root" ]] || {
    echo "installer root is not a directory" >&2
    exit 66
  }
  [[ "$login_uid" =~ ^[1-9][0-9]*$ ]] || {
    echo "LOGIN_UID must be a positive decimal UID" >&2
    exit 64
  }
  root="$(cd "$root" && pwd -P)"
}

require_decimal_id() {
  name="$1"
  value="${!name:-}"
  [[ "$value" =~ ^[1-9][0-9]*$ ]] && ((value <= 4294967295)) || {
    echo "$name must be a positive 32-bit decimal ID" >&2
    exit 64
  }
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

render_template() {
  source_file="$1"
  destination="$2"
  mode="$3"
  mkdir -p "$(dirname "$destination")"
  temporary="${destination}.new.$$"
  sed \
    -e "s|@LOGIN_UID@|$login_uid|g" \
    -e "s|@LOGIN_USER@|$login_user|g" \
    -e "s|@BLOOM_BROKER_USER@|$broker_user|g" \
    -e "s|@BLOOM_BROKER_GROUP@|$broker_group|g" \
    -e "s|@BLOOM_SIGNER_USER@|$signer_user|g" \
    -e "s|@BLOOM_SIGNER_GROUP@|$signer_group|g" \
    -e "s|@BLOOM_BROKER_UID@|$BLOOM_MACOS_BROKER_UID|g" \
    -e "s|@BLOOM_SIGNER_UID@|$BLOOM_MACOS_SIGNER_UID|g" \
    -e "s|@MACHINE_BROKER_GID@|$BLOOM_MACOS_MACHINE_BROKER_GID|g" \
    -e "s|@BROKER_SIGNER_GID@|$BLOOM_MACOS_BROKER_SIGNER_GID|g" \
    -e "s|@REVOKE_GID@|$BLOOM_MACOS_REVOKE_GID|g" \
    -e "s|@BLOOM_MACHINE_BINARY@|$machine_binary|g" \
    -e "s|@BLOOM_BROKER_BINARY@|$broker_binary|g" \
    -e "s|@BLOOM_SIGNER_BINARY@|$signer_binary|g" \
    -e "s|@BLOOM_BROKER_IDENTITY@|$broker_config_root/identity.json|g" \
    -e "s|@BLOOM_BROKER_CONFIG@|$broker_config_root/config.json|g" \
    -e "s|@BLOOM_SIGNER_IDENTITY@|$signer_config_root/identity.json|g" \
    -e "s|@BLOOM_SIGNER_CONFIG@|$signer_config_root/config.json|g" \
    -e "s|@BLOOM_EDGE_MANIFEST@|$edge_manifest|g" \
    -e "s|@BLOOM_BROKER_AUDIT_CHECKPOINT_DIR@|$broker_state/audit-checkpoints|g" \
    -e "s|@BLOOM_SIGNER_AUDIT_CHECKPOINT_DIR@|$signer_state/audit-checkpoints|g" \
    -e "s|@BLOOM_BROKER_STATE_DIR@|$broker_state|g" \
    -e "s|@BLOOM_SIGNER_STATE_DIR@|$signer_state|g" \
    -e "s|@BLOOM_BROKER_SOCKET@|$runtime_root/machine-broker/broker.sock|g" \
    -e "s|@BLOOM_SIGNER_SOCKET@|$runtime_root/broker-signer/signer.sock|g" \
    -e "s|@BLOOM_BROKER_CONTROL_SOCKET@|$runtime_root/revoke/broker-control.sock|g" \
    -e "s|@BLOOM_SIGNER_CONTROL_SOCKET@|$runtime_root/revoke/signer-control.sock|g" \
    -e "s|@BLOOM_BROKER_LOG@|$broker_state/broker.log|g" \
    -e "s|@BLOOM_SIGNER_LOG@|$signer_state/signer.log|g" \
    "$source_file" > "$temporary"
  chmod "$mode" "$temporary"
  mv -f "$temporary" "$destination"
}

case "$action" in
  install)
    [[ $# -eq 4 ]] || usage
    validate_root_uid "$1" "$2"
    login_user="$3"
    payload="$(cd "$4" && pwd -P)"
    [[ "$login_user" =~ ^[a-z_][a-z0-9_-]*$ ]] || {
      echo "LOGIN_USER is not a safe account name" >&2
      exit 64
    }
    platform_claim="$(<"$payload/PLATFORM_CLAIM")"
    if [[ "$root" == "/" ]]; then
      [[ "$platform_claim" == "macos-unix-principals" ]] || {
        echo "live macOS installation requires a macos-unix-principals bundle" >&2
        exit 65
      }
      echo "live account, launchd, and pf activation remains gated on the disposable W0 lane" >&2
      exit 69
    fi
    if [[ ! ("$platform_claim" == "test-unclaimed" &&
      "${BLOOM_ALLOW_TEST_UNCLAIMED:-}" == "true") ]]
    then
      echo "staged macOS installation requires an explicitly allowed test bundle" >&2
      exit 65
    fi
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
    for name in \
      BLOOM_MACOS_BROKER_UID \
      BLOOM_MACOS_SIGNER_UID \
      BLOOM_MACOS_MACHINE_BROKER_GID \
      BLOOM_MACOS_BROKER_SIGNER_GID \
      BLOOM_MACOS_REVOKE_GID
    do
      require_decimal_id "$name"
    done
    [[ "${BLOOM_RELEASE_DIGEST:-}" =~ ^[0-9a-f]{64}$ ]] || {
      echo "BLOOM_RELEASE_DIGEST must be a lowercase SHA-256 digest" >&2
      exit 64
    }

    broker_user="bloom-broker-$login_uid"
    broker_group="$broker_user"
    signer_user="bloom-signer-$login_uid"
    signer_group="$signer_user"

    release_base="$root/usr/local/libexec/bloom"
    release_root="$release_base/releases/$BLOOM_RELEASE_DIGEST"
    mkdir -p "$release_root"
    chmod 0755 "$release_base" "$release_base/releases" "$release_root"
    for binary in bloom bloom-broker bloom-signer; do
      atomic_install "$payload/bin/$binary" "$release_root/$binary" 0755
    done
    current_link="$release_base/current"
    current_new="$release_base/current.new.$$"
    ln -s "releases/$BLOOM_RELEASE_DIGEST" "$current_new"
    mv -f "$current_new" "$current_link"
    machine_binary="$current_link/bloom"
    broker_binary="$current_link/bloom-broker"
    signer_binary="$current_link/bloom-signer"

    product_root="$root/Library/Application Support/BloomTriad"
    enrollment_root="$product_root/enrollments"
    config_root="$product_root/config/$login_uid"
    broker_config_root="$config_root/broker"
    signer_config_root="$config_root/signer"
    edge_manifest="$config_root/edge-manifest.json"
    broker_state="$root/var/db/bloom/$login_uid/broker"
    signer_state="$root/var/db/bloom/$login_uid/signer"
    runtime_root="$root/var/run/bloom/$login_uid"
    mkdir -p \
      "$enrollment_root" \
      "$broker_config_root" \
      "$signer_config_root" \
      "$broker_state/audit-checkpoints" \
      "$signer_state/audit-checkpoints" \
      "$runtime_root/machine-broker" \
      "$runtime_root/broker-signer" \
      "$runtime_root/revoke" \
      "$runtime_root/session" \
      "$runtime_root/status"
    chmod 0755 "$product_root" "$enrollment_root"
    chmod 0711 "$config_root" "$runtime_root"
    chmod 0700 \
      "$broker_config_root" \
      "$signer_config_root" \
      "$broker_state" \
      "$signer_state" \
      "$broker_state/audit-checkpoints" \
      "$signer_state/audit-checkpoints"
    chmod 0710 \
      "$runtime_root/machine-broker" \
      "$runtime_root/broker-signer" \
      "$runtime_root/revoke" \
      "$runtime_root/session"
    chmod 0750 "$runtime_root/status"

    render_template "$payload/config/edge-manifest.json" "$edge_manifest" 0644
    atomic_install "$payload/config/broker.json" "$broker_config_root/config.json" 0600
    atomic_install "$payload/config/signer.json" "$signer_config_root/config.json" 0600
    atomic_install \
      "$payload/config/broker-identity.json" \
      "$broker_config_root/identity.json" \
      0600
    atomic_install \
      "$payload/config/signer-identity.json" \
      "$signer_config_root/identity.json" \
      0600

    enrollment="$enrollment_root/$login_uid.json"
    enrollment_new="$enrollment.new.$$"
    printf '%s\n' \
      '{' \
      '  "schema": "bloom.macos-enrollment.1",' \
      "  \"login_uid\": $login_uid," \
      "  \"login_user\": \"$login_user\"," \
      "  \"broker_user\": \"$broker_user\"," \
      "  \"broker_uid\": $BLOOM_MACOS_BROKER_UID," \
      "  \"signer_user\": \"$signer_user\"," \
      "  \"signer_uid\": $BLOOM_MACOS_SIGNER_UID," \
      "  \"machine_broker_gid\": $BLOOM_MACOS_MACHINE_BROKER_GID," \
      "  \"broker_signer_gid\": $BLOOM_MACOS_BROKER_SIGNER_GID," \
      "  \"revoke_gid\": $BLOOM_MACOS_REVOKE_GID," \
      "  \"release_digest\": \"$BLOOM_RELEASE_DIGEST\"" \
      '}' > "$enrollment_new"
    chmod 0600 "$enrollment_new"
    mv -f "$enrollment_new" "$enrollment"

    launch_daemon_root="$root/Library/LaunchDaemons"
    launch_agent_root="$root/Library/LaunchAgents"
    broker_plist="$launch_daemon_root/com.bloom.broker.$login_uid.plist"
    signer_plist="$launch_daemon_root/com.bloom.signer.$login_uid.plist"
    session_plist="$launch_agent_root/com.bloom.session.plist"
    render_template \
      "$source_root/macos/launchdaemons/com.bloom.broker.plist.in" \
      "$broker_plist" \
      0644
    render_template \
      "$source_root/macos/launchdaemons/com.bloom.signer.plist.in" \
      "$signer_plist" \
      0644
    render_template \
      "$source_root/macos/launchagents/com.bloom.session.plist.in" \
      "$session_plist" \
      0644

    pf_target="$root/etc/pf.anchors/com.bloom.triad.$login_uid"
    render_template \
      "$source_root/macos/pf/com.bloom.login.conf.in" \
      "$pf_target" \
      0600
    ;;
  rotate-config)
    [[ $# -eq 4 ]] || usage
    validate_root_uid "$1" "$2"
    principal="$3"
    replacement="$4"
    [[ "$principal" == "broker" || "$principal" == "signer" ]] || usage
    test -f "$replacement" || {
      echo "replacement config is missing" >&2
      exit 66
    }
    destination="$root/Library/Application Support/BloomTriad/config/$login_uid/$principal/config.json"
    test -d "$(dirname "$destination")" || {
      echo "principal is not installed" >&2
      exit 66
    }
    atomic_install "$replacement" "$destination" 0600
    if [[ "$root" == "/" ]]; then
      launchctl kickstart -k "system/com.bloom.$principal.$login_uid"
    fi
    ;;
  uninstall)
    [[ $# -eq 3 ]] || usage
    validate_root_uid "$1" "$2"
    expected="delete-bloom-login-$login_uid"
    [[ "$3" == "$expected" ]] || {
      echo "uninstall confirmation must equal $expected" >&2
      exit 64
    }
    if [[ "$root" == "/" ]]; then
      echo "live stop, account removal, and pf removal remain gated on the disposable W0 lane" >&2
      exit 69
    fi
    rm -f -- \
      "$root/Library/LaunchDaemons/com.bloom.broker.$login_uid.plist" \
      "$root/Library/LaunchDaemons/com.bloom.signer.$login_uid.plist" \
      "$root/etc/pf.anchors/com.bloom.triad.$login_uid" \
      "$root/Library/Application Support/BloomTriad/enrollments/$login_uid.json"
    config_target="$root/Library/Application Support/BloomTriad/config/$login_uid"
    state_target="$root/var/db/bloom/$login_uid"
    runtime_target="$root/var/run/bloom/$login_uid"
    rm -rf -- "$config_target" "$state_target" "$runtime_target"
    ;;
  *)
    usage
    ;;
esac
