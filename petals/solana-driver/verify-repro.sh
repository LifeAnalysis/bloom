#!/usr/bin/env bash
# Hermetic reproduction check for the solana-driver Petal package.
#
# Verifies that the committed, content-addressed artifacts can be reproduced
# exactly: tool versions must match artifacts/reproducibility.json, every
# recorded input digest must match the committed tree, and a rebuild in a
# fresh CARGO_TARGET_DIR from the committed git state must produce a
# byte-identical build manifest.
#
# No destructive operations: all scratch state lands in a mktemp directory.
set -euo pipefail

petal_root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
repo_root="$(cd "${petal_root}/../.." && pwd -P)"
scratch="$(mktemp -d "${TMPDIR:-/tmp}/solana-driver-repro.XXXXXX")"
trap 'cd /; true' EXIT

fail() { echo "repro FAILED: $*" >&2; exit 1; }

echo "==> toolchain assertions"
rustc_actual="$(rustc --version)"
wasm_tools_actual="$(wasm-tools --version)"
python3 - "$petal_root/artifacts/reproducibility.json" "$rustc_actual" "$wasm_tools_actual" <<'PY'
import json, sys, hashlib, os
meta = json.load(open(sys.argv[1]))
rustc, wasm_tools = sys.argv[2], sys.argv[3]
if meta["toolchain"]["rustc"] != rustc:
    sys.exit(f"rustc mismatch: recorded {meta['toolchain']['rustc']!r}, actual {rustc!r}")
if meta["toolchain"]["wasm-tools"] != wasm_tools:
    sys.exit(f"wasm-tools mismatch: recorded {meta['toolchain']['wasm-tools']!r}, actual {wasm_tools!r}")
base = os.path.dirname(os.path.dirname(sys.argv[1]))  # petal root (artifacts/..)
for rel, want in sorted(meta["inputs"].items()):
    path = os.path.join(base, rel)
    try:
        got = hashlib.sha256(open(path, "rb").read()).hexdigest()
    except FileNotFoundError:
        sys.exit(f"input missing: {rel}")
    if got != want:
        sys.exit(f"input digest mismatch for {rel}: recorded {want}, actual {got}")
print("input digests ok")
PY
[ $? -eq 0 ] || fail "toolchain/input assertions"

echo "==> extracting committed tree"
git -C "$repo_root" archive HEAD petals/solana-driver | tar -x -C "$scratch"
extracted="$scratch/petals/solana-driver"
[ -d "$extracted" ] || fail "archive extraction"

echo "==> building the package (daemon-free pure builder)"
( cd "$repo_root" && cargo run --quiet --package bloom-petals --example build_petal_package -- "$extracted" )

echo "==> rebuilding components in a fresh CARGO_TARGET_DIR"
(
  cd "$extracted"
  CARGO_TARGET_DIR="$scratch/target" cargo build \
    --manifest-path Cargo.toml \
    --target wasm32-unknown-unknown \
    --release --quiet
  mkdir -p petal/solana-driver
  for route in transfer.stage.json transfer.assemble.json; do
    wasm-tools component new \
      "$scratch/target/wasm32-unknown-unknown/release/bloom_solana_driver_petal.wasm" \
      -o "petal/solana-driver/${route}.wasm"
    wasm-tools validate "petal/solana-driver/${route}.wasm"
  done
)

echo "==> comparing manifests"
if ! diff -u \
  "$petal_root/artifacts/build-manifest.json" \
  "$extracted/artifacts/build-manifest.json"; then
  fail "rebuild produced a different build manifest — the committed artifacts are not reproducible from the recorded inputs"
fi

echo "repro OK: committed solana-driver artifacts are byte-identical to a fresh, toolchain-pinned rebuild"
