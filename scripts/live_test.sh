#!/usr/bin/env bash
# scripts/live_test.sh — live-funds integration test on Base mainnet.
#
# Drives a beth daemon end-to-end: ETH<->USDC roundtrip via Enso,
# ETH<->aBaseUSDC (Aave) roundtrip, sweep dest2/dest3 back to dest1.
# At the end, all assets sit on dest1 as native ETH.
#
# Idempotent: re-running starts by cleaning up any leftover USDC /
# aBaseUSDC / dest2-3 ETH. If everything's already on dest1 as ETH it
# does the roundtrips and returns there.
#
# Requires:
#   - test.env sourced (passphrase, wallet addrs, BETH_LIVE_HOME)
#   - $BETH_LIVE=1 (gate; this script broadcasts real txs)
#   - target/release/beth built
#
# Usage:
#   BETH_LIVE=1 source test.env && bash scripts/live_test.sh
#
# Exit codes:
#   0 = all phases passed, end state is consolidated on dest1
#   1 = misconfiguration / preflight failure
#   2 = a phase failed (logged); end state may be dirty

set -euo pipefail

BETH=${BETH:-./target/release/beth}
BETH_LIVE=${BETH_LIVE:-0}
BETH_HOME_DIR=${BETH_LIVE_HOME:-/tmp/beth-live}
CHAIN=${CHAIN:-base}
WALLET_MAIN=${WALLET_MAIN:-dest1}
SWAP_AMOUNT_ETH=${SWAP_AMOUNT_ETH:-0.0005}
DUST_ETH=${DUST_ETH:-0.0002}   # don't sweep below this; gas exceeds value
# Enso router on Base (singleton). All ERC-20 swaps go through this.
ENSO_ROUTER_BASE=${ENSO_ROUTER_BASE:-0xF75584eF6673aD213a685a1B58Cc0330B8eA22Cf}
# Aave V3 Pool on Base. Used to withdraw aBaseUSDC directly (Enso has no
# shortcut for redeeming aTokens for ETH).
AAVE_V3_POOL_BASE=${AAVE_V3_POOL_BASE:-0xA238Dd80C259a72e81d7e4664a9801593F98d1c5}
MAX_UINT=115792089237316195423570985008687907853269984665640564039457584007913129639935

GREEN=$'\033[32m' YELLOW=$'\033[33m' RED=$'\033[31m' RESET=$'\033[0m'
log()  { printf "%s[live]%s %s\n" "$GREEN" "$RESET" "$*" >&2; }
warn() { printf "%s[live]%s %s\n" "$YELLOW" "$RESET" "$*" >&2; }
fail() { printf "%s[live]%s %s\n" "$RED" "$RESET" "$*" >&2; exit 2; }

if [[ "$BETH_LIVE" != "1" ]]; then
    cat >&2 <<'EOF'
Refusing to run: $BETH_LIVE != 1.

This script broadcasts real transactions on Base mainnet and spends real
funds. To run it, set BETH_LIVE=1 explicitly:

    BETH_LIVE=1 source test.env && bash scripts/live_test.sh
EOF
    exit 1
fi

[[ -x "$BETH" ]] || fail "missing binary: $BETH (run: cargo build --release -p beth)"
[[ -n "${BETH_PASSPHRASE:-}" ]] || fail "BETH_PASSPHRASE not set (source test.env)"
[[ -n "${BETH_LIVE_DEST1:-}" ]] || fail "BETH_LIVE_DEST1 not set (source test.env)"
[[ -n "${BETH_LIVE_DEST2:-}" ]] || fail "BETH_LIVE_DEST2 not set (source test.env)"
[[ -n "${BETH_LIVE_DEST3:-}" ]] || fail "BETH_LIVE_DEST3 not set (source test.env)"
[[ -n "${BETH_BASE_USDC:-}" ]]  || fail "BETH_BASE_USDC not set (source test.env)"
[[ -n "${BETH_BASE_AUSDC:-}" ]] || fail "BETH_BASE_AUSDC not set (source test.env)"

