#!/usr/bin/env bash

# Shared helpers for host-side scripts. Callers provide their own
# log/fail functions so messages keep script-specific prefixes.

ANVIL_KEY_0=0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80
ANVIL_KEY_1=0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d
ANVIL_KEY_2=0x5de4111afa1a4b94908f83103eb1f1706367c2e68ca870fc3fb9a804cdab365a

ANVIL_ADDR_0=0xf39Fd6e51aad88F6F4ce6aB8827279cfFFb92266
ANVIL_ADDR_1=0x70997970C51812dc3A010C7d01b50e0d17dc79C8

require_cmd() {
    local c
    for c in "$@"; do
        if ! command -v "$c" >/dev/null 2>&1; then
            if [[ -n "${REQUIRE_CMD_EXIT:-}" ]]; then
                printf 'missing required command: %s\n' "$c" >&2
                exit "$REQUIRE_CMD_EXIT"
            fi
            fail "missing required command: $c"
        fi
    done
}

detect_docker_compose() {
    if docker compose version >/dev/null 2>&1; then
        DC=(docker compose)
    elif command -v docker-compose >/dev/null 2>&1; then
        DC=(docker-compose)
    else
        fail "neither 'docker compose' nor 'docker-compose' is available"
    fi
}

wait_eth_rpc() {
    local url=$1 attempts=${2:-30} delay=${3:-1}
    local ready=0

    for _ in $(seq 1 "$attempts"); do
        if curl -fs -X POST "$url" \
            -H 'content-type: application/json' \
            -d '{"jsonrpc":"2.0","id":1,"method":"eth_chainId","params":[]}' \
            >/dev/null 2>&1; then
            ready=1
            break
        fi
        sleep "$delay"
    done
    [[ "$ready" -eq 1 ]]
}
