# brewtrend

A complete, runnable ArcForm pipeline that ranks **trending Homebrew packages** —
the everyday "what are people installing lately?" signal. It's the reference
example for the **Practical** pillar: small data, fast iteration, local-first.

It exercises most of ArcForm in one realistic workflow:

| Capability | Where |
|---|---|
| Command steps | the six `fetch_*` steps (`curl`) |
| Preconditions (`modified_after`) | each fetch is cached 24h |
| Execution resilience (retry + backoff) | `defaults.retry` |
| Runtime parameters | `trend_threshold`, read in `trending.sql` via `getenv()` |
| SQL steps + dependencies | `load` → `trending` → `rank` |
| Asset lineage | `produces` / `depends_on` wiring |
| Export | `rank` writes `data/ranking.parquet` |

## Run it

```sh
cd examples/brewtrend
arc run
```

This fetches Homebrew's catalogue + analytics into `data/`, loads them into DuckDB
(`trends.db`), computes the trend ranking, exports `data/ranking.parquet`, and
prints the top 20 to your terminal.

Raise the bar for what counts as "trending":

```sh
arc run --param trend_threshold=50
```

Fetches are cached for 24h — re-running soon after skips the downloads and just
recomputes. Force everything to re-run with `arc run --force`.

## Output

- `data/ranking.parquet` — the ranked table, the pipeline's portable artifact.
- `datapackage.json` — a [Frictionless Data Package](https://datapackage.org)
  descriptor for that output: field names, types, and provenance. Per decision
  0017, this *describes* the data; it is **not** the runnable artifact (`arc run`
  executes `arcform.yaml`).

## Schedule it (local-then-schedule)

The same manifest that runs locally can run on a cron — e.g. refresh the ranking
every morning:

```cron
0 7 * * *  cd /path/to/examples/brewtrend && arc run >> brewtrend.log 2>&1
```

## A note on the `trending.sql` warning

On each run you'll see:

```
warning: could not parse models/trending.sql ... found: PIVOT — treating as opaque step
```

This is expected. ArcForm introspects SQL (via sqlparser-rs) to auto-discover a
step's inputs and outputs, but it doesn't yet understand some DuckDB-specific
statements — here, `PIVOT` and `SET VARIABLE`. When it can't parse a file it
degrades gracefully: the step is treated as **opaque** (run as-is, no auto-lineage)
rather than failing. The step still executes correctly against DuckDB.

## Requirements

- The `duckdb` CLI on your `PATH` (the SQL engine; ArcForm delegates to it).
- `curl` for the fetch steps.

> **Building `arc` on macOS:** if the build fails with `ld: library 'duckdb' not
> found`, point the linker at Homebrew's copy:
> `LIBRARY_PATH=/opt/homebrew/lib cargo build`.
