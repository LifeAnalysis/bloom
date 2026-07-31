#!/usr/bin/env bash
# Deterministic mounted-filesystem double for the local integration runner.
set -euo pipefail

args=" $* "
[[ "$args" == *" serve "* ]] || {
  printf 'fake Bloom only supports serve; the runner must use the mount\n' >&2
  exit 1
}

socket=""
mount_dir=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --endpoint)
      socket="${2#unix:}"
      shift 2
      ;;
    --mount)
      mount_dir="$2"
      shift 2
      ;;
    *)
      shift
      ;;
  esac
done
[ -n "$socket" ] && [ -n "$mount_dir" ]

exec python3 - "$socket" "$mount_dir" <<'PY'
import json
import os
import signal
import socket
import sys
import threading
import time
from pathlib import Path

socket_path = Path(sys.argv[1])
mount = Path(sys.argv[2])
state = Path(os.environ.get("BLOOM_FAKE_STATE", "/tmp/bloom-fake-state"))
wallet = "test-passkey"
address = "0x0000000000000000000000000000000000000001"
session = "manual-mainnet-integration"


def write(relative, body):
    path = mount / relative
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(body)
    return path


def write_json(relative, value):
    return write(relative, json.dumps(value) + "\n")


def fifo(relative, callback):
    path = mount / relative
    path.parent.mkdir(parents=True, exist_ok=True)
    try:
        path.unlink()
    except FileNotFoundError:
        pass
    os.mkfifo(path)

    def consume():
        while True:
            try:
                with path.open() as stream:
                    payload = stream.read()
                callback(payload)
            except (FileNotFoundError, OSError):
                return

    threading.Thread(target=consume, daemon=True).start()
    return path


write(f"wallets/{wallet}/kind", "passkey\n")
write(f"wallets/{wallet}/address", address + "\n")
write_json("hyperliquid/mainnet/perp_meta.json", {"universe": [{"name": "BTC"}]})
write_json(
    f"hyperliquid/mainnet/users/{address}/clearinghouse.json",
    {
        "marginSummary": {"accountValue": "20"},
        "withdrawable": "20",
        "assetPositions": [],
    },
)
write_json(
    "petals/polymarket/meta/route-contract.json",
    {"schema": "fake.route-contract.v1", "abi": "0.1"},
)
write_json(
    f"petals/polymarket/onboard/{wallet}/status.json",
    {"stage": "complete", "tradeable": True},
)
write_json(
    f"petals/polymarket/account/{wallet}/status.json",
    {"status": "ready", "tradeable": True},
)
write_json(
    f"petals/polymarket/account/{wallet}/buying_power.json",
    {
        "spendable": {"asset": "pUSD", "raw": "20000000"},
        "can_trade_now": True,
        "funding_needed": False,
    },
)
write_json(
    "petals/polymarket/markets/fixture/market.json",
    {"slug": "fixture", "question": "Fixture?", "outcomes": ["Yes", "No"]},
)
write_json(
    "petals/polymarket/markets/fixture/prices.json",
    {"Yes": "0.5", "No": "0.5"},
)
(mount / f"petals/polymarket/trade/{wallet}/drafts").mkdir(parents=True)
(mount / f"petals/polymarket/trade/{wallet}/receipts").mkdir(parents=True)
state.mkdir(parents=True, exist_ok=True)

hl_new_count = 0


def handle_hl_new(payload):
    global hl_new_count
    request = json.loads(payload)
    agent_name = request["agent_name"]
    if not 1 <= len(agent_name) <= 16:
        write_json(
            f"hyperliquid/mainnet/agent_sessions/{wallet}/last_error.json",
            {"error": "Extra agent name must be between 1 and 16 characters long."},
        )
        return
    sid = request["id"]
    root = f"hyperliquid/mainnet/agent_sessions/{wallet}/{sid}"
    if hl_new_count == 0:
        write_json(
            f"{root}/approval_challenge.json",
            {
                "action_id": "hl-fixture",
                "ceremony_url": "http://127.0.0.1:18734/fixture",
                "expires_at_ms": 9999999999999,
            },
        )

        def handle_order(_payload):
            state.joinpath("hl-order").touch()
            write_json(
                f"{root}/last_response.json",
                {
                    "response": {
                        "status": "ok",
                        "response": {
                            "type": "order",
                            "data": {"statuses": [{"filled": {"oid": 1}}]},
                        },
                    }
                },
            )

        fifo(f"{root}/order.json", handle_order)
        fifo(f"{root}/cancel_all", lambda _payload: None)
        fifo(f"{root}/stop", lambda _payload: None)
    else:
        write_json(f"{root}/session.json", {"status": "active", "max_session_secs": 300})
    hl_new_count += 1


fifo(f"hyperliquid/mainnet/agent_sessions/{wallet}/new.json", handle_hl_new)


def handle_pm_new(_payload):
    root = f"petals/polymarket/trade/{wallet}/drafts/draft-1"
    Path(mount / root).mkdir(parents=True, exist_ok=True)
    write(f"{root}/plan.md", "# Fixture Polymarket plan\n")
    write_json(f"{root}/policy_check.json", {"policy_deny": False, "policy_status": "pass"})
    write_json(
        f"{root}/quote.json",
        {"status": "revalidated", "amount": "1", "limit_price": "0.5"},
    )
    write_json(
        f"{root}/review_intent.json",
        {"status": "final_review_staged", "posting_enabled": True},
    )
    fifo(f"{root}/revalidate", lambda _payload: None)
    post_count = 0

    def handle_post(_post_payload):
        nonlocal post_count
        if post_count == 0:
            write_json(
                f"{root}/approval.json",
                {
                    "action_id": "pm-fixture",
                    "ceremony_url": "http://127.0.0.1:18734/fixture",
                    "expires_ms": 9999999999999,
                },
            )
        else:
            state.joinpath("pm-posted").touch()
            write_json(
                f"petals/polymarket/trade/{wallet}/receipts/draft-1/receipt.json",
                {
                    "draft_id": "draft-1",
                    "clob_status": "matched",
                    "filled_size_micro": 1000000,
                },
            )
        post_count += 1

    fifo(f"{root}/post", handle_post)


fifo(f"petals/polymarket/trade/{wallet}/new", handle_pm_new)

time.sleep(float(os.environ.get("BLOOM_FAKE_STARTUP_DELAY_SECS", "0")))
try:
    socket_path.unlink()
except FileNotFoundError:
    pass
listener = socket.socket(socket.AF_UNIX)
listener.bind(str(socket_path))
listener.listen(1)
signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
while True:
    time.sleep(1)
PY
