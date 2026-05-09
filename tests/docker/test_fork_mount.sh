#!/usr/bin/env bash
# tests/docker/test_fork_mount.sh — dockerized fork-mode mount test.
#
# Sibling of test_enso_aave.sh, but limited to the wallet/outbox +
# chain read surface. No Enso, no DeFi route. The point is to prove
# that an agent with shell access to /eth/ can:
#
#   1. Stage a plain native-ETH transfer via /eth/wallets/<w>/chains/<c>/outbox/new.tx
#   2. Broadcast it via /eth/wallets/<w>/chains/<c>/outbox/pending/<id>/confirm
#   3. Stage a SECOND tx and replace it (same nonce, bumped fees + fresh
#      calldata) via /eth/wallets/<w>/chains/<c>/outbox/pending/<id>/replace
#   4. Read tx receipt + chain head + gas via /eth/chains/<c>/...
#
# All writes go through the kernel NFS mount; nothing uses `beth ipc
# call` shortcuts. If this passes, the fork-mode mount surface is
# wired correctly end-to-end.
#
# Driven inside a beth-test container brought up by
# tests/docker/docker-compose-fork.yml (see run.sh --fork).
#
# Required env (set by docker-compose-fork.yml)
#   BASE_FORK_INTERNAL_URL        RPC URL the daemon hits (anvil-fork:8545)
#   BETH_TEST_WALLET_PASSPHRASE   passphrase for the imported test wallet

set -euo pipefail

MNT=/eth
HOME_DIR=/tmp/beth-fork-home
PIDFILE=/tmp/mount_demo.pid
LOGFILE=/tmp/mount_demo.log
SENTINEL=/.beth-mounted

WALLET=dest1
CHAIN=base
# anvil's deterministic account[0]
ANVIL_KEY=0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80
DEST1=0xf39Fd6e51aad88F6F4ce6aB8827279cfFFb92266
# anvil's deterministic account[1] — used as the recipient for the
# native-ETH send so we don't accidentally self-send to the sender.
RECIPIENT=0x70997970C51812dc3A010C7d01b50e0d17dc79C8

GREEN=$'\033[32m' YELLOW=$'\033[33m' RED=$'\033[31m' RESET=$'\033[0m'
log()  { printf '%s[fork-test]%s %s\n' "$GREEN"  "$RESET" "$*" >&2; }
warn() { printf '%s[fork-test]%s %s\n' "$YELLOW" "$RESET" "$*" >&2; }
fail() { printf '%s[fork-test]%s %s\n' "$RED"    "$RESET" "$*" >&2; exit 1; }

WALLET_PASSPHRASE="${BETH_TEST_WALLET_PASSPHRASE:-}"
[[ -n "$WALLET_PASSPHRASE" ]] || fail "BETH_TEST_WALLET_PASSPHRASE not set"
[[ -n "${BASE_FORK_INTERNAL_URL:-}" ]] || fail "BASE_FORK_INTERNAL_URL not set"
RPC_URL="$BASE_FORK_INTERNAL_URL"

mkdir -p "$MNT" "$HOME_DIR"
rm -rf "$HOME_DIR"/*

# ---------- write the daemon config ----------
log "writing config.toml (rpc: $RPC_URL)"
cat > "$HOME_DIR/config.toml" <<EOF
stage_ttl = "30m"
block_mainnet_broadcast = false
default_chain = "base"

[chains.base]
name = "base"
chain_id = 8453
rpc_urls = ["$RPC_URL"]
allow_broadcast = true
display_name = "Base (forked)"
native_symbol = "ETH"
native_decimals = 18
legacy_tx = false
EOF

# ---------- build mount_demo ----------
log "cargo build --release --features mount --example mount_demo"
cargo build \
    --release \
    --package beth-daemon \
    --features mount \
    --example mount_demo >&2

EXAMPLE_BIN=target/release/examples/mount_demo
if [[ ! -x "$EXAMPLE_BIN" ]]; then
    EXAMPLE_BIN=target/debug/examples/mount_demo
fi
[[ -x "$EXAMPLE_BIN" ]] || fail "could not find mount_demo binary"

# ---------- top up the test wallet on the fork ----------
# Anvil account[0] starts at 10k ETH on a fresh anvil, but a *fork*
# inherits real Base state, so the address may have nothing on real
# Base. Force 10 ETH so subsequent broadcasts have gas room.
log "anvil_setBalance $DEST1 := 10 ETH"
curl -fsS -X POST -H 'content-type: application/json' \
    --data '{"jsonrpc":"2.0","id":1,"method":"anvil_setBalance","params":["'"$DEST1"'","0x8AC7230489E80000"]}' \
    "$RPC_URL" \
    | sed -n 's/.*"result":\([^,}]*\).*/  result=\1/p' >&2 \
    || fail "anvil_setBalance failed (fork RPC unreachable?)"

