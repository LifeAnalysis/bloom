#!/usr/bin/env bash
set -Eeuo pipefail

usage() {
  cat >&2 <<'EOF'
usage:
  install-macos.sh install ROOT LOGIN_UID LOGIN_USER PAYLOAD_DIR
  install-macos.sh rotate-config ROOT LOGIN_UID PRINCIPAL CONFIG_JSON
  install-macos.sh uninstall ROOT LOGIN_UID CONFIRM_TOKEN

Staged-root tests must supply:
  BLOOM_MACOS_BROKER_UID
  BLOOM_MACOS_SIGNER_UID
  BLOOM_MACOS_BROKER_GID
  BLOOM_MACOS_SIGNER_GID
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
live_install=false
provision_committed=false
pf_reference_installed=false
installer_lock=""
generated_material=""
release_staging=""
existing_enrollment=false
fresh_enrollment=false
has_installed_enrollments=false
global_installed_release_digest=""
containment_monitor_created=false
upgrade_in_progress=false
upgrade_transaction=""
upgrade_transaction_staging=""
created_users=()
created_groups=()

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
  root_prefix="${root%/}"
  if [[ "$root" == "/" ]]; then
    live_install=true
  fi
}

require_decimal_id() {
  name="$1"
  value="${!name:-}"
  if [[ ! "$value" =~ ^[1-9][0-9]*$ ]] || ((value > 4294967295)); then
    echo "$name must be a positive 32-bit decimal ID" >&2
    exit 64
  fi
}

require_live_macos_root() {
  [[ "$EUID" -eq 0 ]] || {
    echo "live macOS installation requires root" >&2
    exit 77
  }
  [[ "$(uname -s)" == "Darwin" ]] || {
    echo "live macOS installation requires Darwin" >&2
    exit 69
  }
}

require_disposable_w0_host() {
  marker="/private/var/db/bloom-w0-disposable-host"
  if [[ "${BLOOM_RUN_MACOS_UNIX_W0:-}" != "true" ]] ||
    [[ ! -f "$marker" || -L "$marker" ]] ||
    ! grep -Fx 'bloom-macos-unix-w0-disposable-v1' "$marker" >/dev/null
  then
    echo "macOS W0 bundles run only on an explicitly marked disposable host" >&2
    exit 77
  fi
}

acquire_installer_lock() {
  installer_lock="/private/var/run/bloom-triad-installer.lock"
  mkdir "$installer_lock" 2>/dev/null || {
    echo "another Bloom installer is active" >&2
    exit 75
  }
}

release_installer_lock() {
  if [[ -n "$release_staging" ]]; then
    case "$release_staging" in
      "/usr/local/libexec/bloom/.release."* | */usr/local/libexec/bloom/.release.*)
        rm -rf -- "$release_staging"
        ;;
      *)
        echo "refusing to remove unexpected release staging path" >&2
        ;;
    esac
    release_staging=""
  fi
  if [[ -n "$upgrade_transaction_staging" ]]; then
    case "$upgrade_transaction_staging" in
      "/Library/Application Support/BloomTriad/.upgrade-transaction.new."*)
        rm -rf -- "$upgrade_transaction_staging"
        ;;
      *)
        echo "refusing to remove unexpected upgrade-transaction staging path" >&2
        ;;
    esac
    upgrade_transaction_staging=""
  fi
  if [[ -n "$generated_material" ]]; then
    case "$generated_material" in
      "/Library/Application Support/BloomTriad/.enrollment-material."*)
        rm -rf -- "$generated_material"
        ;;
      *)
        echo "refusing to remove unexpected enrollment-material path" >&2
        ;;
    esac
    generated_material=""
  fi
  if [[ -n "$installer_lock" && -d "$installer_lock" ]]; then
    rmdir "$installer_lock"
  fi
  installer_lock=""
}

