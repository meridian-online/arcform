# Discovery: arcform registry

**Date:** 2026-04-29
**Interviewer:** Nightingale
**Card:** orbit/cards/0022-reference-pipelines.yaml (existing — to be redesigned as "Registry")
**Mode:** discovery

---

## Context

Card 0022 originally framed the work as "reference pipelines" — examples shipped in the repo for learning and integration testing. Hugh proposed an evolution:

- Shift language from **reference** to **registry** — implies a curated, growing, indexed catalogue, not static documentation.
- Organise around **three pillars** that demonstrate distinct reasons to use arcform locally and schedule in the cloud.
- Pillar names settled as **Practical / Foundational / Investigative** (HubSpot-style plain-English content pillars; parallel adjective form).
- Bellingcat treated as inspiration for the Investigative pillar's analytical character, not as a registry entry itself.

Discovery purpose: clarify the registry concept's CLI surface, distribution model, growth governance, and v1 scope before drafting card 0022's replacement spec.

## Prior art surfaced

- **Naming collision risk:** the term "registry" was already in use as "asset registry" (card 0008, decision 0008-adjacent, spec 2026-04-15-asset-registry). Resolved during Q4.
- **Adjacent cards:** 0020 (pipeline scheduling) carries the cloud-schedule narrative; 0008 (asset registry — to rename) is the within-pipeline catalogue concept.
- **Reference inputs available:** `/home/hugh/reference/brewtrend/` (largely-built Practical exemplar), `/home/hugh/reference/gnaf/` (raw inputs for Foundational exemplar).

## Q&A

### Q1: Primary user

**Q:** Who is the arcform registry primarily for?

**A:** End users first — analysts and operators who run these pipelines for actual value. Optimised for working out-of-the-box. Learning and contribution use are side-effects, not primary goals.

### Q2: Distribution model

**Q:** Where do registry entries live, and how do users get them?

**A:** Hosted index + on-demand fetch. Registry entries live remotely; `arc registry` commands hit a published index and fetch entries just-in-time.

### Q3: CLI surface

**Q:** What's the minimum CLI surface for `arc registry`?

**A:** Full surface — `list`, `show <name>`, `fetch <name>`, `run <name>`. List/show return metadata from the index; fetch/run pull the entry to local cache.

### Q4: Naming — registry vs alternatives

**Q:** Given the existing "asset registry" concept, what do we call the new pipeline catalogue?

**A:** Keep "registry" for the new pipeline catalogue, and rename the existing "asset registry" → just **assets**. "Assets" is shorter, more direct, and matches the asset-centric philosophy (decision 0001) where assets are first-class.

### Q5: MVP scope

**Q:** What's the v1 registry scope — how many pillars and entries?

**A:** One per pillar — three entries originally proposed: **brewtrend** (Practical), **gnaf** (Foundational), **fred** (Investigative). *Refined during design (2026-04-29):* fred deferred to v1.1 because it needs API-key handling, and card 0021 (secrets management) ships first. v1 ships **two canonical entries** — brewtrend + gnaf. Investigative pillar exists in the index but is empty in v1.

### Q6: Bundle format

**Q:** What's the unit of distribution — what does an `arc registry fetch` actually pull?

**A:** Sub-discussion on whether a complete pipeline could collapse to a single YAML file (with inlined SQL) for atomic distribution. Trade-off explored: text-level diff is preserved, but SQL-aware tooling and code-review UX degrade for large inline blocks. Hugh chose **YAML + .sql files for both author and ship** — preserves first-class SQL files and full diff fidelity.

### Q6b: Distribution mechanism

**Q:** Given the YAML + .sql file structure, should we distribute as tarballs, or just require git on the user's machine?

**A:** **Monorepo + git fetch with tarball fallback.** A single `meridian-online/registry` repo with subdirs per entry (`practical/brewtrend/`, `foundational/gnaf/`, `investigative/fred/`). `arc registry fetch` shells out to git for sparse/shallow checkout; falls back to GitHub's tarball archive endpoint if git is missing. Versioning via per-entry git tags (e.g. `brewtrend/v1.0`).

### Q7: Test-fixture role

**Q:** What's the test-fixture role of registry entries?

**A:** **Smoke-test only.** CI runs lightweight checks (parse manifest, dry-run via sqlparser-rs, validate dependency graph, check URLs well-formed) without fetching real data. End-user pipelines can use full real data without CI constraints.

### Q8: Card structure

**Q:** How do we split this work — one card with multiple specs, or multiple cards?

**A:** **One card, multiple specs.** Card 0022 redesigned as "Registry" (the capability). Specs spawn from it: registry CLI, monorepo + entries, assets rename. Coordinated via `/orb:rally`. Aligns with the orbit vocabulary (cards = capabilities, specs = work units against cards).

### Q9: Two-tier ownership model (post-interview addition)

**Q:** Should the registry support contributor-maintained entries alongside registry-maintained ones, modelled on Docker Hub?