# ---------- spawn the mount daemon ----------
log "spawning mount_demo (mount=$MNT home=$HOME_DIR)"
BETH_TEST_WALLET_NAME="$WALLET" \
BETH_TEST_WALLET_KEY="$ANVIL_KEY" \
BETH_TEST_WALLET_PASSPHRASE="$WALLET_PASSPHRASE" \
RUST_LOG="${RUST_LOG:-info}" \
    "$EXAMPLE_BIN" "$MNT" "$HOME_DIR" >"$LOGFILE" 2>&1 &
echo $! > "$PIDFILE"
DAEMON_PID=$(cat "$PIDFILE")
log "  pid=$DAEMON_PID, logging to $LOGFILE"

cleanup() {
    if [[ -f "$PIDFILE" ]]; then
        local pid; pid=$(cat "$PIDFILE")
        if kill -0 "$pid" 2>/dev/null; then
            log "stopping mount_demo (pid=$pid)"
            kill -TERM "$pid" 2>/dev/null || true
            for _ in 1 2 3 4 5 6 7 8 9 10; do
                kill -0 "$pid" 2>/dev/null || break
                sleep 1
            done
            kill -KILL "$pid" 2>/dev/null || true
        fi
    fi
    umount "$MNT" 2>/dev/null || true
    if [[ -f "$LOGFILE" ]]; then
        echo '::group::mount_demo log (tail)' >&2
        tail -n 200 "$LOGFILE" >&2 || true
        echo '::endgroup::' >&2
    fi
}
trap cleanup EXIT

# Wait up to 90s for the .beth-mounted sentinel.
log "waiting for $SENTINEL"
for i in $(seq 1 90); do
    if [[ -f "$SENTINEL" ]]; then
        log "  sentinel found after ${i}s"
        break
    fi
    if ! kill -0 "$DAEMON_PID" 2>/dev/null; then
        echo 'mount_demo exited before mount; tail of log:' >&2
        tail -n 60 "$LOGFILE" >&2 || true
        exit 1
    fi
    sleep 1
done
[[ -f "$SENTINEL" ]] || fail "timed out waiting for mount sentinel"

# ---------- breadcrumbs ----------
echo '::group::chain head' >&2
HEAD_NUMBER=$(cat "$MNT/chains/$CHAIN/head/number" | tr -d '\n')
log "chain head: block $HEAD_NUMBER"
echo '::endgroup::' >&2

echo '::group::wallet eth balance' >&2
BAL_ETH=$(cat "$MNT/chains/$CHAIN/addresses/$DEST1/balance.eth" | tr -d '\n')
log "$WALLET ($DEST1) ETH balance: $BAL_ETH"
echo '::endgroup::' >&2

[[ -n "$BAL_ETH" && "$BAL_ETH" != "0" ]] || fail "$WALLET ETH balance is 0 after anvil_setBalance"

# ---------- 1. stage + confirm a native-ETH send via the outbox ----------
# Snapshot the pending set so we can diff it after staging to find the
# new id without parsing the daemon log.
pending_set() {
    ls "$MNT/wallets/$WALLET/chains/$CHAIN/outbox/pending" 2>/dev/null \
        | sort -u | tr '\n' '|' || true
}

PENDING_BEFORE=$(pending_set)

# Value carries an explicit unit. Without one, parse_amount defaults to
# "wei" and rejects fractional digits, so "0.001" fails amount parsing —
# which the engine surfaces as HandlerError::Backend, which the mount
# adapter maps to FsError::Io, which the kernel surfaces to userspace
# as EIO. Use "0.001 eth" so it parses as 1e15 wei.
INTENT_BODY_1=$(printf '{"to":"%s","value":"0.001 eth"}' "$RECIPIENT")
log "stage tx (outbox/new.tx <- '$INTENT_BODY_1')"
printf '%s' "$INTENT_BODY_1" > "$MNT/wallets/$WALLET/chains/$CHAIN/outbox/new.tx"