rollback_provisioning() {
  status=$?
  trap - ERR
  set +e
  if $upgrade_in_progress; then
    rollback_upgrade
  fi
  if $live_install && ! $provision_committed; then
    if [[ -n "${login_uid:-}" && "$login_uid" =~ ^[1-9][0-9]*$ ]]; then
      launchctl bootout "gui/$login_uid/com.bloom.session" 2>/dev/null
      launchctl bootout "system/com.bloom.broker.$login_uid" 2>/dev/null
      launchctl bootout "system/com.bloom.signer.$login_uid" 2>/dev/null
      if $containment_monitor_created && ! $has_installed_enrollments; then
        launchctl bootout "system/com.bloom.containment" 2>/dev/null
        rm -f -- "/Library/LaunchDaemons/com.bloom.containment.plist"
      fi
      if $pf_reference_installed; then
        rewrite_pf_reference remove
      fi
      rm -f -- \
        "/Library/LaunchDaemons/com.bloom.broker.$login_uid.plist" \
        "/Library/LaunchDaemons/com.bloom.signer.$login_uid.plist" \
        "/etc/pf.anchors/com.bloom.triad.$login_uid" \
        "/Library/Application Support/BloomTriad/enrollments/$login_uid.json"
      rm -rf -- \
        "/Library/Application Support/BloomTriad/config/$login_uid" \
        "/private/var/db/bloom/$login_uid" \
        "/private/var/run/bloom/$login_uid"
    fi
    for ((index = ${#created_users[@]} - 1; index >= 0; index--)); do
      dscl . -delete "/Users/${created_users[$index]}" 2>/dev/null
    done
    for ((index = ${#created_groups[@]} - 1; index >= 0; index--)); do
      dscl . -delete "/Groups/${created_groups[$index]}" 2>/dev/null
    done
  fi
  release_installer_lock
  exit "$status"
}

trap rollback_provisioning ERR
trap release_installer_lock EXIT

directory_record_exists() {
  kind="$1"
  name="$2"
  dscl . -read "/$kind/$name" >/dev/null 2>&1
}

next_directory_id() {
  kind="$1"
  attribute="$2"
  candidate="$(
    dscl . -list "/$kind" "$attribute" |
      awk '
        $NF ~ /^[0-9]+$/ && $NF > maximum { maximum = $NF }
        END {
          if (maximum >= 2147483646) exit 1
          print maximum + 1
        }
      '
  )" || {
    echo "cannot allocate an unused numeric ID for $kind" >&2
    exit 70
  }
  [[ "$candidate" =~ ^[1-9][0-9]*$ ]] || {
    echo "Directory Service returned an invalid numeric ID" >&2
    exit 70
  }
  printf '%s\n' "$candidate"
}

create_service_group() {
  name="$1"
  gid="$2"
  directory_record_exists Groups "$name" && {
    echo "refusing to adopt pre-existing group $name" >&2
    exit 65
  }
  dscl . -create "/Groups/$name"
  created_groups+=("$name")
  dscl . -create "/Groups/$name" PrimaryGroupID "$gid"
  dscl . -create "/Groups/$name" RealName "Bloom isolated service group"
}

create_service_user() {
  name="$1"
  uid="$2"
  gid="$3"
  directory_record_exists Users "$name" && {
    echo "refusing to adopt pre-existing user $name" >&2
    exit 65
  }
  dscl . -create "/Users/$name"
  created_users+=("$name")
  dscl . -create "/Users/$name" UniqueID "$uid"
  dscl . -create "/Users/$name" PrimaryGroupID "$gid"
  dscl . -create "/Users/$name" RealName "Bloom isolated service"
  dscl . -create "/Users/$name" NFSHomeDirectory /var/empty
  dscl . -create "/Users/$name" UserShell /usr/bin/false
  dscl . -create "/Users/$name" IsHidden 1
  dscl . -create "/Users/$name" AuthenticationAuthority ";DisabledUser;"
}

add_group_member() {
  group="$1"
  member="$2"
  dseditgroup -o edit -a "$member" -t user "$group"
}

read_enrollment_field() {
  enrollment_file="$1"
  field="$2"
  plutil -extract "$field" raw -o - "$enrollment_file"
}

require_directory_value() {
  kind="$1"
  name="$2"
  attribute="$3"
  expected="$4"
  observed="$(
    dscl . -read "/$kind/$name" "$attribute" |
      sed -n "s/^$attribute: //p"
  )"
  [[ "$observed" == "$expected" ]] || {
    echo "$kind/$name has unexpected $attribute" >&2
    exit 65
  }
}

require_group_member() {
  group="$1"
  member="$2"
  dseditgroup -o checkmember -m "$member" "$group" >/dev/null 2>&1 || {
    echo "$member is not a member of required group $group" >&2
    exit 65
  }
}

require_group_nonmember() {
  group="$1"
  member="$2"
  if dseditgroup -o checkmember -m "$member" "$group" >/dev/null 2>&1; then
    echo "$member has forbidden membership in group $group" >&2
    exit 65
  fi
}

require_directory_contains() {
  kind="$1"
  name="$2"
  attribute="$3"
  expected_fragment="$4"
  dscl . -read "/$kind/$name" "$attribute" |
    grep -F "$expected_fragment" >/dev/null || {
    echo "$kind/$name does not have required $attribute" >&2
    exit 65
  }
}

require_live_file_metadata() {
  path="$1"
  expected_uid="$2"
  expected_gid="$3"
  expected_mode="$4"
  [[ -f "$path" && ! -L "$path" ]] || {
    echo "security file is missing, substituted, or a symlink: $path" >&2
    exit 65
  }
  observed="$(stat -f '%u:%g:%Lp:%l' "$path")"
  expected="$expected_uid:$expected_gid:$expected_mode:1"
  [[ "$observed" == "$expected" ]] || {
    echo "security file has unexpected owner, group, mode, or link count: $path" >&2
    exit 65
  }
}

require_live_directory_metadata() {
  path="$1"
  expected_uid="$2"
  expected_gid="$3"
  expected_mode="$4"
  [[ -d "$path" && ! -L "$path" ]] || {
    echo "security directory is missing, substituted, or a symlink: $path" >&2
    exit 65
  }
  observed="$(stat -f '%u:%g:%Lp' "$path")"
  expected="$expected_uid:$expected_gid:$expected_mode"
  [[ "$observed" == "$expected" ]] || {
    echo "security directory has unexpected owner, group, or mode: $path" >&2
    exit 65
  }
}

require_network_containment_config() {
  config_file="$1"
  enrolled_uid="$2"
  expected_status="/private/var/run/bloom/$enrolled_uid/containment/status.json"
  observed="$(
    plutil -extract network_containment.status_path raw -o - "$config_file"
  )"
  [[ "$observed" == "$expected_status" ]]
  observed="$(plutil -extract network_containment.login_uid raw -o - "$config_file")"
  [[ "$observed" == "$enrolled_uid" ]]
  observed="$(
    plutil -extract network_containment.maximum_age_ms raw -o - "$config_file"
  )"
  [[ "$observed" == "5000" ]]
}

verify_existing_enrollment() {
  [[ "$(read_enrollment_field "$enrollment" schema)" == "bloom.macos-enrollment.1" ]]
  [[ "$(read_enrollment_field "$enrollment" login_uid)" == "$login_uid" ]]
  [[ "$(read_enrollment_field "$enrollment" login_user)" == "$login_user" ]]
  [[ "$(read_enrollment_field "$enrollment" broker_user)" == "$broker_user" ]]
  [[ "$(read_enrollment_field "$enrollment" broker_group)" == "$broker_group" ]]
  [[ "$(read_enrollment_field "$enrollment" signer_user)" == "$signer_user" ]]
  [[ "$(read_enrollment_field "$enrollment" signer_group)" == "$signer_group" ]]
  observed_group="$(read_enrollment_field "$enrollment" machine_broker_group)"
  [[ "$observed_group" == "$machine_broker_group" ]]
  observed_group="$(read_enrollment_field "$enrollment" broker_signer_group)"
  [[ "$observed_group" == "$broker_signer_group" ]]
  [[ "$(read_enrollment_field "$enrollment" revoke_group)" == "$revoke_group" ]]
  BLOOM_MACOS_BROKER_UID="$(read_enrollment_field "$enrollment" broker_uid)"
  BLOOM_MACOS_SIGNER_UID="$(read_enrollment_field "$enrollment" signer_uid)"
  BLOOM_MACOS_BROKER_GID="$(read_enrollment_field "$enrollment" broker_gid)"
  BLOOM_MACOS_SIGNER_GID="$(read_enrollment_field "$enrollment" signer_gid)"
  BLOOM_MACOS_MACHINE_BROKER_GID="$(
    read_enrollment_field "$enrollment" machine_broker_gid
  )"
  BLOOM_MACOS_BROKER_SIGNER_GID="$(
    read_enrollment_field "$enrollment" broker_signer_gid
  )"
  BLOOM_MACOS_REVOKE_GID="$(read_enrollment_field "$enrollment" revoke_gid)"
  for name in \
    BLOOM_MACOS_BROKER_UID \
    BLOOM_MACOS_SIGNER_UID \
    BLOOM_MACOS_BROKER_GID \
    BLOOM_MACOS_SIGNER_GID \
    BLOOM_MACOS_MACHINE_BROKER_GID \
    BLOOM_MACOS_BROKER_SIGNER_GID \
    BLOOM_MACOS_REVOKE_GID
  do
    require_decimal_id "$name"
  done
  require_directory_value Users "$broker_user" UniqueID "$BLOOM_MACOS_BROKER_UID"
  require_directory_value Users "$broker_user" PrimaryGroupID "$BLOOM_MACOS_BROKER_GID"
  require_directory_value Users "$broker_user" IsHidden 1
  require_directory_value Users "$broker_user" UserShell /usr/bin/false
  require_directory_contains Users "$broker_user" AuthenticationAuthority DisabledUser
  require_directory_value Users "$signer_user" UniqueID "$BLOOM_MACOS_SIGNER_UID"
  require_directory_value Users "$signer_user" PrimaryGroupID "$BLOOM_MACOS_SIGNER_GID"
  require_directory_value Users "$signer_user" IsHidden 1
  require_directory_value Users "$signer_user" UserShell /usr/bin/false
  require_directory_contains Users "$signer_user" AuthenticationAuthority DisabledUser
  require_directory_value Groups "$broker_group" PrimaryGroupID "$BLOOM_MACOS_BROKER_GID"
  require_directory_value Groups "$signer_group" PrimaryGroupID "$BLOOM_MACOS_SIGNER_GID"
  require_directory_value \
    Groups \
    "$machine_broker_group" \
    PrimaryGroupID \
    "$BLOOM_MACOS_MACHINE_BROKER_GID"
  require_directory_value \
    Groups \
    "$broker_signer_group" \
    PrimaryGroupID \
    "$BLOOM_MACOS_BROKER_SIGNER_GID"
  require_directory_value Groups "$revoke_group" PrimaryGroupID "$BLOOM_MACOS_REVOKE_GID"
  require_group_member "$machine_broker_group" "$login_user"
  require_group_member "$machine_broker_group" "$broker_user"
  require_group_nonmember "$machine_broker_group" "$signer_user"
  require_group_member "$broker_signer_group" "$broker_user"
  require_group_member "$broker_signer_group" "$signer_user"
  require_group_nonmember "$broker_signer_group" "$login_user"
  require_group_member "$revoke_group" "$login_user"
  require_group_member "$revoke_group" "$broker_user"
  require_group_member "$revoke_group" "$signer_user"
}

verify_installed_security_files() {
  local enrolled_config_root="$1"
  local enrolled_record="$2"
  local enrolled_uid="$login_uid"
  local enrolled_state_root="/private/var/db/bloom/$enrolled_uid"
  local enrolled_runtime_root="/private/var/run/bloom/$enrolled_uid"
  require_live_directory_metadata "$product_root" 0 0 755
  require_live_directory_metadata "$enrollment_root" 0 0 755
  require_live_directory_metadata "$enrolled_config_root" 0 0 711
  require_live_directory_metadata \
    "$enrolled_config_root/broker" \
    "$BLOOM_MACOS_BROKER_UID" \
    "$BLOOM_MACOS_BROKER_GID" \
    700
  require_live_directory_metadata \
    "$enrolled_config_root/signer" \
    "$BLOOM_MACOS_SIGNER_UID" \
    "$BLOOM_MACOS_SIGNER_GID" \
    700
  require_live_directory_metadata \
    "$enrolled_config_root/machine" \
    "$enrolled_uid" \
    "$BLOOM_MACOS_MACHINE_BROKER_GID" \
    700
  require_live_directory_metadata \
    "$enrolled_config_root/session" \
    "$enrolled_uid" \
    "$BLOOM_MACOS_REVOKE_GID" \
    700
  require_live_directory_metadata "$enrolled_config_root/installer" 0 0 700
  require_live_directory_metadata \
    "$enrolled_state_root/broker" \
    "$BLOOM_MACOS_BROKER_UID" \
    "$BLOOM_MACOS_BROKER_GID" \
    700
  require_live_directory_metadata \
    "$enrolled_state_root/broker/audit-checkpoints" \
    "$BLOOM_MACOS_BROKER_UID" \
    "$BLOOM_MACOS_BROKER_GID" \
    700
  require_live_directory_metadata \
    "$enrolled_state_root/signer" \
    "$BLOOM_MACOS_SIGNER_UID" \
    "$BLOOM_MACOS_SIGNER_GID" \
    700
  require_live_directory_metadata \
    "$enrolled_state_root/signer/audit-checkpoints" \
    "$BLOOM_MACOS_SIGNER_UID" \
    "$BLOOM_MACOS_SIGNER_GID" \
    700
  require_live_directory_metadata "$enrolled_runtime_root" 0 0 711
  require_live_directory_metadata "$enrolled_runtime_root/containment" 0 0 755
  require_live_directory_metadata \
    "$enrolled_runtime_root/machine-broker" \
    0 \
    "$BLOOM_MACOS_MACHINE_BROKER_GID" \
    710
  require_live_directory_metadata \
    "$enrolled_runtime_root/broker-signer" \
    0 \
    "$BLOOM_MACOS_BROKER_SIGNER_GID" \
    710
  require_live_directory_metadata \
    "$enrolled_runtime_root/revoke" \
    0 \
    "$BLOOM_MACOS_REVOKE_GID" \
    710
  require_live_directory_metadata \
    "$enrolled_runtime_root/session" \
    "$enrolled_uid" \
    "$BLOOM_MACOS_REVOKE_GID" \
    710
  require_live_directory_metadata \
    "$enrolled_runtime_root/status" \
    "$BLOOM_MACOS_BROKER_UID" \
    "$BLOOM_MACOS_MACHINE_BROKER_GID" \
    750
  require_live_file_metadata "$enrolled_record" 0 0 644
  require_live_file_metadata "$enrolled_config_root/edge-manifest.json" 0 0 644
  require_live_file_metadata \
    "$enrolled_config_root/provenance-catalog.json" \
    0 \
    0 \
    644
  require_live_file_metadata \
    "$enrolled_config_root/broker/config.json" \
    "$BLOOM_MACOS_BROKER_UID" \
    "$BLOOM_MACOS_BROKER_GID" \
    600
  require_live_file_metadata \
    "$enrolled_config_root/broker/identity.json" \
    "$BLOOM_MACOS_BROKER_UID" \
    "$BLOOM_MACOS_BROKER_GID" \
    600
  require_live_file_metadata \
    "$enrolled_config_root/signer/config.json" \
    "$BLOOM_MACOS_SIGNER_UID" \
    "$BLOOM_MACOS_SIGNER_GID" \
    600
  require_live_file_metadata \
    "$enrolled_config_root/signer/identity.json" \
    "$BLOOM_MACOS_SIGNER_UID" \
    "$BLOOM_MACOS_SIGNER_GID" \
    600
  require_network_containment_config \
    "$enrolled_config_root/broker/config.json" \
    "$enrolled_uid"
  require_network_containment_config \
    "$enrolled_config_root/signer/config.json" \
    "$enrolled_uid"
  for login_private in \
    "$enrolled_config_root/machine/identity.json" \
    "$enrolled_config_root/machine/revoke-identity.json"
  do
    require_live_file_metadata \
      "$login_private" \
      "$login_uid" \
      "$BLOOM_MACOS_MACHINE_BROKER_GID" \
      600
  done
  require_live_file_metadata \
    "$enrolled_config_root/session/identity.json" \
    "$login_uid" \
    "$BLOOM_MACOS_REVOKE_GID" \
    600
  require_live_file_metadata \
    "$enrolled_config_root/installer/identity.json" \
    0 \
    0 \
    600
}

