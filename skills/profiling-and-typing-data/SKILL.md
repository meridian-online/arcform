---
name: profiling-and-typing-data
description: "Use after scoping, once the Protocol exists and before any analysis step is built, whenever data needs to be understood before it is trusted: a CSV, JSON, a spreadsheet, a file at a URL or a table already in a Protocol. Gets the data into the Protocol, types every column with FineType through arc, saves a table-level JSON Schema into the Protocol's schemas folder, validates the data against it, describes row and null counts, and tells the user what the findings mean for their question. Do not stop after profiling: a profile alone catches no bad rows. Covers FineType's version floor and everything FineType's own agent skills teach, routed through arc."
metadata:
  arc_version: "0.2.0"
---

# Profiling and typing data

Profile tells you what the data is before you build on it: each column's type and how confident FineType is, a JSON Schema that becomes the data's contract, and which rows break it. The stage ends with a checkpoint: the quality findings and what they mean for the user's question.

The stage has five steps, and profiling is only the second. **Do not stop after profiling.** A profile reads a sample and catches no bad rows; validation reads every row.

**Scale it to the size.** A quick look keeps all five steps but only for the tables and columns the answer rests on: the default sample, one schema, one validation, one description, and the findings folded into the answer with no separate wait. An analysis or a project profiles every table the build will read and stops for the user at step 5.

**Paths over MCP.** The `profile` and `validate` tools take no `dir`: they run FineType from the server's working directory, which is not the Protocol's. Pass `file` and `schema` as absolute paths: the user's CSV by its absolute path, `<dir>/schemas/<table>.json`, `<dir>/build/<table>.parquet`. Only `table_validate` reads `schema` relative to the Protocol folder.

Load `references/reading-the-results.md` when you read a profile or a validation report. Load `references/what-finetype-teaches.md` when the user asks for something FineType's own tools do (a Data Package, a typed table with a reject sidecar, one type's schema).

**FineType's floor.** arc refuses any finetype older than **0.6.54**. Up to and including 0.6.53, any eight-digit number (a financial figure, a surrogate key) was typed as a confident date with a date-parsing transform attached, so a step that followed the transform corrupted the column. If arc refuses with the floor, tell the user to upgrade finetype; do not profile with an older one by hand.

## 1. Get the data in

Every analysis reads its data from the Protocol, so the data goes in through a raw load step whatever its format. What differs is when you can profile it.

- **A local CSV** can be profiled as a file straight away with `profile` (`finetype profile`), before or after you load it.
- **Anything else** (a URL, JSON, a spreadsheet, Parquet, a database export): record a raw load step, run it, and profile the table it makes with `table_profile` (`arc table profile`). `profile` cannot read a URL or a spreadsheet at all, and even where it reads a local file, the table is what the build will read, so profile that. Do not stop at "finetype cannot read this file".

A raw step loads the data as it arrives, with no cleaning: cleaning belongs to the build stage, where it is a step the user can see.

| Step | CLI | MCP |
| --- | --- | --- |
| Fetch a file from a URL | `arc operator record http_fetch --name fetch_orders --with '{"url": "<url>", "out": "data/orders.json"}' --description "<why>" --dir <dir>` | `operator_record` with `operator`, `name`, `with`, `description`, `dir` |
| Load a file as it arrives | `arc sql record "SELECT * FROM read_json('data/orders.json')" --name raw_orders --description "<why>" --dir <dir>` | `sql_record` with `sql`, `name`, `description`, `dir` |
| Run the load | `arc run` (in the Protocol folder) | `protocol_run` with `dir` |

Say before the first run that a fetch step reaches the network. Read the run record before profiling: a load that skipped or made an empty table gives you nothing to profile.

## 2. Profile

| Step | CLI | MCP |
| --- | --- | --- |
| Type each column of a table in the Protocol | `arc table profile <table> --sample-size 500 --dir <dir>` | `table_profile` with `table`, `sample_size`, `dir` |
| Type each column of a local CSV (`file` absolute over MCP) | `finetype profile -f <file.csv> --sample-size <n> -o json` | `profile` with `file`, `sample_size`, `format` |

