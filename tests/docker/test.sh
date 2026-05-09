#!/usr/bin/env bash
# In-container driver for the bloom-eth NFS mount integration test.
#
# Runs inside the Dockerfile next to this script. Steps:
#   1. Build the `mount_demo` example (the only thing that pulls in
#      embednfs); skip a full workspace build to keep the test loop
#      tight.
#   2. Spawn the example pointing at /mnt/beth with a fresh home dir.
#   3. Wait for the .beth-mounted sentinel the example drops.
#   4. Exercise a few VFS paths through the kernel mount.
#   5. SIGTERM the example so it unmounts cleanly, then exit.
#
# The script is intentionally chatty: when something goes wrong inside
# the container the host-side `run.sh` only sees the exit code, so we
# print breadcrumbs that show up in `docker run` stdout.
set -euo pipefail

MNT=/mnt/beth
HOME_DIR=/tmp/beth-home
PIDFILE=/tmp/mount_demo.pid
LOGFILE=/tmp/mount_demo.log
SENTINEL=/mnt/.beth-mounted

mkdir -p "$MNT" "$HOME_DIR"

echo "::group::cargo build --features mount --example mount_demo"
cargo build \
    --release \
    --package beth-daemon \
    --features mount \
    --example mount_demo
echo "::endgroup::"

EXAMPLE_BIN=target/release/examples/mount_demo
if [ ! -x "$EXAMPLE_BIN" ]; then
    # `--release` may have been overridden by a workspace profile;
    # fall back to debug.
    EXAMPLE_BIN=target/debug/examples/mount_demo
fi
if [ ! -x "$EXAMPLE_BIN" ]; then
    echo "could not find mount_demo binary" >&2
    exit 1
fi

echo "::group::spawning daemon"
RUST_LOG=info "$EXAMPLE_BIN" "$MNT" "$HOME_DIR" >"$LOGFILE" 2>&1 &
echo $! > "$PIDFILE"
PID=$(cat "$PIDFILE")
echo "mount_demo pid=$PID, logging to $LOGFILE"
echo "::endgroup::"

cleanup() {
    if [ -f "$PIDFILE" ]; then
        local pid
        pid=$(cat "$PIDFILE")
        if kill -0 "$pid" 2>/dev/null; then
            echo "sending SIGTERM to $pid"
            kill -TERM "$pid" || true
            for _ in 1 2 3 4 5 6 7 8 9 10; do
                if ! kill -0 "$pid" 2>/dev/null; then
                    break
                fi
                sleep 1
            done
            if kill -0 "$pid" 2>/dev/null; then
                echo "force killing $pid"
                kill -KILL "$pid" || true
            fi
        fi
    fi
    # Best-effort umount in case the daemon's Drop didn't fire.
    umount "$MNT" 2>/dev/null || true
    echo "::group::mount_demo log"
    cat "$LOGFILE" || true
    echo "::endgroup::"
}
trap cleanup EXIT

# Wait up to 60s for the sentinel — the daemon writes it after the
# kernel mount completes.
echo "waiting for $SENTINEL"
for i in $(seq 1 60); do
    if [ -f "$SENTINEL" ]; then
        echo "  sentinel found after ${i}s"
        break
    fi
    if ! kill -0 "$PID" 2>/dev/null; then
        echo "mount_demo exited prematurely:" >&2
        cat "$LOGFILE" >&2 || true
        exit 1
    fi
    sleep 1
done
if [ ! -f "$SENTINEL" ]; then
    echo "timed out waiting for mount sentinel" >&2
    cat "$LOGFILE" >&2 || true
    exit 1
fi

# ---- exercise the VFS through the NFS mount ------------------------
echo "::group::ls $MNT"
ls -la "$MNT"
echo "::endgroup::"

fail=0

echo "::group::cat $MNT/status/version"
if ! cat "$MNT/status/version"; then
    echo "FAIL: status/version unreadable" >&2
    fail=1
fi
echo "::endgroup::"

echo "::group::ls $MNT/chains"
if ! ls "$MNT/chains"; then
    echo "FAIL: chains/ unlistable" >&2
    fail=1
fi
echo "::endgroup::"

echo "::group::cat $MNT/tools/keccak/abc"
if ! cat "$MNT/tools/keccak/abc"; then
    echo "FAIL: tools/keccak/abc unreadable" >&2
    fail=1
fi
echo "::endgroup::"

if [ "$fail" -ne 0 ]; then
    echo "one or more VFS reads failed" >&2
    exit 1
fi

echo "all VFS reads succeeded"
exit 0
