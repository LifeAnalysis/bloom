#!/usr/bin/env bash
set -euo pipefail
umask 077

die() { echo "Linux enrollment: $*" >&2; exit 65; }

[[ $# -eq 3 ]] || {
  echo "usage: enroll-linux.sh LOGIN_UID LOGIN_USER PAYLOAD_DIR" >&2
  exit 64
}
[[ "$(id -u)" -eq 0 ]] || die "must run as root"
[[ "$(uname -s)" == Linux ]] || die "requires Linux"

login_uid="$1"
login_user="$2"
payload="$(cd "$3" && pwd -P)"
[[ "$login_uid" =~ ^[1-9][0-9]*$ ]] || die "LOGIN_UID must be positive"
[[ "$login_user" =~ ^[a-z_][a-z0-9_-]*$ ]] || die "unsafe LOGIN_USER"
[[ "$(id -u "$login_user")" == "$login_uid" ]] || die "LOGIN_USER and LOGIN_UID disagree"
login_gid="$(id -g "$login_user")"
[[ "$login_gid" =~ ^[1-9][0-9]*$ ]] || die "LOGIN_USER has an invalid primary group"

broker_user="bloom-broker-$login_uid"
signer_user="bloom-signer-$login_uid"
session_group="bloom-machine-broker-$login_uid"
broker_uid="$(id -u "$broker_user")"
signer_uid="$(id -u "$signer_user")"
broker_gid="$(id -g "$broker_user")"
signer_gid="$(id -g "$signer_user")"
session_gid="$(getent group "$session_group" | cut -d: -f3)"
for value in "$broker_uid" "$signer_uid" "$broker_gid" "$signer_gid" "$session_gid"; do
  [[ "$value" =~ ^[1-9][0-9]*$ ]] || die "system principal allocation is incomplete"
done
[[ "$broker_uid" != "$login_uid" && "$signer_uid" != "$login_uid" &&
  "$broker_uid" != "$signer_uid" ]] || die "system principals are not distinct"

for relative in \
  bin/bloom SHA256SUMS installer/linux/config/edge-manifest.json.in \
  installer/linux/config/broker.json.in installer/linux/config/signer.json.in \
  installer/linux/config/provenance-catalog.unsigned.json
do
  [[ -f "$payload/$relative" && ! -L "$payload/$relative" ]] ||
    die "payload is missing $relative"
done
release_digest="$(sha256sum "$payload/SHA256SUMS" | awk '{print $1}')"
[[ "$release_digest" =~ ^[0-9a-f]{64}$ ]] || die "invalid release digest"

base=/etc/bloom
target="$base/$login_uid"
transactions="$base/.transactions"
transaction="$transactions/$login_uid"
lock_root=/run/lock/bloom
secure_control_directory() {
  local directory="$1" mode="$2"
  if [[ -e "$directory" || -L "$directory" ]]; then
    [[ -d "$directory" && ! -L "$directory" && "$(stat -c %u "$directory")" == 0 ]] ||
      die "unsafe enrollment control directory $directory"
  else
    mkdir -m "$mode" "$directory"
  fi
  chmod "$mode" "$directory"
}
secure_control_directory "$base" 0755
secure_control_directory "$transactions" 0700
secure_control_directory "$lock_root" 0700
exec 9>"$lock_root/enroll-$login_uid.lock"
flock -x 9

remove_tree() {
  local path="$1"
  [[ -d "$path" && ! -L "$path" ]] || return 0
  find "$path" -depth -delete
}

sync_path() { sync -f "$1"; }

write_phase() {
  local phase="$1" temporary="$transaction/phase.new.$$"
  printf '%s\n' "$phase" >"$temporary"
  chmod 0600 "$temporary"
  sync_path "$temporary"
  mv -f "$temporary" "$transaction/phase"
  sync_path "$transaction"
}

discard_transaction() {
  local retired="$transactions/.committed-$login_uid.$$"
  mv -T "$transaction" "$retired"
  sync_path "$transactions"
  remove_tree "$retired"
  sync_path "$transactions"
}

disable_instance() {
  systemctl disable --now \
    "bloom-signer-rpc@$login_uid.socket" \
    "bloom-signer-control@$login_uid.socket" \
    "bloom-broker-rpc@$login_uid.socket" \
    "bloom-broker-control@$login_uid.socket" \
    "bloom-broker-ceremony@$login_uid.socket" >/dev/null 2>&1 || true
  systemctl stop "bloom-broker@$login_uid.service" \
    "bloom-signer@$login_uid.service" >/dev/null 2>&1 || true
}

recover() {
  [[ -e "$transaction" ]] || return 0
  [[ -d "$transaction" && ! -L "$transaction" ]] || die "unsafe transaction record"
  grep -Fx 'bloom.linux-enrollment-transaction.1' "$transaction/schema" >/dev/null ||
    die "invalid transaction schema"
  local phase staging recorded_target recorded_staging
  phase="$(<"$transaction/phase")"
  recorded_target="$(<"$transaction/target")"
  recorded_staging="$(<"$transaction/staging")"
  [[ "$recorded_target" == "$target" ]] || die "transaction target mismatch"
  case "$recorded_staging" in "$base/.staging-$login_uid."*) ;; *) die "unsafe staging path";; esac
  staging="$recorded_staging"
  if [[ "$phase" == committed ]]; then
    discard_transaction
    return 0
  fi
  disable_instance
  if [[ -d "$target" && ! -L "$target" ]]; then
    mkdir -p "$staging"
    [[ ! -e "$staging/tree" ]] || die "recovery staging tree already exists"
    mv -T "$target" "$staging/tree"
    sync_path "$base"
  fi
  remove_tree "$staging"
  remove_tree "$transaction"
  sync_path "$base"
  echo "recovered interrupted enrollment for login UID $login_uid" >&2
}

