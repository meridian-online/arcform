# Cursor

The skills name each tool by its bare name, such as `sql_query`, and each command as `arc ...`. Cursor's agent lists the arc tools under the server's name, `arc`, and runs the commands in its terminal.

## Loading the skills

Use one of two routes, not both: with both, Cursor holds two copies of every skill.

- **The plugin.** This repository is a Cursor plugin (`.cursor-plugin/plugin.json`): it brings the five skills and the arc MCP server. This route is untested. Install it from this repository at the tag `vX.Y.Z` matching `arc --version`, as [Cursor's plugin docs](https://cursor.com/docs/reference/plugins) describe, so the skills match your arc; it works from the first release that carries the plugin manifest.
- **The skills folder.** `arc skills install --agent cursor` writes the skills into `~/.agents/skills/`, the shared folder Cursor reads beside `~/.cursor/skills/`, so Codex and Gemini CLI on the same machine share the one copy. Connect the server yourself (below).

## Connecting `arc mcp`

The plugin does this. Without it, add the server to `~/.cursor/mcp.json` (or a project's `.cursor/mcp.json`):

```json
{
  "mcpServers": {
    "arc": {
      "command": "arc",
      "args": ["mcp"]
    }
  }
}
```

Give `command` as an absolute path (`which arc` prints it), and give the server a `PATH`, under its `env` key, that holds `duckdb`, `finetype` and `uv`, when Cursor does not see your shell's `PATH`.

## Tool names

| The skills write | In Cursor |
| --- | --- |
| `sql_query`, and every other arc tool | The same tool, listed under the `arc` server |
| An `arc ...` command | The same command, through the agent's terminal |
| "Load `references/...`" | Read that file from the skill's folder |
