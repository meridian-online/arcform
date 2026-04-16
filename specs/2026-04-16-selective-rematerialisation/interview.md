# Design: Run State Tracking

**Date:** 2026-04-16
**Interviewer:** Nightingale
**Card:** cards/0010-selective-rematerialisation.yaml

---

## Context

Card: *Run state tracking* (reframed from "Selective re-materialisation") — 5 scenarios, goal: "v0.3 — configurable state backend with content-hash staleness and selective execution"
Prior specs: none — first drive on this card
Gap: `arc run` currently runs ALL steps every time. No staleness awareness, no run history.

## Q&A

### Q1: Staleness mechanism
**Q:** How should arc detect that a step is stale and needs re-running?
**A:** DuckDB metadata table — but the mechanism must be configurable. Run tracking is foundational and needs to work both locally and remotely (card 0011 awareness). Ducklake can complement this later. The state backend should be a trait.

### Q2: Architecture — configurable vs locked
**Q:** Should we build the staleness mechanism as a DuckDB metadata table that Ducklake can later complement, or design around Ducklake from the start?
**A:** Configurable. The StateBackend trait is the interface; DuckDB is the first implementation. Ducklake and filesystem backends are future cards. "I know this is difficult, but we need to make it configurable."

### Q3: State storage model
**Q:** What should the DuckDB state table store per step?
**A:** Two-table model: `_arcform_state` (step_name, sql_hash, last_run_at, status) for staleness, plus `_arcform_runs` (run_id, started_at, finished_at, steps_run, outcome) for history.

### Q4: Command step staleness — preconditions
**Q:** How should command steps determine staleness?
**A:** Generalise using Dagu's concept of "preconditions" — shell commands that check freshness (e.g., file mtime checks). Reference: brewtrend.yml uses `preconditions: test $(find data/cask.json -mtime -1 | wc -l) -ne 1`. But this is a significant manifest schema change — split into a separate card.

### Q5: Command steps without preconditions
**Q:** Until preconditions ship, what should command steps do?
**A:** Always re-run. They're opaque without preconditions. Safe default.

---

## Summary

### Goal
Configurable state backend with SQL content-hash staleness detection and selective execution. Command steps always re-run (preconditions deferred to a separate card).

### Constraints
- StateBackend trait is the interface — DuckDB is the first implementation
- SQL staleness is based on SHA-256 content hash of the SQL file
- Downstream propagation: stale upstream → stale downstream (via asset graph)
- Failed steps are always stale (re-run on next invocation)
- Command steps always re-run (no staleness signal without preconditions)
- `--force` flag overrides all staleness (full rebuild)
- Two DuckDB metadata tables: `_arcform_state` and `_arcform_runs`
- State backend selected by config (future: manifest-level or env var)

### Success Criteria
- `arc run` skips fresh SQL steps and shows skip/run status
- Editing a SQL file makes the step and its dependents stale
- `arc run --force` runs everything
- State persists across runs via DuckDB
- Failed steps always re-run

### Decisions Surfaced
- **Configurable state backend**: trait-based, DuckDB first — enables Ducklake (card 0011) and filesystem backends later
- **Two-table model**: state for staleness, runs for history — richer than minimal
- **Preconditions as separate card**: general staleness mechanism (Dagu-inspired) is too significant for this drive
- **Command steps always re-run**: safe default without preconditions

### Open Questions
- Preconditions card needs to be created (manifest schema change for command step staleness)
- Exact config mechanism for selecting state backend (manifest field? env var?) — defer to implementation