provision_fresh_accounts() {
  for name in \
    "$broker_user" \
    "$signer_user"
  do
    directory_record_exists Users "$name" && {
      echo "refusing to adopt pre-existing user $name" >&2
      exit 65
    }
  done
  for name in \
    "$broker_group" \
    "$signer_group" \
    "$machine_broker_group" \
    "$broker_signer_group" \
    "$revoke_group"
  do
    directory_record_exists Groups "$name" && {
      echo "refusing to adopt pre-existing group $name" >&2
      exit 65
    }
  done

  BLOOM_MACOS_BROKER_GID="$(next_directory_id Groups PrimaryGroupID)"
  create_service_group "$broker_group" "$BLOOM_MACOS_BROKER_GID"
  BLOOM_MACOS_SIGNER_GID="$(next_directory_id Groups PrimaryGroupID)"
  create_service_group "$signer_group" "$BLOOM_MACOS_SIGNER_GID"
  BLOOM_MACOS_MACHINE_BROKER_GID="$(next_directory_id Groups PrimaryGroupID)"
  create_service_group "$machine_broker_group" "$BLOOM_MACOS_MACHINE_BROKER_GID"
  BLOOM_MACOS_BROKER_SIGNER_GID="$(next_directory_id Groups PrimaryGroupID)"
  create_service_group "$broker_signer_group" "$BLOOM_MACOS_BROKER_SIGNER_GID"
  BLOOM_MACOS_REVOKE_GID="$(next_directory_id Groups PrimaryGroupID)"
  create_service_group "$revoke_group" "$BLOOM_MACOS_REVOKE_GID"

  BLOOM_MACOS_BROKER_UID="$(next_directory_id Users UniqueID)"
  create_service_user \
    "$broker_user" \
    "$BLOOM_MACOS_BROKER_UID" \
    "$BLOOM_MACOS_BROKER_GID"
  BLOOM_MACOS_SIGNER_UID="$(next_directory_id Users UniqueID)"
  create_service_user \
    "$signer_user" \
    "$BLOOM_MACOS_SIGNER_UID" \
    "$BLOOM_MACOS_SIGNER_GID"

  add_group_member "$machine_broker_group" "$login_user"
  add_group_member "$machine_broker_group" "$broker_user"
  add_group_member "$broker_signer_group" "$broker_user"
  add_group_member "$broker_signer_group" "$signer_user"
  add_group_member "$revoke_group" "$login_user"
  add_group_member "$revoke_group" "$broker_user"
  add_group_member "$revoke_group" "$signer_user"
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

install_network_containment_config() {
  config_file="$1"
  enrolled_uid="$2"
  containment_json="$(
    printf \
      '{"status_path":"/private/var/run/bloom/%s/containment/status.json","login_uid":%s,"maximum_age_ms":5000}' \
      "$enrolled_uid" \
      "$enrolled_uid"
  )"
  if plutil -type network_containment "$config_file" >/dev/null 2>&1; then
    plutil -replace network_containment -json "$containment_json" "$config_file"
  else
    plutil -insert network_containment -json "$containment_json" "$config_file"
  fi
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
    -e "s|@SESSION_SOCKET_GID@|$BLOOM_MACOS_REVOKE_GID|g" \
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
    -e "s|@BLOOM_SESSION_SOCKET@|$runtime_root/session/session.sock|g" \
    -e "s|@BLOOM_CONTAINMENT_STATUS@|$runtime_root/containment/status.json|g" \
    -e "s|@BLOOM_PROVENANCE_CATALOG@|$config_root/provenance-catalog.json|g" \
    -e "s|@BLOOM_BROKER_LOG@|$broker_state/broker.log|g" \
    -e "s|@BLOOM_SIGNER_LOG@|$signer_state/signer.log|g" \
    "$source_file" > "$temporary"
  chmod "$mode" "$temporary"
  mv -f "$temporary" "$destination"
}

set_live_ownership() {
  chown root:wheel "$release_base" "$release_base/releases" "$release_root"
  chown root:wheel "$release_root/bloom" "$release_root/bloom-broker" "$release_root/bloom-signer"
  chown -h root:wheel "$current_link"
  chown root:wheel "$product_root" "$enrollment_root" "$config_root"
  chown root:wheel "$edge_manifest" "$enrollment"
  chown "$broker_user:$broker_group" \
    "$broker_config_root" \
    "$broker_config_root/config.json" \
    "$broker_config_root/identity.json" \
    "$broker_state" \
    "$broker_state/audit-checkpoints"
  chown "$signer_user:$signer_group" \
    "$signer_config_root" \
    "$signer_config_root/config.json" \
    "$signer_config_root/identity.json" \
    "$signer_state" \
    "$signer_state/audit-checkpoints"
  chown "$login_user:$machine_broker_group" \
    "$machine_config_root" \
    "$machine_config_root/identity.json" \
    "$machine_config_root/revoke-identity.json"
  chown "$login_user:$revoke_group" \
    "$session_config_root" \
    "$session_config_root/identity.json" \
    "$runtime_root/session"
  chown root:wheel \
    "$installer_config_root" \
    "$installer_config_root/identity.json" \
    "$config_root/provenance-catalog.json"
  chown root:wheel "$runtime_root/containment"
  chown "root:$machine_broker_group" "$runtime_root/machine-broker"
  chown "root:$broker_signer_group" "$runtime_root/broker-signer"
  chown "root:$revoke_group" "$runtime_root/revoke"
  chown "$broker_user:$machine_broker_group" "$runtime_root/status"
  chown root:wheel \
    "$broker_plist" \
    "$signer_plist" \
    "$containment_plist" \
    "$session_plist" \
    "$pf_target"
}

rewrite_pf_reference() {
  operation="$1"
  pf_conf="/etc/pf.conf"
  [[ -f "$pf_conf" && ! -L "$pf_conf" ]] || {
    echo "/etc/pf.conf is not a regular root configuration file" >&2
    exit 65
  }
  begin_marker="# BEGIN BLOOM TRIAD $login_uid"
  end_marker="# END BLOOM TRIAD $login_uid"
  temporary="$(mktemp /etc/pf.conf.bloom.XXXXXX)"
  awk \
    -v begin="$begin_marker" \
    -v end="$end_marker" \
    '
      $0 == begin { omitted = 1; next }
      $0 == end { omitted = 0; next }
      !omitted { print }
    ' \
    "$pf_conf" > "$temporary"
  if [[ "$operation" == "add" ]]; then
    {
      printf '\n%s\n' "$begin_marker"
      printf 'anchor "com.bloom.triad/%s"\n' "$login_uid"
      printf \
        'load anchor "com.bloom.triad/%s" from "/etc/pf.anchors/com.bloom.triad.%s"\n' \
        "$login_uid" \
        "$login_uid"
      printf '%s\n' "$end_marker"
    } >> "$temporary"
  fi
  pfctl -nf "$temporary"
  chown root:wheel "$temporary"
  chmod 0644 "$temporary"
  mv -f "$temporary" "$pf_conf"
  pfctl -f "$pf_conf"
  if pfctl -s info 2>/dev/null | grep -F 'Status: Disabled' >/dev/null; then
    pfctl -E >/dev/null
  fi
}

