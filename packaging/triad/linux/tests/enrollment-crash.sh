#!/usr/bin/env bash
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd -P)"
# This is deliberately a plain userland container. The test injects SIGKILL
# into the installer process; it does not boot systemd or need host privileges.
image="${BLOOM_LINUX_TEST_IMAGE:-debian:13-slim}"
binary="${BLOOM_LINUX_TEST_BINARY:-$repo/target/debug/bloom}"
[[ -x "$binary" ]] || {
  echo "build bloom or set BLOOM_LINUX_TEST_BINARY before running the crash test" >&2
  exit 66
}

for boundary in journal-temp prepared published activated committed; do
  docker run --rm \
    --mount "type=bind,src=$repo,dst=/source,readonly" \
    --mount "type=bind,src=$binary,dst=/tested-bloom,readonly" \
    --env "BOUNDARY=$boundary" \
    --entrypoint /bin/bash \
    "$image" /source/packaging/triad/linux/tests/enrollment-crash-inner.sh
done

docker run --rm \
  --mount "type=bind,src=$repo,dst=/source,readonly" \
  --mount "type=bind,src=$binary,dst=/tested-bloom,readonly" \
  --entrypoint /bin/bash \
  "$image" /source/packaging/triad/linux/tests/fresh-install-inner.sh
