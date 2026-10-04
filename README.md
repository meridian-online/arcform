# Arcform

> Local-first data pipeline engine for analytical workflows.

Arcform is a Rust-based workflow engine built for data analysts who want the power of a structured pipeline without the overhead of cloud infrastructure. It runs as a single binary, orchestrates multi-stage dataflows, and understands the internal structure of its steps — not just whether they succeeded.

---

## Design Principles

**Local-first.** Arcform runs on your machine: no managed services, no cloud accounts, no ops overhead. What it does need is a DuckDB CLI, the one `ARC_DUCKDB_BIN` names or else `duckdb` on `PATH` (see [Engine](#engine)), and whatever binaries your steps invoke.

**Asset-aware.** Inspired by Dagster's software-defined asset model, Arcform treats data outputs — not tasks — as the primary unit of work. The pipeline graph reflects data dependencies, not just execution order.

**Structurally transparent.** A SQL step is not a black box. Using the DataFusion SQL parser, Arcform can inspect and decompose queries into their constituent parts — load operations, CTE dependencies, and export targets — enabling fine-grained lineage and partial re-execution.

**Composable by design.** Pipelines are defined in YAML and composed from discrete, reusable steps. Each stage has a clear input contract and output contract.

---

## Execution model

Arcform runs your steps on your machine, as you. There is no sandbox and no container — **running a Protocol you did not write is running a shell script you did not read.** This is deliberate; the table names the fields it covers.

| Field | How Arcform runs it |
| --- | --- |
| `command:` on a step | `sh -c <command>` |
| `command:` on a precondition | `sh -c <command>`, evaluated while Arcform works out what is stale — so it runs even when the step it guards is then skipped |
| `command:` on a hook (`on_init`, `on_success`, `on_failure`, `on_exit`) | `sh -c <command>`, the same path as a step |
| `sql:` on a step or hook | passed to the DuckDB CLI arc was told to run or found on `PATH` (see [Engine](#engine)), which reads and writes whatever the SQL directs |
| `op:` on a step | a catalog operator: in this process — which may itself spawn a tool (`datapackage_describe` runs `finetype`) — or `uv run --script` on a script embedded in the `arc` binary, which may spawn tools of its own too. No shell, and no confinement either |

Each of those inherits the environment Arcform assembled, including the `ARC_PARAM_*` values from `params:`, your dotenv files and `--param` — and the stdout of any earlier step that declared `output:`, which is captured into `ARC_PARAM_<OUTPUT>` for everything that runs after it. `arc registry run` fetches a Protocol and runs it through the same path, so read one before you run it.

### Engine

Arcform runs SQL through the DuckDB CLI, and finds it in this order:

1. `ARC_DUCKDB_BIN`, when set, names the executable. A program that ships Arcform sets it, because an app started from the desktop does not see the `PATH` a terminal does. A set value is final: if it names something that is not an executable file, the run is refused with the variable and the path in the message, and `PATH` is not tried instead.
2. Otherwise, `duckdb` on `PATH`.

The version preflight and the SQL steps run the same executable. A `command:` step that calls `duckdb` by name still gets the one on `PATH`; Arcform does not rewrite commands.

Arcform is tested on DuckDB `>=1.3, <2`, and a run with a SQL step on any other version is refused before a step runs. The oldest version it accepts is 1.3.0, the first release whose command line exits non-zero when a statement in a SQL step fails; on 1.2.x it exits 0, and Arcform learns that a step failed from that exit status alone. A Protocol's `engine_version:` narrows that range and cannot widen it: `">=1.6"` refuses 1.5 as well, `">=1.2"` still refuses 2.0, and `">=1.2"` on DuckDB 1.2 is refused by Arcform's own range. Set `ARC_ALLOW_UNTESTED_ENGINE=1` to run on an engine outside Arcform's range at your own risk; the run prints a warning naming the version and the range, on 1.2.x saying that a failed SQL step is reported as passed, and a Protocol's own `engine_version:` still applies. A development build such as `1.6.0-dev` is outside the range. When Arcform cannot read the engine's version it warns and runs.

A Protocol's SQL may install a DuckDB community extension only when the extension is on the [vetted list](docs/VETTED_EXTENSIONS.md). Before a step runs, Arcform reads the SQL file of every step and hook as the DuckDB CLI reads it with its default parser, and refuses a Protocol whose SQL installs a community extension off that list, installs one from an address or another repository, or names the switch to its second parser, which reads comments and strings by other rules. It reads each step's and hook's own SQL file again just before that step or hook runs, and before each attempt of a step with `retry:`, so a file an earlier step writes over, or writes where there was none, is checked too: a step refused there fails with exit 2 and names itself in `ARC_FAILED_STEP`, the steps before it keep their tables, and a pinned extension first found there is checked against its pin there. It reads the SQL as written and confines nothing: a `command:` step, or a dot command such as `.shell` or `.read` in a SQL file, can run a DuckDB of its own, a process an earlier step leaves running can write over a step's file after Arcform has read it, SQL held in a database the step reads, such as a view's body, is not read, and SQL that a step builds while it runs, or writes to a file other than a step's or hook's own SQL file, is not read, though Arcform warns and runs the step when its SQL calls `query()` or `json_execute_serialized_sql()` on anything but a string literal, imports a database, or names `.duckdbrc`.

A Protocol pins the build of each community extension it installs under the `extensions:` key of `arcform.yaml`, beside `engine_version:`: extension name, then the DuckDB version as DuckDB prints it (`v1.5.5`), then the platform as `PRAGMA platform` prints it (`linux_amd64`), then the SHA-256 of the installed file as 64 lower-case hex digits. Before any step or hook runs, `arc run` installs each pinned extension the Protocol's SQL installs `FROM community`, on the DuckDB the steps run on and without loading it, and refuses the run when the installed file's hash differs from its pin, naming `arc upgrade <name>`, or when the install fails or names no file. An extension with no pin for the engine's DuckDB version and platform draws a warning naming the `extensions:` key and `arc upgrade <name>`, and runs. `arc run` does not write a pin. `arc upgrade <name>` writes or replaces the pin of an extension on the vetted list for the machine's DuckDB version and platform, from the build the community registry serves: it runs `FORCE INSTALL` and `LOAD` of that extension on the DuckDB the steps run on and writes the installed file's SHA-256, keeping every other byte of `arcform.yaml`, with the file as it was left as an `arc history` checkpoint. An author runs it on each platform a Protocol runs on. A step can replace a pinned file during the run with `FORCE INSTALL`, `UPDATE EXTENSIONS` or a `command:`; arc hashes each pinned file again when the run ends and fails the run on a change, by which time the steps after that one may have loaded the file. An arc older than this change ignores the `extensions:` key and runs a pinned Protocol without checking it. A step that sets `extension_directory`, `extension_directories` or `home_directory` is refused. The [vetted list](docs/VETTED_EXTENSIONS.md#the-build-each-extension-is) gives the details.

---

## Pipeline Stages

Arcform pipelines are organised into four stage types, reflecting the natural flow of an analytical workflow.

### Pre-SQL
Preparation steps that operate outside the database.

- File retrieval via `curl`
- JSON and YAML transformation via `jq` / `yq`
- Data validation via JSON Schema (integrated with FineType)
- Remote storage sync via `rclone`

### SQL
Structured query steps executed against DuckDB.

- Data loading via DuckDB `read_*` functions
- Data modelling with standard SQL and CTEs
- Data export via DuckDB `COPY` functions

### Advanced Analytics
Compute-intensive steps for ML workflows.

- Vector embedding generation
- CatBoost model training and inference

### Export and Activation
Output steps that deliver results beyond the database.

- File exports via DuckDB `COPY`
- Remote sync via `rclone`
- Chart rendering
- Markdown report generation

---

## Architecture

Arcform models each pipeline as a directed acyclic graph (DAG) of data assets. Edges in the graph represent data dependencies between assets, not just task sequencing.

This distinction matters: when an upstream asset changes, Arcform knows which downstream assets are stale and can trigger selective re-materialisation rather than a full pipeline re-run.

For SQL steps, the DataFusion SQL parser provides structural introspection — allowing Arcform to surface CTE-level dependencies and treat individual load and export operations as discrete graph nodes.

---

## Relationship to the Meridian Ecosystem

Arcform is part of the [Meridian](https://github.com/meridian) project family, alongside [FineType](https://github.com/meridian/finetype).

- **FineType** classifies and validates text data types, providing a transformation contract from raw text to typed DuckDB expressions.
- **Arcform** orchestrates the pipelines in which that data flows — from ingestion through modelling to output.

The two libraries are designed to complement each other. FineType's JSON Schema validation integrates directly into Arcform's Pre-SQL stage, enabling data quality checks as a first-class pipeline step.

---

## The log in a Protocol's folder

Each version arc's local history records of `arcform.yaml`, or of a file beside it such as a chart file under `panels/`, adds one line to `arcform-log.txt` in the Protocol's folder. The snapshots themselves stay outside the Protocol, in `$ARCFORM_HISTORY_DIR` or `~/.arcform/history`, where `arc history` lists, shows and restores them on the machine that recorded them. The log goes with the folder, through git, a synced drive or an archive, and says which file was recorded, when, through which interface and with which steps. A line holds no contents of any file. The ignore list `arc create-protocol` and `arc init` write does not name the log, so `git add` stages it beside `arcform.yaml`, and `arc history list` ends by saying where it is.

Each line is one JSON object, so the log is newline-delimited JSON that any JSON reader parses line by line:

```json
{"at":"2026-10-03T07:15:02.123Z","file":"arcform.yaml","kind":"save","interface":"app","version":"1791011702123-000-save","step":"big_orders","change":"changed"}
```

At that time `arcform.yaml` was saved in the app, as the version `arc history list` prints with that id, and the save changed the step `big_orders`. A save that touched several steps names each of them in one line:

```json
{"at":"2026-10-03T07:16:40.456Z","file":"arcform.yaml","kind":"save","interface":"app","version":"1791011800456-000-save","steps":[{"step":"tally","change":"changed"},{"step":"big_orders","change":"added"}]}
```

The keys:

- `at`: the time the version was recorded, RFC 3339 in UTC to the millisecond.
- `file`: the file's path inside the folder, with `/` between its parts.
- `kind`: `save`, `checkpoint` or `unsaved`.
- `interface`: the way arc was reached, `terminal` for the `arc` command line, `mcp` for a tool of `arc mcp`, or the word the app or another library caller names for itself. A version recorded with no way has no `interface`.
- `version`: the version's id, as `arc history list` prints it.
- `step` and `change`: the name of the one step the version touched and what happened to it, `added`, `removed` or `changed`. `arc operation record`, `arc sql record` and their `arc mcp` tools name the step they add. For a plain save of `arcform.yaml`, whether from the app or from a library caller, arc compares the saved text with the state its history held before and names each step the save added, removed or changed. A step that was renamed reads as one removed and one added.
- `steps`: instead of `step` and `change`, when a save touched more than one step, a list of objects each holding a `step` and its `change`.

A version with neither names the file and no step: a checkpoint, an unsaved text, a file that is not `arcform.yaml`, a save that touched no step, and a save of `arcform.yaml` when the history holds no earlier state to compare with, because it is new on this machine or because that state is gone. A key at the manifest's top, a comment, the order of a step's keys and the order of the steps are no change to a step.

The folder of a file not named `arcform.yaml` is the nearest directory at or above it that holds one, so a Protocol has one log, and each line names its file. A line is appended in one write, so a reader never sees half of one. A checkpoint taken before a write gets its line when the write lands, just before the save's, so a write arc refuses leaves the folder as it was. A version arc does not record, because its text equals the newest one, gets no line. A line outlives its snapshot: pruning and the merging of quick saves remove snapshots and never lines, so a line's id finds a version only on a machine that still holds it. When arc cannot write a line, because the folder is read-only, the version is recorded all the same.

---

## Using Arcform as a library

`arc` ships a library target alongside the binary, for one purpose: a tool that edits an
`arcform.yaml` should not carry its own copy of the schema. Two schemas drift; one cannot.

```toml
[dependencies]
arc = { version = "=0.1.0", default-features = false }
```

`default-features = false` turns off the `cli` feature. That feature exists for the `arc`
binary; it also compiles a doc-hidden `cli_main` into the crate root, and `cli_main` parses
the *calling* process's `argv` and can exit it. With the feature off the crate root is
`spec` and nothing else, and `clap` leaves the dependency graph.

```rust
use arc::spec::{Manifest, Result, SpecEdit, edit_spec};

fn example() -> Result<()> {
    // A spec on disk.
    let dir = std::path::Path::new("examples/brewtrend");
    let manifest = Manifest::load(dir)?;
    println!("{} — {} steps", manifest.name, manifest.steps.len());

    // Edit it through the write path: applied to the original bytes, validated
    // by the same loader, written atomically — or refused with a reason.
    let edit = SpecEdit::Replace {
        path: vec!["params".into(), "trend_threshold".into(), "default".into()],
        value: "\"25\"".into(),
    };
    edit_spec(dir, &[edit])?;
    Ok(())
}
```

`arc::spec` is the entire public surface: the spec schema types, the loading entry points,
the write path, and the error type. Validation is a **gate, not a transform** — it parses
raw YAML to reach a verdict and hands nothing back to write. The write path is built on
that: a `SpecEdit` is a plain value, applied to the **original bytes** by splicing at
tree-sitter node spans, so every byte an edit does not target — comments, blank lines, key
order, quote style — survives verbatim, and a result that will not load is refused with the
loader's reason, leaving the file untouched. Writes are atomic (temp file + rename).
`create_spec` serialises a brand-new manifest directly — a spec with no prior authorship
has nothing to preserve — through the same gate. Serialising an *existing* spec yourself
remains out of contract: load-serialise-write-back destroys the author's bytes, which is
the whole reason the write path exists. Everything else — the runner, the engine, SQL
introspection, the operator catalog, the registry — is private and moves without notice.

The surface is documented as a contract in `src/spec.rs` and enumerated by
`tests/public_surface.rs`, which fails the build if it widens. Arcform is pre-1.0: **pin an
exact version** (or, inside the same organisation, a git revision).

Two build-time facts come with the dependency. Arcform parses SQL with its vendored
`sqlparser` fork (`vendor/sqlparser-0.55.0`), wired in via `[patch.crates-io]` — and Cargo
patch tables do not propagate through dependencies, so the consuming workspace must carry the
same patch entry pointing at that vendored package in the same pinned Arcform source (without
it, the build fails on missing `SetExpr::Pivot`/`Unpivot` variants). And the private engine
still compiles into the library, so the DuckDB C library must be available to the build
(`DUCKDB_LIB_DIR`/`DUCKDB_INCLUDE_DIR`) even for a consumer that only loads specs.

---

## Status

Early development. The project is in the discovery and design phase.

---

## Credits

Part of the [Meridian](https://meridian.online) project.

Built with [DuckDB](https://duckdb.org), [sqlparser-rs](https://github.com/sqlparser-rs/sqlparser-rs) (SQL introspection), and [Serde](https://serde.rs).

Pipeline model and step execution inspired by [Dagu](https://github.com/dagu-org/dagu). Asset-centric design influenced by [Dagster](https://dagster.io/)'s software-defined asset model. SQL-first approach informed by [SQLMesh](https://sqlmesh.com/) and [dbt](https://www.getdbt.com/). Local-remote parity pattern drawn from [nektos/act](https://github.com/nektos/act).

