#!/usr/bin/env bash
#
# Manual, passkey-backed mainnet integration for Bloom's local developer
# profile. The default is non-spending preflight. No order is sent unless its
# venue-specific --execute-* flag is present and the operator types the final
# acknowledgement at the terminal.

set -euo pipefail

readonly MAX_USD="25"
readonly HL_SESSION_SECS="300"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
home_dir="${BLOOM_HOME:-${HOME}/.bloom}"
wallet=""
execute_hl=0
execute_pm=0
hl_coin=""
hl_asset_id=""
hl_side=""
hl_price=""
hl_size=""
hl_tif="Ioc"
pm_slug=""
pm_outcome=""
pm_side=""
pm_amount=""
pm_bound=""
pm_order_type="FAK"

usage() {
  cat <<'EOF'
Usage:
  scripts/local-mainnet-integration.sh --wallet WALLET

Non-spending preflight is the default. To submit tightly bounded mainnet orders,
add either or both venue blocks:

  --execute-hyperliquid
  --hl-coin COIN --hl-asset-id ID --hl-side buy|sell
  --hl-price PRICE --hl-size SIZE [--hl-tif Ioc|Alo]

  --execute-polymarket
  --pm-slug SLUG --pm-outcome OUTCOME --pm-side buy|sell
  --pm-amount AMOUNT --pm-price-bound PRICE [--pm-order-type FAK|FOK]

Safety properties:
  * Each live venue requires its own flag, exact arguments, and acknowledgement.
  * Hyperliquid is one perp asset, limit only, <= $25 notional, <= 5 minutes.
  * Polymarket is FAK/FOK only and <= $25 maximum consideration.
  * Exact plans and policy checks print before any passkey prompt.
  * The runner never edits wallet keys or policy. It verifies them afterward.
  * The special binary refuses to serve unless --local-integration is explicit.

Environment:
  BLOOM_HOME              Wallet home (default: ~/.bloom)
  BLOOM_INTEGRATION_BIN   Use an already-built local-integration binary
  BLOOM_INTEGRATION_OPEN  Browser opener (default: open)
  BLOOM_INTEGRATION_STARTUP_TIMEOUT_SECS
                          Server/Petal startup deadline (default: 300)
EOF
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

need_value() {
  [ "$#" -ge 2 ] || die "$1 requires a value"
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --wallet) need_value "$@"; wallet="$2"; shift 2 ;;
    --execute-hyperliquid) execute_hl=1; shift ;;
    --hl-coin) need_value "$@"; hl_coin="$2"; shift 2 ;;
    --hl-asset-id) need_value "$@"; hl_asset_id="$2"; shift 2 ;;
    --hl-side) need_value "$@"; hl_side="$2"; shift 2 ;;
    --hl-price) need_value "$@"; hl_price="$2"; shift 2 ;;
    --hl-size) need_value "$@"; hl_size="$2"; shift 2 ;;
    --hl-tif) need_value "$@"; hl_tif="$2"; shift 2 ;;
    --execute-polymarket) execute_pm=1; shift ;;
    --pm-slug) need_value "$@"; pm_slug="$2"; shift 2 ;;
    --pm-outcome) need_value "$@"; pm_outcome="$2"; shift 2 ;;
    --pm-side) need_value "$@"; pm_side="$2"; shift 2 ;;
    --pm-amount) need_value "$@"; pm_amount="$2"; shift 2 ;;
    --pm-price-bound) need_value "$@"; pm_bound="$2"; shift 2 ;;
    --pm-order-type) need_value "$@"; pm_order_type="$2"; shift 2 ;;
    --help|-h) usage; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
done

[ -n "$wallet" ] || die "--wallet is required"
case "$wallet" in
  *[!A-Za-z0-9._-]*|'') die "wallet contains unsafe characters" ;;
esac

command -v jq >/dev/null 2>&1 || die "jq is required (brew install jq)"
command -v shasum >/dev/null 2>&1 || die "shasum is required"
browser_open="${BLOOM_INTEGRATION_OPEN:-open}"
startup_timeout_secs="${BLOOM_INTEGRATION_STARTUP_TIMEOUT_SECS:-300}"
case "$startup_timeout_secs" in
  *[!0-9]*|'') die "BLOOM_INTEGRATION_STARTUP_TIMEOUT_SECS must be an integer" ;;
