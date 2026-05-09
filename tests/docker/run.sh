#!/usr/bin/env bash
# Host-side driver: build the test image and run an in-container
# integration suite.
#
# Usage:
#   ./tests/docker/run.sh [--rebuild] [--workspace|--mount|--enso|--enso-live]
#
# Modes:
#   default       — runs tests/docker/test.sh (NFS mount integration test).
#                   Container runs with SYS_ADMIN + apparmor=unconfined +
#                   /dev/fuse so mount.nfs4 can do its thing.
#   --workspace   — runs tests/docker/test_workspace.sh (cargo test
#                   --workspace --lib). Skips the privileged flags
#                   because the workspace unit tests don't mount.
#   --enso        — runs tests/docker/test_enso_aave.sh inside a
#                   docker-compose stack with an anvil --fork-url=Base
#                   sidecar. Drives the Enso -> Aave intent flow end
#                   to end through the NFS mount at /eth/. Requires
#                   BETH_ENSO_KEY in the environment.
#   --enso-live   — same Enso -> Aave flow but against Base mainnet,
#                   broadcasting from the live keystore at $BETH_LIVE_HOME
#                   under the wallet $BETH_LIVE_DEST1. SPENDS REAL ETH on
#                   every run (default 0.001 ETH; override with
#                   BETH_SWAP_AMOUNT_ETH). The live keystore is mounted
#                   read-only and copied into a throwaway home inside
#                   the container — the canonical keystore is never
#                   written to from this script.
#
# `--rebuild` forces `docker build --no-cache`. The default reuses the
# cached image so iterative loops stay fast.
set -euo pipefail

REPO_ROOT=$(cd "$(dirname "$0")/../.." && pwd)
IMAGE_TAG=bloom-eth-mount-test:latest

REBUILD=0
MODE=mount
for arg in "$@"; do
    case "$arg" in
        --rebuild) REBUILD=1 ;;
        --workspace) MODE=workspace ;;
        --mount) MODE=mount ;;
        --enso) MODE=enso ;;
        --enso-live) MODE=enso-live ;;
        -h|--help)
            cat <<EOF
Usage: $0 [--rebuild] [--workspace|--mount|--enso|--enso-live]

