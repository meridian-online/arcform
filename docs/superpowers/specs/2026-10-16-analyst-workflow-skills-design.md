# Analyst workflow skills: design

## Intent

An analyst brings an agent a roughly tabular dataset, local or online, and a goal: an answer, a chart, a classification model, an extract or a report. Today the agent reaches for throwaway Python. We want it to reach for arc instead, and to say why: "I've used arcform to save tokens and to build you a process you can inspect in Brightfield, build on later, or share."

That needs three things to arrive together: a set of agent skills that carry an analytical workflow, the arc capabilities each stage of that workflow depends on, and packaging that reaches any agent. This design draws the line where they ship together, as one arc release, and names the skills and the capability work each one depends on. The work to reach the line runs as three workstreams (below), each planned separately.

The analyst can be anyone from a confident spreadsheet user to a data scientist. The skills must serve both: the first needs the agent to carry the method, the second needs to see that the method is sound.

## What the analyst ends the session with

These seven outcomes are the definition of done for a session, and the release test checks each one.

1. Confidence that the analytical framework is sound: every step is recorded, ran, and was checked.
2. Work sized to the question: a quick look gets a short Protocol and few checkpoints, a larger piece of work gets agreed requirements and more of them.
3. Awareness of the data's quality issues and what they mean for the answer.
4. A say in the process: the agent plays the plan back and the analyst can refine the requirements before it builds.
5. An arcform Protocol that can be stored, committed to git or shared.
6. The answer, and next steps.
7. A visual account of how the answer was derived, and of anything else found along the way.

## Goals

- An agent given tabular data and a question chooses arc unprompted, in any supported agent.
- One workflow takes the analyst from ingress through analysis and modelling to visualisation and egress, at a size chosen up front.
- Every skill shows the CLI and MCP forms side by side, so an agent with a shell and an agent with only `arc mcp` can both follow it. "Only `arc mcp`" includes a chat app with no file access of its own, such as a desktop client with arc connected: every file the workflow produces must be writable through an arc tool.
- Charts are written in the form Brightfield opens, so the guided route ends in Brightfield. A PDF report, with the charts drawn in it, is the alternative for someone who will not work in Brightfield.
- The skills, the arc capabilities they name, and the packaging release together and are tested together, alongside a Brightfield release that opens an agent-written Protocol with its panels and exports the PDF.

## Non-goals

- Model training (for example CatBoost). The modelling stage at this line is what the existing operators do: entity resolution, text embedding and projection, and Python through the `uv` operator.
- New SQL operations beyond those an analysis at the line needs.
- The registry's distribution phase.
- Operator recipe skills (entity resolution, embedding a text column into a projection, publishing) as separate skills. They follow this line; at the line the build skill's decision table points at the operators and `operator_describe`.
- Routing hooks of the kind Databricks ships. The skill descriptions and the MCP `instructions` do the routing at this line.

## Anti-goals

The work has failed, even if it technically runs, if:

- the skills spend their words teaching DuckDB SQL, which agents already know, instead of arc's own method, verification and traps;
- the agent writes analysis code outside the Protocol for a step arc can record, so the analyst's committed Protocol does not reproduce the answer;
- the answer's charts live only in the chat;
- a skill names a tool, argument or verb the installed `arc` does not have;
- the workflow is tied to one agent.

## Constraints

- Public repository: no private planning identifiers in committed files.
- arc stays local-first: no cloud service in the path, including for PDF output.
- The CLI is the canonical surface. Every MCP tool maps to a CLI capability; a new MCP tool needs its CLI verb.
- The finetype federation keeps its version floor (`src/finetype_cli.rs`), and the skills state it.
- Skills install at the binary's version, so a skill never describes a newer or older arc than the one running. Channels that serve the repository rather than the binary (skills.sh, GitHub, plugin directories) can drift from the user's arc, so each skill states the arc version it was written for, and the entry skill's environment check compares it with `arc --version` and tells the agent to run the install command when they differ. *My call.*

## The workflow

The skills carry one process, modelled on how superpowers carries a development process: a short entry skill that routes, then one skill per stage, each with a checkpoint where the analyst can redirect. The tool underneath stays thin; the method is in the skills.

