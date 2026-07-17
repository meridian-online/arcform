# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""Emit the Frictionless datapackage.json from the built Parquet.

This is the glue behind the Protocol's `describe` step. It splits the descriptor
into the two halves that have different authorities:

  1. The MACHINE-DECIDABLE half — per-field `type` / `format` and the
     `x-finetype-*` semantic labels + observed constraints — comes from
     `finetype profile ... -o datapackage`, which reads the built Parquet
     directly and types every column from its taxonomy. We take finetype's
     output verbatim as the base (including the resource `bytes` / `hash` /
     `format` / `mediatype`, which it computes from the Parquet itself).

  2. The HAND-CURATED half — everything finetype CANNOT infer from column
     values — comes from `descriptor.overrides.json`, a small checked-in
     sidecar. That is: the package title / description / homepage / licenses /
     sources, the published resource `path`, per-field prose `description`s,
     and the relational metadata (`primaryKey` + `foreignKeys` with their
     `x-evidence` / `x-confidence` / `x-status`). Overrides WIN over finetype;
     finetype supplies everything the sidecar does not mention.

finetype limitation (documented in README "Describe"): it emits ONE typed Data
Resource and nothing above the field level — no package identity, no relational
edges, no evidence. Those cannot be derived from the data and so live in the
sidecar. finetype's semantic labels are also only as good as the installed
finetype; a wrong label is corrected by adding the field to the sidecar's
`fields` map (an override sets any field key, including `x-finetype-label`).

The step FAILS LOUDLY if a curated primaryKey / foreignKey names a column that
is not in the built Parquet — that is exactly the descriptor-drift this step
exists to catch, and a silent mismatch would be worse than a stopped run.
"""
from __future__ import annotations

import argparse
import json
import subprocess
import sys

# Top-level override keys handled structurally (everything else is copied to the
# package root as-is, e.g. title / description / homepage / licenses / sources).
_STRUCTURAL = {"resource", "fields", "primaryKey", "foreignKeys"}


def _finetype_datapackage(parquet: str) -> dict:
    """Run finetype and return its Frictionless Data Package as a dict."""
    proc = subprocess.run(
        ["finetype", "profile", "-f", parquet, "-o", "datapackage"],
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        sys.exit(f"describe: finetype failed ({proc.returncode})\n{proc.stderr}")
    try:
        return json.loads(proc.stdout)
    except json.JSONDecodeError as exc:  # pragma: no cover - defensive
        sys.exit(f"describe: could not parse finetype output as JSON: {exc}")


def _merge(base: dict, overrides: dict) -> dict:
    """Overlay the curated sidecar onto finetype's base descriptor (overrides win)."""
    # 1) Package-level curated keys (title, description, homepage, licenses, sources, …).
    for key, value in overrides.items():
        if key not in _STRUCTURAL:
            base[key] = value

    resource = base["resources"][0]

    # 2) Resource-level overrides (e.g. the published `path`; finetype writes the
    #    local build path, which is not where consumers fetch the resource).
    for key, value in overrides.get("resource", {}).items():
        resource[key] = value

    schema = resource["schema"]
    present = {f["name"] for f in schema["fields"]}

    # 3) Per-field overrides, matched by column name (adds `description`, or
    #    corrects a `x-finetype-label`, etc.). A sidecar field that does not
    #    exist in the Parquet is a stale override — warn so it gets cleaned up.
    field_overrides = overrides.get("fields", {})
    for name in field_overrides.keys() - present:
        print(f"describe: WARNING field override '{name}' is not in the Parquet "
              f"(stale sidecar entry?)", file=sys.stderr)
    for field in schema["fields"]:
        for key, value in field_overrides.get(field["name"], {}).items():
            field[key] = value

    # 4) Relational metadata. These are hand-curated and cannot be inferred, but
    #    they MUST reference real columns — hard-fail on drift.
    for rel_key in ("primaryKey", "foreignKeys"):
        if rel_key not in overrides:
            continue
        schema[rel_key] = overrides[rel_key]
    _check_relations(schema, present)

    return base


def _check_relations(schema: dict, present: set[str]) -> None:
    """Fail if a primaryKey / foreignKey names a column absent from the Parquet."""
    missing: list[str] = []
    for col in schema.get("primaryKey", []):
        if col not in present:
            missing.append(f"primaryKey → '{col}'")
    for fk in schema.get("foreignKeys", []):
        for col in fk.get("fields", []):
            if col not in present:
                missing.append(f"foreignKey → '{col}'")
    if missing:
        cols = ", ".join(sorted(present))
        sys.exit(
            "describe: curated relational metadata references columns not in the "
            "built Parquet — the descriptor has drifted from package.sql. "
            "Reconcile descriptor.overrides.json.\n"
            f"  missing: {'; '.join(missing)}\n"
            f"  columns: {cols}"
        )


def main() -> None:
    ap = argparse.ArgumentParser(description="Emit datapackage.json from the built Parquet.")
    ap.add_argument("--parquet", required=True, help="built Parquet (finetype types it)")
    ap.add_argument("--overrides", required=True, help="curated sidecar (JSON)")
    ap.add_argument("--out", required=True, help="datapackage.json to write")
    args = ap.parse_args()

    base = _finetype_datapackage(args.parquet)
    with open(args.overrides, encoding="utf-8") as fh:
        overrides = json.load(fh)

    descriptor = _merge(base, overrides)

    with open(args.out, "w", encoding="utf-8") as fh:
        json.dump(descriptor, fh, indent=2, sort_keys=True, ensure_ascii=False)
        fh.write("\n")
    print(f"describe: wrote {args.out}", file=sys.stderr)


if __name__ == "__main__":
    main()
