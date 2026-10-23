#!/usr/bin/env python3
"""Self-test for check_duckdb_version.py: each pin that can disagree with
Cargo.lock is made to disagree in a scratch tree, and the check must exit 1
naming that file and both versions. Stdlib only.

    python3 -I scripts/check_duckdb_version_selftest.py
"""

import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
CHECK = HERE / "check_duckdb_version.py"

LOCK = '[[package]]\nname = "libduckdb-sys"\nversion = "{}"\nsource = "registry+x"\n'
WORKFLOW = "env:\n  # a comment\n  DUCKDB_VERSION: {}\n"
INSTALL = '#!/bin/sh\nDUCKDB_RECOMMENDED="{}"\n'
VETTED = '{{"extensions": [{{"name": "h3", "duckdb_versions": ["{}"]}}]}}\n'

failures = []


def check(name, ok, detail=""):
    print(("ok    " if ok else "FAIL  ") + name)
    if not ok:
        failures.append(name)
        if detail:
            print("      " + detail.replace("\n", "\n      "))


def run(lock="1.20000.0", ci="v2.0.0", parity="v2.0.0", install="v2.0.0", vetted="v2.0.0"):
    with tempfile.TemporaryDirectory() as d:
        d = Path(d)
        (d / "Cargo.lock").write_text(LOCK.format(lock))
        (d / "ci.yml").write_text(WORKFLOW.format(ci))
        (d / "parity.yml").write_text(WORKFLOW.format(parity))
        (d / "install.sh").write_text(INSTALL.format(install))
        (d / "vetted.json").write_text(VETTED.format(vetted))
        p = subprocess.run(
            [
                sys.executable, "-I", str(CHECK),
                "--lock", str(d / "Cargo.lock"),
                "--workflow", str(d / "ci.yml"),
                "--workflow", str(d / "parity.yml"),
                "--install", str(d / "install.sh"),
                "--vetted", str(d / "vetted.json"),
            ],
            capture_output=True, text=True,
        )
        return p.returncode, p.stdout + p.stderr


def expect_fail(name, file, held, **kw):
    code, out = run(**kw)
    check(name, code == 1 and file in out and held in out and "v2.0.0" in out, f"exit {code}\n{out}")


def main():
    if not CHECK.exists():
        print(f"FAIL  {CHECK.name} is missing")
        return 1

    # The decoding follows duckdb-rs build_linked.rs: major = n / 10000,
    # minor = n / 100 % 100, patch = n % 100.
    for crate, want in [("1.20000.0", "v2.0.0"), ("1.10506.0", "v1.5.6"), ("1.20103.2", "v2.1.3")]:
        code, out = run(lock=crate, ci=want, parity=want, install=want, vetted=want)
        check(f"crate {crate} decodes to {want}", code == 0, out)

    code, out = run()
    check("agreeing files exit 0", code == 0, out)
    expect_fail("a workflow on v2.0.1 fails, naming ci.yml", "ci.yml", "v2.0.1", ci="v2.0.1")
    expect_fail("the parity workflow disagreeing fails", "parity.yml", "v1.5.6", parity="v1.5.6")
    expect_fail("install.sh disagreeing fails", "install.sh", "v1.5.6", install="v1.5.6")
    expect_fail("vetted_extensions.json disagreeing fails", "vetted.json", "v1.5.6", vetted="v1.5.6")

    print(f"\n{len(failures)} failed" if failures else "\nall passed")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
