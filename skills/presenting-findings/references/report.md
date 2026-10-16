# The report: what you write, and what arc adds

`report_write` (`arc report write`) writes `report.md` in the Protocol folder from two parts: your prose first, then three sections arc writes from the run. Write it when the user wants a document, and at the end of a project. It replaces a report already there, keeping the old one in the file's history.

| Step | CLI | MCP |
| --- | --- | --- |
| Write the report from the newest run | `arc report write --dir <dir> < prose.md` | `report_write` with `prose`, `dir` |
| Write it from an earlier run | `arc report write --run <run-id> --dir <dir> < prose.md` | `report_write` with `prose`, `run`, `dir` |
| List the report's recorded versions, to find the id to restore | `arc history list --json --file report.md --dir <dir>` | `history_list` with `file`, `dir` |
| Roll the report back | `arc history restore <id> --file report.md --dir <dir>` | `history_restore` with `id`, `file`, `dir` |

Write the report after the run you are presenting: arc reads that run's record, so a report written before a re-run describes the old one. A Protocol that has not run is refused, and so is empty prose, with nothing written.

## What goes in the prose

The prose is yours, in the user's words, and it comes first. Leave your own `##` headings out: arc's sections follow it and carry theirs.

1. **The answer**, in the first sentence, with its number and its unit: "Travel took 38% of spend in 2026, more than any other category." Name the table it comes from, so a reader can find it in the derivation below.
2. **The caveats from profiling** that bear on the answer: rows dropped and how many, columns that failed validation, a source that covers only part of the period, a profile taken from a sample. Say what each does to the answer, not only that it exists.
3. **What else was found**, briefly: anything the user did not ask about but would want to know.
4. **Next steps**: the question the answer raises, the data that would settle a caveat, the step to add next.

Keep the prose short; the derivation below carries the detail. Put no number in the prose that no table holds: every figure should be one the Protocol computes.

## What arc adds

- **How this was derived**: each step in order, with its description, its status in the run, and the tables it made with their row counts. This is why every step needs a description: a step without one appears here as a bare name.
- **Charts**: each panel under `panels/`, with its `title`, and anything `panel_check` would report against it, marked `Check:`. The panels are checked against the Protocol's current database, not the run's. A finding here means a chart names a table or column the Protocol does not make: fix the panel and write the report again before sharing it.
- **Run**: the run's id, when it finished, its outcome, and each asset's content hash, so a reader can tell whether a later run produced the same tables.

## The PDF

Brightfield is where the report becomes a PDF with the charts drawn in it. The Brightfield release that exports it headless, so an agent can ask for it, is planned and has not shipped: Brightfield has no export command yet to name. Until it ships, give the user `report.md` and the Protocol folder, and say the PDF comes from Brightfield's export once that release is out. Do not make a PDF another way: a chart drawn by a separate tool is not the panel the Protocol holds.
