# Design: Step Preconditions

**Date:** 2026-04-18
**Interviewer:** Nightingale
**Card:** cards/0014-step-preconditions.yaml

---

## Context

Card: *Step preconditions* — 4 scenarios, goal: Step-level freshness checks for command steps — Dagu-inspired preconditions
Prior specs: 0 — first iteration
Gap: Command steps always re-run today (runner.rs line 204). Preconditions would let them participate in the staleness model alongside SQL steps (which use content-hash staleness from card 0010).

## Q&A

### Q1: Precondition polarity and abstraction level
**Q:** The card says "exit 0 = step is fresh, skip it." This is Dagu's semantics. The alternative is the opposite polarity. Which matches your mental model?
**A:** The author pushed back on raw shell commands as the primary interface. Instead of `test $(find data/cask.json -mtime -1 | wc -l) -ne 1`, they want a more intuitive freshness interface like `fresh_for: 24h` or `modified_after: 24h`. This reframed preconditions from "shell escape hatch" to a **typed precondition system** where `modified_after` is one type and `command` (shell) is another.

### Q2: TTL clock target and escape hatch
**Q:** What should the TTL clock check against? And should there be a raw shell escape hatch alongside structured freshness?
**A:** **Output file modification time** — check mtime of a declared output file. More concrete than last-run-time: the actual artifact is fresh. The author also clarified that `command` (shell) is just another precondition type, not a separate mechanism. "TTL" naming was questioned — `modified_after` describes what's being checked more clearly.

### Q3: YAML shape and naming
**Q:** What should the top-level field be called and how should typed preconditions nest?
**A:** `preconditions` (Dagu-compatible naming). Typed entries in a list:
```yaml
preconditions:
  - modified_after:
      path: data/cask.json
      period: 24h
  - command: test $SKIP_FETCH
```

### Q4: --force and SQL step eligibility
**Q:** Should --force override preconditions? Should preconditions be available on SQL steps too?
**A:** 
- **--force overrides all** — consistent with existing behaviour. --force means "run everything, no questions asked."
- **Any step type** can have preconditions, not just command steps. SQL steps without preconditions continue to use content-hash staleness as before.

### Q5: Error handling and staleness combination
**Q:** When a command precondition itself errors (binary not found, crash) — halt or warn? For SQL steps with both hash and preconditions, how do they combine?
**A:**
- **Halt the pipeline** — strict. A broken precondition means unknown state. Better to stop than run something unexpected.
- **Both must agree (AND)** — for SQL steps with preconditions, BOTH the hash AND the precondition must say "fresh" for the step to skip. Hash catches code changes, preconditions catch data freshness. A changed SQL file always triggers re-run even if the precondition says source data is fresh.

---

## Summary

### Goal
Typed precondition system for step-level freshness checks. Two initial types: `modified_after` (file mtime) and `command` (shell). Available on any step type. Integrates with existing staleness model via AND semantics.

### Constraints
- `preconditions` field name (Dagu-compatible)
- Typed precondition entries in a list (extensible for future types)
- `modified_after` type: checks file mtime against declared period
- `command` type: runs shell command, exit 0 = fresh
- All preconditions must pass for step to be considered fresh (AND semantics)
- For SQL steps: preconditions AND content-hash must both say fresh to skip
- For command steps: preconditions are the only freshness signal (today they always re-run)
- `--force` overrides all preconditions (consistent with existing --force behaviour)
- Precondition execution errors (crash, binary not found) halt the pipeline
- Precondition non-zero exit = step is stale = run it

### Success Criteria
- Command steps with `modified_after` skip when output file is fresh
- Command steps with `command` precondition skip when shell exits 0
- Multiple preconditions evaluated with AND semantics
- SQL steps without preconditions use hash staleness unchanged
- SQL steps with preconditions use AND(hash, preconditions)
- `--force` ignores all preconditions
- Broken precondition command halts pipeline with clear error

### Decisions Surfaced
- **Typed precondition system over raw shell**: chose extensible typed model (modified_after, command, future types) over Dagu's shell-only model, because common patterns deserve first-class syntax
- **`preconditions` naming**: chose Dagu-compatible naming over `skip_if`/`fresh_if` for familiarity
- **AND semantics for SQL+precondition**: chose conservative AND over override, because hash catches code changes and preconditions catch data changes — both signals matter
- **Halt on precondition error**: chose strict over graceful degradation, because unknown state should stop the pipeline

### Open Questions
- None — design is clear enough to spec.
