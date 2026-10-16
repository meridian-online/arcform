# Arcform Architecture

Reference doc for Arcform's source layout, key types, execution model, and runtime dependencies. Loaded on demand via the trigger pointer in `CLAUDE.md`.

## Source layout

```
src/
  main.rs         # Entry point
  cli.rs          # Clap CLI definition
  manifest.rs     # YAML manifest parsing (arcform.yaml)
  runner.rs       # Step execution engine (~1150 lines, largest module)
  engine.rs       # SQL engine invocation (DuckDB CLI delegation)
  asset.rs        # Assets, SQL auto-discovery, dependency validation, case-collision gate
  introspect.rs   # SQL introspection via sqlparser-rs
  precondition.rs # Typed step preconditions (modified_after, fresh, command, tool)
  state.rs        # Run state tracking (step config hash, artifact hash, staleness)
  inspect.rs      # Read-only look inside a Protocol's database: `arc table describe`, `arc sql query`
  protocol_files.rs # Writes a Protocol's schemas/, panels/ and README.md, each recorded in its own history: `arc protocol write-file`
  panel_check.rs  # Finds panels that name a table or column the Protocol does not make: `arc panel check`
  report.rs       # Writes report.md from an agent's prose and a run's contract: `arc report write`
  finetype_cli.rs # Runs the `finetype` command and holds its version floor, `MIN_VERSION`; shared by the CLI verbs and the MCP tools
  error.rs        # Error types
  mcp/            # `arc mcp`: a Model Context Protocol server over stdio (feature `mcp`)
    mod.rs        #   JSON-RPC transport, protocol-version negotiation, the tool registry, each tool's read-only, open-world and destructive annotations
    finetype.rs   #   The five FineType tools, each running the `finetype` command
    hero.rs       #   arc's own tools: protocol_run, protocol_create, the record, edit, history, describe, query, check and report tools
```

### `arc mcp` and `protocol_run`

The MCP stdio transport gives standard output to JSON-RPC alone: one message per line, and any other byte there breaks the client's reading of the stream. A run prints a great deal (step progress, the asset graph, the rows a SQL step's `SELECT` prints on the DuckDB command line, whatever a `command:` step writes), so `protocol_run` does not run the Protocol inside the server. It starts `arc run` as a child process from the server's own executable, in the Protocol's directory, with standard input closed and standard output and standard error piped into a buffer that keeps the last 4,000 bytes. When the child exits, the tool returns the run record it wrote under the Protocol's runs directory; when it wrote none, the tool's error carries the exit status and that tail of output.

### Release and distribution

The package and library are `arcform` and the binary is `arc`. The vendored sqlparser fork is published as its own crate, `arcform-sqlparser`, pinned exactly, because Cargo drops `[patch.crates-io]` on publish. A release binary is built with `mcp,bundled-duckdb`, so it carries DuckDB and starts on a machine with no DuckDB library; SQL steps still run through the `duckdb` command. A pushed `vX.Y.Z` tag builds four targets, publishes the GitHub release with the tag's `CHANGELOG.md` section as its notes, publishes both crates and updates the install script, behind a guard that refuses a tag not matching `Cargo.toml` and the changelog. `docs/RELEASE.md` describes the machinery.

### Agent skills

`skills/` at the repository root holds five agent skills, one folder each, that teach an agent the analyst workflow through arc: `using-arcform` (the entry point: the safety model, where results land, the sizes of a task, the environment check and the full CLI-to-MCP verb map), then one per stage, `scoping-an-analysis`, `profiling-and-typing-data`, `building-and-checking-a-protocol` (Build and Check) and `presenting-findings`. A skill is a `SKILL.md` with YAML frontmatter (`name`, `description`, `metadata.arc_version`) and, where it helps, a `references/` folder; each shows every step both as an `arc` command and as the matching `arc mcp` tool. The workflow outline in `INSTRUCTIONS` (`src/mcp/mod.rs`) names the same stages and skills, and its test fails when the outline names a skill that has no folder. The maintainer's `release` skill is separate, in `.claude/skills/`.

`tests/skills.rs` (feature `mcp`) holds the skills to the binary they teach. It checks every MCP tool and argument a skill names against a live `arc mcp` `tools/list`, and every `arc` verb, subcommand and flag against `arc … --help`; requires every row of a CLI and MCP table to fill both columns, an MCP cell naming a tool or saying `None:` and why (the test prints those gaps); checks the frontmatter loads and names this crate's version; and runs each `sh arc-example` block under `set -e` in a fresh copy of a fixture Protocol, matching the `text expect` block after it against the output. An example declares what it needs (`needs=finetype`, `needs=finetype-ext`) and is skipped without it, except that `ARCFORM_REQUIRE_FINETYPE_EXT`, which CI sets, turns a missing extension into a failure. The notation the checker reads is specified in the module doc at the top of the file. Its fixtures: `tests/fixtures/skills/`, the default Protocol for examples (one step loading a 200-row `sales.csv`, beside an `orders.json`); `tests/fixtures/skills-empty-filter/`, whose filter step leaves an empty table to trace; and `tests/fixtures/skills-docs/`, test-only skills that exercise the checker itself, `good/` passing every check and `bad/` and `bad_name/` failing each at the line the test asserts.

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
   - SQL steps: delegate to engine CLI (`duckdb -bail`)
   - Command steps: shell execution with real-time stdout streaming
4. Update run state after each step

## Runtime dependencies

- **clap** — CLI argument parsing
- **serde / serde_yaml** — manifest (de)serialization
- **sqlparser** — SQL introspection (CTE/table extraction)
- **duckdb** — DuckDB Rust bindings (state backend)
- **semver** — engine version constraint checking
- **sha2** — SQL/op config hashing AND produced/read artifact content hashing, both for staleness detection
- **humantime** — duration parsing for preconditions

## Conventions

- Manifest filename: `arcform.yaml`
- SQL files live alongside the manifest, referenced by relative path
- Engine delegation: SQL runs via `duckdb -bail <db> < file.sql`
- State is persisted in the DuckDB database itself (run metadata tables)
