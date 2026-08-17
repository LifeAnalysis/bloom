#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 9 ]]; then
  echo "usage: $0 PAYLOAD_DIR W0_ARCHIVE_SHA256 COMPLETED_AT_UTC HOST_OS_BUILD HOST_ARCH REPORT_ID EVIDENCE_DIR ED25519_KEY OUTPUT_DIR" >&2
  exit 64
fi

payload="$(cd "$1" && pwd -P)"
candidate_digest="$2"
completed_at="$3"
host_os_build="$4"
host_architecture="$5"
report_id="$6"
evidence_dir="$(cd "$7" && pwd -P)"
private_key="$8"
output_dir="$(cd "$9" && pwd -P)"
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

[[ "$candidate_digest" =~ ^[0-9a-f]{64}$ ]]
[[ "$completed_at" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$ ]]
[[ "$host_os_build" =~ ^[A-Za-z0-9._()-]{1,128}$ ]]
[[ "$host_architecture" == "arm64" || "$host_architecture" == "x86_64" ]]
[[ "$report_id" =~ ^[A-Za-z0-9._:-]{1,128}$ ]]
[[ -f "$private_key" && ! -L "$private_key" ]] || {
  echo "macOS conformance signing key is missing or substituted" >&2
  exit 66
}
[[ -d "$evidence_dir" && ! -L "$evidence_dir" ]]
[[ -d "$output_dir" && ! -L "$output_dir" ]]

subject_digest="$("$script_dir/macos-conformance-subject.sh" "$payload")"
for criterion in \
  mui_01 \
  mui_02 \
  mui_03 \
  mui_04 \
  mui_05 \
  mui_06 \
  mui_07 \
  mui_08 \
  mui_09 \
  mui_10 \
  mui_11 \
  mui_12 \
  installed_ac_01_35 \
  negative_access \
  two_login_lifecycle
do
  evidence="$evidence_dir/$criterion.pass"
  [[ -f "$evidence" && ! -L "$evidence" ]] || {
    echo "macOS conformance evidence is missing or substituted: $criterion" >&2
    exit 65
  }
  [[ "$(<"$evidence")" == "$subject_digest" ]] || {
    echo "macOS conformance evidence does not bind this release: $criterion" >&2
    exit 65
  }
done

revision() {
  value="$(sed -n "s/^$1=//p" "$payload/SOURCE_REVISIONS")"
  [[ "$value" =~ ^[0-9a-f]{7,64}$ ]]
  printf '%s' "$value"
}

machine_revision="$(revision BLOOM_MACHINE_SHA)"
broker_revision="$(revision BLOOM_BROKER_SHA)"
signer_revision="$(revision BLOOM_SIGNER_SHA)"
for destination in \
  MACOS_CONFORMANCE_REPORT.json \
  MACOS_CONFORMANCE_REPORT.sig \
  MACOS_CONFORMANCE_REPORT.pub
do
  [[ ! -e "$output_dir/$destination" ]] || {
    echo "refusing to overwrite macOS conformance output: $destination" >&2
    exit 65
  }
done

work="$(mktemp -d "$output_dir/.macos-conformance.XXXXXX")"
trap 'rm -rf "$work"' EXIT
report="$work/MACOS_CONFORMANCE_REPORT.json"
cat > "$report" <<EOF
{
  "schema": "bloom.macos-unix-conformance.1",
  "platform_claim": "macos-unix-principals",
  "release_subject_digest": "$subject_digest",
  "w0_candidate_archive_sha256": "$candidate_digest",
  "completed_at_utc": "$completed_at",
  "host_os_build": "$host_os_build",
  "host_architecture": "$host_architecture",
  "report_id": "$report_id",
  "source_revisions": {
    "machine": "$machine_revision",
    "broker": "$broker_revision",
    "signer": "$signer_revision"
  },
  "evidence": {
    "mui_01": true,
    "mui_02": true,
    "mui_03": true,
    "mui_04": true,
    "mui_05": true,
    "mui_06": true,
    "mui_07": true,
    "mui_08": true,
    "mui_09": true,
    "mui_10": true,
    "mui_11": true,
    "mui_12": true,
    "installed_ac_01_35": true,
    "negative_access": true,
    "two_login_lifecycle": true
  }
}
EOF
"$script_dir/ssh-ed25519-sign.sh" \
  "$private_key" \
  bloom-macos-conformance-v1 \
  "$report" \
  "$work/MACOS_CONFORMANCE_REPORT.sig"
"$script_dir/ssh-ed25519-public-key.sh" \
  "$private_key" \
  "$work/MACOS_CONFORMANCE_REPORT.pub"
chmod 0644 "$work"/MACOS_CONFORMANCE_REPORT.*
mv "$work"/MACOS_CONFORMANCE_REPORT.* "$output_dir/"
