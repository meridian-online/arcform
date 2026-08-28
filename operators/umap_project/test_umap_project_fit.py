# /// script
# requires-python = ">=3.12,<3.13"
# dependencies = [
#   "umap-learn>=0.5,<0.6",
#   "numpy>=1.26,<3",
#   "duckdb>=1,<2",
# ]
# ///
"""End to end for `--fit`: the persisted projection, run for real.

WHY THIS FILE EXISTS SEPARATELY FROM `test_umap_project.py`. That file is stdlib-only
and covers the half of the operator that DECIDES — which rows a fit already holds, which
mismatches are refused. It cannot cover the half that MATTERS, which is that a base row's
coordinates come back byte for byte unchanged after an append, because that needs UMAP.
A decidable-half test is green whether or not `.transform()` was ever called and whether
or not the stored coordinates were used, so on its own it would pin the bookkeeping and
not the claim.

ONE PROCESS. `main()` is called with a patched `sys.argv` rather than spawned per case,
so numba JIT-compiles UMAP once instead of once per invocation — the difference between
a couple of minutes and a quarter of an hour on a cold runner. Only two invocations fit
anything; the damaged-fit sweeps refuse before UMAP is reached, so they cost a DuckDB
scan each. No count is written here because the sweeps size themselves from the keys the
fit on disk actually carries, and a number in this docstring is one nothing reddens.

ONE OF THEM IS A SUBPROCESS, deliberately. Calling `main()` directly cannot see the
`__main__` guard, so an operator that raises a perfectly good `Refusal` and then prints
a traceback of it is green everywhere else in this file. What a caller reads is a stream
and an exit code.

Run: `uv run operators/umap_project/test_umap_project_fit.py`. Needs `uv`; CI runs it in
the `build` job, which installs `uv` for the operator suite anyway.
"""
from __future__ import annotations

import importlib.util
import math
import pickle
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

import duckdb

HERE = Path(__file__).resolve().parent


def _load():
    spec = importlib.util.spec_from_file_location(
        "umap_project_under_test", HERE / "umap_project.py"
    )
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


up = _load()

COLUMNS = ["lon", "lat", "income", "rooms"]
BASE_ROWS = 60
APPENDED_ROWS = 6


def _rows(n: int):
    out = []
    for i in range(n):
        near = i % 2
        a, b = math.sin(i * 1.7) * 0.4, math.cos(i * 2.3) * 0.4
        out.append(
            (
                f"row-{i:04d}",
                (10.0 if near else 40.0) + a,
                (10.0 if near else 40.0) + b,
                (100.0 if near else 400.0) + a * 3,
                (1.0 if near else 4.0) + b,
            )
        )
    return out


def _appended():
    # Names that sort INTO the middle of the base table, not onto the end. A SQL step
    # with `ORDER BY name` is the ordinary producer of this operator's input, so an
    # appended row lands wherever its name falls — and a rule that assumed appends come
    # last would call most of the table new and move coordinates this feature exists to
    # hold still.
    return [
        (f"row-{7 * j:04d}a", 20.0 + j * 0.1, 20.0 + j * 0.1, 200.0 + j, 2.0 + j * 0.05)
        for j in range(APPENDED_ROWS)
    ]


def _write(path: Path, rows) -> None:
    con = duckdb.connect()
    con.execute("SET threads TO 1")
    con.execute("SET preserve_insertion_order TO true")
    con.execute("CREATE TABLE t (name VARCHAR, lon DOUBLE, lat DOUBLE, income DOUBLE, rooms DOUBLE)")
    con.executemany("INSERT INTO t VALUES (?,?,?,?,?)", rows)
    con.execute(f"COPY (SELECT * FROM t ORDER BY name) TO '{path.as_posix()}' (FORMAT parquet)")


def _project(**kwargs) -> None:
    """One invocation of the operator, in this process. Raises `up.Refusal` exactly as
    the script's own `__main__` guard would report it."""
    argv = ["umap_project.py"]
    for column in kwargs.pop("columns", COLUMNS):
        argv += ["--column", column]
    for flag, value in kwargs.items():
        argv += [f"--{flag.replace('_', '-')}", str(value)]
    saved, sys.argv = sys.argv, argv
    try:
        up.main()
    finally:
        sys.argv = saved


