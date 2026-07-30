#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 4 ]]; then
  echo "usage: $0 PRIVATE_KEY NAMESPACE INPUT OUTPUT" >&2
  exit 64
fi

private_key="$1"
namespace="$2"
input="$3"
output="$4"

[[ "$namespace" =~ ^[a-z0-9.-]{1,64}$ ]] || {
  echo "invalid SSHSIG namespace" >&2
  exit 64
}
[[ -f "$private_key" && ! -L "$private_key" && -f "$input" && ! -L "$input" ]] || {
  echo "Ed25519 signing input is missing or substituted" >&2
  exit 66
}

temporary="$(mktemp "${output}.tmp.XXXXXX")"
trap 'rm -f "$temporary"' EXIT
/usr/bin/ssh-keygen \
  -Y sign \
  -f "$private_key" \
  -n "$namespace" \
  < "$input" > "$temporary"
chmod 0644 "$temporary"
mv -f "$temporary" "$output"
trap - EXIT
