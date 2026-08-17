#!/usr/bin/env python3
"""Small fail-closed scanner used by the Machine authority release gate.

The macOS Tart lane reads sources over VirtioFS, where mmap-based search tools
can receive SIGBUS. This scanner uses ordinary buffered reads and reports I/O
or decoding failures as fatal errors rather than treating them as no match.
"""

from __future__ import annotations

import argparse
import os
import re
import sys
from pathlib import Path


def read_bytes(path: Path) -> bytes:
    try:
        return path.read_bytes()
    except OSError as error:
        raise RuntimeError(f"cannot read {path}: {error}") from error


def count(args: argparse.Namespace) -> int:
    path = Path(args.path)
    print(read_bytes(path).count(args.marker.encode()))
    return 0


def files(args: argparse.Namespace) -> int:
    marker = args.marker.encode()
    suffixes = tuple(args.suffix)
    names = set(args.name)
    matches: list[str] = []
    for root_text in args.roots:
        root = Path(root_text)
        if not root.is_dir():
            raise RuntimeError(f"source scan root is not a directory: {root}")
        for directory, _, filenames in os.walk(root, followlinks=False):
            for filename in filenames:
                if filename not in names and not filename.endswith(suffixes):
                    continue
                path = Path(directory, filename)
                if marker in read_bytes(path):
                    matches.append(str(path))
    for match in sorted(matches):
        print(match)
    return 0


def regex(args: argparse.Namespace) -> int:
    flags = re.IGNORECASE if args.ignore_case else 0
    patterns = [re.compile(pattern, flags) for pattern in args.pattern]
    path = Path(args.path)
    try:
        text = read_bytes(path).decode("utf-8")
    except UnicodeDecodeError as error:
        raise RuntimeError(f"cannot decode {path} as UTF-8: {error}") from error
    matched = False
    for line_number, line in enumerate(text.splitlines(), 1):
        if any(pattern.search(line) for pattern in patterns):
            print(f"{path}:{line_number}:{line}")
            matched = True
    return 0 if matched else 1


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser()
    commands = root.add_subparsers(dest="command", required=True)

    count_parser = commands.add_parser("count")
    count_parser.add_argument("--marker", required=True)
    count_parser.add_argument("path")
    count_parser.set_defaults(run=count)

    files_parser = commands.add_parser("files")
    files_parser.add_argument("--marker", required=True)
    files_parser.add_argument("--suffix", action="append", default=[])
    files_parser.add_argument("--name", action="append", default=[])
    files_parser.add_argument("roots", nargs="+")
    files_parser.set_defaults(run=files)

    regex_parser = commands.add_parser("regex")
    regex_parser.add_argument("--ignore-case", action="store_true")
    regex_parser.add_argument("--pattern", action="append", required=True)
    regex_parser.add_argument("path")
    regex_parser.set_defaults(run=regex)
    return root


def main() -> int:
    args = parser().parse_args()
    try:
        return args.run(args)
    except (RuntimeError, re.error) as error:
        print(f"machine authority scan failed: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
