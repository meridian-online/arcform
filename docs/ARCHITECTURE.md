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
  duckdb_tokens.rs # A SQL file's tokens, asked of the DuckDB that runs it (`sql_tokenize`); the refusals read these
  introspect.rs   # SQL introspection via sqlparser-rs
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
