#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fixture_bin="${repo_root}/scripts/test-fixtures/fake-local-integration-bloom.sh"
runner="${repo_root}/scripts/local-mainnet-integration.sh"
test_root="$(mktemp -d "${TMPDIR:-/tmp}/bloom-integration-test.XXXXXX")"
test_ok=0
cleanup_test() {
  if [ "$test_ok" -eq 1 ]; then
    rm -rf "$test_root"
  else
    printf 'failed test fixtures retained at: %s\n' "$test_root" >&2
  fi
}
trap cleanup_test EXIT

wallet_dir="${test_root}/home/keystore/test-passkey"
mkdir -p "$wallet_dir"
printf '0x0000000000000000000000000000000000000001\n' > "${wallet_dir}/address"
printf 'ciphertext fixture\n' > "${wallet_dir}/encrypted.key"
printf 'passkey\n' > "${wallet_dir}/kind"
printf '[polymarket]\nenabled = true\n' > "${wallet_dir}/policy.toml"
printf 'signature fixture\n' > "${wallet_dir}/policy.toml.sig"
printf 'salt fixture\n' > "${wallet_dir}/prf.salt"
printf '{"credential":{"counter":7,"credential_id":"fixture"}}\n' \
  > "${wallet_dir}/passkey.json"

output="$(
  BLOOM_HOME="${test_root}/home" \
  BLOOM_INTEGRATION_BIN="$fixture_bin" \
  BLOOM_INTEGRATION_OPEN=true \
    "$runner" --wallet test-passkey
)"
grep -q 'Preflight passed' <<<"$output"
grep -q 'No passkey prompt was opened and no order was submitted' <<<"$output"

# A first run may spend more than the former ten-second deadline provisioning
# configured Petals before the server creates its IPC socket.
delayed_output="$(
  BLOOM_HOME="${test_root}/home" \
  BLOOM_INTEGRATION_BIN="$fixture_bin" \
  BLOOM_INTEGRATION_OPEN=true \
  BLOOM_FAKE_STARTUP_DELAY_SECS=11 \
    "$runner" --wallet test-passkey 2>"${test_root}/delayed.err"
)"
grep -q 'Preflight passed' <<<"$delayed_output"
grep -q 'Still starting Bloom' "${test_root}/delayed.err"

if BLOOM_HOME="${test_root}/home" \
  BLOOM_INTEGRATION_BIN="$fixture_bin" \
  BLOOM_INTEGRATION_OPEN=true \
    "$runner" --wallet test-passkey --execute-hyperliquid \
    >"${test_root}/partial.out" 2>&1
then
  printf 'incomplete Hyperliquid opt-in unexpectedly succeeded\n' >&2
  exit 1
fi
grep -q 'all live Hyperliquid arguments' "${test_root}/partial.out"

if BLOOM_HOME="${test_root}/home" \
  BLOOM_INTEGRATION_BIN="$fixture_bin" \
  BLOOM_INTEGRATION_OPEN=true \
    "$runner" --wallet test-passkey \
      --execute-hyperliquid \
      --hl-coin BTC --hl-asset-id 0 --hl-side buy --hl-price 100 --hl-size 1 \
      --execute-polymarket \
      --pm-slug fixture --pm-outcome Yes --pm-side buy \
      --pm-amount 1 --pm-price-bound 0.5 \
    >"${test_root}/oversized.out" 2>&1
then
  printf 'oversized Hyperliquid order unexpectedly passed validation\n' >&2
  exit 1
fi
grep -q 'Hyperliquid order notional must be between' "${test_root}/oversized.out"

if command -v expect >/dev/null 2>&1; then
  live_output="${test_root}/live.out"
  # Expect expands its own $env(RUNNER).
  # shellcheck disable=SC2016
  RUNNER="$runner" \
  BLOOM_HOME="${test_root}/home" \
    BLOOM_FAKE_STATE="${test_root}/fake-state" \
    BLOOM_INTEGRATION_BIN="$fixture_bin" \
    BLOOM_INTEGRATION_OPEN=true \
    expect -c '
      set timeout 20
      log_user 1
      spawn $env(RUNNER) \
        --wallet test-passkey \
        --execute-hyperliquid \
        --hl-coin BTC --hl-asset-id 0 --hl-side buy \
        --hl-price 100000 --hl-size 0.0001 --hl-tif Ioc \
        --execute-polymarket \
        --pm-slug fixture --pm-outcome Yes --pm-side buy \
        --pm-amount 1 --pm-price-bound 0.5 --pm-order-type FAK
      expect "to authorize the selected submission(s):"
      send "EXECUTE BOTH MAINNET ORDERS\r"
      expect "Complete the passkey ceremony in the browser, then press Return"
      send "\r"
      expect "to request its passkey approval:"
      send "POST POLYMARKET DRAFT draft-1\r"
      expect "Complete the passkey ceremony in the browser, then press Return"
      send "\r"
      expect eof
      set result [wait]
      exit [lindex $result 3]
    ' >"$live_output"
  grep -q 'PASS: selected mainnet venue submission(s)' "$live_output"
  test -f "${test_root}/fake-state/hl-order"
  test -f "${test_root}/fake-state/pm-posted"
fi

test_ok=1
printf 'local mainnet integration runner tests passed\n'