`table_profile` copies the table's first rows (100 unless you give `sample_size`) and types those. On a large or sorted table, raise the sample size; validation (step 4) is what reads every row. Prefer the table verbs once the data is in the Protocol: they read any table, not only a CSV, and they see the data as the Protocol loaded it.

## 3. Save the schema

A table-level JSON Schema is the data's contract: what each column must look like for the analysis to be sound. Make it with the `json-schema` format, from the same sample size the profile used (when the profile used the default, leave the sample size out here too), and write it into the Protocol at `schemas/<table>.json`, where history versions it. A schema inferred from fewer rows than you profiled makes validation report its own gaps as problems in the data.

| Step | CLI | MCP |
| --- | --- | --- |
| A JSON Schema for a table | `arc table profile <table> --format json-schema --sample-size <n> --dir <dir>` | `table_profile` with `table`, `format`, `sample_size`, `dir` |
| A JSON Schema for a local CSV (`file` absolute over MCP) | `finetype profile -f <file.csv> -o json-schema --sample-size <n>` | `profile` with `file`, `format`, `sample_size` |
| Save it in the Protocol | `arc protocol write-file schemas/<table>.json --dir <dir> < schema.json` | `protocol_write_file` with `path`, `content`, `dir` |

Over MCP, pass the schema the profile returned, unchanged, as `content`. From a shell, pipe it: `arc protocol write-file` refuses empty or non-JSON input, so a failed profile cannot save a blank schema.

## 4. Validate against it

| Step | CLI | MCP |
| --- | --- | --- |
| Validate a table in the Protocol (needs `FINETYPE_DUCKDB_EXT`) | `arc table validate <table> --schema schemas/<table>.json --dir <dir>` | `table_validate` with `table`, `schema`, `dir` |
| Validate a local CSV or Parquet file (`file` and `schema` absolute over MCP) | `finetype validate <file> schemas/<table>.json -o json` | `validate` with `file`, `schema`, `format` |

`table_validate` runs in DuckDB with FineType's extension, found through `FINETYPE_DUCKDB_EXT`. When that is unset, validate a file instead: the source CSV itself, or a Parquet export of the table (record a `parquet_export` operator step, run it, and validate the file it writes). Both validators report rejected rows, and from a shell both `arc table validate` and `finetype validate` exit 1 when there are any. That is a finding to read, not a failure to retry.

## 5. Describe, then report

| Step | CLI | MCP |
| --- | --- | --- |
| Row count, column types and null counts | `arc table describe <table> --dir <dir>` | `table_describe` with `table`, `dir` |

Then tell the user what you found and what it means for their question, using `references/reading-the-results.md`: which columns the question rests on, whether they are typed as expected, how many rows fail the schema and why, and what you propose to do about it in the build. This is the profile stage's checkpoint. For a quick look, fold it into the answer at the end; for an analysis or a project, wait for the user before building.

## Examples

Profile a local CSV:

```sh arc-example needs=finetype
finetype profile -f sales.csv -o json
```

```text expect
"type": "identity.person.email"
```

Save its schema in the Protocol and validate the file against it:

```sh arc-example needs=finetype
finetype profile -f sales.csv -o json-schema | arc protocol write-file schemas/sales.json
finetype validate sales.csv schemas/sales.json -o json
```

```text expect
"grade": "A"
```

Load the CSV into the Protocol, then profile the table it makes and save that table's schema:

```sh arc-example needs=finetype
arc run
arc table profile sales --sample-size 200
arc table profile sales --format json-schema --sample-size 200 | arc protocol write-file schemas/sales.json
arc table describe sales
```

```text expect
"rows_profiled": 200
```

