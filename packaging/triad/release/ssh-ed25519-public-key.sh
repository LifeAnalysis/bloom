#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: $0 PRIVATE_KEY OUTPUT" >&2
  exit 64
fi

private_key="$1"
output="$2"
[[ -f "$private_key" && ! -L "$private_key" ]] || {
  echo "Ed25519 private key is missing or substituted" >&2
  exit 66
}

temporary="$(mktemp "${output}.tmp.XXXXXX")"
trap 'rm -f "$temporary"' EXIT
public_key="$(/usr/bin/ssh-keygen -y -f "$private_key")"
[[ "$public_key" != *$'\n'* ]] || {
  echo "private key produced more than one public-key line" >&2
  exit 65
}
read -r key_type key_body _comment <<< "$public_key"
[[ "$key_type" == "ssh-ed25519" && "$key_body" =~ ^[A-Za-z0-9+/]+={0,2}$ ]] || {
  echo "private key did not produce one unadorned Ed25519 public key" >&2
  exit 65
}
printf '%s %s\n' "$key_type" "$key_body" > "$temporary"
chmod 0644 "$temporary"
mv -f "$temporary" "$output"
trap - EXIT
