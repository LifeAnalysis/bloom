#!/usr/bin/env python3
"""Fail closed if a retained v0.1 Petal signing route stops being a rejection."""

from pathlib import Path
import sys


workspace = Path(__file__).resolve().parents[3]
vm = (workspace / "crates/bloom-petals/src/vm.rs").read_text()
host = (workspace / "crates/bloom-petals/src/host.rs").read_text()
runner = (workspace / "crates/bloom-petals/src/runner.rs").read_text()
daemon = (workspace / "crates/bloom-daemon/src/lib.rs").read_text()

# Tests deliberately mention these names. Only inspect the production prefix.
vm = vm.split("\n#[cfg(test)]\nmod tests", 1)[0]
host = host.split("\n#[cfg(test)]\nmod tests", 1)[0]


def require(condition: bool, message: str) -> None:
    if not condition:
        print(f"legacy hash-only route check failed: {message}", file=sys.stderr)
        raise SystemExit(1)


require(
    'linker.instance("bloom:sign/signing@0.1.0")?' in vm,
    "the retained v0.1 component instance is missing",
)
for route, handler, next_handler in [
    ("sign-hash", "component_sign_hash", "component_sign_payload"),
    ("sign-hashes", "component_sign_hashes", "component_petal_key_request"),
]:
    require(
        f'func_new_async("{route}"' in vm and f"{handler}(store, params, results)" in vm,
        f"{route} is not wired to its reviewed rejection handler",
    )
    start = vm.index(f"async fn {handler}(")
    end = vm.index(f"async fn {next_handler}(", start)
    body = vm[start:end]
    require(
        body.count("legacy_signing_unsupported()") == 1
        and "store.data().host" not in body,
        f"{route} is no longer an unconditional UNSUPPORTED_VERSION rejection",
    )

for method, next_method in [
    ("sign_hash", "sign_hash_outcome"),
    ("sign_hash_outcome", "sign_hashes_outcome"),
    ("sign_hashes_outcome", "sign_payload_outcome"),
]:
    start = host.index(f"async fn {method}(")
    end = host.index(f"async fn {next_method}(", start)
    body = host[start:end]
    require(
        "HostError::UnsupportedVersion" in body and "@0.1.0" in body,
        f"PetalHost::{method} is no longer fail-closed",
    )

for path, source in [("runner.rs", runner), ("bloom-daemon/src/lib.rs", daemon)]:
    for method in ("sign_hash", "sign_hash_outcome", "sign_hashes_outcome"):
        require(
            f"async fn {method}(" not in source,
            f"{path} overrides the fail-closed PetalHost::{method} default",
        )

print("legacy v0.1 Petal signing routes are fail-closed")