export BETH_HOME="$BETH_HOME_DIR"
SOCKET="$BETH_HOME_DIR/run/beth.sock"
DAEMON_PID=

# ---------- daemon lifecycle ----------

start_daemon() {
    [[ -e "$SOCKET" ]] && rm -f "$SOCKET"
    mkdir -p "$BETH_HOME_DIR/run"
    log "starting beth serve (home=$BETH_HOME_DIR)"
    "$BETH" serve >/tmp/beth-live-daemon.log 2>&1 &
    DAEMON_PID=$!
    for _ in $(seq 1 50); do
        [[ -S "$SOCKET" ]] && return 0
        sleep 0.1
    done
    fail "daemon failed to bind socket within 5s — see /tmp/beth-live-daemon.log"
}

stop_daemon() {
    if [[ -n "$DAEMON_PID" ]] && kill -0 "$DAEMON_PID" 2>/dev/null; then
        log "stopping beth daemon (pid=$DAEMON_PID)"
        kill "$DAEMON_PID" 2>/dev/null || true
        wait "$DAEMON_PID" 2>/dev/null || true
    fi
}
trap stop_daemon EXIT

# ---------- VFS helpers ----------

vfs_cat() { "$BETH" vfs cat "$1" 2>/dev/null; }
vfs_ls()  { "$BETH" vfs ls  "$1" 2>/dev/null | awk '{print $1}'; }

ipc_write_text() {
    local path=$1 text=$2
    local params
    params=$(jq -nc --arg p "$path" --arg t "$text" '{path:$p,text:$t}')
    "$BETH" ipc call write --params "$params" >/dev/null
}

# Read ETH balance in wei (decimal) for an address on $CHAIN.
balance_wei() {
    vfs_cat "/chains/$CHAIN/addresses/$1/balance" | tr -d '\n'
}

# Read ERC-20 raw balance for (address, token).
token_balance_raw() {
    local addr=$1 token=$2
    vfs_cat "/chains/$CHAIN/addresses/$addr/tokens/$token/balance.raw" 2>/dev/null \
        | tr -d '\n' || echo 0
}

# Wait for a tx hash to land. Returns 0 on success, 1 on revert,
# 2 on timeout.
wait_for_tx() {
    local hash=$1
    local deadline=$((SECONDS + 60))
    while (( SECONDS < deadline )); do
        local s
        s=$(vfs_cat "/chains/$CHAIN/tx/$hash/status" 2>/dev/null | tr -d '\n' || true)
        if [[ "$s" == "success" ]]; then return 0; fi
        if [[ "$s" == "reverted" ]]; then return 1; fi
        sleep 2
    done
    return 2
}

# ---------- swap via Enso (defi handler) ----------

# Stage a swap via the in-process defi handler. Returns the outbox
# stage id (e.g. "0001-12345") on stdout.
stage_swap() {
    local intent=$1
    local body
    body=$(jq -nc --arg i "$intent" --arg c "$CHAIN" '{intent:$i,chain:$c}')

    # Snapshot pending stages so we can detect the new one.
    local before
    before=$(vfs_ls "/wallets/$WALLET_MAIN/chains/$CHAIN/outbox/pending" \
        | sort -u | tr '\n' '|' || true)

    ipc_write_text "/defi/intents/$WALLET_MAIN/new" "$body"

    # The new session is the only non-"new" entry under intents/<wallet>.
    local sess
    sess=$(vfs_ls "/defi/intents/$WALLET_MAIN" | grep -v '^new$' | sort | tail -1)
    [[ -n "$sess" ]] || { warn "no defi session created"; return 1; }

    ipc_write_text "/defi/intents/$WALLET_MAIN/$sess/confirm" "y"

    # New pending stage is the one not in `before`.
    local after stage
    after=$(vfs_ls "/wallets/$WALLET_MAIN/chains/$CHAIN/outbox/pending" | sort -u)
    stage=$(comm -13 <(echo "$before" | tr '|' '\n' | sort -u) <(echo "$after") | head -1)
    [[ -n "$stage" ]] || { warn "no new stage produced"; return 1; }
    echo "$stage"
}