write_upgrade_phase() {
  phase="$1"
  temporary="$upgrade_transaction/phase.new.$$"
  printf '%s\n' "$phase" > "$temporary"
  chmod 0600 "$temporary"
  mv -f "$temporary" "$upgrade_transaction/phase"
  sync
}

atomic_copy_preserving_metadata() {
  source_file="$1"
  destination="$2"
  temporary="${destination}.upgrade.$$"
  cp -p "$source_file" "$temporary"
  mv -f "$temporary" "$destination"
}

repoint_current_release() {
  target="$1"
  [[ "$target" =~ ^releases/[0-9a-f]{64}$ ]] || {
    echo "refusing an invalid Bloom current-link target" >&2
    return 65
  }
  temporary="$release_base/current.upgrade.$$"
  ln -s "$target" "$temporary"
  chown -h root:wheel "$temporary"
  mv -f "$temporary" "$release_base/current"
}

validate_upgrade_transaction() {
  [[ -d "$upgrade_transaction" && ! -L "$upgrade_transaction" ]] || {
    echo "Bloom upgrade transaction is not a regular directory" >&2
    return 65
  }
  [[ "$(stat -f '%u:%Lp' "$upgrade_transaction")" == "0:700" ]] || {
    echo "Bloom upgrade transaction has unsafe ownership or mode" >&2
    return 65
  }
  [[ -f "$upgrade_transaction/schema" && ! -L "$upgrade_transaction/schema" ]] || {
    echo "Bloom upgrade transaction is missing its schema" >&2
    return 65
  }
  grep -Fx 'bloom.macos-upgrade-transaction.1' \
    "$upgrade_transaction/schema" >/dev/null || {
    echo "Bloom upgrade transaction has an unknown schema" >&2
    return 65
  }
  for required in old-current-target old-digest new-digest uids jobs phase; do
    [[ -f "$upgrade_transaction/$required" &&
      ! -L "$upgrade_transaction/$required" ]] || {
      echo "Bloom upgrade transaction is missing $required" >&2
      return 65
    }
  done
}

stop_upgrade_jobs() {
  launchctl bootout "system/com.bloom.containment" 2>/dev/null || true
  while IFS= read -r enrolled_uid; do
    [[ "$enrolled_uid" =~ ^[1-9][0-9]*$ ]] || return 65
    launchctl bootout "gui/$enrolled_uid/com.bloom.session" 2>/dev/null || true
    launchctl bootout "system/com.bloom.broker.$enrolled_uid" 2>/dev/null || true
    launchctl bootout "system/com.bloom.signer.$enrolled_uid" 2>/dev/null || true
  done < "$upgrade_transaction/uids"
}

restore_upgrade_jobs() {
  job_kind="$1"
  while IFS=' ' read -r kind enrolled_uid; do
    [[ -n "$kind" ]] || continue
    [[ "$kind" == "$job_kind" ]] || continue
    case "$kind" in
      monitor)
        [[ "$enrolled_uid" == "0" ]] || return 65
        launchctl bootstrap \
          system \
          "/Library/LaunchDaemons/com.bloom.containment.plist"
        ;;
      session)
        [[ "$enrolled_uid" =~ ^[1-9][0-9]*$ ]] || return 65
        launchctl bootstrap \
          "gui/$enrolled_uid" \
          "/Library/LaunchAgents/com.bloom.session.plist"
        ;;
      signer)
        [[ "$enrolled_uid" =~ ^[1-9][0-9]*$ ]] || return 65
        launchctl bootstrap \
          system \
          "/Library/LaunchDaemons/com.bloom.signer.$enrolled_uid.plist"
        ;;
      broker)
        [[ "$enrolled_uid" =~ ^[1-9][0-9]*$ ]] || return 65
        launchctl bootstrap \
          system \
          "/Library/LaunchDaemons/com.bloom.broker.$enrolled_uid.plist"
        ;;
      *) return 65 ;;
    esac
  done < "$upgrade_transaction/jobs"
}

health_check_upgrade_jobs() {
  expected_digest="$1"
  while IFS=' ' read -r kind enrolled_uid; do
    [[ "$kind" == "broker" ]] || continue
    [[ "$enrolled_uid" =~ ^[1-9][0-9]*$ ]] || return 65
    enrollment_file="$enrollment_root/$enrolled_uid.json"
    enrolled_user="$(read_enrollment_field "$enrollment_file" login_user)"
    [[ "$enrolled_user" =~ ^[a-z_][a-z0-9_-]*$ ]] || return 65
    healthy=false
    for _attempt in {1..20}; do
      if /usr/bin/sudo -n -u "$enrolled_user" -- \
        "$release_base/current/bloom" \
        --triad-health-check \
        "$expected_digest"
      then
        healthy=true
        break
      fi
      sleep 1
    done
    $healthy || {
      echo "Bloom triad activation failed for login UID $enrolled_uid" >&2
      return 69
    }
  done < "$upgrade_transaction/jobs"
}

restore_upgrade_files() {
  while IFS= read -r enrolled_uid; do
    [[ "$enrolled_uid" =~ ^[1-9][0-9]*$ ]] || return 65
    backup="$upgrade_transaction/backup/$enrolled_uid"
    [[ -d "$backup" && ! -L "$backup" ]] || return 65
    atomic_copy_preserving_metadata \
      "$backup/enrollment.json" \
      "$enrollment_root/$enrolled_uid.json"
    atomic_copy_preserving_metadata \
      "$backup/broker.json" \
      "$product_root/config/$enrolled_uid/broker/config.json"
    atomic_copy_preserving_metadata \
      "$backup/signer.json" \
      "$product_root/config/$enrolled_uid/signer/config.json"
    atomic_copy_preserving_metadata \
      "$backup/broker.plist" \
      "/Library/LaunchDaemons/com.bloom.broker.$enrolled_uid.plist"
    atomic_copy_preserving_metadata \
      "$backup/signer.plist" \
      "/Library/LaunchDaemons/com.bloom.signer.$enrolled_uid.plist"
    atomic_copy_preserving_metadata \
      "$backup/pf.conf" \
      "/etc/pf.anchors/com.bloom.triad.$enrolled_uid"
  done < "$upgrade_transaction/uids"
  atomic_copy_preserving_metadata \
    "$upgrade_transaction/backup/session.plist" \
    "/Library/LaunchAgents/com.bloom.session.plist"
  atomic_copy_preserving_metadata \
    "$upgrade_transaction/backup/containment.plist" \
    "/Library/LaunchDaemons/com.bloom.containment.plist"
  repoint_current_release "$(<"$upgrade_transaction/old-current-target")"
  pfctl -f /etc/pf.conf
  sync
}

rollback_upgrade() {
  validate_upgrade_transaction || return
  phase="$(<"$upgrade_transaction/phase")"
  if [[ "$phase" == "committed" ]]; then
    rm -rf -- "$upgrade_transaction"
    upgrade_in_progress=false
    return
  fi
  stop_upgrade_jobs || return
  restore_upgrade_files || return
  restore_upgrade_jobs monitor || return
  "$release_base/current/bloom" --triad-pf-monitor-once || return
  restore_upgrade_jobs session || return
  restore_upgrade_jobs signer || return
  restore_upgrade_jobs broker || return
  old_digest="$(<"$upgrade_transaction/old-digest")"
  health_check_upgrade_jobs "$old_digest" || return
  rm -rf -- "$upgrade_transaction"
  upgrade_in_progress=false
}

recover_interrupted_upgrade() {
  upgrade_transaction="$product_root/upgrade-transaction"
  [[ -e "$upgrade_transaction" ]] || return
  upgrade_in_progress=true
  echo "recovering an interrupted Bloom macOS upgrade" >&2
  rollback_upgrade
  $upgrade_in_progress && {
    echo "Bloom macOS upgrade rollback remains incomplete" >&2
    exit 70
  }
}

