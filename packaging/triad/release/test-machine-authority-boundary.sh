#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
checker="$script_dir/check-machine-authority-boundary.sh"
baseline="$script_dir/machine-authority-baseline.tsv"
work="$(mktemp -d "${TMPDIR:-/tmp}/bloom-machine-authority-test.XXXXXX")"
trap 'find "$work" -depth -delete' EXIT

"$checker" --check-baseline >"$work/baseline.out"
grep -Fx 'Machine authority source ratchet passed' "$work/baseline.out" >/dev/null

"$checker" --inventory >"$work/inventory.out"
grep -Fx 'schema: bloom.machine-authority-inventory.v1' "$work/inventory.out" >/dev/null
grep -Fx 'baseline-revision: 2767153bfab6' "$work/inventory.out" >/dev/null
grep -F 'feature-set: bloom-default package=bloom ' "$work/inventory.out" >/dev/null
grep -F 'feature-set: bloom-portable package=bloom ' "$work/inventory.out" >/dev/null
grep -F 'feature-set: bloom-machine package=bloom-machine ' "$work/inventory.out" >/dev/null
grep -F 'reachable-forbidden-dependency: bloom-keystore' "$work/inventory.out" >/dev/null
grep -F 'reachable-forbidden-dependency: bloom-auth' "$work/inventory.out" >/dev/null
grep -F 'reachable-forbidden-dependency: bloom-auth-api' "$work/inventory.out" >/dev/null
grep -F 'source-marker: PrivateKeySigner file=crates/bloom-hyperliquid/src/lib.rs count=5 ceiling=5' \
  "$work/inventory.out" >/dev/null

awk -F '\t' 'BEGIN { OFS = "\t" }
  $1 == "PrivateKeySigner" && $2 == "crates/bloom-hyperliquid/src/lib.rs" {
    $3 = $3 - 1
  }
  { print }
' "$baseline" >"$work/lowered-baseline.tsv"
if BLOOM_MACHINE_AUTHORITY_BASELINE="$work/lowered-baseline.tsv" \
  "$checker" --check-baseline >"$work/lowered.out" 2>&1
then
  echo "lowered authority-source ceiling unexpectedly passed" >&2
  exit 1
fi
grep -F 'Machine authority marker expanded: PrivateKeySigner' "$work/lowered.out" >/dev/null

mkdir "$work/new-source-root"
printf 'struct PrivateKeySigner;\n' >"$work/new-source-root/new_authority.rs"
if BLOOM_MACHINE_AUTHORITY_EXTRA_SOURCE_ROOTS="$work/new-source-root" \
  "$checker" --check-baseline >"$work/new-file.out" 2>&1
then
  echo "new authority marker file unexpectedly passed" >&2
  exit 1
fi
grep -F 'Machine authority marker appeared in a new file: PrivateKeySigner' \
  "$work/new-file.out" >/dev/null

if "$checker" --require-clean >"$work/strict.out" 2>&1; then
  echo "strict Machine authority boundary unexpectedly passed before M6" >&2
  exit 1
fi
grep -F 'forbidden production Machine dependency in bloom-default: bloom-keystore' \
  "$work/strict.out" >/dev/null
if grep -F 'forbidden authority-restoring production feature remains' \
  "$work/strict.out" >/dev/null
then
  echo "removed Machine authority feature is still present" >&2
  exit 1
fi

echo "Machine authority boundary M0 tests passed"
