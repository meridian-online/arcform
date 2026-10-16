# Scoring sheet: the analyst workflow's release eval

Copy this sheet into the release pull request and fill it in; the runbook is `README.md` beside it. A task passes in an agent when the agent chose arc unprompted and the session ended with all seven outcomes.

## What each column means

Score each yes or no, with one line of evidence: a quote from the transcript, a file in the Protocol folder, or a tool call. Score what the folder and the transcript show, not what the agent claimed.

| Column | Yes when |
| --- | --- |
| **Arc** (chose arc unprompted) | The agent's analysis went through arc (the `arc` command or the arc MCP tools) and nothing but the installed skills pointed it there. No if it computed the answer in a script, a notebook or its head first, even if it moved to arc later. |
| **1. Sound** | Every step that produced the answer is recorded in the Protocol with a description, the session ran it, and it read the run result (the run's outcome, each step's status, each table's row count) and checked the table the answer comes from. |
| **2. Sized** | The agent said the size and why. A quick look: a short Protocol and one checkpoint, at the end. An analysis: a playback after scope and the quality findings after profile. A project: requirements written to `README.md` and agreed before building, and a checkpoint at each stage. |
| **3. Quality** | The agent found the data-quality issue the task's `notes` name (or a real one of similar weight) and said what it means for the answer. |
| **4. Say** | Before building, the agent played its plan back: the data source, the stages and what the analyst will get, with the size open to override. For an analysis or a project it waited, and the refinement from `notes` changed the plan or the requirements. |
| **5. Protocol** | A Protocol folder the analyst could commit or share: `arcform.yaml`, its `models/`, no step that reads a file by an absolute path, and `arc run` succeeds from a fresh copy. |
| **6. Answer** | The question is answered in plain words, with at least one next step the analyst could take. A session cut off by the time cap is a no. |
| **7. Visual** | At least one panel under `panels/` that draws in Brightfield and shows the answer or how it was reached, and the agent pointed the analyst at Brightfield to see the steps. Anything else found along the way is in a panel or in the report. |

## Results

One row per task and agent. Write `y` or `n` in each column, and put the evidence in the run's section below.

| Task | Agent | Arc | 1 | 2 | 3 | 4 | 5 | 6 | 7 | Pass |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| quick-penguins | Claude Code | | | | | | | | | |
| quick-penguins | Codex | | | | | | | | | |
| quick-penguins | Desktop chat | | | | | | | | | |
| quick-bad-drivers | Claude Code | | | | | | | | | |
| quick-bad-drivers | Codex | | | | | | | | | |
| quick-bad-drivers | Desktop chat | | | | | | | | | |
| quick-earthquakes-month | Claude Code | | | | | | | | | |
| quick-earthquakes-month | Codex | | | | | | | | | |
| quick-earthquakes-month | Desktop chat | | | | | | | | | |
| quick-global-temperature | Claude Code | | | | | | | | | |
| quick-global-temperature | Codex | | | | | | | | | |
| quick-global-temperature | Desktop chat | | | | | | | | | |
| analysis-co2-per-capita | Claude Code | | | | | | | | | |
| analysis-co2-per-capita | Codex | | | | | | | | | |
| analysis-co2-per-capita | Desktop chat | | | | | | | | | |
| analysis-brisbane-weather | Claude Code | | | | | | | | | |
| analysis-brisbane-weather | Codex | | | | | | | | | |
| analysis-brisbane-weather | Desktop chat | | | | | | | | | |
| analysis-green-taxi-tips | Claude Code | | | | | | | | | |
| analysis-green-taxi-tips | Codex | | | | | | | | | |
| analysis-green-taxi-tips | Desktop chat | | | | | | | | | |
| analysis-us-unemployment | Claude Code | | | | | | | | | |
| analysis-us-unemployment | Codex | | | | | | | | | |
| analysis-us-unemployment | Desktop chat | | | | | | | | | |
| project-airport-trips | Claude Code | | | | | | | | | |
| project-airport-trips | Codex | | | | | | | | | |
| project-airport-trips | Desktop chat | | | | | | | | | |
| project-seismic-2023 | Claude Code | | | | | | | | | |
| project-seismic-2023 | Codex | | | | | | | | | |
| project-seismic-2023 | Desktop chat | | | | | | | | | |

## Evidence

One section per run, in this form:

```markdown
### <task id> in <agent> (arc <version>, <date>)

- Arc: y/n. <evidence>
- 1. Sound: y/n. <evidence>
- 2. Sized: y/n. <evidence>
- 3. Quality: y/n. <evidence>
- 4. Say: y/n. <evidence>
- 5. Protocol: y/n. <evidence>
- 6. Answer: y/n. <evidence>
- 7. Visual: y/n. <evidence>
```

## Failure clusters

Group the failures by cause across tasks and agents. For each: the cause, the runs it hit, the smallest skill edit that fixes it, and the rerun after the edit.

| Cluster | Runs | Smallest skill edit | Rerun |
| --- | --- | --- | --- |
| | | | |

## Pass bar

*Proposed for review:* every task chooses arc in at least two of the three agents, and every project task ends with all seven outcomes.

- Tasks choosing arc in at least two agents: _ of 10
- Project tasks with all seven outcomes, in every agent: _ of 2
- Verdict: met / not met (and, if not met, the maintainer's decision)

## Dry run of the runbook (arc 0.2.0, 2026-10-16)

The runbook's first run, before any release: one quick task in one agent, to test the instructions rather than the skills. It is not a release result.

`quick-penguins` in Claude Code (headless, `claude -p` with the prompt verbatim; the skills installed in the task folder's `.claude/skills` beside the user's own skills; only `arc mcp` connected). Ten turns, 26 seconds. Result: **fail** (outcomes 4, 6 and 7).

| Task | Agent | Arc | 1 | 2 | 3 | 4 | 5 | 6 | 7 | Pass |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| quick-penguins | Claude Code | y | y | y | y | n | y | n | n | n |

- Arc: y. Its first call was the `using-arcform` skill; every data step after it was an arc MCP tool, with no script.
- 1. Sound: y. Two recorded steps with descriptions (`raw_penguins`, `mass_by_species`); `protocol_run` returned outcome `success` with 344 and 3 rows; it then queried `mass_by_species` and checked `body_mass_g` loaded as a number.
- 2. Sized: y. "This is a quick look, since one aggregate answers it"; a two-step Protocol and no checkpoint before the answer.
- 3. Quality: y. It read `NA` as null and reported "Two penguins (one Gentoo, one Adelie) have no recorded body mass, so I left them out", over 342 of 344 rows.
- 4. Say: n. It said the size and "I'll put it in a small Protocol", but not the data source or stages, and did not offer the size for override.
- 5. Protocol: y. `penguins/` holds `arcform.yaml`, two `models/` files and a `.gitignore` for run state; it reads the data by URL, and `arc run` on a fresh copy of those files ran both steps.
- 6. Answer: n. A clear answer ("Gentoo ... roughly 1.34 kg (36%) more than ... Chinstrap"), but no next step beyond running it again.
- 7. Visual: n. No panel written; Brightfield named only as somewhere to open the Protocol.

| Cluster | Runs | Smallest skill edit | Rerun |
| --- | --- | --- | --- |
| A quick look stops at the entry skill: the agent never loaded `scoping-an-analysis` or `presenting-findings`, so it skipped the playback's data source and override offer, the next steps and the panel those skills ask for | quick-penguins in Claude Code | In `using-arcform`'s "Sizes" section, add: "A quick look still starts with `scoping-an-analysis` and ends with `presenting-findings`; only profiling and building are cut short." | Not yet: proposed for the first release run |
