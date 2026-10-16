# Gemini CLI

The skills name each tool by its bare name, such as `sql_query`, and each command as `arc ...`. Gemini CLI gives every MCP tool a prefixed name, and runs the commands through its shell tool.

## Loading the skills

Use one of two routes, not both: with both, Gemini CLI holds two copies of every skill.

- **The extension.** This repository is a Gemini CLI extension (`gemini-extension.json`): it brings the five skills, which Gemini CLI finds in the extension's `skills/` folder, and the arc MCP server.

  ```sh
  gemini extensions install https://github.com/meridian-online/arcform
  ```

- **The skills folder.** `arc skills install --agent gemini` writes the skills into `~/.gemini/skills/`. Connect the server yourself (below).

## Connecting `arc mcp`

The extension does this. Without it, add the server once:

```sh
gemini mcp add arc arc mcp
```

That writes the server into the project's `.gemini/settings.json`; add `--scope user` for every project. Give an absolute path to `arc` (`which arc` prints it) when Gemini CLI does not see your shell's `PATH`. Gemini CLI's default MCP timeout is ten minutes, which covers most runs.

## Tool names

| The skills write | Gemini CLI calls it |
| --- | --- |
| `sql_query`, and every other arc tool | `mcp_arc_sql_query`: `mcp_`, the server name, then the tool |
| An `arc ...` command | The same command, through the shell tool |
| Load a skill | The skill-activation tool, or the skill's own description doing its work |
| "Load `references/...`" | Read that file from the skill's folder |

Name the server `arc`, without an underscore: Gemini CLI splits a tool's full name on the first underscore after `mcp_`.