**A:** **Yes — Docker Hub-style two-tier model from v1.** Two ownership tiers:

- **Registry-maintained (canonical):** unprefixed names. Example: `arc registry run brewtrend`. Curated by the arcform team; lives in `meridian-online/registry`.
- **Contributor-maintained:** namespaced as `<owner>/<name>`. Example: `arc registry run duckdb/tpc-h-loader`. Lives in any GitHub repo; the owner controls publishing and versioning.

Mirrors `docker pull python:3.11` (official) vs `docker pull duckdb/duckdb` (contributor). v1 ships only canonical entries (brewtrend, gnaf, fred), but the index format, naming, and fetch protocol must support both tiers from day one to avoid future migration pain.

---

## Summary

### Goal

Ship an end-users-first registry of working arcform pipelines — a curated, growing catalogue organised across three pillars (Practical / Foundational / Investigative) — distributed via a hosted git monorepo and accessed through `arc registry list/show/fetch/run`. v1 ships one canonical exemplar per pillar that demonstrates the local-then-schedule narrative.

### Constraints

- Registry entries must work for real end users producing real outputs (not just learners or fixtures)
- Each entry must be authored as YAML + sidecar SQL files (no inlined-SQL compile step)
- Distribution preferences: git on the user's machine; GitHub tarball archive fallback when git is unavailable
- v1 ships **two canonical entries** — brewtrend (Practical) + gnaf (Foundational); fred (Investigative) deferred to v1.1 pending card 0021 (secrets management)
- Investigative pillar exists in the index from v1, but is empty until fred ships
- Index format and fetch protocol must support **two ownership tiers from v1**: registry-maintained (canonical, unprefixed) and contributor-maintained (`<owner>/<name>`)
- CI smoke-tests entries via parse/dry-run only; CI does not perform network fetches against real data sources
- Monorepo subdirectory structure (canonical entries): `<pillar>/<entry-name>/{arcform.yaml, *.sql, README.md}`
- Contributor entries live in any GitHub repo; structure constrained to `arcform.yaml + sidecar .sql + README.md` at the repo root or a declared subdir
- Per-entry versioning via git tags namespaced by entry (e.g. `brewtrend/v1.0` for canonical, owner-defined tagging for contributor entries)
- Local cache location: `~/.arcform/registry/<name>/<ref>/`
- v1 must demonstrate Bellingcat-as-inspiration through Investigative pillar character (analytical inquiry into matters of consequence) without including Bellingcat itself as an entry

### Success Criteria

- `arc registry list` returns the two v1 entries (brewtrend, gnaf) with pillar tags from the hosted index; Investigative pillar surfaces as empty-but-reserved
- `arc registry show <name>` returns formatted metadata + README without fetching pipeline files
- `arc registry fetch <name>` clones (or tarball-fallback) the entry to `~/.arcform/registry/<name>/<ref>/`
- `arc registry run <name>` fetches (if not cached) and executes the pipeline end-to-end
- Each v1 entry runs successfully on a fresh machine and produces real, useful outputs
- "asset registry" terminology renamed to "assets" throughout forward documentation and code
- CI smoke-test passes for both v1 entries on every release of `meridian-online/registry`
- Entry README shows both modes: `arc registry run <name>` (local) and a scheduling example (cloud)
- fred (Investigative) ships in a follow-up spec on card 0022 once card 0021 (secrets management) lands

### Decisions Surfaced

1. **Pipeline catalogue takes the "registry" name; existing "asset registry" renames to "assets"** — chose to free the registry namespace for the user-facing catalogue. "Assets" matches the asset-centric philosophy (decision 0001) and is shorter/cleaner. → MADR candidate

2. **End-users-first orientation** — chose end users (analysts running for value) over learners or contributors as the primary audience. Learning and test-fixture use become side-effects. → MADR candidate

3. **Hosted index + on-demand fetch** — chose hosted distribution over bundling-with-binary or separate-repo-managed-by-user. Decouples registry lifecycle from `arc` binary releases; supports unbounded growth. → MADR candidate

4. **CLI surface: `list / show / fetch / run`** — full surface for v1. → To be specced

5. **Distribution via monorepo + git fetch (tarball fallback)** — chose monorepo with subdirs per entry, fetched via `git sparse-checkout`, with GitHub archive tarball fallback when git is missing. Free versioning via tags. → MADR candidate

6. **Authoring in YAML + sidecar `.sql` files (no compile step)** — preserves first-class SQL files for diff and tooling. Same form for authoring and shipping. → To be specced

7. **Three pillars: Practical / Foundational / Investigative** — HubSpot-style plain-English content pillars. Parallel adjective form. → MADR candidate (informs registry organisation)

8. **v1 scope: one canonical entry per pillar — brewtrend, gnaf, fred** — proves the three-pillar model end-to-end with realistic variety. → To be specced

9. **Smoke-test fixture role only** — CI runs parse/dry-run/schema validation; no live data fetches in CI. Lets entries serve real end users without CI constraints. → To be specced

