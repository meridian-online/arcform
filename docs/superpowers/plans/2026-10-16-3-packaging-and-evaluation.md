# Analyst workflow, plan 3 of 3: packaging and evaluation

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Get the skills to any agent at the version of the arc it runs, and prove the release with an agent eval.

**Architecture:** The binary embeds `skills/` at build time and `arc skills install` writes them into each detected agent's skills directory. Plugin manifests for several agents are generated from one source file and checked for drift in CI. The eval is a fixed task set with a written runbook, run by hand before a release.

**Tech Stack:** Rust (`build.rs` embedding, a new `skills` verb); JSON plugin manifests; Markdown runbook.

**Spec:** `docs/superpowers/specs/2026-10-16-analyst-workflow-skills-design.md`, sections "Capability work" item 6, "Packaging and discovery" and "Verification" (the release test).

**Order:** Runs after plan 2. The release also waits on Brightfield's release (opening agent-written panels and headless PDF export), which Brightfield plans in its own repository.

## Global Constraints

- Skills install at the binary's version: the installed files are the ones embedded in that binary.
- Agent-agnostic: no agent's format is the source; each manifest is generated.
- Install writes only inside the agent skills directories it names in its output, and never overwrites a skill folder it did not write (it recognises its own by a marker file).
- Public repository: no private planning identifiers. British English.

## Review Focus

1. **A user's own skill with the same folder name** as an arc skill must not be overwritten; install refuses that one and says why. Test in Task 1.
2. **No agent detected** must print where it would install and how to name a target, not exit silently. Test in Task 1.
3. **Reinstalling over an older arc's skills** must replace them, including removing reference files the new version dropped. Test in Task 1.
4. **A manifest edited by hand** must fail CI with a message naming the source file to edit instead. Test in Task 2.
5. **An eval task whose dataset URL has gone** must be noticed before an eval run is wasted. Covered by Task 3's preflight.

## Verification commands and what each costs

- `DUCKDB_LIB_DIR=/opt/homebrew/lib cargo test --features mcp --test skills_install --test plugin_manifests`: about a minute after build.
- The eval (Task 3) costs paid agent time in three agents; run it once per release candidate, not per commit.

---

### Task 1: `arc skills install`

**Files:**
- Create: `build.rs` addition (or a new `build.rs` if none) embedding `skills/**` as `static SKILLS: &[(&str, &[u8])]`; `src/skills.rs`
- Modify: `src/cli.rs` (`arc skills install`, `arc skills list`), `src/lib.rs`, `tests/public_surface.rs` (`MODULE_FILES`), `Cargo.toml` (`"/build.rs"` and `"/skills/**"` in `include`), `.github/workflows/ci.yml` (allow `build.rs` and `skills/` in the package-contents check, so the "Packaged arcform builds" step passes)
- Test: `tests/skills_install.rs`

**Interfaces:**
- Produces:
  - `pub fn agent_targets(home: &Path) -> Vec<AgentTarget>`; `pub struct AgentTarget { pub agent: &'static str, pub skills_dir: PathBuf }`. Detected when the agent's config folder exists: Claude Code `~/.claude/skills`, Codex `~/.codex/skills`, Cursor `~/.cursor/skills`, Gemini CLI `~/.gemini/skills`, and the shared `~/.agents/skills`. Before writing, confirm each path against that agent's current documentation and record the source in a comment.
  - `pub fn install(targets: &[AgentTarget]) -> Result<Vec<Installed>>`: for each target and skill, replaces the folder wholesale when it holds the marker file `.arc-skill` (which records the arc version), writes it when absent, and refuses it otherwise.
  - CLI: `arc skills install [--agent NAME]... [--target DIR]`, printing each folder written; `arc skills list` prints the embedded skills and their `arc_version`.

