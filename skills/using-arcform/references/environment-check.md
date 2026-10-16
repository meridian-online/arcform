# Environment check

Run this once at the start of a session, before the first Protocol. With only the server's tools, the arc version check is the exception: it can usually be made only after a first run (section 1). It takes three commands, and each failure has one thing to tell the user. Do not work around a failure by computing the answer in a script instead: the user then gets an answer with no Protocol behind it.

| Check | With a shell | With only the server's tools |
| --- | --- | --- |
| arc's version matches the skills | `arc --version` | The server version, if your client shows it; otherwise `run.engine.arc` from the first `protocol_run` |
| The MCP server is in this build | `arc mcp --help` exits 0 | You are talking to it, so it is |
| finetype is at or above its floor | `finetype --version` | The first `table_profile` or `profile` call refuses, naming the floor, when it is not |

## 1. arc's version

```sh
arc --version
```

It prints `arc <version>`. Compare that with `metadata.arc_version` in the frontmatter of the skill you loaded. With only the server's tools, the version usually cannot be seen before a run: when the client does not show the server version, compare `run.engine.arc` from the first `protocol_run` with `metadata.arc_version`. Make that first run one whose steps neither fetch nor overwrite anything the user cares about, such as a Protocol whose only step loads the user's file. If no run record comes back, tell the user "arc version unknown" rather than passing the check silently. Skills are written against one arc version: an older arc may lack a verb a skill names, and a newer one may have changed what a verb prints.

When they differ, tell the user: "Your arc is <version> and these skills were written for <arc_version>. Run `arc skills install` to install the skills this arc carries, so the two match." The command writes into each agent's skills folder it finds and prints each folder it writes; a skill folder the user wrote under the same name is refused, so pass on its reason rather than deleting anything. When it finds no agent, it names where it would install: `arc skills install --target <dir>` installs into a folder the user names. Then carry on carefully: if a verb a skill names is refused as unknown, stop and say so rather than guessing at another.

When `arc` is not found at all, tell the user to install it, on macOS or Linux with `curl -fsSL https://install.meridian.online/arc | bash`, and stop: nothing in these skills works without it.

## 2. The MCP server

```sh
arc mcp --help
```

Release builds of arc include the MCP server. A development build made without the `mcp` feature does not, and `arc mcp` is then refused as an unrecognised subcommand. That matters only when the user wants to connect a chat app or another agent to arc. Tell them: "This arc was built without its MCP server; install a release build, or rebuild with `--features mcp`." With a shell you can still follow every skill through the CLI column of the verb map.

When an agent started from a desktop app reports that `duckdb`, `finetype` or `uv` cannot be found although they work in the user's terminal, the app did not inherit the shell's `PATH`. Tell the user to give the MCP server an absolute path to `arc` and a `PATH` in its configuration, as the arcform README's "Using arc from an agent" section shows.

## 3. finetype's floor

```sh
finetype --version
```

finetype types columns for the profile stage. arc refuses any finetype older than **0.6.54**. The floor tracks label correctness, not just which commands exist: up to and including 0.6.53, any eight-digit number (a financial figure, a surrogate key) was typed as a confident date with a date-parsing transform attached, so a step that followed the transform corrupted the column. arc reads `finetype` from `PATH`, or from `FINETYPE_BIN` when that is set.

- **Not installed.** Tell the user: "Profiling needs finetype 0.6.54 or later; install it and I'll type the columns." Until then, describe tables with `table_describe` and say plainly that the columns are untyped.
- **Below the floor.** arc refuses with `finetype <found> is older than the minimum 0.6.54`. Tell the user to upgrade finetype, and why: an older one can silently mistype columns.
- **Validation in the database.** Validating a table in a Protocol (`arc table validate`, `table_validate`) also needs FineType's DuckDB extension, found through the `FINETYPE_DUCKDB_EXT` environment variable. When it is unset, say so and validate the source CSV, or a `parquet_export`, with finetype's own `validate` instead.

## What to say when all is well

Nothing, beyond one line: "arc <version> and finetype <version> are ready." The check is for you, not a ceremony for the user.
