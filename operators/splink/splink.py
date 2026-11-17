# /// script
# requires-python = ">=3.12,<3.13"
# dependencies = ["splink==5.0.0", "duckdb==2.0.0", "pyarrow==26.0.1"]
# ///
"""splink -- arcform operator `splink@1`: score record pairs with a trained Splink model.

Everything dataset-specific arrives from the Protocol's `with:`: the inputs, the model
(a fully specified Splink settings JSON with every non-null level carrying m and u
probabilities) and the output path. The script holds no source's columns or model.

    splink.py --input ROLE=PATH [--input ROLE=PATH ...] --model PATH --out PATH
              [--threshold-match-probability X | --threshold-match-weight X]

Exit 0 on success; exit 2 with one stderr line per refusal. Refusals are decided
before the Linker is built. `model_problems` is importable with the standard library
alone, so it is tested without Splink: Splink, DuckDB and pyarrow load inside `main()`.
"""
from __future__ import annotations

import argparse
import json
import os
import re
import sys

LINK_TYPES = ("dedupe_only", "link_only", "link_and_dedupe")


def _quoted(name: str) -> str:
    return '"' + name.replace('"', '""') + '"'


def _level_columns(sql: str) -> set[str]:
    """Columns a comparison-level condition reads: `"col_l"`, `col_r`, ..."""
    found = set(re.findall(r'"([^"]+)_[lr]"', sql))
    found |= set(re.findall(r"(?<![\w\"])([A-Za-z_]\w*)_[lr](?!\w)", sql))
    return found


def _rule_columns(rule: str) -> set[str]:
    """Columns a blocking rule reads: `l.col`, `r."col"`."""
    return {a or b for a, b in re.findall(r'\b[lr]\.(?:"([^"]+)"|(\w+))', rule)}


def model_problems(settings: dict, columns: dict[str, dict[str, str]]) -> list[str]:
    """One message per reason the model cannot run over these inputs.

    `columns` maps each input's role to {column_name: duckdb_type}.
    """
    problems: list[str] = []
    comparisons = settings.get("comparisons")
    if not isinstance(comparisons, list) or not comparisons:
        problems.append("model has no `comparisons`: it needs at least one comparison")
        comparisons = []

    needed: dict[str, str] = {}  # column -> where the model names it
    uid = settings.get("unique_id_column_name", "unique_id")
    needed[uid] = "unique_id_column_name"

    for comp in comparisons:
        cname = comp.get("output_column_name", "?")
        for level in comp.get("comparison_levels", []):
            label = level.get("label_for_charts") or level.get("sql_condition", "?")
            if not level.get("is_null_level"):
                missing = [k for k in ("m_probability", "u_probability") if level.get(k) is None]
                if missing:
                    problems.append(
                        f"comparison {cname!r} level {label!r} has no "
                        f"{' or '.join(missing)}: the model is not fully specified"
                    )
            for col in _level_columns(level.get("sql_condition", "")):
                needed.setdefault(col, f"comparison {cname!r} level {label!r}")

    for rule in settings.get("blocking_rules_to_generate_predictions", []) or []:
        sql = rule.get("blocking_rule", "") if isinstance(rule, dict) else str(rule)
        for col in _rule_columns(sql):
            needed.setdefault(col, f"blocking rule {sql!r}")
    for col in settings.get("additional_columns_to_retain", []) or []:
        needed.setdefault(col, "additional_columns_to_retain")

    link_type = settings.get("link_type", "dedupe_only")
    n = len(columns)
    if link_type == "dedupe_only" and n != 1:
        problems.append(f"link_type dedupe_only needs exactly one input, found {n}")
    elif link_type in ("link_only", "link_and_dedupe") and n < 2:
        problems.append(f"link_type {link_type} needs at least two inputs, found {n}")
    elif link_type not in LINK_TYPES:
        problems.append(f"link_type {link_type!r} is not one of {', '.join(LINK_TYPES)}")

    for role, cols in columns.items():
        for col, where in needed.items():
            if col not in cols:
                problems.append(f"input {role!r} has no column {col!r}, named by {where}")

    for col, where in needed.items():
        seen = {r: c[col] for r, c in columns.items() if col in c}
        if len(set(seen.values())) > 1:
            detail = ", ".join(f"{r} is {t}" for r, t in seen.items())
            problems.append(f"column {col!r} has different types across inputs: {detail}")
    return problems


