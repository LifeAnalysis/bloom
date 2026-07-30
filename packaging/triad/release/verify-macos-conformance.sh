#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 1 || $# -gt 2 ]]; then
  echo "usage: $0 PAYLOAD_DIR [EXPECTED_CONFORMANCE_KEY_SHA256]" >&2
  exit 64
fi

payload="$(cd "$1" && pwd -P)"
expected_key_digest="${2:-}"
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
report="$payload/MACOS_CONFORMANCE_REPORT.json"
signature="$payload/MACOS_CONFORMANCE_REPORT.sig"
public_key="$payload/MACOS_CONFORMANCE_REPORT.pub"

[[ "$(uname -s)" == "Darwin" ]] || {
  echo "production macOS conformance is verified only on Darwin" >&2
  exit 69
}
for input in "$report" "$signature" "$public_key"; do
  [[ -f "$input" && ! -L "$input" ]] || {
    echo "macOS conformance input is missing, substituted, or a symlink: $input" >&2
    exit 65
  }
done
if [[ -n "$expected_key_digest" ]]; then
  [[ "$expected_key_digest" =~ ^[0-9a-f]{64}$ ]] || {
    echo "expected macOS conformance key digest is invalid" >&2
    exit 64
  }
  observed_key_digest="$(shasum -a 256 "$public_key" | awk '{print $1}')"
  [[ "$observed_key_digest" == "$expected_key_digest" ]] || {
    echo "macOS conformance key does not match the release-policy pin" >&2
    exit 65
  }
fi
openssl pkeyutl \
  -verify \
  -rawin \
  -pubin \
  -inkey "$public_key" \
  -in "$report" \
  -sigfile "$signature" >/dev/null

field() {
  plutil -extract "$1" raw -o - "$report"
}

[[ "$(field schema)" == "bloom.macos-unix-conformance.1" ]]
[[ "$(field platform_claim)" == "macos-unix-principals" ]]
subject_digest="$(field release_subject_digest)"
candidate_digest="$(field w0_candidate_archive_sha256)"
completed_at="$(field completed_at_utc)"
host_os_build="$(field host_os_build)"
host_architecture="$(field host_architecture)"
report_id="$(field report_id)"
[[ "$subject_digest" =~ ^[0-9a-f]{64}$ ]]
[[ "$candidate_digest" =~ ^[0-9a-f]{64}$ ]]
[[ "$completed_at" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$ ]]
[[ "$host_os_build" =~ ^[A-Za-z0-9._()-]{1,128}$ ]]
[[ "$host_architecture" == "arm64" || "$host_architecture" == "x86_64" ]]
[[ "$report_id" =~ ^[A-Za-z0-9._:-]{1,128}$ ]]

observed_subject="$("$script_dir/macos-conformance-subject.sh" "$payload")"
[[ "$observed_subject" == "$subject_digest" ]] || {
  echo "macOS conformance report does not bind this release subject" >&2
  exit 65
}

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
  [[ "$(field "evidence.$criterion")" == "true" ]] || {
    echo "macOS conformance evidence is incomplete: $criterion" >&2
    exit 65
  }
done

for revision_and_field in \
  "BLOOM_MACHINE_SHA source_revisions.machine" \
  "BLOOM_BROKER_SHA source_revisions.broker" \
  "BLOOM_SIGNER_SHA source_revisions.signer"
do
  revision_name="${revision_and_field%% *}"
  report_field="${revision_and_field#* }"
  expected_revision="$(
    sed -n "s/^$revision_name=//p" "$payload/SOURCE_REVISIONS"
  )"
  observed_revision="$(field "$report_field")"
  [[ "$expected_revision" =~ ^[0-9a-f]{7,64}$ ]]
  [[ "$observed_revision" == "$expected_revision" ]] || {
    echo "macOS conformance report source revision mismatch: $revision_name" >&2
    exit 65
  }
done
