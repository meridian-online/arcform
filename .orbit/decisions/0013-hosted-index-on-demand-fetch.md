---
status: accepted
date-created: 2026-04-29
date-modified: 2026-04-29
---
# 0013. Registry Uses a Hosted Index with On-Demand Fetch

## Context and Problem Statement

End-users-first registry (decision 0012) requires a way to discover and obtain entries. The distribution model determines registry growth potential, contribution flow, and binary footprint.

## Considered Options

- **Bundled with arc binary** — registry data ships inside or alongside the `arc` binary. Works offline, smallest external surface, but tightly couples registry growth to binary releases and limits catalogue size.
- **Separate companion repo (user-managed)** — `arcform-registry` as its own repo; users clone it manually. Decoupled lifecycle, but every user must understand and manage the clone.
- **Hosted index + on-demand fetch** — registry entries live remotely. `arc registry` commands hit a published index and fetch entries just-in-time into a local cache.
- **Hybrid: in-repo seed + remote** — small curated seed bundles with `arc`; `arc registry update` fetches more. Best of both worlds, but two distribution paths to maintain.

## Decision Outcome

Chosen option: "Hosted index + on-demand fetch", because it scales the catalogue independently of `arc` binary releases, decouples registry maintenance from compiler releases, and supports the two-tier ownership model (decision 0016) — which would be impractical with bundled distribution.

Architecture:
- A hosted index (e.g. `registry.arcform.dev` or alongside `meridian-online/web/`) publishes the canonical entry list
- `arc registry list` queries the index, returns metadata
- `arc registry show <name>` returns metadata + README without fetching pipeline files
- `arc registry fetch <name>` downloads the entry to local cache (`~/.arcform/registry/<name>/<ref>/`)
- `arc registry run <name>` performs fetch (if not cached) + execute

### Consequences

- Good, because registry size is unbounded — no binary-bloat constraints
- Good, because registry lifecycle is decoupled from `arc` binary releases
- Good, because supports the two-tier ownership model (decision 0016) cleanly
- Good, because the index is a small, fast resource amenable to CDN caching
- Bad, because requires hosting infrastructure (index endpoint, availability)
- Bad, because offline UX needs a thoughtful cache + retry strategy
- Bad, because index format becomes a versioned interface that must remain compatible across `arc` releases
