# The runner image uploads a run's outputs: implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task (a small change, built inline, with roborev reviewing each commit). Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** a Protocol declares its outputs in `arcform.yaml`; arc checks and records them; the runner image uploads them, then the run record, then the log.

**Architecture:** arc owns the declaration (manifest field, load-time refusals, post-run check, `outputs` in the run record). The runner's entrypoint gains one `outputs` step between `run` and the log upload, which reads arc's run record, never `arcform.yaml`.

**Tech Stack:** Rust (arc), bash + curl `--aws-sigv4` + jq (runner), Python stub and bash tests under `runner/tests/`.

**Spec:** `docs/superpowers/specs/2026-11-13-runner-uploads-outputs-design.md`

## Global Constraints

- `outputs:` is optional, top-level, a list of paths relative to the Protocol's folder; absolute paths, `..` or `.` components, duplicates and paths under `build/.arcform/` are refused at load.
- A missing declared output fails the run with the exit code a failed step gets, naming the path.
- Run record field `outputs`, added under the existing `b4/1` tag with `#[serde(default)]`: entries `{path, kind: "file"|"directory", files: [{path, bytes, sha256}]}` in manifest order; a directory's files sorted by path bytes; a file output has one entry in `files`, its own.
- Object keys: `<prefix>outputs/<path>` for each file, `<prefix>run.json` for the record, `<prefix>run.log` as today. `run.json` is uploaded after the last output and only if every output uploaded.
- Per-file limit 5 GiB (5,368,709,120 bytes); larger is refused, not split.
- Runner exit codes unchanged: an output refused or failed to upload is 14; a stop during the output upload is 15 and skips the remaining outputs and `run.json`, then uploads the log.
- Nothing in `runner/` reads `arcform.yaml`. Every file read for upload resolves (`realpath -e`) to a regular file inside the checkout.

## Review Focus

1. **A step leaves a background process that rewrites an output after arc hashed it.** The runner kills `protocol`'s processes first and refuses a hash mismatch. Test: `refuses_an_output_changed_after_the_run` (Task 4).
2. **A declared output is a symlink to the runner's credential file.** Refused, and no uploaded object holds any credential part. Test: `refuses_an_output_outside_the_checkout` (Task 4).
3. **An output path with spaces or unicode.** Uploaded under the same key, URL-encoded once. Test: `uploads_an_output_with_a_space_in_its_name` (Task 4).
4. **A directory output with a nested folder.** Every file uploaded with its relative path, in the record's order. Test: `records_a_directory_output` (Task 2) and `runs_pinned_protocol_with_outputs` (Task 4).
5. **Leftover run records in a committed `build/.arcform/runs/`.** More than one record fails the step rather than uploading the wrong run's list. Test: `refuses_more_than_one_run_record` (Task 4).

---

### Task 1: `outputs:` in the manifest

**Files:**
- Modify: `src/manifest.rs` (field on `Manifest`, checks in `Manifest::validate`)
- Test: the `#[cfg(test)]` module in `src/manifest.rs`

**Interfaces:**
- Produces: `Manifest.outputs: Vec<String>` (`#[serde(default)]`), validated by `validate()`, errors as `Error::ManifestValidation(String)`.

- [ ] **Step 1: Write the failing tests** `outputs_parse`, `outputs_refuse_absolute`, `outputs_refuse_parent_and_dot`, `outputs_refuse_duplicates`, `outputs_refuse_run_state`. Each loads a manifest string; the refusals assert `ManifestValidation` whose message names the offending path; `outputs_parse` asserts `["build/out.parquet", "datapackage.json"]` in order and that a manifest without the key gives `[]`.
- [ ] **Step 2:** `cargo test --lib manifest::tests::outputs` fails to compile (no field).
- [ ] **Step 3:** add the field and the checks, using `Path::components()`; a trailing `/` is normalised away before the duplicate check.
- [ ] **Step 4:** the same command passes; `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` clean.
- [ ] **Step 5:** commit "Let a Protocol declare its outputs in arcform.yaml".

### Task 2: arc checks the outputs after a run and records them

