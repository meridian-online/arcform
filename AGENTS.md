# Agents

## Analysing data with arc

If you are an agent helping someone analyse tabular data, load the skills in [`skills/`](skills/), starting with [`using-arcform`](skills/using-arcform/SKILL.md). They show each step both as an `arc` command and as the matching `arc mcp` tool.

- **Get the skills.** `arc skills install` writes them into each agent's skills folder it finds, at the version of that `arc`. This repository is also a plugin for Claude Code, Codex, Cursor and Gemini CLI, which brings the skills and the server together; use one route, not both.
- **Connect the server.** `arc mcp` is a stdio MCP server, in release builds and in builds with `--features mcp`. The notes for your agent in [`skills/using-arcform/references/agents/`](skills/using-arcform/references/agents/) show how to connect it and what your agent calls its tools.

## Working on this repository

[`CLAUDE.md`](CLAUDE.md) holds the project's instructions, and [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) its design. The plugin manifests (`.claude-plugin/`, `.codex-plugin/`, `.cursor-plugin/`, `gemini-extension.json`, `plugin/codex-mcp.json`) are generated: edit [`plugin/plugin.meta.json`](plugin/plugin.meta.json) and run `python3 scripts/generate-plugins.py`, never the manifests themselves. The skills are checked against the binary by `cargo test --features mcp --test skills`.
