# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""Cover the finetype version gate in describe.py.

describe.py shells out to whatever `finetype` is on PATH to type columns. A
stale binary types columns wrong but still emits valid JSON, so the step asserts
a minimum finetype version and fails closed below it (see the module docstring
and `_require_finetype`). This test pins that guard: a current-enough finetype
passes; a stale / missing / broken / unparseable one stops the run.

Stdlib-only (unittest), matching describe.py's `dependencies = []` posture — no
real finetype is needed. Each case drops a fake `finetype` executable onto a
temp PATH so the gate is exercised through the same `subprocess` call the
operator uses in production. Run it directly:

    python3 operators/datapackage_describe/test_describe.py
    # or:  uv run operators/datapackage_describe/test_describe.py
"""
from __future__ import annotations

import importlib.util
import os
import stat
import tempfile
import unittest
from pathlib import Path

# Import the operator script by path — it is a uv-run script, not an installed
# module, and its side-effecting work lives under `if __name__ == "__main__"`,
# so importing only pulls in the definitions we want to test.
_DESCRIBE_PATH = Path(__file__).with_name("describe.py")
_spec = importlib.util.spec_from_file_location("describe_under_test", _DESCRIBE_PATH)
describe = importlib.util.module_from_spec(_spec)
assert _spec and _spec.loader
_spec.loader.exec_module(describe)


def _fake_finetype(dir_path: Path, *, version_line: str | None, exit_code: int = 0) -> None:
    """Write an executable `finetype` into dir_path that mimics `--version`.

    version_line=None simulates a binary that prints nothing (unparseable);
    exit_code!=0 simulates `finetype --version` itself failing.
    """
    body = ["#!/bin/sh"]
    if version_line is not None:
        body.append(f'echo "{version_line}"')
    if exit_code:
        body.append(f"exit {exit_code}")
    script = dir_path / "finetype"
    script.write_text("\n".join(body) + "\n")
    script.chmod(script.stat().st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH)


class VersionGateTest(unittest.TestCase):
    def setUp(self) -> None:
        self._orig_path = os.environ.get("PATH", "")
        self._tmp = tempfile.TemporaryDirectory()
        self.bindir = Path(self._tmp.name)
        # Isolate PATH so ONLY the fake we install can be resolved — a real
        # finetype on the developer's PATH must not leak into these cases.
        os.environ["PATH"] = str(self.bindir)

    def tearDown(self) -> None:
        os.environ["PATH"] = self._orig_path
        self._tmp.cleanup()

    # --- version parsing -------------------------------------------------
    def test_parse_version_extracts_first_dotted_triple(self) -> None:
        self.assertEqual(describe._parse_version("finetype 0.6.53"), (0, 6, 53))
        self.assertEqual(describe._parse_version("v12.0.1-rc1"), (12, 0, 1))

    def test_parse_version_empty_when_absent(self) -> None:
        self.assertEqual(describe._parse_version("no version here"), ())

    # --- the gate passes for a current-enough finetype -------------------
    def test_current_version_passes(self) -> None:
        _fake_finetype(self.bindir, version_line="finetype 0.6.53")
        # At or above the floor must NOT raise.
        describe._require_finetype("0.6.52")

    def test_exact_floor_passes(self) -> None:
        _fake_finetype(self.bindir, version_line="finetype 0.6.52")
        describe._require_finetype("0.6.52")

    # --- the gate fails closed below the floor ---------------------------
    def test_stale_version_fails_closed(self) -> None:
        # 0.6.41 is the real stale-engine version that shipped wrong labels.
        _fake_finetype(self.bindir, version_line="finetype 0.6.41")
        with self.assertRaises(SystemExit) as ctx:
            describe._require_finetype("0.6.52")
        msg = str(ctx.exception)
        self.assertIn("0.6.41", msg)
        self.assertIn("older", msg)

    def test_missing_finetype_fails_closed(self) -> None:
        # Empty bindir → nothing named `finetype` resolves on PATH.
        with self.assertRaises(SystemExit) as ctx:
            describe._require_finetype("0.6.52")
        self.assertIn("not on PATH", str(ctx.exception))

    def test_version_command_failure_fails_closed(self) -> None:
        _fake_finetype(self.bindir, version_line="broken", exit_code=3)
        with self.assertRaises(SystemExit) as ctx:
            describe._require_finetype("0.6.52")
        self.assertIn("failed", str(ctx.exception))

    def test_unparseable_version_fails_closed(self) -> None:
        _fake_finetype(self.bindir, version_line="finetype (unknown build)")
        with self.assertRaises(SystemExit) as ctx:
            describe._require_finetype("0.6.52")
        self.assertIn("could not parse", str(ctx.exception))


if __name__ == "__main__":
    unittest.main()
