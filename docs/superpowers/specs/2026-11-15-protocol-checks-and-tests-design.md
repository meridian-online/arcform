# Protocol checks, measures and tests

Design, 2026-11-15. Arcform builds it; nothing here is built yet.

## Intent

An analyst choosing a pipeline framework judges it by how much they have to write around it. Today a Protocol cannot be tested without Python beside it. Meridian's open-analytics code lists each carry a self-test harness of 225–677 lines that never calls `arc`: it hard-codes the manifest's step order, copies fixture files to where the fetch steps would write, runs the SQL through Python's DuckDB, and imitates the export and `uv` steps by hand. Checks inside a run already work, as `sql:` steps that call `error()`, but the run record cannot tell a check from any other step, and a passing check must create a dummy table only so that it produces something.

This design makes a Protocol testable, and its data checkable and measurable, with SQL and YAML alone. Most checks and tests will be written by agents and read by people when they fail, so a test earns its place only by guarding a material risk, and a failure must make sense to a person who did not write it.

## Goals

- **Checks.** A check is a SQL query that returns the rows that are wrong; no rows means it passes. It runs as a step in every run, with severity `error` (stops the run before anything publishes) or `warn` (the run carries on and reports it).
- **Measures.** A measure is a SQL query that returns one row of named numbers. Every run records its measures, and checks can compare them with the same measures in earlier runs.
- **Protocol tests.** `arc test` runs the Protocol's real steps against checked-in issuer files in place of its fetches, then asserts either that its checks pass or that the run fails with a given message.
- **One record.** The run record carries every check and measure in a standard shape, which the CLI, the MCP server, a webhook, CI and Brightfield all read.
- **Replace the harnesses.** The six open-analytics `test_*_models.py` files and `test_contracts.py` can be rewritten as Protocol tests and deleted, and the next code lists need no Python test file.

## Non-goals

- Anomaly detection built into arc. A check can compute a z-score in SQL over `measure_history`, but arc ships no statistics of its own.
- Per-service notification integrations (Slack, Teams, email) in arcform. A generic webhook covers them; email for managed Protocols belongs to the hosted runner.
- Showing results in Brightfield's window. That is Brightfield's own work and reads the run record this design defines.
- Emitting full OpenLineage events. The run record borrows OpenLineage's field names for checks; whether arc goes further is a separate review.
- Unit tests of a single SQL step against mocked input rows, as dbt and SQLMesh offer. Meridian's load steps read issuer files directly, so the unit worth testing is the Protocol run from those files.

## Anti-goals

- **No assertion language in YAML.** No `column X equals Y`, `not_null`, `accepted_values` or similar. dbt's generic tests drift this way; every check here is SQL.
- **No edit language for broken fixtures.** A broken input is made by SQL over the fixture as a table, or checked in as a file, never by a vocabulary of arc-specific edit verbs.
- **No warning nobody reads.** A warning reaches the analyst through the run's end-of-run summary, the webhook, CI and the run record.
- **No data rows in outgoing notifications.** Failing rows stay in the run record.

## Constraints

- arc is a local-first single binary. Tests and checks run on a laptop with no network, as well as on the hosted runner.
- SQL runs through the DuckDB command line, and arc reads each step's SQL to find what it reads and produces. Checks and measures go through the same reader.
- The run record's shape is versioned (`contract_version`, currently `b4/1`), and its published copy on R2 is public, so anything added to it obeys the redaction rules the publish step already applies.
- No cargo build runs while this is only a design.

## Approaches considered

How a check or measure is declared in a Protocol:

