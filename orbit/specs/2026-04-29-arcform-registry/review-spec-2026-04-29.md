# Spec Review

**Date:** 2026-04-29
**Reviewer:** Context-separated agent (fresh session)
**Spec:** orbit/specs/2026-04-29-arcform-registry/spec.yaml
**Verdict:** REQUEST_CHANGES

---

## Review Depth

| Pass | Triggered by | Findings |
|------|-------------|----------|
| 1 — Structural scan | always | 5 |
| 2 — Assumption & failure | content signal (security, transport, cache trust) + MEDIUM Pass 1 findings | 6 |
| 3 — Adversarial | not triggered (no cascading structural failure surfaced) | — |

## Findings

### [HIGH] Tarball extraction has no path-traversal guard
**Category:** missing-requirement
**Pass:** 2
**Description:** AC-04 specifies the production transport falls back to `https://<repo_url>/archive/<ref_>.tar.gz` extracted via `tar`+`flate2`. Contributor entries (per the two-tier model in constraint #4) declare arbitrary `repo_url` and `repo_path`, and may serve hostile or malformed tarballs. The default `tar::Archive::unpack` accepts entries with `..` path segments and absolute paths; without explicit `set_overwrite(false)` and per-entry path validation, a malicious archive can write outside the cache directory. No constraint or AC requires this protection, and the verification block explicitly excludes the production transport from tests ("documented as sister-work surface") — meaning the hardening would not even be checked.
**Evidence:** spec.yaml AC-04 (lines 80–95) defines the fallback path. No constraint mentions sandbox/traversal. The two-tier model in constraints #4 and AC-01 admits arbitrary contributor `repo_url`s, so the trust boundary is real even if v1 ships only canonical entries.
**Recommendation:** Add a constraint and a code-AC that the tarball extractor (a) rejects entries whose normalised path escapes the destination, (b) rejects absolute paths, and (c) does not follow symlinks during extraction. Add at least one unit test (with a hand-crafted hostile fixture tarball) to lock the behaviour in. This is not "sister-work surface" — it ships in the `arc` binary the moment a contributor entry is fetched.

### [HIGH] `--latest` resolution path is undefined
**Category:** missing-requirement
**Pass:** 2
**Description:** AC-02 says `Latest` returns a sentinel `ResolvedEntry { ref_: "latest", ... }` and that "the actual rolling resolution happens at fetch time against the transport." But AC-04 defines `Transport::fetch(&self, src: &TransportSrc { ref_, ... }, dest: &Path) -> Result<()>` with no method or contract for *resolving* `"latest"` to a concrete tag/commit. AC-04's verification only exercises FixtureTransport copying from `<fixture_root>/<repo_path>/<ref_>/` — passing `ref_ = "latest"` would either look up a `"latest"/` directory or fail. There is no AC that exercises `arc registry fetch foo --latest` end-to-end, and no AC that says "the cache key for `--latest` is the *resolved* ref" (which is required by constraint #3: "Cache key is the resolved ref, never the alias").
**Evidence:** AC-02 verification (lines 64–67) tests "Latest returns the sentinel" but not what happens after. AC-04's Transport trait (lines 83–89) has no resolve method. Constraint #3 (line 27) demands cache key by resolved ref. AC-09's verification (lines 153–155) covers "cold fetch", "already-cached", and "transport failure" but not `--latest`.
**Recommendation:** Either (a) extend the `Transport` trait with `resolve(&self, src: &TransportSrc) -> Result<String>` returning a concrete ref when given `"latest"`, with a fixture impl that maps to a designated ref, and add an AC covering `arc registry fetch <name> --latest` end-to-end including cache-key verification; or (b) drop `--latest` from v1 (keep the flag rejected as unimplemented) and revisit when the production transport is exercised.

### [MEDIUM] `arc registry run` chdir is unnecessary and risky
**Category:** failure-mode
**Pass:** 2
**Description:** AC-10 says "`chdir` into the cache path and call `runner::run_with_params(&cache_path, &engine, &state, force, &cli_params)`". `run_with_params` already takes the project directory as its first argument (it does not depend on cwd — confirmed in src/runner.rs:110). The existing `cli::run_pipeline` does not chdir; it passes `cwd` directly. Mutating global process cwd inside `arc registry run` adds unneeded global state mutation, makes the command non-restartable in long-lived processes (none today, but shaping the constraint matters), and risks interacting with any future code path that resolves a relative path from cwd.
**Evidence:** spec.yaml AC-10 (line 161) prescribes chdir. src/runner.rs:110 confirms `run_with_params` takes the dir as a parameter. src/cli.rs:98–106 shows the existing `run_pipeline` passes `cwd` without chdir.
**Recommendation:** Drop the chdir from AC-10. State explicitly: "pass the cache path as the first argument to `run_with_params`; do not mutate process cwd."

### [MEDIUM] Cache write is non-atomic; partial fetches leave poisoned directories
**Category:** failure-mode
**Pass:** 2
**Description:** AC-04 has the transport copy/extract directly into the cache path. If the operation is interrupted (Ctrl-C, OOM, disk full, transport panic), a partially-populated `<ref>/` directory persists. AC-09 distinguishes "cold fetch" from "already-cached" by directory existence (implied by the on-disk byte count language and the `(cached)` line). Subsequent `arc registry fetch` or `run` would treat the partial directory as cached and fail downstream with cryptic errors at parse/run time rather than re-fetching cleanly.
**Evidence:** AC-04 (lines 80–95) has no atomicity language. AC-09 (lines 146–155) implies cached-detection is "directory exists". No constraint covers crash-resilience.
**Recommendation:** Add a constraint: "transport.fetch writes to a sibling temp directory and renames into place atomically on success; partial writes are cleaned up on failure." Add a unit test with a transport that errors mid-fetch, asserting no `<ref>/` directory remains.

### [MEDIUM] `min_arcform_version` is parsed but never enforced
**Category:** test-gap
**Pass:** 2
**Description:** AC-01 includes `min_arcform_version: Option<String>` on `IndexEntry`. No AC, constraint, or success criterion ever consults this field — not at resolve, fetch, run, list, or show time. It becomes dead data: an end user on `arc 0.1.0` can fetch and run an entry whose `min_arcform_version` says `"0.5.0"` and then crash on a missing manifest field. Either the field should be honoured (constraint plus AC) or removed from v1.
**Evidence:** spec.yaml AC-01 (line 47) defines the field. Grep over spec.yaml for `min_arcform_version` returns one hit (the definition) and the design.md (one hit, illustrative YAML). No AC mentions checking it.
**Recommendation:** Add an AC: "On `arc registry fetch`/`run`, if `min_arcform_version` is set and the running `arc` version is below it (semver compare), error before fetching with `RegistryArcformVersionMismatch { required, current }`." Or, for true v1 minimalism, drop the field from `IndexEntry` and add it back in a follow-up.

### [MEDIUM] No `--param` parsing path declared for `arc registry run`
**Category:** missing-requirement
**Pass:** 2
**Description:** AC-06 declares `params: Vec<String>` on `RegistryCmd::Run`. AC-10 says they "flow through" to `run_with_params`. But `runner::run_with_params` takes `&[(String, String)]`, not `&[String]` (src/runner.rs:115). The existing CLI uses `cli::parse_params` to convert — invalid `KEY=VALUE` formats produce `Error::ManifestValidation`. The spec doesn't say which function does the parsing for the registry path, where validation errors surface, or whether they should reuse `cli::parse_params`. Minor but it leaves a parser-shape decision implicit.
**Evidence:** spec.yaml AC-06 (lines 113–114), AC-10 (lines 158–164). src/runner.rs:115 (signature). src/cli.rs:74–95 (existing parser).
**Recommendation:** Add to AC-10: "the registry run subcommand reuses `cli::parse_params` for `--param` parsing; invalid formats error before any cache/network work."

### [LOW] Concurrent fetches race on the cache directory
**Category:** failure-mode
**Pass:** 2
**Description:** Two simultaneous `arc registry fetch foo` invocations would both detect cache miss, both invoke transport, both write into the same `<ref>/` path. No file lock or rename-on-success pattern is mentioned. Not catastrophic for v1 (single-user, single-shell typical use) but combined with finding 4 (non-atomic writes) it can produce visibly corrupt cache state.
**Evidence:** AC-04, AC-09 — no locking discussed.
**Recommendation:** Out of scope for v1 if non-atomic writes (finding 4) are fixed by tmp-dir + rename — the rename is racy but loser-takes-correct-value. Note explicitly in the spec that concurrent invocations are out of scope, or fix with the atomic-rename pattern.

### [LOW] `dirs::home_dir()` returning None is not covered
**Category:** test-gap
**Pass:** 2
**Description:** AC-03 says `cache_root()` returns `Result<PathBuf>` and reads `$ARCFORM_REGISTRY_CACHE` if set, else `dirs::home_dir().join(".arcform/registry")`. The `dirs` crate returns `Option<PathBuf>` for `home_dir()`. The verification in AC-03 covers env override and "both ownership tiers" but not the `home_dir() == None` branch (rare, but real on locked-down CI containers).
**Evidence:** AC-03 (lines 71–78). The `dirs` crate's `home_dir()` is `Option`-returning.
**Recommendation:** Add a one-line test that monkeypatches or simulates the None branch (or use a small wrapper trait), and ensure the error variant is `RegistryCacheIo` or a dedicated `RegistryCacheRootMissing`. Or add a constraint clarifying that None is unrecoverable and surfaces a clear error.

### [LOW] `index.yaml.fetched` timestamp format unspecified
**Category:** test-gap
**Pass:** 1
**Description:** AC-05 references `<cache_root>/index.yaml.fetched` and a "now - fetched > ttl" comparison but doesn't specify the on-disk format (epoch seconds? RFC3339? `SystemTime` debug?). Different choices have different forward-compat properties (epoch is timezone-stable; RFC3339 is human-readable). Cross-shell debugging benefits from a stated choice.
**Evidence:** AC-05 (lines 99–105).
**Recommendation:** Add a one-sentence note to AC-05: "fetched timestamp stored as Unix epoch seconds (decimal text), single line."

### [LOW] AC-12 verification claims a stronger guarantee than it tests
**Category:** test-gap
**Pass:** 1
**Description:** AC-12 verification says "`cargo doc --no-deps` builds without warnings on the registry module" — but this is a project-wide command; warnings elsewhere would also fail the build. More importantly, "manual doc-read confirms the four vocabulary points are present" is a human gate, not a deterministic test.
**Evidence:** AC-12 (lines 189–191).
**Recommendation:** Either accept the manual gate (make it explicit: "this AC is verified by the reviewer reading the module doc comment during PR review, not by automated tests") or add a Rust doc test that asserts the rendered doc string contains the four vocabulary anchors (`asset`, `two-tier`, `transport`, `sister work`).

### [LOW] Pillar enum extension policy unclear
**Category:** missing-requirement
**Pass:** 1
**Description:** `Pillar` enum has three variants. If the registry maintainer adds a fourth pillar (e.g. "Educational") in a future index version, every running `arc` binary serde-rejects the entire index. The spec doesn't say whether unknown pillars should be soft-skipped or hard-fail-the-load. Probably fine for v1 (only the arcform team owns the index), but worth a stance.
**Evidence:** AC-01 (line 47): `Pillar` enum, `serde rename_all = "lowercase"`. No constraint on forward compat.
**Recommendation:** Add to AC-01: "unknown pillar values fail index parsing in v1; future versions may relax to skip-unknown."

---

## Honest Assessment

The spec is well-scoped, internally consistent on the happy paths, and does a good job naming the sister-work boundary. The architecture (transport trait, fixture-backed tests, two-tier ownership, index TTL with offline grace) is sound and pragmatically testable. The biggest risk is **not** the design — it's the trust boundary the registry quietly opens up. Once contributor entries are real (architecturally supported from v1 per constraint #4), the fetch path consumes arbitrary URLs and unpacks arbitrary tarballs into the user's home directory. Two of the three HIGH/MEDIUM findings (tarball traversal, non-atomic writes) attack that surface. The third (`--latest` undefined) is an internal gap that will surface the moment someone tries the flag.

I'd request changes rather than block: the fixes are local edits to AC-04, AC-10, and one new constraint, not a redesign. Approve once the three highest findings have explicit constraints and ACs.