def _parse_args(argv):
    p = argparse.ArgumentParser(prog="splink.py")
    p.add_argument("--input", action="append", default=[], metavar="ROLE=PATH")
    p.add_argument("--model", required=True)
    p.add_argument("--out", required=True)
    p.add_argument("--threshold-match-probability", type=float)
    p.add_argument("--threshold-match-weight", type=float)
    return p.parse_args(argv)


def _refuse(lines: list[str]) -> int:
    for line in lines:
        print(f"splink: {line}", file=sys.stderr)
    return 2


def main(argv=None) -> int:
    args = _parse_args(argv)
    refusals: list[str] = []
    inputs: dict[str, str] = {}
    for item in args.input:
        role, sep, path = item.partition("=")
        if not sep or not role or not path:
            refusals.append(f"--input {item!r} is not ROLE=PATH")
        elif role in inputs:
            refusals.append(f"role {role!r} is given twice")
        else:
            inputs[role] = path
    if not inputs and not refusals:
        refusals.append("at least one --input is required")
    if args.threshold_match_probability is not None and args.threshold_match_weight is not None:
        refusals.append("set threshold_match_probability or threshold_match_weight, not both")
    p = args.threshold_match_probability
    if p is not None and not 0 <= p <= 1:
        refusals.append(f"threshold_match_probability {p} is outside 0 to 1")

    settings = None
    try:
        with open(args.model, encoding="utf-8") as f:
            settings = json.load(f)
        if not isinstance(settings, dict):
            raise ValueError("it is not a JSON object")
    except (OSError, ValueError) as e:
        refusals.append(f"model {args.model} cannot be read as a settings object: {e}")
        settings = None
    if refusals:
        return _refuse(refusals)

    # This file is named splink.py: drop its own directory from the import path so
    # `import splink` finds the package, not the script.
    here = os.path.dirname(os.path.abspath(__file__))
    sys.path[:] = [q for q in sys.path if os.path.abspath(q or ".") != here]

    import duckdb
    import pyarrow.parquet as pq
    from splink import DuckDBAPI, Linker

    con = duckdb.connect()
    relations, columns = {}, {}
    for role, path in inputs.items():
        try:
            rel = con.read_parquet(path)  # path is bound by the driver, never pasted into SQL
        except duckdb.Error as e:
            return _refuse([f"input {role!r} at {path} cannot be read as Parquet: {e}"])
        relations[role] = rel
        columns[role] = dict(zip(rel.columns, (str(t) for t in rel.types)))

    problems = model_problems(settings, columns)
    if problems:
        return _refuse(problems)

    db_api = DuckDBAPI(connection=con)
    sdfs = [db_api.register(rel, dataset_display_name=role) for role, rel in relations.items()]
    linker = Linker(sdfs if len(sdfs) > 1 else sdfs[0], args.model)
    predictions = linker.inference.predict(
        threshold_match_probability=args.threshold_match_probability,
        threshold_match_weight=args.threshold_match_weight,
    )

    uid = settings.get("unique_id_column_name", "unique_id")
    keys = [f"{_quoted(uid + '_l')}", f"{_quoted(uid + '_r')}"]
    if len(inputs) > 1:
        keys = ['"source_dataset_l"', '"source_dataset_r"'] + keys
    table = con.sql(
        f"SELECT * FROM {predictions.physical_name} ORDER BY {', '.join(keys)}"
    ).to_arrow_table()
    pq.write_table(table, args.out, compression="zstd")
    return 0


if __name__ == "__main__":
    sys.exit(main())