- **A. Checks and measures are steps** (`check:` and `measure:` beside `sql:`, `op:` and `command:`). Chosen. It is the smallest change from today (open-analytics' check steps change `sql:` to `check:`), a check that reads several steps' tables needs nothing special, and arc can refuse a manifest whose publish step runs before an error check on the tables it publishes. Its cost is a longer steps list.
- **B. Checks hang off the step they check**, as dbt tests sit under a model and Dagster's asset checks under an asset. It groups a check with its table but has to file a multi-table check under one of them.
- **C. Convention, no wiring:** every file in `checks/` or `measures/` runs as soon as the tables it reads exist. No wiring mistakes, but a reader of `arcform.yaml` cannot see the checks, against arc's principle that a Protocol is structurally transparent.

Hugh chose A.

How a refusal test gets its broken input:

- **A broken file checked in by hand** for each refusal. Simple, but one opaque binary per refusal and nothing a reviewer can read.
- **An edit described in the test**, applied by arc. Readable, but arc would need an editor per format and an edit vocabulary.
- **SQL over the fixture as a table, written back by DuckDB.** Chosen, with checked-in files where SQL cannot express the break. Hugh's preference was the described break, as the readable and deterministic option; SQL gives that without an edit vocabulary, since DuckDB both reads and writes xlsx, CSV and JSON.

## The design

### Checks

```yaml
- name: parents_exist
  check: checks/parents_exist.sql
  risk: an orphaned code publishes a broken hierarchy
  severity: warn          # optional; error by default
```

```sql
-- checks/parents_exist.sql
SELECT code, parent FROM soc_2018
WHERE parent IS NOT NULL AND parent NOT IN (SELECT code FROM soc_2018);
```

- A check file holds exactly one query; arc refuses a check file with any other statement in it. arc runs it, counts the rows, and keeps the first rows it returned (capped; the cap is the builder's to set, around 20 rows).
- `risk:` is required: one line saying what would go wrong in a published dataset if the check were missing. arc refuses a check step without one. A failure leads with it.
- A check produces nothing and changes nothing. Its reads come from arc's SQL reader, as any SQL step's do, so the asset graph knows what it checks.
- **Ordering rule.** arc refuses a manifest in which a step that publishes (`ducklake_write`, `ducklake_publish`) comes before an `error` check that reads, directly or through the graph, a table the publish step writes. A check placed after publication protects nothing.
- A failed `error` check fails the step, and the run stops as it does for any failed step, so nothing after it publishes. A failed `warn` check leaves the step succeeded; the run carries on and finishes green with warnings.
- Checks and measures are never skipped as fresh. They are cheap and their point is to run.
- `error()` calls inside load models stay as they are. Those are refusals: they stop a load the moment an issuer's file looks wrong, which is different from a check over a finished table.

A spot check of exact values is a check of the same shape:

```sql
SELECT * FROM (VALUES ('11-1011', 'Chief Executives')) AS expected(code, title)
EXCEPT SELECT code, title FROM soc_2018;
```

Longer expected outputs are a CSV beside it: `FROM 'expected/titles.csv' EXCEPT …`.

### Measures

```yaml
- name: match_rates
  measure: measures/match_rates.sql
```

```sql
-- measures/match_rates.sql
SELECT count(*) FILTER (WHERE status = 'confirmed') / count(*) AS confirmed_share,
       count(*) AS candidates
FROM crosswalk;
```

- A measure file holds one query returning exactly one row whose columns are all numeric; each column is a measure named `<step>.<column>`. arc refuses anything else.
- Every measure is recorded in the run record.
- **Built-in measures.** Each table a step produces is measured as soon as that step finishes, and recorded as `table:<table>.row_count` and `table:<table>.bytes`, so a check later in the run can compare them without a measure step. The `table:` prefix keeps them apart from step measures; a step name cannot contain a colon. Today arc measures tables only at the end of the run; this moves the measuring to each step's end, using DuckDB's own counts.
- An accuracy measure for a probabilistic step is a measure query against a checked-in file of labelled pairs; precision and recall are counted against those labels at a chosen threshold, as Splink's evaluation does.

### Comparing with earlier runs

Checks that run after a measure can read two tables arc provides:

- `measures`: this run's measures so far (`name`, `value`).
- `measure_history`: earlier runs' measures (`run_id`, `run_at`, `name`, `value`), newest last, over a window the Protocol sets once:

```yaml
measure_history: { runs: 30 }     # the default
measure_history: { since: 90d }   # for a fast-moving dataset
```

A comparison is then an ordinary check:

```sql
SELECT name FROM measures m JOIN (
  SELECT name, last(value ORDER BY run_at) AS prev FROM measure_history GROUP BY name
) p USING (name)
WHERE m.value < p.prev * 0.5;
```

History comes from the run records in the Protocol's runs folder (`build/.arcform/runs/`), counting only runs whose outcome was success, warnings included. A failed or partial run's measures are left out, so a comparison is always against data that passed its checks. A run with no earlier record gets an empty `measure_history`, and a comparison check over it returns no rows. On the hosted runner, the runner restores the Protocol's earlier published run records into that folder before it runs, so a hosted run compares against what readers actually got.

### The run record

`contract_version` moves to a new tag. Each check and measure step's entry gains a `check` or `measure` section, and the run gains a summary:

```json
"check": {
  "name": "parents_exist",
  "risk": "an orphaned code publishes a broken hierarchy",
  "severity": "warn",
  "success": false,
  "rows_returned": 3,
  "sample": [["11-9999", "11-9000"]],
  "sample_columns": ["code", "parent"]
}
```

```json
"measure": { "values": { "match_rates.confirmed_share": 0.47, "match_rates.candidates": 10433 } }
```

```json
"run": { "...": "...", "checks": { "passed": 12, "warned": 1, "failed": 0 } }
```

- The field names for a check follow OpenLineage's data-quality assertions facet (`name`, `success`, `severity`), so the shape matches a published standard. `expected`/`actual` are not used: a failing-rows check has no single expected value.
- The published copy of the record is public. Sample rows go through the same redaction as the rest of the record, and a Protocol can set `check_samples: false` to keep samples out of the published copy entirely (they stay in the local record).

### Telling the analyst

- **The end of `arc run`** lists every warning and failure: the check's name, its risk line, its row count and its first rows.
- **Exit codes.** 0 for a run that passed, with or without warnings; 1 for a failed run. `--fail-on-warn` makes warnings exit 1, for teams that want that.
- **`arc runs show <run> --checks`** prints a run's checks, sample rows and measures; an MCP tool returns the same as JSON, so an agent can read a failure and fix the Protocol.
- **Order at the end of a run.** arc writes the run record, then runs `on_exit`, then rewrites the record if the hook changed the outcome (a refused hook after a successful run still fails it, as today), then uploads the published copy if the run published, then sends the webhook. The webhook therefore always reports the final outcome and, when there is one, the published record's address.
- **Lifecycle hooks.** `on_exit` also receives `ARC_RUN_RECORD` (the record's path) and `ARC_CHECKS_STATUS` (`passed`, `warned` or `failed`), so anyone can write their own notification.
- **A webhook.**

```yaml
notify:
  webhook: ARC_NOTIFY_URL          # environment variable holding the URL
  secret: ARC_NOTIFY_SECRET        # optional; signs the payload
  on: [warn, fail]                 # default
```

The webhook rules, from the comparison with Airflow, Dagu, dbt, Dagster, Prefect, Kestra, Mage, Soda, OpenLineage and the CloudEvents and Standard Webhooks specs:

1. **Delivery never changes the run's outcome.** A bounded timeout (about 10 seconds), a few retries with backoff on 429 and 5xx, no redirects followed. Whether it was delivered is recorded in the run record.
2. **The URL is a credential.** It comes from the environment (or the manifest's dotenv files) and is never printed in a log, error, report or run record.
3. **Signed with the Standard Webhooks scheme** when `secret` is set: `webhook-id`, `webhook-timestamp` and `webhook-signature` headers, HMAC-SHA256 over the id, timestamp and body together. Receivers use the published libraries.
4. **A thin, versioned payload:** `{"version": "v1", "run_id", "protocol", "outcome", "checks": {"passed", "warned", "failed"}, "failed_checks": [{"name", "severity", "risk", "rows_returned"}], "record": <published record address, if any>}`. No data rows. No per-service body templates in v1.
5. **Egress filtering on the hosted runner** (private, link-local and cloud-metadata addresses blocked after DNS resolution) is the runner's work, not arcform's; a laptop is unrestricted.

### Protocol tests

One file per Protocol, `tests.yaml` beside `arcform.yaml`:

```yaml
- name: a moved definitions header is refused
  risk: BLS moves the header row and titles load into the wrong columns
  fixtures:
    fetch_definitions: testdata/soc_definitions.xlsx
  break:
    fetch_definitions: UPDATE fixture SET B = 'Code' WHERE row = 8
  expect:
    fails_with: header row has moved

- name: every parent code exists
  risk: an orphaned code publishes a broken hierarchy
  fixtures:
    fetch_definitions: testdata/soc_definitions.xlsx
  checks:
    - SELECT code FROM soc_2018 WHERE parent NOT IN (SELECT code FROM soc_2018)
  params:
    release: "2018"
```

- **`risk:` is required** on every test, as on every check step, and leads its failure output.
- **`fixtures:`** maps a step's name to a checked-in file that stands in for what that step would write. The step does not run. A step that writes several outputs maps each output path to a file instead.
- **A test never fetches data.** The operator catalogue gains an `ingress` flag on the operators that fetch data (`http_fetch`, `opendal_fetch`, `gleif_ra_fetch`, `html_link_discover`); an ingress step with no fixture refuses the test, naming the step. A `sql:` step that reads a remote address (`FROM 'https://…'`, which arc already classes as a remote asset) refuses the test the same way. Publishing steps never run in a test. Every other step runs as written, `uv` and `command:` steps included, since the open-analytics harnesses had to fake exactly those steps by hand; a tool such as `uv` may still download what it needs to run, but no issuer data comes from the network.
- **`break:`** maps a fixture to one or more SQL statements. arc reads the fixture into a table named `fixture` (for xlsx: every cell as text, no header, a `row` column and one column per sheet letter; for CSV and JSON, DuckDB's reader), runs the statements, and writes the table back in the same format with DuckDB's writer. A format SQL cannot express (a zip archive, an encoding change, an old .xls) uses a second checked-in fixture instead.
- **A control for every break.** A test with `break:` also runs once with the same round trip and no statements, and that run must not fail with the expected message. Agents will not remember to write one, so arc does it; it proves a refusal comes from the break and not from the rewrite.
- **`expect: fails_with:`** asserts the run fails and the failure message contains the text. With no `expect:`, the test asserts the run succeeds, every `error` check passes, and every check listed under `checks:` returns no rows. Test checks are inline SQL of the same failing-rows shape.
- **`params:`** overrides the Protocol's parameters for the test.
- **`measure_history:`** in a test names a CSV (`run_id, run_at, name, value`) that stands in for earlier runs, so a comparison check can be shown to fail. Without it, a test's history is empty.
- **How it runs.** `arc test` copies the Protocol into a temporary directory with its own database, places fixtures, applies breaks, runs the real steps in manifest order (in-run checks included), then the test's checks. Tests run one after another; `arc test <name>` runs one.
- **Output.** One line per passing test. A failing test prints its name, its risk line, and what happened against what was expected:

```
FAIL a moved definitions header is refused
  risk: BLS moves the header row and titles load into the wrong columns
  expected the run to fail with "header row has moved"; it succeeded
```

- `arc test --junit <file>` writes JUnit XML for CI, each failure's message led by its risk line. Exit code 1 when any test fails.

### What changes in open-analytics, for scale

- The nine check files become `check:` steps, one rule per file, with the `SELECT error(…)` wrapping and the `contract_*_result` tables removed; the shared `contracts/check_titles.sql` and `check_crosswalk.sql` each split into one file per rule.
- Each `test_*_models.py` becomes a `tests.yaml` over the fixtures already in `scripts/testdata/`, and its `RefusalTest` cases become `break:` tests.

That migration is open-analytics' own work after this build lands.

## Calls made on Hugh's behalf

- `risk:` is required on every check step and every test, and arc refuses one without it.
- A check file holds exactly one query, and each rule gets its own file.
- Checks are never skipped as fresh.
- The ordering rule: a publish step before an `error` check on its tables is refused.
- arc runs a control for every `break:` test automatically.
- An ingress step without a fixture refuses the test, so tests never reach the network.
- Measure names are `<step>.<column>`; each produced table is measured at its step's end as `table:<table>.row_count` and `table:<table>.bytes`.
- `measure_history` counts only successful runs (warnings included), and is empty for a Protocol's first run.
- Measure steps, like checks, are never skipped as fresh.
- Tests never fetch data: fetch operators carry an `ingress` flag, and they and remote SQL reads refuse a test without a fixture; `uv` and `command:` steps run.
- A test can supply earlier runs' measures from a CSV.
- The end of a run writes the record, runs `on_exit`, rewrites the record if the outcome changed, uploads, then sends the webhook.
- On the hosted runner, earlier published records are restored into the runs folder before the run.
- Sample rows can be kept out of the published record with `check_samples: false`.
- Warnings exit 0 unless `--fail-on-warn`.
- No per-service body templates in the webhook's v1.

## Left to the builder

- The sample-row cap and how a sample is truncated.
- The exact new `contract_version` tag and the run record's field order.
- The xlsx `fixture` table's column naming beyond sheet letters, and how a workbook's other sheets survive a round trip.
- How `arc test`'s temporary directory and database are laid out and cleaned up.
- The MCP tool's name and the `arc runs show` output layout.
- Retry counts and backoff for the webhook, within the rules above.

## Work that follows elsewhere

- The hosted runner: restore earlier run records before a run, email a managed Protocol's owner on warnings and failures, and filter webhook egress.
- Brightfield: show check results and measures in the window beside the steps they check.
- open-analytics: migrate the check files and the self-test harnesses; build NAICS, FIPS and OurAirports with Protocol tests.
- A review of what the stack can take from OpenLineage beyond the field names used here.
