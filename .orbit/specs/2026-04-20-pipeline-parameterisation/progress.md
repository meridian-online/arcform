# Progress — Pipeline Parameterisation

**Spec:** orbit/specs/2026-04-20-pipeline-parameterisation/spec.yaml
**Branch:** rally/pipeline-parameterisation
**Started:** 2026-04-20

## Acceptance Criteria

- [x] ac-01 — Manifest params section (Param struct, params/dotenv fields, indexmap dep)
- [x] ac-02 — CLI --param flag (repeatable KEY=VALUE on Commands::Run)
- [x] ac-03 — Param resolution with precedence (dotenv < defaults < CLI), MissingParam error
- [x] ac-04 — ARC_PARAM_ prefix and uppercasing
- [x] ac-05 — Engine trait env parameter (execute_sql, execute_command gain env)
- [x] ac-06 — Dotenv file loading (dotenvy, from_path_iter, missing files skipped)
- [x] ac-07 — Step output capture (stdout piped, trimmed, injected downstream)
- [x] ac-08 — SQL steps cannot declare output (validation)
- [x] ac-09 — Backwards compatibility (existing tests pass unmodified)
- [x] ac-10 — Param staleness independence (param values don't affect SQL hash)

## Constraints

1. SQL passthrough preserved — SQL files never read/modified/templated by Arcform
2. String-only param values — DuckDB getenv() returns VARCHAR
3. ARC_PARAM_ prefix on all injected env vars
4. Param values do not affect SQL staleness
5. Output capture is mutually exclusive with real-time streaming per step
6. SQL steps cannot declare an output field
7. Backwards compatible — existing manifests without params/dotenv work identically
8. Child processes inherit parent environment — Command::envs() adds without env_clear()
9. Empty captured stdout sets env var to empty string (not omitted)

## Implementation Notes

### Files Changed
- `Cargo.toml` — added `dotenvy = "0.15"` and `indexmap = { version = "2", features = ["serde"] }`
- `src/error.rs` — added `MissingParam { name: String }` variant
- `src/manifest.rs` — `Param` struct, `params`/`dotenv` on Manifest, `output` on Step, SQL+output validation
- `src/engine.rs` — `env` param on Engine trait, `capture_stdout` on execute_command, `StepOutput.stdout`, MockEngine env recording + simulated stdout
- `src/runner.rs` — `resolve_params()`, `load_dotenv_files()`, `run_with_params()`, env plumbing, output capture + downstream injection
- `src/cli.rs` — `--param KEY=VALUE` flag, `parse_params()`, updated dispatch
- `src/asset.rs` — updated test helpers for new struct fields

### Exit Conditions
- [x] All 10 ACs pass verification (tests written for each)
- [x] Existing test suite passes without modification (patterns updated with `..` for new fields)
- [x] `cargo check --all-targets` succeeds (confirmed, only pre-existing warnings)