recover
while IFS= read -r abandoned; do
  [[ -d "$abandoned" && ! -L "$abandoned" ]] || die "unsafe abandoned transaction record"
  remove_tree "$abandoned"
done < <(find "$transactions" -mindepth 1 -maxdepth 1 \
  \( -name ".new-$login_uid.*" -o -name ".committed-$login_uid.*" \) -print)
sync_path "$transactions"
if [[ -e "$target" ]]; then
  [[ -d "$target" && ! -L "$target" ]] || die "installed enrollment root is unsafe"
  for relative in edge-manifest.json provenance-catalog.json \
    machine/identity.json machine/revoke-identity.json session/identity.json \
    installer/identity.json broker/config.json broker/identity.json \
    signer/config.json signer/identity.json
  do
    [[ -f "$target/$relative" && ! -L "$target/$relative" ]] ||
      die "installed enrollment is incomplete; refusing regeneration"
  done
  exit 0
fi

staging="$(mktemp -d "$base/.staging-$login_uid.XXXXXXXX")"
chmod 0700 "$staging"
material="$staging/material"
tree="$staging/tree"
mkdir -m 0700 "$material" "$tree" "$tree/machine" "$tree/session" \
  "$tree/installer" "$tree/broker" "$tree/signer"

cleanup_unpublished() {
  local status=$?
  trap - EXIT INT TERM HUP
  if [[ -d "$transaction" && ! -L "$transaction" ]]; then
    recover || true
  elif [[ ! -e "$target" ]]; then
    remove_tree "$staging"
  fi
  exit "$status"
}
trap cleanup_unpublished EXIT INT TERM HUP

"$payload/bin/bloom" init triad-render-linux-enrollment \
  "$payload/installer/linux/config" "$material" "$login_uid" \
  "$broker_uid" "$signer_uid" "$session_gid" "$release_digest"

install -m 0644 "$material/edge-manifest.json" "$tree/edge-manifest.json"
install -m 0644 "$material/provenance-catalog.json" "$tree/provenance-catalog.json"
install -m 0600 "$material/machine-identity.json" "$tree/machine/identity.json"
install -m 0600 "$material/revoke-identity.json" "$tree/machine/revoke-identity.json"
install -m 0600 "$material/session-identity.json" "$tree/session/identity.json"
install -m 0600 "$material/installer-identity.json" "$tree/installer/identity.json"
install -m 0600 "$material/broker.json" "$tree/broker/config.json"
install -m 0600 "$material/broker-identity.json" "$tree/broker/identity.json"
install -m 0600 "$material/signer.json" "$tree/signer/config.json"
install -m 0600 "$material/signer-identity.json" "$tree/signer/identity.json"
printf '%s\n' '{"schema":"bloom.machine-audit-trust.v1","predecessors":[]}' \
  >"$tree/machine-audit-history.json"
printf '%s\n' '{"schema":"bloom.authority-edge-application-history.1","historical_keys":[],"handovers":[]}' \
  >"$tree/authority-edge-history.json"
chmod 0644 "$tree/machine-audit-history.json" "$tree/authority-edge-history.json"

chown -R root:root "$tree"
chown "$login_uid:$login_gid" "$tree/machine" "$tree/machine/identity.json" \
  "$tree/machine/revoke-identity.json" "$tree/session" "$tree/session/identity.json"
chown "$broker_uid:$broker_gid" "$tree/broker" "$tree/broker/"*.json
chown "$signer_uid:$signer_gid" "$tree/signer" "$tree/signer/"*.json
chmod 0711 "$tree"

while IFS= read -r file; do
  [[ -f "$file" && ! -L "$file" && "$(stat -c %h "$file")" == 1 ]] ||
    die "candidate contains an unsafe file"
  sync_path "$file"
done < <(find "$tree" -type f -print)
sync_path "$tree"

transaction_tmp="$transactions/.new-$login_uid.$$"
mkdir -m 0700 "$transaction_tmp"
printf '%s\n' 'bloom.linux-enrollment-transaction.1' >"$transaction_tmp/schema"
printf '%s\n' prepared >"$transaction_tmp/phase"
printf '%s\n' "$target" >"$transaction_tmp/target"
printf '%s\n' "$staging" >"$transaction_tmp/staging"
printf '%s\n' "$release_digest" >"$transaction_tmp/release-digest"
chmod 0600 "$transaction_tmp/"*
for file in "$transaction_tmp/"*; do sync_path "$file"; done
sync_path "$transaction_tmp"
mv -T "$transaction_tmp" "$transaction"
sync_path "$transactions"

mv -T "$tree" "$target"
sync_path "$base"
write_phase published

systemctl daemon-reload
systemctl enable --now \
  "bloom-signer-rpc@$login_uid.socket" \
  "bloom-signer-control@$login_uid.socket" \
  "bloom-broker-rpc@$login_uid.socket" \
  "bloom-broker-control@$login_uid.socket" \
  "bloom-broker-ceremony@$login_uid.socket"
runuser -u "$login_user" -- "$payload/bin/bloom" serve triad-health-check "$release_digest"
write_phase committed
discard_transaction
remove_tree "$staging"
trap - EXIT INT TERM HUP
