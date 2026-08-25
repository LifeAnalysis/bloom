#!/usr/bin/env python3
"""Validate immutable built-in Petal release pins and activation eligibility."""

from __future__ import annotations

import argparse
import json
import pathlib
import re
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parents[3]
SOURCE = ROOT / "crates/bloom/src/github_source.rs"
CONFIG = ROOT / "crates/bloom-proto/src/config.rs"
FULL = re.compile(r"^[0-9a-f]{40}$")
SHA = re.compile(r"^[0-9a-f]{64}$")


def fail(message: str) -> None:
    raise SystemExit(message)


def parse_catalog() -> list[dict[str, str]]:
    source = SOURCE.read_text()
    constants = dict(re.findall(r'^const (\w+): &str = "([^"]+)";', source, re.M))
    records = []
    for block in re.findall(r"const PREINSTALLED_[A-Z_]+: PreinstalledPetal = PreinstalledPetal \{(.*?)\n\};", source, re.S):
        fields = dict(re.findall(r"^\s*(\w+): (?:Some\()?\"([^\"]+)\"\)?,$", block, re.M))
        for key, value in re.findall(r"^\s*(\w+): (\w+),$", block, re.M):
            if value in constants:
                fields[key] = constants[value]
        eligible = re.search(r"default_eligible: (true|false)", block)
        if eligible:
            fields["default_eligible"] = eligible.group(1)
        records.append(fields)
    if not records:
        fail("built-in Petal catalog could not be parsed")
    return records


def get_json(url: str) -> dict:
    request = urllib.request.Request(url, headers={"User-Agent": "bloom-petal-audit"})
    with urllib.request.urlopen(request, timeout=30) as response:
        return json.load(response)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--remote", action="store_true")
    args = parser.parse_args()
    records = parse_catalog()
    config_block = CONFIG.read_text().split("fn default_preinstalled_petals()", 1)[1].split("\n}", 1)[0]
    defaults = set(re.findall(r'"([a-z0-9-]+)"\.to_string\(\)', config_block))

    for entry in records:
        name = entry.get("name", "<missing>")
        for field in ("repository", "commit", "release_tag", "archive", "expected_hash", "archive_sha256", "tooling_commit", "petal_abi", "default_eligible"):
            if field not in entry:
                fail(f"{name}: catalog is missing {field}")
        if not FULL.fullmatch(entry["commit"]) or not FULL.fullmatch(entry["tooling_commit"]):
            fail(f"{name}: source and tooling revisions must be full commits")
        if not SHA.fullmatch(entry["expected_hash"]) or not SHA.fullmatch(entry["archive_sha256"]):
            fail(f"{name}: package and archive hashes must be SHA-256")
        if name in defaults and entry["default_eligible"] != "true":
            fail(f"{name}: incompatible catalog artifact is default-activated")
        if entry["default_eligible"] == "true" and "triad-compatible" not in entry["petal_abi"]:
            fail(f"{name}: default-eligible artifact lacks a triad-compatible ABI declaration")
        if not args.remote:
            continue
        owner_repo = entry["repository"].removeprefix("https://github.com/")
        base = f"https://github.com/{owner_repo}/releases/download/{entry['release_tag']}"
        manifest = get_json(f"{base}/petal-release.json")
        expected = {
            "petal_name": name,
            "source_repository": owner_repo,
            "source_commit": entry["commit"],
            "release_tag": entry["release_tag"],
            "archive": entry["archive"],
            "archive_sha256": entry["archive_sha256"],
            "package_hash": entry["expected_hash"],
            "tooling_repository": "bloom-directory/petal",
            "tooling_commit": entry["tooling_commit"],
        }
        if manifest.get("schema") != "bloom.petal.release.v1" or any(manifest.get(k) != v for k, v in expected.items()):
            fail(f"{name}: remote release manifest does not match the catalog")
        get_json(f"https://api.github.com/repos/{owner_repo}/commits/{entry['commit']}")
        get_json(f"https://api.github.com/repos/bloom-directory/petal/commits/{entry['tooling_commit']}")
    print("default Petal release catalog is immutable and activation-compatible")


if __name__ == "__main__":
    main()