**Files:**
- Modify: `src/contract.rs` (`OutputEntry`, `OutputFile`, `Contract.outputs`, filled in `build_contract` via `ContractInputs`)
- Modify: `src/runner.rs` (after the step loop's `Ok(())`, before hooks' `on_success`: check each output exists)
- Modify: `src/error.rs` (`Error::OutputMissing { path: String }`, in the `exit_code` arm with `StepFailed`)
- Modify: `docs/ARCHITECTURE.md` (manifest section: `outputs:`; run record section: `outputs`), `CHANGELOG.md` `[Unreleased]`
- Test: `tests/run_outputs.rs` (spawns the real `arc`, as `tests/directory_produce_reported_missing.rs` does; needs a DuckDB 2 CLI on PATH)

**Interfaces:**
- Consumes: `Manifest.outputs`.
- Produces: in `<run_id>.json`, `"outputs": [{"path": "...", "kind": "file"|"directory", "files": [{"path": "...", "bytes": N, "sha256": "<hex>"}]}]`. The runner (Task 4) reads exactly these names.

- [ ] **Step 1: Write the failing tests:**
  - `records_file_outputs`: a SQL step `COPY (SELECT 1 AS n) TO 'build/out.parquet'` and `outputs: [build/out.parquet]`; exit 0; the record's `outputs[0]` has `kind == "file"`, one `files` entry whose `bytes` equals the file's length and `sha256` its digest.
  - `records_a_directory_output`: a `PARTITION_BY` COPY to `build/parts` plus a hand-made `build/parts/sub/extra.txt` via a command step; `files` lists every file with paths relative to the Protocol, sorted.
  - `missing_output_fails_the_run`: `outputs: [build/never.parquet]`; exit code equals a failed step's (1); stderr names `build/never.parquet`; the record's `run.outcome` is not `success`; `on_success` does not run, `on_failure` does.
  - `no_outputs_records_empty_list`: no key, record has `"outputs": []`.
  - `old_record_reads`: a fixture record without `outputs` loads through `load_contract` with the list empty.
- [ ] **Step 2:** `cargo test --test run_outputs` fails.
- [ ] **Step 3:** implement; hash with the `sha2` already used by `state.rs`; walk directories without following symlinks, recording a symlink inside a directory output as an error (`OutputMissing` naming it), since the runner will refuse it anyway.
- [ ] **Step 4:** `cargo test --test run_outputs` and `cargo test --lib contract` pass; fmt and clippy clean; `cargo test --test public_surface` still passes (update its pinned surface if it lists record fields).
- [ ] **Step 5:** commit "Check a run's declared outputs and record their sizes and hashes".

### Task 3: the stub keeps uploaded bytes

**Files:**
- Modify: `runner/tests/stub/stub.py` (`do_PUT` stores, per key, `{"bytes": len, "sha256": hex, "text": body if it decodes as UTF-8 else null}`; `--state` prints it)
- Modify: `runner/tests/run.sh` (the existing tests read `.text` where they read the body today)

**Interfaces:**
- Produces: `state | jq '.objects["<key>"].sha256'` and `.text`, used by Task 4's tests.

- [ ] **Step 1:** change the stub and the existing tests' reads together.
- [ ] **Step 2:** `docker build --platform linux/arm64 -f runner/Dockerfile -t arc-runner .` then `runner/tests/run.sh`: 11 passed, 0 failed (the image is unchanged, so this proves the stub change alone). About 90 s plus the build.
- [ ] **Step 3:** commit "Keep the bytes the runner tests' storage stub receives".

### Task 4: the runner's `outputs` step

