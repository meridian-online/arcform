# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""Cover the finetype version gate AND the version stamp in describe.py.

describe.py shells out to whatever `finetype` is on PATH to type columns. A
stale binary types columns wrong but still emits valid JSON, so the step asserts
a minimum finetype version and fails closed below it (see the module docstring
and `_require_finetype`). This test pins that guard: a current-enough finetype
passes; a stale / missing / broken / unparseable one stops the run.

The cases read `describe.MIN_FINETYPE_VERSION` rather than restating a literal,
so they follow the constant instead of quietly testing a floor the operator no
longer uses. One case does name versions outright — the superseded releases in
`SUPERSEDED_RELEASES`, each of which mistypes columns the published datasets
depend on. Those must stay rejected whatever the constant says, so each is
asserted directly *and* the constant is asserted to sit above it.

`StampTest` and `ExpectVersionTest` cover the newer half: the step writes
`x-finetype-version` into every descriptor it emits, and that value must be the
ACTUAL resolved binary, not a constant and not a second, independent lookup.
`StampTest` runs describe.py's real `main()` end to end — fake `finetype` on
PATH, real files on a temp dir, real JSON read back off disk — against two
DIFFERENT resolved versions, so a hardcoded stamp (or one read from
`MIN_FINETYPE_VERSION` rather than the binary that ran) fails on at least one
of them, not just goes untested.

Stdlib-only (unittest), matching describe.py's `dependencies = []` posture — no
real finetype is needed. Each case drops a fake `finetype` executable onto a
temp PATH so the gate is exercised through the same `subprocess` call the
operator uses in production. Run it directly:

    python3 operators/datapackage_describe/test_describe.py
    # or:  uv run operators/datapackage_describe/test_describe.py
