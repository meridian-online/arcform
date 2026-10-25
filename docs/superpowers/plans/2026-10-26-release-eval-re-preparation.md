# Release eval re-preparation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the 39 eval sessions of 2–6 Nov ready to start from a handout, against a build of main, without installing anything into the agents.

**Architecture:** Two small repo changes (the preflight's timeout on its own branch and pull request; the runbook's pass bar on this branch), then a new eval folder outside the repo built from one pinned arc binary: run folders, desktop zips, preflight results, a Protocol smoke run and a handout.

**Tech Stack:** Rust (cargo, `--features mcp`), bash, curl, Homebrew DuckDB 2.0.0_1, zip.

**Spec:** `docs/superpowers/specs/2026-10-26-release-eval-re-preparation-design.md`

## Global Constraints

- Public repo: no local paths, private planning references or personal tool names in tracked files, commits or pull request text.
- The eval folder is `$EVAL = ~/arc-eval/2026-11-02` (outside the repo). Never write to `~/arc-eval/2026-10-20/`.
- Never write to `~/.cargo/bin`, `~/.agents/skills`, `~/.codex/`, `~/.claude.json`, `~/.claude/skills`, or Claude Desktop's config or `Application Support` folder. No `claude mcp add`, no `arc skills install --agent ...`.
- One cargo build at a time. Build with `DUCKDB_LIB_DIR=/opt/homebrew/lib cargo build --features mcp`.
- Run the default-task preflight at most twice (Open-Meteo rate limit).
- Task ids come from `eval/analyst-workflow/tasks.yaml` (10) and `dashboard-tasks.yaml` (3); agents are `claude-code`, `codex`, `desktop`.

## Review Focus

1. A run folder that holds anything naming arc beyond `.claude/skills/` (Claude Code only): check with `find $EVAL/runs -mindepth 3 -not -path '*/.claude/skills*'` printing nothing.
2. A skill installed or zipped from a binary other than the pinned one: every `.arc-skill` marker and zip comes from `$EVAL/bin/arc`; check six skills in each Claude Code folder and six zips.
3. A zip that carries the `.arc-skill` marker, or nests its folder wrongly: `unzip -l` shows `<skill>/SKILL.md` at the top and no `.arc-skill`.
4. The pinned binary refusing Homebrew's `duckdb` on a real run: Task 3's smoke run exercises it.
5. A handout start line that points at the old 2026-10-20 folder or a missing run folder: Task 5 checks every path the handout names exists.

---

### Task 1: Preflight timeout, on its own branch and pull request

**Files:**
- Modify: `eval/analyst-workflow/preflight.sh` (the two `--max-time 30` lines, and the header comment)

- [ ] **Step 1:** From `main`, branch `raise-the-eval-preflight-url-timeout`.
- [ ] **Step 2:** Add `timeout="${PREFLIGHT_TIMEOUT:-90}"` beside the other settings, use `--max-time "$timeout"` on both curl calls, and add one usage line to the header: `PREFLIGHT_TIMEOUT=120 eval/analyst-workflow/preflight.sh` (seconds per URL, default 90).
- [ ] **Step 3:** Verify: `bash -n eval/analyst-workflow/preflight.sh` exits 0; `shellcheck eval/analyst-workflow/preflight.sh` reports nothing new against main's; `TASKS=eval/analyst-workflow/dashboard-tasks.yaml ARC=/nonexistent eval/analyst-workflow/preflight.sh` prints `ok` for each dataset URL and fails only on the arc check.
- [ ] **Step 4:** Add a `CHANGELOG.md` `[Unreleased]` line only if the changelog lists eval changes already (check first); commit, push, open the pull request, run `roborev wait <sha>`, then the branch review and checks as `practice/loop.md` says. Link it to the Kata issue in Forge.

### Task 2: Runbook says the analyst pass bar is approved (this branch)

**Files:**
- Modify: `eval/analyst-workflow/README.md:137`, `eval/analyst-workflow/score.md:84`

- [ ] **Step 1:** Replace `*Proposed for review:*` in both with `*Approved by the maintainer on 21 October 2026:*`, keeping the rest of each sentence. Leave the dashboard rubric's wording alone.
- [ ] **Step 2:** Verify: `grep -n "roposed for review" eval/analyst-workflow/README.md eval/analyst-workflow/score.md` prints only README's dashboard line (148).
- [ ] **Step 3:** Commit; `roborev wait <sha>`.

### Task 3: Pinned build, folder skeleton and smoke run

**Files (outside the repo):** `$EVAL/bin/arc`, `$EVAL/BUILD.txt`, `$EVAL/smoke/`

- [ ] **Step 1:** On `main` at the current commit (record `git rev-parse --short HEAD`; expect `7d148cc`), build: `DUCKDB_LIB_DIR=/opt/homebrew/lib cargo build --features mcp`. Copy `target/debug/arc` to `$EVAL/bin/arc`.
- [ ] **Step 2:** Write `$EVAL/BUILD.txt`: the commit, `arc --version`, `/opt/homebrew/bin/duckdb --version`, `brew info duckdb | head -1`, `finetype --version`, `uv --version`, `brightfield --version`, date.
- [ ] **Step 3:** Smoke run: copy `tests/fixtures/dashboard-from-data/` to `$EVAL/smoke/`, then `cd $EVAL/smoke && PATH=$EVAL/bin:/opt/homebrew/bin:/usr/bin:/bin arc run`. Expected: exit 0 and the step reported done. A refusal naming the engine or the tokenizer is a blocker: stop and flag the issue.
- [ ] **Step 4:** Verify `$EVAL/bin/arc --version` prints `arc 0.2.0` and `$EVAL/bin/arc mcp --help` exits 0.

