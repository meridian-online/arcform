# A general Splink operator: implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `splink@1`, an operator that scores record pairs with a Splink model the Protocol supplies, move open-analytics' EDGAR to GLEIF crosswalk onto it with its output unchanged, and add the review rule that refuses an operator written for one source.

**Architecture:** A frozen Python script, embedded in arc and run under uv like the other Python operators, registers each input Parquet with Splink 5's DuckDB backend under its role, loads the model JSON, refuses anything that would make a run unreproducible, and writes Splink's predictions sorted by id. The Rust side declares the reads, writes and refusals that can be checked before uv starts. The crosswalk's cleaning and tie-breaks become SQL steps in its Protocol.

**Tech Stack:** Rust (arc), Python 3.12 under uv, Splink 5.0.0, DuckDB 2.0.0.

**Spec:** `docs/superpowers/specs/2026-11-18-splink-operator-design.md`

## Global Constraints

- Operator name `splink`, version 1.0.0, addressed as `splink@1`. `splink_resolve@1` stays in the catalogue until open-analytics no longer uses it; its removal is not in this plan.
- The script pins `splink==5.0.0`, `duckdb==2.0.0` and `pyarrow` at one exact version, in its PEP 723 block. If Task 4's parity check fails on Splink 5 and passes on `splink==4.0.17`, the pin moves to 4.0.17 (with Splink 4's registration API) and the PR says so.
- `with:` keys: `inputs` (list of `{role, path}`, at least one), `model` (path), `threshold_match_probability` (number, 0–1, optional), `threshold_match_weight` (number, optional), `out` (path). Setting both thresholds is refused.
- Output: one Parquet file, Splink's `predict()` table plus `additional_columns_to_retain`, sorted by (`unique_id_l`, `unique_id_r`) using the model's `unique_id_column_name`, and by `source_dataset_l`, `source_dataset_r` first when there are two or more inputs. Byte-identical across two runs on the same inputs.
- Refusals before Splink starts, each naming what it found: a level other than the null level without both `m_probability` and `u_probability`; `link_type` `dedupe_only` with other than one input, or `link_only`/`link_and_dedupe` with fewer than two; an input missing `unique_id_column_name` or a column a comparison level, blocking rule or `additional_columns_to_retain` names; a role given twice.
- Public repos (arcform, open-analytics, methods): no Kata refs, private repo names or local paths in branch names, commits, PR text or tracked files.
- REVIEW.md line, verbatim, under "Meridian-specific — check on each review": `- **An operator is not one source:** a change that adds or extends an operator whose inputs are named for one source, or whose script holds one dataset's columns, endpoints, paging or model, is a finding at Medium or above: an operator takes those from the Protocol's \`with:\`.`
- One cargo build at a time; build and test against DuckDB's v2.0.0 release library, as CLAUDE.md's Known issues describe.

## Review Focus