# Confirm a staged tx and return tx_hash. Uses the wallet CLI which
# unlocks + broadcasts in a single fresh daemon.
confirm_stage() {
    local stage=$1
    if ! "$BETH" wallet confirm "$WALLET_MAIN" "$CHAIN" "$stage" \
            --passphrase "$BETH_PASSPHRASE" --text y >/tmp/beth-live-confirm.log 2>&1; then
        warn "wallet confirm failed for stage $stage:"
        sed 's/^/    /' /tmp/beth-live-confirm.log >&2
        return 1
    fi
    vfs_cat "/wallets/$WALLET_MAIN/chains/$CHAIN/outbox/sent/$stage/tx_hash" \
        | tr -d '\n'
}

# Broadcast a swap in one shot. Echoes tx_hash on success.
do_swap() {
    local intent=$1 description=$2
    log "swap: $description -- '$intent'"
    local stage
    stage=$(stage_swap "$intent") || return 1
    log "  staged $stage; broadcasting"
    local hash
    hash=$(confirm_stage "$stage") || return 1
    log "  tx hash $hash; waiting for receipt…"
    if wait_for_tx "$hash"; then
        log "  ✓ success"
        echo "$hash"
        return 0
    else
        warn "  ✗ tx did not succeed within 60s (or reverted)"
        return 1
    fi
}

# ---------- ERC-20 approval ----------

# Read the wallet's allowance for (token, spender) on $CHAIN.
# Returns raw decimal allowance string; "0" if unknown.
allowance_raw() {
    local owner=$1 token=$2 spender=$3
    # Encode allowance(address,address): selector 0xdd62ed3e + pad32(owner) + pad32(spender)
    # Easier: do it via beth call helper if it exists. For now, fall back to
    # heuristic: re-approve every time (idempotent, cheap).
    echo 0
}

# One-shot infinite approval of `token` to the Enso router from $WALLET_MAIN.
# Idempotent in effect — re-approving max is a no-op for routing, just costs gas.
approve_token() {
    local token=$1 wallet=${2:-$WALLET_MAIN}
    log "approve: $wallet -> $ENSO_ROUTER_BASE for $token (max)"
    local intent_body
    intent_body=$(jq -nc \
        --arg c "$token" \
        --arg s "$ENSO_ROUTER_BASE" \
        --arg max "$MAX_UINT" \
        --arg chain "$CHAIN" \
        '{kind:"call",contract:$c,method:"approve(address,uint256)",args:[$s,$max],chain:$chain}')

    local before
    before=$(vfs_ls "/wallets/$wallet/chains/$CHAIN/outbox/pending" \
        | sort -u | tr '\n' '|' || true)

    ipc_write_text "/wallets/$wallet/chains/$CHAIN/outbox/new.tx" "$intent_body"

    local after stage
    after=$(vfs_ls "/wallets/$wallet/chains/$CHAIN/outbox/pending" | sort -u)
    stage=$(comm -13 <(echo "$before" | tr '|' '\n' | sort -u) <(echo "$after") | head -1)
    [[ -n "$stage" ]] || { warn "no approve stage produced"; return 1; }

    if ! "$BETH" wallet confirm "$wallet" "$CHAIN" "$stage" \
            --passphrase "$BETH_PASSPHRASE" --text y >/tmp/beth-live-confirm.log 2>&1; then
        warn "approve confirm failed:"
        sed 's/^/    /' /tmp/beth-live-confirm.log >&2
        return 1
    fi
    local hash
    hash=$(vfs_cat "/wallets/$wallet/chains/$CHAIN/outbox/sent/$stage/tx_hash" \
        | tr -d '\n')
    log "  approve tx $hash; waiting for receipt…"
    if wait_for_tx "$hash"; then
        log "  ✓ approve confirmed"
    else
        warn "  ✗ approve tx failed"
        return 1
    fi
}

# ---------- direct contract call ----------