### Task 4: Run folders, Claude Code skills, desktop zips, preflight

**Interfaces:** Consumes `$EVAL/bin/arc` from Task 3.

- [ ] **Step 1:** Create `$EVAL/runs/<agent>/<task id>/` for all 3 × 13, and `$EVAL/transcripts/`, `$EVAL/task-ids.txt` (13 ids, analyst tasks first in file order, then dashboard).
- [ ] **Step 2:** For each Claude Code folder: `$EVAL/bin/arc skills install --target <folder>/.claude/skills`. Expected: six skills each.
- [ ] **Step 3:** `$EVAL/bin/arc skills install --target $EVAL/desktop-skills/staging`; then for each skill, from `staging/`, `zip -r ../<skill>.zip <skill> -x '<skill>/.arc-skill'`. Expected: six zips, `unzip -l` shows `<skill>/SKILL.md` and no `.arc-skill`.
- [ ] **Step 4:** Preflight, with Task 1's script from its branch if not yet merged (`git show raise-the-eval-preflight-url-timeout:eval/analyst-workflow/preflight.sh > $EVAL/preflight.sh`, run with `TASKS=` pointing at main's task files): `ARC=$EVAL/bin/arc` for `tasks.yaml` and for `dashboard-tasks.yaml`. Save both outputs to `$EVAL/preflight-tasks.txt` and `$EVAL/preflight-dashboard.txt`. Expected: `preflight: all checks passed` for both. A dataset gone is a needs-human flag, not a substitution.
- [ ] **Step 5:** Verify Review Focus 1–3 with the commands named there.

### Task 5: Handout

**Files (outside the repo):** `$EVAL/HANDOUT.md`

Model it on `~/arc-eval/2026-10-20/HANDOUT.md`, with these sections:

- [ ] **Step 1:** Header: arc 0.2.0, main at the recorded commit, Homebrew DuckDB 2.0.0_1, the preflight results; nothing has been installed into the agents yet.
- [ ] **Step 2:** "Before the first session (2 Nov)": (a) re-check `duckdb --version` prints `v2.0.0` and `duckdb -c "select count(*) from sql_tokenize('select 1')"` gives 2; (b) back up into `$EVAL/backups/`: `~/.codex/config.toml`, Claude Desktop's `claude_desktop_config.json`, a listing of `~/.agents/skills`, and `~/.cargo/bin/arc`'s version; (c) move `~/.cargo/bin/arc` (0.1.0) to `$EVAL/backups/arc-0.1.0-from-cargo-bin` and symlink `$EVAL/bin/arc` there; (d) Codex: `$EVAL/bin/arc skills install --agent codex`, add the arc server with `tool_timeout_sec = 600` to `config.toml` (give the exact TOML block, absolute command path); (e) Claude Code: `claude mcp add arc -- $EVAL/bin/arc mcp` (local scope) in each of the 13 Claude Code run folders, as one loop; (f) Claude Desktop: add the arc server by absolute path to its config, remove any arc skill uploads left from October, upload the six zips with `using-arcform.zip` first, turn off file-system connectors, restart the app.
- [ ] **Step 3:** "Playing the analyst": copy the runbook's script; add the dashboard differences (no playback, 15-minute cap including download, open the data or Protocol in Brightfield when the agent says to, results are practice).
- [ ] **Step 4:** One section per task (13), as in the old handout: prompt verbatim from the task file, the three start lines pointing into `$EVAL/runs/`, the refinement from `notes` for analyses and projects.
- [ ] **Step 5:** "Transcripts": as the old handout, into `$EVAL/transcripts/`.
- [ ] **Step 6:** "After the eval: restore": undo (c)–(f) from `$EVAL/backups/`, removing the six arc skills from `~/.agents/skills` and the local arc server from each Claude Code run folder; fall back to `~/arc-eval/2026-10-20/` backups only if a file in `$EVAL/backups/` is missing.
- [ ] **Step 7:** Verify: every absolute path under `$EVAL` that the handout names exists, except `backups/`; `grep -c 2026-10-20 $EVAL/HANDOUT.md` matches only the fallback line; each prompt matches its task file (`diff` the extracted prompts against `yq -r '.tasks[].prompt'`).

### Task 6: Fold and finish the branch

- [ ] **Step 1:** Put one paragraph in `docs/ARCHITECTURE.md`'s eval section only if this work changed how the eval works (it should not); delete the spec and plan; commit.
- [ ] **Step 2:** `roborev review --branch --wait`; fix or answer Medium and above; close it. Open the pull request, `gh pr checks --watch`, link it to the Kata issue in Forge, comment the eval folder and handout path on the Kata issue.