- An input with zero rows, or no pair above the threshold: the step succeeds and writes a Parquet with the full predict schema and no rows.
- A path containing a space or a single quote: it is read correctly, never interpolated into SQL unquoted.
- The same column with different types in two inputs (VARCHAR in one, BIGINT in the other): refused naming the column, both roles and both types, rather than failing inside Splink.
- A model JSON that does not parse, or is a settings dict missing `comparisons`: refused naming the model path and the problem.
- Neither threshold set: every blocked pair is written (Splink's default), not refused.

---

### Task 1: The frozen script

**Files:**
- Create: `operators/splink/splink.py`
- Create: `operators/splink/test_splink.py` (stdlib only)
- Create: `operators/splink/test_splink_e2e.py` (under uv, Splink and DuckDB)

**Interfaces:**
- Produces: CLI `splink.py --input ROLE=PATH [--input ROLE=PATH ...] --model PATH --out PATH [--threshold-match-probability X | --threshold-match-weight X]`; exit 0 on success, exit 2 with one line on stderr for each refusal.
- Produces, importable without Splink (Splink, DuckDB and pyarrow are imported inside `main()`, as `umap_project.py` does): `model_problems(settings: dict, columns: dict[str, dict[str, str]]) -> list[str]`, where `columns` maps role to `{column_name: duckdb_type}`; returns one message per refusal, empty when the model can run.

- [ ] **Step 1: Write the stdlib tests in `test_splink.py`.** One test per refusal in Global Constraints and per Review Focus line that `model_problems` owns (type mismatch across roles, missing `comparisons`), each asserting the message names the role, column or level. Plus `test_a_complete_two_input_link_model_has_no_problems`.
- [ ] **Step 2: Run `python3 operators/splink/test_splink.py`.** Expected: fails, `model_problems` undefined.
- [ ] **Step 3: Implement `model_problems` and `main()`.** In `main()`, register each input with `DuckDBAPI(connection=con).register(<read_parquet relation>, dataset_display_name=role)`, building the relation with a bound parameter rather than a pasted path. Read each input's column types from DuckDB, call `model_problems`, and refuse before building the `Linker`. Then `Linker(sdfs, settings_path)`, `predict(...)`, and `COPY (SELECT * FROM <predictions> ORDER BY <sort keys>) TO out (FORMAT parquet)`. An empty prediction still writes the schema.
- [ ] **Step 4: Run the stdlib tests.** Expected: pass.
- [ ] **Step 5: Write `test_splink_e2e.py`.** It builds two small Parquet inputs in a temp dir and a fully specified `link_only` model (one exact comparison, one blocking rule), then asserts:
  - `test_scores_the_blocked_pairs_and_sorts_them`: rows equal an expected list of (`unique_id_l`, `unique_id_r`, `match_probability` rounded to 6 places).
  - `test_two_runs_write_identical_bytes`.
  - `test_no_pair_above_threshold_writes_an_empty_file_with_the_schema`.
  - `test_a_path_with_a_space_and_a_quote_is_read`.
  - `test_an_untrained_model_is_refused_with_exit_2`.
  - `test_a_model_that_does_not_parse_is_refused`.
- [ ] **Step 6: Run `uv run operators/splink/test_splink_e2e.py`.** Expected: pass.
- [ ] **Step 7: Commit.** `Add the splink operator's frozen script`

### Task 2: The operator in arc's catalogue

**Files:**
- Modify: `src/operator.rs`: the `SPLINK` static and registry entry beside `SPLINK_RESOLVE`; a `"splink"` arm in the `with:` schema (`object(...)`), `configuration_fields` and `read_as`; a `Described` entry with `wraps` ids `splink` and `duckdb` and the example step from the spec; `struct SplinkConfig`, `impl Operator for Splink`, and `fn splink_invocation(cfg: &SplinkConfig, dir: &Path) -> Result<Vec<String>>` beside the `splink_resolve` code.
- Modify: `tests/operation_catalogue.rs`, which lists every operator.
- Create: `operators/splink/README.md`. One hand-written paragraph above the generated Reference: what it does, that the model is Splink's own JSON, and that cleaning and tie-breaks belong in the Protocol's SQL.
- Modify: `.github/workflows/ci.yml`: `python3 operators/splink/test_splink.py` in the `operators` job; `uv run operators/splink/test_splink_e2e.py` in the job that installs uv, after the umap step.

**Interfaces:**
- Consumes: Task 1's CLI.
- Produces: `op: splink@1`. `assets()` records each `inputs[].path` and `model` as `AssetKind::File` reads, and `out` as a file produce. `materialize_frozen_script("splink", "1.0.0", SPLINK_PY)`; the cache dir is `arcform-op-splink-1.0.0`. It runs with `OutputMode::Inherit`.

- [ ] **Step 1: Write the Rust unit tests** in the operator tests module, beside the `splink_resolve` ones:
  - `splink_declares_every_input_and_the_model_as_reads`
  - `splink_refuses_a_role_given_twice`
  - `splink_refuses_both_thresholds`
  - `splink_refuses_no_inputs`
  - `splink_resolves_by_semver` (`splink@1` ok, `splink@2` err)
  - `splink_argv_carries_each_input_and_one_threshold`
- [ ] **Step 2: Run `cargo test --lib splink_`.** Expected: fails to compile.
- [ ] **Step 3: Implement the config, operator, schema, description and registration.** Config refusals are `Error::ManifestValidation` naming the key.
- [ ] **Step 4: Run `cargo test --lib splink_`, then `ARC_UPDATE_READMES=1 cargo test --lib operator::reference`, then `cargo test --test operation_catalogue`.** Expected: pass, with the generated section written into the new README.
- [ ] **Step 5: Run `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings`.** Expected: clean.
- [ ] **Step 6: Commit.** `Add splink@1 to arc's operator catalogue`

### Task 3: The review rule, the docs and the changelog (arcform)

**Files:**
- Modify: `REVIEW.md`: the Global Constraints line, byte-identical to methods' canonical copy after Task 5.
- Modify: `docs/ARCHITECTURE.md`, section "Operator descriptions and READMEs": a paragraph stating the same rule for an author, citing `dlt` and `splink` as the shape (source specifics in the step's `with:` and the Protocol's SQL). Also the spec's outcome: what `splink@1` reads, writes and refuses, and Hugh's rulings with their reasons (generalise first; the model is the Protocol's; prediction only; Splink 5 unless parity fails).
- Modify: `CHANGELOG.md` `[Unreleased]` `### Added`: a `splink@1` entry in the file's style, a bold one-sentence summary and then the reason.
- Delete: `docs/superpowers/specs/2026-11-18-splink-operator-design.md` and this plan, once Tasks 1–5 are done.

- [ ] **Step 1: Make the edits.**
- [ ] **Step 2: Diff `REVIEW.md` against methods' branch copy from Task 5.** Expected: no difference.
- [ ] **Step 3: Commit.** `Refuse an operator written for one source in review, and document splink@1`

### Task 4: The crosswalk on splink@1 (open-analytics)

**Files** (in a worktree of open-analytics on a new branch from `origin/main`):
- Create: `datasets/edgar_gleif/models/splink_inputs.sql`. It builds tables `edgar_in` and `gleif_in` with `splink_resolve` 1.0.0's `NORM_SQL` and `blocking_select()` output columns (`uid`, `nname`, `country`, `blk_p6`, `blk_p8`, `blk_tok`, `blk_exact`), plus `edgar_src` and `gleif_src` with the columns its `edges` query joins back to. Each is copied to `build/edgar_in.parquet` and `build/gleif_in.parquet`. Translate the SQL from `operators/splink_resolve/resolve.py` at arcform `origin/main` exactly, except that EDGAR's `uid` is numbered in a fixed order (`ORDER BY cik, ticker, company_name`), since `row_number() OVER ()` is not deterministic and `uid` never reaches the published file.
- Create: `datasets/edgar_gleif/models/edgar_gleif_model.json`, today's model as Splink settings JSON:
  - `link_type: link_only`, `unique_id_column_name: uid`, `probability_two_random_records_match: 0.01`.
  - The four blocking rules as `block_on` SQL.
  - The one `nname` comparison with today's six levels and their m and u.
- Create: `datasets/edgar_gleif/models/resolved.sql`, today's `edges` query, writing `build/resolved.parquet` with the same columns and order.
- Modify: `datasets/edgar_gleif/arcform.yaml`: the `splink_resolve@1` step becomes `splink_inputs` (sql), `score_pairs` (`op: splink@1`, `threshold_match_probability: 0.30`, `retry: { max_attempts: 1 }`) and `resolved` (sql). `tier` is unchanged.
- Modify: the README step table (`python3 scripts/check_protocol_readme.py --fix` or the script's documented regenerate flag); `ARC_REV` and `Cargo.lock` to arcform's branch head after Task 2; `REVIEW.md` as in Task 5.

- [ ] **Step 1: Build the parity inputs.** Fetch `sec/company_tickers_exchange.parquet` and `gleif/lei.parquet` from openlake once into a scratch dir.
- [ ] **Step 2: Run the old step alone** with arc built from arcform's branch: `splink_resolve@1` over the Protocol's `build/sec_entities.parquet` and `build/gleif.parquet`, made from the scratch inputs. Keep `resolved.parquet` as `resolved_v1.parquet`.
- [ ] **Step 3: Make the Protocol edits and run the new steps** on the same inputs.
- [ ] **Step 4: Compare.** A DuckDB query that full-outer-joins the two files on (`cik`, `ticker`, `lei`), counting rows present on one side only, rows where any non-float column differs, and the largest `abs()` difference of `match_probability` and `match_weight`. Expected: 0, 0, and below 1e-12. If it fails only on the float bound under Splink 5, re-pin Task 1 to 4.0.17 and re-run; record which pin passed.
- [ ] **Step 5: Run the repo's own checks:** `arc validate` on the Protocol, `scripts/check_protocol_readme.py`, and the Protocol's `arc test` if it has `tests.yaml`.
- [ ] **Step 6: Commit, push and open the PR.** Its description gives the parity numbers and states that `ARC_REV` must move to arcform's merged main commit before merge.

### Task 5: The review rule in every repo

**Files:** `REVIEW.md` in methods (canonical, first), then a byte copy in every other meridian-online repo that carries one, as `tools/review-drift` in methods lists them. arcform's and open-analytics' copies ride in Tasks 3 and 4.

- [ ] **Step 1: In a methods worktree, add the Global Constraints line, run `tools/review-drift`'s own tests (`python3 tools/test_review_drift.py`), commit, push, and open the PR.**
- [ ] **Step 2: For each repo listed:** in a worktree from its `origin/main`, copy methods' new REVIEW.md over its own, check `shasum` matches methods', then commit, push and open a PR titled `Review: an operator is not one source`.
- [ ] **Step 3: Run `tools/review-drift`** against each worktree. Expected: each reports in sync with methods' branch copy.

## Verification for the arcform PR

The full gate before opening it: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` (about 15 minutes warm), `python3 operators/splink/test_splink.py`, and `uv run operators/splink/test_splink_e2e.py` (under a minute after the first dependency resolve).
