# The analyst workflow's release eval

This eval checks that the analyst skills under `skills/` do their job in real agents: given a data question that never mentions arc, the agent reaches for arc on its own and ends the session with the seven outcomes the skills promise. It is the release test for the skills.

It runs by hand before each release, because it needs paid agent sessions. It is not a CI job. Its scored results go in the release pull request.

| File | What it holds |
| --- | --- |
| `tasks.yaml` | Ten analyst tasks on public datasets: four quick looks, four analyses and two projects, each with the prompt to paste, the dataset's licence and notes for the evaluator |
| `preflight.sh` | Checks every dataset URL still answers and that arc matches its skills' version, before any paid session starts |
| `score.md` | The scoring sheet: one row per task and agent, the failure clusters, and the pass bar |
| `dry-run.md` | The runbook's first run: one quick task in Claude Code, scored, with the failure cluster it found |
| `description-candidates.md` | Alternative descriptions for the entry skill, for when the eval shows agents not choosing arc |

## The agents

Run every task in three agents:

1. **Claude Code**, with the skills installed and `arc mcp` connected, and its shell.
2. **Codex**, the same way.
3. **A desktop chat client** (Claude Desktop, for example) with only `arc mcp` connected and the skills uploaded, and no file access of its own. This is the no-file-access route: everything the agent writes, arc writes for it.

## 1. Preflight

