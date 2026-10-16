# Step kinds: which to record, and how

Every step goes into the Protocol through a record verb, and every record verb runs nothing. Take the first kind that fits.

| Order | Kind | When |
| --- | --- | --- |
| 1 | An operation arc holds | `sql_recognise` names one for your statement |
| 2 | A SQL step | Any other statement DuckDB parses |
| 3 | An operator | Fetching, entity resolution, embedding, projection, export or publishing |
| 4 | A shell command | Nothing above can do it |

The order is about what arc can see. An operation is a step arc knows by name: it knows what the step does, not only what it reads and writes, and shows it that way. A SQL step is read by arc's SQL introspection, so arc knows its inputs and outputs. An operator declares what it reads and writes and has its `with:` checked before anything runs. A shell command is opaque: arc sees neither what it reads nor what it writes unless you declare them, so it comes last.

## 1. An operation arc holds

Ask arc what it reads your statement as. When the reply names an operation (`filter-rows`, `sort-rows`), record that operation. `sql_record` does this for you: a statement `sql_recognise` names is recorded as the operation's step either way.

| Step | CLI | MCP |
| --- | --- | --- |
| What arc reads a statement as | `arc sql recognise "<statement>"` | `sql_recognise` with `sql` |
| The operations arc holds | `arc operation list --json` | `operation_describe` (no arguments) |
| What one operation takes | `arc operation describe <long-name>` | `operation_describe` with `operation` |
| Record it | `arc operation record <long-name> --on <table> --name <step> --arg <key>=<value> --description "<why>" --dir <dir>` | `operation_record` with `operation`, `on`, `name`, `arguments`, `description`, `dir` |

`SELECT *` from one table with a `WHERE` and no other clause is `filter-rows` on that table.

## 2. A SQL step

Record a `SELECT`. arc writes it as a model under `models/` that makes a table named after the step, so the step's name is the table's name. One `SELECT` per step: one table, one purpose.

| Step | CLI | MCP |
| --- | --- | --- |
| Record a statement as a step | `arc sql record "<select>" --name <step> --description "<why>" --dir <dir>` | `sql_record` with `sql`, `name`, `description`, `dir` |
| Try the `SELECT` first, read-only | `arc sql query "<select>" --limit 20 --dir <dir>` | `sql_query` with `sql`, `limit`, `dir` |

Trying a `SELECT` with `sql_query` is how you check it before recording; the answer still comes from the recorded step and a run, never from the query.

No verb rewrites a step's model in place. To change a step, delete it and record it again under the same name; it is recorded at the end of the list, so move it back before the steps that read its table.

| Step | CLI | MCP |
| --- | --- | --- |
| Delete step `<i>` (counting from 0) | `arc edit-protocol --dir <dir> delete steps[<i>]` | `protocol_edit` with `edit`, `path`, `dir` |
| Move a step | `arc edit-protocol --dir <dir> reorder steps <from> <to>` | `protocol_edit` with `edit`, `path`, `from`, `to`, `dir` |

## 3. An operator

Use an operator for work that is not a query over tables already in the database. arc 0.2.0 holds these; `operator_describe` with no arguments lists the current set.

| Purpose | Operators |
| --- | --- |
| Fetching | `http_fetch` (one file at an address), `html_link_discover` (the links on an index page), `archive_extract` (members of a zip), `gleif_ra_fetch` (GLEIF entities for one registration authority) |
| Entity resolution | `splink_resolve` |
| Embedding and projection | `text_embed`, `umap_project` |
| Export and description | `parquet_export`, `datapackage_describe`, `finetype_validate` |
| Publishing | `ducklake_publish` |
| A pinned Python script | `uv` (a script pinned by its digest, run with uv) |

Read the operator's `with:` before recording it, and pass it as JSON. `operator_record` checks it against the operator's schema and refuses a block that does not fit, naming the field, with the Protocol untouched.

| Step | CLI | MCP |
| --- | --- | --- |
| The operator's `with:` schema | `arc operation describe <operator>` | `operator_describe` with `operator` |
| Record it | `arc operator record <operator> --name <step> --with '<json>' --description "<why>" --dir <dir>` | `operator_record` with `operator`, `name`, `with`, `description`, `dir` |
| Change one value of its `with:` | `arc edit-protocol --dir <dir> replace steps[<i>].with.<key> <value>` | `protocol_edit` with `edit`, `path`, `value`, `dir` |

An operator's output file is declared by the operator, so do not add a `produces:` for it: a step that reads the file depends on its path. A fetch that should not re-download on every run takes a precondition, as brewtrend's do:

```yaml
- name: fetch_cask_30d
  description: Download cask install counts for the last 30 days
  op: http_fetch@1
  with:
    url: https://formulae.brew.sh/api/analytics/cask-install/homebrew-cask/30d.json
    out: data/cask/30d.json
  preconditions:
    - modified_after: { path: data/cask/30d.json, period: 24h }
```

Record a fetch (this records the step and runs nothing, so nothing is downloaded):

```sh arc-example
arc operator record http_fetch --name fetch_rates --with '{"url": "https://example.com/rates.csv", "out": "data/rates.csv"}' --description "Download the day's exchange rates"
```

```text expect
recorded step fetch_rates as http_fetch
```

Over MCP:

```json tool=operator_record
{"operator": "http_fetch", "name": "fetch_rates", "with": {"url": "https://example.com/rates.csv", "out": "data/rates.csv"}, "description": "Download the day's exchange rates", "dir": "/Users/me/work/rates"}
```

## 4. A shell command, last

Only when no operation, SQL step or operator can do the work: a tool arc has no operator for, such as zipping the exported files, or printing to the terminal (brewtrend's `show_top20`). No record verb writes a command step; append it to the spec as YAML, with its own `- ` line indented to match the other steps.

| Step | CLI | MCP |
| --- | --- | --- |
| Append a command step | `arc edit-protocol --dir <dir> append steps '<item>'` | `protocol_edit` with `edit`, `path`, `item`, `dir` |

Give it a `description:`, and declare what it reads (`depends_on:`) and writes (`produces:`), because arc cannot see either. Without a precondition a command step runs on every run.

```json tool=protocol_edit
{"edit": "append", "path": "steps", "item": "  - name: bundle_outputs\n    description: Zip the exported file for the user to send\n    command: zip -qj build/outputs.zip build/sales.parquet\n    depends_on: [build/sales.parquet]\n    produces: [build/outputs.zip]\n", "dir": "/Users/me/work/monthly-sales"}
```
