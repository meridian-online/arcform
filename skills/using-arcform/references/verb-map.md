# Verb map: the CLI and MCP forms of every verb

Every verb the analyst skills use, as an `arc` command and as the matching `arc mcp` tool. The two forms do the same thing: pick the column for the way you reach arc. An agent with a shell can use either; an agent with only `arc mcp` (a desktop chat app, say) uses the MCP column and never needs to write a file itself.

In the CLI column, `--dir` defaults to the current directory; for `arc edit-protocol` it goes before the edit (`arc edit-protocol --dir <dir> replace <path> <value>`). In the MCP column, pass `dir` as the Protocol folder's absolute path: the server's working directory is not yours.

## Discover

| What it does | CLI | MCP |
| --- | --- | --- |
| Describe a table already in a Protocol's database: row count, column types, null counts, five sample rows | `arc table describe <table> --dir <dir>` | `table_describe` with `table`, `dir` |
| Run read-only SELECTs and return the rows | `arc sql query "<select>" --limit 100 --dir <dir>` | `sql_query` with `sql`, `limit`, `dir` |
| Say what arc reads a SQL statement as: an operation it holds, or none | `arc sql recognise "<statement>"` | `sql_recognise` with `sql` |
| List the SQL operations arc holds | `arc operation list --json` | `operation_describe` (no arguments) |
| Say what one operation takes | `arc operation describe <long-name>` | `operation_describe` with `operation` |
| List the operators | None: the CLI has no operator listing | `operator_describe` with no arguments |
| Give one operator's `with:` JSON Schema | `arc operation describe <operator>` | `operator_describe` with `operator` |

## Profile and type

| What it does | CLI | MCP |
| --- | --- | --- |
| Type each column of a table in the Protocol with FineType | `arc table profile <table> --sample-size 100 --dir <dir>` | `table_profile` with `table`, `sample_size`, `dir` |
| A JSON Schema the table's rows pass | `arc table profile <table> --format json-schema --dir <dir>` | `table_profile` with `table`, `format`, `dir` |
| Save that schema in the Protocol | `arc protocol write-file schemas/<table>.json --dir <dir> < schema.json` | `protocol_write_file` with `path`, `content`, `dir` |
| Validate a table against the saved schema (needs `FINETYPE_DUCKDB_EXT`) | `arc table validate <table> --schema schemas/<table>.json --dir <dir>` | `table_validate` with `table`, `schema`, `dir` |
| Profile a CSV file before it is loaded | `finetype profile -f <file.csv> -o json` | `profile` with `file`, `format`; `file` absolute |
| A JSON Schema for a CSV file | `finetype profile -f <file.csv> -o json-schema` | `profile` with `file`, `format`; `file` absolute |
| Validate a CSV or Parquet file against a schema | `finetype validate <file> <schema.json> -o json` | `validate` with `file`, `schema`, `format`; `file` and `schema` absolute (`<dir>/schemas/<table>.json`) |
| Type a handful of loose values | `finetype infer -f <values.txt> -o json` | `infer` with `values` |
| Browse FineType's types, or export one type's schema | `finetype taxonomy <type-key> -o json` | `taxonomy` with `type_key`, `format` |

The `finetype` rows run a separate binary; arc's MCP server federates them as tools. Those tools take no `dir` and run from the server's working directory, so give them absolute paths. Prefer the `arc table` verbs once the data is in a Protocol: they read any table, not only a CSV file.

## Record

Recording writes the step into `arcform.yaml` (and a model under `models/`) and runs nothing.

| What it does | CLI | MCP |
| --- | --- | --- |
| Make a new Protocol | `arc create-protocol <dir> --name <name>` | `protocol_create` with `dir`, `name` |
| Record a SQL statement as a step | `arc sql record "<statement>" --name <step> --description "<why>" --dir <dir>` | `sql_record` with `sql`, `name`, `description`, `dir` |
| Record an operation arc holds (such as `filter-rows`) as a step | `arc operation record <long-name> --on <table> --name <step> --arg <key>=<value> --description "<why>" --dir <dir>` | `operation_record` with `operation`, `on`, `name`, `arguments`, `description`, `dir` |
| Record an operator step (fetch, resolution, embedding, export), its `with:` checked | `arc operator record <operator> --name <step> --with '<json>' --description "<why>" --dir <dir>` | `operator_record` with `operator`, `name`, `with`, `description`, `dir` |
| Replace one value in the spec | `arc edit-protocol --dir <dir> replace <path> <value>` | `protocol_edit` with `edit`, `path`, `value`, `dir` |
| Rewrite one occurrence inside a value | `arc edit-protocol --dir <dir> rewrite <path> <from> <to>` | `protocol_edit` with `edit`, `path`, `from`, `to`, `dir` |
| Add a key to a mapping | `arc edit-protocol --dir <dir> add <path> <key> <value>` | `protocol_edit` with `edit`, `path`, `key`, `value`, `dir` |
| Append an item to a sequence | `arc edit-protocol --dir <dir> append <path> <item>` | `protocol_edit` with `edit`, `path`, `item`, `dir` |
| Delete an element | `arc edit-protocol --dir <dir> delete <path>` | `protocol_edit` with `edit`, `path`, `dir` |
| Move an item within a sequence | `arc edit-protocol --dir <dir> reorder <path> <from> <to>` | `protocol_edit` with `edit`, `path`, `from`, `to`, `dir` |
| Write the requirements README | `arc protocol write-file README.md --dir <dir> < README.md` | `protocol_write_file` with `path`, `content`, `dir` |

## Run and check

| What it does | CLI | MCP |
| --- | --- | --- |
| Run the Protocol and get its run record | `arc run --param <key>=<value>` (in the Protocol folder) | `protocol_run` with `dir`, `params` |
| Re-run every step, fresh or not (offer it; do not default to it) | `arc run --force` | `protocol_run` with `dir`, `force` |
| List the recorded versions of the spec, or of one file | `arc history list --json --file <file> --dir <dir>` | `history_list` with `file`, `dir` |
| Print the bytes one version recorded | `arc history show <id> --file <file> --dir <dir>` | None: no tool prints a version |
| Roll the spec, or one file, back to a version | `arc history restore <id> --file <file> --dir <dir>` | `history_restore` with `id`, `file`, `dir`. Over MCP you cannot read a version first, so choose the id by its at and way fields in the history list; a wrong choice is undone by restoring the checkpoint the restore itself made |

## Present

| What it does | CLI | MCP |
| --- | --- | --- |
| Write a chart as a panel Brightfield opens | `arc protocol write-file panels/<name>.yaml --dir <dir> < panel.yaml` | `protocol_write_file` with `path`, `content`, `dir` |
| Report each panel that names a table or column the Protocol does not make | `arc panel check --dir <dir>` | `panel_check` with `dir` |
| Write `report.md` from your prose and the newest run | `arc report write --dir <dir> < prose.md` | `report_write` with `prose`, `dir` |
| Write the report of an earlier run | `arc report write --run <run-id> --dir <dir> < prose.md` | `report_write` with `prose`, `run`, `dir` |