On the release branch, after the version bump (the skills' `metadata.arc_version` must already equal the new version), build arc with the MCP feature and run the preflight against that binary:

```sh
DUCKDB_LIB_DIR=/opt/homebrew/lib cargo build --features mcp
ARC=target/debug/arc eval/analyst-workflow/preflight.sh
```

It prints `ok` or `FAIL` per check and exits non-zero on any failure. Do not start a session until it passes. When a dataset URL has gone, replace the task with another public dataset of the same size and format, record its licence in `tasks.yaml`, and run the preflight again. When a skill's version differs from arc's, the version bump is incomplete: see the release skill.

## 2. Set up each agent

Use the binary the preflight passed, and make sure an older `arc` is not ahead of it on `PATH` (`which arc`, `arc --version`): a `cargo install` from an earlier release in `~/.cargo/bin` is the usual culprit. The agent must also see `duckdb`, `finetype` and `uv`.

Install the skills and connect the server the way a user would, following `skills/using-arcform/references/agents/` for each agent. Use one route per agent, not both the plugin and the skills folder, or the agent holds two copies of every skill.

- **Claude Code:** `arc skills install --target <task folder>/.claude/skills` in each task's folder (or `--agent claude-code` once, for `~/.claude/skills`), and the server through `claude mcp add arc -- arc mcp` or an `--mcp-config` file.
- **Codex:** `arc skills install --agent codex` (it writes `~/.agents/skills`), and the server in `~/.codex/config.toml` with `tool_timeout_sec = 600`.
- **Desktop chat client:** zip each skill folder and upload it, and connect `arc mcp` by absolute path, as `desktop-chat-tools.md` shows. Turn off any file-system connector, so arc is the only route to files.

Nothing else may tell the agent about arc. Each task runs in a new, empty folder (not this repository), with no `CLAUDE.md`, `AGENTS.md` or other instructions that name arc. The agent's usual user settings stay as they are: an analyst's agent has other skills competing for the same question, and the eval should see that.

## 3. Run a task

Start a fresh session per task, in the task's empty folder, and paste the task's `prompt` verbatim. Then play the analyst, the same way in every agent:

- **At the plan playback** (an analysis or a project), give the refinement in the task's `notes`, in your own words. Agree to everything else as it stands.
- **At any other checkpoint**, agree and let it go on, unless the agent is about to answer a different question from the one asked; then restate the question once.
- **When the agent asks you to put the data somewhere** (the desktop client cannot fetch or copy files itself), download the dataset into a new folder named for the question and give it that folder's absolute path.
- **When the agent asks which tool to use**, say "whatever you think is best". Never name arc, Protocols or Brightfield first.
- **When the agent offers a report**, accept it for a project and decline it for a quick look.

Give each size a time cap, and stop a session that reaches it: 15 minutes for a quick look, 45 for an analysis, 90 for a project. A session that hits the cap fails outcome 6.

Keep the transcript (Claude Code and Codex can export one; for the desktop client, copy the conversation) and the folder the session left, outside the repository. Score from them, not from memory.

### Running a quick look headless in Claude Code

A quick look's only checkpoint is at the end, so in Claude Code it can run headless, with no one at the keyboard. Headless sessions cannot answer a checkpoint, so run analyses and projects interactively.

Lay out a run folder so that the agent sees only the task folder, not the binary, the server config or the transcript:

```text
run/
  bin/arc              the binary the preflight passed
  mcp.json             the server, by absolute path
  transcript.jsonl     written by the run
  work/                the task folder: the session starts here
    .claude/skills/    from arc skills install --target run/work/.claude/skills
```

`mcp.json` names the binary by absolute path and gives the server a `PATH` holding `duckdb`, `finetype` and `uv`:

```json
{"mcpServers": {"arc": {"command": "/abs/path/run/bin/arc", "args": ["mcp"],
  "env": {"PATH": "/abs/path/run/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"}}}}
```

Then, from `run/work`, with the prompt copied verbatim from `tasks.yaml` (`yq -r '.tasks[] | select(.id == "quick-penguins") | .prompt' tasks.yaml` prints it) and the 15-minute cap enforced by `perl`'s `alarm`, since macOS has no `timeout`:

```sh
PATH="$PWD/../bin:$PATH" perl -e 'alarm 900; exec @ARGV' \
  claude -p "<the task's prompt>" --output-format stream-json --verbose \
  --mcp-config ../mcp.json --strict-mcp-config --permission-mode acceptEdits \
  --allowedTools Bash Read Write Edit Glob Grep Skill WebFetch mcp__arc \
  > ../transcript.jsonl
```

The first line of the transcript (`"subtype":"init"`) lists the skills and MCP servers the session loaded: check the five skills and a connected `arc` are there before scoring. To read the session as text:

```sh
jq -r 'select(.type == "assistant") | .message.content[]
  | if .type == "text" then "TEXT: " + .text
    elif .type == "tool_use" then "TOOL: " + .name + " " + (.input | tostring)[:200]
    else empty end' ../transcript.jsonl
```

## 4. Open the panels in Brightfield

For every session that wrote panels, open its Protocol folder in Brightfield and check each panel draws. arc checks a panel's table and column names; only Brightfield shows whether it can draw the chart. A panel that does not draw fails outcome 7.

## 5. Score

Fill one row of `score.md` per task and agent: whether the agent chose arc unprompted, and each of the seven outcomes, yes or no, with one line of evidence each (a quote, a file, a tool call). Its "What each column means" table says what counts as a yes at each size; a quick look is held to the same seven outcomes as a project, at its own scale. Score what the session did, not what it said it did: check the Protocol folder, its `arcform.yaml`, its run state and its history, and run `arc run` on a fresh copy of `arcform.yaml` and `models/` for outcome 5.

A task passes in an agent when it chose arc unprompted and ended with all seven outcomes.

## 6. Cluster the failures and fix them

Group the failures by cause, not by task: "the description lost to the default data-analysis skill" is one cluster across every task it hit. For each cluster, propose the smallest skill edit that would fix it (one sentence in a description, a row in an error-to-fix table, a line in a reference), make it on the release branch, then rebuild arc with the MCP feature (`skills/` is embedded in the binary, so the old binary would reinstall the old skills), run the preflight again, reinstall the skills in each agent with the rebuilt binary the second preflight passed (`target/debug/arc skills install ...`, not an `arc` found on `PATH`, which may be older; copy it over `run/bin/arc` again for a headless rerun; for the desktop client, zip and upload each skill folder again), and only then run the failing tasks again in the agent that failed them. When agents keep choosing something other than arc, try the entry skill's other descriptions from `description-candidates.md` before writing a new one.

## 7. Report in the release pull request

Attach the filled `score.md` to the release pull request: the summary table, the pass bar's verdict, each failure cluster with the edit made, and the reruns. The release skill (`.claude/skills/release/SKILL.md`) has the step.

## The pass bar

*Proposed for review:* every task chooses arc in at least two of the three agents, and every project task ends with all seven outcomes in every agent. A release that misses it ships only with the maintainer's explicit decision, recorded in the release pull request.

## The datasets

Every dataset is public and its licence allows this use; each task's `licence` field names it. Two tasks read live data (`quick-earthquakes-month` reads a rolling 30-day feed, and Open-Meteo's archive is revised), so score those on method, not on the number. Open-Meteo's free API is for non-commercial use and is rate-limited; each preflight sends it a request (a HEAD, and a ranged GET if that fails), and that counts against the limit, so do not run the preflight in a loop.