| Stage | What happens | Checkpoint with the analyst |
|---|---|---|
| Scope | Restate the question and the end state, choose the size, agree the requirements | The plan played back before anything is built (sizes below) |
| Profile | Fetch or load, type every column with finetype, describe each table, write a schema and validate against it | The data-quality findings and what they mean for the question |
| Build | Record steps into a Protocol, layered from raw, to cleaned and conformed, to the aggregated tables the analysis reads; one purpose per step | None by default at the small size; at larger sizes, when the model changes grain or drops rows |
| Check | Run, read the run result, trace any empty or wrong table upstream, undo through history | Anything that changes the answer's meaning |
| Present | Charts as Brightfield panels, the answer and next steps, and a report when the analyst wants one | The answer, how it was derived, and what else was found |

The three layers of the build stage follow the medallion convention (raw as it landed, cleaned and conformed, aggregated) and dbt's rule of one purpose per model. The skill teaches them as arc practice in plain words, not as either tool's vocabulary.

### Sizes

The scope skill picks one and says it; the analyst can override.

- **A quick look.** A question one or two aggregates can answer. Minimal profiling, a short Protocol, one checkpoint at the end. The Protocol is still written, because it is the record of how the answer was reached.
- **An analysis.** The default. All five stages, with the plan played back after scope and the quality findings shown after profile.
- **A project.** Requirements written into the Protocol folder (in its README) and agreed before building; a checkpoint at each stage; a report at the end.

## The skills

Five skills ship at the line. Names are the builder's call within one rule: each says what it does in the analyst's words, not arc's. Each SKILL.md is short and routes to reference files loaded only when a step needs them, as Databricks' skills do.

1. **Entry and guide to arc.** Its description is the most important text in the set: it must make an agent choose arc for any tabular data question, a chart or a report, not only when arc is named, and it must say when not to use arc. Its body covers: what a Protocol is; the CLI-to-MCP verb map; the safety model (recording runs nothing, a run runs, history undoes); where run results land; an environment check (arc version, an `mcp` build, finetype at or above its floor); the sizes; and routing to the other four.
2. **Scope the analysis.** Restate the goal, choose the size, play the plan back, write the requirements for a project.
3. **Profile and type the data.** finetype's verbs through arc: `profile`, a table-level JSON Schema saved to a file, `validate` against it, and not stopping after `profile`; `table_describe` for tables already in a Protocol; what each result means for the analyst. It also absorbs what finetype's own two agent skills teach, routing each piece through arc or saying it is not reachable.
4. **Build and check a Protocol.** Discover first; a decision table for the step kind (a catalogue operation when `sql_recognise` names one, else a SQL step, an operator for fetch, resolution, embedding or export, a shell command last); a description on every step; run; read the run result rather than the exit status (`run.outcome`, then each step's status, then each asset's row count); trace an empty or wrong table upstream; offer `--force` rather than default to it; undo through history. `examples/brewtrend` is the worked Protocol.
5. **Present the findings.** Write each chart as a Mosaic spec at `panels/<name>.yaml` in the Protocol folder, which Brightfield opens and `arc history` versions; state the answer, the quality caveats from profile, and next steps; produce a report on request, as Markdown or PDF. Guide toward opening the Protocol in Brightfield; offer the PDF to someone who will not.

Each skill states the arc version it was written for (the constraint above), ends with when to switch to another skill, and puts its known traps in an error-to-fix table.

## Capability work arc needs before the line

Each item below is a dependency of a skill above.

