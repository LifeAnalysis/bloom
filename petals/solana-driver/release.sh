#!/usr/bin/env bash
# Release pipeline for the Solana driver package + verifier corpus.
#
# Stages:
#  1. Hermetic reproduction gate (verify-repro.sh must pass).
#  2. Verifier corpus publication: the frozen golden/mutation/reference
#     vectors and their recorded digest are written under artifacts/.
#  3. Catalog signing (only when CATALOG_SEED_FILE points at an operator-held
#     32-byte hex seed; never in this repository).
#
# No secret is ever read from, or written to, the repository.
set -euo pipefail

petal_root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
repo_root="$(cd "${petal_root}/../.." && pwd -P)"

echo "==> [1/3] hermetic reproduction gate"
"${petal_root}/verify-repro.sh"

echo "==> [2/3] publishing the verifier corpus"
(
  cd "${repo_root}"
  cargo run --quiet -p bloom-solana-machine --example corpus_publish \
    -- "${petal_root}/artifacts/verifier-corpus.json"
)

echo "==> [3/3] catalog entry"
if [[ -n "${CATALOG_SEED_FILE:-}" ]]; then
  (
    cd "${repo_root}"
    cargo run --quiet -p bloom-solana-machine --example catalog-sign -- \
      --package "${petal_root}" \
      --seed-file "${CATALOG_SEED_FILE}" \
      --clusters devnet,localnet \
      --out "${petal_root}/artifacts/catalog-entry.json"
  )
  echo "signed: ${petal_root}/artifacts/catalog-entry.json"
else
  echo "CATALOG_SEED_FILE not set: skipping signing (release is content-pinned only)"
fi

echo "release pipeline OK"
