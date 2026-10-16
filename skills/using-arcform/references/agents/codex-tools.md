# Codex

The skills name each tool by its bare name, such as `sql_query`, and each command as `arc ...`. Codex lists the arc tools under the server's name, `arc`, and runs the commands in its shell.

## Loading the skills

Use one of two routes, not both: with both, Codex holds two copies of every skill.

- **The plugin.** Add this repository as a marketplace and install the `arcform` plugin from it. The plugin brings the five skills and the arc MCP server, with its tool timeout already raised.

  ```sh
  codex plugin marketplace add meridian-online/arcform
  codex plugin add arcform@arcform
  ```

- **The skills folder.** `arc skills install --agent codex` writes the skills into `~/.agents/skills/`, the folder Codex reads. Connect the server yourself (below).

## Connecting `arc mcp`

The plugin does this. Without it, add the server to `~/.codex/config.toml`:

```toml
[mcp_servers.arc]
command = "arc"
args = ["mcp"]
tool_timeout_sec = 600
```

Codex stops a tool call after 60 seconds by default, and a `protocol_run` can take longer, so keep `tool_timeout_sec` raised. Give `command` as an absolute path (`which arc` prints it) when Codex does not see your shell's `PATH`.

## Tool names

| The skills write | In Codex |
| --- | --- |
| `sql_query`, and every other arc tool | The same tool, listed under the `arc` server |
| An `arc ...` command | The same command, through Codex's shell tool |
| "Load `references/...`" | Read that file from the skill's folder |
