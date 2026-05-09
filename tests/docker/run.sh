#!/usr/bin/env bash
# Host-side driver: build the test image and run an in-container
# integration suite.
#
# Usage:
#   ./tests/docker/run.sh [--rebuild] [--workspace]
#
# Modes:
#   default       — runs tests/docker/test.sh (NFS mount integration test).
#                   Container runs with SYS_ADMIN + apparmor=unconfined +
#                   /dev/fuse so mount.nfs4 can do its thing.
#   --workspace   — runs tests/docker/test_workspace.sh (cargo test
#                   --workspace --lib). Skips the privileged flags
#                   because the workspace unit tests don't mount.
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
        -h|--help)
            cat <<EOF
Usage: $0 [--rebuild] [--workspace|--mount]

Default mode runs the NFS mount integration test.
--workspace runs \`cargo test --workspace --lib\` inside the same image.
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
    *)
        echo "internal error: unknown mode $MODE" >&2
        exit 2
        ;;
esac

echo "::group::docker run ($MODE)"
docker run "${run_args[@]}" "$IMAGE_TAG" "${cmd[@]}"
echo "::endgroup::"
