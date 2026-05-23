# Spec Review

**Date:** 2026-04-20
**Reviewer:** Context-separated agent (fresh session)
**Spec:** orbit/specs/2026-04-20-lifecycle-hooks/spec.yaml
**Verdict:** REQUEST_CHANGES

---

## Review Depth

| Pass | Triggered by | Findings |
|------|-------------|----------|
| 1 — Structural scan | always | 3 |
| 2 — Assumption & failure | Pass 1 findings (MEDIUM severity) | 3 |
| 3 — Adversarial | not triggered | — |

## Findings

### [MEDIUM] AC-04 env var scope ambiguity — does on_failure also receive ARC_PIPELINE_STATUS?

**Category:** missing-requirement
**Pass:** 1
**Description:** AC-05 specifies that on_exit receives `ARC_PIPELINE_STATUS`, and constraint line 14 says "on_exit also gets ARC_PIPELINE_STATUS". However, it is unclear whether on_failure also receives `ARC_PIPELINE_STATUS='failed'`. The on_failure hook receives `ARC_FAILED_STEP` and `ARC_EXIT_CODE` per AC-04, but not `ARC_PIPELINE_STATUS`. This is probably intentional (on_failure implies failed, so the var is redundant), but a hook implementor writing a shared script for both on_failure and on_exit would benefit from knowing whether `ARC_PIPELINE_STATUS` is always present or only in on_exit.
**Evidence:** Constraint line 14: "Failure context env vars: ARC_FAILED_STEP (step name), ARC_EXIT_CODE (exit code string). on_exit also gets ARC_PIPELINE_STATUS." AC-04 verification mentions only ARC_FAILED_STEP and ARC_EXIT_CODE. AC-05 verification mentions only ARC_PIPELINE_STATUS.
**Recommendation:** Add a note to AC-04 or the constraints clarifying that on_failure does NOT receive ARC_PIPELINE_STATUS (or that it does). Either choice is fine — just make it explicit so the implementor does not have to guess.

### [MEDIUM] AC-07 validation scope is incomplete — `timeout_sec` rejection is in constraints but not in AC-07 verification

**Category:** test-gap
**Pass:** 1
**Description:** Constraint line 15 states "retry/timeout_sec on hook steps are rejected at manifest validation." AC-07's description mentions `retry, timeout_sec` in the reject list, and the verification line enumerates checks for "preconditions, retry, produces, output." The verification omits `timeout_sec` and `depends_on` explicitly — both are listed in the AC-07 description but not in the verification checklist.
**Evidence:** AC-07 description: "Preconditions, produces, depends_on, retry, timeout_sec, and output on hooks are rejected." AC-07 verification: "Hook with preconditions -> error. Hook with retry -> error. Hook with produces -> error. Hook with output -> error. Hook name same as step name -> error." Missing: `timeout_sec` and `depends_on`.
**Recommendation:** Add to AC-07 verification: "Hook with timeout_sec -> error. Hook with depends_on -> error." This ensures the implementation test list covers the full reject surface.

### [LOW] AC-10 has no independent verification — relies solely on AC-02 through AC-06

**Category:** test-gap
**Pass:** 1
**Description:** AC-10 (execute_hook helper) declares verification as "Covered by ac-02 through ac-06 integration tests." This is pragmatic but means the helper function's interface (signature, return type, non-fatal wrapping logic) is never tested in isolation. If a refactor changes the helper boundary, coverage could silently regress.
**Evidence:** AC-10 verification field text.
**Recommendation:** This is acceptable for v1 given the small surface area. No change required, but consider adding a unit test for the non-fatal wrapper logic specifically (takes an Err, prints it, returns Ok) during implementation if natural to do so.

### [MEDIUM] on_exit must run even when on_failure panics or aborts — but the spec does not address panic/abort scenarios

**Category:** assumption
**Pass:** 2
**Description:** The spec guarantees "on_exit always runs if on_init was attempted" (try/finally semantics). The interview and spec discuss hook *failure* (non-zero exit code), but not what happens if the on_failure hook's subprocess is killed by a signal (SIGKILL, SIGTERM) or the thread panics. In Rust, a caught panic would bypass the on_exit call unless structured with `catch_unwind` or the on_exit call is placed in a `Drop` guard.
**Evidence:** Constraint line 6: "on_exit always runs if on_init was attempted (try/finally semantics) — even if on_init, on_failure, or on_success fail." The Engine trait returns `Result<StepOutput>` — signal-killed processes return `StepFailed` with `code = -1` (from `status.code().unwrap_or(1)`). So normal signal cases are covered. However, if the *arcform process itself* receives SIGTERM mid-hook, on_exit is skipped.
**Recommendation:** Clarify scope: "on_exit runs after hook/step failures that produce a Result::Err. Arcform process-level signals (SIGKILL, SIGTERM to the parent) are out of scope for v1." This prevents implementors from over-engineering a signal handler.

### [MEDIUM] Dependency on card 0016 — spec assumes `env: &HashMap<String, String>` already exists on Engine trait

**Category:** assumption
**Pass:** 2
**Description:** The spec's AC-09 states hooks "execute via the same engine.execute_sql / engine.execute_command paths as steps" and "env vars are passed via the same HashMap mechanism from 0016." This dependency is real and already shipped — the Engine trait in `engine.rs` already accepts `env: &HashMap<String, String>`. However, neither the spec nor interview explicitly lists 0016 as a prerequisite/dependency.
**Evidence:** Interview line 9: "Depends on 0016's `env: &HashMap<String, String>` parameter on the Engine trait." The spec itself has no `dependencies:` or `requires:` field.
**Recommendation:** No blocker since 0016 is already merged (visible in the codebase). However, adding a `dependencies: [0016]` field to spec metadata would make the relationship machine-traceable for future tooling.

### [LOW] Constraint conflict potential: "hooks do not have produces/depends_on" vs SQL hooks that auto-discover assets

**Category:** constraint-conflict
**Pass:** 2
**Description:** Constraint line 16 says hooks cannot have `produces/depends_on`, and AC-07 rejects them at validation. However, SQL steps normally have their assets auto-discovered via sqlparser introspection (per the asset module). If a hook is a SQL step, does the asset graph builder try to introspect it? If so, it might register the hook's outputs/inputs in the asset graph, creating phantom dependencies.
**Evidence:** `AssetGraph::build(&manifest, dir)` in runner.rs line 145 operates on `manifest.steps`. If hooks are NOT in `manifest.steps` (they are in `manifest.hooks`), this is not an issue. The spec's Hooks struct places hooks separately from steps, so the asset graph builder would naturally skip them.
**Recommendation:** No change needed — the structural separation (hooks live in `Hooks` struct, not in `steps: Vec<Step>`) naturally prevents this. But worth a mental note during implementation: do not add hooks to the step list before passing to AssetGraph.

---

## Honest Assessment

This spec is well-structured and nearly implementation-ready. The design is sound — reusing the existing Step struct and Engine trait keeps complexity low, and the try/finally semantics are clearly defined. The two substantive issues are: (1) the AC-07 verification checklist is incomplete relative to its own description (missing `timeout_sec` and `depends_on` checks), and (2) the env var scope for on_failure vs on_exit should be explicitly disambiguated to prevent implementor guesswork. Both are straightforward fixes. The biggest implementation risk is ensuring the on_exit guarantee holds across all error paths in runner.rs without introducing nested Result-juggling complexity — but the spec's decision to make hook failures non-fatal simplifies this considerably.
