#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 PAYLOAD_DIR" >&2
  exit 64
fi

payload="$(cd "$1" && pwd -P)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
manifest="$work/subject-manifest"

(
  cd "$payload"
  find . -type l -print | grep . >/dev/null && {
    echo "macOS conformance subject contains a symlink" >&2
    exit 65
  }
  find . -type f -print |
    LC_ALL=C sort |
    while IFS= read -r file; do
      case "$file" in
        ./PLATFORM_CLAIM|\
        ./MACOS_CONFORMANCE_REPORT.json|\
        ./MACOS_CONFORMANCE_REPORT.sig|\
        ./MACOS_CONFORMANCE_REPORT.pub|\
        ./RELEASE_PUBLIC_KEY.pem|\
        ./RELEASE_SIGNATURE|\
        ./SHA256SUMS)
          continue
          ;;
      esac
      digest="$(shasum -a 256 "$file" | awk '{print $1}')"
      printf '%s  %s\n' "$digest" "$file"
    done
) > "$manifest"

[[ -s "$manifest" ]] || {
  echo "macOS conformance subject is empty" >&2
  exit 65
}
shasum -a 256 "$manifest" | awk '{print $1}'
