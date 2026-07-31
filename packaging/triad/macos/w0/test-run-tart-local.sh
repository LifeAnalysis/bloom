#!/bin/bash
set -Eeuo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
main_root="$(cd "$script_dir/../../../.." && pwd)"
work="$(mktemp -d "${TMPDIR:-/tmp}/bloom-tart-local-test.XXXXXX")"
trap 'find "$work" -depth -delete' EXIT

mkdir -p "$work/bin"
cat >"$work/bin/tart" <<'EOF'
#!/bin/bash
case "${1:-}" in
  list) exit 42 ;;
  *) exit 0 ;;
esac
EOF
chmod +x "$work/bin/tart"

status=0
PATH="$work/bin:$PATH" \
  BLOOM_TART_BROKER_ROOT="$main_root" \
  BLOOM_TART_SIGNER_ROOT="$main_root" \
  "$script_dir/run-tart-local.sh" >"$work/list-failure.out" 2>&1 || status=$?
if [[ "$status" -ne 70 ]]; then
  echo "Tart list failure returned $status instead of 70" >&2
  cat "$work/list-failure.out" >&2
  exit 1
fi
grep -Fx 'failed to list local Tart VMs' "$work/list-failure.out" >/dev/null

cat >"$work/bin/tart" <<'EOF'
#!/bin/bash
case "${1:-}" in
  list)
    printf '%s\n' '[{"Source":"local","Name":"fake-base","Running":false}]'
    ;;
  run) exit 23 ;;
  *) exit 0 ;;
esac
EOF
chmod +x "$work/bin/tart"

status=0
PATH="$work/bin:$PATH" \
  BLOOM_TART_BROKER_ROOT="$main_root" \
  BLOOM_TART_SIGNER_ROOT="$main_root" \
  BLOOM_TART_DEVELOPMENT_BASE=fake-base \
  BLOOM_TART_OUTPUT_ROOT="$work/output" \
  "$script_dir/run-tart-local.sh" >"$work/run-failure.out" 2>&1 || status=$?
if [[ "$status" -eq 0 ]]; then
  echo "early Tart run-process exit unexpectedly passed" >&2
  cat "$work/run-failure.out" >&2
  exit 1
fi
grep -F 'Tart VM process exited before SSH was ready: fake-base (status 23)' \
  "$work/run-failure.out" >/dev/null

echo 'local Tart orchestration failure tests passed'
