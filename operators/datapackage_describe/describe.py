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

Because we shell out to whatever `finetype` is on PATH, the step first asserts a
minimum finetype version (`--min-finetype-version`) and fails closed below it — a
stale binary types columns wrong but still emits valid JSON, so trusting PATH
once shipped wrong labels downstream with no error. The SAME resolved binary is
then recorded: the step stamps the package-level `x-finetype-version` with the
dotted version that binary reported, so a descriptor carries the identity of the
engine that produced it and a stale one is visible by reading the file rather
than by trusting whatever produced it. A caller that wants to pin a run to one
exact release (not just "at least the floor") passes `--expect-finetype-version`;
a resolved binary that does not match it exactly refuses the run, naming both
versions, before finetype is ever invoked to profile the Parquet.

The step FAILS LOUDLY if a curated primaryKey / foreignKey names a column that
is not in the built Parquet — that is exactly the descriptor-drift this step
exists to catch, and a silent mismatch would be worse than a stopped run.
"""
from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys

# Top-level override keys handled structurally (everything else is copied to the
# package root as-is, e.g. title / description / homepage / licenses / sources).
_STRUCTURAL = {"resource", "fields", "primaryKey", "foreignKeys"}

# The floor finetype version whose taxonomy types these datasets correctly. A
# drifted, older binary on PATH once silently shipped wrong labels — the guard
# below turns that into a stopped run. Bump this when a newer finetype is needed
# for correct labels; a Protocol may override it with `--min-finetype-version`.
#
# 0.6.54 is the release that stopped eight-digit numbers typing as confident
# dates. Up to and including 0.6.53 the year-first and day-first compact date
# leaves both validated on `^\d{8}$`, so any eight-digit token — a financial
# figure, a surrogate key — came back as a high-confidence date WITH A `strptime`
# TRANSFORM ATTACHED. That is worse than a wrong label: a consumer that follows
# the transform gets a corrupted column, not a mislabelled one. 0.6.53 therefore
# has to be refused as firmly as 0.6.52 was.
MIN_FINETYPE_VERSION = "0.6.54"


def _parse_version(text: str) -> tuple[int, ...]:
    """Pull the first dotted numeric version out of text (e.g. 'finetype 0.6.53')."""
    m = re.search(r"(\d+)\.(\d+)\.(\d+)", text)
    return tuple(int(g) for g in m.groups()) if m else ()


def _require_finetype(min_version: str) -> str:
    """Fail closed unless the `finetype` on PATH is >= min_version.

    Column typing is only as good as the installed binary, and a stale one is
    invisible here — its output is still valid JSON. So assert the version before
    profiling rather than trust whatever PATH resolves to.

    Returns the resolved dotted version string (e.g. "0.6.60") on success — the
    SAME string this call already parsed to check the floor, reused by the
    caller both to stamp `x-finetype-version` and to check `--expect-finetype-version`.
    Resolving the version once and threading it through means the stamp can only
    ever name the binary this run actually asserted and ran, never a second,
    independent lookup that could in principle disagree with the first.
    """
    try:
        proc = subprocess.run(["finetype", "--version"], capture_output=True, text=True)
    except FileNotFoundError:
        sys.exit("describe: `finetype` is not on PATH — cannot type columns.")
    if proc.returncode != 0:
        sys.exit(f"describe: `finetype --version` failed ({proc.returncode})\n{proc.stderr}")
    got, want = _parse_version(proc.stdout), _parse_version(min_version)
    if not got:
        sys.exit(f"describe: could not parse a version from `finetype --version` ({proc.stdout!r}).")
    if got < want:
        got_s = ".".join(map(str, got))
        sys.exit(
            f"describe: finetype {got_s} on PATH is older than the required {min_version}; "
            "its labels are not trustworthy for these datasets. Update it "
            "(e.g. `cargo install --path crates/finetype-cli --force`) and re-run."
        )
    return ".".join(map(str, got))


def _require_exact_version(resolved: str, expect: str) -> None:
    """Fail closed unless the already-resolved `resolved` version equals `expect`.

    `--min-finetype-version` is a floor: anything newer passes, because a newer
    finetype only ever has a taxonomy at least as good. `--expect-finetype-version`
    is a pin: a caller that wants THIS EXACT release run (reproducing a prior
    descriptor, or holding a pipeline to a release under evaluation) asks for that
    directly, since "newer is fine" is not always true for a pinned pipeline — the
    engine that describes a dataset changing out from under it, silently, is
    exactly the failure `x-finetype-version` exists to make visible, and refusing
    up front is cheaper than discovering it by reading the stamp after the fact.
    """
    if resolved != expect:
        sys.exit(
            f"describe: finetype on PATH reports {resolved}, but "
            f"--expect-finetype-version {expect} was requested. Resolve which "
            "binary should produce this descriptor before running (e.g. `cargo "
            "install --path crates/finetype-cli --force` for a specific tag, or "
            "adjust --expect-finetype-version if a newer release is intended)."
        )


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
    ap.add_argument(
        "--min-finetype-version",
        default=MIN_FINETYPE_VERSION,
        help=f"fail if the finetype on PATH is older (default {MIN_FINETYPE_VERSION})",
    )
    ap.add_argument(
        "--expect-finetype-version",
        help="fail unless the resolved finetype reports exactly this version; "
        "pins a run to a known release instead of accepting any release at or "
        "above --min-finetype-version",
    )
    args = ap.parse_args()

    version = _require_finetype(args.min_finetype_version)
    if args.expect_finetype_version:
        _require_exact_version(version, args.expect_finetype_version)
    base = _finetype_datapackage(args.parquet)
    with open(args.overrides, encoding="utf-8") as fh:
        overrides = json.load(fh)

    descriptor = _merge(base, overrides)
    # Stamped AFTER the merge, so a sidecar override cannot supply (or clobber)
    # the identity of the engine that actually ran — this field is machine-derived
    # like the rest of finetype's half of the descriptor, not hand-curated, and it
    # is what makes a descriptor produced by a superseded finetype distinguishable
    # from a current one just by reading the file.
    descriptor["x-finetype-version"] = version

    with open(args.out, "w", encoding="utf-8") as fh:
        json.dump(descriptor, fh, indent=2, sort_keys=True, ensure_ascii=False)
        fh.write("\n")
    print(f"describe: wrote {args.out}", file=sys.stderr)


if __name__ == "__main__":
    main()
