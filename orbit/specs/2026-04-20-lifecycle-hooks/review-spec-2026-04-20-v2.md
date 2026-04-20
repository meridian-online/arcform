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
| 2 — Assumption & failure | Pass 1 findings (constraint ambiguity, env var gap) | 3 |
| 3 — Adversarial | not triggered | — |

## Findings

### [MEDIUM] Constraint conflict: "retry/timeout_sec rejected" vs Step struct reuse
**Category:** constraint-conflict
**Pass:** 1
**Description:** Constraint states "Hooks are full Step objects — reuse existing Step struct" while also stating "retry/timeout_sec on hook steps are rejected at manifest validation." The Step struct has `retry` and `timeout_sec` as Option fields with serde defaults. Deserialization will happily accept them, so the rejection is purely a post-deserialize validation rule. This is fine architecturally, but the spec never states whether hooks inherit `manifest.defaults.retry`. If defaults include a retry policy, does the hook silently inherit it? The constraint says "hooks are not retried" but does not address inherited defaults explicitly.
**Evidence:** Constraint: "retry/timeout_sec on hook steps are rejected at manifest validation (hooks are not retried)". Manifest struct has `defaults: Option<Defaults>` which contains `retry: Option<RetryPolicy>`. runner.rs line 227-229 resolves effective retry by checking step then defaults.
**Recommendation:** Add a constraint or clarification: "Hooks do not inherit manifest-level defaults (retry, timeout). The execute_hook helper ignores defaults.retry." Alternatively, AC-07 verification should explicitly test that a hook step does NOT inherit a manifest-level retry default.

### [MEDIUM] AC-05 env var gap: on_exit during init_failed — what about ARC_EXIT_CODE?
**Category:** missing-requirement
**Pass:** 1
**Description:** Constraint 11 says on_exit receives ARC_PIPELINE_STATUS plus "ARC_FAILED_STEP and ARC_EXIT_CODE when status is 'failed'." But when status is 'init_failed', the init hook itself failed — should ARC_FAILED_STEP and ARC_EXIT_CODE reflect the init failure? The spec is silent on whether those vars are populated for the init_failed path. AC-05 verification tests "init failure; verify on_exit runs with ARC_PIPELINE_STATUS=init_failed" but doesn't specify whether ARC_FAILED_STEP is set.
**Evidence:** Constraint 11: "on_exit receives ARC_PIPELINE_STATUS ('success', 'failed', 'init_failed') plus ARC_FAILED_STEP and ARC_EXIT_CODE when status is 'failed'." Implicitly, init_failed path has no ARC_FAILED_STEP — but the init hook has a name. The omission should be made explicit.
**Recommendation:** Clarify constraint 11 with: "When status is 'init_failed', ARC_FAILED_STEP is set to the init hook's step name and ARC_EXIT_CODE is set to its exit code" OR "When status is 'init_failed', ARC_FAILED_STEP and ARC_EXIT_CODE are not set." The choice is a design decision — pick one and make it explicit so the implementer doesn't guess.

### [LOW] AC-10 verification delegates entirely to other ACs
**Category:** test-gap
**Pass:** 1
**Description:** AC-10 (execute_hook helper) has verification "Covered by ac-02 through ac-06 integration tests." This is not independently testable — if the helper's internal logic has a subtle bug in how it wraps errors, no dedicated test catches it. The helper is the core mechanism; it deserves at least one focused unit test verifying its fatal-vs-non-fatal branching.
**Evidence:** AC-10 verification field.
**Recommendation:** Add a dedicated unit test: "call execute_hook with fatal=true and a failing step; verify error propagates. Call with fatal=false and a failing step; verify Ok(()) returned and error printed." This provides regression safety independent of the integration tests.

### [MEDIUM] Assumption: 0016 env propagation is already implemented
**Category:** assumption
**Pass:** 2
**Description:** The spec and interview both state "injected via the same Command::envs() mechanism from 0016" and "Depends on 0016's env: &HashMap<String, String> parameter on the Engine trait." The Engine trait already has env parameters (confirmed in engine.rs), and runner.rs already resolves params and passes env_map. However, the card roadmap says 0016 is in the current sprint, and if it's not fully merged, the hook implementation could be blocked or built on unstable ground.
**Evidence:** Interview line: "Depends on 0016's env: &HashMap<String, String> parameter on the Engine trait for injecting failure context variables." Engine trait already has env params. runner.rs already builds env_map and passes it.
**Recommendation:** Confirm 0016 is fully merged before starting 0017 implementation. If it is (the code suggests it is), no action needed — but the spec should note this dependency as resolved rather than leaving it ambiguous.

### [LOW] Hook name collision detection scope unclear
**Category:** missing-requirement
**Pass:** 2
**Description:** AC-07 says "Hook step names must not collide with pipeline step names." What about collision between hooks themselves? E.g., if on_init and on_exit both have name "setup". The spec doesn't specify whether inter-hook name uniqueness is required.
**Evidence:** Constraint: "Hook step names must not collide with pipeline step names." No mention of inter-hook collision.
**Recommendation:** Add to constraint or AC-07: "Hook names must also be unique across all hooks (no two hooks may share a name)." This is the natural interpretation but should be explicit for the implementer.

### [LOW] Pipeline timeout interaction with hooks unspecified
**Category:** failure-mode
**Pass:** 2
**Description:** The current runner has pipeline-level timeout tracking (manifest.timeout_sec). If a pipeline times out mid-step, it returns PipelineTimeout error immediately. With hooks, what happens if the pipeline times out during on_exit or on_failure? Does the timeout apply to hook execution? If on_exit is cleanup-critical (e.g., releasing a lock), a timeout killing it could leave state dirty.
**Evidence:** runner.rs lines 188-196 check pipeline timeout before each step. Constraint 8 says "Hook failures (except on_init) are non-fatal" but timeout is not a hook failure — it's an infrastructure kill.
**Recommendation:** State explicitly: "Pipeline timeout does not apply to hook execution (hooks run outside the timeout boundary)" OR "Pipeline timeout applies to the full run including hooks — if a hook is interrupted by timeout, it counts as a hook failure (non-fatal for success/failure/exit hooks)." The first option is safer for cleanup semantics.

---

## Honest Assessment

This spec is well-structured, internally consistent, and demonstrates clear thinking about execution semantics. The constraints are detailed and the ACs are testable. However, three gaps need resolution before implementation: (1) the inherited defaults question for retry could cause a subtle production bug where hooks unexpectedly retry, (2) the init_failed env var ambiguity will force the implementer to make an undocumented design decision, and (3) pipeline timeout interaction with hooks is a real failure mode that needs a declared stance. None of these are architectural problems — they're edge case clarifications that take 5 minutes to resolve but could cost hours of rework if guessed wrong. Fix these and it's ready to build.
