# Scoring sheet: the dashboard eval

*Proposed for review.* This rubric waits on the maintainer's approval before any release is scored against it. Copy the sheet into the release pull request and fill it in; the runbook is `README.md` beside it, in its "Dashboard tasks" section, and the tasks are `dashboard-tasks.yaml`.

A dashboard task asks the analyst to see what is in a dataset, not to answer a question, so it is scored on different outcomes from the ones in `score.md`. A task passes in an agent when every outcome below is yes.

## What each column means

Score each yes or no, with one line of evidence: a quote from the transcript, a file in the Protocol folder, or a tool call. Score what the folder and the transcript show, not what the agent claimed.

| Column | Yes when |
| --- | --- |
| **Arc** (chose arc) | The agent's work went through arc (the `arc` command or the arc MCP tools) and nothing but the installed skills pointed it there. No if it computed or charted the data in a script or notebook first, even if it moved to arc later. |
| **1. First picture** | The analyst had something to look at within 5 minutes of the prompt being sent: a first panel written, or the agent telling the analyst to open the data file or Protocol folder in Brightfield, before the exploration was finished. Time it from the prompt to that message,, less the time spent downloading the data. A session that explored in silence and showed everything at the end is a no. |
| **2. Claim checked** | The session states what the data says as a claim, and an `arc sql query` or `sql_query` it ran shows the numbers the claim rests on. Every figure in the claim appears in a query result. If the data has no story, a session that hands back the profile and says why is a yes. |
| **3. Quality** | The dashboard holds a quality view in the panel: the data's missing values, failing rows or suspect columns, drawn beside the views and tied to what they mean for the claim. The issue the task's `notes` name, or a real one of similar weight, is in it. |
| **4. One question** | The agent asked at most one question before it built, and only when it helped (two credible stories, or tables whose relationship it could not confirm). Otherwise it acted and said what it assumed. Two or more questions, or a question the data could have answered, is a no. |
| **5. No template** | The views follow from this data. Across the three tasks the dashboards differ in which views they hold and why, and none is a fixed set (a KPI row, a trend and a table; a histogram per column) that appears in all three. Score this once per agent, on its last task, for the three tasks together. An agent that did not attempt all three tasks scores `n` here, because there is nothing to compare. |
| **Time cap** | A session that reaches the 15-minute cap without a panel that `panel_check` passes scores `n` on outcome 6, and so fails the task. Outcomes 1 to 4 are scored on what the session did before the cap. The download of a data file counts toward the cap, since the analyst waits through it. |
| **6. Panel check** | `arc panel check` (`panel_check`) comes back clean on the final panel, and the panel draws in Brightfield. |

## Results

One row per task and agent. Write `y` or `n` in each column, and put the evidence in the run's section below. The Pass column is `y` when every outcome except 5 is `y`. Outcome 5 is judged once per agent, on its last task, so the Pass on the agent's earlier rows is provisional until then, and a `n` on outcome 5 turns all three of that agent's rows to `n`.

| Task | Agent | Arc | 1 | 2 | 3 | 4 | 5 | 6 | Pass |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| dashboard-gistemp | Claude Code | | | | | | | | |
| dashboard-gistemp | Codex | | | | | | | | |
| dashboard-gistemp | Desktop chat | | | | | | | | |
| dashboard-retail-sales | Claude Code | | | | | | | | |
| dashboard-retail-sales | Codex | | | | | | | | |
| dashboard-retail-sales | Desktop chat | | | | | | | | |
| dashboard-flight-delays | Claude Code | | | | | | | | |
| dashboard-flight-delays | Codex | | | | | | | | |
| dashboard-flight-delays | Desktop chat | | | | | | | | |

## Evidence

One section per run, in this form:

```markdown
### <task id> in <agent> (arc <version>, <date>)

- Arc: y/n. <evidence>
- 1. First picture: y/n. <evidence, with the minutes from the start>
- 2. Claim checked: y/n. <the claim, and the query that shows its numbers>
- 3. Quality: y/n. <the quality view and the issue it names>
- 4. One question: y/n. <the questions asked, if any>
- 5. No template: y/n. <last task only: the views each of the agent's three dashboards holds, side by side>
- 6. Panel check: y/n. <panel_check result; whether Brightfield drew it>
```

## Pass bar

*Proposed for review:* every task chooses arc in at least two of the three agents, and every task in every agent ends with outcomes 1 to 4 and 6 as yes, with outcome 5 yes for each agent's three dashboards.

- Tasks choosing arc in at least two agents: _ of 3
- Task runs with outcomes 1 to 4 and 6: _ of 9
- Agents whose three dashboards differ: _ of 3
- Verdict: met / not met (and, if not met, the maintainer's decision)
