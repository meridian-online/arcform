# Arcform Architecture

Reference doc for Arcform's source layout, key types, execution model, and runtime dependencies. Loaded on demand via the trigger pointer in `CLAUDE.md`.

## Source layout

```
src/
  main.rs         # Entry point
  cli.rs          # Clap CLI definition
  manifest.rs     # YAML manifest parsing (arcform.yaml)
  runner.rs       # Step execution engine (~1150 lines, largest module)
  engine.rs       # SQL engine invocation (DuckDB CLI delegation), the engine range, the extension refusals
  asset.rs        # Assets, SQL auto-discovery, dependency validation, case-collision gate
  duckdb_tokens.rs # A SQL file's tokens, asked of the DuckDB that runs it (`sql_tokenize`); the refusals and the asset graph read these
  duckdb_lineage.rs # What each statement reads and produces, from DuckDB 2.0's tokens and parse trees
  introspect.rs   # SQL introspection: chooses the reader, falls back to sqlparser-rs per statement
  precondition.rs # Typed step preconditions (modified_after, fresh, command, tool)
  state.rs        # Run state tracking (step config hash, artifact hash, staleness)
  inspect.rs      # Read-only look inside a Protocol's database: `arc table describe`, `arc sql query`
  error.rs        # Error types
  mcp/            # `arc mcp`: a Model Context Protocol server over stdio (feature `mcp`)
    mod.rs        #   JSON-RPC transport, protocol-version negotiation, the tool registry
    finetype.rs   #   The five FineType tools, each running the `finetype` command
    hero.rs       #   arc's own tools: protocol_run, protocol_create, the record, describe and query tools
```

### `arc mcp` and `protocol_run`

The MCP stdio transport gives standard output to JSON-RPC alone: one message per line, and any other byte there breaks the client's reading of the stream. A run prints a great deal (step progress, the asset graph, the rows a SQL step's `SELECT` prints on the DuckDB command line, whatever a `command:` step writes), so `protocol_run` does not run the Protocol inside the server. It starts `arc run` as a child process from the server's own executable, in the Protocol's directory, with standard input closed and standard output and standard error piped into a buffer that keeps the last 4,000 bytes. When the child exits, the tool returns the run record it wrote under the Protocol's runs directory; when it wrote none, the tool's error carries the exit status and that tail of output.

`tests/finetype_mcp.rs` (feature `mcp`) calls the five FineType tools through `arc mcp` against the real `finetype`, with every property and enum value their `tools/list` schemas advertise; a property with no call, or a call setting one no schema advertises, fails the test. The unit tests in `src/mcp/finetype.rs` use a stand-in that accepts any arguments, so this is the test that catches a flag finetype drops. CI's build job installs the finetype release pinned by `FINETYPE_VERSION` and `FINETYPE_SHA256` in `.github/workflows/ci.yml` and sets `ARCFORM_REQUIRE_FINETYPE_CLI`, under which this test fails rather than skips when the CLI is missing; `tests/common` resolves the binary as arc does, from `FINETYPE_BIN` or `PATH`, against the floor in `src/mcp/finetype.rs`.

### DuckDB 2.0: the engine, the shared database and agent mode

arc links the DuckDB 2.0 library, the `duckdb` crate at `=1.20000.0` (DuckDB 2.0.0), and runs each SQL step and SQL hook on a DuckDB command line, `ARC_DUCKDB_BIN` or `duckdb` on `PATH`, as `duckdb <db> -f <file.sql>`. It accepts a command line in `>=2.0, <3` (`SUPPORTED_ENGINE_RANGE` and `OLDEST_ENGINE` in `src/engine.rs`). The two must share a major version because they share a file: the command line writes the Protocol's database, and arc opens the same file through the linked library for run state, `arc table describe` and `arc sql query`. A database DuckDB 2.0 creates cannot be opened by DuckDB 1.x, so a 1.x command line is refused as too old even with `ARC_ALLOW_UNTESTED_ENGINE`, and the refusal names arc 0.2.1 as the release for DuckDB 1.5. The variable lifts the range only upwards, to a 2.x pre-release or a 3.x, where a failure to open the file is the user's accepted risk.

A development build whose printed source id is the commit of a release's tag counts as that release wherever arc compares versions (its range, a Protocol's `engine_version:`, the extension pin keys, the new-engine notice); the run record keeps the raw string. Homebrew's 2.0.0 prints `v2.0.0-dev88614 (Development Version) d54cbbe3df`, the v2.0.0 tag's commit. The release-tag-to-commit table is `RELEASE_COMMITS` in `src/engine.rs`, and each later 2.x patch needs a row there. Such a build looks community extensions up under its commit hash, which the registry does not serve, so a run of a Protocol that installs one warns before the first step, naming DuckDB's installer for the release build.

