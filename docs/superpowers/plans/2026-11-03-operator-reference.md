# Operator reference Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Each catalogue operator's description and `operators/<name>/README.md` state what it does, what it wraps, its parameters with defaults and an example step, from one source.

**Architecture:** `Described` in `src/operator.rs` gains `wraps: &[Wraps]` and `example: &str`; `description()` emits them. A new `src/operator/reference.rs` renders an operator's Markdown Reference section from `description()`, and a unit test compares it with the marked section of each README, rewriting it under `ARC_UPDATE_READMES=1`.

**Tech Stack:** Rust, serde_json, serde_yaml (already a dependency for manifests).

**Spec:** `docs/superpowers/specs/2026-11-03-operator-reference-design.md`

## Global Constraints

- Operation descriptions keep exactly five keys; operator descriptions hold those five plus `wraps` and `example`.
- `wraps` entries: `id` (stable kebab-case), `name`, `url`, `use` (one sentence). Empty list when arc does the work itself.
- Markers: `<!-- arc:reference:start -->` and `<!-- arc:reference:end -->`. Regenerate with `ARC_UPDATE_READMES=1`.
- READMEs name vendors as links, no logos. Public repo: no Kata refs or private repo names in tracked text.
- One cargo build at a time.

## Review Focus

- An example whose `with:` passes the schema but fails the operator's typed parse (`assets()`): the example test calls `assets_for`, not only `check_with`.
- `opendal_fetch` is only in an `opendal` build: the README test must cover it from `DESCRIBED` without the feature, so render from `described()` + `with_schema()` directly rather than through `catalog()`.
- A README with the start marker but no end marker: the test fails naming the file rather than rewriting the rest of it.
- A schema default in a nested object (`credential`) or array: the parameters table lists top-level `with:` keys only.
- Hand prose outside the markers survives regeneration byte-for-byte.

---

### Task 1: `wraps` and `example` in the catalogue

**Files:** Modify `src/operator.rs` (`Described`, `DESCRIBED`, `description()`), `tests/operation_catalogue.rs`.

**Interfaces — Produces:** `struct Wraps { id: &'static str, name: &'static str, url: &'static str, use_: &'static str }` (emitted as `use`); `Described.wraps: &'static [Wraps]`, `Described.example: &'static str` (a YAML list holding one step, `- name: … op: <name>@1 with: …`).

- [ ] Test (`tests/operation_catalogue.rs`): `an_operator_description_names_what_it_wraps_and_an_example_step` — for each operator, keys are the five plus `example` and `wraps`; `wraps` is an array whose entries hold non-empty `id`, `name`, `url` (starting `https://`), `use`; `splink_resolve`'s ids include `splink` and `duckdb`. Existing tests asserting `DESCRIPTION_KEYS` for operators switch to `OPERATOR_DESCRIPTION_KEYS` (seven keys); operations keep five.
- [ ] Unit test in `src/operator.rs`: `every_example_step_loads` — for each name in `DESCRIBED`, parse `example` with serde_yaml into a list of one mapping, `op` resolves to this operator, `check_with(name, with)` is empty, and (for names in `catalog_names()`) `assets_for(op, Some(with))` is `Ok`.
- [ ] Fill both fields for all 13 operators from what each operator's code calls. Check each `with_schema` property against its config's serde default and add any missing `default`.
- [ ] `cargo test --lib operator:: && cargo test --test operation_catalogue` pass; commit.

### Task 2: Generated Reference sections and the 13 READMEs

**Files:** Create `src/operator/reference.rs` (declared from `src/operator.rs`), `operators/<name>/README.md` for the ten without one; modify the three existing READMEs.

**Interfaces — Consumes:** `described()`, `with_schema()`, `description()` shape from Task 1. **Produces:** `pub(crate) fn reference_markdown(op_name: &str) -> Option<String>`.

- [ ] Unit tests: `reference_lists_every_parameter_with_its_default` (umap_project's section holds a row for `neighbors` with `15`, and for `metric` with `euclidean`); `every_operator_readme_holds_its_generated_reference` (for each `DESCRIBED` name, reads `operators/<name>/README.md` from `CARGO_MANIFEST_DIR`, extracts between markers, equals `reference_markdown`; under `ARC_UPDATE_READMES=1` writes it instead; fails naming the file on a missing marker); `hand_prose_outside_the_markers_is_kept` (splice into a fixture string, text before and after unchanged).
- [ ] Section layout: `## Reference`, summary, `**Wraps:**` linked names each with its use (or "arc does this itself"), `**Reads** / **Writes**` role lists, a parameters table `| Key | Type | Required | Default | Description |`, and the example in a ```yaml block.
- [ ] Write each new README: title, one-paragraph intro in plain words, markers. Insert markers into the three existing READMEs after their opening section. Run with `ARC_UPDATE_READMES=1`, then without; both pass. Commit.

### Task 3: Docs and changelog

- [ ] `CHANGELOG.md` [Unreleased]: operator descriptions gain `wraps` and `example`; every operator has a README. `docs/ARCHITECTURE.md`: the reference section and its test, in a few lines, with Hugh's rulings (README for every operator; vendor ids, no logos, web shows the mark beside the shape icon). README.md's `operator_describe` row unchanged. Delete this plan and the spec. Commit.