def _xy(path: Path) -> dict[str, tuple[float, float]]:
    con = duckdb.connect()
    rows = con.execute(
        f"SELECT name, projection_x, projection_y FROM read_parquet('{path.as_posix()}')"
    ).fetchall()
    return {name: (x, y) for name, x, y in rows}


def _fit_ids(path: Path) -> set[str]:
    con = duckdb.connect()
    return {
        r[0]
        for r in con.execute(
            f"SELECT DISTINCT projection_fit_id FROM read_parquet('{path.as_posix()}')"
        ).fetchall()
    }


class ADamagedFitIsRefusedRatherThanCrashingTest(unittest.TestCase):
    """`--fit` names a file the CALLER chose, and the ways it can be wrong are not a list.

    THE SWEEPS BELOW ARE DERIVED FROM THE FIT THE OPERATOR ITSELF JUST WROTE, not from
    cases written out beside them, and that is the whole point of the class. A field added
    to the persisted record later is swept without anyone remembering to add a case, which
    is precisely how the holes this closes got in: the refusal surface covered the header
    fields in `FIT_COMPARED_FIELDS` and the file was touched at seven other points with
    nothing around any of them.

    Every case runs against the UNCHANGED base input on purpose. That is the harder half:
    with no appended rows the operator never calls `.transform()`, so a fit with no
    reducer at all used to run to completion and write plausible coordinates — the failure
    waits for the first append, which is the one run whose entire purpose was to not move
    the map.
    """

    @classmethod
    def setUpClass(cls) -> None:
        cls._tmp = tempfile.TemporaryDirectory()
        d = cls.dir = Path(cls._tmp.name)
        _write(d / "base.parquet", _rows(BASE_ROWS))
        _project(input=d / "base.parquet", out=d / "first.parquet", fit=d / "good.fit")
        cls.good = (d / "good.fit").read_bytes()

    @classmethod
    def tearDownClass(cls) -> None:
        cls._tmp.cleanup()

    def outcome(self, fit: Path) -> str | None:
        """Run the operator against `fit`; return the refusal text, or `None` if it
        completed.

        ANYTHING ELSE PROPAGATES, and that is the assertion rather than a convenience: a
        `KeyError`, an `IndexError` or an `UnpicklingError` reaching here fails the test
        as an error, which is what the README's promise of one named line means when it is
        stated as a check instead of as prose.
        """
        out = self.dir / "damaged-out.parquet"
        before = out.read_bytes() if out.is_file() else None
        try:
            _project(input=self.dir / "base.parquet", out=out, fit=fit)
            return None
        except up.Refusal as refusal:
            after = out.read_bytes() if out.is_file() else None
            self.assertEqual(before, after, "a refused run must not have written an output")
            return str(refusal)

    def written(self, name: str, payload: bytes) -> Path:
        path = self.dir / name
        path.write_bytes(payload)
        return path

    def repickled(self, name: str, mutate) -> Path:
        """The fit the operator really wrote, unpickled, damaged, and written back."""
        record = pickle.loads(self.good)
        mutate(record)
        return self.written(name, pickle.dumps(record, protocol=5))

    def test_a_file_that_is_not_a_pickle_at_all_is_refused_by_name(self) -> None:
        """The four an operator actually produces: a path that never held a fit, an
        interrupted write, and the two flags swapped. Every one of these was an unhandled
        traceback with `pickle` in its first frame."""
        cases = {
            "empty": b"",
            "text": b"this is not a fit\n",
            "random": bytes(range(256)) * 4,
            "parquet": (self.dir / "base.parquet").read_bytes(),
        }
        for name, payload in cases.items():
            with self.subTest(case=name):
                said = self.outcome(self.written(f"not-a-fit-{name}", payload))
                self.assertIsNotNone(said, f"a {name} file was accepted as a fit")
                self.assertIn(f"not-a-fit-{name}", said, "the refusal does not name the file")

    def test_a_fit_truncated_at_any_point_is_refused(self) -> None:
        """An interrupted write leaves a prefix, and a prefix of a pickle is never a
        pickle — it has no STOP. Swept across the file rather than at one offset because
        an interrupted write does not choose where it stops."""
        for tenth in range(10):
            cut = len(self.good) * tenth // 10
            with self.subTest(bytes_kept=cut):
                said = self.outcome(self.written(f"truncated-{tenth}.fit", self.good[:cut]))
                self.assertIsNotNone(said, f"a fit truncated to {cut} bytes was accepted")

    def test_a_byte_flipped_anywhere_in_a_fit_never_escapes_as_a_crash(self) -> None:
        """Corruption in storage or transfer, which does not choose its offset either. The
        assertion is NOT that every flip is refused — a flip inside the stored float data
        yields a fit that is still readable and merely wrong, and refusing it is not
        something this operator can promise. It is that no flip escapes as anything the
        caller cannot read. The count of refusals is asserted to be non-zero so that a
        sweep which happened to exercise nothing cannot pass as one that did."""
        refused = 0
        for step in range(16):
            offset = len(self.good) * step // 16
            flipped = bytearray(self.good)
            flipped[offset] ^= 0xFF
            with self.subTest(offset=offset):
                if self.outcome(self.written(f"flipped-{step}.fit", bytes(flipped))) is not None:
                    refused += 1
        self.assertGreater(refused, 0, "no flip reached the refusal path; the sweep pins nothing")

    def test_deleting_any_key_the_written_fit_carries_is_refused(self) -> None:
        """Swept over the keys of the real artifact, so this covers a field added to the
        record tomorrow. Before the boundary, three of these were a `KeyError`, one an
        `IndexError`, and one ran to completion."""
        for key in sorted(pickle.loads(self.good)):
            with self.subTest(key=key):
                fit = self.repickled(f"no-{key}.fit", lambda r, k=key: r.pop(k))
                said = self.outcome(fit)
                self.assertIsNotNone(said, f"a fit carrying no {key!r} was accepted")

    def test_deleting_any_header_field_the_written_fit_carries_is_refused(self) -> None:
        """The same sweep one level down. What it pins is the statement a caller relies on
        and that neither validator makes alone: no field of the written header can go
        missing without SOMETHING refusing."""
        for key in sorted(pickle.loads(self.good)["header"]):
            with self.subTest(key=key):
                fit = self.repickled(
                    f"no-header-{key}.fit", lambda r, k=key: r["header"].pop(k)
                )
                said = self.outcome(fit)
                self.assertIsNotNone(said, f"a fit whose header has no {key!r} was accepted")

    def test_a_fit_that_cannot_place_a_row_is_refused_on_the_run_before_the_append(
        self,
    ) -> None:
        """The most expensive of them, driven where it actually bites. The input is
        unchanged, so nothing is appended and `.transform()` is never reached — which is
        exactly why this used to succeed. A fit that cannot place a row has to be refused
        by the run that READS it, not by the run that finally needs it."""
        for name, stand_in in (("none", None), ("string", "not a reducer"), ("dict", {})):
            with self.subTest(reducer=name):
                fit = self.repickled(
                    f"bad-reducer-{name}.fit",
                    lambda r, s=stand_in: r.__setitem__("reducer", s),
                )
                said = self.outcome(fit)
                self.assertIsNotNone(said, f"a fit whose reducer is {name} was accepted")
                self.assertIn("reducer", said)

    def test_a_fit_path_that_is_not_a_writable_file_is_refused(self) -> None:
        """The write side of the same class, and the mistake that is easiest to make:
        `--fit build/` meaning the directory. It left an `IsADirectoryError` with the flag
        that caused it nowhere in the message."""
        a_directory = self.dir / "fit-dir"
        a_directory.mkdir(exist_ok=True)
        under_a_file = self.dir / "base.parquet" / "fit.pkl"
        for name, path in (("directory", a_directory), ("under a file", under_a_file)):
            with self.subTest(case=name):
                said = self.outcome(path)
                self.assertIsNotNone(said, f"--fit naming a {name} was accepted")
                self.assertIn("--fit", said)

    def test_the_operator_reports_one_line_and_no_traceback_to_a_caller(self) -> None:
        """The only test here that sees the `__main__` guard, and the reason it is worth a
        subprocess: every case above calls `main()` directly, so all of them are green on
        an operator that raises a perfectly good `Refusal` and then prints a traceback of
        it. What a caller reads is a stream and an exit code, so that is what this reads.
        """
        proc = subprocess.run(
            [
                sys.executable,
                str(HERE / "umap_project.py"),
                "--input", str(self.dir / "base.parquet"),
                "--column", "lon", "--column", "lat",
                "--out", str(self.dir / "subprocess-out.parquet"),
                "--fit", str(self.written("subprocess-garbage.fit", b"not a fit")),
            ],
            capture_output=True,
            text=True,
        )
        self.assertEqual(proc.returncode, 1, f"exit {proc.returncode}: {proc.stderr}")
        self.assertNotIn("Traceback", proc.stderr)
        lines = [line for line in proc.stderr.splitlines() if line.strip()]
        self.assertEqual(len(lines), 1, f"stderr is not one line: {proc.stderr!r}")
        self.assertTrue(
            lines[0].startswith("umap_project: "), f"stderr does not name the operator: {lines[0]!r}"
        )
        self.assertFalse(
            (self.dir / "subprocess-out.parquet").is_file(),
            "a refused run wrote an output",
        )


