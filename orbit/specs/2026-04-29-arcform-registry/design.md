# Design: Registry

**Date:** 2026-04-29
**Interviewer:** Nightingale
**Card:** orbit/cards/0022-registry.yaml

---

## Context

**Card:** *Registry* — 9 scenarios. Goal: ship an end-users-first catalogue of working ArcForm pipelines, organised across three pillars, distributed via hosted monorepo + on-demand fetch.

**Prior specs:** none (greenfield).

**Discovery:** `2026-04-29-arcform-registry/interview.md` — completed earlier this session. Locked: end-users-first, three pillars (Practical / Foundational / Investigative), monorepo + git fetch with tarball fallback, two-tier ownership (Docker Hub pattern), CLI surface (list / show / fetch / run), one canonical entry per pillar in v1.

**Decisions in place:** 0011 (naming/assets rename), 0012 (end-users-first), 0013 (hosted index), 0014 (monorepo+git), 0015 (three pillars), 0016 (two-tier ownership).

**Gap addressed by this design session:** the *what* is fully clear; this session locked phasing, CLI UX style, version resolution policy, and the FRED-secrets dependency.

**Codebase observations:**
- No HTTP, fetch, cache, or registry code exists in `src/` today. Greenfield work.
- New module `registry.rs` will hold most of the implementation.
- `cli.rs` (172 lines) extends with the `arc registry` subcommand tree.
- Doesn't entangle with `runner.rs` (2985 lines, the largest module).
- Will need new dependencies: HTTP client (reqwest or ureq), tar extraction (tar crate), and `std::process::Command` for the git shell-out.

## Q&A

### Q1: Spec slicing & phasing strategy

**Q:** How should we slice card 0022 into specs and phase the work — vertical slice (walking skeleton), horizontal layers, outside-in (entries drive CLI), or all-in-one big spec?

**A:** **All-in-one big spec for the registry work.** Reframed during design as a **rally** rather than a drive: the assets rename is forward-usage refactor on card 0008's territory (where the "asset registry" terminology was introduced), so it lives as its own spec on card 0008 — not folded into card 0022. The registry capability ships as one fat spec on card 0022 (CLI + monorepo + brewtrend + gnaf). Rally coordinates the two cards, with the rename sequenced first so the registry spec inherits clean terminology.

### Q2: CLI output style

**Q:** What CLI feel for `arc registry list/show/fetch/run` — uv-style quiet, cargo-style structured progress, docker-style layered, or hybrid?

**A:** **uv-style quiet, signal-dense.** Clean grouped table for `list`. Minimal progress for `fetch` (one line per entry, completion indicator). Terse formatted metadata + README inline for `show`. `run` defers to existing arcform progress feedback (card 0007). Errors are surgical. `--verbose` opens up the firehose (git output, HTTP detail).

### Q3: Version resolution policy

**Q:** How does `arc registry run brewtrend` (no version flag) resolve to a specific version — index-pinned latest-stable, always latest tag, pinned default with explicit override, or require explicit version?

**A:** **Index-pinned default with explicit override.** Index entry pins `current_version` (the recommended/stable ref). User can override with `--version v1.1` for explicit pin or `--latest` to opt into rolling resolution. Cache key is the *resolved* ref, not the alias, so paths stay deterministic. Same policy applies to contributor entries.

### Q4: FRED secrets dependency

**Q:** How should the FRED entry handle its API key in v1, given card 0021 (secrets management) is planned but not shipped?

**A:** **Block fred on card 0021.** v1 ships brewtrend + gnaf only. fred slips to a later spec on card 0022, gated on 0021 shipping first. The Investigative pillar exists in the index but is empty in v1.

---

## Summary

### Goal

Ship the ArcForm registry capability via a **rally** coordinating two cards:

- **Card 0022 (Registry)** — single fat spec: `arc registry` CLI subcommand tree, `meridian-online/registry` monorepo with index, two canonical entries (brewtrend in Practical, gnaf in Foundational).
- **Card 0008 (Assets)** — single spec: forward-usage rename "asset registry" → "asset" across `src/`, docs, and error messages.

Rally sequences the rename first so the registry spec inherits clean terminology. fred (Investigative) ships in a follow-up spec against card 0022 once card 0021 (secrets management) is shipped.

### Constraints

