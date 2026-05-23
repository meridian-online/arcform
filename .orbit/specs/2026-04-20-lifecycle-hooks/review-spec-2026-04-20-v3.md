# Spec Review

**Date:** 2026-04-20
**Reviewer:** Context-separated agent (fresh session)
**Spec:** orbit/specs/2026-04-20-lifecycle-hooks/spec.yaml
**Verdict:** APPROVE

---

## Review Depth

| Pass | Triggered by | Findings |
|------|-------------|----------|
| 1 — Structural scan | always | 2 |
| 2 — Assumption & failure | not triggered | — |
| 3 — Adversarial | not triggered | — |

## Prior Review Resolution

This is review v3. The spec (now v1.2) has addressed all MEDIUM findings from v1 and v2:

- **v1: AC-04 env var scope ambiguity** — Resolved. Constraint 14 now explicitly enumerates which env vars each hook receives, including the explicit exclusion of ARC_PIPELINE_STATUS from on_failure.
- **v1: AC-07 verification incomplete** — Resolved. AC-07 verification now lists timeout_sec and depends_on checks.
- **v2: Inherited defaults for retry** — Resolved. Constraint 17 states "Hooks do not inherit manifest.defaults.retry — defaults resolution skips hooks entirely."
- **v2: init_failed env var gap** — Resolved. Constraint 18 explicitly states ARC_FAILED_STEP and ARC_EXIT_CODE are NOT set on init_failed path, with rationale.
- **v2: Pipeline timeout interaction** — Resolved. Constraint 19 states hooks run outside the pipeline timeout boundary with explicit "unbounded in v1" scoping.

## Findings

### [LOW] Inter-hook name collision not explicitly covered
**Category:** missing-requirement
**Pass:** 1
**Description:** Constraint 8 and AC-07 require "Hook step names must not collide with pipeline step names." However, uniqueness between hooks themselves is not stated. If on_init and on_exit both have `name: cleanup`, is that valid? The natural expectation is no (names should be globally unique for log clarity), but the spec is silent on this case.
**Evidence:** Constraint 8: "Hook step names must not collide with pipeline step names." AC-07 verification: "Hook name same as step name -> error." Neither mentions inter-hook uniqueness.
**Recommendation:** The implementer will likely enforce this naturally (collecting all names into a HashSet), but if you want belt-and-suspenders clarity, add to AC-07 verification: "Two hooks with same name -> error." Not a blocker — implementation will almost certainly do the right thing.

### [LOW] AC-10 still has no independent verification
**Category:** test-gap
**Pass:** 1
**Description:** AC-10 (execute_hook helper) verification remains "Covered by ac-02 through ac-06 integration tests." This was flagged in v1 and v2 as LOW. The spec maintainer has implicitly accepted this trade-off by not changing it across two revisions, which is a valid decision for a small helper function whose behaviour is fully exercised by the surrounding ACs.
**Evidence:** AC-10 verification field unchanged across three reviews.
**Recommendation:** No action required. Noted for completeness.

---

## Honest Assessment

The spec is ready for implementation. Version 1.2 has cleanly resolved every MEDIUM finding from two prior reviews — the env var scoping is now unambiguous, the defaults inheritance question is explicitly answered, and the pipeline timeout boundary is declared. The remaining two LOW findings are cosmetic: inter-hook name collision will be caught naturally by any reasonable implementation, and AC-10's delegated verification is an acceptable trade-off for a helper function with trivial branching logic. The biggest implementation challenge will be restructuring the step loop in runner.rs (currently ~150 lines of inline logic) to support the init/success/failure/exit wrapping without introducing nested Result complexity — but the spec's non-fatal semantics and clear phase ordering make that tractable. Ship it.
