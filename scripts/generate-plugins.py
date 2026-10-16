#!/usr/bin/env python3
"""Write each agent's plugin manifest from the one source, plugin/plugin.meta.json.

No agent's format is the source. plugin/plugin.meta.json holds what the plugin is (its name,
descriptions, homepage, licence, where its skills are and the MCP server it connects), the
version comes from Cargo.toml's [package] version, and this script writes:

  .claude-plugin/plugin.json       Claude Code plugin (skills/ and the arc MCP server)
  .claude-plugin/marketplace.json  marketplace listing the plugin at the release tag v<version>;
                                   Claude Code and Codex both read it
  .codex-plugin/plugin.json        Codex plugin
  plugin/codex-mcp.json            the MCP server file the Codex plugin names
  .cursor-plugin/plugin.json       Cursor plugin
  gemini-extension.json            Gemini CLI extension

Usage, from anywhere:

  python3 scripts/generate-plugins.py             # write into the repository root
  python3 scripts/generate-plugins.py --out DIR   # write into DIR instead

tests/plugin_manifests.rs runs it with --out and compares the result with the committed files,
so edit plugin/plugin.meta.json and re-run this; never edit the generated files by hand.
Standard library only (Python 3.9 or later).
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path
from typing import NoReturn

ROOT = Path(__file__).resolve().parent.parent
META = ROOT / "plugin" / "plugin.meta.json"
CARGO = ROOT / "Cargo.toml"

# The Codex plugin names its MCP server file by path. It is not the root `.mcp.json`, which
# Claude Code would also read: as the Claude plugin's default MCP file, and as this
# repository's own project-scoped MCP configuration for anyone working on it.
CODEX_MCP = "plugin/codex-mcp.json"

KEYS = {
    "name",
    "displayName",
    "description",
    "longDescription",
    "shortDescription",
    "author",
    "homepage",
    "repository",
    "license",
    "keywords",
    "category",
    "defaultPrompt",
    "skills",
    "mcpServer",
    "marketplace",
}
MCP_KEYS = {"name", "command", "args", "toolTimeoutSec"}


def fail(message: str) -> NoReturn:
    print(f"generate-plugins: {message}", file=sys.stderr)
    sys.exit(1)


def cargo_package(keys: tuple[str, ...]) -> dict:
    """The given `key = "value"` lines of Cargo.toml's [package] table.

    Not a TOML parser (tomllib needs Python 3.11): it reads only plain double-quoted strings
    on one line, which is how these keys are written here.
    """
    found: dict = {}
    in_package = False
    for line in CARGO.read_text(encoding="utf-8").splitlines():
        text = line.strip()
        if text.startswith("["):
            in_package = text == "[package]"
            continue
        match = re.fullmatch(r'([A-Za-z0-9_-]+)\s*=\s*"([^"\\]*)"\s*(#.*)?', text)
        if in_package and match and match.group(1) in keys:
            found[match.group(1)] = match.group(2)
    absent = [key for key in keys if key not in found]
    if absent:
        fail(f'Cargo.toml: [package] has no plain `key = "value"` line for {absent}')
    return found


def load() -> tuple[dict, str]:
    meta = json.loads(META.read_text(encoding="utf-8"))
    unknown = set(meta) - KEYS
    missing = KEYS - set(meta)
    if unknown or missing:
        fail(f"{META.relative_to(ROOT)}: unknown keys {sorted(unknown)}, missing keys {sorted(missing)}")
    if set(meta["mcpServer"]) != MCP_KEYS:
        fail(f"{META.relative_to(ROOT)}: mcpServer must have exactly {sorted(MCP_KEYS)}")

    package = cargo_package(("version", "license", "homepage", "repository"))
    # The crate and the plugin are one project: where both say it, they must say the same.
    for key in ("license", "homepage", "repository"):
        if meta[key] != package[key]:
            fail(
                f"{META.relative_to(ROOT)} says {key} {meta[key]!r} but Cargo.toml says "
                f"{package[key]!r}; make them agree"
            )
    return meta, package["version"]


def server(meta: dict) -> dict:
    mcp = meta["mcpServer"]
    return {"command": mcp["command"], "args": mcp["args"]}


def claude_plugin(meta: dict, version: str) -> dict:
    # skills/ at the plugin root is Claude Code's default, so no `skills` key.
    return {
        "name": meta["name"],
        "displayName": meta["displayName"],
        "description": meta["description"],
        "version": version,
        "author": meta["author"],
        "homepage": meta["homepage"],
        "repository": meta["repository"],
        "license": meta["license"],
        "keywords": meta["keywords"],
        "mcpServers": {meta["mcpServer"]["name"]: server(meta)},
    }


def release_source(meta: dict, version: str) -> dict:
    # The plugin is fetched from the release tag of this version, not the default branch,
    # so a plugin install carries the skills of a released arc. Both Claude Code and
    # Codex read a `url` source with a `ref` (Codex has no `github` source type).
    return {"source": "url", "url": f"{meta['repository']}.git", "ref": f"v{version}"}


def claude_marketplace(meta: dict, version: str) -> dict:
    return {
        "name": meta["marketplace"]["name"],
        "description": meta["marketplace"]["description"],
        "owner": meta["author"],
        "plugins": [
            {
                "name": meta["name"],
                "description": meta["description"],
                "version": version,
                "source": release_source(meta, version),
                "author": meta["author"],
            }
        ],
    }


def codex_plugin(meta: dict, version: str) -> dict:
    return {
        "name": meta["name"],
        "version": version,
        "description": meta["description"],
        "author": meta["author"],
        "homepage": meta["homepage"],
        "repository": meta["repository"],
        "license": meta["license"],
        "keywords": meta["keywords"],
        "skills": meta["skills"],
        "mcpServers": f"./{CODEX_MCP}",
        "interface": {
            "displayName": meta["displayName"],
            "shortDescription": meta["shortDescription"],
            "longDescription": meta["longDescription"],
            "developerName": meta["author"]["name"],
            "category": meta["category"],
            "capabilities": ["Interactive", "Read", "Write"],
            "defaultPrompt": meta["defaultPrompt"],
            "websiteURL": meta["homepage"],
        },
    }


def codex_mcp(meta: dict) -> dict:
    # A protocol_run can outlast Codex's default 60-second tool timeout.
    entry = server(meta) | {"tool_timeout_sec": meta["mcpServer"]["toolTimeoutSec"]}
    return {"mcpServers": {meta["mcpServer"]["name"]: entry}}


def cursor_plugin(meta: dict, version: str) -> dict:
    return {
        "name": meta["name"],
        "displayName": meta["displayName"],
        "description": meta["description"],
        "version": version,
        "author": {"name": meta["author"]["name"]},
        "homepage": meta["homepage"],
        "repository": meta["repository"],
        "license": meta["license"],
        "keywords": meta["keywords"],
        "skills": meta["skills"],
        "mcpServers": {meta["mcpServer"]["name"]: server(meta)},
    }


def gemini_extension(meta: dict, version: str) -> dict:
    # Gemini CLI finds an extension's skills in its skills/ folder by itself, and its
    # default MCP timeout (ten minutes) already covers a long run.
    return {
        "name": meta["name"],
        "version": version,
        "description": meta["description"],
        "mcpServers": {meta["mcpServer"]["name"]: server(meta)},
    }


def render(doc: dict) -> str:
    return json.dumps(doc, indent=2, ensure_ascii=False) + "\n"


def main() -> None:
    parser = argparse.ArgumentParser(description=(__doc__ or "").strip().partition("\n")[0])
    parser.add_argument("--out", type=Path, default=ROOT, help="folder to write into (default: the repository root)")
    out = parser.parse_args().out

    meta, version = load()
    files = {
        ".claude-plugin/plugin.json": claude_plugin(meta, version),
        ".claude-plugin/marketplace.json": claude_marketplace(meta, version),
        ".codex-plugin/plugin.json": codex_plugin(meta, version),
        CODEX_MCP: codex_mcp(meta),
        ".cursor-plugin/plugin.json": cursor_plugin(meta, version),
        "gemini-extension.json": gemini_extension(meta, version),
    }
    for rel, doc in files.items():
        path = out / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        # Path.write_text has no `newline` argument before Python 3.10.
        with path.open("w", encoding="utf-8", newline="\n") as f:
            f.write(render(doc))
        print(rel)


if __name__ == "__main__":
    main()
