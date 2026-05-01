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
  asset.rs        # Assets, SQL auto-discovery, dependency validation
  introspect.rs   # SQL introspection via sqlparser-rs
  precondition.rs # Typed step preconditions (modified_after, command)
  state.rs        # Run state tracking (step hashes, staleness)
  error.rs        # Error types
```

## Key types

- **`Manifest`** — top-level project config loaded from `arcform.yaml`. Contains `name`, `engine`, `engine_version`, `db`, `steps`, `assets`.
- **`Step`** — a pipeline step. Either `sql` (path to .sql file) or `command` (shell string). Has `produces`, `depends_on`, `preconditions`.
- **`Precondition`** — typed freshness check. Variants: `modified_after` (file age), `command` (shell exit code). AND semantics — all must pass to skip.
- **`AssetOverride`** — manual asset dependency declaration (for command steps; SQL steps auto-discover).

## Execution model

1. Load `arcform.yaml` manifest
2. Build asset dependency graph (SQL introspection + manual overrides)
3. For each step in order:
   - Evaluate preconditions (if any) + SQL hash staleness
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
- **sha2** — SQL content hashing for staleness detection
- **humantime** — duration parsing for preconditions

## Conventions

- Manifest filename: `arcform.yaml`
- SQL files live alongside the manifest, referenced by relative path
- Engine delegation: SQL runs via `duckdb -bail <db> < file.sql`
- State is persisted in the DuckDB database itself (run metadata tables)