class PersistedFitTest(unittest.TestCase):
    """Fit once, append, and read the coordinates back."""

    @classmethod
    def setUpClass(cls) -> None:
        cls._tmp = tempfile.TemporaryDirectory()
        d = cls.dir = Path(cls._tmp.name)
        _write(d / "base.parquet", _rows(BASE_ROWS))
        _write(d / "appended.parquet", _rows(BASE_ROWS) + _appended())
        _write(d / "shrunk.parquet", _rows(BASE_ROWS)[1:] + _appended())

        _project(input=d / "base.parquet", out=d / "first.parquet", fit=d / "fit.pkl")
        cls.fit_bytes = (d / "fit.pkl").read_bytes()
        cls.fit_mtime = (d / "fit.pkl").stat().st_mtime_ns
        _project(input=d / "appended.parquet", out=d / "second.parquet", fit=d / "fit.pkl")
        # No `--fit`: the refit control, and the thing this feature exists to replace.
        _project(input=d / "appended.parquet", out=d / "refit.parquet")

        cls.first = _xy(d / "first.parquet")
        cls.second = _xy(d / "second.parquet")
        cls.refit = _xy(d / "refit.parquet")

    @classmethod
    def tearDownClass(cls) -> None:
        cls._tmp.cleanup()

    def test_every_pre_existing_row_keeps_its_exact_coordinates(self) -> None:
        """The claim, stated as the number a reader would act on: not 'close', not
        'within a tolerance' — the same float. A tolerance here would pass on a re-placement of the
        base rows through `.transform()`, which is the plausible wrong implementation and
        the one an analyst could not see."""
        self.assertEqual(len(self.first), BASE_ROWS)
        moved = [
            (name, self.first[name], self.second[name])
            for name in self.first
            if self.first[name] != self.second[name]
        ]
        self.assertEqual(moved, [], "a row already on the map moved when rows were appended")

    def test_the_appended_rows_are_placed_and_are_real_positions(self) -> None:
        new = set(self.second) - set(self.first)
        self.assertEqual(len(new), APPENDED_ROWS)
        base_x = [p[0] for p in self.first.values()]
        base_y = [p[1] for p in self.first.values()]
        for name in new:
            x, y = self.second[name]
            self.assertTrue(math.isfinite(x) and math.isfinite(y), f"{name} has no position")
            # Inside the base map's own extent, generously bounded. A placement that
            # ignored its input — a constant, or a NaN swept to zero — would sit outside
            # a layout this compact, and "it is finite" alone would not notice.
            self.assertLess(abs(x), 10 * (max(base_x) - min(base_x)) + abs(min(base_x)) + 10)
            self.assertLess(abs(y), 10 * (max(base_y) - min(base_y)) + abs(min(base_y)) + 10)

    def test_the_control_shows_the_refit_this_replaces_moves_the_same_rows(self) -> None:
        """Without the control, 'the coordinates did not change' is also what a step that
        did nothing at all would report."""
        worst = max(
            max(abs(self.first[n][0] - self.refit[n][0]), abs(self.first[n][1] - self.refit[n][1]))
            for n in self.first
        )
        self.assertGreater(
            worst, 1.0, "the no-fit refit left every base row where it was, so this "
            "comparison cannot tell a held layout from a re-drawn one"
        )

    def test_the_fit_id_names_the_layout_so_an_append_stays_comparable(self) -> None:
        """One id per file, the same across the append — so the two files may be read
        row for row against each other — and a different one for the refit, which is what
        says they may not be."""
        first = _fit_ids(self.dir / "first.parquet")
        second = _fit_ids(self.dir / "second.parquet")
        refit = _fit_ids(self.dir / "refit.parquet")
        self.assertEqual(len(first), 1)
        self.assertEqual(len(second), 1)
        self.assertEqual(first, second, "an append into a persisted fit is the same layout")
        self.assertNotEqual(first, refit, "a refit is a different layout and must say so")

    def test_the_append_run_does_not_rewrite_the_fit(self) -> None:
        """The fit is written once. A run that re-pickled it on every append would move
        the artifact without moving the map, and a Protocol hashing that asset would call
        the next step stale for no reason.

        THE TIMESTAMP IS CHECKED AS WELL AS THE BYTES, and that is not belt-and-braces:
        re-pickling the SAME object graph in the same process can reproduce the same
        bytes, so a bytes-only assertion is green against a re-write that really happened.
        Breaking the code on purpose is how that was found."""
        fit = self.dir / "fit.pkl"
        self.assertEqual(fit.read_bytes(), self.fit_bytes)
        self.assertEqual(fit.stat().st_mtime_ns, self.fit_mtime, "the fit was re-written")


