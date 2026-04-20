# Spec Review

**Date:** 2026-04-18
**Reviewer:** Context-separated agent (fresh session)
**Spec:** orbit/specs/2026-04-20-pipeline-parameterisation/spec.yaml
**Verdict:** APPROVE

---

## Review Depth

```
| Pass | Triggered by | Findings |
|------|-------------|----------|
| 1 — Structural scan | always | 2 |
| 2 — Assumption & failure | content signals (Engine trait cross-cutting change, manifest format) | 1 |
| 3 — Adversarial | not triggered | — |
```

## Prior Review Status

This is a re-review following REQUEST_CHANGES in v1 (review-spec-2026-04-20.md). All 7 findings from v1 have been addressed:

- **indexmap dependency** (LOW): AC-01 now explicitly requires `indexmap` with serde feature.
- **Engine trait error variant** (MEDIUM): AC-05 clarifies StepExecution catches env propagation; AC-03 defines `Error::MissingParam`.
- **stderr behaviour for capturing steps** (MEDIUM): AC-07 now states "Stderr remains inherited (streams to terminal)."
- **Command::envs() additive assumption** (MEDIUM): Constraint 8 codifies the no-env_clear invariant.
- **Empty stdout capture semantics** (MEDIUM): Constraint 9 and AC-07 both specify empty-string-not-omitted.
- **AC-10 test pattern** (LOW): Verification now matches codebase MockEngine call-count pattern.
- **Silent dotenv skip** (LOW): Kept as-is -- acceptable UX choice for v1 of the feature.

## Findings

### [LOW] AC-07 does not specify trimming behaviour beyond trailing newline
**Category:** assumption
**Pass:** 1
**Description:** AC-07 says stdout is "trimmed of trailing newline." The interview also describes this as "single trailing newline stripped, like bash `$(...)` ." However, bash command substitution strips _all_ trailing newlines, not just one. If a command outputs `"foo\n\n\n"`, bash `$(...)` produces `"foo"`, while "trimmed of trailing newline" (singular) would produce `"foo\n\n"`. The spec should pick one interpretation. Given the interview explicitly says "like bash `$(...)`", stripping all trailing newlines is likely the intent, but the AC wording says "trailing newline" (singular).
**Evidence:** AC-07 line: "trimmed of trailing newline." Interview line: "single trailing newline stripped, like bash `$(...)`."
**Recommendation:** Minor wording clarification. Not blocking -- implementer can follow the interview's bash analogy. If the intent is truly single-newline-only, document why the deviation from bash semantics.

### [LOW] No explicit constraint on param name character set
**Category:** missing-requirement
**Pass:** 1
**Description:** The spec does not restrict which characters are valid in param names. A param name like `start date` (with space) or `key=value` (with equals) would produce env var `ARC_PARAM_START DATE` or `ARC_PARAM_KEY=VALUE`, which are invalid or confusing on most platforms. The CLI `--param` parsing splits on first `=`, so `--param key=value=extra` correctly yields key `key` / value `value=extra`, but the key itself is not validated.
**Evidence:** AC-02 describes parsing but not key validation. AC-04 describes uppercasing but not character filtering.
**Recommendation:** Not blocking for v1 -- the surface area is small (manifest authors control param names) and invalid env var names will fail at the OS level with a clear error from `Command::envs()`. Consider adding name validation (e.g. `[a-zA-Z_][a-zA-Z0-9_]*`) in a follow-up.

---

### [LOW] Assumption: dotenv file paths are relative to manifest directory
**Category:** assumption
**Pass:** 2
**Description:** AC-06 and the interview describe dotenv file loading with paths like `.env` and `.env.local`, but the spec does not explicitly state the base directory for these paths. The manifest is loaded via `Manifest::load(dir)` where `dir` is `cwd`. Dotenv paths are almost certainly relative to the manifest directory (matching `dotenvy::from_path_iter(dir.join(path))`), but this is an unstated assumption.
**Evidence:** AC-06 says "create temp .env file with KEY=VALUE; verify resolve_params reads it." The interview YAML shape shows `dotenv: [.env, .env.local]` without qualifying the base path. `Manifest::load` uses `dir` (the current working directory) as the base for all relative paths (db_path, sql files).
**Recommendation:** Not blocking -- the codebase pattern is clear and the implementer will naturally resolve relative to `dir`. A one-line constraint ("dotenv paths are relative to the manifest directory") would be a nice addition but is not required.

---

## Honest Assessment

The spec is ready for implementation. All MEDIUM findings from the v1 review have been incorporated as explicit constraints or AC clarifications, demonstrating a careful revision cycle. The remaining findings are all LOW severity -- minor wording ambiguities and edge cases that the implementer can resolve during development without spec rework. The design is sound: env vars as the transport layer, ARC_PARAM_ prefix for namespace isolation, dotenvy for file loading, and explicit precedence ordering. The Engine trait signature change (AC-05) is the highest-risk area due to its cross-cutting nature, but the spec handles it well by specifying MockEngine behaviour and confirming the existing StepExecution error variant covers propagation failures. The 10 ACs are specific, testable, and map cleanly to the existing codebase patterns.
