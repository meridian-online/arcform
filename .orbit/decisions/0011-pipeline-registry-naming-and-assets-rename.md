---
status: accepted
date-created: 2026-04-29
date-modified: 2026-04-29
---
# 0011. Pipeline Catalogue Takes "Registry"; Existing "Asset Registry" Renames to "Assets"

## Context and Problem Statement

Two concepts in ArcForm both reach for the word "registry":

1. **Asset registry** (card 0008, spec 2026-04-15-asset-registry) — the within-pipeline catalogue of declared data assets and their dependencies.
2. **Pipeline registry** (this discovery, card 0022 successor) — the user-facing catalogue of complete, runnable pipelines fetched on demand.

Using "registry" for both concepts produces ambiguous vocabulary in docs, CLI design, and code. The collision needs resolving before the new pipeline catalogue lands.

## Considered Options

- **Keep "asset registry"; rename pipeline catalogue** (e.g. catalogue, library, gallery) — preserves existing terminology; user-facing concept gets a less natural name.
- **Rename "asset registry" → "assets"; pipeline catalogue takes "registry"** — shortens internal vocabulary; gives the natural user-facing word to the user-facing concept.
- **Both keep "registry"; scope by command prefix** (`arc registry` vs `arc assets`) — accepts ambiguity; relies on context to disambiguate in conversation and docs.

## Decision Outcome

Chosen option: "Rename 'asset registry' → 'assets'; pipeline catalogue takes 'registry'", because the asset-centric philosophy (decision 0001) already treats assets as first-class objects — the "registry" suffix was always a slightly awkward addition. "Assets" is shorter, more direct, and matches the rest of the asset vocabulary in the codebase.

Forward usage:
- `arc assets` → list assets in the current manifest, lineage info
- `arc registry` → CLI for the user-facing pipeline catalogue
- Docs and code use "assets" (not "asset registry") going forward

Historical artefacts (card 0008 slug, spec folder `2026-04-15-asset-registry/`) are kept under their original names for traceability; only forward usage changes.

### Consequences

- Good, because shorter, more direct vocabulary aligns with the asset-centric philosophy
- Good, because the user-facing "registry" namespace becomes available for the pipeline catalogue
- Good, because CLI commands separate cleanly (`arc registry` vs `arc assets`)
- Bad, because cross-cutting rename across docs, internal code, and forward references
- Bad, because historical specs and card 0008 retain "asset registry" terminology, creating a small terminology gap between past and present artefacts
