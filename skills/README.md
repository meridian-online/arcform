# Skills

These are agent skills: short Markdown guides that teach an AI agent the analyst workflow through arc. Each folder holds a `SKILL.md` (and, where it helps, a `references/` folder) that an agent loads when the task calls for it, such as loading a file, profiling a table or writing up a result.

A skill shows each step both as an `arc` command and as the matching `arc mcp` tool, so an agent can follow it whichever way it reaches arc.

They are checked against the binary. `cargo test --features mcp --test skills` confirms that every tool, argument, verb and flag a skill names exists, that its examples run against a small fixture Protocol, and that its frontmatter loads and names the arc version it was written for.

If you are writing a skill, the notation the checker reads (how to name a tool, a CLI form or a runnable example) is described in the module doc at the top of [`tests/skills.rs`](https://github.com/meridian-online/arcform/blob/main/tests/skills.rs). The tests live in the arcform repository, not in a copy of these skills.
