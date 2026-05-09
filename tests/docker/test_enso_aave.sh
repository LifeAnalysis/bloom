#!/usr/bin/env bash
# tests/docker/test_enso_aave.sh — dockerized Enso -> Aave integration
# test driver. Runs *inside* the beth-test container brought up by
# tests/docker/docker-compose-enso.yml (fork mode) or by
# tests/docker/run.sh --enso-live (live mainnet mode).
#
# What this proves
#   The agent-facing surface (NFS mount at /eth/) end-to-ends a real
#   DeFi intent: ETH -> aBaseUSDC via Enso shortcut -> Aave V3 supply.
#   Every step except the wallet unlock (in-process by design) is
#   driven through plain filesystem ops on /eth/ — no `beth vfs write`
#   short-circuits, no `beth ipc call`. If this test passes, an agent
#   with shell access to /eth/ can place real DeFi trades.
#
# Modes (selected by BETH_TEST_MODE; default "fork")
#   fork  — broadcasts land on an anvil --fork-url=Base sidecar.
#           Throwaway state, no real funds.
#   live  — broadcasts land on Base mainnet via $BETH_BASE_RPC_URL.
#           Spends real ETH from $BETH_LIVE_DEST1. The keystore is
#           expected at /beth-live-home/keystore (mounted read-only by
#           run.sh --enso-live) and is COPIED into a throwaway home
#           before the daemon starts so the canonical keystore is
#           never written to.
#
# How to run (host side)
#   set -a && source test.env && set +a
#   bash tests/docker/run.sh --enso         # fork
#   bash tests/docker/run.sh --enso-live    # mainnet, spends real ETH
#
# Required env (fork mode — set by docker-compose-enso.yml)
#   BETH_ENSO_KEY              Enso v1 API key
#   BETH_TEST_WALLET_PASSPHRASE   passphrase for the imported test wallet
#   BASE_FORK_INTERNAL_URL     RPC URL the daemon hits (anvil-fork:8545)
#
# Required env (live mode — set by run.sh --enso-live)
#   BETH_TEST_MODE=live        selects this branch
#   BETH_ENSO_KEY              Enso v1 API key
#   BETH_PASSPHRASE            passphrase for the live keystore
#   BETH_LIVE_DEST1            sender address (must exist as `dest1`
#                              under /beth-live-home/keystore)
#   BETH_BASE_RPC_URL          real Base RPC the daemon broadcasts to
#   BETH_SWAP_AMOUNT_ETH       optional, defaults to 0.001
#
# Idempotency
#   Fork mode wipes the home dir per run and the anvil fork is fresh
#   each `docker compose up`. Live mode wipes only the throwaway
#   /tmp/beth-enso-home; the canonical $BETH_LIVE_HOME on the host is
#   read-only-mounted and never modified.

set -euo pipefail

MODE="${BETH_TEST_MODE:-fork}"

MNT=/eth
HOME_DIR=/tmp/beth-enso-home
PIDFILE=/tmp/mount_demo.pid
LOGFILE=/tmp/mount_demo.log
SENTINEL=/.beth-mounted

WALLET=dest1
CHAIN=base
USDC=0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913
AUSDC=0x4e65fE4DbA92790696d040ac24Aa414708F5c0AB

GREEN=$'\033[32m' YELLOW=$'\033[33m' RED=$'\033[31m' RESET=$'\033[0m'
log()  { printf '%s[enso-test]%s %s\n' "$GREEN" "$RESET" "$*" >&2; }
warn() { printf '%s[enso-test]%s %s\n' "$YELLOW" "$RESET" "$*" >&2; }
fail() { printf '%s[enso-test]%s %s\n' "$RED"   "$RESET" "$*" >&2; exit 1; }