10. **One card, multiple specs** — card 0022 becomes "Registry" (the capability). Specs spawn from it: CLI, monorepo+entries, assets rename. → Process decision

11. **Two-tier ownership model (Docker Hub pattern)** — registry supports both registry-maintained (canonical, unprefixed names) and contributor-maintained (namespaced `<owner>/<name>`) entries. v1 ships only canonical entries, but the architecture supports both tiers from day one. Trust signalling via verified-canonical badge in `list`/`show` output. → MADR candidate

### Implementation Notes

**Index format** (registry.yaml at monorepo root, schema TBD by spec):
- Per entry (at minimum): name, owner (null for canonical, e.g. `duckdb` for contributor), pillar, summary, current_version, repo_url, repo_path, sources, schedule_guidance, min_arcform_version
- Canonical entries: `owner: null`, `repo_url: meridian-online/registry`, `repo_path: <pillar>/<name>`
- Contributor entries: `owner: <github-user-or-org>`, `repo_url: <any-github-repo>`, `repo_path: <subdir-or-root>`

**Naming and resolution:**
- `arc registry run brewtrend` → resolves to canonical entry (owner null)
- `arc registry run duckdb/tpc-h-loader` → resolves to contributor entry under owner `duckdb`
- Collision rule: canonical names take precedence; contributor entries cannot shadow canonical ones (validated at index publish time)

**Trust signalling:**
- `arc registry list` and `arc registry show` mark canonical entries with a verified indicator (TBD in spec — could be a tag, a column, or a colour)
- Contributor entries display owner prominently; warning on first fetch of an unfamiliar owner is a v2 consideration

**Per-entry directory shape:**
- `<pillar>/<name>/arcform.yaml`
- `<pillar>/<name>/*.sql`
- `<pillar>/<name>/README.md` (with both `arc run` and scheduling examples)

**Cache:** `~/.arcform/registry/<name>/<ref>/` — keyed by entry name and resolved ref

**Fetch protocol:**
- Prefer `git` (`git clone --depth=1` + `git sparse-checkout` to entry subdir at requested ref)
- Fallback to `https://github.com/meridian-online/registry/archive/refs/tags/<tag>.tar.gz` extracted into cache
- Detection: probe `git --version` once; cache result for session

**Versioning:** per-entry git tags (e.g. `brewtrend/v1.0`, `gnaf/v0.3`) — enabled by Git's tag namespacing

**New repo to create:** `meridian-online/registry` (greenfield)

**v1 entry workload:**
- **brewtrend (Practical):** port from `/home/hugh/reference/brewtrend/`; polish; add metadata + README
- **gnaf (Foundational):** greenfield; multi-stage zip extraction + PSV bulk load with `modified_after` preconditions; reference inputs at `/home/hugh/reference/gnaf/`
- **fred (Investigative):** greenfield; HTTP API + JSON ingestion + time-series transforms; needs API key handling (touches card 0021 — secrets management)

**Smoke-test scope (per entry, in CI):**
- Parse `arcform.yaml` against current arcform schema
- Run sqlparser-rs introspection over each `.sql` file
- Validate `depends_on` graph is consistent (no cycles, all targets resolve)
- Lint URLs in `sources` for well-formedness (no fetch)

**Adjacencies:**
- Card 0008 (asset registry) — needs rename to "assets"; affects card slug, internal vocabulary, future docs (existing spec folder kept for historical accuracy)
- Card 0020 (pipeline scheduling) — registry entries should demonstrate this; per-entry README shows scheduling alongside `arc registry run`
- Card 0021 (secrets management) — FRED entry depends on this for API key handling

**Spec split (proposed for /orb:rally to coordinate):**
- Spec A: Registry CLI + index + cache (the infrastructure capability)
- Spec B: Registry monorepo scaffolding + three v1 entries (the content)
- Spec C: Assets rename (cross-cutting cleanup; touches card 0008's terminology)

### Open Questions

- Should the index support pinning min/max arcform versions per entry (compat envelope), or assume registry tracks the latest arcform?
- FRED API key — acceptable as a v1 entry given the key requirement, or wait for an entry that needs no auth?
- Schedule guidance format — cron string, human-readable text, or both? How prescriptive?
- Capture Bellingcat-as-inspiration where? Registry-level methodology doc, Investigative-pillar README section, or leave informal?
- Assets rename mechanics — rename existing card 0008's slug + spec folder, or keep historical names and rename only forward usage?
- Per-entry README schedule examples — concrete cron + arc-schedule snippet, or just narrative guidance?
- Contributor-tier discovery — does `arc registry list` show contributor entries by default (querying registry.arcform.dev for the union), or only canonical until a contributor source is explicitly added?
- Contributor entries pre-publication — how does an owner advertise a new entry? Self-service form on registry.arcform.dev, PR to a contributor index file in `meridian-online/registry`, or pure pull-from-GitHub at fetch time?
- Trust UX for contributor entries — verified badge styling, first-fetch warning, owner reputation signals?