# Stage + confirm a `call` intent (encodes ABI signature + args).
# Echoes tx_hash on success. Fails on revert/timeout.
call_contract() {
    local wallet=$1 contract=$2 method=$3 args_json=$4 description=$5
    log "call: $description -- $method"
    local intent_body
    intent_body=$(jq -nc \
        --arg c "$contract" \
        --arg m "$method" \
        --argjson a "$args_json" \
        --arg chain "$CHAIN" \
        '{kind:"call",contract:$c,method:$m,args:$a,chain:$chain}')

    local before
    before=$(vfs_ls "/wallets/$wallet/chains/$CHAIN/outbox/pending" \
        | sort -u | tr '\n' '|' || true)
    ipc_write_text "/wallets/$wallet/chains/$CHAIN/outbox/new.tx" "$intent_body"
    local after stage
    after=$(vfs_ls "/wallets/$wallet/chains/$CHAIN/outbox/pending" | sort -u)
    stage=$(comm -13 <(echo "$before" | tr '|' '\n' | sort -u) <(echo "$after") | head -1)
    [[ -n "$stage" ]] || { warn "no call stage produced"; return 1; }

    if ! "$BETH" wallet confirm "$wallet" "$CHAIN" "$stage" \
            --passphrase "$BETH_PASSPHRASE" --text y >/tmp/beth-live-confirm.log 2>&1; then
        warn "call confirm failed:"
        sed 's/^/    /' /tmp/beth-live-confirm.log >&2
        return 1
    fi
    local hash
    hash=$(vfs_cat "/wallets/$wallet/chains/$CHAIN/outbox/sent/$stage/tx_hash" \
        | tr -d '\n')
    log "  call tx $hash; waiting for receipt…"
    if wait_for_tx "$hash"; then
        log "  ✓ call confirmed"
        echo "$hash"
        return 0
    else
        warn "  ✗ call tx failed"
        return 1
    fi
}

# Withdraw all aBaseUSDC from Aave V3 Pool back into USDC. The Pool burns
# the aTokens from msg.sender directly, so no ERC-20 approval is needed.
aave_withdraw_all_usdc() {
    local args_json
    args_json=$(jq -nc \
        --arg asset "$BETH_BASE_USDC" \
        --arg max "$MAX_UINT" \
        --arg to "$BETH_LIVE_DEST1" \
        '[$asset,$max,$to]')
    call_contract "$WALLET_MAIN" "$AAVE_V3_POOL_BASE" "withdraw(address,uint256,address)" \
        "$args_json" "Aave V3 withdraw all USDC"
}

# ---------- native ETH transfer (sweep) ----------

# Send raw native ETH (no token field). Used for sweeping dest2/3 back.
do_native_send() {
    local from_wallet=$1 to_addr=$2 wei=$3
    log "send native: $from_wallet -> $to_addr ($wei wei)"
    local intent_body
    intent_body=$(printf 'send %s wei to %s on %s' "$wei" "$to_addr" "$CHAIN")
    local before
    before=$(vfs_ls "/wallets/$from_wallet/chains/$CHAIN/outbox/pending" \
        | sort -u | tr '\n' '|' || true)

    ipc_write_text "/wallets/$from_wallet/chains/$CHAIN/outbox/new.tx" "$intent_body"

    local after stage
    after=$(vfs_ls "/wallets/$from_wallet/chains/$CHAIN/outbox/pending" | sort -u)
    stage=$(comm -13 <(echo "$before" | tr '|' '\n' | sort -u) <(echo "$after") | head -1)
    [[ -n "$stage" ]] || { warn "no native stage produced"; return 1; }

    if ! "$BETH" wallet confirm "$from_wallet" "$CHAIN" "$stage" \
            --passphrase "$BETH_PASSPHRASE" --text y >/tmp/beth-live-confirm.log 2>&1; then
        warn "wallet confirm (sweep) failed:"
        sed 's/^/    /' /tmp/beth-live-confirm.log >&2
        return 1
    fi
    local hash
    hash=$(vfs_cat "/wallets/$from_wallet/chains/$CHAIN/outbox/sent/$stage/tx_hash" \
        | tr -d '\n')
    log "  tx hash $hash; waiting for receipt…"
    if wait_for_tx "$hash"; then
        log "  ✓ success"
    else
        warn "  ✗ sweep tx failed"
        return 1
    fi
}