# ---------- mode-specific config ----------
case "$MODE" in
    fork)
        # Anvil's deterministic account[0]. On a fork the on-chain
        # balance is whatever real Base has at the fork block, so we
        # top it up via anvil_setBalance below.
        ANVIL_KEY=0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80
        DEST1=0xf39Fd6e51aad88F6F4ce6aB8827279cfFFb92266
        SWAP_AMOUNT_ETH=0.05
        WALLET_PASSPHRASE="${BETH_TEST_WALLET_PASSPHRASE:-}"
        IMPORT_KEY="$ANVIL_KEY"
        [[ -n "${BETH_ENSO_KEY:-}" ]] || fail "BETH_ENSO_KEY not set"
        [[ -n "$WALLET_PASSPHRASE" ]] || fail "BETH_TEST_WALLET_PASSPHRASE not set"
        [[ -n "${BASE_FORK_INTERNAL_URL:-}" ]] \
            || fail "BASE_FORK_INTERNAL_URL not set"
        RPC_URL="$BASE_FORK_INTERNAL_URL"
        CHAIN_DISPLAY="Base (forked)"
        BLOCK_MAINNET_BROADCAST=false
        ;;
    live)
        DEST1="${BETH_LIVE_DEST1:-}"
        SWAP_AMOUNT_ETH="${BETH_SWAP_AMOUNT_ETH:-0.001}"
        WALLET_PASSPHRASE="${BETH_PASSPHRASE:-}"
        # No key import in live mode — the keystore is the source of
        # truth and was created by `beth wallet create` long ago.
        IMPORT_KEY=
        [[ -n "${BETH_ENSO_KEY:-}" ]] || fail "BETH_ENSO_KEY not set"
        [[ -n "$DEST1" ]]             || fail "BETH_LIVE_DEST1 not set"
        [[ -n "$WALLET_PASSPHRASE" ]] || fail "BETH_PASSPHRASE not set"
        [[ -n "${BETH_BASE_RPC_URL:-}" ]] \
            || fail "BETH_BASE_RPC_URL not set"
        [[ -d /beth-live-home/keystore ]] \
            || fail "/beth-live-home/keystore missing (mount via run.sh --enso-live)"
        RPC_URL="$BETH_BASE_RPC_URL"
        CHAIN_DISPLAY="Base (mainnet)"
        # block_mainnet_broadcast guards against unexpected broadcasts
        # to a chain id that matches a known mainnet. Live mode wants
        # to broadcast to Base mainnet, so we leave the guard off.
        BLOCK_MAINNET_BROADCAST=false
        warn "LIVE MODE: broadcasting to Base mainnet from $DEST1"
        warn "           swap = $SWAP_AMOUNT_ETH ETH (real funds)"
        ;;
    *)
        fail "unknown BETH_TEST_MODE='$MODE' (expected fork|live)"
        ;;
esac

mkdir -p "$MNT" "$HOME_DIR"
rm -rf "$HOME_DIR"/*

# Live mode: copy the canonical keystore into the throwaway home so
# the daemon can unlock it without touching the read-only mount. The
# daemon writes outbox state, prices cache, etc. into HOME_DIR; only
# the keystore needs to come from the host.
if [[ "$MODE" == "live" ]]; then
    log "copying live keystore -> $HOME_DIR/keystore (in-container, throwaway)"
    cp -r /beth-live-home/keystore "$HOME_DIR/keystore"
    if [[ ! -d "$HOME_DIR/keystore/$WALLET" ]]; then
        fail "no '$WALLET' entry under /beth-live-home/keystore"
    fi
fi

# ---------- write the daemon config ----------
log "writing config.toml (rpc: $RPC_URL)"
cat > "$HOME_DIR/config.toml" <<EOF
stage_ttl = "30m"
block_mainnet_broadcast = $BLOCK_MAINNET_BROADCAST
default_chain = "base"

[chains.base]
name = "base"
chain_id = 8453
rpc_urls = ["$RPC_URL"]
allow_broadcast = true
display_name = "$CHAIN_DISPLAY"
native_symbol = "ETH"
native_decimals = 18
legacy_tx = false

[enso]
api_key = "$BETH_ENSO_KEY"
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
# Anvil's account[0] starts with 10k ETH on a brand-new anvil, but a
# *fork* respects upstream state — the address may have nothing on
# real Base. Use anvil_setBalance to guarantee 10 ETH for the run.
# Live mode skips this — there is no anvil and the wallet is expected
# to already hold enough ETH for the swap plus gas.
if [[ "$MODE" == "fork" ]]; then
    log "anvil_setBalance $DEST1 := 10 ETH"
    curl -fsS -X POST -H 'content-type: application/json' \
        --data '{"jsonrpc":"2.0","id":1,"method":"anvil_setBalance","params":["'"$DEST1"'","0x8AC7230489E80000"]}' \
        "$BASE_FORK_INTERNAL_URL" \
        | sed -n 's/.*"result":\([^,}]*\).*/  result=\1/p' >&2 \
        || fail "anvil_setBalance failed (fork RPC unreachable?)"
