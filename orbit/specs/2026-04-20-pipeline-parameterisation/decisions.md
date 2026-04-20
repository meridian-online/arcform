# 0016 Pipeline Parameterisation — Decision Pack

## Decision 1: Parameter Value Types

**Context:** Params could support typed values (string, number, bool) or be string-only. DuckDB's `getenv()` returns VARCHAR regardless.

**Options:**
- A. String-only values — all params are strings; numbers/bools passed as string representation
- B. Typed values with validation — parse as YAML types, validate at manifest load
- C. String with type hints — string storage but optional `type:` field for documentation

**Trade-offs:**
- A: Simple, matches DuckDB `getenv()` return type, no false type safety. Loses compile-time type checking.
- B: Richer validation but adds complexity; DuckDB still sees VARCHAR. Risk of mismatched expectations.
- C: Documentation value but no enforcement — worst of both worlds.

**Recommendation:** A — string-only. Matches the engine's reality. Type coercion is the SQL layer's job.

## Decision 2: Environment Variable Prefix

**Context:** Params become env vars on child processes. Need to avoid collisions with system vars (PATH, HOME) and make the contract explicit.

**Options:**
- A. `ARC_PARAM_` prefix — param `start_date` becomes `ARC_PARAM_START_DATE`
- B. Bare names — param `start_date` becomes `START_DATE`
- C. `ARC_` prefix — shorter but risks collision with future arc-internal vars

**Trade-offs:**
- A: Unambiguous, no collision risk, slightly verbose in SQL (`getenv('ARC_PARAM_START_DATE')`)
- B: Clean in SQL but dangerous — `HOME`, `PATH`, `USER` would be overwritten
- C: Shorter but `ARC_EXIT_CODE` (lifecycle hooks) would collide with params namespace

**Recommendation:** A — `ARC_PARAM_` prefix. Safety over brevity.

## Decision 3: Staleness Interaction

**Context:** When params change between runs (e.g. different `--param start_date`), should SQL steps be considered stale?

**Options:**
- A. Params don't affect staleness — SQL file hash is the only staleness signal; use `--force` to re-run
- B. Param hash included in staleness — changing any param makes all steps stale
- C. Per-step opt-in — a `param_hash` precondition type lets individual steps opt into param-sensitive staleness

**Trade-offs:**
- A: Simple, matches "SQL file is the definition" mental model. User must remember `--force` when params change.
- B: Safe but aggressive — changing `debug=true` re-runs everything
- C: Flexible but adds a new precondition type (scope creep for this card)

**Recommendation:** A — params don't affect staleness. Params are runtime context, not pipeline definition. The SQL hasn't changed. Option C is a good future addition but out of scope.

## Decision 4: Output Capture vs Real-Time Streaming

**Context:** Steps currently stream stdout to the terminal. Output capture needs to pipe stdout instead of inheriting it. These are mutually exclusive per step.

**Options:**
- A. Capture disables streaming — output-capturing steps are silent; downstream value is the priority
- B. Tee approach — capture AND stream simultaneously via a tee pipe
- C. Capture with echo — capture stdout, then echo it after the step completes

**Trade-offs:**
- A: Simple, clean separation. User loses real-time visibility for capturing steps.
- B: Complex pipe management; risk of deadlocks or buffering issues
- C: Delayed output — user sees nothing during execution, then a dump at the end

**Recommendation:** A — capture disables streaming. Clean and simple. Capturing steps are typically short (e.g. `wc -l`) where real-time output isn't valuable.

## Decision 5: Dotenv Loading Semantics

**Context:** Multiple dotenv files may overlap; CLI params and manifest defaults also contribute. Need a clear precedence order.

**Options:**
- A. Last-wins layered: process env → dotenv files (in order) → manifest defaults (fill gaps) → CLI params (override all)
- B. First-wins: CLI params → manifest defaults → dotenv → process env
- C. Strict isolation: dotenv and params are separate namespaces

**Trade-offs:**
- A: Intuitive — CLI always wins, dotenv is the baseline, defaults fill gaps. Matches common dotenv conventions.
- B: Unusual — most tools use "later overrides earlier" for layered config
- C: Clean separation but users would need to learn two systems

**Recommendation:** A — last-wins layered. CLI params have highest priority; dotenv provides a baseline; manifest defaults fill gaps only.
