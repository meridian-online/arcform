# Changelog

All notable changes to arcform will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/),
and this project adheres to [Semantic Versioning](https://semver.org/).

Rationale for each change is recorded in the project's design notes and commit history.

## [Unreleased]

### Added

- **With `ARC_SQL_READER=duckdb`, `arc run` takes what each SQL step reads and produces from DuckDB's own parse of it.** The asset graph a run prints and the run contract both come from the reader over DuckDB 2.0's parse, asked of the DuckDB arc runs (`ARC_DUCKDB_BIN`, or `duckdb` on PATH). A step arc's own parser cannot parse, such as `CREATE TABLE r AS SELECT * EXCLUDE (a) FROM t ASOF LEFT JOIN u USING (k)`, is read as producing `r` and reading `t` and `u` instead of degrading to an opaque node. Both readers' statements pass through one set of rules: a file reader such as `read_xlsx('build/budget.xlsx')` records its file, `range(10)` records no table, a call such as `mlpack_random_forest_train("X", "Y", "params", "model")` draws the warning that asks for `depends_on:` and `produces:`, and a `COPY … TO` records a file or a directory by its options, each as without the variable. The reader needs a DuckDB 2.0 build, which is outside the versions arc is tested on, so a run on the 2.0 preview sets `ARC_ALLOW_UNTESTED_ENGINE=1` as well. `arc run` refuses before a step runs when the variable is set to any value other than `duckdb`, naming the one it takes, and when the DuckDB it runs is older than 2.0, naming the version found. With the variable unset, every step is read as before.

- **`arc history list`, `show` and `restore` take `--file <FILE>`, so a chart file's versions can be listed, read and restored from the command line.** Given a file, each verb addresses that file's own history: `list --file panels/a.yaml` prints that file's entries and none of the Protocol's or another file's, `show <id> --file panels/a.yaml` prints the entry's exact text, and `restore <id> --file panels/a.yaml` writes that one file, leaves `arcform.yaml` and every other file untouched, and checkpoints the text it replaced in the file's history first. A relative `--file` is read from `--dir`. The "does not load as a spec" note after a restore is printed only for `arcform.yaml`, including when it is given as `--file`. With no `--file` the three verbs print and write what they did before.

- **`arc::spec::LocalHistory` keeps a file's versions under that file, so a chart file beside a Protocol has a history of its own.** Each call that takes a protocol directory has a twin that takes a file: `record_save_for_file`, `record_checkpoint_for_file`, `entries_for_file`, `read_for_file` and `restore_for_file`. A file's entries are keyed by its own canonical path, so two chart files under one Protocol list apart, neither lists the Protocol's entries nor the Protocol theirs, and `HISTORY_MAX_ENTRIES` bounds each file separately. `restore_for_file` writes the one file it was given, leaving `arcform.yaml` and every other file untouched, and checkpoints the text it replaced in that file's history first, so the restore can itself be undone. The Protocol's key is unchanged: the file calls given a directory's `arcform.yaml` read the history the directory calls recorded, including a history recorded before this change. A path naming a directory or naming no file is refused. The directory calls keep their signatures, and the file calls are reachable with `default-features = false`.

- **arc's SQL introspection parses DuckDB's `INSTALL … FROM <repository>`, `FORCE INSTALL`, an empty-string `COPY` option (`QUOTE ''`, `ESCAPE ''`, `DELIMITER ''`), a `PRAGMA` call with several positional and/or named arguments, and `SET VARIABLE <name> = <expr>`.** A step that opens with any of these — loading a community extension to read a format finetype doesn't cover, fitting a regression, or embedding text — used to degrade the whole step to an **opaque** node: `arc run` printed `could not parse … — treating as opaque step`, and the tables that step produced or read never reached the asset graph, so a downstream step could run against a table arc still believed was fresh after an upstream rebuild changed it. Three of arcform's own example models opened with `SET VARIABLE` and were opaque for exactly this reason — `examples/brewtrend/models/trending.sql`, `examples/code-lists/models/transform_naics.sql` and `examples/code-lists/models/transform_icd10cm.sql` — all three now parse; `examples/brewtrend/README.md` is updated to say so. See `vendor/sqlparser-0.55.0/MERIDIAN_PATCH.md` additions 4 through 7 for the parser changes and `tests/duckdb_statement_forms.rs` for the coverage.

- **`arc::spec::apply_yaml_edits` splices YAML text of any shape, not only a Protocol.** It takes the text and a list of `SpecEdit` values and returns the edited text: the same splice `apply_edits` runs, keeping every byte an edit did not target, comments included, with the final newline its one normalisation. It does not ask whether the result is a Protocol, so a chart file with no `name:` is edited where `apply_edits` refuses it; its one gate is that the result still loads as YAML, and a splice that would not is refused with the reason. It reads and writes nothing. `apply_edits` and `edit_spec` keep the Protocol gate and refuse text that is not a Protocol as before. It is reachable with `default-features = false`.

- **`op: ducklake_publish@1` ends a Protocol by publishing its build into a DuckLake catalog, as one snapshot.** The step reads the Parquet the Run built (`file:`), so it runs after the step that produces it, is skipped when that step is, and runs again only when the file was rebuilt. It attaches `catalog:` in a private in-memory DuckDB, places a byte-for-byte copy of the file under the catalog's data path and registers the copy with `ducklake_add_data_files`, replacing the table's rows in one transaction; the table is created from the file's schema on first publish. It registers a copy because a build rewrites its output in place and DuckLake treats a registered file as immutable: registering the build output directly broke time travel to the published version the moment the next build ran. The copy is hashed as it is written, and that digest is recorded with the snapshot, so a publish of bytes the table already holds is a no-op that reports the existing snapshot rather than committing a second one. A `credential:` names a DuckDB secret type and the environment variables holding its values; when any is unset the step refuses, naming the variables, before anything is attached. The run contract's step entry gains a nullable `report`, which for this operator names the snapshot. A catalog whose data path is object storage is refused in this version.

- **arc can read what each statement of a DuckDB SQL step reads and produces from DuckDB 2.0's own parse of it.** A new reader asks a DuckDB 2.0 build, through its command line, for the step's tokens (`sql_tokenize`) and for the tree of each statement (`json_serialize_sql`), and takes nothing from the parser arc reads steps with today. It splits the step at each `;` outside a string or a comment; reads each table a query names, less the names its `WITH` declares; produces the table of a `CREATE TABLE … AS`, `CREATE VIEW`, `INSERT` or `COPY … FROM`, and the file of a `COPY … TO`; returns each table function as a call with its arguments as written; reads `INSTALL`, `LOAD`, `PRAGMA` and a `SET` of a setting as touching no table, and a `SET VARIABLE` as reading what its value reads; and marks a statement whose reads it cannot take, such as a `DELETE`, as unread rather than as reading nothing. A statement DuckDB cannot parse refuses the step with DuckDB's own message, and a DuckDB before 2.0 is refused with the version found. `ARC_SQL_READER=duckdb` makes `arc run` read steps with it, as the entry above says. A `PIVOT` whose source is joined to another relation, `PIVOT t JOIN u USING (k) ON …`, is marked unread rather than read as reading its first relation alone. Its tests read from answers captured from the 2.0 preview in every `cargo test`, and `ci.yml` downloads the preview and runs them against it when the reader or the workflow changes.

- **`arc operation list` prints the long name of each SQL operation arc holds, and `arc operation describe <long name>` prints what that operation takes.** An agent or a second application that wants to write a step can ask arc which operations exist and what one takes, where before the one operation that can be recorded was described only in the code of the tool that records it. `arc operation list` prints each long name with one line saying what the operation does, and `arc operation list --json` prints the same as a JSON array of `long_name` and `summary`. `arc operation describe filter-rows` prints a JSON object holding the long name, one sentence saying what the operation does, what it is applied to, and `parameters`, a JSON Schema of its arguments. `applied_to` is a list: a `table`, then a `column` that is `of` the table, each with a `name`, a `kind` and a sentence of `description`. The column is where the operation is asked from and is not an argument. `parameters` holds one required string, `where`, and no other key. `where` carries `x-kind: condition` and `x-comparisons`, the six comparisons a condition offers by word, each a `word` and the `sign` SQL writes it as: is `=`, is not `!=`, over `>`, under `<`, between, is null. Both are annotations: `where` is any SQL condition, recorded as written, and holds no `enum` and no `pattern`. The catalogue holds one operation, `filter-rows`; a name arc does not hold is refused with exit 1, nothing on stdout, and a message pointing at `arc operation list`. Neither verb reads a protocol, and neither records a step or opens a database. `arc mcp` does not yet offer the same two answers.

### Changed

- **`SpecEdit::Add` into a sequence item puts the new key at the item's keys' column.** The indent was read off the first line below the item that was neither blank nor a comment, and when the item's first key opens a nested block (`- plot:` over `    - mark: lineY`, or a step opening with `- depends_on:` over a list) that line is the nested value, so the key landed inside it and the result did not load. The column of the first key on the `- ` line is now the answer; a mapping that opens on its own line is unchanged.

- **A SQL step's table function is recorded by what it reads, not by its own name.** `read_xlsx`, `read_xml`, `read_dta`, `read_stat` and `ST_Read` now lift the file path (or glob pattern) they read as the lineage input, the way `read_parquet`/`read_csv`/etc. already did, instead of recording the opaque function name as a table; `glob(...)` lifts its path pattern the same way. `range(...)`, `generate_series(...)`, `duckdb_functions()`, `duckdb_tables()` and `duckdb_secrets()` produce rows from no backing table at all, so a step reading one of them records no table input, and the asset graph a run prints has no node under the function's name. Any other table-valued function called with no string and no quoted identifier, such as a table macro like `recent()`, still records its own name, unchanged; one called with a string or a quoted identifier records nothing under its name and draws a warning, as the entry below says.

- **`arc run` can be told which DuckDB to run, and refuses a DuckDB outside `>=1.2, <2` by default.** `ARC_DUCKDB_BIN` names the executable the version preflight and the SQL steps run; unset, arc runs the `duckdb` on `PATH` as before. A set value that is missing, empty, a directory or not executable refuses the run with the variable and the path named, and `PATH` is not tried instead, so a told engine never silently becomes another install. A relative value resolves against the working directory. The version guard used to enforce only a manifest's `engine_version:`, and a manifest stating `">=1.2"` ran on any 2.x; arc now enforces its own tested range on every run with a SQL step, a manifest's constraint narrows it without replacing it, and `ARC_ALLOW_UNTESTED_ENGINE=1` lifts arc's range with a warning while leaving the manifest's constraint in force. A pre-release build is outside the range; an unreadable version still warns and runs.

- **`datapackage_describe` refuses a curated sidecar that forges or contradicts a
  finetype nomination, and warns on one that pre-empts it.** The per-field overlay in
  `descriptor.overrides.json` was a blind key-by-key insert: whatever a field block
  named was copied onto the field with no validation. That is fine while every type in
  the descriptor is inferred and stops being fine the moment finetype can be TOLD a
  column's type, because a nominated field carries `x-finetype-nominated: true` and a
  sidecar that can write that key can make a hand-typed field claim to be a declared
  one — the mark forgeable by exactly the mechanism it exists to distinguish itself
  from. Four outcomes, checked before a single key is copied:

  - **Refused** — a field block that sets `x-finetype-nominated` itself, on any base.
    That mark is the engine's.
  - **Refused** — a field block that sets `type`, `x-finetype-label` or
    `x-finetype-confidence` on a column finetype already marked nominated. A nomination
    is taken as given.
  - **Warned, exit zero** — the same three keys on a column nobody nominated. Nine live
    fields across three published sidecars do this today, and the repo holding them
    never runs `arc` in its own CI, so refusing would redden nothing there and would
    instead fail the next rebuild of three published datasets. The warning is the
    deprecation notice; the refusal follows when the last sidecar is clean.
  - **Warned, exit zero** — a nominated column whose `constraints` the sidecar replaces
    while claiming none of the three keys above. Tightening bounds under a nominated
    label is legitimate curation, so the warning prints both sets rather than refusing.

  The forgery outranks both warnings: a block that sets the mark AND a label is refused
  for the forgery, not warned about for the label.

  **The refusal is structural — it fires on the mark, never on the constraint
  vocabulary.** arcform links no finetype crate and reaches the engine only as a
  subprocess, so deciding whether a type and a constraint keyword belong together would
  mean copying finetype's vocabulary table into this repo. finetype already filters
  every field's constraints to its declared type, so that pair-level refusal is done and
  is not arcform's. Two gaps stay open and are named here rather than left implied: a
  sidecar that overrides `type` alone on a non-nominated field leaves constraints
  finetype filtered for a different type, and a sidecar that supplies `constraints`
  alone bypasses that filter entirely. Both need the vocabulary table; neither is caught
  here.

  **One hole is larger than those two and is named here rather than left to be found.**
  The guard reads the sidecar's `fields` block, which is the block all four published
  sidecars use. It does not see two other paths in the same file that write the same
  field objects: a `resource` override carrying a `schema`, and a top-level `resources`
  array. Either replaces finetype's field list wholesale, so either can carry
  `x-finetype-nominated` through to the published descriptor. Measured against the real
  `arc` binary: the mark in a `fields` block exits 2 and writes nothing, while the same
  mark in a `resource.schema` override and in a top-level `resources` array each exit 0,
  print no warning, and write a descriptor carrying it. A hand-written sidecar can
  therefore still publish a forged nomination mark by moving it one block up. Closing
  that means comparing the merged descriptor's nominated columns against finetype's own
  output rather than against the post-merge state, which is a change to the merge's order
  and not to this guard.

  No configuration changed and no manifest needs editing — a sidecar that curates
  `description`, `title`, `resource.path` or relational metadata behaves exactly as
  before.

- **`umap_project` can place appended rows into a persisted fit instead of refitting the
  map, so an analyst who adds data keeps the reading they already formed.** `--fit PATH`
  writes the fitted projection on the run that fits it and reads it back on every later
  run: a row whose numbers the fit already holds keeps the exact coordinates the fit gave
  it, and a row the fit has never seen is placed into that same layout with
  `UMAP.transform`. Without `--fit` nothing changes and the whole input is refit.

  **Measured rather than asserted.** On a 60-row fixture appended to 66, every one of the
  60 pre-existing rows comes back as the same float — maximum absolute difference 0.0 —
  and the refit control beside it moves those same rows by up to 11.5 map units.
  `operators/umap_project/test_umap_project_fit.py` runs that comparison for real, in CI.

  **A row's identity is its own feature values, not its position in the file.** A SQL
  step with an `ORDER BY` puts an appended row wherever its sort key falls, so a rule
  that assumed appends arrive last would call most of the table new and re-place rows the
  fit already holds — the exact movement this flag exists to prevent.

  **`projection_fit_id` now names the LAYOUT when a fit is persisted**, not the run's
  input. A file with appended rows carries the same id as the file before the append, so
  the two may be read row for row against each other; a refit carries a different one,
  which is what says they may not be. That is the discrimination this column previously
  could not offer, because every run was a full refit and "the data changed" and "the
  layout changed" were the same event.

  **A fit that does not describe the current input is refused, naming what differs.**
  Different projected columns, a different vector width (a fit for 256-d embeddings
  against an input now carrying 384-d), a different `neighbors`/`min_dist`/`metric`/seed,
  a different umap-learn, a file naming another operator, or an input a fit row is no
  longer present in — every one of those LOADS and produces plausible coordinates, which
  is why each is compared rather than trusted. The comparison is a pure function over a
  header the fit records, so CI drives every field of it with the standard library alone.

  **And a file that is not a usable fit at all is refused the same way.** A header
  comparison can only answer a file that HAS a header. An empty file left by an
  interrupted write, a Parquet passed to `--fit` by mistake, a truncated or corrupted
  fit, a record missing the row identities or the fitted projection behind that header,
  or `--fit` naming a directory — every one of those was an unhandled Python traceback,
  and two of them were worse: a record whose `reducer` cannot place a row RAN TO
  COMPLETION and wrote plausible coordinates, because the reducer is touched only when
  there are appended rows, so the failure waited for the first append. Every use of the
  file's contents now happens inside one function that returns three plain values or
  raises a refusal, and the operator's own test sweeps the fit it just wrote — every key,
  every header field, truncation and byte-corruption across its length — rather than a
  list of cases written beside it.

  **A manifest sets this as `fit:`, and the fit is an asset the step both reads and
  produces** (`umap_project` 1.1.0 -> 1.2.0; `@1` in a manifest still resolves it, and a
  Protocol that sets no `fit:` gets the argv, the assets and the map it got before). The
  path resolves against the protocol directory the way `input:` and `out:` do, and its
  parent directory is created before the script is asked to write into it.

  What the declaration buys is that arc can see the fit at all. A second run over an
  unchanged input is hash-clean and SKIPS, so the fit is found rather than re-done and no
  `uv` is spawned; delete the fit, or rewrite its bytes, and the step goes stale and
  refits. With no `fit:` declared the same file on disk is one no step is answerable for,
  and deleting it changes nothing arc can see — that control is what makes this a claim
  about the declaration rather than about the file.

  Two objections were raised against this field before it was built. The self-edge is a
  shape the asset graph already carried for SQL steps that read and write one table, and
  this is the first operator to declare it. The other was the real one — a fit's pickled
  bytes are not reproducible run to run, so hashing them would report a step stale that
  is not — and the answer is that the fit is written once and not rewritten: an append
  run reads it and leaves the bytes alone, so the hash recorded after one run is the hash
  the next run computes.

  **Moving a knob while a fit exists is refused**, which is the refusal above doing its
  job rather than a new one: `neighbors:`, `min_dist:` and `metric:` are part of what a
  fit is, so editing one marks the step stale and the run then finds a fit built under
  the old value. Asking for a new layout under new knobs means deleting the fit, which is
  a decision rather than something a re-run makes on an analyst's behalf by moving every
  point. See `operators/umap_project/README.md`, "Placing appended rows into a persisted
  fit."

- **`eval/map-refit-stability/check_findings.py` checks every statement of a figure, not
  the first one it finds — and the placement bounds now run in CI.** The checker asked
  whether a correct rendering existed ANYWHERE in a file, so a stale copy elsewhere was
  invisible, and one had shipped: the operator README stated the `.transform()` gap
  correctly in one paragraph and staled it in its conclusion three paragraphs down. That
  figure is corrected, and `require_every`/`require_exact_set` now take a figure's SHAPE
  and compare every rendering of it in both files — reporting, too, when a shape matches
  nothing, because a check that has stopped checking must not read as agreement.

  `eval/map-refit-stability/placement_bounds.py` holds the bounds a placement measurement
  clears. `price_transform.py` asserts them at harness time as before; `check_findings.py`
  applies the same functions to the committed pricing JSON in CI, which is the half that
  was missing — nothing in CI ran the harness, so whether its bounds were clearing was
  never observed. A third bound is added: the two fidelity arms must not be the same
  number. Setting `.transform()`'s fidelity equal to the full refit's — what scoring its
  rows against the full refit's own base pool produces — cleared every previous bound at
  all three fractions, and the page would then have read that out-of-sample placement is
  exactly as faithful as a refit. `test_map_refit_findings.py` is the self-test and runs
  in CI ahead of both checkers.

- **`text_embed` takes its vectors from the DuckDB embedding extension instead of
  computing its own, so one static embedder exists in the product rather than two.**
  An analyst who embeds a column in SQL and an analyst whose Protocol embeds the same
  column now get the same numbers, and before this nothing made that true.

  It was not true. Measured against byte-identical weights over the corpus in
  `tests/text_embed_parity.rs`, the Python path and the extension agreed to float32
  summation order on ordinary text, case folding, accents, empty text, whitespace and
  an apostrophe — and disagreed on three shapes: text made only of tokens outside the
  vocabulary (the Python path averaged the tokenizer's unknown-token row into a
  unit-norm vector, the extension drops those ids and returns a zero vector), such
  tokens mixed into real words, and long text (the extension truncates it, the Python
  path did not). Nothing on either side went red, because each was correct on its own
  terms. The causes were confirmed rather than inferred: dropping the unknown-token row
  and applying the extension's truncation in the Python path drove every divergence to
  zero or to float32 summation order. Against `model2vec.StaticModel.encode` the
  extension agreed on every case in that corpus that is not NULL and the Python path
  did not, so the Python path was the side that was wrong, and it is deleted rather
  than corrected. `numpy`, `safetensors` and `tokenizers` leave the script's dependency
  header; `duckdb` alone remains.

  **No divergence magnitude is quoted, deliberately.** The Python path is deleted, so
  nothing regenerates a difference against it and no test reddens when such a figure
  goes stale. What is quoted below is the truncation boundary, which a probe pins.

  **`extension:` is a new required field, and the artifact is a declared `reads` asset**
  on the same terms the model directory used to have: an input the Protocol puts there,
  never a download and never a registry install. It is hashed for staleness, so
  swapping the artifact re-runs the embedding rather than leaving vectors from a
  different embedder in place, and `arc run` refuses before it spawns `uv` when it is
  not on disk.

  **`model:` is now optional, and is checked rather than loaded.** The weights live
  inside the extension, so a model directory can no longer be read for them; what it
  can do is state which model the Protocol believes it is embedding with. The
  extension publishes a content address over its bundled assets, this operator
  recomputes it from the declared directory, and a mismatch stops the run naming both.
  Reproducing the address needs **three** files — `tokenizer.json`,
  `model.safetensors` AND `config.json` — plus the release identity and revision, which
  none of the files carries: hence `model_release: <model-id>@<revision>` beside
  `model:`, and hence a directory holding only the weights and the tokenizer is refused
  by name rather than checked against a weaker digest. Both fields are required
  together, because either alone is a check that silently does not happen.

  **Two claims this operator made about itself were false and are corrected.** The
  `1.7e-08` agreement with `model2vec.StaticModel.encode` held only for text with no
  out-of-vocabulary tokens and short enough not to be truncated. And text made only of such tokens did
  **not** embed as a zero vector — it averaged the tokenizer's unknown-token row into a
  unit-norm vector for text nothing had been understood of, and those rows were not
  counted on stderr. Both statements are true of the new path: with the documented
  `coalesce(t, '')` bridge, NULL, empty, whitespace and out-of-vocabulary-only text all
  embed as a full-width zero vector, and all of them are counted.

  **Vectors move for out-of-vocabulary and truncated text, and no published dataset
  is affected** — nothing published carries an embedding column. `eval/map-refit-stability`
  is regenerated in the same change and its numbers move: UMAP is refit from vectors
  that are no longer bit-identical, and a refit moves every point, which is the very
  effect that eval exists to measure.

  **Long text is truncated at whichever of two cuts comes first** — the raw text at
  **3,072 characters**, before tokenising, and the token ids at **512** — which the
  extension does and this operator inherits. The character cut is `512 × the model's
  median token length`, and for ordinary English it usually wins: a text can be
  truncated while still under 512 tokens. It is stated in the operator's README and is
  not reported per-run; the count belongs in the SQL surface, where the person who
  cannot otherwise tell is sitting.

  The fixture's tiny generated model and `make_fixture_model.py` are deleted with the
  code that read them, and the fixture Protocol declares the extension instead. The
  artifact is tens of megabytes and is not committed, so the end-to-end tests stage one
  from `ARC_SUBTOKEN_EXTENSION` and return early without it; what CI runs from that
  file is the refusal when the extension asset is missing, decided in Rust before
  anything is spawned. `tests/text_embed_parity.rs` carries the value-for-value
  comparison over the diverging cases, a probe that pins both truncation cuts exactly
  (each stated so that a boundary one character or one token out of place reddens it),
  a declared model whose address agrees with the extension's for its leading characters
  and diverges after — so a model check narrowed to a prefix of the published address
  accepts it and reddens — and, running everywhere and needing nothing, a check that
  its own comparison predicate still tells two vectors apart.

- **A SQL step that calls a table function whose arguments arc does not read draws a warning, and the function's name is no longer recorded as a table.** A table-valued function called with a string or a quoted identifier among its arguments, other than the file readers, `glob` and the row-generators that the "table function is recorded by what it reads" entry in this section names, names tables, files or queries — `mlpack_random_forest_train("X", "Y", "params", "model")` reads three tables and writes one, `query('SELECT …')` reads whatever the query reads — and arc has no signature to say which. Until now such a call recorded the function's own name as a table, which is no table and gave the step's real inputs no staleness. It now records nothing under that name, and `arc run` prints one warning per function and step that names both and says `depends_on:` and `produces:` declare what the step reads and writes. A step that declares either, or that an `assets:` entry says produces something, draws no warning and reads and produces what it declared. A call with no string and no quoted identifier — `range(10)`, a table macro such as `recent()` — is unchanged: no warning, and the macro still records its own name. `query(...)` and `query_table(...)` draw the warning.

- **A `COPY (SELECT …) TO 'file'` records the file it writes.** The query's tables were already recorded as read; the target file is now recorded as produced, as `COPY <table> TO 'file'` records it, so the file appears in the asset graph a run prints and in the run contract, and a step that reads the file later depends on the step that wrote it and re-runs after that step's SQL changes the file, where before it was skipped as fresh. The target is classified from the statement's own options as the table form's is: `PARTITION_BY` with columns, `PER_THREAD_OUTPUT`, `FILE_SIZE_BYTES` and `ROW_GROUPS_PER_FILE` make it a directory. A `COPY (SELECT …) TO STDOUT` names no file and records none. `COPY <table> TO 'file'` and `COPY <table> FROM 'file'` record what they recorded before.

- **`arc run` refuses a Protocol whose SQL installs a DuckDB community extension off the vetted list.** arc holds a list of vetted community extensions in `src/vetted_extensions.json`, shown in `docs/VETTED_EXTENSIONS.md`, and before a step runs it reads the SQL of every step and hook. It refuses `INSTALL <name> FROM community` for a name not on the list, an `INSTALL` from an address or from a repository other than `core` and `community`, an `INSTALL` whose name is a path, a statement naming `custom_extension_repository` or `autoinstall_extension_repository`, and a statement or string naming `enable_peg_parser` or `allow_parser_override_extension`, which switch DuckDB to a second parser that does not nest block comments or read backslash escapes in `E'…'`; the message names the step or hook, the file and line, and what was installed. `FORCE INSTALL`, any letter case, and an `INSTALL` after `EXPLAIN ANALYZE` are read as `INSTALL` is; a step arc's parser cannot read is checked too. Each SQL file is read as the DuckDB CLI reads a file given with `-f`, by rules copied from DuckDB v1.5.5's source: a `.` or `#` line where no statement is open is not SQL, the parser's unicode spaces (a byte-order mark, U+200B and others) read as spaces, a `--` comment ends at `\r` as well as `\n`, two strings with a line end between them are one, and comments and strings are not read as SQL. `INSTALL` from `core`, `LOAD`, and SQL a step builds or writes while it runs (`query()` on an assembled string, `IMPORT DATABASE`, a written `~/.duckdbrc`) are not checked, no variable lifts the refusal, and the check confines nothing a `command:` step does. A vetted extension on an engine version its entry does not name draws one warning and the run goes ahead. `install_from_a_url_that_does_not_resolve` now holds that arc parses the statement, since the run is refused before a step.

### Added

- **`datapackage_describe@1.1.0` passes declared column types to finetype through a
  new `nominations:` key.** A step whose `with:` block carries
  `nominations: nominations.finetype.json` resolves that path against the manifest
  directory and appends `--nominations <FILE>` to its `finetype profile` call, so a
  declared type reaches the engine, and comes back marked `x-finetype-nominated`,
  instead of being hand-copied into `descriptor.overrides.json`. Before this key the
  step's config refused any unknown field, so the only place a maintainer could write
  a type was the sidecar. `@1` still resolves; a step without the key sends no flag
  and behaves as before.

  - **New refusal: a finetype older than 0.6.60 on a step with `nominations:`.**
    `MIN_FINETYPE_NOMINATIONS_VERSION` is 0.6.60, the first finetype release whose
    `profile` takes `--nominations`. Without it, the general 0.6.54 floor would admit
    0.6.54 through 0.6.59, and the step would die inside the subprocess on clap's
    unknown-flag error, which names no release to install. The refusal happens before
    `profile` is spawned and names both the required and the found version. A step
    without `nominations:` is still held to 0.6.54.
  - **Fixed: editing a curated input left `describe` hash-clean.** The step recorded
    the Parquet as its only read, so an edit to `descriptor.overrides.json` changed
    neither the step config nor any hashed asset, and a warm `arc run` skipped
    `describe` and kept the stale `datapackage.json`. Both the sidecar and the
    nominations file are now reads: editing either re-runs `describe` alone, and the
    steps upstream of it stay `[skip: hash_clean]`.

- **`embed_project` is split into `umap_project` and `text_embed`, because one name
  over two jobs made each one reachable only through the other.** An analyst who
  wanted vectors — for similarity, clustering, deduplication, or as classifier
  features — could not get them from a Protocol without also computing a 2-D map they
  had not asked for, and an analyst who wanted a map of columns that were ALREADY
  numbers could not use the step at all, because it insisted on embedding text first.
  The published Embedding Atlas gallery makes the second case concrete: its housing
  example draws a map from a longitude and a latitude with no embedding anywhere
  behind it, and the merged step could not serve it.

  `umap_project@1` takes `columns:` — a list of columns that are already numbers — and
  writes `projection_x` and `projection_y`. A numeric scalar contributes one feature; a
  list or array of numerics (a vector column) contributes one per element, so
  `[longitude, latitude]` maps a table of places and `[embedding]` maps whatever wrote
  a vector column, without the operator knowing which. A fixed-size array survives a
  Parquet round trip as a plain list, so `FLOAT[16]` and `FLOAT[]` are both accepted; a
  chained Protocol would otherwise refuse its own previous step's output. A column that
  is not a number is refused naming the column AND the type it found. A new `metric:`
  (`euclidean` or `cosine`) is how a Protocol says what distance between two rows
  means — `euclidean` by default, which is umap-learn's own and the right reading of an
  arbitrary feature matrix. Nothing scales your columns, deliberately: under euclidean
  a wider-spread column dominates the layout, and that decision belongs in the SQL step
  that selects them, where it is visible.

  `text_embed@1` writes vectors and nothing else, into a `FLOAT[]` column named by
  `vector_column:` (default `embedding`). It carries no projection knob, and
  `umap_project` carries no text column and no model — `deny_unknown_fields` makes a
  manifest that mixes them stop at load rather than silently ignore the field.
  **`text_embed` is marked PROVISIONAL in its own source and README, naming the DuckDB
  embedding extension as where the capability is going**: embedding is a table lookup
  and a mean, and it has no business at the `uv` tier. As shipped here the same
  capability was implemented twice in two languages — a cost named rather than tidied
  away, and one the next entry in this release settles.

  `embed_project` is GONE, not aliased — nothing outside arcform referenced it, so the
  rename is free today and would have been a breaking change the moment a Protocol
  depended on it. `op: embed_project@1` now fails manifest validation with the
  unknown-operator refusal, which is a better answer than an alias quietly resolving to
  one of the two halves. Both new names start at `@1`: neither is the old operator at a
  later version, because each does strictly less than it did.

  Everything the merged operator pinned is preserved. `op@1` still addresses exact
  script bytes; the projection's seed is still frozen in the script rather than exposed
  in `with:`; threads are still pinned before numpy/numba import and row order through
  an explicit ordinal, so two runs over the same input still emit byte-identical
  Parquet within one resolved environment. The model is still a DECLARED READ of kind
  Directory rather than a download — a node in the asset graph, hashed for staleness,
  and a model that is not on disk still stops the step non-retryably before `uv` is
  spawned, naming the file it looked for. Both operators are registered
  unconditionally, beside the other uv-run ops: arcform validates a manifest against
  the same catalog it executes from, so a feature gate named for the transport would
  force a consumer that only wants to READ a Protocol naming these steps to claim the
  capability to run them.

  `umap_project.py` imports duckdb, numpy and umap inside `main()` rather than at module
  scope, so the half of the script that DECIDES — the column-type classifier, the
  feature-width check, the SQL quoting — is importable with the standard library alone
  and is covered by `operators/umap_project/test_umap_project.py` in CI. The
  end-to-end tests need `uv` and skip on every runner here, so without that the script
  had no CI coverage at all.

- **`datapackage_describe` stamps `x-finetype-version` into every descriptor it
  writes** — the dotted version reported by the SAME `finetype` binary the step
  already resolves and runs to type the columns, so a descriptor names the engine
  that produced it and a stale one is visible by reading the file rather than by
  trusting whatever produced it. The stamp is written after the curated
  `descriptor.overrides.json` sidecar is merged in, so a sidecar cannot supply or
  overwrite it — the field is machine-derived, not hand-curated. A new optional
  `expect_finetype_version` on the operator's `with:` block (piped to describe.py's
  `--expect-finetype-version`) pins a run to one exact release: unlike the existing
  `--min-finetype-version` floor, which passes anything at or above it, a pin
  refuses a NEWER binary too if it is not the one asked for, naming both versions
  in the refusal. This replaces the shape of a per-dataset `stamp_finetype_version.py`
  script that recorded the same fact after the fact, outside the step that ran
  finetype — one change here now covers every descriptor the operator produces.
