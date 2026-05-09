# Dockerfile for the bloom-eth NFS mount integration test.
#
# Mirrors the bloom approach: rust:bookworm + nfs-common provides the
# mount.nfs4 binary so the in-process embednfs server can be attached
# inside the container. We use the bookworm tag to match the kernel
# headers shipped by Docker Desktop for Mac and most Linux hosts.
FROM rust:bookworm

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        nfs-common \
        ca-certificates \
        netbase \
        procps \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /workspace

# The host driver bind-mounts the repo at /workspace and runs
# `tests/docker/test.sh` inside. The container is otherwise stateless.
CMD ["bash", "tests/docker/test.sh"]
