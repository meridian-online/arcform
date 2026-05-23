# 0015 Execution Resilience — Decision Pack

## Decision 1: Backoff Strategy

**Context:** Retry needs a delay between attempts. The design space ranges from fixed delay to exponential with jitter.

**Options:**
- A. Exponential only — `backoff_sec * 2^(attempt-1)`. Simple, effective.
- B. Configurable strategy — `backoff: fixed | exponential | exponential_jitter`
- C. Fixed delay only — always wait `backoff_sec` between attempts

**Trade-offs:**
- A: Covers 95% of cases. No config complexity. Fixed delay is the degenerate case (max_attempts=2, accept the doubling).
- B: Maximum flexibility but adds config surface. Jitter is irrelevant for a local-first tool (no thundering herd).
- C: Too simple — exponential is standard for API retries and costs nothing extra.

**Recommendation:** A — exponential only. No jitter needed locally. Add a `backoff:` discriminator later if demand emerges.

## Decision 2: Retry + Preconditions Interaction

**Context:** When a step is retried, should preconditions be re-evaluated?

**Options:**
- A. Preconditions evaluate once before the first attempt — freshness gate only
- B. Preconditions re-evaluate on each retry — readiness probe
- C. Configurable per step

**Trade-offs:**
- A: Clean separation — preconditions answer "should this run?", retries answer "it failed, try again". Simple mental model.
- B: Could catch cases where a transient condition changes (e.g. file appears), but conflates two concerns.
- C: Maximum flexibility, maximum config surface.

**Recommendation:** A — evaluate once. Preconditions and retries are separate concerns.

## Decision 3: Exit Code Filtering

**Context:** The card scenarios include retrying only on specific exit codes. This allows distinguishing transient from permanent failures.

**Options:**
- A. Drop for v1 — retry on any non-zero exit code
- B. Include `exit_codes` field — retry only on listed codes, halt on others

**Trade-offs:**
- A: Simpler config. Trades precision (permanent failures waste retry attempts) for simplicity. Most pipelines have 1-2 retryable steps.
- B: Precise but adds config complexity for a niche case. Requires users to know their tools' exit codes.

**Recommendation:** A — drop for v1. Agreed simplification from the prior design review. Add later if brewtrend or other reference pipelines demonstrate the need.

## Decision 4: State Tracking for Retries

**Context:** How should retries appear in the state backend? Per-attempt rows or final-outcome only?

**Options:**
- A. Final outcome only — one row per step, with a `total_retries` count for observability
- B. Per-attempt rows — each retry gets its own state record

**Trade-offs:**
- A: Simple staleness queries. Retry count is still visible via `_arcform_runs.total_retries`.
- B: Full audit trail but complicates "is this step stale?" queries and bloats state tables.

**Recommendation:** A — final outcome only. Staleness queries stay simple; retry count is captured for observability.

## Decision 5: Timeout Implementation

**Context:** Need to kill subprocess on timeout. Rust's `Child::wait()` blocks indefinitely.

**Options:**
- A. Polling loop — `child.try_wait()` with ~100ms sleeps, check elapsed time against `Instant` deadline. On timeout, `child.kill()`.
- B. Threaded approach — spawn a timer thread that kills the child on expiry
- C. Async runtime — use tokio timeout

**Trade-offs:**
- A: Simple, no new dependencies. 100ms granularity is fine for timeouts measured in seconds/minutes. SIGKILL handles DuckDB CLI (single process).
- B: Thread management overhead for a simple timeout. Risk of race conditions.
- C: Would require adding an async runtime to a synchronous codebase — massive scope creep.

**Recommendation:** A — polling loop. Simplest approach, no new dependencies, adequate granularity.