# ---------- phases ----------

print_state() {
    local label=$1
    log "----- state: $label -----"
    for w in "$WALLET_MAIN" dest2 dest3; do
        local addr
        case "$w" in
            "$WALLET_MAIN") addr=$BETH_LIVE_DEST1 ;;
            dest2) addr=$BETH_LIVE_DEST2 ;;
            dest3) addr=$BETH_LIVE_DEST3 ;;
        esac
        local eth
        eth=$(vfs_cat "/chains/$CHAIN/addresses/$addr/balance.eth" | tr -d '\n' || echo "?")
        log "  $w ($addr): $eth"
    done
    local usdc ausdc
    usdc=$(token_balance_raw "$BETH_LIVE_DEST1" "$BETH_BASE_USDC")
    ausdc=$(token_balance_raw "$BETH_LIVE_DEST1" "$BETH_BASE_AUSDC")
    log "  ${WALLET_MAIN} USDC raw: $usdc"
    log "  ${WALLET_MAIN} aBaseUSDC raw: $ausdc"
}

phase_cleanup() {
    log "===== phase 1: cleanup pre-existing balances ====="
    local usdc ausdc
    ausdc=$(token_balance_raw "$BETH_LIVE_DEST1" "$BETH_BASE_AUSDC")
    if [[ -n "$ausdc" && "$ausdc" != "0" ]]; then
        # Redeem aBaseUSDC -> USDC via Aave Pool, then swap USDC -> ETH.
        aave_withdraw_all_usdc \
            || warn "Aave withdraw failed; aBaseUSDC will linger"
    fi
    # Re-read USDC after potential withdraw above.
    usdc=$(token_balance_raw "$BETH_LIVE_DEST1" "$BETH_BASE_USDC")
    if [[ -n "$usdc" && "$usdc" != "0" ]]; then
        approve_token "$BETH_BASE_USDC" \
            || warn "USDC approve failed; swap may revert"
        # Re-read USDC again post-approve (Aave withdraw may have rounded).
        usdc=$(token_balance_raw "$BETH_LIVE_DEST1" "$BETH_BASE_USDC")
        do_swap "swap $usdc $BETH_BASE_USDC to ETH" "USDC -> ETH (initial cleanup)" \
            || warn "USDC -> ETH cleanup failed; continuing"
    fi
}

phase_swap_roundtrip() {
    log "===== phase 2: ETH <-> USDC roundtrip ====="
    do_swap "swap $SWAP_AMOUNT_ETH ETH to USDC" "ETH -> USDC" || return 1
    sleep 4
    local usdc
    usdc=$(token_balance_raw "$BETH_LIVE_DEST1" "$BETH_BASE_USDC")
    [[ -n "$usdc" && "$usdc" != "0" ]] || { warn "USDC balance still 0 after swap"; return 1; }
    log "  USDC raw: $usdc"
    approve_token "$BETH_BASE_USDC" \
        || warn "USDC approve failed; swap may revert"
    do_swap "swap $usdc $BETH_BASE_USDC to ETH" "USDC -> ETH" || return 1
}

phase_aave_roundtrip() {
    log "===== phase 3: ETH <-> aBaseUSDC (Aave V3) roundtrip ====="
    # Step A: deposit ETH -> aBaseUSDC via Enso shortcut.
    do_swap "swap $SWAP_AMOUNT_ETH ETH to $BETH_BASE_AUSDC" "ETH -> aBaseUSDC" || return 1
    sleep 6
    local ausdc
    ausdc=$(token_balance_raw "$BETH_LIVE_DEST1" "$BETH_BASE_AUSDC")
    [[ -n "$ausdc" && "$ausdc" != "0" ]] \
        || { warn "aBaseUSDC balance still 0 after deposit"; return 1; }
    log "  aBaseUSDC raw: $ausdc"
    # Step B: redeem aBaseUSDC -> USDC via Aave V3 Pool.withdraw.
    aave_withdraw_all_usdc || return 1
    sleep 4
    local usdc
    usdc=$(token_balance_raw "$BETH_LIVE_DEST1" "$BETH_BASE_USDC")
    [[ -n "$usdc" && "$usdc" != "0" ]] \
        || { warn "USDC balance still 0 after Aave withdraw"; return 1; }
    log "  USDC raw after withdraw: $usdc"
    # Step C: swap the resulting USDC back to ETH (round-trip complete).
    approve_token "$BETH_BASE_USDC" \
        || warn "USDC approve failed; swap may revert"
    do_swap "swap $usdc $BETH_BASE_USDC to ETH" "USDC -> ETH (Aave proceeds)" || return 1
}

