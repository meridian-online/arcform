# Design: Assets Rename

**Date:** 2026-04-29
**Interviewer:** Nightingale
**Card:** orbit/cards/0008-asset-registry.yaml
**Mode:** design (rally — derivative interview)

---

## Context

This interview is **derivative** — the design decisions for this work were
surfaced and accepted during the registry-rally design session
(`orbit/specs/2026-04-29-arcform-registry/design.md`) and captured as
**decision 0011** (Pipeline Registry Naming and Assets Rename).

This file exists to give the rally a per-card design record at the path
drive-full's §11 file-presence resumption expects
(`<spec_dir>/interview.md`), and to scope what the rename spec must deliver.

Card 0008 introduced the term "asset registry" via decision 0004 + spec
`2026-04-15-asset-registry/`. Card 0022 (Registry) needs to take the
"registry" term for the new pipeline catalogue. Decision 0011 resolved the
naming collision in card 0022's favour and assigned this card the rename
work.

## Q&A

### Q1: Scope of the rename

**Q:** Where exactly does "asset registry" → "asset" land?

**A:** **Forward usage only.** Three surfaces:
- `src/` — type names, doc comments, log/error strings, module-level docs in
  `asset.rs`, `manifest.rs`, and any other site referring to the
  capability as "asset registry"
- Project docs (`CLAUDE.md`, README sections, design-principle wording)
- Error messages and CLI output strings end users see

Out of scope:
- Existing card 0008's slug (`0008-asset-registry.yaml`) — kept for
  traceability per decision 0011
- Existing spec folder `specs/2026-04-15-asset-registry/` — kept for
  historical accuracy
- Decision 0008 file name — kept; the decision itself is unchanged

### Q2: Behavioural change

**Q:** Does the rename change any runtime behaviour, manifest schema, or
public CLI surface?

**A:** **No.** Pure terminology refactor. The `assets:` key in
`arcform.yaml` already uses the singular noun (decision 0004) — that stays.
Type names like `AssetOverride` stay (already singular). The change is
strictly to docstring + log + error wording where the phrase "asset
registry" appears as a capability label.

### Q3: Test impact

**Q:** Are there tests that assert specific error/log strings?

**A:** **Implementation note for the spec author** — grep before drafting
ACs. Any string-asserting tests touching the renamed phrasing are
in-scope; behaviour-asserting tests are not.

### Q4: Sequencing inside the rally

**Q:** Why does this card go first?

**A:** Card 0022's new registry CLI surface (`src/registry.rs`, end-user
docs, CLI help text) will reference asset-language alongside the new
pipeline-registry terminology. Letting the rename land first lets card
0022 inherit clean wording and avoids a parallel-merge churn pass where
both branches touch the same docs surface.

---

## Summary

### Goal

Forward-usage rename of "asset registry" → "asset" across `src/`, docs,
and error/CLI strings. No schema, behaviour, or public-surface change.
Frees the "registry" term for card 0022's new pipeline catalogue.

### Constraints

- Forward-usage only — historical artefact names (card 0008 slug, spec
  `2026-04-15-asset-registry/` folder, decision 0008 filename) preserved
- No manifest schema change; no CLI flag change; no public type renames
  beyond what already-singular naming dictates (none expected)
- All affected string-asserting tests updated; behaviour-asserting tests
  untouched
- Sequenced first in the registry rally; merges before card 0022's
  registry-CLI work begins

### Success Criteria

- `grep -ri "asset registry"` returns zero matches in `src/`, top-level
  docs, and error-string sources
- Manifest parsing, asset-graph building, and existing tests behave
  unchanged
- Rename PR merges cleanly to main and provides the base for card 0022's
  rally branch
- Code review confirms no behavioural drift hidden in the rename

### Decisions Surfaced

(All previously surfaced and accepted during the registry-rally design
session — captured as decision 0011.)

1. **Take "registry" for the pipeline catalogue; rename the existing
   "asset registry" to "asset"** — chose to free the registry namespace
   for the user-facing pipeline catalogue. "Asset" matches the
   asset-centric philosophy (decision 0001) and is shorter/cleaner than
   "asset registry".
   → decision 0011

2. **Forward-usage rename only — historical artefacts preserved** —
   chose to leave card 0008's slug and the existing
   `2026-04-15-asset-registry/` spec folder unchanged for traceability.
   → captured in decision 0011

### Implementation Notes

**Likely surfaces (for the spec author to enumerate as ACs):**

- `src/asset.rs` — module-level docs, struct/impl docs
- `src/manifest.rs` — references to the asset-registry concept in
  docstrings
- `src/runner.rs` — log lines mentioning "asset registry"
- `src/error.rs` and any `Display`-impl error wording
- `CLAUDE.md` — Architecture section's source-layout comment, key-types
  bullet on `AssetOverride`
- README/docs — any descriptive prose

**Suggested workflow:**

1. `rg -i "asset registry"` to enumerate the full hit list
2. Triage hits into: capability-label hits (rename) vs proper-noun hits
   in historical decisions (leave)
3. Apply renames in a single coherent commit (or per-surface commits
   if the diff is large)
4. Re-run `rg` to confirm zero forward-usage hits

**Rally context:** this card is sequenced first; on merge to main, card
0022's `rally/arcform-registry` branch rebases onto the new tip and
inherits the clean terminology.

### Open Questions

- Should we capture the rename rationale as a one-line note in the
  affected source files' module docs ("renamed from asset-registry per
  decision 0011"), or rely on git history alone? — recommend the latter
  (lean diff)