fi

# ---------- spawn the mount daemon ----------
log "spawning mount_demo (mount=$MNT home=$HOME_DIR)"
# BETH_TEST_WALLET_KEY is only set in fork mode where mount_demo
# imports an Anvil-derived key under the name "dest1". In live mode the
# keystore was copied in above, so we leave the import key empty —
# mount_demo will skip the import branch and just unlock the existing
# entry with BETH_TEST_WALLET_PASSPHRASE.
BETH_TEST_WALLET_NAME="$WALLET" \
BETH_TEST_WALLET_KEY="$IMPORT_KEY" \
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

# Wait up to 90s for the .beth-mounted sentinel; cargo on a cold cache
# can need a moment to finish tracing init even after build is done.
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
HEAD=$(cat "$MNT/chains/$CHAIN/head/number" | tr -d '\n')
log "chain head: block $HEAD"
echo '::endgroup::' >&2

echo '::group::wallet eth balance' >&2
BAL_ETH=$(cat "$MNT/chains/$CHAIN/addresses/$DEST1/balance.eth" | tr -d '\n')
log "$WALLET ($DEST1) ETH balance: $BAL_ETH"
echo '::endgroup::' >&2

# Sanity: the test only makes sense if dest1 has ETH to spend. In fork
# mode anvil_setBalance guarantees 10 ETH; in live mode the wallet
# must already hold > swap+gas. We use awk for the comparison because
# `[[ ... -lt ... ]]` is integer-only and the balance is decimal.
[[ -n "$BAL_ETH" && "$BAL_ETH" != "0" ]] || fail "$WALLET ETH balance is 0"
if ! awk -v b="$BAL_ETH" -v s="$SWAP_AMOUNT_ETH" 'BEGIN { exit !(b+0 > s+0) }'; then
    fail "$WALLET ETH balance ($BAL_ETH) is not greater than swap amount ($SWAP_AMOUNT_ETH); top up before retrying"
fi

# ---------- post the intent through the mount ----------
INTENT_BODY=$(printf '{"intent":"swap %s ETH to %s on base","chain":"%s"}' \
    "$SWAP_AMOUNT_ETH" "$AUSDC" "$CHAIN")
log "POST intent (via /eth write): $INTENT_BODY"

# Snapshot the pending set so we can diff it after confirmation and
# learn the staged id. This used to be impossible — `BethFs::getattr`
# returned a stable `change` attribute, so once the kernel cached the
# empty listing it never refreshed. Now `dir_change` hashes the actual
# listing, so a daemon-side write moves the change attribute and the
# kernel re-issues READDIR.
PENDING_BEFORE=$(ls "$MNT/wallets/$WALLET/chains/$CHAIN/outbox/pending" 2>/dev/null \
    | sort -u | tr '\n' '|' || true)

printf '%s' "$INTENT_BODY" > "$MNT/defi/intents/$WALLET/new"

# Pull the new session id (the only entry under defi/intents/<w> that
# isn't `new`).
SESS=$(ls "$MNT/defi/intents/$WALLET" | grep -v '^new$' | sort | tail -n1 || true)
[[ -n "$SESS" ]] || fail "no defi session created under $MNT/defi/intents/$WALLET"
log "session: $SESS"

echo '::group::session plan.md' >&2
cat "$MNT/defi/intents/$WALLET/$SESS/plan.md" >&2 || true
echo '::endgroup::' >&2

# Confirm the session — that stages a tx into the wallet outbox.
# The confirm write returns once the daemon has accepted it, but the
# tx engine still has to estimate gas (which can take tens of seconds
# against an upstream RPC) before the stage appears under outbox/pending.
# Poll instead of snapshotting once.
log "confirm defi session"
echo y > "$MNT/defi/intents/$WALLET/$SESS/confirm"