esac
[ "$startup_timeout_secs" -ge 1 ] && [ "$startup_timeout_secs" -le 1800 ] ||
  die "BLOOM_INTEGRATION_STARTUP_TIMEOUT_SECS must be between 1 and 1800"

live=0
preflight_blockers=0
if [ "$execute_hl" -eq 1 ] || [ "$execute_pm" -eq 1 ]; then
  live=1
fi

is_positive_decimal() {
  jq -en --arg value "$1" \
    '($value | test("^[0-9]+([.][0-9]+)?$")) and (($value | tonumber) > 0)' \
    >/dev/null
}

if [ "$live" -eq 1 ]; then
  command -v "$browser_open" >/dev/null 2>&1 ||
    die "browser opener '$browser_open' was not found"
  if [ "$execute_hl" -eq 1 ]; then
    for value in "$hl_coin" "$hl_asset_id" "$hl_side" "$hl_price" "$hl_size"; do
      [ -n "$value" ] || die "all live Hyperliquid arguments shown in --help are required"
    done
    case "$hl_asset_id" in *[!0-9]*|'') die "--hl-asset-id must be an unsigned integer" ;; esac
    case "$hl_side" in buy|sell) ;; *) die "--hl-side must be buy or sell" ;; esac
    case "$hl_tif" in Ioc|Alo) ;; *) die "--hl-tif must be Ioc or Alo" ;; esac
    is_positive_decimal "$hl_price" || die "--hl-price must be a positive decimal"
    is_positive_decimal "$hl_size" || die "--hl-size must be a positive decimal"
    hl_notional="$(jq -nr --arg p "$hl_price" --arg s "$hl_size" \
      '($p|tonumber) * ($s|tonumber)')"
    jq -en --arg n "$hl_notional" --arg cap "$MAX_USD" \
      '($n|tonumber) >= 10 and ($n|tonumber) <= ($cap|tonumber)' >/dev/null ||
      die "Hyperliquid order notional must be between \$10 and \$${MAX_USD}"
  fi
  if [ "$execute_pm" -eq 1 ]; then
    for value in "$pm_slug" "$pm_outcome" "$pm_side" "$pm_amount" "$pm_bound"; do
      [ -n "$value" ] || die "all live Polymarket arguments shown in --help are required"
    done
    case "$pm_side" in buy|sell) ;; *) die "--pm-side must be buy or sell" ;; esac
    case "$pm_order_type" in FAK|FOK) ;; *) die "--pm-order-type must be FAK or FOK" ;; esac
    is_positive_decimal "$pm_amount" || die "--pm-amount must be a positive decimal"
    is_positive_decimal "$pm_bound" || die "--pm-price-bound must be a positive decimal"
    jq -en --arg p "$pm_bound" \
      '($p|tonumber) > 0 and ($p|tonumber) <= 1' >/dev/null ||
      die "Polymarket price bound must be in (0, 1]"
    if [ "$pm_side" = "buy" ]; then
      pm_max_consideration="$pm_amount"
    else
      pm_max_consideration="$(jq -nr --arg a "$pm_amount" --arg p "$pm_bound" \
        '($a|tonumber) * ($p|tonumber)')"
    fi
    jq -en --arg n "$pm_max_consideration" --arg cap "$MAX_USD" \
      '($n|tonumber) <= ($cap|tonumber)' >/dev/null ||
      die "Polymarket maximum consideration exceeds \$${MAX_USD}"
  fi
  [ -t 0 ] || die "live mode requires an interactive terminal"
fi

wallet_dir="${home_dir}/keystore/${wallet}"
[ -d "$wallet_dir" ] || die "wallet not found at ${wallet_dir}"
[ "$(tr -d '[:space:]' < "${wallet_dir}/kind")" = "passkey" ] ||
  die "wallet '$wallet' is not a passkey wallet"
for file in encrypted.key prf.salt passkey.json policy.toml policy.toml.sig; do
  [ -f "${wallet_dir}/${file}" ] || die "passkey wallet is missing ${file}"
done

run_dir="$(mktemp -d "${TMPDIR:-/tmp}/bloom-mainnet-integration.XXXXXX")"
socket="${run_dir}/bloom.sock"
server_log="${run_dir}/serve.log"
fingerprint_before="${run_dir}/wallet.before"
fingerprint_after="${run_dir}/wallet.after"
server_pid=""
session_active=0
session_id="manual-mainnet-integration-$(date +%s)-$$"

