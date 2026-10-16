# Claude Code

The skills name each tool by its bare name, such as `sql_query`, and each command as `arc ...`. In Claude Code the arc tools carry a prefix, and the shell runs the commands.

## Loading the skills

Use one of two routes, not both: with both, Claude Code holds two copies of every skill.

- **The plugin.** Add this repository as a marketplace and install the `arcform` plugin from it. The plugin brings the five skills and starts the arc MCP server for you; its skills are listed as `arcform:using-arcform` and so on. The marketplace names the plugin at the tag of the newest arc release, so it brings the skills of a released arc; it works from the first release that carries the plugin manifests.

  ```sh
  claude plugin marketplace add meridian-online/arcform
  claude plugin install arcform@arcform
  ```

  After upgrading arc, refresh both: `claude plugin marketplace update arcform`, then `claude plugin update arcform@arcform`.

- **The skills folder.** `arc skills install --agent claude-code` writes the skills into `~/.claude/skills/`, at the version of the arc that wrote them. Connect the server yourself (below).

## Connecting `arc mcp`

The plugin does this. Without it, add the server once:

```sh
claude mcp add arc -- arc mcp
```

Either way the server runs `arc` from `PATH`. When Claude Code is started from somewhere that does not see your shell's `PATH`, give the server an absolute path to `arc` and a `PATH` that holds `duckdb`, `finetype` and `uv`, as the arcform README's "Using arc from an agent" section shows.

## Tool names

| The skills write | Claude Code calls it |
| --- | --- |
| `sql_query`, and every other arc tool | `mcp__arc__sql_query` when the server was added with `claude mcp add arc`; `mcp__plugin_arcform_arc__sql_query` when it came with the plugin |
| An `arc ...` command | The same command, through the Bash tool |
| "Load `references/...`" | Read that file from the skill's folder |

Claude Code's default MCP tool timeout is long enough for a `protocol_run`; nothing needs raising.
