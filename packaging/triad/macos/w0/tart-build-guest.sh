#!/bin/bash
set -Eeuo pipefail

export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin"
export CARGO_TARGET_DIR="$HOME/Library/Caches/bloom-w0-target"

readonly shared_root="/Volumes/My Shared Files"
readonly main_root="$shared_root/bloom"
readonly broker_root="$shared_root/bloom-broker"
readonly signer_root="$shared_root/bloom-signer"
readonly output_root="$shared_root/output"
readonly staging_root="$output_root/triad-staging"
readonly distribution_root="$output_root/triad-dist"
readonly verified_root="$output_root/verified"
readonly release_key="$output_root/w0-release-key"

[[ "$(uname -s)" == "Darwin" ]] || {
  echo "Tart W0 guest build requires Darwin" >&2
  exit 69
}
for path in "$main_root" "$broker_root" "$signer_root" "$output_root"; do
  [[ -d "$path" ]] || {
    echo "missing Tart shared directory: $path" >&2
    exit 69
  }
done
for command_name in cargo git ssh-keygen tar; do
  command -v "$command_name" >/dev/null 2>&1 || {
    echo "missing Tart W0 guest build dependency: $command_name" >&2
    exit 69
  }
done

mkdir -p \
  "$staging_root/bin" \
  "$distribution_root" \
  "$verified_root"

"$main_root/packaging/triad/release/check-machine-authority-boundary.sh" \
  --require-clean

cargo build \
  --manifest-path "$main_root/Cargo.toml" \
  --release \
  -p bloom \
  --locked
cargo build \
  --manifest-path "$broker_root/Cargo.toml" \
  --release \
  -p bloom-broker \
  --locked
cargo build \
  --manifest-path "$signer_root/Cargo.toml" \
  --release \
  -p bloom-signer \
  --locked

cp "$CARGO_TARGET_DIR/release/bloom" "$staging_root/bin/"
cp "$CARGO_TARGET_DIR/release/bloom-broker" "$staging_root/bin/"
cp "$CARGO_TARGET_DIR/release/bloom-signer" "$staging_root/bin/"

ssh-keygen -q -t ed25519 -N '' -f "$release_key"

export BLOOM_PLATFORM_CLAIM=macos-unix-principals-w0
export BLOOM_ALLOW_MACOS_UNIX_W0=true
export BLOOM_MACHINE_SHA
export BLOOM_BROKER_SHA
export BLOOM_SIGNER_SHA
BLOOM_MACHINE_SHA="$(git -C "$main_root" rev-parse HEAD)"
BLOOM_BROKER_SHA="$(git -C "$broker_root" rev-parse HEAD)"
BLOOM_SIGNER_SHA="$(git -C "$signer_root" rev-parse HEAD)"

"$main_root/packaging/triad/release/build-bundle.sh" \
  "$staging_root" \
  "$distribution_root/bloom-triad.tar.gz" \
  "$release_key" \
  1700000000
"$main_root/packaging/triad/release/verify-bundle.sh" \
  "$distribution_root/bloom-triad.tar.gz" \
  "$distribution_root/bloom-triad.tar.gz.sha256" \
  "$distribution_root/bloom-triad.tar.gz.sig" \
  "$distribution_root/bloom-triad.tar.gz.pub"

tar -xzf \
  "$distribution_root/bloom-triad.tar.gz" \
  -C "$verified_root"

echo "local Tart W0 candidate built at $verified_root/bloom-triad"
