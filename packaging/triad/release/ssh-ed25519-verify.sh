#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 4 ]]; then
  echo "usage: $0 PUBLIC_KEY NAMESPACE INPUT SIGNATURE" >&2
  exit 64
fi

public_key="$1"
namespace="$2"
input="$3"
signature="$4"

[[ "$namespace" =~ ^[a-z0-9.-]{1,64}$ ]] || {
  echo "invalid SSHSIG namespace" >&2
  exit 64
}
for path in "$public_key" "$input" "$signature"; do
  [[ -f "$path" && ! -L "$path" ]] || {
    echo "Ed25519 verification input is missing or substituted: $path" >&2
    exit 66
  }
done

read -r key_type key_body trailing < "$public_key"
[[ "$key_type" == "ssh-ed25519" && "$key_body" =~ ^[A-Za-z0-9+/]+={0,2}$ &&
  -z "${trailing:-}" && "$(wc -l < "$public_key" | tr -d ' ')" == "1" ]] || {
  echo "release public key is not one unadorned Ed25519 OpenSSH key" >&2
  exit 65
}

temporary="$(mktemp -d)"
trap 'rm -rf "$temporary"' EXIT
printf 'bloom-release %s %s\n' "$key_type" "$key_body" \
  > "$temporary/allowed_signers"
chmod 0600 "$temporary/allowed_signers"
/usr/bin/ssh-keygen \
  -Y verify \
  -f "$temporary/allowed_signers" \
  -I bloom-release \
  -n "$namespace" \
  -s "$signature" \
  < "$input" >/dev/null
