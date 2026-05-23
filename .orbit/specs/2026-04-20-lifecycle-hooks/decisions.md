# 0017 Lifecycle Hooks — Decision Pack

## Decision 1: Hook Type — Full Step or Command-Only

**Context:** Hooks could reuse the existing `Step` struct (supporting both SQL and shell commands) or be a simpler command-only type.

**Options:**
- A. Full Step objects — hooks reuse the existing Step struct, supporting both SQL and shell commands
- B. Command-only — hooks are always shell commands (simpler struct, no sql field)
- C. New HookStep type — subset of Step fields (name, command, sql) without produces/depends_on/preconditions

**Trade-offs:**
- A: Maximum reuse — hooks get the same validation, same execution paths. A SQL teardown hook (e.g. `DROP TABLE staging`) is natural.
- B: Simpler but limits use cases. SQL cleanup hooks are a real need (drop temp tables, vacuum).
- C: New type for a small feature. Validation duplication.

**Recommendation:** A — full Step objects. Reuse is free; SQL hooks are a real use case. Validation rejects fields that don't apply to hooks (preconditions, produces, depends_on).

## Decision 2: Init Failure Semantics

**Context:** If `on_init` fails, what happens to `on_failure` and `on_exit`?

**Options:**
- A. Init failure is fatal — pipeline aborts. `on_exit` still runs. `on_failure` does NOT run (no step failed — init failure is a separate path).
- B. Init failure triggers `on_failure` then `on_exit`
- C. Init failure skips all hooks — immediate exit

**Trade-offs:**
- A: Clean semantics — `on_failure` means "a pipeline step failed", not "anything failed". `on_exit` is the try/finally guarantee.
- B: Conflates init failure with step failure. `on_failure` handlers written for step context (e.g. `$ARC_FAILED_STEP`) would get unexpected empty values.
- C: Loses the cleanup guarantee — the whole point of `on_exit`.

**Recommendation:** A — init failure aborts, `on_exit` still runs, `on_failure` does not. Clean separation of failure modes.

## Decision 3: Hook Failure Impact on Exit Code

**Context:** If a hook (other than init) fails, should it change the pipeline's exit code?

**Options:**
- A. Non-fatal — hook failures are reported (stderr) but pipeline exit code reflects the original outcome
- B. Fatal — any hook failure changes exit code to non-zero
- C. Configurable per hook — `fatal: true|false`

**Trade-offs:**
- A: Pipeline exit code is honest about pipeline outcome. A flaky notification hook doesn't cause CI failures.
- B: Strict but causes cascading failures — a Slack webhook timeout would fail the whole pipeline.
- C: Flexible but adds config surface for an edge case.

**Recommendation:** A — non-fatal. Pipeline exit code reflects pipeline outcome, not hook outcome. Exception: `on_init` is fatal because it gates step execution.

## Decision 4: Hook State Recording

**Context:** Should hook executions be recorded in the state backend?

**Options:**
- A. No state recording for v1 — hooks are invisible to the state tracker; terminal output is sufficient
- B. Record in step-state table alongside pipeline steps
- C. Record on the run record only (not step-state)

**Trade-offs:**
- A: Simplest. No staleness pollution. Terminal output shows what hooks ran and whether they succeeded. Agreed simplification from the prior design review.
- B: Pollutes staleness model — hooks would need special-casing in freshness queries.
- C: Partial observability without staleness pollution, but adds schema changes for minimal value.

**Recommendation:** A — no state recording for v1. Terminal output is enough. Add run-record annotations later if observability demand emerges.

## Decision 5: Hook Staleness Participation

**Context:** Should hooks participate in staleness detection (preconditions, hash checking)?

**Options:**
- A. Hooks always run when their phase triggers — no staleness
- B. Hooks participate in staleness like normal steps

**Trade-offs:**
- A: Correct semantics — init is setup, success/failure are notifications, exit is cleanup. All must fire every time.
- B: Would allow skipping cleanup or notifications, defeating the purpose.

**Recommendation:** A — hooks always run. Validation rejects preconditions on hook steps to make this explicit.