prepare_upgrade_transaction() {
  old_digest="$1"
  new_digest="$2"
  upgrade_transaction="$product_root/upgrade-transaction"
  [[ ! -e "$upgrade_transaction" ]] || {
    echo "an unrecovered Bloom upgrade transaction already exists" >&2
    return 75
  }
  current_metadata="$(stat -f '%u:%g' "$release_base/current" 2>/dev/null || true)"
  [[ -L "$release_base/current" && "$current_metadata" == "0:0" ]] || {
    echo "Bloom current link has unsafe ownership or type" >&2
    return 65
  }
  old_current_target="$(readlink "$release_base/current")"
  [[ "$old_current_target" == "releases/$old_digest" ]] || {
    echo "Bloom current link does not match every installed enrollment" >&2
    return 65
  }

  upgrade_transaction_staging="$product_root/.upgrade-transaction.new.$$"
  [[ ! -e "$upgrade_transaction_staging" ]] || return 75
  mkdir "$upgrade_transaction_staging"
  chmod 0700 "$upgrade_transaction_staging"
  chown root:wheel "$upgrade_transaction_staging"
  mkdir \
    "$upgrade_transaction_staging/backup" \
    "$upgrade_transaction_staging/staged"
  chmod 0700 \
    "$upgrade_transaction_staging/backup" \
    "$upgrade_transaction_staging/staged"
  printf '%s\n' 'bloom.macos-upgrade-transaction.1' \
    > "$upgrade_transaction_staging/schema"
  printf '%s\n' "$old_current_target" \
    > "$upgrade_transaction_staging/old-current-target"
  printf '%s\n' "$old_digest" > "$upgrade_transaction_staging/old-digest"
  printf '%s\n' "$new_digest" > "$upgrade_transaction_staging/new-digest"
  : > "$upgrade_transaction_staging/uids"
  : > "$upgrade_transaction_staging/jobs"
  session_plist="/Library/LaunchAgents/com.bloom.session.plist"
  containment_plist="/Library/LaunchDaemons/com.bloom.containment.plist"
  [[ -f "$session_plist" && ! -L "$session_plist" ]] || return 65
  [[ -f "$containment_plist" && ! -L "$containment_plist" ]] || return 65
  require_live_file_metadata "$session_plist" 0 0 644
  require_live_file_metadata "$containment_plist" 0 0 644
  require_live_file_metadata /etc/pf.conf 0 0 644
  cp -p "$session_plist" "$upgrade_transaction_staging/backup/session.plist"
  cp -p \
    "$containment_plist" \
    "$upgrade_transaction_staging/backup/containment.plist"
  launchctl print "system/com.bloom.containment" >/dev/null 2>&1 || {
    echo "Bloom packet-filter monitor is not loaded" >&2
    return 69
  }
  printf '%s\n' 'monitor 0' >> "$upgrade_transaction_staging/jobs"
  session_rendered=false

  shopt -s nullglob
  enrollment_files=("$enrollment_root"/*.json)
  shopt -u nullglob
  ((${#enrollment_files[@]} > 0)) || {
    echo "Bloom has no enrollment set to upgrade" >&2
    return 66
  }
  for enrollment_file in "${enrollment_files[@]}"; do
    [[ -f "$enrollment_file" && ! -L "$enrollment_file" ]] || return 65
    enrolled_uid="$(read_enrollment_field "$enrollment_file" login_uid)"
    [[ "$enrolled_uid" =~ ^[1-9][0-9]*$ ]] || return 65
    [[ "$(basename "$enrollment_file")" == "$enrolled_uid.json" ]] || return 65
    [[ "$(read_enrollment_field "$enrollment_file" release_digest)" == "$old_digest" ]] || {
      echo "installed enrollments do not share one complete release" >&2
      return 65
    }
    login_uid="$enrolled_uid"
    login_user="$(read_enrollment_field "$enrollment_file" login_user)"
    broker_user="bloom-broker-$login_uid"
    broker_group="$broker_user"
    signer_user="bloom-signer-$login_uid"
    signer_group="$signer_user"
    machine_broker_group="bloom-machine-broker-$login_uid"
    broker_signer_group="bloom-broker-signer-$login_uid"
    revoke_group="bloom-revoke-$login_uid"
    enrollment="$enrollment_file"
    verify_existing_enrollment
    verify_installed_security_files \
      "$product_root/config/$enrolled_uid" \
      "$enrollment_file"

    printf '%s\n' "$enrolled_uid" >> "$upgrade_transaction_staging/uids"
    backup="$upgrade_transaction_staging/backup/$enrolled_uid"
    staged="$upgrade_transaction_staging/staged/$enrolled_uid"
    mkdir "$backup" "$staged"
    chmod 0700 "$backup" "$staged"
    broker_config="$product_root/config/$enrolled_uid/broker/config.json"
    signer_config="$product_root/config/$enrolled_uid/signer/config.json"
    broker_plist="/Library/LaunchDaemons/com.bloom.broker.$enrolled_uid.plist"
    signer_plist="/Library/LaunchDaemons/com.bloom.signer.$enrolled_uid.plist"
    pf_target="/etc/pf.anchors/com.bloom.triad.$enrolled_uid"
    for config in "$broker_config" "$signer_config"; do
      [[ -f "$config" && ! -L "$config" ]] || return 65
    done
    require_live_file_metadata "$broker_plist" 0 0 644
    require_live_file_metadata "$signer_plist" 0 0 644
    require_live_file_metadata "$pf_target" 0 0 600
    grep -Fx "anchor \"com.bloom.triad/$enrolled_uid\"" /etc/pf.conf >/dev/null
    grep -Fx \
      "load anchor \"com.bloom.triad/$enrolled_uid\" from \"$pf_target\"" \
      /etc/pf.conf >/dev/null
    cp -p "$enrollment_file" "$backup/enrollment.json"
    cp -p "$broker_config" "$backup/broker.json"
    cp -p "$signer_config" "$backup/signer.json"
    cp -p "$broker_plist" "$backup/broker.plist"
    cp -p "$signer_plist" "$backup/signer.plist"
    cp -p "$pf_target" "$backup/pf.conf"
    cp -p "$enrollment_file" "$staged/enrollment.json"
    cp -p "$broker_config" "$staged/broker.json"
    cp -p "$signer_config" "$staged/signer.json"
    plutil -replace release_digest -string "$new_digest" \
      "$staged/enrollment.json"
    plutil -replace build_digest -string "$new_digest" "$staged/broker.json"
    plutil -replace build_digest -string "$new_digest" "$staged/signer.json"
    install_network_containment_config "$staged/broker.json" "$enrolled_uid"
    install_network_containment_config "$staged/signer.json" "$enrolled_uid"
    staged_digest="$(
      read_enrollment_field "$staged/enrollment.json" release_digest
    )"
    [[ "$staged_digest" == "$new_digest" ]]
    staged_digest="$(read_enrollment_field "$staged/broker.json" build_digest)"
    [[ "$staged_digest" == "$new_digest" ]]
    staged_digest="$(read_enrollment_field "$staged/signer.json" build_digest)"
    [[ "$staged_digest" == "$new_digest" ]]
    chown root:wheel "$staged/enrollment.json"
    chown "$BLOOM_MACOS_BROKER_UID:$BLOOM_MACOS_BROKER_GID" \
      "$staged/broker.json"
    chown "$BLOOM_MACOS_SIGNER_UID:$BLOOM_MACOS_SIGNER_GID" \
      "$staged/signer.json"
    chmod 0644 "$staged/enrollment.json"
    chmod 0600 "$staged/broker.json" "$staged/signer.json"

    config_root="$product_root/config/$enrolled_uid"
    broker_config_root="$config_root/broker"
    signer_config_root="$config_root/signer"
    machine_config_root="$config_root/machine"
    session_config_root="$config_root/session"
    installer_config_root="$config_root/installer"
    edge_manifest="$config_root/edge-manifest.json"
    broker_state="/private/var/db/bloom/$enrolled_uid/broker"
    signer_state="/private/var/db/bloom/$enrolled_uid/signer"
    runtime_root="/private/var/run/bloom/$enrolled_uid"
    machine_binary="$release_base/current/bloom"
    broker_binary="$release_base/current/bloom-broker"
    signer_binary="$release_base/current/bloom-signer"
    render_template \
      "$source_root/macos/launchdaemons/com.bloom.broker.plist.in" \
      "$staged/broker.plist" \
      0644
    render_template \
      "$source_root/macos/launchdaemons/com.bloom.signer.plist.in" \
      "$staged/signer.plist" \
      0644
    render_template \
      "$source_root/macos/pf/com.bloom.login.conf.in" \
      "$staged/pf.conf" \
      0600
    if ! $session_rendered; then
      render_template \
        "$source_root/macos/launchagents/com.bloom.session.plist.in" \
        "$upgrade_transaction_staging/staged/session.plist" \
        0644
      session_rendered=true
      render_template \
        "$source_root/macos/launchdaemons/com.bloom.containment.plist.in" \
        "$upgrade_transaction_staging/staged/containment.plist" \
        0644
    fi
    chown root:wheel \
      "$staged/broker.plist" \
      "$staged/signer.plist" \
      "$staged/pf.conf"
    plutil -lint "$staged/broker.plist" "$staged/signer.plist" >/dev/null
    pfctl -nf "$staged/pf.conf"

    if launchctl print "gui/$enrolled_uid/com.bloom.session" >/dev/null 2>&1; then
      printf 'session %s\n' "$enrolled_uid" \
        >> "$upgrade_transaction_staging/jobs"
    fi
    if launchctl print "system/com.bloom.signer.$enrolled_uid" >/dev/null 2>&1; then
      printf 'signer %s\n' "$enrolled_uid" \
        >> "$upgrade_transaction_staging/jobs"
    fi
    if launchctl print "system/com.bloom.broker.$enrolled_uid" >/dev/null 2>&1; then
      printf 'broker %s\n' "$enrolled_uid" \
        >> "$upgrade_transaction_staging/jobs"
    fi
  done
  chown root:wheel \
    "$upgrade_transaction_staging/staged/session.plist" \
    "$upgrade_transaction_staging/staged/containment.plist"
  plutil -lint \
    "$upgrade_transaction_staging/staged/session.plist" \
    "$upgrade_transaction_staging/staged/containment.plist" >/dev/null
  printf '%s\n' prepared > "$upgrade_transaction_staging/phase"
  chmod 0600 \
    "$upgrade_transaction_staging/schema" \
    "$upgrade_transaction_staging/old-current-target" \
    "$upgrade_transaction_staging/old-digest" \
    "$upgrade_transaction_staging/new-digest" \
    "$upgrade_transaction_staging/uids" \
    "$upgrade_transaction_staging/jobs" \
    "$upgrade_transaction_staging/phase"
  chmod 0700 \
    "$upgrade_transaction_staging/backup" \
    "$upgrade_transaction_staging/staged"
  for transaction_directory in \
    "$upgrade_transaction_staging"/backup/[0-9]* \
    "$upgrade_transaction_staging"/staged/[0-9]*
  do
    [[ -d "$transaction_directory" && ! -L "$transaction_directory" ]] || return 65
    chmod 0700 "$transaction_directory"
  done
  sync
  mv "$upgrade_transaction_staging" "$upgrade_transaction"
  upgrade_transaction_staging=""
  sync
  upgrade_in_progress=true
}

activate_upgrade_transaction() {
  new_digest="$(<"$upgrade_transaction/new-digest")"
  stop_upgrade_jobs
  write_upgrade_phase switching
  while IFS= read -r enrolled_uid; do
    staged="$upgrade_transaction/staged/$enrolled_uid"
    atomic_copy_preserving_metadata \
      "$staged/enrollment.json" \
      "$enrollment_root/$enrolled_uid.json"
    atomic_copy_preserving_metadata \
      "$staged/broker.json" \
      "$product_root/config/$enrolled_uid/broker/config.json"
    atomic_copy_preserving_metadata \
      "$staged/signer.json" \
      "$product_root/config/$enrolled_uid/signer/config.json"
    atomic_copy_preserving_metadata \
      "$staged/broker.plist" \
      "/Library/LaunchDaemons/com.bloom.broker.$enrolled_uid.plist"
    atomic_copy_preserving_metadata \
      "$staged/signer.plist" \
      "/Library/LaunchDaemons/com.bloom.signer.$enrolled_uid.plist"
    atomic_copy_preserving_metadata \
      "$staged/pf.conf" \
      "/etc/pf.anchors/com.bloom.triad.$enrolled_uid"
  done < "$upgrade_transaction/uids"
  atomic_copy_preserving_metadata \
    "$upgrade_transaction/staged/session.plist" \
    "/Library/LaunchAgents/com.bloom.session.plist"
  atomic_copy_preserving_metadata \
    "$upgrade_transaction/staged/containment.plist" \
    "/Library/LaunchDaemons/com.bloom.containment.plist"
  repoint_current_release "releases/$new_digest"
  pfctl -f /etc/pf.conf
  launchctl bootstrap system "/Library/LaunchDaemons/com.bloom.containment.plist"
  "$release_base/current/bloom" --triad-pf-monitor-once
  sync
  write_upgrade_phase switched
  restore_upgrade_jobs session
  restore_upgrade_jobs signer
  restore_upgrade_jobs broker
  write_upgrade_phase activating
  health_check_upgrade_jobs "$new_digest"
  write_upgrade_phase committed
  upgrade_in_progress=false
  rm -rf -- "$upgrade_transaction"
}

install_immutable_release() {
  release_base="$1"
  release_root="$2"
  payload="$3"
  mkdir -p "$release_base" "$release_base/releases"
  chmod 0755 "$release_base" "$release_base/releases"
  if [[ -e "$release_root" ]]; then
    [[ -d "$release_root" && ! -L "$release_root" ]] || {
      echo "versioned Bloom release path is not an immutable directory" >&2
      return 65
    }
    installed_entries="$(find "$release_root" -mindepth 1 -maxdepth 1 -print | wc -l |
      tr -d '[:space:]')"
    [[ "$installed_entries" == "3" ]] || {
      echo "versioned Bloom release has an unexpected file inventory" >&2
      return 65
    }
    for binary in bloom bloom-broker bloom-signer; do
      [[ -f "$release_root/$binary" && ! -L "$release_root/$binary" ]] || {
        echo "versioned Bloom release contains a substituted binary" >&2
        return 65
      }
      cmp "$payload/bin/$binary" "$release_root/$binary" >/dev/null || {
        echo "existing versioned Bloom release does not match its digest" >&2
        return 65
      }
      if $live_install; then
        binary_metadata="$(stat -f '%u:%g:%Lp:%l' "$release_root/$binary")"
        [[ "$binary_metadata" == "0:0:755:1" ]] || {
          echo "versioned Bloom binary has unsafe metadata" >&2
          return 65
        }
      fi
    done
    return
  fi
  release_staging="$release_base/.release.$BLOOM_RELEASE_DIGEST.$$"
  mkdir "$release_staging"
  chmod 0755 "$release_staging"
  for binary in bloom bloom-broker bloom-signer; do
    install -m 0755 "$payload/bin/$binary" "$release_staging/$binary"
  done
  if $live_install; then
    chown -R root:wheel "$release_staging"
  fi
  sync
  mv "$release_staging" "$release_root"
  release_staging=""
  sync
}

bootstrap_live_jobs() {
  plutil -lint \
    "$broker_plist" \
    "$signer_plist" \
    "$containment_plist" \
    "$session_plist" >/dev/null
  pfctl -nf "$pf_target"
  if ! launchctl print "system/com.bloom.containment" >/dev/null 2>&1; then
    containment_monitor_created=true
  fi
  launchctl bootout "system/com.bloom.containment" 2>/dev/null || true
  launchctl bootstrap system "$containment_plist"
  "$machine_binary" --triad-pf-monitor-once
  launchctl bootout "gui/$login_uid/com.bloom.session" 2>/dev/null || true
  launchctl bootout "system/com.bloom.broker.$login_uid" 2>/dev/null || true
  launchctl bootout "system/com.bloom.signer.$login_uid" 2>/dev/null || true
  launchctl bootstrap "gui/$login_uid" "$session_plist"
  launchctl bootstrap system "$signer_plist"
  launchctl bootstrap system "$broker_plist"
}

health_check_enrollment() {
  healthy=false
  for _attempt in {1..20}; do
    if /usr/bin/sudo -n -u "$login_user" -- \
      "$machine_binary" \
      --triad-health-check \
      "$BLOOM_RELEASE_DIGEST"
    then
      healthy=true
      break
    fi
    sleep 1
  done
  $healthy || {
    echo "Bloom triad activation failed for login UID $login_uid" >&2
    return 69
  }
}

delete_directory_record_exact() {
  kind="$1"
  name="$2"
  attribute="$3"
  expected="$4"
  require_directory_value "$kind" "$name" "$attribute" "$expected"
  dscl . -delete "/$kind/$name"
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
    if $live_install; then
      require_live_macos_root
      case "$platform_claim" in
        macos-unix-principals) ;;
        macos-unix-principals-w0) require_disposable_w0_host ;;
        *)
          echo "live macOS installation requires a Unix-principal macOS bundle" >&2
          exit 65
          ;;
      esac
      acquire_installer_lock
      product_root="/Library/Application Support/BloomTriad"
      enrollment_root="$product_root/enrollments"
      release_base="/usr/local/libexec/bloom"
      recover_interrupted_upgrade
      [[ "$(id -u "$login_user")" == "$login_uid" ]] || {
        echo "LOGIN_USER does not resolve to LOGIN_UID" >&2
        exit 65
      }
      launchctl print "gui/$login_uid" >/dev/null 2>&1 || {
        echo "the enrolled login must have an active GUI launchd domain" >&2
        exit 69
      }
    elif [[ ! ("$platform_claim" == "test-unclaimed" &&
      "${BLOOM_ALLOW_TEST_UNCLAIMED:-}" == "true") ]]
    then
      echo "staged macOS installation requires an explicitly allowed test bundle" >&2
      exit 65
    fi
    for required in \
      bin/bloom \
      bin/bloom-broker \
      bin/bloom-signer
    do
      test -f "$payload/$required" || {
        echo "payload is missing $required" >&2
        exit 66
      }
    done
    if [[ "$platform_claim" != "macos-unix-principals" ]]; then
      for required in \
        config/edge-manifest.json \
        config/broker.json \
        config/signer.json \
        config/machine-identity.json \
        config/broker-identity.json \
        config/signer-identity.json \
        config/revoke-identity.json \
        config/session-identity.json \
        config/installer-identity.json \
        config/provenance-catalog.json
      do
        test -f "$payload/$required" || {
          echo "payload is missing $required" >&2
          exit 66
        }
      done
    fi

    broker_user="bloom-broker-$login_uid"
    broker_group="$broker_user"
    signer_user="bloom-signer-$login_uid"
    signer_group="$signer_user"
    machine_broker_group="bloom-machine-broker-$login_uid"
    broker_signer_group="bloom-broker-signer-$login_uid"
    revoke_group="bloom-revoke-$login_uid"

    product_root="$root_prefix/Library/Application Support/BloomTriad"
    enrollment_root="$product_root/enrollments"
    enrollment="$enrollment_root/$login_uid.json"
    if $live_install; then
      test -f "$payload/SHA256SUMS" || {
        echo "live macOS payload is missing SHA256SUMS" >&2
        exit 66
      }
      for signed_input in RELEASE_PUBLIC_KEY.pem RELEASE_SIGNATURE; do
        test -f "$payload/$signed_input" || {
          echo "live macOS payload is missing $signed_input" >&2
          exit 66
        }
      done
      if [[ "$platform_claim" == "macos-unix-principals" ]]; then
        pinned_key="${BLOOM_RELEASE_PUBLIC_KEY:-}"
        [[ -f "$pinned_key" && ! -L "$pinned_key" ]] || {
          echo "BLOOM_RELEASE_PUBLIC_KEY must name the pinned root-owned key" >&2
          exit 66
        }
        pinned_metadata="$(stat -f '%u:%Lp' "$pinned_key")"
        pinned_uid="${pinned_metadata%%:*}"
        pinned_mode="${pinned_metadata#*:}"
        [[ "$pinned_uid" == "0" && $((8#$pinned_mode & 022)) -eq 0 ]] || {
          echo "pinned release key must be root-owned and not group/world writable" >&2
          exit 65
        }
        cmp "$pinned_key" "$payload/RELEASE_PUBLIC_KEY.pem" >/dev/null || {
          echo "payload release key does not match the pinned release key" >&2
          exit 65
        }
      fi
      openssl pkeyutl \
        -verify \
        -rawin \
        -pubin \
        -inkey "$payload/RELEASE_PUBLIC_KEY.pem" \
        -in "$payload/SHA256SUMS" \
        -sigfile "$payload/RELEASE_SIGNATURE" >/dev/null
      (
        cd "$payload"
        shasum -a 256 -c SHA256SUMS
      ) >/dev/null
      BLOOM_RELEASE_DIGEST="$(
        shasum -a 256 "$payload/SHA256SUMS" |
          awk '{print $1}'
      )"
      if [[ -d "$enrollment_root" && ! -L "$enrollment_root" ]]; then
        shopt -s nullglob
        installed_enrollment_files=("$enrollment_root"/*.json)
        shopt -u nullglob
        for installed_enrollment in "${installed_enrollment_files[@]}"; do
          [[ -f "$installed_enrollment" && ! -L "$installed_enrollment" ]] || {
            echo "installed enrollment set contains a substituted record" >&2
            exit 65
          }
          observed_release_digest="$(
            read_enrollment_field "$installed_enrollment" release_digest
          )"
          [[ "$observed_release_digest" =~ ^[0-9a-f]{64}$ ]] || {
            echo "installed enrollment has an invalid release digest" >&2
            exit 65
          }
          if $has_installed_enrollments; then
            [[ "$observed_release_digest" == "$global_installed_release_digest" ]] || {
              echo "installed enrollments do not share one complete release" >&2
              exit 65
            }
          else
            has_installed_enrollments=true
            global_installed_release_digest="$observed_release_digest"
          fi
        done
      fi
      if [[ -e "$enrollment" ]]; then
        [[ -f "$enrollment" && ! -L "$enrollment" ]] || {
          echo "enrollment record is not a regular file" >&2
          exit 65
        }
        verify_existing_enrollment
        existing_enrollment=true
        installed_release_digest="$(read_enrollment_field "$enrollment" release_digest)"
        [[ "$installed_release_digest" == "$global_installed_release_digest" ]]
        provision_committed=true
      else
        fresh_enrollment=true
      fi
    else
      for name in \
        BLOOM_MACOS_BROKER_UID \
        BLOOM_MACOS_SIGNER_UID \
        BLOOM_MACOS_BROKER_GID \
        BLOOM_MACOS_SIGNER_GID \
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
    fi

    release_base="$root_prefix/usr/local/libexec/bloom"
    release_root="$release_base/releases/$BLOOM_RELEASE_DIGEST"
    install_immutable_release "$release_base" "$release_root" "$payload"
    current_link="$release_base/current"
    if $live_install && $has_installed_enrollments &&
      [[ "$global_installed_release_digest" != "$BLOOM_RELEASE_DIGEST" ]]
    then
      prepare_upgrade_transaction \
        "$global_installed_release_digest" \
        "$BLOOM_RELEASE_DIGEST"
      activate_upgrade_transaction
      if $existing_enrollment; then
        echo "Bloom macOS release upgraded atomically across all enrollments"
        exit 0
      fi
      login_uid="$2"
      login_user="$3"
      broker_user="bloom-broker-$login_uid"
      broker_group="$broker_user"
      signer_user="bloom-signer-$login_uid"
      signer_group="$signer_user"
      machine_broker_group="bloom-machine-broker-$login_uid"
      broker_signer_group="bloom-broker-signer-$login_uid"
      revoke_group="bloom-revoke-$login_uid"
      enrollment="$enrollment_root/$login_uid.json"
    elif $live_install && $has_installed_enrollments; then
      current_target="$(readlink "$current_link" 2>/dev/null || true)"
      [[ -L "$current_link" &&
        "$current_target" == "releases/$BLOOM_RELEASE_DIGEST" ]] || {
        echo "Bloom current link does not match the installed enrollment" >&2
        exit 65
      }
    else
      current_new="$release_base/current.new.$$"
      ln -s "releases/$BLOOM_RELEASE_DIGEST" "$current_new"
      if $live_install; then
        chown -h root:wheel "$current_new"
      fi
      mv -f "$current_new" "$current_link"
    fi
    if $live_install && $fresh_enrollment; then
      provision_fresh_accounts
    fi
    machine_binary="$current_link/bloom"
    broker_binary="$current_link/bloom-broker"
    signer_binary="$current_link/bloom-signer"

    config_root="$product_root/config/$login_uid"
    broker_config_root="$config_root/broker"
    signer_config_root="$config_root/signer"
    machine_config_root="$config_root/machine"
    session_config_root="$config_root/session"
    installer_config_root="$config_root/installer"
    edge_manifest="$config_root/edge-manifest.json"
    if $live_install; then
      variable_root="/private/var"
    else
      variable_root="$root_prefix/var"
    fi
    broker_state="$variable_root/db/bloom/$login_uid/broker"
    signer_state="$variable_root/db/bloom/$login_uid/signer"
    runtime_root="$variable_root/run/bloom/$login_uid"
    if $live_install && $existing_enrollment; then
      verify_installed_security_files "$config_root" "$enrollment"
    fi
    mkdir -p \
      "$enrollment_root" \
      "$broker_config_root" \
      "$signer_config_root" \
      "$machine_config_root" \
      "$session_config_root" \
      "$installer_config_root" \
      "$broker_state/audit-checkpoints" \
      "$signer_state/audit-checkpoints" \
      "$runtime_root/machine-broker" \
      "$runtime_root/broker-signer" \
      "$runtime_root/revoke" \
      "$runtime_root/session" \
      "$runtime_root/containment" \
      "$runtime_root/status"
    for security_directory in \
      "$product_root" \
      "$enrollment_root" \
      "$config_root" \
      "$broker_config_root" \
      "$signer_config_root" \
      "$machine_config_root" \
      "$session_config_root" \
      "$installer_config_root" \
      "$broker_state" \
      "$signer_state" \
      "$broker_state/audit-checkpoints" \
      "$signer_state/audit-checkpoints" \
      "$runtime_root" \
      "$runtime_root/machine-broker" \
      "$runtime_root/broker-signer" \
      "$runtime_root/revoke" \
      "$runtime_root/session" \
      "$runtime_root/containment" \
      "$runtime_root/status"
    do
      [[ -d "$security_directory" && ! -L "$security_directory" ]] || {
        echo "security directory is missing, substituted, or a symlink: $security_directory" >&2
        exit 65
      }
    done
    chmod 0755 "$product_root" "$enrollment_root"
    chmod 0711 "$config_root" "$runtime_root"
    chmod 0700 \
      "$broker_config_root" \
      "$signer_config_root" \
      "$machine_config_root" \
      "$session_config_root" \
      "$installer_config_root" \
      "$broker_state" \
      "$signer_state" \
      "$broker_state/audit-checkpoints" \
      "$signer_state/audit-checkpoints"
    chmod 0710 \
      "$runtime_root/machine-broker" \
      "$runtime_root/broker-signer" \
      "$runtime_root/revoke" \
      "$runtime_root/session"
    chmod 0755 "$runtime_root/containment"
    chmod 0750 "$runtime_root/status"

    if ! $existing_enrollment; then
      if [[ "$platform_claim" == "macos-unix-principals" ]]; then
        generated_material="$(mktemp -d "$product_root/.enrollment-material.XXXXXX")"
        chmod 0700 "$generated_material"
        chown root:wheel "$generated_material"
        "$machine_binary" \
          --triad-render-macos-enrollment \
          "$source_root/macos/config" \
          "$generated_material" \
          "$login_uid" \
          "$BLOOM_MACOS_BROKER_UID" \
          "$BLOOM_MACOS_SIGNER_UID" \
          "$BLOOM_MACOS_REVOKE_GID" \
          "$BLOOM_RELEASE_DIGEST"
        config_source="$generated_material"
      else
        config_source="$payload/config"
      fi
      render_template "$config_source/edge-manifest.json" "$edge_manifest" 0644
      render_template "$config_source/broker.json" "$broker_config_root/config.json" 0600
      render_template "$config_source/signer.json" "$signer_config_root/config.json" 0600
      if $live_install; then
        install_network_containment_config \
          "$broker_config_root/config.json" \
          "$login_uid"
        install_network_containment_config \
          "$signer_config_root/config.json" \
          "$login_uid"
      fi
      atomic_install \
        "$config_source/machine-identity.json" \
        "$machine_config_root/identity.json" \
        0600
      atomic_install \
        "$config_source/broker-identity.json" \
        "$broker_config_root/identity.json" \
        0600
      atomic_install \
        "$config_source/signer-identity.json" \
        "$signer_config_root/identity.json" \
        0600
      atomic_install \
        "$config_source/revoke-identity.json" \
        "$machine_config_root/revoke-identity.json" \
        0600
      atomic_install \
        "$config_source/session-identity.json" \
        "$session_config_root/identity.json" \
        0600
      atomic_install \
        "$config_source/installer-identity.json" \
        "$installer_config_root/identity.json" \
        0600
      atomic_install \
        "$config_source/provenance-catalog.json" \
        "$config_root/provenance-catalog.json" \
        0644
    fi

    enrollment_new="$enrollment.new.$$"
    printf '%s\n' \
      '{' \
      '  "schema": "bloom.macos-enrollment.1",' \
      "  \"login_uid\": $login_uid," \
      "  \"login_user\": \"$login_user\"," \
      "  \"broker_user\": \"$broker_user\"," \
      "  \"broker_uid\": $BLOOM_MACOS_BROKER_UID," \
      "  \"broker_group\": \"$broker_group\"," \
      "  \"broker_gid\": $BLOOM_MACOS_BROKER_GID," \
      "  \"signer_user\": \"$signer_user\"," \
      "  \"signer_uid\": $BLOOM_MACOS_SIGNER_UID," \
      "  \"signer_group\": \"$signer_group\"," \
      "  \"signer_gid\": $BLOOM_MACOS_SIGNER_GID," \
      "  \"machine_broker_group\": \"$machine_broker_group\"," \
      "  \"machine_broker_gid\": $BLOOM_MACOS_MACHINE_BROKER_GID," \
      "  \"broker_signer_group\": \"$broker_signer_group\"," \
      "  \"broker_signer_gid\": $BLOOM_MACOS_BROKER_SIGNER_GID," \
      "  \"revoke_group\": \"$revoke_group\"," \
      "  \"revoke_gid\": $BLOOM_MACOS_REVOKE_GID," \
      "  \"release_digest\": \"$BLOOM_RELEASE_DIGEST\"" \
      '}' > "$enrollment_new"
    chmod 0644 "$enrollment_new"
    mv -f "$enrollment_new" "$enrollment"

    launch_daemon_root="$root_prefix/Library/LaunchDaemons"
    launch_agent_root="$root_prefix/Library/LaunchAgents"
    broker_plist="$launch_daemon_root/com.bloom.broker.$login_uid.plist"
    signer_plist="$launch_daemon_root/com.bloom.signer.$login_uid.plist"
    containment_plist="$launch_daemon_root/com.bloom.containment.plist"
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
      "$source_root/macos/launchdaemons/com.bloom.containment.plist.in" \
      "$containment_plist" \
      0644
    render_template \
      "$source_root/macos/launchagents/com.bloom.session.plist.in" \
      "$session_plist" \
      0644

    pf_target="$root_prefix/etc/pf.anchors/com.bloom.triad.$login_uid"
    render_template \
      "$source_root/macos/pf/com.bloom.login.conf.in" \
      "$pf_target" \
      0600
    if $live_install; then
      set_live_ownership
      pf_reference_installed=true
      rewrite_pf_reference add
      bootstrap_live_jobs
      health_check_enrollment
      provision_committed=true
      echo "Bloom macOS enrollment installed; log out and back in before first use so the login principal receives its new RPC groups"
    fi
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
    if $live_install; then
      require_live_macos_root
      acquire_installer_lock
      enrollment="$root_prefix/Library/Application Support/BloomTriad/enrollments/$login_uid.json"
      broker_user="bloom-broker-$login_uid"
      broker_group="$broker_user"
      signer_user="bloom-signer-$login_uid"
      signer_group="$signer_user"
      machine_broker_group="bloom-machine-broker-$login_uid"
      broker_signer_group="bloom-broker-signer-$login_uid"
      revoke_group="bloom-revoke-$login_uid"
      [[ -f "$enrollment" && ! -L "$enrollment" ]] || {
        echo "enrollment record is missing or invalid" >&2
        exit 66
      }
      verify_existing_enrollment
      provision_committed=true
    fi
    destination="$root_prefix/Library/Application Support/BloomTriad/config/$login_uid/$principal/config.json"
    test -d "$(dirname "$destination")" || {
      echo "principal is not installed" >&2
      exit 66
    }
    atomic_install "$replacement" "$destination" 0600
    if $live_install; then
      chown "bloom-$principal-$login_uid:bloom-$principal-$login_uid" "$destination"
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
    product_root="$root_prefix/Library/Application Support/BloomTriad"
    enrollment_root="$product_root/enrollments"
    enrollment="$enrollment_root/$login_uid.json"
    broker_user="bloom-broker-$login_uid"
    broker_group="$broker_user"
    signer_user="bloom-signer-$login_uid"
    signer_group="$signer_user"
    machine_broker_group="bloom-machine-broker-$login_uid"
    broker_signer_group="bloom-broker-signer-$login_uid"
    revoke_group="bloom-revoke-$login_uid"
    if $live_install; then
      require_live_macos_root
      acquire_installer_lock
      [[ -f "$enrollment" && ! -L "$enrollment" ]] || {
        echo "enrollment record is missing or invalid" >&2
        exit 66
      }
      verify_existing_enrollment
      provision_committed=true
      launchctl bootout "gui/$login_uid/com.bloom.session" 2>/dev/null || true
      launchctl bootout "system/com.bloom.broker.$login_uid" 2>/dev/null || true
      launchctl bootout "system/com.bloom.signer.$login_uid" 2>/dev/null || true
      rewrite_pf_reference remove
      variable_root="/private/var"
    else
      variable_root="$root_prefix/var"
    fi
    rm -f -- \
      "$root_prefix/Library/LaunchDaemons/com.bloom.broker.$login_uid.plist" \
      "$root_prefix/Library/LaunchDaemons/com.bloom.signer.$login_uid.plist" \
      "$root_prefix/etc/pf.anchors/com.bloom.triad.$login_uid" \
      "$enrollment"
    config_target="$product_root/config/$login_uid"
    state_target="$variable_root/db/bloom/$login_uid"
    runtime_target="$variable_root/run/bloom/$login_uid"
    rm -rf -- "$config_target" "$state_target" "$runtime_target"
    if $live_install; then
      delete_directory_record_exact Users "$broker_user" UniqueID "$BLOOM_MACOS_BROKER_UID"
      delete_directory_record_exact Users "$signer_user" UniqueID "$BLOOM_MACOS_SIGNER_UID"
      delete_directory_record_exact Groups "$broker_group" PrimaryGroupID "$BLOOM_MACOS_BROKER_GID"
      delete_directory_record_exact Groups "$signer_group" PrimaryGroupID "$BLOOM_MACOS_SIGNER_GID"
      delete_directory_record_exact \
        Groups \
        "$machine_broker_group" \
        PrimaryGroupID \
        "$BLOOM_MACOS_MACHINE_BROKER_GID"
      delete_directory_record_exact \
        Groups \
        "$broker_signer_group" \
        PrimaryGroupID \
        "$BLOOM_MACOS_BROKER_SIGNER_GID"
      delete_directory_record_exact Groups "$revoke_group" PrimaryGroupID "$BLOOM_MACOS_REVOKE_GID"
      if ! find "$enrollment_root" -type f -name '*.json' -mindepth 1 -maxdepth 1 |
        grep . >/dev/null
      then
        launchctl bootout "system/com.bloom.containment" 2>/dev/null || true
        rm -f -- "$root_prefix/Library/LaunchDaemons/com.bloom.containment.plist"
        rm -f -- "$root_prefix/Library/LaunchAgents/com.bloom.session.plist"
        rm -rf -- "$root_prefix/usr/local/libexec/bloom"
      fi
    fi
    ;;
  *)
    usage
    ;;
esac