"""
from __future__ import annotations

import importlib.util
import json
import os
import shutil
import stat
import sys
import tempfile
import unittest
from pathlib import Path

# The releases the floor exists to keep out, each with the defect that put it
# there. Named as literals on purpose: these are facts about those binaries, not
# about the constant, so they must stay rejected however the constant is edited.
SUPERSEDED_RELEASES = {
    # Wrong labels for the ticker, industry-code level and resolved legal-name
    # columns; the website no longer suppresses them at display time.
    "0.6.52": "wrong ticker / industry-level / legal-name labels",
    # Both the year-first and day-first compact date leaves validated on
    # `^\d{8}$`, so any eight-digit token — a financial figure, a surrogate key —
    # came back a confident date WITH a `strptime` transform attached. A consumer
    # that follows the transform gets a corrupted column, not just a wrong label.
    "0.6.53": "eight-digit figures typed as confident dates, with a transform",
}

# Import the operator script by path — it is a uv-run script, not an installed
# module, and its side-effecting work lives under `if __name__ == "__main__"`,
# so importing only pulls in the definitions we want to test.
_DESCRIBE_PATH = Path(__file__).with_name("describe.py")
_spec = importlib.util.spec_from_file_location("describe_under_test", _DESCRIBE_PATH)
assert _spec is not None and _spec.loader is not None
describe = importlib.util.module_from_spec(_spec)
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


def _one_patch_above(version: str) -> str:
    """The next patch release above `version` — a binary newer than the floor."""
    major, minor, patch = (int(part) for part in version.split(".")[:3])
    return f"{major}.{minor}.{patch + 1}"


def _fake_finetype_full(dir_path: Path, *, version: str, profile_json: dict) -> None:
    """Write an executable `finetype` that answers BOTH subcommands `main()` calls.

    `finetype --version` prints `version`; `finetype profile -f … -o datapackage`
    (any other argv) prints `profile_json` as its stdout. Used by `StampTest`, which
    drives describe.py's real `main()` end to end, so the fake has to stand in for
    finetype at both call sites, not just the one `_require_finetype` makes.

    `#!/bin/sh` and shell BUILTINS only (`echo`, `if`/`test`) — these tests isolate
    PATH down to just the fake's own directory (so a real `finetype` cannot leak
    in), which also means no *external* command (`env`, `cat`, …) can be resolved
    from inside the fake: `/bin/sh` itself is fine because a `#!` shebang is an
    absolute path, not a PATH lookup, but anything the script then shells out to
    is not.
    """
    script = dir_path / "finetype"
    profile_text = json.dumps(profile_json)
    quoted_profile = "'" + profile_text.replace("'", "'\\''") + "'"
    body = (
        "#!/bin/sh\n"
        'if [ "$1" = "--version" ]; then\n'
        f'  echo "finetype {version}"\n'
        "else\n"
        f"  echo {quoted_profile}\n"
        "fi\n"
    )
    script.write_text(body)
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
        newer = _one_patch_above(describe.MIN_FINETYPE_VERSION)
        _fake_finetype(self.bindir, version_line=f"finetype {newer}")
        # Above the floor must NOT raise, and returns the RESOLVED version (not
        # the floor it was checked against) — this is the value `main()` stamps.
        self.assertEqual(describe._require_finetype(describe.MIN_FINETYPE_VERSION), newer)

    def test_exact_floor_passes(self) -> None:
        _fake_finetype(
            self.bindir, version_line=f"finetype {describe.MIN_FINETYPE_VERSION}"
        )
        self.assertEqual(
            describe._require_finetype(describe.MIN_FINETYPE_VERSION),
            describe.MIN_FINETYPE_VERSION,
        )

    # --- the gate fails closed below the floor ---------------------------
    def test_superseded_releases_fail_closed(self) -> None:
        """The operator's OWN floor must reject every superseded release.

        Every other case would still pass if the constant slipped back a release,
        because they read the constant. This one names the binaries that must
        stay out and checks the constant sits above each — so dropping the floor
        to re-admit one of them fails here rather than downstream.
        """
        for release, defect in SUPERSEDED_RELEASES.items():
            with self.subTest(release=release, defect=defect):
                self.assertGreater(
                    describe._parse_version(describe.MIN_FINETYPE_VERSION),
                    describe._parse_version(release),
                    f"the floor must sit above {release} — {defect}",
                )
                _fake_finetype(self.bindir, version_line=f"finetype {release}")
                with self.assertRaises(SystemExit) as ctx:
                    describe._require_finetype(describe.MIN_FINETYPE_VERSION)
                msg = str(ctx.exception)
                self.assertIn(release, msg)
                self.assertIn("older", msg)

    def test_stale_version_fails_closed(self) -> None:
        # 0.6.41 is the real stale-engine version that shipped wrong labels.
        _fake_finetype(self.bindir, version_line="finetype 0.6.41")
        with self.assertRaises(SystemExit) as ctx:
            describe._require_finetype(describe.MIN_FINETYPE_VERSION)
        msg = str(ctx.exception)
        self.assertIn("0.6.41", msg)
        self.assertIn("older", msg)

    def test_missing_finetype_fails_closed(self) -> None:
        # Empty bindir → nothing named `finetype` resolves on PATH.
        with self.assertRaises(SystemExit) as ctx:
            describe._require_finetype(describe.MIN_FINETYPE_VERSION)
        self.assertIn("not on PATH", str(ctx.exception))

    def test_version_command_failure_fails_closed(self) -> None:
        _fake_finetype(self.bindir, version_line="broken", exit_code=3)
        with self.assertRaises(SystemExit) as ctx:
            describe._require_finetype(describe.MIN_FINETYPE_VERSION)
        self.assertIn("failed", str(ctx.exception))

    def test_unparseable_version_fails_closed(self) -> None:
        _fake_finetype(self.bindir, version_line="finetype (unknown build)")
        with self.assertRaises(SystemExit) as ctx:
            describe._require_finetype(describe.MIN_FINETYPE_VERSION)
        self.assertIn("could not parse", str(ctx.exception))


class ExpectVersionTest(unittest.TestCase):
    """`_require_exact_version` — the `--expect-finetype-version` pin (AC2).

    Unlike the floor, a pin is an EXACT match: a newer release must still be
    refused if it isn't the one asked for, because "newer is fine" is not always
    true for a caller pinning a run. No subprocess here — `_require_exact_version`
    takes the already-resolved version as a plain argument.
    """

    def test_matching_version_does_not_raise(self) -> None:
        describe._require_exact_version("0.6.60", "0.6.60")

    def test_newer_than_expected_still_refused(self) -> None:
        # A pin is exact, not a floor — 0.6.61 does not satisfy "expect 0.6.60"
        # even though it would satisfy "min 0.6.60".
        with self.assertRaises(SystemExit) as ctx:
            describe._require_exact_version("0.6.61", "0.6.60")
        msg = str(ctx.exception)
        self.assertIn("0.6.61", msg)
        self.assertIn("0.6.60", msg)

    def test_mismatch_names_both_versions(self) -> None:
        with self.assertRaises(SystemExit) as ctx:
            describe._require_exact_version("0.6.60", "0.6.99")
        msg = str(ctx.exception)
        self.assertIn("0.6.60", msg)
        self.assertIn("0.6.99", msg)


class StampTest(unittest.TestCase):
    """`x-finetype-version` (AC1) and `--expect-finetype-version` (AC2), driven
    through describe.py's real `main()` — a fake `finetype` on a temp PATH, real
    files on a temp dir, the WRITTEN descriptor read back off disk. Nothing here
    calls describe.py's internals directly, so a stamp sourced from anywhere other
    than the binary `main()` actually resolved and ran shows up as a wrong value
    in the file, exactly as it would in production.
    """

    def setUp(self) -> None:
        self._orig_path = os.environ.get("PATH", "")
        self._orig_argv = sys.argv
        self._tmp = tempfile.TemporaryDirectory()
        self.bindir = Path(self._tmp.name)
        os.environ["PATH"] = str(self.bindir)
        self.work = Path(tempfile.mkdtemp())
        self.parquet = self.work / "d.parquet"
        self.parquet.write_bytes(b"")  # never opened by the fake finetype
        self.overrides = self.work / "descriptor.overrides.json"
        self.overrides.write_text("{}")
        self.out = self.work / "datapackage.json"

    def tearDown(self) -> None:
        os.environ["PATH"] = self._orig_path
        sys.argv = self._orig_argv
        self._tmp.cleanup()
        shutil.rmtree(self.work, ignore_errors=True)

    def _run_main(self, *, version: str, expect: str | None = None) -> dict:
        """Run describe.py's real `main()` against a fake finetype reporting
        `version`, then return the descriptor it wrote."""
        _fake_finetype_full(
            self.bindir,
            version=version,
            profile_json={"resources": [{"schema": {"fields": []}}]},
        )
        argv = [
            "describe.py",
            "--parquet",
            str(self.parquet),
            "--overrides",
            str(self.overrides),
            "--out",
            str(self.out),
        ]
        if expect is not None:
            argv += ["--expect-finetype-version", expect]
        sys.argv = argv
        describe.main()
        with self.out.open(encoding="utf-8") as fh:
            return json.load(fh)

    def test_stamp_matches_the_resolved_binary(self) -> None:
        # Two DIFFERENT resolved versions, both above the floor. A hardcoded
        # string, or a value read from MIN_FINETYPE_VERSION instead of the
        # binary that ran, would stamp the SAME value both times — this fails on
        # whichever one does not match the constant.
        newer = _one_patch_above(describe.MIN_FINETYPE_VERSION)
        later = _one_patch_above(newer)
        for version in (newer, later):
            with self.subTest(version=version):
                descriptor = self._run_main(version=version)
                self.assertEqual(descriptor["x-finetype-version"], version)

    def test_override_sidecar_cannot_clobber_the_stamp(self) -> None:
        # A sidecar that (accidentally or not) sets x-finetype-version must not
        # win — the stamp is machine-derived, applied after the override merge.
        self.overrides.write_text(json.dumps({"x-finetype-version": "9.9.9"}))
        newer = _one_patch_above(describe.MIN_FINETYPE_VERSION)
        descriptor = self._run_main(version=newer)
        self.assertEqual(descriptor["x-finetype-version"], newer)

    def test_expect_finetype_version_matching_runs_and_stamps(self) -> None:
        newer = _one_patch_above(describe.MIN_FINETYPE_VERSION)
        descriptor = self._run_main(version=newer, expect=newer)
        self.assertEqual(descriptor["x-finetype-version"], newer)

    def test_expect_finetype_version_mismatch_refuses_before_writing(self) -> None:
        newer = _one_patch_above(describe.MIN_FINETYPE_VERSION)
        later = _one_patch_above(newer)
        with self.assertRaises(SystemExit) as ctx:
            self._run_main(version=newer, expect=later)
        msg = str(ctx.exception)
        self.assertIn(newer, msg)
        self.assertIn(later, msg)
        # Refused BEFORE finetype ever profiled the Parquet, so nothing is written.
        self.assertFalse(self.out.exists())


if __name__ == "__main__":
    unittest.main()
