# Gemini CLI

The skills name each tool by its bare name, such as `sql_query`, and each command as `arc ...`. Gemini CLI gives every MCP tool a prefixed name, and runs the commands through its shell tool.

## Loading the skills

Use one of two routes, not both: with both, Gemini CLI holds two copies of every skill.

- **The extension.** This repository is a Gemini CLI extension (`gemini-extension.json`): it brings the six skills, which Gemini CLI finds in the extension's `skills/` folder, and the arc MCP server.

  ```sh
  gemini extensions install https://github.com/meridian-online/arcform --ref vX.Y.Z
  ```

  Put the version `arc --version` prints in place of `X.Y.Z`, so the skills match your arc; it works from the first release that carries the extension manifest. After upgrading arc, uninstall the extension and install it again at the new tag.

- **The skills folder.** `arc skills install --agent gemini` writes the skills into `~/.agents/skills/`, the shared folder Gemini CLI reads ahead of `~/.gemini/skills/`, so Codex and Cursor on the same machine share the one copy. Connect the server yourself (below).

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
