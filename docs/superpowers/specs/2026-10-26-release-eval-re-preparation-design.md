# Re-preparing the release eval against main

## Intent

The skills release (0.3.0) is tested by the analyst-workflow eval in `eval/analyst-workflow/`. A first preparation, on 20 Oct against an earlier main, was put back unused when Homebrew's DuckDB moved to 2.0 and the pinned arc 0.2.0 refused it. Main now carries the DuckDB 2.0 move and a sixth skill, `dashboard-from-data`. The eval sessions run from 2 to 6 Nov with the maintainer playing the analyst. This work makes everything ready for those sessions so the first one can start from the handout, and stops before anything is installed into the maintainer's own agents.

## Decisions taken with the maintainer

- The eval runs the ten tasks of `tasks.yaml` and the three of `dashboard-tasks.yaml`, each in Claude Code, Codex and a desktop chat client: 39 sessions.
- The nine dashboard runs are practice. Their results go in the release pull request and do not hold the release; the dashboard rubric stays *proposed for review*.
- The analyst pass bar in `score.md` was approved as proposed on 21 Oct.
- The preparation goes in a new eval folder, dated for the first session (2026-11-02), beside the 20 Oct one, built from main. The 20 Oct folder is left untouched: it holds the only backups of the agents' configs from before any eval change, which are what the maintainer restores from afterwards.

## Goals

1. One arc binary, built from main with the MCP feature, linked against Homebrew's DuckDB 2.0 library, pinned in the new folder. `arc --version` prints the version every skill's `arc_version` names.
2. The preflight passes for both task files against that binary, on this machine's `duckdb`, `finetype` and `uv`. A Protocol run through the pinned binary succeeds on Homebrew's `duckdb` (the 23 Oct failure was an engine refusal; this proves it is gone).
3. 39 empty run folders, one per task and agent. Each Claude Code folder holds the six skills, installed by the pinned binary with `--target`. Codex and desktop folders are empty.
4. The six skills, written by the pinned binary to a staging folder, zipped one per skill without the `.arc-skill` marker, for upload to the desktop client.
5. A handout for the maintainer, in the new folder, covering all 13 tasks: the setup steps that touch the agents (listed, not done), the analyst's script from the runbook, each task's prompt, its per-agent start command, its refinement, the dashboard differences, and where transcripts go. A matching restore section says how to put each agent back after the eval, to the state it is in on the morning of 2 Nov: the handout's first setup step copies the Codex and desktop configs into the new folder and lists Codex's user skills folder, and the restore uses those copies, falling back to the 20 Oct backups only if they are missing. *Builder's call:* the 20 Oct restore already ran on 23 Oct, so its backups describe a state the machine has moved on from (they would bring back five skills from an older arc).
6. The repo's runbook says what is now true: the analyst pass bar is approved.
7. The preflight's per-URL timeout no longer sits at the edge of the USGS 2023 query (about 29 s against a 30 s limit), raised in its own pull request.

## Non-goals

- Installing anything into the agents: no change to `~/.cargo/bin`, `~/.agents/skills`, `~/.codex/config.toml`, Claude Desktop's config or skills, or Claude Code's MCP registrations. The handout lists those steps for the morning of 2 Nov.
- Running any eval session, or scoring.
- Bumping the version, rolling the changelog or opening the release pull request; the release skill does those after the eval.
- Approving or changing the dashboard rubric.
- A preparation script in the repo (considered and declined; see Approaches).

## Anti-goals

- A session that starts on 2 Nov and finds a stale skill, the wrong binary, or a dataset gone. Every file in the new folder must come from the pinned build or from the task files on main.
- Anything in a run folder that names arc to the agent beyond the installed skills (no `CLAUDE.md`, `AGENTS.md`, `.mcp.json` or notes).
- Touching the 20 Oct folder.

## Constraints

- arcform is a public repo: tracked files, commits and pull request text carry no local paths, private planning references or the names of the maintainer's personal tools. Machine paths live in the handout (outside the repo) and on the tracking issue.
- One cargo build at a time on this machine (16 GB). A debug `--features mcp` build is enough; `bundled-duckdb` is not used.
- Homebrew's `duckdb` is 2.0.0_1: it reports `v2.0.0` and has `sql_tokenize` (checked 26 Oct). If it changes before 2 Nov the handout's first step re-checks it.
- The maintainer's zsh profile puts `~/.cargo/bin` ahead of everything, where arc 0.1.0 lives. The handout's install section deals with it the way 20 Oct did (move 0.1.0 aside into the new folder, symlink the pinned build), as a step for 2 Nov, with its restore.
- Open-Meteo is rate-limited: run the preflight for `tasks.yaml` no more than needed.

## Approaches

1. **Refresh the 20 Oct folder in place**: swap the binary, reinstall six skills over five, rebuild zips and handout. Least work; risks a leftover file from the old preparation and a folder whose name no longer says what it holds.
2. **A new dated folder built from main** (chosen): fresh binary, fresh run folders, skills and zips from that binary, a new handout that points at the 20 Oct backups for the restore. Slightly more setup; every file traces to the pinned build.
3. **Option 2 plus a committed preparation script**: makes the rebuild-and-reinstall after each skill fix one command. More public code to review before the runs, and parts of it are specific to this machine. Declined: reruns are few enough to follow the runbook by hand.

## Left to the builder

- The handout's wording and order, modelled on the 20 Oct handout.
- How the dashboard tasks' run folders and handout sections are laid out, as long as each of the 39 sessions has its own folder and start line.
- The preflight's new timeout value, and whether only the HEAD, the ranged GET, or both change.
- Whether the Protocol smoke run in goal 2 uses a fixture from the repo or a small Protocol written for it, kept outside the run folders.
