# Vetted community extensions

A Protocol's SQL may install a DuckDB community extension when the extension is on this list. `arc run` reads the SQL of every step and hook before the first one runs, and refuses the Protocol when a statement installs a community extension that is not on it. The list says which extensions a Protocol may install, and a Protocol's own `arcform.yaml` pins which build of each: see [The build each extension is](#the-build-each-extension-is).

The list arc enforces is `src/vetted_extensions.json`, compiled into the `arc` binary. This page shows the same list, and a test fails when the two differ.

## The list

Each extension below has a permissive licence, a build the community registry serves for `linux_amd64` and `osx_arm64`, and a statement that ran it on data and checked the result. The entries were taken from the community registry's descriptors at commit `80bed7b173cd`, and their builds were probed on 2026-09-27.

| extension | licence | repository | DuckDB version | what was run |
|---|---|---|---|---|
| `dta` | MIT | `codedthinking/duckdb-dta` | v1.5.5 | read a Stata file; write one and read it back |
| `finetype` | MIT | `meridian-online/finetype` | v1.5.5 | the semantic type of a column and of a value is inferred from what it holds |
| `h3` | Apache-2.0 | `isaacbrodsky/h3-duckdb` | v1.5.5 | the hexagonal cell for a point |
| `http_client` | MIT | `query-farm/httpclient` | v1.5.5 | an HTTP GET returning status and body |
| `minijinja` | Apache-2.0 | `query-farm/minijinja` | v1.5.5 | a template is rendered once for each row and once over all of them |
| `mlpack` | MIT | `eddelbuettel/duckdb-mlpack` | v1.5.5 | train a random forest, store it and score new rows; a logistic regression; k-means |
| `onager` | MIT OR Apache-2.0 | `CogitatorTech/onager` | v1.5.5 | shortest paths and components from an edge table |
| `rapidfuzz` | MIT | `query-farm/rapidfuzz` | v1.5.5 | a token-sort ratio between two strings |
| `splink_udfs` | MIT | `moj-analytical-services/splink_udfs` | v1.5.5 | names are blocked by the soundex of the surname and compared without diacritics |
| `stats_duck` | Apache-2.0 | `KoliStat/the-stats-duck` | v1.5.5 | a two-sample t-test |
| `stochastic` | Apache-2.0 | `query-farm/stochastic` | v1.5.5 | the normal distribution at its known points |
| `subtoken` | MIT | `meridian-online/subtoken` | v1.5.5 | embed text and rank by cosine similarity |
| `textplot` | Apache-2.0 | `query-farm/textplot` | v1.5.5 | a text plot from SQL |
| `us_address_standardizer` | MIT | `ericmanning/duckdb-address-standardizer` | v1.5.5 | a US address parsed into its parts |
| `webbed` | MIT | `teaguesterling/duckdb_webbed` | v1.5.5 | parse XML into rows; write rows as XML |
| `zipfs` | MIT | `isaacbrodsky/duckdb-zipfs` | v1.5.5 | read a file inside a zip archive |

## What `arc run` refuses

- `INSTALL <name> FROM community`, when `<name>` is not on the list.
- `INSTALL <name> FROM '<address>'`, whatever the name: a URL or a directory.
- `INSTALL <name> FROM <repository>`, for a repository other than `core` and `community`, such as `core_nightly`.
- `INSTALL '<path>'`, whose name is itself a file or an address. DuckDB reads a name holding a `.`, a `/` or a `\` that way.
- A statement that names the setting `custom_extension_repository` or `autoinstall_extension_repository`. After `SET custom_extension_repository = '/tmp/ext'`, a bare `INSTALL mlpack;` fetches from `/tmp/ext`, and the second setting does the same for an extension DuckDB installs on its own when a function needs one.
- A statement that names the setting `extension_directory`. It moves where DuckDB keeps the extensions it installs, so a step that sets it installs a file where the check of each pinned extension does not read it, and where a second DuckDB does not find it.
- A statement that names `enable_peg_parser` or `allow_parser_override_extension`, in any form and whatever value it sets, and a string that names either. These switch DuckDB to its second parser: the DuckDB CLI loads the `autocomplete` extension, whose parser replaces the default one once `allow_parser_override_extension` is `'fallback'` or `'strict'`, as `CALL enable_peg_parser();` sets it. That parser does not nest `/* */` comments and applies no backslash escape inside `E'…'`, so after the switch DuckDB runs statements that the default parser's rules, the ones arc reads by, place inside a comment or a string. A string is refused as well because `query('FROM enable_peg_parser()')` runs the SQL the string holds.

`FORCE INSTALL` is checked as `INSTALL` is, in any letter case, and so is an `INSTALL` that follows `EXPLAIN ANALYZE`, which DuckDB runs. A step whose SQL arc cannot otherwise parse is checked too.

arc reads a SQL file as the DuckDB CLI reads a file given with `-f` with its default parser, which is how `arc run` runs a step. A statement that names the switch to the second parser is refused (above). The rules are copied from DuckDB v1.5.5's source, which is the same as v1.5.4's:

- The CLI's lines and the batches it hands the parser. A line starting with `.` or `#` while no statement is open is a dot command or a comment, and is not SQL, whatever quote or comment it holds. A line starting with the byte `0x03` drops the lines held before it. A NUL byte drops the rest of the chunk the CLI read it in.
- The parser's pre-pass, which reads a byte-order mark, a zero-width space (U+200B) and the other unicode spaces it knows as a space, except inside what it reads as a string or a comment.
- The scanner's rules. A `--` comment ends at a line feed or a carriage return, a `/* */` comment nests, and two strings with a line end between them are one string. Text inside a comment or a string is not SQL.

The refusal names the step or hook, the file and line, and what the statement installs, or the setting or parser switch it names. No variable lifts it.

## What it does not check

- `INSTALL <name>` with no `FROM`, and `INSTALL <name> FROM core`. These install from DuckDB's own repository.
- `LOAD`. SQL does not say where a loaded extension was installed from.
- The build of an extension installed from `core`, or loaded without an `INSTALL … FROM community` in the Protocol's SQL. The pin check below covers the community extensions the SQL installs.
- A `command:` step, and anything a step runs outside its SQL. **This check is not a sandbox.** arc does not sandbox a Protocol: a `command:` step can start a DuckDB of its own and install what it likes, and running a Protocol you did not write is running a shell script you did not read.
- SQL a step builds or writes while it runs, and SQL held in a database it reads. arc reads the text of a Protocol's SQL files, not what that text computes or what a database holds, so it refuses none of these. On three shapes, read by the rules above, `arc run` prints one warning for the step or hook, naming its file and the line of each, and runs the step:
  - `query()` or `json_execute_serialized_sql()` called on anything but one string literal, or on a literal whose SQL makes such a call, in any letter case, quoted, or after a schema such as `main.`. These run a `SELECT` held in a string the step can assemble, such as `'FROM enable_' || 'peg_parser()'`, which switches the parser for the statements after it. A table's name followed by its column list, as in `CREATE TABLE query (a INT)`, is not a call.
  - `IMPORT DATABASE`, or `import_database` as in `PRAGMA import_database('imp')`. These run the SQL files of a directory, which a step can write with `COPY`, and those files can hold any statement, `INSTALL` among them.
  - A string or a quoted name holding `.duckdbrc`, in any letter case. The DuckDB CLI runs `~/.duckdbrc` before each step's file, and a step can write that file.

  The warning says arc does not read the SQL the step builds or writes, and not that the step installs anything. It reads shapes and is not a boundary: a path to `.duckdbrc` assembled from pieces, such as `'~/.duck' || 'dbrc'`, draws no warning. Neither does SQL held in a database. A view or a table macro holds SQL in a database, and a step that reads from it runs that SQL, in the database arc runs the step on or in one the step attaches: after `ATTACH 'lib.duckdb' AS lib;`, `FROM lib.v;` runs what the view `v` holds, and a view holding `FROM enable_peg_parser()` switches the parser for the statements after it, though the step's file does not name the switch.
- A step's SQL file that an earlier step writes over after the check has read it. The check reads each file once, before the first step runs, and a step runs in the Protocol's directory, so `COPY (SELECT 'INSTALL anofox_forecast FROM community;') TO 'models/s2.sql' (HEADER false, QUOTE '');` in one step replaces the file a later step runs.
- A SQL file that is not there when the run starts, such as one an earlier `command:` step writes.
- What a DuckDB CLI dot command in a SQL file runs, such as the file `.read` names or the program `.shell` starts. arc skips the dot command's line, as DuckDB does.
- A DuckDB version other than v1.5.4 and v1.5.5. The CLI's rules above were read from those two, and another version's were not compared.

## The DuckDB version

Each entry names the DuckDB version its build was probed for and its statement was run on. When a step installs an extension on the list and the engine reports a version the entry does not name, `arc run` prints one warning naming the extension, the engine's version and the entry's, and the run goes ahead.

## The build each extension is

A Protocol pins the build of each community extension it installs under the `extensions:` key of `arcform.yaml`, beside `engine_version:`: the extension's name, then the DuckDB version as DuckDB prints it, then the platform as `PRAGMA platform` prints it, then the SHA-256 of the installed file as 64 lower-case hex digits.

```yaml
engine_version: ">=1.5"
extensions:
  mlpack:
    v1.5.5:
      linux_amd64: <the SHA-256 of the file DuckDB v1.5.5 installs on linux_amd64>
      osx_arm64: <the SHA-256 of the file DuckDB v1.5.5 installs on osx_arm64>
```

A manifest whose pin is not 64 lower-case hex digits is refused when it loads, and the message names the pin's place, such as `extensions.mlpack.v1.5.5.linux_amd64`.

- **Before any step or hook runs**, `arc run` installs each extension that a step's or hook's SQL installs `FROM community` and that has a pin for the engine's DuckDB version and platform. It runs `INSTALL <name> FROM community` on the DuckDB the steps run on, with no Protocol database and no `LOAD`, so the extension's code does not run in the check; on a machine that does not hold the extension yet, the install fetches it. arc then hashes the file DuckDB names as the extension's `install_path` in `duckdb_extensions()`, and refuses the run with exit 1 when that hash differs from the pin, when the install fails, or when DuckDB names no installed file. The refusal names the extension, the DuckDB version, the platform, the pin, the hash found and the file. The check runs whether or not the steps are fresh, and under `--force`.
- **An extension with no pin** for the engine's DuckDB version and platform draws one warning naming the extension and the `extensions:` key of `arcform.yaml`, and the run goes ahead with whichever build DuckDB installs. A pin for another DuckDB version or another platform alone is no pin for this one. When arc cannot read the engine's version it cannot choose a pin, and each extension is unpinned. A pin for an extension the Protocol's SQL does not install is neither installed nor compared.
- **`arc run` does not write a pin**, and does not write `arcform.yaml`. An author writes each pin into the `extensions:` key: the SHA-256 of the file that `SELECT install_path FROM duckdb_extensions() WHERE extension_name = 'mlpack'` names, on the DuckDB version and platform the pin is for. A run record holds the SHA-256 of the manifest it ran from, and so of its pins.
- **When the run ends**, after the last step and the `on_exit` hook, and after a failed `on_init`, arc hashes each file it checked before the run again, and fails the run with exit 2 when one differs from its pin, naming the extension, the pin and the hash found; the run record gives an outcome other than `success`, and a run that failed already keeps its own error and prints this one beside it. A step can replace a pinned file during the run with `FORCE INSTALL <name> FROM community`, with `UPDATE EXTENSIONS`, or with a `command:` that writes to it. arc finds the change when the run ends, and the steps after the one that replaced the file may have loaded it by then.
- **An arc older than this change runs a pinned Protocol without checking it.** It reads a manifest holding `extensions:` and ignores the key.

A `~/.duckdbrc` that sets `extension_directory` lies outside the Protocol. The check starts DuckDB as a step's DuckDB is started, so that file moves the directory the check reads and the directory a step installs to together.

## Adding an extension

An extension joins the list with a permissive licence, a build the registry serves for the DuckDB version the entry names on both platforms above, and a statement run on data whose result was checked. Add the entry to `src/vetted_extensions.json` and the row to this page in the same change.
