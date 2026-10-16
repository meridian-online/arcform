---
name: scoping-an-analysis
description: "Use at the start of every data question handled through arc, before any data is loaded or any step recorded, and again whenever the user changes the question. Restates the question and the end state in the user's words, chooses the size of the work (a quick look, an analysis or a project) and says it, plays the plan back so the user can redirect it, writes agreed requirements into the Protocol's README.md for a project, and creates the Protocol. Keeps a one-number question quick and gives a shared report the requirements it needs."
metadata:
  arc_version: "0.2.0"
---

# Scoping an analysis

Scope decides how much process the question gets. Do it first, even for a quick look: a quick look stays quick because you said so up front, and a project gets its requirements agreed before anything is built rather than after.

Scoping records no analysis steps and runs nothing. It ends with a Protocol folder (and, for a project, its `README.md`), ready for `profiling-and-typing-data`.

## 1. Restate the question and the end state

In one or two sentences, in the user's words, say what they asked and what they will have at the end: a number, a table, a chart, a report. If you cannot say what the end state is, ask that one question before going on; do not ask a list.

## 2. Choose the size

Apply these in order and stop at the first that fits:

1. **A quick look** when one or two aggregates answer it ("total sales last month", "how many rows have no email") and the user has not asked for a report. A large, messy or unfamiliar file does not change this: profiling will find the mess, and that is not a reason to add ceremony.
2. **A project** when the user wants a report, the work spans several datasets, or the answer will be shared beyond the conversation (a board pack, a team, a client). Sharing alone does not lift a question that one or two numbers answer: that stays a quick look by rule 1. When the user says such a number is for a deck or a client, offer the larger size and let them choose.
3. **An analysis** otherwise. It is the default: a question that needs a comparison, a trend, a breakdown or a judgement about the data.

Say the size in one line with its reason, and let the user override it: "This is a quick look: one total answers it. Say if you want more than that." Take an override without argument.

| Size | Checkpoints | Requirements | Ends with |
| --- | --- | --- | --- |
| A quick look | One, at the end | The restated question is enough | The answer and the Protocol that reached it |
| An analysis | After scope (the plan), after profile (the quality findings), at a change of grain while building, and at present | Played back, not written | The answer, its caveats and next steps |
| A project | At every stage | Written into `README.md` and agreed | A report, as well as the answer |

## 3. Play the plan back

Before anything is built, tell the user three things: the data source (the file, URL or table, and how it gets in), the stages you will run, and what they will get. Keep it to a few lines.

- **A quick look:** state the plan in the same message as the size and carry on; the checkpoint is at the end.
- **An analysis:** wait for the user to agree or redirect.
- **A project:** the plan becomes the requirements (step 4); wait for agreement on those.

## 4. Create the Protocol, and for a project write the requirements

Create the Protocol in its own folder, named for the question, with the data inside it, so that it runs on any machine. If you have no file access of your own (an agent reaching arc only through `arc mcp`), you cannot copy the data in: ask the user for the folder that holds the data, or to move the file into the working folder, then create the Protocol in that folder and load the file by a path relative to it (`sales.csv`, not its absolute path). A step that reads a file by its absolute path ties the Protocol to this machine. Then, for a project only, write the requirements into its `README.md`: the question, the end state, the data sources, what is in and out of scope, the checks the answer must pass, and who will read it. Show the user the README and have it agreed before profiling. If they change it, write it again: each write is kept in history.

| Step | CLI | MCP |
| --- | --- | --- |
| Create the Protocol | `arc create-protocol <dir> --name <name>` | `protocol_create` with `dir`, `name` |
| Write the requirements | `arc protocol write-file README.md --dir <dir> < README.md` | `protocol_write_file` with `path`, `content`, `dir` |
| See earlier versions of the README | `arc history list --json --file README.md --dir <dir>` | `history_list` with `file`, `dir` |

Over MCP, pass `dir` as the folder's absolute path and the README's text as `content`:

```json tool=protocol_write_file
{"path": "README.md", "content": "# Monthly sales by category\n\n...", "dir": "/Users/me/work/monthly-sales"}
```

The same two steps from a shell, run in a folder that already holds another Protocol:

```sh arc-example
arc create-protocol monthly-sales --name monthly-sales
arc protocol write-file README.md --dir monthly-sales <<'EOF'
# Monthly sales by category

Question: how do sales move month to month in each category?
End state: a report for the board, with one chart per category.
Data: sales.csv (order_date, category, amount, email).
Out of scope: customer-level analysis; the email column is not used.
Checks: monthly totals sum to the file's total; no category is dropped.
EOF
```

```text expect
wrote README.md as a readme in monthly-sales
```

## Worked examples

**A quick look.** "What did we sell in total in January?" One sum over `sales.csv` answers it. Say: "This is a quick look: one total answers it. I'll load `sales.csv` into a new Protocol, sum `amount` for January and give you the figure with the Protocol behind it." Create the Protocol and go straight on; no README, no waiting.

**An analysis.** "Which categories are growing?" That needs monthly totals per category and a judgement about trend, so it is an analysis. Play back: the source (`sales.csv`), the stages (profile the columns, build monthly totals per category, check them, present a chart), and the end state (the growing categories, with the caveats profiling finds). Wait for agreement, then create the Protocol.

**A project.** "Put together the monthly sales report for the board, against this year's targets." A report, two datasets (sales and targets) and a shared audience: a project. Create the Protocol, write the requirements into its `README.md` as in the example above, show it, and build only once it is agreed.

## When to switch

- The size is said, the plan is agreed (or, for a quick look, stated) and the Protocol exists: load `profiling-and-typing-data`.
- The user changes the question part-way through any stage: come back here, restate it, check the size still fits, and keep the Protocol you have.
- You need the exact form of another verb: the verb map in `using-arcform`, at `../using-arcform/references/verb-map.md`.

## Traps

| Error or symptom | Fix |
| --- | --- |
| You are about to write a README, or wait for agreement, on a one-number question | It is a quick look. Say the size and the plan in one message and carry on |
| You sized the work up because the data looked messy | Size by the question, not the data. Profiling deals with the mess |
| `a spec already exists at <dir>/arcform.yaml` | A Protocol is already there. Use it, or create the new one in its own folder |
| `<dir> holds no arcform.yaml; make the Protocol first` | Create the Protocol before writing its README |
| `<path> is not a file arc writes` | `protocol_write_file` (`arc protocol write-file`) accepts only `README.md`, `schemas/<name>.json` and `panels/<name>.yaml`. The requirements go in `README.md` at the Protocol's top level |
| `report.md is written by the report_write tool, not this one` | The report goes through `report_write` (`arc report write`), at the present stage, not through `protocol_write_file` |
| The README landed in the wrong Protocol | `--dir` defaults to the current folder; pass the new Protocol's folder. Over MCP, `dir` must be absolute |
| The user rewrote the requirements and wants the old ones back | `history_restore` with `id`, `file` set to `README.md`, and `dir` (`arc history restore <id> --file README.md --dir <dir>`) |