**Files:**
- Modify: `runner/entrypoint.sh` (new `upload_file KEY PATH`, factored out of `upload_log`, which then calls it; new `outputs` section between `run` and `finish 0 run`; a flag so `on_signal` during the output upload abandons it and goes to `finish 15`)
- Create: `runner/tests/protocols/with-outputs/1/{arcform.yaml,models/out.sql}`, `runner/tests/protocols/output-symlink/1/arcform.yaml`, `runner/tests/protocols/output-rewritten/1/arcform.yaml`, `runner/tests/protocols/output-space/1/arcform.yaml`, `runner/tests/protocols/two-records/1/...` (a committed second record under `build/.arcform/runs/`), `runner/tests/protocols/slow-upload/1/arcform.yaml` (a large output, with the stub told to delay PUTs under `outputs/`)
- Modify: `runner/tests/stub/stub.py` (`STUB_PUT_DELAY` seconds for keys containing `/outputs/`)
- Modify: `runner/tests/run.sh`, `docs/ARCHITECTURE.md` ("The runner image": step list, exit table, the outputs table from the spec, the new tests), `CHANGELOG.md`

**Interfaces:**
- Consumes: the record's `outputs[].files[].{path,bytes,sha256}` (Task 2); the stub's `objects` shape (Task 3).
- Produces: objects `<prefix>outputs/<path>`, `<prefix>run.json`, `<prefix>run.log`; a log line `arc-runner: wrote <n> outputs, <bytes> bytes`.

- [ ] **Step 1: Write the failing tests** in `run.sh`:
  - `runs_pinned_protocol_with_outputs`: exit 0; `sub_test/runs/<id>/outputs/build/out.parquet` and every file of a directory output are present with the record's sha256; `run.json` and `run.log` present; the log has `wrote 2 outputs`.
  - `uploads_no_outputs_when_a_step_fails`: the existing `fails-second` Protocol given `outputs:`; exit 13; only `run.log` under the prefix.
  - `refuses_an_output_outside_the_checkout`: a command step `ln -s /run/arc-runner/credential.json build/cred` with `outputs: [build/cred]`; exit 14; no `run.json`; no object's `.text` contains any issued credential part (reuse the leak check from `token_and_credential_kept_out_of_step_env`).
  - `refuses_an_output_changed_after_the_run`: a step starts `nohup sh -c 'sleep 1; echo x >> build/out.txt' &`; either the kill stops it (exit 0, uploaded sha256 equals the record's) or the hash check refuses (exit 14). Assert the uploaded bytes never differ from the record's sha256.
  - `uploads_an_output_with_a_space_in_its_name`: `build/my out.csv` lands at key `.../outputs/build/my out.csv`.
  - `refuses_more_than_one_run_record`: exit 14, the log names both records.
  - `stopped_during_output_upload`: `STUB_PUT_DELAY=5`, `docker stop` two seconds after the log says the outputs step began; exit 15; `run.log` present; `run.json` absent.
- [ ] **Step 2:** rebuild the image (needs Task 2's arc; one cargo build at a time on the Mac) and run `runner/tests/run.sh`: the new tests fail, the old 11 pass.
- [ ] **Step 3:** implement. Kill leftovers with `pkill -KILL -u protocol` and wait until `pgrep -u protocol` is empty (bounded, 5 s). Find the record with a glob under `$WORK/build/.arcform/runs/*.json` read as root after the kill. Percent-encode each key segment once for the URL (jq `@uri` per segment). Check `realpath -e` is under `$WORK/` and `[[ -f ]]` and not `-L` on the given path; size with `stat -c %s` against 5,368,709,120.
- [ ] **Step 4:** `shellcheck runner/entrypoint.sh runner/tests/*.sh runner/tests/stub/*.sh` clean; `runner/tests/run.sh`: 18 passed, 0 failed (several minutes; the stalled-fetch test alone takes one).
- [ ] **Step 5:** commit "Upload a run's declared outputs and its record beside the log".

## Verification and cost

| Command | Proves | Cost |
|---|---|---|
| `cargo test --lib manifest::tests::outputs` | Task 1 refusals | seconds after a build |
| `cargo test --test run_outputs` | Task 2, against a real DuckDB 2 CLI | one incremental cargo build, about a minute |
| `runner/tests/run.sh` | Tasks 3–4, in `linux/arm64` containers | an image build (minutes, it compiles arc) plus 2–4 minutes of tests |
| CI on the pull request | the full gate, including `runner-image` | no local cost |

Tasks run in order: 2 needs 1, 4 needs 2 and 3. Task 3 is independent of 1 and 2 and can go first.
