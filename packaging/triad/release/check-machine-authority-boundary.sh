#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
workspace="$(cd "$script_dir/../../.." && pwd -P)"
baseline="${BLOOM_MACHINE_AUTHORITY_BASELINE:-$script_dir/machine-authority-baseline.tsv}"
feature_sets="${BLOOM_MACHINE_PRODUCTION_FEATURE_SETS:-$script_dir/machine-production-feature-sets.tsv}"
mode="${1:-}"

usage() {
  echo "usage: $0 --check-baseline|--inventory|--require-clean" >&2
  exit 64
}

case "$mode" in
  --check-baseline|--inventory|--require-clean) ;;
  *) usage ;;
esac

for command_name in cargo rg awk sed sort uniq; do
  command -v "$command_name" >/dev/null 2>&1 || {
    echo "machine authority boundary check requires $command_name" >&2
    exit 69
  }
done
test -f "$baseline" || {
  echo "missing Machine authority baseline: $baseline" >&2
  exit 66
}
test -f "$feature_sets" || {
  echo "missing Machine production feature sets: $feature_sets" >&2
  exit 66
}

count_occurrences() {
  local marker="$1"
  local relative="$2"
  local path="$workspace/$relative"
  if [[ ! -f "$path" ]]; then
    echo 0
    return
  fi
  { rg -F -o -- "$marker" "$path" || true; } | wc -l | tr -d ' '
}

baseline_has_file() {
  local marker="$1"
  local relative="$2"
  awk -F '\t' -v marker="$marker" -v relative="$relative" '
    $0 !~ /^#/ && $1 == marker && $2 == relative { found = 1 }
    END { exit(found ? 0 : 1) }
  ' "$baseline"
}

check_source_ratchet() {
  local failed=0
  local marker relative maximum actual absolute
  while IFS=$'\t' read -r marker relative maximum; do
    [[ -z "$marker" || "$marker" == \#* ]] && continue
    actual="$(count_occurrences "$marker" "$relative")"
    if (( actual > maximum )); then
      echo "Machine authority marker expanded: $marker in $relative ($actual > $maximum)" >&2
      failed=1
    fi
  done < "$baseline"

  while IFS= read -r marker; do
    [[ -z "$marker" ]] && continue
    while IFS= read -r absolute; do
      [[ -z "$absolute" ]] && continue
      relative="${absolute#"$workspace/"}"
      if ! baseline_has_file "$marker" "$relative"; then
        echo "Machine authority marker appeared in a new file: $marker in $relative" >&2
        failed=1
      fi
    done < <(rg -l -F -g '*.rs' -g 'Cargo.toml' -- "$marker" "${source_roots[@]}" || true)
  done < <(awk -F '\t' '$0 !~ /^#/ && NF >= 3 { print $1 }' "$baseline" | sort -u)

  (( failed == 0 ))
}

cargo_tree_for_set() {
  local package="$1"
  local defaults="$2"
  local features="$3"
  local args=(
    tree
    --manifest-path "$workspace/Cargo.toml"
    -p "$package"
    -e "normal,build"
    --prefix none
  )
  [[ "$defaults" == "no" ]] && args+=(--no-default-features)
  [[ "$features" != "-" ]] && args+=(--features "$features")
  cargo "${args[@]}"
}

production_source_roots() {
  local label package defaults features
  while IFS=$'\t' read -r label package defaults features; do
    [[ -z "$label" || "$label" == \#* ]] && continue
    cargo_tree_for_set "$package" "$defaults" "$features"
  done < "$feature_sets" |
    sed -n -E 's#^.* \((/[^)]*)\)( \(\*\))?$#\1#p' |
    sort -u |
    awk -v workspace="$workspace" '
      $0 != workspace "/crates/bloom-keystore" &&
      $0 != workspace "/crates/bloom-auth" &&
      $0 != workspace "/crates/bloom-auth-api"
    '
}

source_roots=()
while IFS= read -r source_root; do
  [[ -n "$source_root" ]] && source_roots+=("$source_root")
done < <(production_source_roots)
if [[ -n "${BLOOM_MACHINE_AUTHORITY_EXTRA_SOURCE_ROOTS:-}" ]]; then
  IFS=':' read -r -a extra_source_roots <<<"$BLOOM_MACHINE_AUTHORITY_EXTRA_SOURCE_ROOTS"
  source_roots+=("${extra_source_roots[@]}")
fi

normalized_tree_packages() {
  sed -E 's/ v[0-9][^ ]*( \([^)]*\))?( \(\*\))?$//' | sed -E 's/ \(\*\)$//'
}

inventory_dependencies() {
  local label package defaults features dependency tree
  while IFS=$'\t' read -r label package defaults features; do
    [[ -z "$label" || "$label" == \#* ]] && continue
    echo "feature-set: $label package=$package default-features=$defaults features=$features"
    tree="$(cargo_tree_for_set "$package" "$defaults" "$features")"
    for dependency in bloom-keystore bloom-auth bloom-auth-api; do
      if printf '%s\n' "$tree" | normalized_tree_packages | grep -Fx "$dependency" >/dev/null; then
        echo "  reachable-forbidden-dependency: $dependency"
      fi
    done
  done < "$feature_sets"
}

require_clean_dependencies() {
  local failed=0
  local label package defaults features dependency tree
  while IFS=$'\t' read -r label package defaults features; do
    [[ -z "$label" || "$label" == \#* ]] && continue
    case ",${features}," in
      *,unsafe-debug-signer,*|*,local-integration,*)
        echo "forbidden production Machine feature in $label: $features" >&2
        failed=1
        ;;
    esac
    tree="$(cargo_tree_for_set "$package" "$defaults" "$features")"
    for dependency in bloom-keystore bloom-auth bloom-auth-api; do
      if printf '%s\n' "$tree" | normalized_tree_packages | grep -Fx "$dependency" >/dev/null; then
        echo "forbidden production Machine dependency in $label: $dependency" >&2
        failed=1
      fi
    done
  done < "$feature_sets"

  for manifest in \
    crates/bloom/Cargo.toml \
    crates/bloom-daemon/Cargo.toml \
    crates/bloom-vfs/Cargo.toml \
    crates/bloom-tx/Cargo.toml
  do
    if rg -n '^(unsafe-debug-signer|local-integration)[[:space:]]*=' \
      "$workspace/$manifest" >&2
    then
      echo "forbidden authority-restoring production feature remains in $manifest" >&2
      failed=1
    fi
  done
  (( failed == 0 ))
}

case "$mode" in
  --check-baseline)
    check_source_ratchet
    echo "Machine authority source ratchet passed"
    ;;
  --inventory)
    echo "schema: bloom.machine-authority-inventory.v1"
    echo "baseline-revision: 2767153bfab6"
    echo "observed-revision: $(git -C "$workspace" rev-parse --short=12 HEAD)"
    inventory_dependencies
    while IFS=$'\t' read -r marker relative maximum; do
      [[ -z "$marker" || "$marker" == \#* ]] && continue
      actual="$(count_occurrences "$marker" "$relative")"
      [[ "$actual" == 0 ]] || echo "source-marker: $marker file=$relative count=$actual ceiling=$maximum"
    done < "$baseline"
    ;;
  --require-clean)
    check_source_ratchet
    require_clean_dependencies
    echo "Machine production authority boundary is clean"
    ;;
esac
