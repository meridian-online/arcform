# Dry runs of the runbook (arc 0.2.0)

Runs made before any release. The first, below, is one quick task in one agent, to test the instructions rather than the skills; the later sections, each dated, rerun tasks in Claude Code to test skill edits. It is not a release result, and it is kept apart from `score.md` so that copies of the sheet start empty.

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
| A quick look stops at the entry skill: the agent never loaded `scoping-an-analysis` or `presenting-findings`, so it skipped the playback's data source and override offer, the next steps and the panel those skills ask for | quick-penguins in Claude Code | In `using-arcform`'s "Sizes" section, add: "A quick look still starts with `scoping-an-analysis` and ends with `presenting-findings`; only profiling and building are cut short." | Made, then strengthened; rerun twice below. Scoping and presenting now load and a panel is written; the override offer and an offered next step are still missing |
| The size is never offered for override: the agent copies `scoping-an-analysis`'s quick-look example, which said the size and plan but not "Say if you want more than that" | quick-penguins in Claude Code (both reruns) | Add the override sentence to that example | Made before the second rerun; the agent still left it out |

## Reruns after the edit (arc 0.2.0, 2026-10-16)

Same setup as above, arc rebuilt with the MCP feature, preflight passed and the skills reinstalled from the rebuilt binary before each rerun. Both runs used the `arc` command through Bash rather than the MCP tools.

**Rerun 1**, with the sentence as proposed. Nine turns, 37 seconds. Result: **fail** (outcomes 4, 6 and 7). It loaded `scoping-an-analysis` after `using-arcform`, then never loaded `presenting-findings`.

- 4. Say: n. "This is a quick look, because one grouped average answers it. I'll put the CSV into a new Protocol, average `body_mass_g` by species and give you the gap." There was no offer to change the size.
- 6. Answer: n. A clear answer (Gentoo, 1,343 g more than Chinstrap), but the only next step was to open it in Brightfield or re-run it.
- 7. Visual: n. No panel.

**Rerun 2**, with "loaded before you give the answer" added to the sentence and the override sentence added to scoping's quick-look example. Sixteen turns, 45 seconds. Result: **fail** (outcomes 4 and 6; 7 not yet confirmed). It loaded all five skills in order.

| Task | Agent | Arc | 1 | 2 | 3 | 4 | 5 | 6 | 7 | Pass |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| quick-penguins | Claude Code | y | y | y | y | n | y | n | ? | n |

- Arc: y. `using-arcform` first, then every data step through `arc`, with no script.
- 1. Sound: y. Three recorded steps with descriptions; it ran after each one and queried `mass_gap_by_species`, the table the answer comes from.
- 2. Sized: y. "This is a quick look: one mean per species answers it", with no checkpoint before the answer.
- 3. Quality: y. It read `NA` as null and reported "Two penguins have no body mass recorded (one Adelie, one Gentoo), so 342 of the 344 are averaged".
- 4. Say: n. "I'll load the CSV into a new Protocol, average `body_mass_g` by species, and give you the gap": the source, the plan and the result, but still no offer to change the size.
- 5. Protocol: y. `arcform.yaml`, three `models/` files, a schema and a panel; it reads the CSV by a relative path, and `arc run` on a fresh copy ran 3/3 steps.
- 6. Answer: n, held to the same bar as the first run. The answer is clear, but the next step is only implied, never offered: "too small to say either is heavier without checking each sex separately".
- 7. Visual: not confirmed until the panel is opened in Brightfield. `panels/mass_by_species.yaml` is a bar chart of the averages, `arc panel check` came back clean, and it said "open `penguin-mass/` in Brightfield".

The release run should open the panel in Brightfield to settle outcome 7, and check whether outcomes 4 and 6 still fail. If outcome 6 still fails, `presenting-findings` should ask for the next step to be offered outright. If outcome 4 still fails, the next edit goes in `scoping-an-analysis`'s rule rather than its example: on a quick look the override offer is part of the sentence that gives the size, and leaving it out is the trap.

## The ten tasks, unattended, and the quick looks rerun (arc 0.2.0, 2026-10-17)

All ten tasks ran headless in Claude Code, scored for outcomes 4 and 6 only. Each prompt carried a line the runbook does not allow: "I won't be around to answer questions, so go ahead without checking back, make your own sensible calls, chart what helps, and write me a report at the end." That line made every quick look a project, because a report was asked for, and told every agent not to wait, so outcome 4 could not be judged fairly. The runbook now says to add nothing to a prompt.