# `stage` is synchronous wrt our in-process VFS, so the new pending dir
# is visible as soon as the write returns. Diff to find the id.
PENDING_AFTER=$(pending_set)
STAGE_1=$(comm -13 \
    <(printf '%s' "$PENDING_BEFORE" | tr '|' '\n' | sort -u) \
    <(printf '%s' "$PENDING_AFTER"  | tr '|' '\n' | sort -u) \
    | grep -v '^$' | head -n1 || true)
[[ -n "$STAGE_1" ]] || fail "no pending stage produced after outbox/new.tx write"
log "  stage id: $STAGE_1"

STAGE_DIR_1="$MNT/wallets/$WALLET/chains/$CHAIN/outbox/pending/$STAGE_1"
echo '::group::stage 1 plan.md' >&2
cat "$STAGE_DIR_1/plan.md" >&2 || true
echo '::endgroup::' >&2

# Verify the stage advertises the writable control files (these are
# virtual sinks the handler returns even before they exist on disk).
log "verify stage advertises confirm/replace/cancel"
ls "$STAGE_DIR_1" >&2
for ctrl in confirm replace cancel; do
    # The mount only exposes these as writable files; ls follows the
    # readdir reply, so the names should appear without erroring.
    if ! ls "$STAGE_DIR_1" 2>/dev/null | grep -q "^${ctrl}\$"; then
        warn "  $ctrl not advertised under $STAGE_DIR_1 (continuing)"
    fi
done

# Confirm the first stage — broadcasts on the fork.
log "confirm stage 1 (broadcast on fork)"
echo y > "$STAGE_DIR_1/confirm"

# After confirm returns, the stage moves to sent/<id> and tx_hash is
# readable.
HASH_1=$(cat "$MNT/wallets/$WALLET/chains/$CHAIN/outbox/sent/$STAGE_1/tx_hash" \
    | tr -d '\n' || true)
[[ -n "$HASH_1" ]] || fail "tx_hash missing after broadcast of stage 1"
log "  tx hash: $HASH_1"

# ---------- 2. stage a SECOND tx and replace it (bump fees + new calldata)
# The first tx was confirmed and moved to sent/, so the wallet's nonce
# advanced. A second stage uses the new nonce; we then replace it
# in-place (same nonce, bumped fees, possibly different calldata).
PENDING_BEFORE=$(pending_set)

INTENT_BODY_2=$(printf '{"to":"%s","value":"0.0005 eth"}' "$RECIPIENT")
log "stage tx 2 (outbox/new.tx <- '$INTENT_BODY_2')"
printf '%s' "$INTENT_BODY_2" > "$MNT/wallets/$WALLET/chains/$CHAIN/outbox/new.tx"

PENDING_AFTER=$(pending_set)
STAGE_2=$(comm -13 \
    <(printf '%s' "$PENDING_BEFORE" | tr '|' '\n' | sort -u) \
    <(printf '%s' "$PENDING_AFTER"  | tr '|' '\n' | sort -u) \
    | grep -v '^$' | head -n1 || true)
[[ -n "$STAGE_2" ]] || fail "no pending stage produced after second outbox/new.tx write"
log "  stage id: $STAGE_2"

STAGE_DIR_2="$MNT/wallets/$WALLET/chains/$CHAIN/outbox/pending/$STAGE_2"

# Replace with a fresh intent — same wallet, but value drops to almost
# zero (and fees auto-bump by 10%). The replace handler broadcasts.
INTENT_REPLACE=$(printf '{"to":"%s","value":"0.00001 eth"}' "$RECIPIENT")
log "replace stage 2 (outbox/pending/$STAGE_2/replace <- '$INTENT_REPLACE')"
printf '%s' "$INTENT_REPLACE" > "$STAGE_DIR_2/replace"

# `replace_with_intent` writes replacement_tx_hash next to the original
# pending entry. Read it back through the mount.
REPLACE_HASH=$(cat "$STAGE_DIR_2/replacement_tx_hash" | tr -d '\n' || true)
[[ -n "$REPLACE_HASH" ]] || fail "replacement_tx_hash missing after replace"
log "  replacement hash: $REPLACE_HASH"
[[ "$REPLACE_HASH" =~ ^0x[0-9a-fA-F]{64}$ ]] \
    || fail "replacement hash not a 32-byte hex ('$REPLACE_HASH')"

