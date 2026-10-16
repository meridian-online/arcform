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
  skills.rs       # `arc skills`: lists the agent skills `build.rs` embeds from `skills/`, and installs them into agents' skills folders
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

The skills exist so that an agent given a tabular dataset and a question reaches for arc, not a throwaway script, and leaves the analyst a Protocol they can re-run, inspect in Brightfield or share. Their design follows from that:

- **The method is in the skills; the tool stays thin.** A short entry skill routes to one skill per stage, each with a checkpoint where the analyst can redirect, and the work is sized up front (a quick look, an analysis or a project) so the checkpoints scale with it. The skills spend their words on arc's method, checks and traps, not on SQL agents already know. Routing is left to the skills' descriptions and the `arc mcp` instructions; there are no routing hooks.
- **An agent with only `arc mcp` must be able to follow every step.** That includes a desktop chat app with no file access of its own, so every file the workflow produces (a schema, a panel, the project's `README.md`, the report) is written by an arc verb with a matching tool, inside the Protocol folder and recorded in history. The CLI stays canonical: each MCP tool has its CLI verb.
- **arc draws no chart.** A chart is a Mosaic spec the agent writes to `panels/<name>.yaml`, which Brightfield opens and history versions. `arc panel check` checks only that a panel's tables and columns exist; whether Brightfield can draw it is left to the release eval, which opens every panel. A PDF of the report comes from Brightfield too, so arc gains no renderer and stays local-first.
- **The report is a verb, not a step.** It is a snapshot of one session's findings, cited to the run it describes and versioned by history, so a re-run does not rewrite it; the Protocol stays the reproducible record.
- **A skill never describes a different arc from the one running.** `arc skills install` writes the skills embedded in that binary. Channels that serve the repository instead (a plugin, GitHub, a skills directory) can drift from the user's arc, so each skill names the version it was written for and the entry skill's environment check compares it with `arc --version`.

Model training and per-operator recipe skills are outside the set: the modelling the skills teach is what the existing operators do, and the build skill points at the operators and `operator_describe`.

The skills ship inside the binary. `build.rs` embeds every file of each skill folder (a directory under `skills/` holding a `SKILL.md`; hidden files and the maintainer's `skills/README.md` stay out) as `SKILLS` in `$OUT_DIR/skills.rs`, and reruns when `skills/` changes. `arc skills install` (`src/skills.rs`) writes them into each agent whose folder exists under the home directory: Claude Code `~/.claude/skills`, Codex `~/.agents/skills` (when `~/.codex` exists), Cursor `~/.cursor/skills`, Gemini CLI `~/.gemini/skills` and the shared `~/.agents/skills`, one target per distinct folder; `--agent` and `--target` name them instead. Each folder written gets a `.arc-skill` marker whose first line is `arc <version>`. Each target is all or nothing as far as refusal goes: every destination in it is checked before any is written; one with the marker is replaced, an absent one is written, and any other is refused, which writes nothing into that target while other targets still install, and the command exits 1. A target that passes has every skill staged in a hidden folder beside its destination before any is swapped in; a swap renames the old folder aside, renames the new one in, then deletes the old one, putting it back if the rename fails, so an error leaves each skill folder one version whole unless the put-back fails too. That case, and a process killed between the two renames, leave no folder and the old one set aside as `.<skill>.arc-old` (a failed put-back's error names it); the next run writes the skill afresh and clears that folder, as it clears any `.arc-old` once a new folder is in place. With no agent found and none named, it prints where each would go and exits 1. `tests/skills_install.rs` drives it with `HOME` set to a temporary folder.

The repository is also a plugin for Claude Code, Codex and Cursor and an extension for Gemini CLI, each bringing the skills and starting `arc mcp` from `PATH`. No agent's format is the source: `scripts/generate-plugins.py` (standard library Python) writes every manifest from `plugin/plugin.meta.json`, with the version from `Cargo.toml`, and `tests/plugin_manifests.rs` runs it into a temporary folder and fails, naming the source file to edit, when a committed manifest differs. The marketplace that Claude Code and Codex read names the plugin at the release tag `v<version>`, not the default branch, so those agents install the newest release's skills; Gemini CLI and Cursor install at the tag the user names. `skills/using-arcform/references/agents/` holds the notes for each agent and for a desktop chat app, which has no plugin route and uploads the skills `arc skills install --target` writes.

`tests/skills.rs` (feature `mcp`) holds the skills to the binary they teach. It checks every MCP tool and argument a skill names against a live `arc mcp` `tools/list`, and every `arc` verb, subcommand and flag against `arc … --help`; requires every row of a CLI and MCP table to fill both columns, an MCP cell naming a tool or saying `None:` and why (the test prints those gaps); checks the frontmatter loads and names this crate's version; and runs each `sh arc-example` block under `set -e` in a fresh copy of a fixture Protocol, matching the `text expect` block after it against the output. An example declares what it needs (`needs=finetype`, `needs=finetype-ext`) and is skipped without it, except that `ARCFORM_REQUIRE_FINETYPE_EXT`, which CI sets, turns a missing extension into a failure. The notation the checker reads is specified in the module doc at the top of the file. Its fixtures: `tests/fixtures/skills/`, the default Protocol for examples (one step loading a 200-row `sales.csv`, beside an `orders.json`); `tests/fixtures/skills-empty-filter/`, whose filter step leaves an empty table to trace; and `tests/fixtures/skills-docs/`, test-only skills that exercise the checker itself, `good/` passing every check and `bad/` and `bad_name/` failing each at the line the test asserts.

The structural test proves a skill names only what arc has, not that an agent picks arc up and follows the method. `eval/analyst-workflow/` is the release test for that: ten analyst tasks on public datasets, across the three sizes, run by hand before each release in Claude Code, Codex and a desktop chat client with only `arc mcp`. A task passes in an agent when the agent chose arc unprompted and ended with the seven outcomes the skills promise; failures are clustered by cause and fixed with the smallest skill edit, and the scored sheet goes in the release pull request. It is not a CI job, because it needs paid agent sessions. Its `README.md` is the runbook.

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
