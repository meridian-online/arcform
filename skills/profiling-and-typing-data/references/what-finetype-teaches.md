# What FineType teaches, routed through arc

FineType ships two agent skills of its own: a reference to its command line, and a guide to its profile, schema and validate pipeline. An agent working through arc should not need them. This page takes each thing they teach and gives its route through arc, or says it is not reachable and what to do instead.

Three rules decide the routes. Every analysis step lives in the Protocol, so FineType's own output files (a separate DuckDB database, a schema next to the data) become steps or files in the Protocol. And over MCP, FineType's own tools (`profile`, `validate`, `infer`, `taxonomy`) take no `dir` and run from the server's working directory, so every `file` and `schema` you give them is an absolute path: the user's CSV by its absolute path, `<dir>/schemas/<table>.json`, `<dir>/build/<table>.parquet`. Only `table_validate` reads `schema` relative to the Protocol folder. And finetype must be **0.6.54** or later: arc refuses an older one, because up to and including 0.6.53 any eight-digit number was typed as a confident date with a date-parsing transform attached.

## The pipeline guide

FineType's pipeline guide teaches profile, then schema, then validate, then a typed table, and not stopping after profile.

| What it teaches | CLI | MCP |
| --- | --- | --- |
| Profile the data and read each column's type, storage type and confidence | `arc table profile <table> --dir <dir>` | `table_profile` with `table`, `dir` |
| The same for a local CSV, before it is loaded (`file` absolute over MCP) | `finetype profile -f <file.csv> -o json` | `profile` with `file`, `format` |
| Raise the sample for a large file | `arc table profile <table> --sample-size 500 --dir <dir>` | `table_profile` with `table`, `sample_size`, `dir` |
| Name the delimiter of an awkward file | `finetype profile -f <file> --delimiter ';' -o json` | `profile` with `file`, `delimiter`, `format` |
| Capture the profile as a JSON Schema, from the same sample size the profile used | `arc table profile <table> --format json-schema --sample-size <n> --dir <dir>` | `table_profile` with `table`, `format`, `sample_size`, `dir` |
| Save the schema beside the data as the contract | `arc protocol write-file schemas/<table>.json --dir <dir> < schema.json` | `protocol_write_file` with `path`, `content`, `dir` |
| Edit the schema by hand when FineType chose the wrong type | Write the corrected schema with `arc protocol write-file schemas/<table>.json --dir <dir>`; history keeps the one it replaced | `protocol_write_file` with `path`, `content`, `dir` |
| Validate every row of a table against the schema as a quality gate, check-only, with per-column rejects | `arc table validate <table> --schema schemas/<table>.json --dir <dir>` (needs `FINETYPE_DUCKDB_EXT`) | `table_validate` with `table`, `schema`, `dir` |
| Validate a CSV or Parquet file, with its grade and reject types (`SEMANTIC_TYPE`, `TRANSFORM_FAILED`); `file` and `schema` absolute over MCP | `finetype validate <file> schemas/<table>.json -o json` | `validate` with `file`, `schema`, `format` |
| Report the rejects without a failing exit code | `finetype validate <file> <schema> --lenient -o json` | `validate` reports without failing unless `strict` is set |
| Make the schema a gate a re-run must pass | Export the table to Parquet (`parquet_export`), then a `finetype_validate` step that fails when the file breaks the schema: `arc operator record finetype_validate --name check_<table> --with '{"parquet": "build/<table>.parquet", "schema": "schemas/<table>.json"}' --description "<why>" --dir <dir>` (needs `FINETYPE_DUCKDB_EXT`, or `extension` in the `with:` block) | `operator_record` with `operator`, `name`, `with`, `description`, `dir`, naming the finetype_validate operator |
| Look at one outlier value to see why confidence is low | `finetype infer -i "<value>" --confidence` | `infer` with `value` |
| Type a handful of loose values as one column | `finetype infer -f <values.txt> --mode column --header <name> -o json` | `infer` with `values`, `header` |
| Browse the type system | `finetype taxonomy -d <domain> -o json` | `taxonomy` with `domain`, `category`, `full`, `format` |

