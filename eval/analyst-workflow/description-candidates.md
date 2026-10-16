# Candidate descriptions for `using-arcform`

The entry skill's description decides whether an agent reaches for arc at all, so three candidates are kept for the analyst-workflow eval to compare. The first is the one in `skills/using-arcform/SKILL.md` until the eval shows another does better. Each must stay at most 1,024 characters and is quoted in the frontmatter.

## 1. Data shapes and outcomes, then the reason, then the exclusions (in use)

Tries naming the inputs and outputs a user actually mentions, so the skill matches without the word "arc", and gives the agent the reason to prefer arc over a script.

> Use for any question about tabular data, such as a CSV, a spreadsheet, JSON records, a Parquet file or a table on a web page, and for any chart, summary, model, extract or report made from such data, even when the user never mentions arc or arcform. Works through arc (the `arc` command or the `arc mcp` tools) so every step is recorded in a Protocol the user can re-run, inspect in Brightfield, commit to git and share, instead of a throwaway script. Start here: it covers what a Protocol is, the safety model, an environment check, the size of the work, and which analyst skill to load next. Do not use it to read one value off a small table the user pasted into the chat, or for data that is not tabular (prose, images, audio, source code).

## 2. Displace the default habit

Tries naming the behaviour it replaces (reaching for pandas or a Python script), on the bet that an agent matches a description against what it is about to do rather than against the user's words.

> Use instead of writing a pandas, polars or Python script whenever the user wants something worked out from rows and columns: totals, trends, comparisons, a cleaned extract, a chart, a classification or a written report, from a CSV, Excel sheet, JSON file, Parquet file, database table or a table on a website. The work goes through arc (`arc` on the command line or the `arc mcp` tools), which records each step in a Protocol the user can re-run, open in Brightfield and share. Load this first in any data-analysis session; it checks the environment and routes to the scoping, profiling, building and presenting skills. Skip it for a single figure read straight off a few pasted lines, and for anything that is not tabular data.

## 3. Short and keyword-dense

Tries the fewest words with the most trigger terms, on the bet that a short description is read whole and weighted more heavily by skill routers that truncate.

> Analyse tabular data with arc. Triggers on CSV, Excel, spreadsheet, JSON records, Parquet, a web table, "analyse this data", "make a chart", "what does this dataset say", "build a report", data cleaning, data quality and aggregation, whether or not arc is named. Records the analysis as a re-runnable, shareable Protocol (CLI `arc` or MCP `arc mcp`) instead of a one-off script, and routes to the stage skills. Not for one value from a tiny pasted table, nor for non-tabular data.
