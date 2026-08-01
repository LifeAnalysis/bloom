#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
# shellcheck disable=SC1091
source "${repo_root}/scripts/lib/bounded-process.sh"
test_root="$(mktemp -d "${TMPDIR:-/tmp}/bloom-bounded-process.XXXXXX")"
trap 'rm -rf -- "$test_root"' EXIT INT TERM
pid_file="${test_root}/pids"
log_file="${test_root}/capture.log"
started="$(date +%s)"
status=0
bloom_bounded_process 1 "$log_file" bash -c '
  trap "" TERM
  printf "%s\n" "$$" > "$1"
  bash -c '\''trap "" TERM; while :; do sleep 1; done'\'' &
  printf "%s\n" "$!" >> "$1"
  wait
' _ "$pid_file" || status=$?
[ "$status" -eq 124 ] || {
  printf 'expected timeout 124, got %s\n' "$status" >&2
  exit 1
}
[ $(( $(date +%s) - started )) -lt 8 ] || {
  printf 'bounded process timeout did not return promptly\n' >&2
  exit 1
}
while IFS= read -r pid; do
  [ -n "$pid" ] || continue
  if kill -0 "$pid" 2>/dev/null; then
    printf 'bounded process survivor: %s\n' "$pid" >&2
    exit 1
  fi
done < "$pid_file"
grep -F 'bounded process exceeded 1s' "$log_file" >/dev/null
printf 'bounded process timeout regression passed\n'