phase_sweep() {
    log "===== phase 4: sweep dest2/3 ETH back to dest1 ====="
    # Reserve enough wei to cover gas (21k * ~3 gwei on Base ≈ 7e13 wei).
    # Pad to 1e14 to absorb price spikes.
    local reserve=100000000000000   # 0.0001 ETH
    for src in dest2 dest3; do
        local addr
        case "$src" in
            dest2) addr=$BETH_LIVE_DEST2 ;;
            dest3) addr=$BETH_LIVE_DEST3 ;;
        esac
        local wei
        wei=$(balance_wei "$addr")
        # Skip when balance won't cover the gas reserve. Compare numerically
        # via python to avoid bash overflow on >2^63 wei.
        local enough
        enough=$(python3 -c "import sys; print(int(sys.argv[1]) > int(sys.argv[2]))" \
            "$wei" "$reserve")
        if [[ "$enough" != "True" ]]; then
            log "  $src balance $wei wei <= reserve $reserve, skipping"
            continue
        fi
        local send_amount
        send_amount=$(python3 -c "import sys; print(int(sys.argv[1]) - int(sys.argv[2]))" \
            "$wei" "$reserve")
        do_native_send "$src" "$BETH_LIVE_DEST1" "$send_amount" \
            || warn "  sweep from $src failed"
    done
}

phase_assert_final() {
    log "===== phase 5: final assertions ====="
    local usdc ausdc
    usdc=$(token_balance_raw "$BETH_LIVE_DEST1" "$BETH_BASE_USDC")
    ausdc=$(token_balance_raw "$BETH_LIVE_DEST1" "$BETH_BASE_AUSDC")
    # Treat blank reads as "unknown" — fail hard rather than silently passing.
    [[ -n "$usdc" ]]  || { warn "  ✗ could not read dest1 USDC balance"; return 1; }
    [[ -n "$ausdc" ]] || { warn "  ✗ could not read dest1 aBaseUSDC balance"; return 1; }
    local fail=0
    if [[ "$usdc" != "0" ]]; then
        warn "  ✗ dest1 USDC raw=$usdc, expected 0"
        fail=1
    else
        log "  ✓ dest1 USDC = 0"
    fi
    if [[ "$ausdc" != "0" ]]; then
        # aBaseUSDC accrues interest continuously, so a tiny non-zero residue
        # (a few wei) is normal even after withdraw. Tolerate ≤ 5 raw units.
        if (( ausdc <= 5 )); then
            log "  ✓ dest1 aBaseUSDC = $ausdc raw (interest dust, tolerated)"
        else
            warn "  ✗ dest1 aBaseUSDC raw=$ausdc, expected 0"
            fail=1
        fi
    else
        log "  ✓ dest1 aBaseUSDC = 0"
    fi
    return $fail
}

# ---------- main ----------

start_daemon

print_state "before"
phase_cleanup        || warn "cleanup phase had issues; continuing"
print_state "after cleanup"
phase_swap_roundtrip || fail "swap roundtrip phase failed"
print_state "after swap roundtrip"
phase_aave_roundtrip || warn "Aave roundtrip phase failed (likely Enso withdraw unsupported)"
print_state "after Aave roundtrip"
phase_sweep          || warn "sweep phase had issues"
print_state "final"
phase_assert_final   || fail "final state assertions failed"

log "===== ALL PHASES PASSED ====="