arc reads SQL tokens from `sql_tokenize`, which lives in DuckDB's autocomplete extension. DuckDB's release command line links it in; a build without it, such as Homebrew's 2.0.0 at the time of writing, fails to autoload it, so it gives no tokens for any file. `arc run` names that cause rather than refusing the first file: when the pre-run token check fails, or asked DuckDB nothing (no file held SQL yet), it probes `SELECT count(*) FROM sql_tokenize('SELECT 1')` (`duckdb_tokens::tokenizer_failure`, through `Engine::tokenizer_failure`), and a DuckDB that exits with an error there refuses the run with exit 2 before any step (`Error::NoTokenizer`), naming the DuckDB, its printed version and DuckDB's installer pinned to `DUCKDB_RECOMMENDED` in `src/engine.rs`. The message says the build lacks the autocomplete extension only when DuckDB's error names `autocomplete` or `sql_tokenize`; any other error is quoted, with the same fix. A run whose files read starts no extra DuckDB process: the probe adds one only on those two paths. arc probes the capability and never recognises a build by name or path, so a build that adds the extension just works. Outside a run, the one sqlparser-rs warning names the same cause and fix when the same probe fails. `scripts/install.sh` runs that probe on the `duckdb` on `PATH` after installing arc, and installs DuckDB's release build with DuckDB's installer when there is none or it fails (naming a 1.x by its version, since 1.x has no `sql_tokenize` at all), unless `ARC_SKIP_DUCKDB` is set.

Every DuckDB process arc starts goes through `DuckDbProgram::command()`, which sets `DUCKDB_AGENT_MODE=0`. DuckDB 2.0's shell otherwise switches to markdown output, a row cap and a stderr banner when it finds a variable an agent's harness sets (`AI_AGENT`, `CLAUDECODE` and others), and arc would misread its answers and report the banner as a step's error. The environment arc adds for a step holds only `ARC_*` names, so a Protocol cannot turn agent mode back on or change which DuckDB runs it.

`arc run` prints one notice on stderr before the first step when the newest readable run record that names `duckdb_cli` names a different major.minor from the engine about to run; for 1.x to 2.x it also names the four results DuckDB 2.0 changes without an error. It changes no setting.

One DuckDB version runs throughout: the crate pin in `Cargo.toml`, `DUCKDB_VERSION` in the CI workflows, `DUCKDB_RECOMMENDED` in `scripts/install.sh` and in `src/engine.rs` (the version arc's messages pin DuckDB's installer to) and each entry's `duckdb_versions` in `src/vetted_extensions.json`. `scripts/check_duckdb_version.py`, run in CI's `release-guard` job with its self-test, fails when a pin disagrees with the version Cargo.lock's `libduckdb-sys` decodes to; a vetted entry must include the linked version and may list others. CI's build job also runs the tests that need a real DuckDB, with `--include-ignored`, against the v2.0.0 command line it installs, named by `ARC_DUCKDB_RECORD_BIN`.

### Reading a step's SQL

arc reads each SQL file twice over for two purposes, from one answer: the refusals (which extensions a step installs, which settings it names) and the asset graph (what each statement reads and produces). Both start from the same two layers.

- **The command-line layer is arc's own.** What the DuckDB command line does to a `-f` file before its parser sees it is not something DuckDB will report, so `cli_lines`, `sql_is_complete` and `cli_batches` in `src/engine.rs` copy it from DuckDB v2.0.0's `tools/shell/shell.cpp` and `src/parser/parser.cpp`, each rule citing its source: dot commands and `#` lines, the `0x03` byte, NUL bytes, how lines batch into statements (a backtick counts as a quote in statement completion on 2.0), and the unicode spaces the parser reads as a space.
- **The tokens are DuckDB's.** `src/duckdb_tokens.rs` asks the program the step runs on (never `PATH` when `ARC_DUCKDB_BIN` is set, and never the linked library) for each batch's `sql_tokenize`, one process per file. Each batch is sent hex-encoded, because the command line drops a `\r` before a `\n` in piped standard input, and every token's start is a byte offset into the file arc holds. Answers are cached by the SHA-256 of the file's bytes and shared between the pre-run check, the check just before each step, and the asset graph, so a file read again unchanged is not asked again.

The refusals fail closed: a DuckDB that fails, answers in a form arc cannot read, or answers from a version other than 2.x stops the run with exit 2, naming the step or hook. An empty token list (no SQL in the file) is a valid answer and asks nothing. `PARSER_SWITCHES` is `allow_parser_override_extension` and `current_dialect`; `enable_peg_parser` is gone in 2.0. `docs/VETTED_EXTENSIONS.md` is the user-facing statement of what is refused and what is not read.

