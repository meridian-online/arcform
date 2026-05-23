# Spec Review

**Date:** 2026-04-17
**Reviewer:** Nightingale (inline review, same session — design context retained)
**Spec:** specs/2026-04-17-local-remote-parity/spec.yaml
**Verdict:** REQUEST_CHANGES

---

## Review Depth

| Pass | Triggered by | Findings |
|------|-------------|----------|
| 1 — Structural scan | always | 2 |
| 2 — Assumption & failure | MEDIUM findings in Pass 1 | 1 |
| 3 — Adversarial | not triggered | — |

## Findings

### [MEDIUM] Missing AC for unparseable version output
**Category:** missing-requirement
**Pass:** 1
**Description:** ac-03 specifies parsing `duckdb --version` output but no AC covers the failure case: what happens when the output doesn't match the expected format? Future DuckDB releases could change the format. The spec should specify graceful degradation — warn and skip version check, or fail hard?
**Evidence:** ac-03 verification only tests the happy path: "extracts '1.5.2' from 'v1.5.2 (Variegata) 8a5851971f'". No edge case handling specified.
**Recommendation:** Add an AC (ac-13): "If engine version output cannot be parsed, preflight succeeds with a warning and the version check is skipped. The pipeline still runs." This follows ArcForm's existing pattern — unparseable SQL warns but still executes (v0.2 AC-07).

### [LOW] ac-09 doesn't specify what version to scaffold
**Category:** missing-requirement
**Pass:** 1
**Description:** ac-09 says "arc init scaffolds engine_version in the generated arcform.yaml" but doesn't specify what value. Options: detect current version, use a hardcoded default like ">=1.0", or leave it as a commented-out example.
**Evidence:** Interview summary says "arc init includes engine_version in scaffolded manifest" without specifying the value.
**Recommendation:** Scaffold with the currently-detected version as a minimum: if duckdb is installed, scaffold `engine_version: ">=<detected>"`. If not installed, omit the field. This gives the best UX — the constraint is automatically set to what works.

### [LOW] ac-03 version parsing could be more robust
**Category:** test-gap
**Pass:** 2
**Description:** The verification for ac-03 tests one specific format. DuckDB's actual output has varied across versions (some lack the codename). The test should cover multiple known formats.
**Evidence:** Current DuckDB output: `v1.5.2 (Variegata) 8a5851971f`. Older versions may output differently. The regex should be flexible (match `vX.Y.Z` at minimum).
**Recommendation:** ac-03 verification should include at least two format variants, e.g. with and without codename.

---

## Honest Assessment

This is a clean, well-scoped spec. The goal matches the ACs, constraints are complementary, and backwards compatibility is explicitly handled. The two substantive findings are both about graceful degradation — what happens when the real world doesn't match the happy path. Adding an AC for unparseable version output (following ArcForm's existing warn-and-continue pattern) and clarifying the init scaffold value will make this implementation-ready. No structural concerns — REQUEST_CHANGES, not BLOCK.
