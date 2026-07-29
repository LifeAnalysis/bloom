#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 4 ]]; then
  echo "usage: $0 ARCHIVE SHA256_FILE SIGNATURE PUBLIC_KEY" >&2
  exit 64
fi

archive="$1"
checksum="$2"
signature="$3"
public_key="$4"

openssl pkeyutl -verify -rawin -pubin -inkey "$public_key" \
  -in "$checksum" -sigfile "$signature" >/dev/null
(
  cd "$(dirname "$archive")"
  sha256sum -c "$(basename "$checksum")"
)

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
tar -xzf "$archive" -C "$work"
payload="$work/bloom-triad"
for required in \
  bin/bloom \
  bin/bloom-broker \
  bin/bloom-signer \
  PLATFORM_CLAIM \
  compatibility-v1.toml \
  installer/release/install-linux.sh \
  installer/release/install-macos.sh \
  SOURCE_REVISIONS \
  SHA256SUMS
do
  test -f "$payload/$required" || {
    echo "bundle is missing $required" >&2
    exit 65
  }
done
(
  cd "$payload"
  sha256sum -c SHA256SUMS
)
grep -Fx 'downgrade = "forbidden"' "$payload/compatibility-v1.toml" >/dev/null
grep -Fx 'adjacent_versions_supported = false' "$payload/compatibility-v1.toml" >/dev/null
platform_claim="$(<"$payload/PLATFORM_CLAIM")"
if [[ "$platform_claim" != "linux" ]] &&
  [[ ! ("$platform_claim" == "test-unclaimed" &&
    "${BLOOM_ALLOW_TEST_UNCLAIMED:-}" == "true") ]]
then
  echo "bundle has no verifiable production platform claim" >&2
  exit 65
fi
if [[ "$platform_claim" == "linux" ]]; then
  for binary in bloom bloom-broker bloom-signer; do
    file -b "$payload/bin/$binary" | grep -F 'ELF ' >/dev/null || {
      echo "Linux bundle contains a non-ELF production binary" >&2
      exit 65
    }
  done
fi