Default mode runs the NFS mount integration test.
--workspace runs \`cargo test --workspace --lib\` inside the same image.
--enso runs the Enso -> Aave integration test against an anvil fork.
--enso-live runs the same flow against Base mainnet (spends real ETH).
--rebuild forces \`docker build --no-cache\`.
EOF
            exit 0
            ;;
        *) echo "unknown arg: $arg" >&2; exit 2 ;;
    esac
done

echo "::group::docker build"
if [ "$REBUILD" -eq 1 ]; then
    docker build --no-cache \
        -t "$IMAGE_TAG" \
        -f "$REPO_ROOT/tests/docker/Dockerfile" \
        "$REPO_ROOT"
else
    docker build \
        -t "$IMAGE_TAG" \
        -f "$REPO_ROOT/tests/docker/Dockerfile" \
        "$REPO_ROOT"
fi
echo "::endgroup::"

run_args=(
    --rm
    -v "$REPO_ROOT":/workspace
    -w /workspace
)

case "$MODE" in
    mount)
        # --cap-add SYS_ADMIN          — allows mount() inside the container
        # --device /dev/fuse           — only needed if we ever switch to FUSE,
        #                                 but harmless and matches bloom's run
        # --security-opt apparmor=unconfined
        #                              — Debian/Ubuntu hosts ship an apparmor
        #                                 profile that blocks mount() even with
        #                                 SYS_ADMIN; unconfined gets us past it
        run_args+=(
            --cap-add SYS_ADMIN
            --device /dev/fuse
            --security-opt apparmor=unconfined
        )
        cmd=(bash tests/docker/test.sh)
        ;;
    workspace)
        # Workspace unit tests don't need any of the mount privileges.
        cmd=(bash tests/docker/test_workspace.sh)
        ;;
    enso)
        # Compose-driven: anvil-fork sidecar + beth-test driver. The
        # compose file pins the same image we just built and threads
        # SYS_ADMIN / apparmor flags via cap_add+security_opt.
        if [[ -z "${BETH_ENSO_KEY:-}" ]]; then
            echo "BETH_ENSO_KEY not set; required for --enso." >&2
            echo "  hint: source test.env or pass it inline." >&2
            exit 2
        fi
        if command -v docker-compose >/dev/null 2>&1; then
            COMPOSE=(docker-compose)
        else
            COMPOSE=(docker compose)
        fi
        echo "::group::docker compose up ($MODE)"
        export REPO_ROOT
        export BETH_ENSO_KEY
        export BETH_TEST_IMAGE="$IMAGE_TAG"
        # Optional override: when public Base RPC rate-limits, point
        # at a private endpoint via BASE_FORK_RPC_URL.
        export BASE_FORK_RPC_URL="${BASE_FORK_RPC_URL:-https://base-rpc.publicnode.com}"
        compose_file="$REPO_ROOT/tests/docker/docker-compose-enso.yml"
        # Tear down any previous run before bringing the stack up so a
        # stale anvil fork can't leak state into a new test.
        "${COMPOSE[@]}" -f "$compose_file" down --remove-orphans >/dev/null 2>&1 || true
        # `up --abort-on-container-exit` returns the driver's exit code.
        rc=0
        "${COMPOSE[@]}" -f "$compose_file" up \
            --abort-on-container-exit --exit-code-from beth-test \
            || rc=$?
        "${COMPOSE[@]}" -f "$compose_file" down --remove-orphans >/dev/null 2>&1 || true
        echo "::endgroup::"
        exit "$rc"
        ;;
    enso-live)
        # No anvil sidecar: the daemon points at a real Base RPC and
        # the broadcast lands on Base mainnet. Single privileged
        # `docker run` so the in-container kernel can mount NFS.
        for v in BETH_ENSO_KEY BETH_LIVE_HOME BETH_LIVE_DEST1 BETH_PASSPHRASE; do
            if [[ -z "${!v:-}" ]]; then
                echo "$v not set; required for --enso-live." >&2
                echo "  hint: \`set -a && source test.env && set +a\` first." >&2
                exit 2
            fi
        done
        if [[ ! -d "$BETH_LIVE_HOME/keystore" ]]; then
            echo "BETH_LIVE_HOME=$BETH_LIVE_HOME has no keystore/ subdir." >&2
            echo "  the live wallet must already exist before this test runs." >&2
            exit 2
        fi
        SWAP_AMOUNT_ETH="${BETH_SWAP_AMOUNT_ETH:-0.001}"
        BASE_RPC_URL="${BETH_BASE_RPC_URL:-https://base-rpc.publicnode.com}"
        echo "::group::docker run (enso-live)"
        echo "  wallet: $BETH_LIVE_DEST1" >&2
        echo "  swap:   $SWAP_AMOUNT_ETH ETH (override via BETH_SWAP_AMOUNT_ETH)" >&2
        echo "  rpc:    $BASE_RPC_URL" >&2
        echo "  NOTE:   this broadcasts to Base mainnet and spends real ETH." >&2
        # The live keystore is mounted read-only; the test script
        # copies it into a throwaway home inside the container so an
        # in-container daemon write can't corrupt the canonical copy.
        docker run --rm \
            --cap-add SYS_ADMIN \
            --device /dev/fuse \
            --security-opt apparmor=unconfined \
            --security-opt seccomp=unconfined \
            -v "$REPO_ROOT":/workspace \
            -v "$BETH_LIVE_HOME":/beth-live-home:ro \
            -e BETH_TEST_MODE=live \
            -e BETH_ENSO_KEY \
            -e BETH_PASSPHRASE \
            -e BETH_LIVE_DEST1 \
            -e BETH_BASE_RPC_URL="$BASE_RPC_URL" \
            -e BETH_SWAP_AMOUNT_ETH="$SWAP_AMOUNT_ETH" \
            -e RUST_LOG="${RUST_LOG:-info}" \
            -w /workspace \
            "$IMAGE_TAG" \
            bash tests/docker/test_enso_aave.sh
        rc=$?
        echo "::endgroup::"
        exit "$rc"
        ;;
    *)
        echo "internal error: unknown mode $MODE" >&2
        exit 2
        ;;
esac

echo "::group::docker run ($MODE)"
docker run "${run_args[@]}" "$IMAGE_TAG" "${cmd[@]}"
echo "::endgroup::"