STAGE=
# Budget is generous because gas estimation walks the Enso route
# through Aave/USDC/Uniswap, all of which lazy-fetch state from the
# fork's upstream RPC. A cold fork against a slow public endpoint can
# easily push this past 90s.
log "waiting for new outbox stage (300s budget)"
for i in $(seq 1 300); do
    PENDING_AFTER=$(ls "$MNT/wallets/$WALLET/chains/$CHAIN/outbox/pending" 2>/dev/null \
        | sort -u | tr '\n' '|' || true)
    # `|| true`: grep exits 1 when there's no new stage yet, which
    # combined with `set -euo pipefail` would kill the script on the
    # first poll. We *want* to keep polling.
    STAGE=$(comm -13 \
        <(printf '%s' "$PENDING_BEFORE" | tr '|' '\n' | sort -u) \
        <(printf '%s' "$PENDING_AFTER"  | tr '|' '\n' | sort -u) \
        | grep -v '^$' | head -n1 || true)
    if [[ -n "$STAGE" ]]; then
        log "  stage appeared after ${i}s"
        break
    fi
    if ! kill -0 "$DAEMON_PID" 2>/dev/null; then
        fail "mount_demo died while waiting for stage"
    fi
    sleep 1
done
[[ -n "$STAGE" ]] || fail "no new outbox stage produced within 300s"
log "stage: $STAGE"

echo '::group::stage plan.md' >&2
cat "$MNT/wallets/$WALLET/chains/$CHAIN/outbox/pending/$STAGE/plan.md" >&2 || true
echo '::endgroup::' >&2

# Broadcast through the mount. The keystore was unlocked at startup
# inside the daemon process, so the write is allowed.
log "broadcast via outbox confirm"
echo y > "$MNT/wallets/$WALLET/chains/$CHAIN/outbox/pending/$STAGE/confirm"

# After the write returns, the stage moves to sent/<id>/tx_hash. The
# NFS write is synchronous wrt our in-process VFS, so we can read
# immediately.
HASH=$(cat "$MNT/wallets/$WALLET/chains/$CHAIN/outbox/sent/$STAGE/tx_hash" \
    | tr -d '\n' || true)
[[ -n "$HASH" ]] || fail "tx_hash missing after broadcast"
log "tx hash: $HASH"

# ---------- poll for receipt ----------
log "polling /chains/$CHAIN/tx/$HASH/status (60s budget)"
STATUS=
for i in $(seq 1 60); do
    STATUS=$(cat "$MNT/chains/$CHAIN/tx/$HASH/status" 2>/dev/null | tr -d '\n' || true)
    case "$STATUS" in
        success)
            log "  status=success after ${i}s"
            break
            ;;
        reverted)
            fail "tx reverted on-chain (hash=$HASH)"
            ;;
        *)
            sleep 1
            ;;
    esac
done
[[ "$STATUS" == "success" ]] || fail "tx did not confirm within 60s (last status='$STATUS')"

# ---------- verify all receipt VFS paths are populated ----------
# `status` already proved the receipt is fetched. Now exercise every
# path the chains handler exposes under chains/<c>/tx/<hash>/ so we
# know an agent can pull the full receipt picture from the mount, not
# just the tx_hash + status.
TX_DIR="$MNT/chains/$CHAIN/tx/$HASH"
echo '::group::tx receipt paths' >&2

BLOCK_NUMBER=$(cat "$TX_DIR/block_number" 2>/dev/null | tr -d '\n' || true)
[[ -n "$BLOCK_NUMBER" && "$BLOCK_NUMBER" =~ ^[0-9]+$ ]] \
    || fail "block_number empty or non-numeric ('$BLOCK_NUMBER') at $TX_DIR/block_number"
log "  block_number: $BLOCK_NUMBER"

GAS_USED=$(cat "$TX_DIR/gas_used" 2>/dev/null | tr -d '\n' || true)
[[ -n "$GAS_USED" && "$GAS_USED" =~ ^[0-9]+$ ]] \
    || fail "gas_used empty or non-numeric ('$GAS_USED') at $TX_DIR/gas_used"
log "  gas_used: $GAS_USED"

# receipt.json / logs.json / full.json: confirm each is non-empty and
# starts with the expected JSON sentinel. The container ships without
# jq/python so we keep validation to bash builtins — a `{` for the
# object payloads, `[` for the logs array. That's enough to catch a
# regression where the path returns empty bytes or an error string.
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
echo '::endgroup::' >&2

# ---------- assert aBaseUSDC balance ----------
AUSDC_RAW=$(cat "$MNT/chains/$CHAIN/addresses/$DEST1/tokens/$AUSDC/balance.raw" \
    | tr -d '\n' || true)
log "aBaseUSDC raw balance after supply: $AUSDC_RAW"
[[ -n "$AUSDC_RAW" && "$AUSDC_RAW" != "0" ]] \
    || fail "aBaseUSDC balance is 0 after a successful Enso route"

log "===== Enso -> Aave integration test PASSED ====="
exit 0
