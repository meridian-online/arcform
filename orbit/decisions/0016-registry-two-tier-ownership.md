---
status: accepted
date-created: 2026-04-29
date-modified: 2026-04-29
---
# 0016. Registry Supports Two-Tier Ownership (Canonical + Contributor)

## Context and Problem Statement

Registry growth needs a contribution surface. Pure curation (everything reviewed by the ArcForm team) gates community contributions and bottlenecks the catalogue on maintainer attention. Pure self-publish (everyone publishes; no curation) loses curatorial signal and trust gradient.

Docker Hub's two-tier model is the closest parallel: `docker pull python:3.11` (official, curated) versus `docker pull duckdb/duckdb` (contributor-namespaced). Users intuitively grasp the distinction. The same pattern applies cleanly to ArcForm registry entries.

## Considered Options

- **Canonical only** — every registry entry reviewed and merged by the ArcForm team. High trust, low scale.
- **Contributor only** — anyone can publish; no curation. High scale, no trust gradient.
- **Two-tier (canonical + namespaced contributor)** — Docker Hub pattern. Canonical entries unprefixed; contributor entries `<owner>/<name>`.
- **Federation (multiple registries, user adds sources)** — npm/cargo-style multiple registries; user explicitly adds sources. More flexible, more complex CLI surface and trust UX.

## Decision Outcome

Chosen option: "Two-tier (canonical + namespaced contributor)", because it matches the user mental model already established by Docker Hub, npm scoped packages, and similar ecosystems. Canonical entries get curatorial trust; contributor entries scale via owner accountability without bottlenecking on ArcForm-team review.

Naming convention:
- **Canonical:** unprefixed (e.g. `brewtrend`, `gnaf`, `fred`)
- **Contributor:** `<owner>/<name>` (e.g. `duckdb/tpc-h-loader`, `mycorp/sales-pipeline`)

Resolution rules:
- Canonical names take precedence — contributor entries cannot shadow canonical ones
- Collision is detected at index publish time and rejected
- `arc registry run brewtrend` always resolves to the canonical entry; `arc registry run someone/brewtrend` is allowed (different namespace)

Storage:
- Canonical entries live in `meridian-online/registry` (per decision 0014)
- Contributor entries live in any GitHub repo; the index records `{owner, repo_url, repo_path, ref}`
- Both tiers fetched via the same protocol (git + tarball fallback)

Trust signalling (TBD by spec):
- `arc registry list` and `arc registry show` mark canonical entries with a verified indicator
- Contributor entries display owner prominently
- First-fetch warning for unfamiliar owners — deferred to v2

v1 ships only canonical entries (brewtrend, gnaf, fred), but the index format, naming convention, and resolver support both tiers from day one. This avoids a future migration when contributor entries arrive.

### Consequences

- Good, because matches user mental model (Docker Hub, npm, etc.) — minimal new vocabulary
- Good, because canonical entries retain curatorial signal; contributor entries scale via owner accountability
- Good, because canonical names cannot be shadowed (collision rule preserves trust)
- Good, because no `arc` migration when contributor entries arrive (architecture supports both tiers from v1)
- Good, because supports community growth without bottlenecking on ArcForm-team review capacity
- Bad, because the index/CLI must distinguish tiers visually (verified indicator + owner prominence) — design surface to be specced
- Bad, because owner verification policy needs definition before contributor entries are accepted (e.g. who counts as `duckdb`?) — deferred to v2
- Bad, because trust UX (first-fetch warnings, owner reputation signals) becomes ongoing design surface as the catalogue grows
