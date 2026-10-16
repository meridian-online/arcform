---
name: good
description: A test-only skill that every check must accept.
metadata:
  arc_version: "0.0.0"
---

| Step | CLI | MCP |
| --- | --- | --- |
| Look at a table | `arc table describe sales` | `table_describe` with `table` |
| Print one version of the spec | `arc history show <id>` | None: no tool prints a version |

An MCP call as a JSON envelope:

```json
{"name": "table_describe", "arguments": {"table": "sales"}}
```

An MCP call whose body is the arguments:

```json tool=sql_query
{"sql": "SELECT count(*) FROM sales"}
```

A CLI form in a block and inline, `arc run --param x=1`:

```sh
FOO=bar arc table describe sales --dir . && arc sql query "SELECT 1" --limit 1
```

Prose names such as `arcform.yaml` and `row_count` are not tools, but prose can name one: `sql_query` with `sql`, `limit` (the arguments end at the clause). A placeholder verb, `arc <verb> --help`, is skipped.

```sh arc-example
arc run
arc table describe sales
```

```text expect
order_date
```
