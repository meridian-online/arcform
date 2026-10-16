---
name: bad
description: A test-only skill that the checks must reject, line by line, writing panels/<name>.yaml.
metadata:
  arc_version: "0.0.0"
---

| Step | CLI | MCP |
| --- | --- | --- |
| UNKNOWN-TOOL | `arc run` | `no_such_tool` |
| UNKNOWN-ARG | `arc table describe sales` | `sql_query` with `sql`, `bogus_arg` |
| EMPTY-CELL | `arc run` | n/a |
| FREE-TEXT-CELL | `arc run` | run the Protocol over MCP |

```json
{"name": "fake_json_tool"}
```

```json
{"name": "table_describe", "arguments": 
```

```sh
arc nosuchverb
```

```sh
arc table nosuchsub
```

```sh
arc run --bogus-flag
```

```sh arc-example
arc table describe nosuchtable
arc run
```

```sh
FOO=bar arc nosuchverb
```

Inline: `arc nosuchverb2`.

```json tool=sql_query
{"bogus_key": 1}
```

```json
{"params": {"name": "params_wrapped_tool", "arguments": {}}, "note": "PARAMS-WRAPPED"}
```

```json
{"name": "table_describe", "arguments": ["ARRAY-ARGS"]}
```

```json tool=sql_query
[1, 2]
```

```sh
arc table_describe sales
```

```sh
arc sql record_step "SELECT 1"
```

Prose naming a tool: `no_prose_tool` with `sql`.

Prose naming an argument: `sql_query` with `sql`, `prose_bogus_arg`.

```sh arc-example needs=nonsense
arc run
```