1. **An MCP route to every verb a skill tells an agent to use.** Today an MCP-only agent cannot add an operator step (only `arc edit-protocol` can, as a raw YAML edit), restore from history, or edit a step. That blocks fetching, resolution, embedding and export over MCP. The minimum is a tool that records an operator step, checking its `with:` against the operator's schema, with a matching CLI verb; and history list and restore as tools. The builder enumerates the verbs the five skills use and closes each gap.
2. **An arc tool for every file the workflow writes.** An agent in a chat app cannot write files itself, so arc writes them: the JSON Schema `validate` reads (today `profile` returns it inline and `validate` needs a path), each panel, the project's requirements README, and the report. Each tool has its CLI verb, writes only inside the Protocol folder, and is recorded in history like any other machine edit.
3. **Profiling a table, not only a CSV file.** finetype's `profile` reads CSV files only. Online data, JSON, spreadsheets and tables already in a Protocol need a route: an arc verb and tool that profiles a table in the Protocol's database, and one that validates it against the saved schema in DuckDB with finetype's extension (`ft_validate`), so the skill's rule is "load it into a raw step, then profile and validate the table". The route (the `duckdb-finetype` extension in a SQL step, or an export to CSV under the hood) is the builder's call. *My call: the design said nothing on this; the profile stage cannot work for non-CSV inputs without it.*
4. **Charts as panels.** arc draws no chart. The agent composes the Mosaic spec, arc writes it to `panels/<name>.yaml` (item 2), history versions it, and a check reports a panel that names a table or column the Protocol does not make. *My call: arc checks names only; it does not check the spec against what Brightfield can draw. The skill names the chart kinds Brightfield draws, and the release eval opens every panel in Brightfield, which catches a spec Brightfield cannot draw.*
5. **A report, written by a verb, not a step.** *My call:* the report is a snapshot of one session's findings, so it is a file beside the Protocol, versioned by history, not a step that a re-run rewrites. The agent supplies the prose (the answer, quality caveats, next steps); arc adds the parts it knows (the steps that derived the answer, their row counts, the run it came from, the panels) and writes `report.md`. The Protocol stays the reproducible record; the report cites the run it describes.
6. **The install command.** An `arc` verb that installs the skills matching the binary's version into the skills directory of each agent it detects, and prints where it wrote them.

## Brightfield's part

Brightfield is a separate repository with its own plan, but the line depends on it: a Brightfield release that opens a Protocol an agent wrote, draws its `panels/`, and exports the report to PDF with the charts drawn in it, run headless so an agent can ask for it. Brightfield already depends on arc, so it is the one place that both draws charts and can read a report; arc gains no renderer.

## Packaging and discovery

- The skills live at the repository root in a user-facing `skills/` directory, one folder per skill, apart from the maintainer `release` skill under `.claude/skills/`. That layout is what skills.sh and GitHub-based installers read.
- The repository carries plugin manifests for the agents superpowers supports where they are cheap to add (at least Claude Code, Codex, Cursor and Gemini), generated from one source and checked for drift in CI.
- Per-agent tool notes, as superpowers' `references/<agent>-tools.md` do, cover the few places where agents differ.
- `arc mcp`'s `initialize` instructions name the entry skill and the workflow, so an MCP-only client with no skill support still gets the method in outline.
- Discovery is measured across skills.sh, Anthropic's plugin directory, GitHub and agent citations (tracked through DataforSEO). Tracking is outside this design; the packaging must not prevent any of these channels.

## Verification

- **Structural, in CI:** each skill names only tools and arguments `tools/list` advertises from an `mcp` build, and only CLI verbs and flags `--help` lists; frontmatter passes a validator that rejects what common skill loaders silently drop; generated plugin manifests match their source.
- **Examples run:** each command or tool call a skill shows returns what the skill says, against the released binary.
- **The release test, an agent eval:** a fixed set of analyst tasks (public tabular datasets, each with a question, spread across the three sizes) run in at least two agents. A task passes when the agent chose arc without being told to and the session ended with all seven outcomes. Failures are clustered and fixed with the smallest skill edit, as Databricks does. The pass bar is the builder's to propose. *My call:* the eval is a manual step before the release, run in Claude Code and Codex, with one more agent (a desktop chat client with only `arc mcp`) for the no-file-access route; its results go in the release pull request. It is not a CI job, because it needs paid agents.

## Workstreams

1. **Skills and process:** the five skills, references, per-agent notes.
2. **arc capabilities:** the MCP routes, the file-writing tools, table profiling, the panel check, the report and the install command. Brightfield's release (opening agent-written panels, headless PDF export) is planned in Brightfield and tracked as a dependency.
3. **Packaging and evaluation:** directory layout, plugin manifests, the structural CI checks, the agent eval and its task set.

Workstream 1 can draft against the capabilities as specified here, but no skill text is final until the verbs it names exist.

## Left to the builder

- Skill names and the split of each skill's material between SKILL.md and references.
- The exact tool and verb names for the new capabilities, within the CLI-canonical rule.
- Where the panel check runs.
- The eval task set and pass bar.