wallet_fingerprint() {
  (
    cd "$wallet_dir"
    for file in address encrypted.key kind policy.toml policy.toml.sig prf.salt public.key; do
      if [ -f "$file" ]; then
        shasum -a 256 "$file"
      fi
    done
    # A hardware authenticator may legitimately advance only its anti-clone
    # counter. Hash every other passkey field so that update is narrowly
    # tolerated without concealing credential replacement.
    jq -S 'walk(if type == "object" then del(.counter) else . end)' passkey.json |
      shasum -a 256 | sed 's/  -$/  passkey.json (counter ignored)/'
  )
}

cleanup() {
  status=$?
  trap - EXIT INT TERM
  if [ "$session_active" -eq 1 ] && [ -S "$socket" ] && [ -x "${bloom_bin:-}" ]; then
    "$bloom_bin" --quiet --home "$home_dir" --connect "unix:${socket}" vfs write \
      "/hyperliquid/mainnet/agent_sessions/${wallet}/${session_id}/cancel_all" \
      --data '{}' >/dev/null 2>&1 || true
    "$bloom_bin" --quiet --home "$home_dir" --connect "unix:${socket}" vfs write \
      "/hyperliquid/mainnet/agent_sessions/${wallet}/${session_id}/stop" \
      --data '{}' >/dev/null 2>&1 || true
  fi
  if [ -n "$server_pid" ] && kill -0 "$server_pid" 2>/dev/null; then
    kill "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  fi
  wallet_fingerprint > "$fingerprint_after" || status=1
  if ! cmp -s "$fingerprint_before" "$fingerprint_after"; then
    printf '%s\n' \
      "FATAL: immutable wallet material changed during the integration run." \
      "The temporary diagnostics are retained at: ${run_dir}" >&2
    diff -u "$fingerprint_before" "$fingerprint_after" >&2 || true
    status=1
  elif [ "$status" -eq 0 ]; then
    rm -rf "$run_dir"
  else
    printf 'diagnostics retained at: %s\n' "$run_dir" >&2
  fi
  exit "$status"
}
wallet_fingerprint > "$fingerprint_before"
trap cleanup EXIT INT TERM

if [ -n "${BLOOM_INTEGRATION_BIN:-}" ]; then
  bloom_bin="$BLOOM_INTEGRATION_BIN"
  [ -x "$bloom_bin" ] || die "BLOOM_INTEGRATION_BIN is not executable"
else
  (
    cd "$repo_root"
    cargo build -p bloom --no-default-features --features local-integration
  )
  bloom_bin="${repo_root}/target/debug/bloom"
fi

"$bloom_bin" --home "$home_dir" serve \
  --endpoint "unix:${socket}" --local-integration >"$server_log" 2>&1 &
server_pid=$!

startup_started_at="$(date +%s)"
startup_deadline=$((startup_started_at + startup_timeout_secs))
startup_next_notice=$((startup_started_at + 10))
while [ ! -S "$socket" ]; do
  if ! kill -0 "$server_pid" 2>/dev/null; then
    cat "$server_log" >&2
    die "local integration server exited during startup"
  fi
  startup_now="$(date +%s)"
  if [ "$startup_now" -ge "$startup_deadline" ]; then
    cat "$server_log" >&2
    die "local integration server did not become ready within ${startup_timeout_secs}s"
  fi
  if [ "$startup_now" -ge "$startup_next_notice" ]; then
    startup_last_log="$(tail -n 1 "$server_log" 2>/dev/null || true)"
    printf 'Still starting Bloom (%ss elapsed); configured Petals may be provisioning.\n' \
      "$((startup_now - startup_started_at))" >&2
    if [ -n "$startup_last_log" ]; then
      printf '  last server log: %s\n' "$startup_last_log" >&2
    fi
    startup_next_notice=$((startup_now + 30))
  fi
  sleep 0.2
done

bloom() {
  "$bloom_bin" --quiet --home "$home_dir" --connect "unix:${socket}" "$@"
}

vcat() {
  bloom vfs cat "$1"
}

vwrite() {
  bloom vfs write "$1" --data "$2"
}

