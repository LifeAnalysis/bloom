#!/usr/bin/env bash
set -euo pipefail

petal_root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
repo_root="$(cd "${petal_root}/../.." && pwd -P)"
target_root="${petal_root}/target"
core_wasm="${target_root}/wasm32-unknown-unknown/release/bloom_solana_driver_petal.wasm"
bloom_builder="${BLOOM_PETAL_BUILDER:-${repo_root}/target/debug/bloom}"

cargo build \
  --manifest-path "${petal_root}/Cargo.toml" \
  --target wasm32-unknown-unknown \
  --release

mkdir -p "${petal_root}/petal/solana-driver"
for route in transfer.stage.json transfer.assemble.json; do
  wasm-tools component new "$core_wasm" -o "${petal_root}/petal/solana-driver/${route}.wasm"
  wasm-tools validate "${petal_root}/petal/solana-driver/${route}.wasm"
done

if [[ ! -x "$bloom_builder" ]]; then
  cargo build --manifest-path "${repo_root}/Cargo.toml" --package bloom --bin bloom
fi
"$bloom_builder" petals build "$petal_root"
