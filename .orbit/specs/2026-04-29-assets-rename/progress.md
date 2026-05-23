# Progress: Assets Rename

**Spec:** orbit/specs/2026-04-29-assets-rename/spec.yaml
**Branch:** rally/assets-rename
**Started:** 2026-04-29

---

## Discovery — scope was already largely complete

The spec interview anticipated a cross-cutting rename of "asset registry" →
"assets" across `src/`, docs, and error/log strings. Discovery during spec
authoring revealed that the active code surfaces had already evolved with the
correct vocabulary:

| Surface          | Pre-spec state                                                 |
|------------------|----------------------------------------------------------------|
| `src/asset.rs`   | Module docstring uses "Asset graph construction…" — already correct |
| `src/manifest.rs`| Uses `assets:` field naming and "asset" prose — already correct      |
| `src/runner.rs`  | Asset-graph references already singular — no `asset registry` strings |
| `src/error.rs`   | No "asset registry" in error wording                            |
| `README.md`      | "asset-aware", "data assets", "asset-centric design" — already correct |
| `Cargo.toml`     | No "asset registry" tokens                                       |

`rg -i 'asset registry' src/ README.md Cargo.toml` returns **zero matches**
before any edits in this spec.

The practical scope was therefore reduced to:

1. **One label edit** — card 0008's `feature:` line ("Asset registry" → "Assets")
2. **Audit + verify** — confirm CLAUDE.md's three remaining mentions are
   quotation-form references to the renamed-from term (decisions table row,
   vocabulary note, sprint card label)
3. **Traceability assertion** — historical specs and decisions retain
   "asset registry" wording as fixed-in-time records, per decision 0011

This finding is a positive signal: the codebase had already absorbed the
asset-centric vocabulary (decision 0001) such that decision 0011's
forward-usage rename was largely complete before the spec began.

## ACs

- [x] **ac-01** — Active code surfaces contain zero forward-usage
      occurrences of "asset registry"
      Verification: `rg -i 'asset registry' src/ README.md Cargo.toml` →
      0 matches (run pre- and post-edit; both empty)

- [x] **ac-02** — Card 0008's `feature:` label updated to "Assets"; file
      slug preserved
      Verification: `orbit/cards/0008-asset-registry.yaml` line 1 now reads
      `feature: "Assets"`; file path unchanged

- [x] **ac-03** — CLAUDE.md's remaining "asset registry" mentions are all
      quotation-form references
      Verification (manual audit):
      - Line 90 — decisions table row for 0011: `| 0011 | Pipeline catalogue
        takes "registry"; rename "asset registry" → "assets" |` — quoted in a
        table cell describing the decision itself
      - Line 97 — vocabulary note: `forward usage refers to **assets** (not
        "asset registry")` — explicitly contrastive, naming the renamed-from
        term in scare quotes
      - Line 155 — Current Sprint card label: `0008: "Assets rename — 'asset
        registry' → 'asset' in forward usage (sequenced first)"` — single-quoted
        within a sprint card description naming the rename work
      All three are appropriate references to the rename itself, not naked
      forward usage.

- [x] **ac-04** — `cargo build` does not regress
      Verification: `cargo check` passes cleanly (one pre-existing dead-code
      warning unrelated to this PR). `cargo build` link step fails with
      `cannot find -lduckdb` per CLAUDE.md "Known Issues" — environmental,
      not a regression. The rename touched zero Rust source files, so build
      behaviour is necessarily unchanged.

- [x] **ac-05** — progress.md captures the scoping discovery (this file)

- [x] **ac-06** — Card 0008's `specs:` array appended with the new spec
      path
      Verification: `orbit/cards/0008-asset-registry.yaml` `specs` list now
      contains `orbit/specs/2026-04-29-assets-rename/spec.yaml`

## Files Changed

- `orbit/cards/0008-asset-registry.yaml`
  - line 1: `feature: "Asset registry"` → `feature: "Assets"`
  - `specs:` array: appended `orbit/specs/2026-04-29-assets-rename/spec.yaml`
- `orbit/specs/2026-04-29-assets-rename/spec.yaml` — new (this spec)
- `orbit/specs/2026-04-29-assets-rename/drive.yaml` — new (orchestration state)
- `orbit/specs/2026-04-29-assets-rename/review-spec-2026-04-29.md` — new (review)
- `orbit/specs/2026-04-29-assets-rename/progress.md` — new (this file)

No `src/`, `README.md`, or `Cargo.toml` modifications.

## Disposition note

The disposition guidance (drive § Disposition) says: *"Treat negative results
as constraints on the next iteration, not as conclusions."* In this case the
"negative result" was positive — the rename was largely already done in the
codebase. The honest move was to scope the spec to what actually remained
rather than invent rename work that didn't exist. Card 0022's downstream
rebase onto post-rename main is now a no-op for terminology; the rally's
sequencing rationale still holds because card 0022's docs surface will
reference asset vocabulary without any conflict.