open_approval() {
  artifact_path="$1"
  artifact="$(vcat "$artifact_path")"
  ceremony_url="$(printf '%s' "$artifact" | jq -er '.ceremony_url')"
  expires_ms="$(printf '%s' "$artifact" | jq -r '.expires_ms // .expires_at_ms // "unknown"')"
  printf '\nPasskey approval required:\n'
  printf '%s\n' "$artifact" | jq .
  "$browser_open" "$ceremony_url"
  printf 'Complete the passkey ceremony in the browser, then press Return (expires %s): ' \
    "${expires_ms:-unknown}"
  IFS= read -r _
}

printf '\nBloom local mainnet integration preflight\n'
printf '  home:   %s\n  wallet: %s\n' "$home_dir" "$wallet"
printf '  mode:   %s\n\n' "$([ "$live" -eq 1 ] && printf LIVE || printf NON-SPENDING)"

wallet_kind="$(vcat "/wallets/${wallet}/kind" | tr -d '[:space:]')"
[ "$wallet_kind" = "passkey" ] || die "VFS reports wallet kind '$wallet_kind', expected passkey"
wallet_address="$(vcat "/wallets/${wallet}/address" | tr -d '[:space:]')"
printf 'Passkey wallet: %s (%s)\n' "$wallet" "$wallet_address"

if [ "$live" -eq 0 ] || [ "$execute_hl" -eq 1 ]; then
  hl_meta="$(vcat "/hyperliquid/mainnet/perp_meta.json")"
  hl_resolved_coin="$(printf '%s' "$hl_meta" |
    jq -er --argjson id "${hl_asset_id:-0}" '.universe[$id].name')"
  printf 'Hyperliquid mainnet: reachable (asset 0 is %s)\n' \
    "$(printf '%s' "$hl_meta" | jq -er '.universe[0].name')"
  if [ -n "$hl_coin" ]; then
    hl_discovered_id="$(printf '%s' "$hl_meta" | jq -er --arg coin "$hl_coin" '
      .universe | to_entries[] | select(.value.name == $coin) | .key
    ')"
    printf 'Requested Hyperliquid coin: %s has asset id %s\n' "$hl_coin" "$hl_discovered_id"
  fi
  hl_account="$(vcat "/hyperliquid/mainnet/users/${wallet_address}/clearinghouse.json")"
  printf 'Hyperliquid account snapshot:\n'
  printf '%s\n' "$hl_account" | jq '{
    account_value: .marginSummary.accountValue,
    withdrawable: .withdrawable,
    positions: [.assetPositions[]? | {
      coin: .position.coin,
      size: .position.szi,
      entry_price: .position.entryPx,
      unrealized_pnl: .position.unrealizedPnl
    }]
  }'
  if ! printf '%s' "$hl_account" |
    jq -e '(.marginSummary.accountValue | tonumber) > 0' >/dev/null
  then
    printf 'BLOCKER: Hyperliquid account value is zero or unreadable.\n' >&2
    preflight_blockers=1
  fi
fi

onboard_status='{}'
if [ "$live" -eq 0 ] || [ "$execute_pm" -eq 1 ]; then
  route_contract="$(vcat "/petals/polymarket/meta/route-contract.json")"
  printf '%s' "$route_contract" | jq -e . >/dev/null ||
    die "Polymarket Petal route contract is unavailable"
  printf 'Polymarket Petal: loaded\n'
  onboard_status="$(vcat "/petals/polymarket/onboard/${wallet}/status.json")"
  printf '\nPolymarket onboarding status:\n'
  printf '%s\n' "$onboard_status" | jq .
  if printf '%s' "$onboard_status" |
    jq -e '.stage == "complete" and .tradeable == true' >/dev/null
  then
    printf 'Polymarket account snapshot:\n'
    vcat "/petals/polymarket/account/${wallet}/status.json" | jq .
    printf 'Polymarket buying power:\n'
    pm_buying_power="$(vcat "/petals/polymarket/account/${wallet}/buying_power.json")"
    printf '%s\n' "$pm_buying_power" | jq .
    if ! printf '%s' "$pm_buying_power" |
      jq -e '.can_trade_now == true' >/dev/null
    then
      if [ "$execute_pm" -eq 1 ] && [ "$pm_side" = "buy" ]; then
        printf 'BLOCKER: Polymarket reports no current buying power.\n' >&2
        preflight_blockers=1
      else
        printf 'NOTICE: Polymarket reports no current pUSD buying power; a sell may still be possible.\n' >&2
      fi
    fi
  else
    printf 'BLOCKER: Polymarket onboarding is not complete and tradeable.\n' >&2
    preflight_blockers=1
  fi
  if [ -n "$pm_slug" ]; then
    case "$pm_slug" in *[!A-Za-z0-9._-]*|'') die "Polymarket slug contains unsafe characters" ;; esac
    printf 'Requested Polymarket market:\n'
    vcat "/petals/polymarket/markets/${pm_slug}/market.json" | jq .
    printf 'Requested Polymarket prices:\n'
    vcat "/petals/polymarket/markets/${pm_slug}/prices.json" | jq .
  fi