# ---------- 3. read-heavy chain reads against the broadcast tx ----------
# Use HASH_1 (stage 1) for the receipt reads — it confirmed cleanly on
# the fork. Anvil mines blocks at 1s intervals so the receipt should
# appear within a couple of polls.
log "polling /chains/$CHAIN/tx/$HASH_1/status (60s budget)"
STATUS=
for i in $(seq 1 60); do
    STATUS=$(cat "$MNT/chains/$CHAIN/tx/$HASH_1/status" 2>/dev/null | tr -d '\n' || true)
    case "$STATUS" in
        success)
            log "  status=success after ${i}s"
            break
            ;;
        reverted)
            fail "stage 1 tx reverted on-chain (hash=$HASH_1)"
            ;;
        *)
            sleep 1
            ;;
    esac
done
[[ "$STATUS" == "success" ]] || fail "tx 1 did not confirm within 60s (last status='$STATUS')"

# Read 1: chain head full.json — the daemon wraps the latest block as
# pretty-printed JSON. Assert non-empty + starts with `{`.
HEAD_JSON="$MNT/chains/$CHAIN/head/full.json"
log "read head json: $HEAD_JSON"
[[ -s "$HEAD_JSON" ]] || fail "head full.json is empty"
head1=$(head -c1 "$HEAD_JSON")
[[ "$head1" == "{" ]] || fail "head full.json does not start with '{' (got '$head1')"

# Read 2: tx receipt for the broadcast tx. Multiple files under the
# tx subtree — verify the high-value ones are populated. (This mirrors
# the assertions test_enso_aave.sh runs after a successful broadcast.)
TX_DIR="$MNT/chains/$CHAIN/tx/$HASH_1"
log "read tx receipt subtree: $TX_DIR"

BLOCK_NUMBER=$(cat "$TX_DIR/block_number" 2>/dev/null | tr -d '\n' || true)
[[ -n "$BLOCK_NUMBER" && "$BLOCK_NUMBER" =~ ^[0-9]+$ ]] \
    || fail "block_number empty or non-numeric ('$BLOCK_NUMBER') at $TX_DIR/block_number"
log "  block_number: $BLOCK_NUMBER"

GAS_USED=$(cat "$TX_DIR/gas_used" 2>/dev/null | tr -d '\n' || true)
[[ -n "$GAS_USED" && "$GAS_USED" =~ ^[0-9]+$ ]] \
    || fail "gas_used empty or non-numeric ('$GAS_USED') at $TX_DIR/gas_used"
log "  gas_used: $GAS_USED"

# Receipt + logs JSON: the container ships without jq, so we sniff the
# leading character to confirm we got JSON (not an error string).
for spec in 'receipt.json:{' 'logs.json:[' 'full.json:{'; do
    f="${spec%:*}"
    expect="${spec##*:}"
    P="$TX_DIR/$f"
    [[ -s "$P" ]] || fail "$f is empty at $P"
    head1=$(head -c1 "$P")
    [[ "$head1" == "$expect" ]] \
        || fail "$f does not start with '$expect' (got '$head1') at $P"
    sz=$(wc -c <"$P" | tr -d ' ')
    log "  $f: ok (${sz}B)"
done

# Read 3: gas/current.json — exposes the current gas_price_wei. The
# fork RPC always answers eth_gasPrice, so this should never be empty.
GAS_JSON="$MNT/chains/$CHAIN/gas/current.json"
log "read gas json: $GAS_JSON"
[[ -s "$GAS_JSON" ]] || fail "gas current.json is empty"
head1=$(head -c1 "$GAS_JSON")
[[ "$head1" == "{" ]] || fail "gas current.json does not start with '{' (got '$head1')"
# Cheap key sniff — no jq in the container, but grep -q is fine for a
# regression check.
grep -q 'gas_price_wei' "$GAS_JSON" \
    || fail "gas current.json missing 'gas_price_wei' field"

# Read 4 (bonus): block by number — the latest mined block via the
# numeric path. Confirms the /chains/<c>/blocks/<n>/full.json route
# resolves end to end (a path test.sh doesn't cover).
BLOCK_JSON="$MNT/chains/$CHAIN/blocks/$BLOCK_NUMBER/full.json"
log "read block json: $BLOCK_JSON"
[[ -s "$BLOCK_JSON" ]] || fail "blocks/$BLOCK_NUMBER/full.json is empty"
head1=$(head -c1 "$BLOCK_JSON")
[[ "$head1" == "{" ]] || fail "block full.json does not start with '{' (got '$head1')"

log "===== fork-mode mount integration test PASSED ====="
exit 0