- **A `tool:` precondition declares the external binary or artifact a step depends on**
  — a fourth precondition kind, beside `modified_after`, `fresh` and `command`. A
  step's staleness hash covers what the manifest says and nothing about the machine it
  runs on, so a step whose output is decided by a binary somewhere on that machine is
  clean for ever while the binary moves underneath it. The declaration has two halves,
  each explicit: where the tool is (exactly one of `name:` on `PATH`, `path:`, or
  `env:` naming a variable that holds the path) and what identifies it (exactly one of
  `version:`, a shell command with `$ARC_TOOL` bound to the resolved path, or
  `contents: true`, the sha256 of the resolved file's bytes). Both shapes are needed:
  a released binary announces a version, while an artifact rebuilt in place keeps its
  path and its version and changes only its bytes. `arc` refuses at manifest load when
  either half is missing or given two ways.

  Same identity as the step's last successful run = skip, any difference = run, no
  prior identity = run. The identity is observed at plan time and recorded after the
  step succeeds — `evaluate` writes nothing, so asking whether a step is stale cannot
  change the answer, and a step un-skipped by this gate and then never reached runs
  again. Identities live in `build/.arcform/tools.json` beside the run records, so
  deleting `build/` forgets them and the step goes stale. A skip yields the new
  `precondition_tool` reason in the run contract, distinct from `hash_clean` and from
  `precondition_fresh`.

  **A tool that cannot be identified halts the run** — it is never "fresh", and not
  "stale" either. `modified_after` calls a missing file stale because the step itself
  produces that file, so running it is the remedy; nothing a step does creates the tool
  it was told to depend on. The run stops at plan time naming the step and the path the
  lookup reached, rather than surfacing later from whichever operator tripped over the
  absence.

- **`parquet_export` can stamp key-value metadata into the Parquet footer** — a new
  optional `metadata:` mapping on the operator's `with:` block, written through
  DuckDB's `KV_METADATA` copy option, so a dataset arcform writes can carry a
  description inside the file rather than only in a sidecar. Keys and values are
  strings written as their UTF-8 bytes; Parquet's footer map is untyped, so DuckDB
  reads them back as `BLOB` and `decode(key)` / `decode(value)` over
  `parquet_kv_metadata()` recovers the text — a `::VARCHAR` cast does not, it yields
  DuckDB's escaped rendering. The operator takes no view on what the keys mean.
  Entries are emitted in sorted key order (the config is a `BTreeMap`), because
  DuckDB writes the map into the footer in the order given and an unordered map
  would move the output bytes between runs.

  **Effect on output bytes, measured rather than assumed.** An export declaring no
  `metadata:` — or an empty map — emits no `KV_METADATA` option at all and is
  byte-identical to what the operator produced before, so no existing output moves.
  An export that does declare metadata necessarily changes the file and therefore
  its hash, but the change is confined to the footer: every data page is
  byte-identical, and the same stamp twice is the same file, so the `order_by`
  clause still buys the reproducibility it was added for. A publish step that pins
  the hash of a file that starts being stamped re-pins it once, not on every run.
  An empty map has to take the no-metadata path in any case: DuckDB rejects
  `KV_METADATA {}` as a syntax error.

- **Local history + machine-edit checkpoints** — the middle tier between editor undo
  and version control, and the tier that makes machine edits safe to accept. Saves
  record debounced, bounded snapshots of `arcform.yaml` under `~/.arcform/history`
  (override: `$ARCFORM_HISTORY_DIR`) — outside the protocol directory, so nothing
  appears in `git status` or a diff; at most 50 entries are kept per spec (oldest
  pruned first) and saves within 10 seconds of the newest save merge into it. Machine
  edits go through checkpointed roads — `edit_spec_with_history`,
  `record_step_with_history`, now used by `arc create-protocol` / `arc edit-protocol` —
  that snapshot the state being replaced *before* writing: no checkpoint, no write.
  New `arc history list|show|restore` verbs list the recorded states (with the
  retention policy printed under them), print an entry's exact bytes, and roll a spec
  back — restore checkpoints the state it replaces, so a rollback is itself
  reversible, and none of it needs a git repository or an account. Nothing is ever
  promoted to git: the only automatic promotion is undo → local history at the save
  boundary.
- **The record path** — `arc::spec` can now promote an exploration into a step:
  `record_step` writes the captured SQL as a new numbered model under `models/` (create
  mode, opened by a one-line `-- generated:` provenance marker) and splices a
  `name:` + `sql:` step onto the manifest through the same gated, byte-preserving write
  path — refusal-first, so a promotion that cannot apply leaves the directory untouched.
  The step name must record faithfully: it is spliced as a plain YAML scalar, so a name
  that would not read back as itself (newlines or other control characters, `#`, `:`,
  surrounding whitespace, a leading YAML indicator) is refused up front, and the
  reloaded document is checked to carry exactly the step that was asked for.
  `amend_step_sql` regenerates a model only when it carries the marker: the marker is
  the license to regenerate, and a hand-authored model is refused with the remedy in
  the reason (record a new step downstream instead). Recording never runs anything —
  a recorded step's outputs exist only after `arc run` says so, proven by a parity test
  that grows a spec purely by recording and executes it under the bare binary.
- **CLI authoring verbs** — `arc create-protocol` writes a fresh `arcform.yaml` from
  scratch and `arc edit-protocol` amends an existing one (`replace`, `rewrite`, `add`,
  `append`, `delete`, `reorder`, addressed as `steps[2].command`-style paths). Both are
  argument surface over the spec write path — the same splice, the same validation gate,
  the same atomic write the library gives every caller, so an agent can author and amend
  a runnable protocol with nothing but the binary in the loop. An edit that cannot apply,
  or whose result would not load, is refused with the reason before the file is touched;
  every untargeted byte is preserved verbatim, and no verb reformats as a side effect.
- **The spec write path** — `arc::spec` now edits specs as well as loading them:
  `SpecEdit` describes a change as an inspectable value (`Replace`, `RewriteFragment`,
  `Add`, `Append`, `Delete`, `Reorder`), `apply_edits` applies it to the original bytes
  and gates the result through the real loader in memory, and `edit_spec` is the
  one-shot apply → validate → atomic-write against a protocol directory. Edits splice
  raw bytes at tree-sitter node spans (via `yamlpath`), so every byte an edit does not
  target — comments, blank lines, key order, quote style — survives verbatim; the only
  normalisation is a final newline. A result that will not load is refused with the
  loader's reason and the file on disk is untouched; writes go through a temp file +
  rename, so an interrupt can never leave a truncated spec. Delete/reorder follow a
  documented comment-ownership convention (a `#` block flush against an item is that
  item's header and travels with it). `create_spec` covers the other mode: a brand-new
  spec serialises directly — no preservation machinery — through the same gate and the
  same atomic write. New corpus example `examples/almanac` exercises inline
  `command: |` block scalars, which the rest of the corpus lacked.
- **Execution resilience** — step `retry` with exponential backoff, plus step- and
  pipeline-level `timeout_sec`. Transient API failures retry; stuck processes are killed.
- **Pipeline parameterisation** — runtime `arc run --param KEY=VALUE` with manifest defaults,
  `dotenv` loading, and command-step output capture. Values reach shell steps as `ARC_PARAM_*`
  env vars and SQL via DuckDB `getenv()`; SQL passthrough is preserved.
- **Lifecycle hooks** — `on_init`, `on_success`, `on_failure`, and `on_exit` handlers for
  pipeline setup, teardown, and notification. Hook failures are non-fatal to the exit code.
- **Step preconditions** — typed freshness checks (`modified_after`, `command`) that skip a
  step when all pass.
- **Asset registry & SQL introspection** — SQL steps auto-discover their inputs/outputs via
  sqlparser-rs; command steps declare assets manually. Unparseable SQL degrades to an opaque
  step with a warning.
- **Run state tracking** — step hashes and run metadata persisted in the DuckDB database for
  staleness detection across runs.
- **Registry CLI scaffolding** — `arc registry list/show/fetch/run` command tree, index/resolver/
  cache/transport modules, and sandboxed tarball extraction. Production transport is not yet
  wired to a live index.
- **brewtrend reference pipeline** — the first complete runnable example under
  `examples/brewtrend/`, exercising command + SQL steps, preconditions, retries, and a runtime
  parameter; ships a Frictionless Data Package describing its output.
- **`umap_project` writes `projection_fit_id`** (1.1.0). This operator persists
  nothing between invocations, so appending rows and re-running today means the
  whole map is refit and every point can move — measured at 3,000 real rows against
  a frozen, committed corpus (`eval/map-refit-stability/`, re-derivable with `uv run
  eval/map-refit-stability/measure.py`): a 5% append already shares only 42% of a
  point's 20 nearest map-neighbours with the pre-append layout, growing to 39% at 20%
  and 36% at 50%. That is the same order of magnitude as a sibling measurement (in a
  different repository, its source not committed here, so this figure is context and
  not one `check_findings.py` pins) of how much neighbourhood structure survives
  swapping the embedding model entirely on a comparable corpus (0.13-0.40 depending on
  text length) — neither disturbance is uniformly worse than the other; a 5% append
  actually preserves more structure than any measured embedder swap, while a 20% or
  50% append loses slightly more than swapping the embedder on its easiest case.
  `umap-learn`'s `UMAP.transform()` — public, and verified by calling it
  (`eval/map-refit-stability/price_transform.py`) — CAN place new rows into an
  existing fit without moving the old ones (0.0 measured drift in the base rows'
  own coordinates across every `.transform()` call), priced at a 3.6 MB persisted
  model for a 3,000-row base. Placement fidelity is read against this corpus's own
  ceiling, not against 1.0 or against the churn figure above, which is a different
  quantity: scored against the 256-d embeddings as ground truth, a base row's own
  fit recovers 30.3% of its true neighbourhood at k=20, a full refit places a new
  row at 89-97% of the ceiling (29.6-27.0%, least close at the 50% append, where
  the recommendation is most exposed), and `.transform()` places the same rows at
  27.2-26.4% — 3 to 4 points under the ceiling, -0.2-2.6 points under the full refit
  itself (ahead of it, marginally, at the largest append). Refitting only on an
  explicit user action, by contrast, is not implementable in arcform today —
  `compute_staleness` marks any step whose declared input changed stale
  unconditionally, and no `arc run` flag can hold a hash-stale step un-run. See
  `operators/umap_project/README.md`, "Appending rows moves the whole map," including
  where the asymmetry argument for pinning stops being obvious. A DIFFERENT
  `projection_fit_id` means a shared row's position must not be compared between two
  files; it cannot today distinguish the data changing from the layout changing,
  because every run is a full refit and those are the same event — a matching id
  means the same data and knobs were used, not that the coordinates are guaranteed
  identical (this operator's dependency resolve is not pinned — see "Does
  byte-identity survive a dependency upgrade?" above). Persisting the fit and
  discriminating the two is real design work of its own and is left to a separate
  separately.

### Changed

- **`datapackage_describe` no longer runs Python.** A Frictionless Data Package is a
  specification, not a runtime: the operator used to wrap `describe.py` — a frozen
  script, run via `uv run --script`, that shelled to `finetype profile … -o
  datapackage` and merged the curated `descriptor.overrides.json` sidecar over it in
  a Python dict — and there was no library behind that script to justify the Python
  runtime it needed. The JSON merge (`merge_datapackage` + `check_relations`) is now
  native Rust over `serde_json::Value`; the machine-decidable half is UNCHANGED — the
  operator still shells the `finetype` CLI directly (no longer through `uv`/Python)
  and forms no opinion of its own about column types. Verified byte-identical to the
  retired path against the real Parquet + `descriptor.overrides.json` for all four
  published datasets (`edgar`, `naics`, `gleif`, `edgar_gleif`) — the only field that
  differs between the two is `created`, the per-run timestamp `finetype profile`
  itself stamps on every invocation, which was already non-deterministic before this
  change. Confirmed separately with no Python interpreter or `uv` anywhere on PATH
  (only `finetype` and the `duckdb` CLI every `arc run` already needs): the operator
  still describes the dataset; the retired uv-run substrate could not have. `serde_json`
  carries no `preserve_order` feature in this crate's own dependency edge (confirmed
  via `cargo tree`), so its `Map` is BTreeMap-backed and keys serialize sorted at
  every nesting level with no explicit sort step — matching Python's
  `json.dump(..., indent=2, sort_keys=True, ensure_ascii=False)` byte for byte.
  `operators/datapackage_describe/{describe.py,test_describe.py}` are deleted. The
  operator's version stays `1.0.0`: the `with:` contract and the produced bytes are
  unchanged, the same precedent set when `x-finetype-version` was added (also not a
  version bump). The `op@<version>` guarantee `materialize_frozen_script`'s
  write-if-changed cache gave the embedded script — a behaviour change is a rebuild,
  never a silent edit — now holds by a simpler mechanism: there is no separate script
  materialized at runtime anymore, so the operator's behaviour is entirely the
  compiled binary, exactly like every other in-process operator in this catalog
  (`parquet_export`, `archive_extract`, `finetype_validate`) that never needed
  `materialize_frozen_script` to make that same claim.
- **Raised the minimum `finetype` to 0.6.54 in both places that gate it.** finetype
  0.6.54 stopped eight-digit numbers typing as confident dates. Up to and including
  0.6.53, the year-first and day-first compact date leaves both validated on `^\d{8}$`,
  so any eight-digit token — a financial figure, a surrogate key — came back a
  high-confidence date *with a `strptime` transform attached*. That is a worse failure
  than the mislabelling the previous bump addressed: a consumer that follows the
  transform does not get a wrong name for a correct column, it gets a corrupted one. So
  0.6.53 is now refused as firmly as 0.6.52 was. Both gates name the superseded releases
  in one place — `SUPERSEDED_RELEASES` in the operator's tests and in the `arc mcp`
  module — and each is asserted refused *and* asserted to sit below the floor, so the
  constant cannot drift back onto either one unnoticed. Verified against the real 0.6.53
  binary, not only fixtures: the operator exits non-zero on it, and the `taxonomy` tool
  returns `isError` naming both versions, while the same binary reporting 0.6.54 passes
  and serves the call.
- **Raised the minimum `finetype` to 0.6.53 in both places that gate it.** The
  `datapackage_describe` operator and the `arc mcp` finetype proxies each shell out to
  whatever `finetype` is on PATH, and each asserted 0.6.52. But 0.6.53 is the release
  that corrected three labels the published datasets depend on — the ticker column, the
  industry-code level column, and the resolved legal-name column — and the consuming
  website has stopped suppressing the older, wrong labels at display time. An 0.6.52
  binary therefore passed the gate, re-emitted superseded labels, and nothing downstream
  caught it: precisely the stale-binary-on-PATH failure the gate exists to stop, which
  has silently mis-described a published dataset once already. The operator's tests now
  read `MIN_FINETYPE_VERSION` instead of restating a literal, and a new case in each
  language refuses an 0.6.52 fixture outright and asserts the floor sits above it, so
  the constant cannot drift back unnoticed. CI now runs the operator's Python tests —
  `cargo test` never executed them, so the gate had been shipping untested.
- **Feature-gated the HTTP ingress path behind `http-fetch`.** The ureq-backed
  `http_fetch` and `html_link_discover` operators, and the `fresh` precondition's remote
  HEAD probe, now compile only under the new `http-fetch` feature, which `cli` pulls in.
  A pipeline is only ever run through the `arc` CLI and the published library surface is
  `spec` alone, so a crate that links arc with `default-features = false` cannot reach
  this path — yet it previously still compiled `ureq` (and, as ureq's only runtime
  consumer, the whole rustls/ring TLS stack) into its binary as dead weight. With the
  gate, such a consumer drops `ureq` and its TLS stack from the dependency graph
  entirely; `ureq` is now an optional dependency. The default build and the `arc` binary
  are unchanged.
- **Adopted Frictionless Data Package as the data-description standard.** A Data Package
  *describes* data (Table Schema, `foreignKeys`, provenance); it does not execute — the runnable
  artifact stays the arcform manifest. Pipelines may ship a `datapackage.json` describing their
  IO.
- **Engine version assertion** — manifests may pin an `engine_version` semver constraint, checked
  at preflight for local/remote parity.
- **Vocabulary: "assets" not "asset registry"** for within-pipeline data declarations, freeing
  "registry" for the user-facing pipeline catalogue.

### Fixed

- **A frozen operator script is written under another name and renamed into place, so a reader of it opens the whole script and not an empty one.** `materialize_frozen_script` wrote the script with `std::fs::write`, which truncates the file and then writes it, and a caller that found the script already holding its bytes returned the path without waiting for a writer. A second caller that opened the path between the truncate and the write read an empty file: two runs of `arc` on one machine that use the same operator could meet it, and so could two threads of `cargo test`, where `operator::tests::umap_project_invocation_materialises_the_frozen_script_and_names_it` failed with `left: ""` on some runs and not others. The write now goes through the temp-file-and-rename helper the spec editor uses: a file named for the process and a sequence number in the script's own directory, flushed and renamed over the script. A reader opens the whole old script or the whole new one, and a reader that already had the old one open keeps reading it. A script whose bytes on disk are those asked for is still not written again, and the path returned is the one returned before. After a write the script's directory holds the script and no file under the temp name.

## [0.1.0] - 2026-03-31

### Added

- Initial release: the asset-centric step-execution foundation.
- `arc init` scaffolds a new project; `arc run` executes the pipeline in `arcform.yaml`.
- YAML manifest parsing and validation.
- Engine preflight — detects the DuckDB CLI before running.
- SQL passthrough — SQL files are handed to the engine unmodified.
- Shell command steps with real-time stdout streaming.
- Sequential step execution with per-step progress feedback.

[Unreleased]: https://github.com/meridian-online/arcform/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/meridian-online/arcform/releases/tag/v0.1.0