With `FINETYPE_DUCKDB_EXT` set, validate the table in the database. The schema here is a small one written by hand, as you would write one to correct a type FineType chose wrongly; this needs only the extension, not the finetype binary:

```sh arc-example needs=finetype-ext
arc run
arc protocol write-file schemas/sales.json <<'EOF'
{
  "type": "object",
  "properties": {
    "category": {"type": "string", "enum": ["books", "dining", "groceries", "software", "travel"]},
    "email": {"type": "string", "pattern": "^[^@ ]+@[^@ ]+$"}
  },
  "required": ["category", "email"]
}
EOF
arc table validate sales --schema schemas/sales.json
```

```text expect
"valid": true
```

Without `FINETYPE_DUCKDB_EXT`, validate the table through a Parquet export of it:

```sh arc-example needs=finetype
arc operator record parquet_export --name export_sales --with '{"input": "sales", "dest": "build/sales.parquet"}' --description "Export sales to validate it against its schema"
arc run
arc table profile sales --format json-schema | arc protocol write-file schemas/sales.json
finetype validate build/sales.parquet schemas/sales.json -o json
```

```text expect
"valid_rows": 200
```

Data that is not a CSV goes in through a raw step first. Here `orders.json`, a JSON array of orders, is loaded as it arrives and described; none of this needs finetype:

```sh arc-example
arc sql record "SELECT * FROM read_json('orders.json')" --name raw_orders --description "Load the orders as they arrive"
arc run
arc table describe raw_orders
```

```text expect
"row_count": 2
```

Then profile the table it makes:

```sh arc-example needs=finetype
arc sql record "SELECT * FROM read_json('orders.json')" --name raw_orders --description "Load the orders as they arrive"
arc run
arc table profile raw_orders
```

```text expect
"table": "raw_orders"
```

## When to switch

- The findings are reported and, at the analysis and project sizes, the user has agreed what to do about them: load `building-and-checking-a-protocol`.
- The findings change what the question can answer (the column it rests on is mostly empty, or is not what its name says): go back to `scoping-an-analysis` with the user.
- You need the exact form of another verb: the verb map in `using-arcform`, at `../using-arcform/references/verb-map.md`.

## Traps

| Error or symptom | Fix |
| --- | --- |
| You have a profile and are about to build | Save the schema, validate, describe and report first. A profile catches no bad rows |
| `profile` fails on a URL or a spreadsheet | Load it with a raw step, run it, and use `table_profile` |
| `finetype <version> is older than the minimum 0.6.54` | Tell the user to upgrade finetype to 0.6.54 or later: an older one types eight-digit numbers as dates |
| `no database for this Protocol yet` | Run the load step, then profile |
| `<table> has no rows to infer a schema from` | The load made an empty table. Read the run record and trace it upstream before profiling again |
| `finetype DuckDB extension not configured` | Set `FINETYPE_DUCKDB_EXT`, or validate a file: the source CSV, or a `parquet_export` of the table, with `validate` |
| `no schema file at <path>` or `<path> is outside the Protocol folder` from `table_validate` | Save the schema first, at `schemas/<table>.json`, and pass that path relative to the Protocol folder |
| `profile` or `validate` over MCP cannot find a file that is in the Protocol | Those tools run from the server's working directory and take no `dir`. Pass `file` and `schema` as absolute paths (`<dir>/schemas/<table>.json`) |
| `the schema is not valid JSON` when saving | The profile before it failed; read its error, not this one |
| `finetype validate` or `arc table validate` exits 1 | Rows were rejected. Read the report and say what the rejects mean; `finetype validate --lenient` gives the report without the exit code (the MCP `validate` tool is lenient unless `strict` is set) |
| A column's type changes when you profile again | The first sample missed part of the data. Raise `sample_size`, and trust validation over either profile |
| The output starts with `Loading model from ...` | FineType's progress on standard error. The JSON follows it on standard output |
| The user wants a typed DuckDB table with a reject sidecar, or a Data Package | See `references/what-finetype-teaches.md` for the route through arc |