class RefusesAFitThatDoesNotDescribeTheInputTest(unittest.TestCase):
    """Driven through the real script rather than through `describe_mismatch` alone:
    a fit that loads, produces plausible coordinates, and is wrong."""

    @classmethod
    def setUpClass(cls) -> None:
        cls._tmp = tempfile.TemporaryDirectory()
        d = cls.dir = Path(cls._tmp.name)
        _write(d / "base.parquet", _rows(BASE_ROWS))
        _write(d / "appended.parquet", _rows(BASE_ROWS) + _appended())
        _write(d / "shrunk.parquet", _rows(BASE_ROWS)[1:] + _appended())
        _project(input=d / "base.parquet", out=d / "first.parquet", fit=d / "fit.pkl")

    @classmethod
    def tearDownClass(cls) -> None:
        cls._tmp.cleanup()

    def _refused(self, **kwargs) -> str:
        d = self.dir
        kwargs.setdefault("input", d / "appended.parquet")
        kwargs.setdefault("fit", d / "fit.pkl")
        out = d / "refused.parquet"
        before = out.read_bytes() if out.is_file() else None
        with self.assertRaises(up.Refusal) as ctx:
            _project(out=out, **kwargs)
        after = out.read_bytes() if out.is_file() else None
        self.assertEqual(before, after, "a refused run must not have written an output")
        return str(ctx.exception)

    def test_a_fit_for_different_columns(self) -> None:
        said = self._refused(columns=["lon", "lat"])
        self.assertIn("income", said)

    def test_a_fit_under_a_different_knob(self) -> None:
        self.assertIn("30", self._refused(neighbors=30))
        self.assertIn("cosine", self._refused(metric="cosine"))

    def test_an_input_a_row_was_removed_from_is_not_an_append(self) -> None:
        said = self._refused(input=self.dir / "shrunk.parquet")
        self.assertIn("1 of the 60", said)

    def test_a_file_that_is_not_this_operators_fit(self) -> None:
        stranger = self.dir / "stranger.pkl"
        stranger.write_bytes(pickle.dumps({"header": {"operator": "splink_resolve"}}))
        self.assertIn("splink_resolve", self._refused(fit=stranger))


if __name__ == "__main__":
    unittest.main(verbosity=2)
