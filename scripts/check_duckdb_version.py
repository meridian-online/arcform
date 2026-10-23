#!/usr/bin/env python3
"""Every pinned DuckDB version must be the one Cargo.lock links.

arc loads extensions into the linked libduckdb in-process, and an extension
loads only into the version it was built for. So CI's DuckDB, the version
install.sh recommends must be the DuckDB that `libduckdb-sys` in Cargo.lock
links, and each entry in vetted_extensions.json must list that version (it may
list others too, as an extension can be vetted on two patches at once).

libduckdb-sys encodes DuckDB's version in its own: 1.20000.0 is v2.0.0. The
decoding follows duckdb-rs build_linked.rs, major = n / 10000,
minor = n / 100 % 100, patch = n % 100. The crate's third number is
duckdb-rs's own patch, not DuckDB's.

    python3 -I scripts/check_duckdb_version.py [--lock Cargo.lock]
        [--workflow PATH ...] [--install scripts/install.sh]
        [--vetted src/vetted_extensions.json]

    exit 0  every pin agrees
    exit 1  a pin disagrees; each is named with the version it holds
    exit 2  a file could not be read or holds no version
"""

import argparse
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
WORKFLOWS = [".github/workflows/ci.yml", ".github/workflows/text-embed-parity.yml"]


class Unreadable(Exception):
    pass


def linked_version(lock_text):
    m = re.search(r'name = "libduckdb-sys"\s*\nversion = "(\d+)\.(\d+)\.(\d+)"', lock_text)
    if not m:
        raise Unreadable("no libduckdb-sys version")
    n = int(m.group(2))
    return f"v{n // 10000}.{n // 100 % 100}.{n % 100}"


def pin(path, pattern):
    m = re.search(pattern, path.read_text(), re.M)
    if not m:
        raise Unreadable(f"{path}: no DuckDB version pin")
    return m.group(1)


def main(argv):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--lock", default=str(ROOT / "Cargo.lock"))
    ap.add_argument("--workflow", action="append")
    ap.add_argument("--install", default=str(ROOT / "scripts/install.sh"))
    ap.add_argument("--vetted", default=str(ROOT / "src/vetted_extensions.json"))
    args = ap.parse_args(argv)
    workflows = args.workflow or [str(ROOT / w) for w in WORKFLOWS]

    try:
        want = linked_version(Path(args.lock).read_text())
        held = [
            (w, pin(Path(w), r"^\s*DUCKDB_VERSION:\s*(v[\d.]+)\s*$")) for w in workflows
        ]
        held.append((args.install, pin(Path(args.install), r'^DUCKDB_RECOMMENDED="(v[\d.]+)"')))
        vetted = json.loads(Path(args.vetted).read_text())
        lacking = [
            f"{args.vetted}: extension {e['name']} lists {e['duckdb_versions']}"
            for e in vetted["extensions"]
            if want not in e["duckdb_versions"]
        ]
    except (OSError, ValueError, KeyError, Unreadable) as e:
        print(f"check_duckdb_version: {e}", file=sys.stderr)
        return 2

    bad = [f"{f} holds {v}" for f, v in held if v != want] + lacking
    if bad:
        print(f"{args.lock} links DuckDB {want}, but:", file=sys.stderr)
        for line in bad:
            print(f"  {line}", file=sys.stderr)
        return 1
    print(f"every DuckDB pin is {want}, as {args.lock} links")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
