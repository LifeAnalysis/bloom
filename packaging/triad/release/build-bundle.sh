#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 4 ]]; then
  echo "usage: $0 STAGING_DIR OUTPUT_ARCHIVE ED25519_SIGNING_KEY SOURCE_DATE_EPOCH" >&2
  exit 64
fi

staging="$(cd "$1" && pwd -P)"
output="$2"
signing_key="$3"
source_date_epoch="$4"
tar_command="${TAR:-tar}"
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

[[ "$source_date_epoch" =~ ^[0-9]+$ ]] || {
  echo "SOURCE_DATE_EPOCH must be an unsigned decimal integer" >&2
  exit 64
}
for binary in bloom bloom-broker bloom-signer; do
  test -f "$staging/bin/$binary" || {
    echo "missing production binary: bin/$binary" >&2
    exit 66
  }
done
for forbidden in \
  bloom-broker-debug-driver \
  broker-audit-test-1 \
  accepting_verifier \
  mint_approval \
  bloom.sign-hash \
  test-only-release-key \
  test_credential
do
  if LC_ALL=C grep -aR -F "$forbidden" "$staging" >/dev/null; then
    echo "forbidden production artifact marker: $forbidden" >&2
    exit 65
  fi
done
machine_version="$(sed -n -E 's/^machine = "([^"]+)"$/\1/p' "$script_dir/compatibility-v1.toml")"
broker_version="$(sed -n -E 's/^broker = "([^"]+)"$/\1/p' "$script_dir/compatibility-v1.toml")"
signer_version="$(sed -n -E 's/^signer = "([^"]+)"$/\1/p' "$script_dir/compatibility-v1.toml")"
for identity in \
  "bloom:$machine_version" \
  "bloom-broker:$broker_version" \
  "bloom-signer:$signer_version"
do
  binary="${identity%%:*}"
  expected="${identity#*:}"
  actual="$("$staging/bin/$binary" --version | awk '{print $2}')"
  [[ "$actual" == "$expected" ]] || {
    echo "$binary version $actual is outside the compatibility matrix ($expected)" >&2
    exit 65
  }
done
test -f "$signing_key" || {
  echo "missing Ed25519 bundle signing key" >&2
  exit 66
}

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
payload="$work/bloom-triad"
mkdir -p "$payload"
cp -R "$staging/." "$payload/"
platform_claim="${BLOOM_PLATFORM_CLAIM:-test-unclaimed}"
case "$platform_claim" in
  linux)
    for binary in bloom bloom-broker bloom-signer; do
      file -b "$staging/bin/$binary" | grep -F 'ELF ' >/dev/null || {
        echo "Linux platform claim requires ELF production binaries" >&2
        exit 65
      }
    done
    ;;
  macos-unix-principals-w0)
    [[ "$(uname -s)" == "Darwin" ]] &&
      [[ "${BLOOM_ALLOW_MACOS_UNIX_W0:-}" == "true" ]] || {
      echo "macOS W0 claim requires its disposable Darwin build lane" >&2
      exit 65
    }
    for binary in bloom bloom-broker bloom-signer; do
      file -b "$staging/bin/$binary" | grep -F 'Mach-O ' >/dev/null || {
        echo "macOS W0 claim requires Mach-O production binaries" >&2
        exit 65
      }
    done
    ;;
  test-unclaimed)
    [[ "${BLOOM_ALLOW_TEST_UNCLAIMED:-}" == "true" ]] || {
      echo "test-unclaimed bundles require BLOOM_ALLOW_TEST_UNCLAIMED=true" >&2
      exit 65
    }
    ;;
  *)
    echo "BLOOM_PLATFORM_CLAIM is invalid" >&2
    exit 64
    ;;
esac
printf '%s\n' "$platform_claim" > "$payload/PLATFORM_CLAIM"
install -m 0644 "$script_dir/compatibility-v1.toml" "$payload/compatibility-v1.toml"
mkdir -p "$payload/installer/release"
cp -R "$script_dir/../linux" "$payload/installer/linux"
cp -R "$script_dir/../macos" "$payload/installer/macos"
install -m 0755 \
  "$script_dir/install-linux.sh" \
  "$script_dir/install-macos.sh" \
  "$payload/installer/release/"
install -m 0755 "$script_dir/verify-bundle.sh" "$payload/installer/release/"

for forbidden in \
  bloom-broker-debug-driver \
  broker-audit-test-1 \
  accepting_verifier \
  mint_approval \
  bloom.sign-hash \
  test-only-release-key \
  test_credential
do
  if LC_ALL=C grep -aR -F "$forbidden" "$payload" >/dev/null; then
    echo "forbidden packaged artifact marker: $forbidden" >&2
    exit 65
  fi
done

for revision_name in BLOOM_MACHINE_SHA BLOOM_BROKER_SHA BLOOM_SIGNER_SHA; do
  revision="${!revision_name:-}"
  [[ "$revision" =~ ^[0-9a-f]{7,64}$ ]] || {
    echo "$revision_name must be a lowercase git commit ID" >&2
    exit 64
  }
  printf '%s=%s\n' "$revision_name" "$revision"
done | LC_ALL=C sort > "$payload/SOURCE_REVISIONS"

openssl pkey -in "$signing_key" -pubout -out "$payload/RELEASE_PUBLIC_KEY.pem" 2>/dev/null
(
  cd "$payload"
  find . -type f ! -name SHA256SUMS ! -name SHA256SUMS.new ! -name RELEASE_SIGNATURE -print |
    LC_ALL=C sort |
    while IFS= read -r file; do
      shasum -a 256 "$file"
    done
) > "$payload/SHA256SUMS.new"
mv "$payload/SHA256SUMS.new" "$payload/SHA256SUMS"
openssl pkeyutl \
  -sign \
  -rawin \
  -inkey "$signing_key" \
  -in "$payload/SHA256SUMS" \
  -out "$payload/RELEASE_SIGNATURE"
# The single-quoted expression is Perl source, not a shell interpolation.
# shellcheck disable=SC2016
find "$payload" -print0 |
  xargs -0 perl -e '$timestamp = shift; utime $timestamp, $timestamp, @ARGV' \
    "$source_date_epoch"

mkdir -p "$(dirname "$output")"
output_dir="$(cd "$(dirname "$output")" && pwd -P)"
output="$output_dir/$(basename "$output")"
archive_tmp="$work/archive.tar"
(
  cd "$work"
  find bloom-triad -print | LC_ALL=C sort > archive-files
  "$tar_command" \
  --format=ustar \
  --uid=0 \
  --gid=0 \
  --uname=root \
  --gname=root \
  --no-recursion \
  -cf "$archive_tmp" \
  -T archive-files
)
gzip -n -9 < "$archive_tmp" > "$output"
(
  cd "$output_dir"
  shasum -a 256 "$(basename "$output")" > "$(basename "$output").sha256"
)
openssl pkeyutl -sign -rawin -inkey "$signing_key" -in "$output.sha256" -out "$output.sig"
openssl pkey -in "$signing_key" -pubout -out "$output.pub" 2>/dev/null
