# Dry run of the runbook (arc 0.2.0, 2026-10-16)

The runbook's first run, before any release: one quick task in one agent, to test the instructions rather than the skills. It is not a release result, and it is kept apart from `score.md` so that copies of the sheet start empty.

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