| Task | 4. Say | 6. Answer |
| --- | --- | --- |
| quick-penguins | n | n |
| quick-bad-drivers | n | y |
| quick-earthquakes-month | n | n |
| quick-global-temperature | n | y |
| analysis-co2-per-capita | n | n |
| analysis-brisbane-weather | n | n |
| analysis-green-taxi-tips | n | y |
| analysis-us-unemployment | n | y |
| project-airport-trips | n | y |
| project-seismic-2023 | n | n |

- 4. Say: no run offered the size for override. Seven said the size and that they would not wait ("I treated this as a project with a written report, because you asked for one. I didn't wait for you to approve the plan.", quick-bad-drivers); three played nothing back.
- 6. Answer: half offered a next step outright ("Worth doing next: Add more months to see whether the evening peak and the Bronx gap hold across the year", analysis-green-taxi-tips). The other half ended at opening, re-running or sharing the Protocol ("you can open it in Brightfield to see the charts and each step, re-run it, or share it", analysis-co2-per-capita).

**Rerun 1**, the four quick looks with the prompts verbatim, after `presenting-findings` was edited to ask for at least one next step offered outright. All four were sized as quick looks.

| Task | Skills loaded | 4. Say | 6. Answer |
| --- | --- | --- | --- |
| quick-penguins | `using-arcform`, `scoping-an-analysis` | n | n |
| quick-bad-drivers | all but `building-and-checking-a-protocol` | n | y |
| quick-earthquakes-month | `using-arcform`, `scoping-an-analysis`, `presenting-findings` | n | y |
| quick-global-temperature | all but `building-and-checking-a-protocol` | n | y |

- 4. Say: three gave the size and the plan without the offer ("Quick look: one mean per species answers this. I'll download the CSV into a new Protocol, average `body_mass_g` by species, and give you the gap.", quick-penguins); quick-global-temperature played nothing back.
- 6. Answer: every run that loaded `presenting-findings` offered a next step ("A possible next step is to compare each record against the trend line", quick-global-temperature). quick-penguins never loaded it and offered none.

**Rerun 2**, after `scoping-an-analysis`'s rule for a quick look was edited to end the size's message with the offer to override it.

| Task | Skills loaded | 4. Say | 6. Answer |
| --- | --- | --- | --- |
| quick-penguins | `using-arcform` only | n | n |
| quick-bad-drivers | all five | n | y |
| quick-earthquakes-month | all but `building-and-checking-a-protocol` | n | y |
| quick-global-temperature | `using-arcform`, `presenting-findings` | n | y |

- 4. Say: still no offer. quick-penguins and quick-global-temperature never loaded `scoping-an-analysis` ("This is a quick look: one aggregate answers it. The run will fetch the CSV from GitHub."); quick-bad-drivers and quick-earthquakes-month loaded it and played nothing back before building.
- 6. Answer: again every run that loaded `presenting-findings` offered a next step ("Next step. Panama has few M7+ events, so an M7.7 there probably set off aftershocks. Counting the feed's events within about 200 km of the main shock would show whether they account for much of the 620.", quick-earthquakes-month), and quick-penguins, which did not, offered none.

| Cluster | Runs | Smallest skill edit | Rerun |
| --- | --- | --- | --- |
| The answer ends without a next step beyond opening, re-running or sharing the Protocol | Five of the ten unattended runs | In `presenting-findings`, ask for at least one next step offered outright, and say opening, re-running or sharing the Protocol is not one | Made; in both reruns every run that loaded the skill offered one (six of six) |
| A quick look skips `scoping-an-analysis` or `presenting-findings`, despite `using-arcform` saying both are loaded | quick-penguins in both reruns, quick-global-temperature in rerun 2 | Not yet found: the sentence in `using-arcform` is already there, and quick-penguins skipped more skills in rerun 2 than rerun 1 | — |
| The quick look's size is never offered for override, and often no plan is played back at all | All four quick looks in both reruns | Made: the offer moved from `scoping-an-analysis`'s example into its rule and its trap row | No change; none of the eight runs offered it, including the two that loaded the skill after the edit |

Outcome 4 for a quick look is not fixed by wording in `scoping-an-analysis`, because half the runs never load it and the rest skip the playback. The next thing to try is in `using-arcform`, which every run loads: put the playback sentence, with its override offer, in that skill's own steps for a quick look, so it does not depend on a second skill being loaded. The analyses and projects still need the interactive runs the runbook prescribes before their outcome 4 can be scored.
