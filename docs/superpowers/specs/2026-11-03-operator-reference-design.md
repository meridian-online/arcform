# Operator reference: what each operator wraps, its defaults and an example step

## Intent

meridian.online's operation pages name each built arc operator in a computed status panel and link it to `operators/<name>/README.md` in this repository, at arcform's latest release tag. The researched prose on those pages never names arcform, so the README is where a reader learns the arcform-specific detail: what the operator does, the tool or method it wraps, its parameters with their defaults, and an example step. arc's own catalogue (`arc operation describe <operator>`, and `arc mcp`'s `operation_describe`) must say the same things, from the same source.

## Goals

- Every operator in the catalogue (13, `opendal_fetch` included) has `operators/<name>/README.md`. The eight written in Rust under `src/` (`parquet_export`, `http_fetch`, `opendal_fetch`, `html_link_discover`, `archive_extract`, `finetype_validate`, `uv`, `ducklake_publish`) get a README-only directory. Hugh: no issue with a directory that holds only a README.
- Each operator's catalogue entry gains `wraps` and `example` (a YAML step), and `arc operation describe` prints both for an operator. `wraps` is a list, because an operator can stand on more than one project (`splink_resolve` on Splink and DuckDB). Each entry has a stable `id` (`splink`, `opendal`, `duckdb`, `umap-learn`), a display `name`, the project's `url` and one sentence on what the operator uses it for. The empty list means arc does the work itself.
- The vendor `id` is what lets the web pages show the vendor. Hugh, 2026-11-03: "some operations are vendor enabled - which is also important to reference. For instance: OpenDal, Splink and even DuckDB have logos. We should include this somehow. N.B. this icon shouldn't displace the symbol we decided earlier which is the 'shape' of the operation - but we may want to reveal vendors." arcform carries the id and no logo files; web binds each id to a logo in one place, as it binds type-icon names to Tabler today, and shows it beside the operation's shape icon, never in its place. That display is web's work, raised as its own web issue.
- Each `with:` schema property that has a serde default carries it as `default`, so the parameters table shows every default.
- Each README holds a Reference section generated from the catalogue: summary, wraps, reads and writes, a parameters table (key, type, required, default, description), and the example step.

## Non-goals

- No research text. The researched entries live in `web`.
- No change to an operation's description: its five keys stay as they are; `wraps` and `example` are an operator's.
- The hand-written prose in the existing READMEs (`splink_resolve`, `text_embed`, `umap_project`) is kept outside the generated section and is not rewritten.

## Anti-goals

- A README that drifts from the operator. A test fails when a README's generated section differs from what the catalogue generates.
- An example step that would not load. A test runs each example through the same `with:` check and asset declaration a manifest load does.

## Approach (Hugh picked option 1)

1. **The catalogue generates the Reference section** (chosen). The catalogue is the one source, and a README cannot fall out of step with it. It costs a small generator.
2. Both fields in the catalogue with READMEs written by hand, checked by tests that parse them. Not chosen: 13 hand tables, and the checks only reach what they parse.
3. READMEs alone. Dropped: the issue requires arc's catalogue to carry this too.

## Calls made on Hugh's behalf

- The generated section sits between `<!-- arc:reference:start -->` and `<!-- arc:reference:end -->`. In a new README it follows a title and one-line intro; in an existing one it goes after the opening section.
- The drift check is a Rust test that renders each operator's section and compares it with the README; `ARC_UPDATE_READMES=1` rewrites the files instead of failing.
- The README shows each vendor as a linked name, with no logo, so this repo ships no third-party marks.
- `wraps` names the tool and its version line where arc pins one (Splink 4 with its DuckDB backend, umap-learn, the subtoken DuckDB extension and so on), from what each operator's code actually calls.
- Defaults in the schema are hand-kept in step with each config's serde defaults, as the schemas already are.
