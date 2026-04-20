# 0016 Pipeline Parameterisation — Design Interview

## Context

Arcform pipelines currently have no way to accept runtime input. The same manifest always does the same thing. Card 0016 adds named parameters (with defaults), dotenv file loading, and step output capture — all flowing through environment variables to preserve SQL passthrough (decision 0003).

Key source files: `runner.rs` (step execution), `manifest.rs` (YAML parsing), `engine.rs` (subprocess spawning), `cli.rs` (Clap CLI).

Architectural constraint: **SQL files are never read, modified, or templated by Arcform.** Parameters reach SQL via environment variables and DuckDB's `getenv()` function.

## Q&A

**Q: What types do params support?**
A: String-only. DuckDB `getenv()` returns VARCHAR, so keeping params as strings avoids false type safety. Numbers and booleans are passed as their string representation.

**Q: How does `--param key=value` work with Clap?**
A: Repeatable `--param` flag as `Vec<String>`. Parsing splits on the first `=` in the runner. No `--param-file` in v1 — dotenv covers file-based loading.

**Q: How do params become env vars? What prefix?**
A: All params are set as env vars with the prefix `ARC_PARAM_` on the child process. Param `start_date` becomes `ARC_PARAM_START_DATE`. Prefix prevents collision with system env vars and makes the contract explicit. Applied via `Command::envs()` which adds to (does not replace) inherited environment.

**Q: Dotenv loading order?**
A: Last wins, in this order:
1. Process environment (inherited)
2. Dotenv files (in declared order, later files override earlier)
3. Manifest `params` defaults (fill gaps only — don't override dotenv)
4. `--param` CLI flags (highest priority, override everything)

Missing dotenv files are silently skipped (common for `.env.local`).

**Q: How does output capture work? Does it conflict with real-time streaming?**
A: Step field `output: <var_name>` captures stdout to a named env var. For capturing steps, stdout is piped instead of inherited — output is captured, not streamed. After completion, the trimmed value (single trailing newline stripped, like bash `$(...)`) is injected as `ARC_PARAM_<VAR_NAME>` into the env map for downstream steps. SQL steps cannot produce output — validate this at manifest load.

**Q: Do param values affect staleness?**
A: No. Params are runtime context, not pipeline definition. The SQL file hasn't changed. Running with different `--param` values doesn't make steps stale — use `--force` to re-run. A `param_hash` precondition type could be added later but is out of scope.

**Q: Which dotenv crate?**
A: `dotenvy` (maintained fork of `dotenv`). Use `from_path_iter()` to parse without setting process env, then merge manually.

## Summary

### Goal

Named runtime parameters with defaults, dotenv file loading, and step output capture. All values flow as environment variables, preserving SQL passthrough.

### Constraints

- SQL passthrough preserved — no template substitution in SQL files
- String-only param values (DuckDB `getenv()` returns VARCHAR)
- `ARC_PARAM_` prefix on all env vars
- Param values do not affect staleness
- Output capture mutually exclusive with real-time streaming per step
- SQL steps cannot declare `output`

### Success Criteria

1. `--param key=value` sets `ARC_PARAM_KEY` as env var on child processes
2. Manifest `params` section declares named params with optional defaults; missing required params produce a clear error
3. Dotenv files load in declared order; CLI params override everything
4. Command step `output` field captures stdout and injects it as `ARC_PARAM_*` for downstream steps
5. SQL steps access params via `getenv('ARC_PARAM_*')` — SQL files unchanged
6. Existing manifests without params/dotenv work identically (backwards compatible)

### Decisions Surfaced

- **String-only params** — matches DuckDB `getenv()` return type, avoids false type safety
- **`ARC_PARAM_` prefix** — prevents collision, makes the contract explicit
- **Params don't affect staleness** — runtime context, not pipeline definition; use `--force`
- **Output capture disables streaming** — can't both pipe and inherit stdout; capturing step output is silent

### YAML Shape

```yaml
dotenv:
  - .env
  - .env.local

params:
  start_date:
    default: "2026-01-01"
  end_date:
    default: "2026-01-31"
  environment:              # no default = required

steps:
  - name: fetch
    command: "curl -o data.csv https://api.example.com/data?since=${ARC_PARAM_START_DATE}"
    output: row_count

  - name: transform
    sql: models/transform.sql
    # SQL: SELECT * FROM read_csv('data.csv') WHERE date >= getenv('ARC_PARAM_START_DATE')
```

### Structs

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Param {
    #[serde(default)]
    pub default: Option<String>,
}
```

### Files That Change

| File | Change |
|------|--------|
| `manifest.rs` | Add `params: IndexMap<String, Param>`, `dotenv: Vec<String>` to Manifest; add `output: Option<String>` to Step; `Param` struct; validation |
| `cli.rs` | Add `params: Vec<String>` to `Commands::Run`; parse key=value pairs |
| `engine.rs` | Add `env: &HashMap<String, String>` param to Engine trait methods; apply via `Command::envs()` |
| `engine.rs` | Add `stdout: Option<String>` to `StepOutput`; `capture_stdout` param on `execute_command` |
| `runner.rs` | New `resolve_params()` function; pass env map to engine; handle output capture + downstream injection |
| `Cargo.toml` | Add `dotenvy` dependency |