How to read the results (confidence as a quality signal, the grade, rejects, nulls, mixed types) is in `reading-the-results.md`, beside this file.

## The command-line reference

FineType's command-line reference documents every verb and flag. Most of it is reached through the rows above; the rest:

| What it teaches | CLI | MCP |
| --- | --- | --- |
| One type's JSON Schema (type-mode, rather than a table's) | `finetype taxonomy identity.person.email -o json-schema` | `taxonomy` with `type_key`, `format` |
| Observed-data constraints (lengths, ranges, a closed enum, null rate) in the schema, for a local CSV (`file` absolute over MCP) | `finetype profile -f <file.csv> -o json-schema --stats` | `profile` with `file`, `format`, `stats` |
| A Frictionless Data Package describing the data | A `datapackage_describe` step over a built Parquet: `arc operator record datapackage_describe --name describe_<table> --with '{"parquet": "build/<table>.parquet", "overrides": "schemas/<table>-overrides.json", "out": "datapackage.json"}' --description "<why>" --dir <dir>` | `operator_record` with `operator`, `name`, `with`, `description`, `dir`, naming the datapackage_describe operator |
| The `domain.category.type` label format | Read it from the type field of any profile | Read it from the type field of any profile |

The Data Package step needs a Parquet file (record a `parquet_export` step before it) and an `overrides` file of curated descriptions, which can be `{}` to start. Keep it at `schemas/<table>-overrides.json`, and any declared column types (`nominations`, of the shape `{"version": 1, "resources": {...}}`) at `schemas/<table>-nominations.json` (declaring `nominations` needs finetype 0.6.60 or later, the first release whose `profile` takes them; the step refuses an older one before profiling): `protocol_write_file` (`arc protocol write-file`) writes any valid JSON there, so an agent with only the MCP tools can write both, and history versions them. The step writes the descriptor each run, so it stays in step with the data.

## Not reachable through arc, and what to do instead

- **A typed DuckDB table with a reject sidecar** (`finetype validate <file> <schema> --db <out.db> --table <name>`, with its `finetype_reject_errors` table). This writes a separate database outside the Protocol, which no later step can read and no re-run rebuilds. Instead, type the data in the build: a SQL step that casts each column with the `transform` its profile gave, so the typed table is a step in the Protocol. Find the rejected rows before building with `table_validate`, or with `sql_query` over the values the cast refuses, and decide with the user whether the cleaning step drops, nulls or fixes them. Run FineType's `--db` form only when the user asks for that database file itself, and say it is not part of the Protocol.
- **Plain, CSV, Markdown and Arrow output.** arc returns JSON. Read the JSON; there is no need for another format inside a Protocol.
- **`--no-header-hint`, `--enum-threshold`, `--nominations`, verbose tracing.** Tuning switches on `finetype profile` that arc does not pass. When the user needs one, run `finetype profile` on a local file to look; to make it part of the Protocol, `datapackage_describe` takes `nominations` (declared column types) as a file at `schemas/<table>-nominations.json`. `--model-type` is gone from current FineType releases; ignore it.
- **Batch and diagnostic inference** (`finetype infer --batch`, `--explain`). Debugging tools for FineType's own models. Not part of an analysis.
- **The pipeline guide's grade table** (A or B: materialise; C or D: investigate the rejects; F: do not materialise). arc does not materialise, so read the grade as the size of the problem instead: `reading-the-results.md` says what each size of reject count means for the question and what to tell the user. An F still means check the load, the delimiter and the sample before calling the data bad.
- **The pipeline guide's quick path, which skips the saved schema.** Not offered. The saved schema is what validation and any later re-run check against, and writing it is one call; a quick look keeps it.
- **Exit code 2** (`finetype validate` could not run: a malformed schema, an unreadable file). Not a finding about the data. Read the error: most often over MCP it is a relative `file` or `schema` path the server could not find.
- **`finetype load` and `finetype schema <file>`.** Both were removed from FineType. Use the schema and validate routes above.
- **`finetype generate`, `finetype check`, `finetype mcp`.** For FineType's maintainers. arc's server already federates FineType's tools; do not run a second server.