The asset graph is read by `src/duckdb_lineage.rs` by default: statements split at DuckDB's `;` tokens, then `json_serialize_sql` gives the tree of each query, and what a non-query statement creates, inserts into or copies comes from its tokens. A keyword is matched by its text, never by the token type, which on 2.0 changes with context. sqlparser-rs (the vendored `arcform-sqlparser` fork) reads, per statement, any statement DuckDB gives no tree for; `ARC_SQL_READER=sqlparser` has it read every statement, and any value other than `duckdb` or `sqlparser` is refused. A statement neither reader can read draws a warning and reads nothing; the rest of the step is still read. A bare-name statement (`tbl;`, which the 2.0 shell runs as `SELECT * FROM tbl`) reads its table.

When no DuckDB 2 answers, `arc run` refuses before any step runs, and a DuckDB 2 whose answer for a file cannot be read refuses that step by name. Outside a run (the run contract's per-statement reading, `arc sql record`, `arc operation record`, the MCP tools), no DuckDB 2 means sqlparser-rs reads the whole graph, with one warning per process, and an unreadable answer draws a per-file warning.

The run contract records each statement's `byte_range`, computed from DuckDB's statement split and mapped from the command-line batches back to file bytes (a range that cannot be placed is `null`), and its `read_by`: `duckdb`, `sqlparser`, or `nothing`.

`tests/reader_parity.rs` is the parity gate. It reads every `.sql` file under `tests/fixtures` and `examples` with both readers and compares what each statement produces and reads; each known difference is pinned exactly with its reason, and the test also pins which statements take the sqlparser-rs fallback, so a change in DuckDB's reading or in the fallback shows up as a failure. Known sqlparser-rs gaps, which only the forced and fallback paths see: tables inside `EXISTS` and scalar subqueries.

### Release and distribution

The package and library are `arcform` and the binary is `arc`. The vendored sqlparser fork is published as its own crate, `arcform-sqlparser`, pinned exactly, because Cargo drops `[patch.crates-io]` on publish. A release binary is built with `mcp,bundled-duckdb`, so it carries DuckDB and starts on a machine with no DuckDB library; SQL steps still run through the `duckdb` command. A pushed `vX.Y.Z` tag builds four targets, publishes the GitHub release with the tag's `CHANGELOG.md` section as its notes, publishes both crates and updates the install script, behind a guard that refuses a tag not matching `Cargo.toml` and the changelog. `docs/RELEASE.md` describes the machinery.

## Key types

- **`Manifest`** — top-level project config loaded from `arcform.yaml`. Contains `name`, `engine`, `engine_version`, `db`, `steps`, `assets`.
- **`Step`** — a pipeline step. Either `sql` (path to .sql file) or `command` (shell string). Has `produces`, `depends_on`, `preconditions`.
- **`Precondition`** — typed freshness check. Variants: `modified_after` (file age), `fresh` (a fetched artifact's recorded identity vs. the remote), `command` (shell exit code), `tool` (the identity of an external binary or artifact the step did not produce — a reported version or a content hash). AND semantics — all must pass to skip.
- **`AssetOverride`** — manual asset dependency declaration (for command steps; SQL steps auto-discover).

## Execution model

1. Load `arcform.yaml` manifest
2. Build asset dependency graph (SQL introspection + manual overrides); refuse the manifest if two produces:/depends_on: declarations collapse onto one asset by case alone
3. For each step in order:
   - Evaluate preconditions (if any) + SQL/op config hash staleness + produced/read artifact hash staleness
   - Skip if fresh; execute if stale
   - SQL steps: delegate to engine CLI (`duckdb <db> -f <file.sql>`)
   - Command steps: shell execution with real-time stdout streaming
4. Update run state after each step

## Runtime dependencies

- **clap** — CLI argument parsing
- **serde / serde_yaml** — manifest (de)serialization
- **sqlparser** — SQL introspection fallback (the vendored `arcform-sqlparser` fork), for statements DuckDB's reading cannot take or when `ARC_SQL_READER=sqlparser`
- **duckdb** — DuckDB 2.0 Rust bindings (state backend; reads the database the command line writes)
- **semver** — engine version constraint checking
- **sha2** — SQL/op config hashing AND produced/read artifact content hashing, both for staleness detection
- **humantime** — duration parsing for preconditions

## Conventions

- Manifest filename: `arcform.yaml`
- SQL files live alongside the manifest, referenced by relative path
- Engine delegation: SQL runs via `duckdb <db> -f <file.sql>`, with `DUCKDB_AGENT_MODE=0`
- State is persisted in the DuckDB database itself (run metadata tables)