- [ ] **Step 1: Write failing tests** with a temporary home: `installs_into_each_detected_agent`; `refuses_user_folder_of_same_name`; `replaces_older_install_and_drops_removed_files`; `no_agent_detected_prints_targets_and_exits_1`; `target_flag_installs_there`; `embedded_skills_match_skills_dir` (the embedded list equals the files under `skills/`).
- [ ] **Step 2: Run** `cargo test --features mcp --test skills_install`. Expected: fail.
- [ ] **Step 3: Implement** `build.rs` embedding (rerun when `skills/` changes) and `src/skills.rs`.
- [ ] **Step 4: Run** the tests. Expected: pass.
- [ ] **Step 5: Update** the entry skill's `environment-check.md` and the README with the install command; run `cargo test --features mcp --test skills`.
- [ ] **Step 6: Commit** "Install the analyst skills at the binary's version".

### Task 2: Plugin manifests, per-agent notes, and the README

**Files:**
- Create: `plugin/plugin.meta.json` (the one source: name, description, version from `Cargo.toml`, homepage, licence, the MCP server command `arc mcp`), `scripts/generate-plugins.py` (stdlib only), the generated `.claude-plugin/plugin.json`, `.claude-plugin/marketplace.json`, `.codex-plugin/plugin.json`, `.cursor-plugin/plugin.json`, `gemini-extension.json`, `AGENTS.md` pointer section
- Create: `skills/using-arcform/references/agents/<agent>-tools.md` for Claude Code, Codex, Cursor, Gemini CLI and a desktop chat client: how that agent loads skills, connects `arc mcp`, and which tools it names differently
- Modify: `README.md` (a section for analysts: install arc with the MCP feature, `arc skills install`, connecting `arc mcp`, and the plugin routes); `tests/plugin_manifests.rs`; `.github/workflows/` CI job running the drift test

Follow superpowers' layout for each agent's manifest, reading its current files rather than memory.

- [ ] **Step 1: Write the failing test** `manifests_match_source`: runs the generator into a temporary folder and compares byte for byte with the committed files; on difference, names `plugin/plugin.meta.json` as the file to edit.
- [ ] **Step 2: Run** `cargo test --test plugin_manifests`. Expected: fail (no files).
- [ ] **Step 3: Write the source, the generator, the generated files and the per-agent notes.**
- [ ] **Step 4: Run** the test and the skills test. Expected: pass.
- [ ] **Step 5: Commit** "Package the analyst skills for several agents from one source".

### Task 3: The release eval

**Files:**
- Create: `eval/analyst-workflow/README.md` (the runbook), `eval/analyst-workflow/tasks.yaml`, `eval/analyst-workflow/score.md` (the scoring sheet), `eval/analyst-workflow/preflight.sh`
- Modify: `.claude/skills/release/SKILL.md` (two steps: set every skill's `metadata.arc_version` to the new version when bumping it, since the skills test requires them equal; and attach the eval's scored results to the release pull request)

**Interfaces:**
- `tasks.yaml`: ten tasks, each `{ id, size: quick|analysis|project, dataset_url, licence, prompt, notes }`. The prompt is what an analyst would type and never names arc. Four quick, four analysis, two project; at least three datasets that are not CSV (JSON API, spreadsheet, Parquet). Public datasets whose licences allow use.
- `score.md`: per task and agent, did the agent choose arc unprompted, and each of the spec's seven outcomes, yes or no with one line of evidence; then failures clustered, each with the smallest skill edit proposed.
- Pass bar, *proposed for review*: every task chooses arc in at least two of the three agents, and every project task ends with all seven outcomes.
- Agents: Claude Code, Codex, and one desktop chat client with only `arc mcp` connected and no file access.

- [ ] **Step 1: Write `preflight.sh`**: for each task, HTTP HEAD on `dataset_url` returns 2xx, and `arc --version` matches the skills' `arc_version`. Run it. Expected: all pass.
- [ ] **Step 2: Write the runbook, tasks and scoring sheet.**
- [ ] **Step 3: Dry-run one quick task in Claude Code** with the skills installed, fill its row of the scoring sheet, and fix anything in the runbook that was unclear.
- [ ] **Step 4: Commit** "Add the analyst workflow's release eval".
