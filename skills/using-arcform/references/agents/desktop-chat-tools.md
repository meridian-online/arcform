# A desktop chat app

A desktop chat app, such as Claude Desktop, usually has no shell. It reaches arc only through the MCP tools, so follow the MCP column of each skill and never the `arc ...` commands. The environment check's "With only the server's tools" column is the one that applies.

## Loading the skills

There is no plugin to install. Claude Desktop takes each skill as a ZIP of its folder, uploaded through its skills settings. Run `arc skills install --target <folder>` to write this arc's skills into a folder of your choosing, then zip each skill folder there (start with `using-arcform`) and upload it: from that folder, `zip -r using-arcform.zip using-arcform -x '*/.arc-skill'`. Leave out the `.arc-skill` marker, as that command does: it tells `arc skills install` the folder is its own to replace, which means nothing to an uploaded copy. `arc skills list` prints the skills this arc carries and the version each was written for. Upload the skills again after upgrading arc.

## Connecting `arc mcp`

Add the server to the app's MCP configuration. For Claude Desktop, open Settings, then Developer, then Edit Config, which opens `claude_desktop_config.json` (on macOS, in `~/Library/Application Support/Claude/`):

```json
{
  "mcpServers": {
    "arc": {
      "command": "/Users/you/.local/bin/arc",
      "args": ["mcp"],
      "env": {
        "PATH": "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"
      }
    }
  }
}
```

Quit and reopen the app to start the server. A desktop app does not inherit your shell's `PATH`, so give `arc` by its absolute path (`which arc` prints it) and a `PATH` that holds `duckdb`, `finetype` and `uv`.

## Tool names

| The skills write | In a desktop chat app |
| --- | --- |
| `sql_query`, and every other arc tool | The same tool, listed under the `arc` server |
| An `arc ...` command | Not available: use the matching tool from the skill's verb table, or ask the user to run the command |
| "Load `references/...`" | Read the reference from the uploaded skill |
