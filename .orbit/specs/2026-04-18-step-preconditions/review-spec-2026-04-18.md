# Spec Review

**Date:** 2026-04-18
**Reviewer:** Nightingale (inline review, drive session)
**Spec:** specs/2026-04-18-step-preconditions/spec.yaml
**Verdict:** REQUEST_CHANGES

---

## Review Depth

| Pass | Triggered by | Findings |
|------|-------------|----------|
| 1 — Structural scan | always | 2 |
| 2 — Assumption & failure | backwards-compat content signal | 2 |
| 3 — Adversarial | not triggered | — |

## Findings

### [LOW] modified_after path resolution not explicit
**Category:** missing-requirement
**Pass:** 1
**Description:** The spec says `path` is a string but doesn't state what it's relative to. SQL file paths in arcform are relative to the manifest directory.
**Evidence:** ontology_schema field `path` has no resolution note; interview doesn't address this.
**Recommendation:** Add constraint: "`modified_after` paths are relative to the manifest directory (same convention as `sql` file paths)"

### [LOW] modified_after permission error unspecified
**Category:** failure-mode
**Pass:** 1
**Description:** If the file exists but can't be stat'd (permission denied), the behaviour is undefined. Missing file = stale (ac-04), but permission error could be treated as stale or as a halt-worthy error.
**Recommendation:** Add to ac-04 or as a note: "File stat errors (permission denied) are treated as stale, not as pipeline-halting errors — same as missing file."

### [LOW] Test strategy for file mtime
**Category:** test-gap
**Pass:** 2
**Description:** ac-03 verification says "touch file with old mtime" but doesn't specify how. Wall-clock-dependent tests are fragile.
**Recommendation:** Add constraint: "Use `filetime` crate in tests to set file mtime explicitly — no sleep-based or wall-clock-dependent tests"

### [LOW] ac-09 four-combination coverage
**Category:** test-gap
**Pass:** 2
**Description:** ac-09 verification describes three of four (hash_stale, precondition_stale) combinations. The fourth (both stale → runs) is trivially implied but should be explicit for completeness.
**Recommendation:** Update ac-09 verification to list all four combinations.

---

## Honest Assessment

This is a well-structured spec. The typed precondition model is a clean design — extensible without being over-engineered. The AND semantics with hash staleness are the right conservative choice. All four findings are LOW severity — implementation guidance and completeness notes, not structural problems. Addressing them strengthens the spec but none block implementation. Recommending REQUEST_CHANGES to tighten the spec before implementation begins.
