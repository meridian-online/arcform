# Spec Review

**Date:** 2026-04-20
**Reviewer:** Context-separated agent (fresh session)
**Spec:** orbit/specs/2026-04-20-pipeline-parameterisation/spec.yaml
**Verdict:** REQUEST_CHANGES

---

## Review Depth

```
| Pass | Triggered by | Findings |
|------|-------------|----------|
| 1 — Structural scan | always | 4 |
| 2 — Assumption & failure | Pass 1 findings + engine trait change content signal | 3 |
| 3 — Adversarial | not triggered | — |
```

## Findings

### [LOW] Missing `indexmap` dependency in Cargo.toml change list
**Category:** missing-requirement
**Pass:** 1
**Description:** AC-01 specifies `params: IndexMap<String, Param>` but `indexmap` is not a current direct dependency (only transitive via Cargo.lock). The interview's "Files That Change" table lists the `dotenvy` addition to Cargo.toml but omits `indexmap`.
**Evidence:** Cargo.toml (line 12-22) shows no `indexmap` dependency. Spec AC-01 references `IndexMap<String, Param>`. Interview "Files That Change" only mentions `dotenvy`.
**Recommendation:** Add `indexmap = { version = "2", features = ["serde"] }` to the Cargo.toml changes in the interview, or switch to `Vec<(String, Param)>` / `BTreeMap` if insertion-order preservation is not load-bearing. (IndexMap is the right call given that deterministic merge order matters for resolve_params; just document the dependency.)

### [MEDIUM] AC-05 Engine trait signature change has no Error variant for env propagation failure
**Category:** missing-requirement
**Pass:** 1
**Description:** AC-05 adds an `env: &HashMap<String, String>` parameter to `Engine::execute_sql` and `Engine::execute_command`. This is a breaking change to the trait. The spec does not address what happens if `Command::envs()` fails (e.g. env var with NUL bytes in key or value on some platforms). More importantly, there is no new `Error` variant for param-related failures beyond `MissingParam` (AC-03).
**Evidence:** error.rs currently has no `MissingParam` variant. The spec mentions `Error::MissingParam` in AC-03 but does not list it as an explicit requirement for the error enum. The existing `StepExecution` error would likely cover env propagation failures, but this should be stated.
**Recommendation:** Add an AC or clarify in constraints that (a) `Error::MissingParam { name: String }` is a new error variant, and (b) `Command::envs()` failures are caught by the existing `StepExecution` variant — no new variant needed for that path.

### [MEDIUM] AC-07 output capture does not specify stderr behaviour for capturing steps
**Category:** assumption
**Pass:** 1
**Description:** AC-07 specifies that stdout is piped and captured when `output` is set on a command step. The interview states "capturing steps are silent." However, the spec does not specify what happens to stderr for output-capturing steps. Currently, `execute_command` uses `Stdio::inherit()` for stderr (line 102 of engine.rs), while `execute_sql` pipes and streams stderr. If a capturing command emits errors on stderr, the user may see no output at all (if stderr is also piped) or only stderr (if inherited). This needs an explicit decision.
**Evidence:** engine.rs line 99-101: command steps currently inherit both stdout and stderr. AC-07 changes stdout to piped but is silent on stderr. Interview says "output is captured, not streamed" which could be read as applying to all output.
**Recommendation:** Add a constraint or clarify in AC-07: "stderr remains inherited (streams to terminal) for output-capturing command steps." This preserves error visibility while capturing stdout.

### [LOW] AC-06 "missing files silently skipped" could mask configuration errors
**Category:** assumption
**Pass:** 1
**Description:** AC-06 states missing dotenv files are silently skipped. While this is a common pattern for `.env.local`, it means a typo in the manifest `dotenv:` list (e.g. `.en` instead of `.env`) produces no warning. Users may run with missing params and get `MissingParam` errors without understanding why.
**Evidence:** Spec constraint "Missing files silently skipped" and AC-06 verification "Test missing .env.local is silently skipped."
**Recommendation:** Consider logging a debug-level or `--verbose` warning when a dotenv file is not found, rather than pure silence. Not blocking — this is a UX concern, not correctness.

---

### [MEDIUM] Assumption: `Command::envs()` adds to inherited environment
**Category:** assumption
**Pass:** 2
**Description:** The interview states "Applied via `Command::envs()` which adds to (does not replace) inherited environment." This is correct for Rust's `std::process::Command` — `envs()` adds to the default inherited env. However, the codebase currently does NOT call `env_clear()` anywhere. If a future change adds `env_clear()` (e.g. for sandboxing), ARC_PARAM_ vars would be the only env and commands would break (no PATH, HOME, etc.). The spec should state this assumption explicitly as a constraint.
**Evidence:** engine.rs lines 67-77 and 97-107 show Command usage without `env_clear()`. The spec relies on additive env semantics but doesn't name it as a constraint.
**Recommendation:** Add a constraint: "Child processes inherit the parent process environment. `Command::envs()` adds ARC_PARAM_ vars without clearing inherited vars." This protects against future refactoring.

### [MEDIUM] Failure mode: output capture step fails mid-execution, downstream step uses stale/missing ARC_PARAM_
**Category:** failure-mode
**Pass:** 2
**Description:** AC-07 says output is captured and injected as `ARC_PARAM_{NAME}` for downstream steps. But the spec does not address what happens if the capturing step fails (non-zero exit). Currently, runner.rs halts on failure (line 137), so downstream steps never run. But if halt-on-failure behaviour ever changes (e.g. `continue_on_error` flag), a failed output-capture step would leave a dangling or missing env var for downstream steps. More immediately: what if the capturing step succeeds (exit 0) but produces empty stdout?
**Evidence:** AC-07 says "trimmed of trailing newline" — but if stdout is completely empty after trimming, is the env var set to empty string or omitted? This matters for `getenv()` in DuckDB which returns NULL for missing env vars vs empty string for set-but-empty.
**Recommendation:** Clarify: "If a capturing step produces empty stdout (after trimming), the env var is set to empty string (not omitted)." This distinction matters for downstream SQL using `getenv()`.

### [LOW] Test adequacy: AC-10 verification relies on internal state, not observable behaviour
**Category:** test-gap
**Pass:** 2
**Description:** AC-10's verification says "run with param A, then param B; verify SQL step is not marked stale (hash unchanged)." This tests internal staleness logic, which is fine, but the real proof is that the step is actually skipped. The existing staleness test pattern (e.g. `test_v03_ac04_fresh_step_skipped`) checks that the MockEngine receives no SQL call on the second run — AC-10's test should follow the same pattern.
**Evidence:** runner.rs test patterns at lines 562-585 verify skipping by asserting MockEngine call count.
**Recommendation:** Rephrase AC-10 verification to: "Unit test: run with param A, then param B (SQL file unchanged); verify the step is skipped on the second run (no engine call)." This aligns with the codebase's existing test strategy.

---

## Honest Assessment

This spec is well-structured, well-scoped, and closely aligned with the interview decisions. The core design — env vars as the transport, ARC_PARAM_ prefix, SQL passthrough preservation — is sound and consistent with decision 0003. The biggest risk is the Engine trait signature change (AC-05): adding `env` to both trait methods is a shotgun change that touches every Engine implementation and every call site. The spec accounts for MockEngine but should be more explicit about the error handling path. The stderr behaviour for output-capturing steps (AC-07) is the most likely source of a surprise during implementation — it needs one sentence of clarification. The missing `indexmap` dependency is trivial to fix. Overall, this is close to ready; the MEDIUM findings are addressable with minor spec edits, not a rethink.
