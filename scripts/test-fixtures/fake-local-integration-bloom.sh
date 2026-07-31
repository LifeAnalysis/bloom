#!/usr/bin/env bash
# Deterministic CLI double for scripts/test-local-mainnet-integration.sh.
set -euo pipefail

args=" $* "
if [[ "$args" == *" serve "* ]]; then
  socket=""
  while [ "$#" -gt 0 ]; do
    if [ "$1" = "--endpoint" ]; then
      socket="${2#unix:}"
      break
    fi
    shift
  done
  [ -n "$socket" ]
  exec python3 - "$socket" <<'PY'
import os
import signal
import socket
import sys
import time

path = sys.argv[1]
time.sleep(float(os.environ.get("BLOOM_FAKE_STARTUP_DELAY_SECS", "0")))
try:
    os.unlink(path)
except FileNotFoundError:
    pass
listener = socket.socket(socket.AF_UNIX)
listener.bind(path)
listener.listen(1)
signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
while True:
    time.sleep(1)
PY
fi

operation=""
path=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    cat|ls|write)
      operation="$1"
      path="${2:-}"
      break
      ;;
  esac
  shift
done
[ -n "$operation" ] && [ -n "$path" ]
state="${BLOOM_FAKE_STATE:-${TMPDIR:-/tmp}/bloom-fake-state}"
mkdir -p "$state"

if [ "$operation" = "ls" ]; then
  case "$path" in
    /petals/polymarket/trade/test-passkey/drafts)
      [ -f "${state}/pm-draft" ] && printf 'draft-1\tDir\n'
      exit 0
      ;;
  esac
fi

if [ "$operation" = "write" ]; then
  case "$path" in
    /hyperliquid/mainnet/agent_sessions/test-passkey/new.json)
      if [ ! -f "${state}/hl-approval-staged" ]; then
        touch "${state}/hl-approval-staged"
        exit 1
      fi
      touch "${state}/hl-session"
      exit 0
      ;;
    /hyperliquid/mainnet/agent_sessions/test-passkey/*/order.json)
      touch "${state}/hl-order"
      exit 0
      ;;
    /hyperliquid/mainnet/agent_sessions/test-passkey/*/cancel_all|\
    /hyperliquid/mainnet/agent_sessions/test-passkey/*/stop)
      exit 0
      ;;
    /petals/polymarket/trade/test-passkey/new)
      touch "${state}/pm-draft"
      exit 0
      ;;
    /petals/polymarket/trade/test-passkey/drafts/draft-1/revalidate)
      exit 0
      ;;
    /petals/polymarket/trade/test-passkey/drafts/draft-1/post)
      if [ ! -f "${state}/pm-approval-staged" ]; then
        touch "${state}/pm-approval-staged"
        exit 1
      fi
      touch "${state}/pm-posted"
      exit 0
      ;;
  esac
fi

case "$path" in
  /wallets/test-passkey/kind)
    printf 'passkey\n'
    ;;
  /wallets/test-passkey/address)
    printf '0x0000000000000000000000000000000000000001\n'
    ;;
  /petals/polymarket/meta/route-contract.json)
    printf '{"schema":"fake.route-contract.v1","abi":"0.1"}\n'
    ;;
  /hyperliquid/mainnet/perp_meta.json)
    printf '{"universe":[{"name":"BTC"}]}\n'
    ;;
  /hyperliquid/mainnet/users/0x0000000000000000000000000000000000000001/clearinghouse.json)
    printf '{"marginSummary":{"accountValue":"20"},"withdrawable":"20","assetPositions":[]}\n'
    ;;
  /petals/polymarket/onboard/test-passkey/status.json)
    printf '{"stage":"complete","tradeable":true}\n'
    ;;
  /petals/polymarket/account/test-passkey/status.json)
    printf '{"status":"ready","tradeable":true}\n'
    ;;
  /petals/polymarket/account/test-passkey/buying_power.json)
    printf '{"spendable":{"asset":"pUSD","raw":"20000000"},"can_trade_now":true,"funding_needed":false}\n'
    ;;
  /petals/polymarket/markets/fixture/market.json)
    printf '{"slug":"fixture","question":"Fixture?","outcomes":["Yes","No"]}\n'
    ;;
  /petals/polymarket/markets/fixture/prices.json)
    printf '{"Yes":"0.5","No":"0.5"}\n'
    ;;
  /hyperliquid/mainnet/agent_sessions/test-passkey/*/approval_challenge.json)
    printf '{"action_id":"hl-fixture","ceremony_url":"http://127.0.0.1:18734/fixture","expires_at_ms":9999999999999}\n'
    ;;
  /hyperliquid/mainnet/agent_sessions/test-passkey/*/session.json)
    printf '{"status":"active","max_session_secs":300}\n'
    ;;
  /hyperliquid/mainnet/agent_sessions/test-passkey/*/last_response.json)
    printf '{"response":{"status":"ok","response":{"type":"order","data":{"statuses":[{"filled":{"oid":1}}]}}}}\n'
    ;;
  /petals/polymarket/trade/test-passkey/drafts/draft-1/plan.md)
    printf '# Fixture Polymarket plan\n'
    ;;
  /petals/polymarket/trade/test-passkey/drafts/draft-1/policy_check.json)
    printf '{"policy_deny":false,"policy_status":"pass"}\n'
    ;;
  /petals/polymarket/trade/test-passkey/drafts/draft-1/quote.json)
    printf '{"status":"revalidated","amount":"1","limit_price":"0.5"}\n'
    ;;
  /petals/polymarket/trade/test-passkey/drafts/draft-1/review_intent.json)
    printf '{"status":"final_review_staged","posting_enabled":true}\n'
    ;;
  /petals/polymarket/trade/test-passkey/drafts/draft-1/approval.json)
    printf '{"action_id":"pm-fixture","ceremony_url":"http://127.0.0.1:18734/fixture","expires_ms":9999999999999}\n'
    ;;
  /petals/polymarket/trade/test-passkey/receipts/draft-1/receipt.json)
    printf '{"draft_id":"draft-1","clob_status":"matched","filled_size_micro":1000000}\n'
    ;;
  *)
    printf 'fake Bloom received an unexpected path: %s\n' "$path" >&2
    exit 1
    ;;
esac
