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

ONE PROCESS, EIGHT RUNS. `main()` is called with a patched `sys.argv` rather than
spawned eight times, so numba JIT-compiles UMAP once instead of once per case. That is
the difference between a couple of minutes and a quarter of an hour on a cold runner.

Run: `uv run operators/umap_project/test_umap_project_fit.py`. Needs `uv`; CI runs it in
the `build` job, which installs `uv` for the operator suite anyway.
"""
from __future__ import annotations

import importlib.util
import math
import pickle
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
        """AC1, stated as the number a reader would act on: not 'close', not 'within a
        tolerance' — the same float. A tolerance here would pass on a re-placement of the
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
        """AC3. One id per file, the same across the append — the two files may be read
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
        the artifact's bytes without moving the map, and a Protocol hashing that asset
        would call the next step stale for no reason."""
        self.assertEqual((self.dir / "fit.pkl").read_bytes(), self.fit_bytes)


class RefusesAFitThatDoesNotDescribeTheInputTest(unittest.TestCase):
    """AC4, driven through the real script rather than through `describe_mismatch`
    alone: a fit that loads, produces plausible coordinates, and is wrong."""

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
