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
  shasum -a 256 -c "$(basename "$checksum")"
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
  RELEASE_PUBLIC_KEY.pem \
  RELEASE_SIGNATURE \
  SHA256SUMS
do
  test -f "$payload/$required" || {
    echo "bundle is missing $required" >&2
    exit 65
  }
done
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
)
grep -Fx 'downgrade = "forbidden"' "$payload/compatibility-v1.toml" >/dev/null
grep -Fx 'adjacent_versions_supported = false' "$payload/compatibility-v1.toml" >/dev/null
platform_claim="$(<"$payload/PLATFORM_CLAIM")"
case "$platform_claim" in
  linux)
    for binary in bloom bloom-broker bloom-signer; do
      file -b "$payload/bin/$binary" | grep -F 'ELF ' >/dev/null || {
        echo "Linux bundle contains a non-ELF production binary" >&2
        exit 65
      }
    done
    ;;
  macos-unix-principals-w0)
    [[ "$(uname -s)" == "Darwin" ]] &&
      [[ "${BLOOM_ALLOW_MACOS_UNIX_W0:-}" == "true" ]] || {
      echo "macOS W0 bundle requires its disposable Darwin verification lane" >&2
      exit 65
    }
    for binary in bloom bloom-broker bloom-signer; do
      file -b "$payload/bin/$binary" | grep -F 'Mach-O ' >/dev/null || {
        echo "macOS W0 bundle contains a non-Mach-O production binary" >&2
        exit 65
      }
    done
    ;;
  test-unclaimed)
    [[ "${BLOOM_ALLOW_TEST_UNCLAIMED:-}" == "true" ]] || {
      echo "test-unclaimed bundle verification was not explicitly enabled" >&2
      exit 65
    }
    ;;
  *)
    echo "bundle has no verifiable production platform claim" >&2
    exit 65
    ;;
esac
if [[ "$platform_claim" == "macos-unix-principals" ]]; then
  if find "$payload" -type f \
    \( -name '*identity*.json' -o -name '*credentials*' \) |
    grep . >/dev/null
  then
    echo "production macOS bundle contains a private identity-shaped file" >&2
    exit 65
  fi
  if LC_ALL=C grep -aER \
    '"[^"]*(private_key_seed_hex|signing_seed_hex|state_authentication_key_hex)"[[:space:]]*:[[:space:]]*"[0-9a-f]{64}"' \
    "$payload" >/dev/null
  then
    echo "production macOS bundle contains private key material" >&2
    exit 65
  fi
fi
