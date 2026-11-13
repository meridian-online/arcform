# The runner image uploads a run's outputs beside its log

Status: approved 2026-11-13. Written while the author was away; the calls below are the agent's, and the author approved them as written.

## Why

The runner image (`runner/`) uploads only `<prefix>run.log`. A Protocol's outputs stay in the container and are lost when it stops. The first hosted trial run needs a run's output and its log in the bucket under the run's folder, and the hand-dispatched publish of an operator run fetches the run's artifact from `runs/<run_id>/`. So the image has to upload outputs before either can work.

## Goals

- A run that succeeds leaves its declared outputs, its run record and its log under the prefix the credential exchange names.
- A reader can tell a complete set of outputs from a partial one without trusting timing.
- Only finished outputs leave the container. Raw downloads, scratch files and the working database stay behind, because every byte sent out of AWS is charged and counts against a subscriber's allowance.
- The whole run is still treated as hostile: nothing a Protocol does can make the entrypoint upload a file outside the Protocol's checkout, or write outside the run's prefix.

## Non-goals

- Dataset versions, the switch that makes a version current, and publishing. Those are later work; this change only puts a run's files in its own folder.
- Multipart upload. One `PUT` carries up to 5 GiB, which covers every open dataset today.
- Python, uv, finetype and the shared scripts that Meridian's open-dataset Protocols need. Every one of them uses at least one, so none runs in the image yet. That is tracked separately, and both changes are needed before a real open dataset runs hosted.

## Approaches

1. **Upload every file the run record says the run produced.** No new manifest field, but the record lists raw downloads too (GLEIF's is a ~470 MB zip), so they would be uploaded beside the dataset. That is the traffic the cost model says must not leave, so this was rejected.
2. **Upload what `datapackage.json` lists.** It fits Meridian's description standard, but the open datasets' descriptors give each resource's published URL as its `path`, not the file in the build, and a Protocol without a descriptor would upload nothing.
3. **The Protocol declares its outputs in `arcform.yaml`, arc checks and records them, and the runner uploads what the run record lists.** (Chosen.) One explicit list, checked by arc at load and after the run, with each file's size and SHA-256 in the record, so the runner reads paths from arc's own JSON rather than parsing YAML in bash.

A fourth, a fixed `outputs/` folder by convention with no arc change, was dropped: nothing checks it, and the open datasets' publish already reads `build/<name>.parquet` and `datapackage.json` where they are.

## Design

**arc.** `arcform.yaml` gains an optional top-level `outputs:`, a list of paths relative to the Protocol's folder, each a file or a directory:

```yaml
outputs:
  - build/gleif.parquet
  - datapackage.json
```

- At load, arc refuses an absolute path, one with a `..` or `.` component, a duplicate, and a path inside `build/.arcform/`.
- After the last step of a run that would otherwise succeed, arc checks each output exists. A missing one fails the run, naming the path, with the same exit code as a failed step.
- The run record (`build/.arcform/runs/<run_id>.json`) gains `outputs`: one entry per declared path in manifest order, with `path`, `kind` (`file` or `directory`), and for each file its `bytes` and `sha256` (a directory lists its files, in byte order of their paths). A record written before this reads with the list empty.
- `outputs:` changes nothing about a local run beyond that check and the record.

**The runner.** After `arc run` exits 0, a new `outputs` step, between run and the log upload:

1. Kills every process left running as `protocol`, so nothing rewrites a file between hashing and upload.
2. Reads the one run record under `build/.arcform/runs/`. No record, or more than one, fails the step.
3. For each file the record lists: resolves it with `realpath -e`, refuses it unless it is a regular file inside the checkout (so a symlink to the runner's state directory is refused), refuses it over 5 GiB, hashes it, and refuses it if the hash differs from the record's.
4. Uploads each file to `<prefix>outputs/<path>`, keeping its path, signed as the log is, with one credential copy per file.
5. Uploads the run record to `<prefix>run.json`, last. **`run.json` is the completion marker: it is uploaded only once every output has been, so a folder without it holds no finished output.**
6. Says how many files and bytes it uploaded, in the log and on standard output, so the first hosted run can measure its bytes out.

Then the log upload runs as today.

| Case | What is uploaded | Exit |
|---|---|---|
| run succeeds, outputs upload | outputs, `run.json`, `run.log` | 0 |
| run succeeds, no `outputs:` | `run.json`, `run.log` | 0 |
| a step fails, or a declared output is missing | `run.log` | 13 |
| an output is refused or its upload fails | the outputs before it, `run.log`; no `run.json` | 14 |
| stopped during the output upload | the outputs before the stop, `run.log`; no `run.json` | 15 |

A stop during the output upload abandons the rest and goes straight to the log, because a Spot interruption gives two minutes and the log matters more than a partial dataset. The exchange's reply shape does not change: outputs go under the log's prefix.

## Calls made on the author's behalf

- Outputs are declared in `arcform.yaml` (approach 3), which adds a field to arc's public manifest format.
- Outputs go under `<prefix>outputs/`, keeping their paths, and the run record goes beside the log as `run.json`.
- `run.json` marks a complete set. A failed or interrupted run never uploads it, and uploads no output after the point of failure.
- A missing declared output fails the run in arc itself, locally too.
- Over 5 GiB is refused rather than split.

## Left to the builder

The stub's S3 stand-in stores bodies as text; it has to keep bytes (or their size and SHA-256) for the output tests. Exact wording of messages, and how the runner finds `protocol`'s processes.
