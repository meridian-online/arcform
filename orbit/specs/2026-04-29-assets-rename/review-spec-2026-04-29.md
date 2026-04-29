# Spec Review

**Date:** 2026-04-29
**Reviewer:** Context-separated agent (fresh session)
**Spec:** orbit/specs/2026-04-29-assets-rename/spec.yaml
**Verdict:** APPROVE

---

## Review Depth

| Pass | Triggered by | Findings |
|------|-------------|----------|
| 1 — Structural scan | always | 2 (LOW) |
| 2 — Assumption & failure | not triggered (no >=MEDIUM findings; no content-signal hits — no auth/data/infra/migration surface) | — |
| 3 — Adversarial | not triggered | — |

Pass 1 produced two LOW-severity observations only. No MEDIUM/HIGH findings, and the spec touches none of the deepening triggers (training data, deployment, cross-system boundaries, security, migrations). Per the protocol's "zero findings ≥ MEDIUM AND no content signals → APPROVE" rule, deeper passes are not warranted.

## Findings

### [LOW] ac-06 may already be satisfied — verb "appended" is misleading
**Category:** test-gap
**Pass:** 1
**Description:** ac-06 says card 0008's `specs:` array should be **appended** with the new spec path, but `orbit/cards/0008-asset-registry.yaml` line 29 already contains `"orbit/specs/2026-04-29-assets-rename/spec.yaml"`. The criterion is therefore already met at spec-authoring time. This is harmless — verification still passes — but the imperative phrasing ("appended") implies pending work that is not actually pending. Consider rewording to "Card 0008's `specs:` array contains 'orbit/specs/2026-04-29-assets-rename/spec.yaml'" so the AC reads as a state assertion rather than an action item, and the implementer doesn't waste a cycle wondering whether they missed the edit.
**Evidence:** `orbit/cards/0008-asset-registry.yaml:29` contains the path; spec.yaml line 47 says "appended with"; spec verification on line 48 reads as a state check ("`specs` list contains the new spec path"), so the verification is fine — only the description verb is misleading.
**Recommendation:** Reword ac-06 description from "appended with" to "contains" (or annotate "already satisfied at spec authoring; verify on review"). No blocking impact.

### [LOW] ac-03 line-number citations are advisory but stated as fact
**Category:** test-gap
**Pass:** 1
**Description:** ac-03 cites three exact line numbers in CLAUDE.md (~90, ~97, ~155) for the surviving "asset registry" mentions. I confirmed those are accurate today (decisions table row, vocabulary note, current-sprint card label). The risk is small but real: if any earlier rally edit retitles a section or inserts a row before one of these lines, the verifier will need to re-locate the hits. The "~" softens this slightly, but the implement step's auditor may still flag a mismatch.
**Evidence:** `CLAUDE.md:90`, `CLAUDE.md:97`, `CLAUDE.md:155` — all match the spec's claim verbatim today. Verification text on line 33 ("Manual audit of each CLAUDE.md hit confirms it is wrapped in quotes...") is robust to drift because it audits all hits, not specific lines. So the canonical verification is already line-number-agnostic; only the description text could mislead.
**Recommendation:** Optional — replace "decisions-table row at ~line 90, vocabulary note at ~line 97, sprint-cards entry at ~line 155" with "decisions-table row, vocabulary note, sprint-cards entry" (drop the line numbers; the categories already locate them). Non-blocking.

---

## Pass 1 — Detailed Checks

**1. AC testability.** All six ACs have concrete verification steps (rg invocation, file-line read, file existence, audit checklist). ac-04 explicitly accepts a skip with documented reason (libduckdb.so missing per CLAUDE.md known issues) — that is a deliberate, traceable concession, not an untestable AC.

**2. Constraint conflicts.** The four constraints are internally consistent: forward-usage-only + no schema/runtime/type rename + card-feature-label-only + rally-sequenced-first. No contradictions detected.

**3. Scope vs goal.** The goal honestly describes a "verification + label tweak" rather than a refactor; the AC count (6, half of which are documentation/audit ACs) matches that scope. Not over-specified, not under-specified.

**4. Obvious gaps.**
- Error handling: not applicable — no behaviour change.
- Rollback: trivial (single-line YAML edit + progress.md creation; `git revert` suffices). Spec's lean diff makes this implicit.
- Monitoring: not applicable.
- Edge cases: ac-01's `rg` glob covers `src/`, `README.md`, `Cargo.toml`. The interview also mentions "error/CLI strings end users see" (Q1) and `src/error.rs` (implementation notes). Verified: a project-wide `rg -i 'asset registry'` already returns zero hits in any `.rs` file. So there is no live string anywhere in the codebase; ac-01's narrower glob is therefore not a coverage gap (it is the canonical surface set), but a future reader might ask "why not the full repo?" — answered by the discovery note in implementation_notes line 51.

**5. Gate-AC verification check (deterministic).**
- ac-04 is the only `ac_type: gate` in the spec. Its `verification` field:
  - Non-empty: ✅
  - Not a placeholder token (TBD/TODO/FIXME/PLACEHOLDER/XXX/???): ✅ (literal trimmed value is the full sentence beginning "cargo build (where linkable)…")
  - Minimum length ≥20 chars: ✅ (well over 20)
- Gate AC passes all three deterministic rules. No finding raised.

**6. Content signal scan.** None of the deepening triggers fire:
- No training data / model / eval surface (pure terminology refactor)
- No deployment / infrastructure / cron / production service touched
- No cross-system boundary (rally context noted, but the rebase is mechanical and the spec calls it out)
- No security / auth / permissions / key management
- No data migration / schema change / backwards compatibility surface (constraint line 15 explicitly forbids these)

Therefore the trigger condition for Pass 2 ("any finding ≥ MEDIUM OR content signals present") is not met.

---

## Honest Assessment

This plan is ready. The spec is small, structurally clean, and self-aware about its scope: the discovery note in implementation_notes line 51 is an unusually candid statement that the rename was effectively complete before the spec began, and the ACs honestly reflect that — most are audit/label-tweak/discovery-capture ACs rather than refactor ACs. The biggest residual risk is purely cosmetic: ac-06's "appended with" phrasing implies pending work that is already done, which could cause an implementer a few seconds of confusion. There is no behaviour-change risk, no rollback risk, and no rally-coupling risk beyond what constraint line 17 already captures (card 0022 rebases onto post-rename main). Approve as-is, or accept the trivial reword in the LOW finding above before kicking off implement.
