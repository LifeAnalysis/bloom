#!/usr/bin/env bash
set -euo pipefail

main_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd -P)"
broker_root="${BLOOM_BROKER_ROOT:-$main_root/../bloom-broker}"
signer_root="${BLOOM_SIGNER_ROOT:-$main_root/../bloom-signer}"
test_key=false
if [[ "${1:-}" == "--test-signing-key" ]]; then
  test_key=true
elif [[ $# -ne 0 ]]; then
  echo "usage: $0 [--test-signing-key]" >&2
  exit 64
fi
for root in "$main_root" "$broker_root" "$signer_root"; do
  test -f "$root/Cargo.toml" || {
    echo "missing triad workspace: $root" >&2
    exit 66
  }
  git -C "$root" diff --quiet
  git -C "$root" diff --cached --quiet
  while IFS= read -r untracked; do
    case "$root/$untracked" in
      "$main_root/docs/.DS_Store"|\
      "$main_root/docs/specs/2026-07-23-triad-process-architecture.md"|\
      "$main_root/docs/specs/2026-07-24-triad-implementability-review.md")
        ;;
      *)
        echo "untracked input prevents source attribution: $root/$untracked" >&2
        exit 65
        ;;
    esac
  done < <(git -C "$root" ls-files --others --exclude-standard)
done

"$main_root/packaging/triad/release/check-machine-authority-boundary.sh" --require-clean
python3 "$main_root/packaging/triad/release/check-legacy-hash-only-routes.py"
python3 "$main_root/packaging/triad/release/check-external-pins.py" --remote \
  "$main_root/Cargo.toml" "$broker_root/Cargo.toml" "$signer_root/Cargo.toml"
python3 "$main_root/packaging/triad/release/check-default-petal-releases.py" --remote

resolved_machine_features="$(cargo tree --manifest-path "$main_root/Cargo.toml" -p bloom -e normal,build,features --prefix none)"
for forbidden_feature in unsigned-audit-test-seam audit-test-seam; do
  if grep -F "feature \"$forbidden_feature\"" <<<"$resolved_machine_features" >/dev/null; then
    echo "forbidden production Machine feature resolved: $forbidden_feature" >&2
    exit 65
  fi
done

for root in "$main_root" "$broker_root" "$signer_root"; do
  (
    cd "$root"
    # cargo-audit scans a few optional lockfile entries that Cargo proves are
    # absent from every resolved target graph. Only those exact advisories are
    # ignored; reachable vulnerabilities and memory-unsound warnings remain
    # release-blocking. Other warnings remain visible for review.
    cargo audit --deny unsound \
      --ignore RUSTSEC-2026-0235 \
      --ignore RUSTSEC-2025-0055 \
      --ignore RUSTSEC-2026-0205
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets --locked -- -D warnings
  )
done

cargo build --manifest-path "$main_root/Cargo.toml" --release -p bloom --locked
cargo build --manifest-path "$broker_root/Cargo.toml" --release -p bloom-broker --locked
cargo build --manifest-path "$signer_root/Cargo.toml" --release -p bloom-signer --locked
if [[ "$(uname -s)" == Linux ]]; then
  BLOOM_LINUX_TEST_BINARY="$main_root/target/release/bloom" \
    "$main_root/packaging/triad/linux/tests/enrollment-crash.sh"
fi

work="$(mktemp -d)"
trap 'find "$work" -depth -delete' EXIT
mkdir -p "$work/staging/bin" "$work/dist-a" "$work/dist-b"
cp "$main_root/target/release/bloom" "$work/staging/bin/"
cp "$broker_root/target/release/bloom-broker" "$work/staging/bin/"
cp "$signer_root/target/release/bloom-signer" "$work/staging/bin/"
cp "$signer_root/target/release/bloom-signer-migrate" "$work/staging/bin/"
if $test_key; then
  signing_key="$work/test-only-release-key"
  /usr/bin/ssh-keygen -q -t ed25519 -N '' -f "$signing_key"
  export BLOOM_PLATFORM_CLAIM="test-unclaimed"
  export BLOOM_ALLOW_TEST_UNCLAIMED="true"