fi

if [ "$live" -eq 0 ]; then
  [ "$preflight_blockers" -eq 0 ] ||
    die "preflight found an external prerequisite blocker; no order was submitted"
  printf '\nPreflight passed. No passkey prompt was opened and no order was submitted.\n'
  printf 'Re-run with the exact live arguments shown by --help when ready.\n'
  exit 0
fi

[ "$preflight_blockers" -eq 0 ] ||
  die "live preflight found an external prerequisite blocker; no order was staged"

if [ "$execute_hl" -eq 1 ]; then
  [ "$hl_resolved_coin" = "$hl_coin" ] ||
    die "Hyperliquid asset id ${hl_asset_id} resolves to '${hl_resolved_coin}', not '${hl_coin}'"
  session_path="/hyperliquid/mainnet/agent_sessions/${wallet}/${session_id}"
  session_policy="$(jq -nc \
    --arg coin "$hl_coin" --argjson cap 25 --argjson secs "$HL_SESSION_SECS" \
    '{
      allowed_assets:[$coin],
      allowed_order_types:["limit"],
      max_notional_usd:($cap|tostring),
      max_position_usd:($cap|tostring),
      max_loss_usd:($cap|tostring),
      max_session_secs:$secs,
      allow_reduce_only:true,
      allow_trigger_orders:false,
      allow_twap:false,
      allow_builder_fees:false,
      allow_vault_or_subaccount:false
    }')"
  session_request="$(jq -nc --arg id "$session_id" \
    --arg name "bloom-manual-mainnet-integration" \
    --argjson bounds "$session_policy" \
    '{id:$id,agent_name:$name,integration_bounds:$bounds}')"
  printf '\nPinned Hyperliquid request\n'
  printf '  %s %s %s @ %s, tif=%s, max notional=$%s\n' \
    "$hl_side" "$hl_size" "$hl_coin" "$hl_price" "$hl_tif" "$hl_notional"
  printf '  ephemeral policy:\n'
  printf '%s\n' "$session_policy" | jq .
fi

if [ "$execute_pm" -eq 1 ]; then
  printf '%s' "$onboard_status" |
    jq -e '.stage == "complete" and .tradeable == true' >/dev/null ||
    die "Polymarket onboarding is not complete/tradeable; no order has been sent"
  if [ "$pm_side" = "buy" ]; then
    pm_price_json="$(jq -nc --arg p "$pm_bound" '{max_price:$p}')"
  else
    pm_price_json="$(jq -nc --arg p "$pm_bound" '{min_price:$p}')"
  fi
  pm_request="$(jq -nc \
    --arg slug "$pm_slug" --arg outcome "$pm_outcome" --arg side "$pm_side" \
    --arg amount "$pm_amount" --arg order_type "$pm_order_type" \
    --argjson bound "$pm_price_json" \
    '{slug:$slug,outcome:$outcome,side:$side,amount:$amount,order_type:$order_type} + $bound')"
  printf '\nCreating the unsigned Polymarket draft for review...\n'
  drafts_before="$(bloom vfs ls "/petals/polymarket/trade/${wallet}/drafts" 2>/dev/null |
    awk '{print $1}' || true)"
  vwrite "/petals/polymarket/trade/${wallet}/new" "$pm_request"
  drafts_after="$(bloom vfs ls "/petals/polymarket/trade/${wallet}/drafts" |
    awk '{print $1}')"
  draft_id="$(comm -13 \
    <(printf '%s\n' "$drafts_before" | sed '/^$/d' | sort) \
    <(printf '%s\n' "$drafts_after" | sed '/^$/d' | sort) | tail -n 1)"
  [ -n "$draft_id" ] || die "could not identify the new Polymarket draft"
  draft_path="/petals/polymarket/trade/${wallet}/drafts/${draft_id}"
  vwrite "${draft_path}/revalidate" '{"revalidate":true}'
  printf '\nPolymarket draft plan:\n'
  vcat "${draft_path}/plan.md"
  printf '\nPolymarket policy check:\n'
  vcat "${draft_path}/policy_check.json" | jq .
  printf '\nFinal Polymarket quote:\n'
  vcat "${draft_path}/quote.json" | jq .
  printf '\nFinal Polymarket review intent:\n'
  vcat "${draft_path}/review_intent.json" | jq .
