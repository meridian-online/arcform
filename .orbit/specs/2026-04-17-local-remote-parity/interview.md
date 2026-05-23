# Design: Local-Remote Parity

**Date:** 2026-04-17
**Interviewer:** Nightingale
**Card:** cards/0011-local-remote-parity.yaml

---

## Context

Card: *Local-Remote Parity* — 3 scenarios, goal: version assertion mechanism
Prior specs: 0 — this is the first iteration
Gap: No version checking exists. `Engine::preflight()` runs `duckdb --version` but discards the output — only checks exit code.

References: decision 0006 (local-first, remote-compatible), decision 0003 (delegate to engine CLI).

## Q&A

### Q1: Version constraint format
**Q:** The manifest has `engine: "duckdb"` but no version field. How should version constraints be expressed — minimum version, exact pin, or semver range?
**A:** Research first. Findings: dbt, Terraform, and Cargo all converge on comparison-operator ranges. The `semver` crate (dtolnay, 661M downloads) handles Cargo-style syntax natively with `VersionReq::parse(">=1.5")` and `req.matches(&version)`. Author confirmed: `engine_version: ">=1.5"` — Cargo-style, optional field.

### Q2: Check site
**Q:** Should the version check extend `Engine::preflight()` or be a separate trait method?
**A:** Extend preflight. It already runs `duckdb --version` — parse the output instead of discarding it. Single check point, no new trait methods.

### Q3: Error UX
**Q:** What should the error message look like on version mismatch?
**A:** Rich error with both required and found versions: "engine version mismatch: requires >=1.5, found v1.3.0". Actionable — user knows exactly what to fix. No install-method-specific hints.

### Q4: Preflight return type
**Q:** `preflight()` currently returns `Result<()>`. To report version info, should it return `Result<EngineInfo>` or keep `Result<()>` with enriched errors?
**A:** Return `EngineInfo` struct. The runner can log "Using duckdb v1.5.2" on success. Richer than pass/fail. MockEngine returns fake EngineInfo.

### Q5: Version comparison responsibility
**Q:** Should the engine or the runner compare the detected version against the manifest constraint?
**A:** Runner compares. `preflight()` detects and returns the version. Runner parses the manifest's `engine_version` constraint and checks `req.matches(&info.version)`. Keeps Engine trait simple — it just reports what's installed.

### Q6: Portability scenario
**Q:** Card scenario 3 ("no local-only features") is a design principle, not a testable feature. How should we handle it?
**A:** Doc AC — record as a constraint/design principle. Verified by audit, not code test. All current features are already portable by construction.

---

## Summary

### Goal
Implement engine version assertion so the same manifest catches version mismatches before execution, whether running locally or remotely.

### Constraints
- `semver` crate (dtolnay) for version parsing and comparison
- `engine_version` field is optional — omitting skips check (backwards compatible)
- Extend existing `preflight()`, don't add new trait methods
- Engine detects, runner compares (separation of concerns)
- Parse `duckdb --version` output: `v1.5.2 (Variegata) 8a5851971f` → extract `1.5.2`

### Success Criteria
- Version mismatch caught before any step executes
- Error message shows both required and found versions
- Existing manifests without `engine_version` work unchanged
- `arc init` includes `engine_version` in scaffolded manifest

### Decisions Surfaced
- **Version constraint format**: Cargo-style semver (`engine_version: ">=1.5"`) over exact pins or custom syntax, because Rust devs know it and the `semver` crate handles it natively
- **Preflight return type**: `Result<EngineInfo>` over `Result<()>`, because the runner needs version data for comparison and success logging
- **Comparison responsibility**: Runner compares over engine compares, because it separates detection (engine's job) from policy (runner's job)
- **Portability scenario**: Doc AC over code test, because all features are already portable by construction

### Open Questions
- None — all design decisions resolved.