- **Rally coordinates two cards** (0008 rename + 0022 registry); each card ships one spec; rename sequenced first.
- **CLI feel:** uv-style — quiet by default, signal-dense, terse errors, `--verbose` for firehose.
- **Version resolution:** index-pinned default; `--version` and `--latest` flags as overrides; cache by resolved ref.
- **v1 scope is two entries.** brewtrend + gnaf. fred deferred to a follow-up spec.
- **Investigative pillar is visible-but-empty in v1.** UX for empty pillars (don't render? render with placeholder?) is an implementation choice for the spec.
- **No secrets dependency in v1.** Entries that need auth (fred) are out of scope until card 0021 ships.
- **All discovery + decisions 0011–0016 constraints carry forward.**

### Success Criteria

- Two specs (one per card) inside a rally, sequenced rename → registry
- Rename spec on card 0008: "asset registry" → "asset" complete in forward usage (`src/`, docs, errors)
- Registry spec on card 0022: ACs cover CLI, monorepo, brewtrend, gnaf
- `arc registry list/show/fetch/run` work against the published index
- Output style matches uv-aesthetic preview (Q2)
- Version resolution behaves per Q3 (pinned default, explicit override)
- Two canonical entries (brewtrend, gnaf) ship runnable in v1; fred deferred and tracked as a future spec on card 0022
- Smoke-test CI passes for the two v1 entries in `meridian-online/registry`

### Decisions Surfaced

1. **Rally coordinating two cards (0008 rename + 0022 registry); single spec per card** — registry work stays in one big spec on card 0022 (per Q1's all-in-one preference); the assets rename lives on card 0008 because that's the card whose terminology is changing. Trade-off: rally adds coordination overhead vs a single drive, but produces clean separation by card territory and lets the rename land first. → Process decision

2. **uv-style CLI aesthetic** — chose quiet, signal-dense output over cargo-structured, docker-layered, or hybrid. Trade-off: less informative-by-default for slow operations (GNAF fetch will appear quiet), offset by `--verbose` opt-in. → MADR candidate (small; could roll into spec)

3. **Index-pinned default version with explicit override** — chose pinned-with-override over rolling, explicit-only, or implicit-latest. Trade-off: registry maintainer overhead (bump `current_version` per release) for end-user safety. Mirrors cargo/uv. → MADR candidate

4. **fred deferred to v1.1, gated on card 0021** — chose to block fred rather than ship with env-var-only auth or substitute a no-auth Investigative entry. Trade-off: empty Investigative pillar in v1, but preserves fred as the canonical exemplar and avoids tech debt from ad-hoc secrets handling. → Process decision (alters card 0022 v1 scope)

### Implementation Notes

**Module layout (proposed):**

- `src/registry.rs` — index parsing, resolver (name + version → ref + URL), cache management, fetch orchestration
- `src/registry/fetch.rs` (sub-module) — git shell-out + tarball fallback transport
- `src/registry/index.rs` (sub-module) — `registry.yaml` schema, network fetch, parsing
- `src/cli.rs` — extend with `arc registry {list, show, fetch, run}` subcommand tree
- `src/runner.rs` — invoked unchanged after registry resolves the cache path; registry then passes the local manifest path to existing run logic

**New dependencies (proposed):**

- `reqwest` or `ureq` — HTTP client for index + tarball fallback (ureq is lighter and synchronous; reqwest is more capable but pulls async runtime)
- `tar` + `flate2` — gzipped tarball extraction for the fallback path
- `std::process::Command` — git shell-out (no new crate)
- `dirs` (already a likely transitive) — XDG-friendly cache path resolution (`~/.arcform/registry/`)

**Cache structure (proposed):**

```
~/.arcform/registry/
  index.yaml                    # cached index, refreshed by `arc registry list` or TTL
  index.yaml.fetched            # timestamp of last fetch
  brewtrend/
    v1.0/                       # cache key = resolved ref
      arcform.yaml
      *.sql
      README.md
  someone/myproject/            # contributor entries nest under owner
    v0.3/
      arcform.yaml
      ...
```

**Index schema (proposed minimum, formalised in spec):**

```yaml
version: 1
entries:
  - name: brewtrend
    owner: null                          # null = canonical
    pillar: practical
    summary: Homebrew analytics & trending packages
    repo_url: https://github.com/meridian-online/registry
    repo_path: practical/brewtrend
    current_version: v1.0
    sources:
      - https://formulae.brew.sh/api/analytics/install/...
    schedule_guidance: daily
    min_arcform_version: "0.2.0"
```

**uv-style output specifics (per Q2 preview):**

- `list` groups by uppercase pillar header, two-space indent for entries, columns aligned (name, version, summary)
- `fetch` prints one completion line: `➜ brewtrend v1.0 (12 KB)` after success, single line on failure with reason
- `show` prints metadata block + README inline, no tree drawing characters
- `run` delegates to existing arcform progress (card 0007); registry adds nothing
- Errors: single-line message + suggested next action; `--verbose` for stack traces, git output, HTTP responses

**Version resolution flow:**

1. User runs `arc registry run brewtrend`
2. arc loads cached `index.yaml` (refresh if stale beyond TTL)
3. Resolver finds entry; without `--version` or `--latest`, returns `current_version`
4. Cache lookup: `~/.arcform/registry/brewtrend/v1.0/`
5. If miss: fetch via git (sparse + shallow at ref `brewtrend/v1.0`), tarball fallback if git missing
6. Hand local manifest path to existing run logic

**Empty-pillar UX (Investigative in v1):**

- Option A: omit pillars with zero entries from `list` output (cleanest)
- Option B: render the pillar header with "(no entries yet)" subtext (signals intent)
- Decision: defer to spec authoring; recommend option B for end-user discoverability

**Asset registry rename mechanics:**

- Forward usage in `src/asset.rs`, `src/manifest.rs`, docs, error messages: `asset` (singular) where it was `asset registry`
- Existing card 0008 + spec folder names retained for traceability (per decision 0011)
- Specific tasks for the spec to enumerate as ACs

**fred deferral capture:**

- Decision 0015's pillar table updates: fred shown as "deferred to v1.1, gated on card 0021"
- Card 0022 scenarios update from "three pillars represented" to "two pillars populated in v1; Investigative pillar exists for future entries"
- New spec slot reserved on card 0022's `specs` array once card 0021 ships

### Open Questions

- Empty-pillar `list` UX — omit or render with placeholder? (suggested option B; final call in spec)
- Index TTL for `arc registry list` — fetch fresh every invocation, or cache with TTL (e.g. 1 hour) and `--refresh` to force? (lean toward cached + TTL for offline grace)
- `arc registry update` as an explicit refresh verb — yes/no for v1? (defer; not in card 0022's scenario list)
- `arc registry init` — scaffold a project from a registry entry? (defer; arc init exists separately)
- Smoke-test runner — bash script, GitHub Actions workflow, or `arc registry validate <path>` subcommand? (recommend the latter for dogfooding)