fi

if [ "$execute_hl" -eq 1 ] && [ "$execute_pm" -eq 1 ]; then
  mainnet_ack="EXECUTE BOTH MAINNET ORDERS"
elif [ "$execute_hl" -eq 1 ]; then
  mainnet_ack="EXECUTE HYPERLIQUID MAINNET ORDER"
else
  mainnet_ack="EXECUTE POLYMARKET MAINNET ORDER"
fi
printf '\nType exactly “%s” to authorize the selected submission(s): ' "$mainnet_ack"
IFS= read -r acknowledgement
[ "$acknowledgement" = "$mainnet_ack" ] || die "mainnet acknowledgement did not match"

if [ "$execute_hl" -eq 1 ]; then
  printf '\nStaging Hyperliquid session approval...\n'
  if vwrite "/hyperliquid/mainnet/agent_sessions/${wallet}/new.json" "$session_request"; then
    die "Hyperliquid session unexpectedly started without its passkey ceremony"
  fi
  open_approval "${session_path}/approval_challenge.json"
  vwrite "/hyperliquid/mainnet/agent_sessions/${wallet}/new.json" "$session_request"
  session_active=1
  printf 'Hyperliquid bounded session:\n'
  vcat "${session_path}/session.json" | jq .

  hl_is_buy=false
  [ "$hl_side" = "buy" ] && hl_is_buy=true
  hl_order="$(jq -nc \
    --argjson asset "$hl_asset_id" --argjson buy "$hl_is_buy" \
    --arg price "$hl_price" --arg size "$hl_size" --arg tif "$hl_tif" \
    '{
      action:{
        type:"order",
        orders:[{a:$asset,b:$buy,p:$price,s:$size,r:false,t:{limit:{tif:$tif}}}],
        grouping:"na"
      }
    }')"
  printf '\nSubmitting Hyperliquid order...\n'
  vwrite "${session_path}/order.json" "$hl_order"
  hl_response="$(vcat "${session_path}/last_response.json")"
  printf '%s\n' "$hl_response" | jq .
  printf '%s' "$hl_response" | jq -e '
    (.response.status == "ok") and
    ([.. | objects | .error? // empty] | length == 0)
  ' >/dev/null || die "Hyperliquid did not return a clean successful response"
fi

if [ "$execute_pm" -eq 1 ]; then
  printf '\nType exactly “POST POLYMARKET DRAFT %s” to request its passkey approval: ' "$draft_id"
  IFS= read -r pm_ack
  [ "$pm_ack" = "POST POLYMARKET DRAFT ${draft_id}" ] ||
    die "Polymarket draft acknowledgement did not match"
  post_request='{"post":true,"acknowledge_warnings":true}'
  if vwrite "${draft_path}/post" "$post_request"; then
    die "Polymarket draft unexpectedly posted without its passkey ceremony"
  fi
  open_approval "${draft_path}/approval.json"
  vwrite "${draft_path}/post" "$post_request"
  pm_receipt="$(vcat "/petals/polymarket/trade/${wallet}/receipts/${draft_id}/receipt.json")"
  printf '\nPolymarket receipt:\n'
  printf '%s\n' "$pm_receipt" | jq .
  printf '%s' "$pm_receipt" | jq -e '
    (.clob_status | ascii_downcase) as $status |
    ($status != "rejected") and ($status != "failed")
  ' >/dev/null || die "Polymarket receipt reports rejection/failure"
fi

if [ "$execute_hl" -eq 1 ]; then
  # Alo is post-only and may rest. Cancel all session orders before stopping so
  # the manual test cannot accidentally leave an order behind.
  if [ "$hl_tif" = "Alo" ]; then
    printf '\nCancelling any resting Hyperliquid order from the ALO test...\n'
    vwrite "${session_path}/cancel_all" '{}'
  fi
  vwrite "${session_path}/stop" '{}'
  session_active=0
fi

printf '\nPASS: selected mainnet venue submission(s) returned non-error receipts.\n'
printf 'Inspect fills/positions separately; venue acceptance does not guarantee a fill.\n'