else
  signing_key="${TRIAD_RELEASE_SIGNING_KEY:-}"
  test -f "$signing_key" || {
    echo "TRIAD_RELEASE_SIGNING_KEY must name the reviewed Ed25519 release key" >&2
    exit 66
  }
  case "$(uname -s)" in
    Linux) export BLOOM_PLATFORM_CLAIM="linux" ;;
    Darwin)
      export BLOOM_PLATFORM_CLAIM="macos-unix-principals"
      for evidence_name in \
        BLOOM_MACOS_CONFORMANCE_REPORT \
        BLOOM_MACOS_CONFORMANCE_SIGNATURE \
        BLOOM_MACOS_CONFORMANCE_PUBLIC_KEY \
        BLOOM_MACOS_CONFORMANCE_KEY_SHA256
      do
        [[ -n "${!evidence_name:-}" ]] || {
          echo "$evidence_name is required for a production macOS release" >&2
          exit 66
        }
      done
      ;;
    *)
      echo "unsupported release host" >&2
      exit 69
      ;;
  esac
fi

export BLOOM_MACHINE_SHA BLOOM_BROKER_SHA BLOOM_SIGNER_SHA
BLOOM_MACHINE_SHA="$(git -C "$main_root" rev-parse HEAD)"
BLOOM_BROKER_SHA="$(git -C "$broker_root" rev-parse HEAD)"
BLOOM_SIGNER_SHA="$(git -C "$signer_root" rev-parse HEAD)"
builder="$main_root/packaging/triad/release/build-bundle.sh"
verifier="$main_root/packaging/triad/release/verify-bundle.sh"
for output in \
  "$work/dist-a/bloom-triad.tar.gz" \
  "$work/dist-b/bloom-triad.tar.gz"
do
  "$builder" "$work/staging" "$output" "$signing_key" "${SOURCE_DATE_EPOCH:-1700000000}"
  "$verifier" "$output" "$output.sha256" "$output.sig" "$output.pub"
done
cmp "$work/dist-a/bloom-triad.tar.gz" "$work/dist-b/bloom-triad.tar.gz"
mkdir -p "$work/verified"
tar -xzf "$work/dist-a/bloom-triad.tar.gz" -C "$work/verified"
bundle="$work/verified/bloom-triad"
grep -Fx "BLOOM_MACHINE_SHA=$BLOOM_MACHINE_SHA" "$bundle/SOURCE_REVISIONS" >/dev/null
grep -Fx "BLOOM_BROKER_SHA=$BLOOM_BROKER_SHA" "$bundle/SOURCE_REVISIONS" >/dev/null
grep -Fx "BLOOM_SIGNER_SHA=$BLOOM_SIGNER_SHA" "$bundle/SOURCE_REVISIONS" >/dev/null
for binary in bloom bloom-broker bloom-signer; do
  test "$("$bundle/bin/$binary" --version)" = \
    "$("$work/staging/bin/$binary" --version)"
done
for root in "$main_root" "$broker_root" "$signer_root"; do
  (
    cd "$root"
    BLOOM_ACCEPTANCE_BUNDLE_ROOT="$bundle" cargo test --workspace --locked
  )
done
install_payload="$work/install-payload"
cp -R "$bundle" "$install_payload"
if [[ "$BLOOM_PLATFORM_CLAIM" == "macos-unix-principals" ]]; then
  "$bundle/installer/release/verify-macos-conformance.sh" "$bundle"
else
  mkdir -p "$install_payload/config"
  for config in \
    edge-manifest.json \
    broker.json \
    signer.json \
    broker-identity.json \
    signer-identity.json
  do
    printf '{}\n' > "$install_payload/config/$config"
  done
  printf 'time.cloudflare.com\ntime.nist.gov\n' \
    > "$install_payload/config/nts-servers.conf"
  mkdir -p "$work/linux-root"
  "$install_payload/installer/release/install-linux.sh" \
    install "$work/linux-root" 1000 releaseuser "$install_payload"
  test -x "$work/linux-root/usr/libexec/bloom/bloom-broker"
  test -f "$work/linux-root/etc/bloom/1000/signer/config.json"
  "$install_payload/installer/release/install-linux.sh" \
    uninstall "$work/linux-root" 1000 delete-bloom-login-1000
  test ! -e "$work/linux-root/etc/bloom/1000"
fi
echo "Bloom triad release gate passed for $BLOOM_MACHINE_SHA / $BLOOM_BROKER_SHA / $BLOOM_SIGNER_SHA"
