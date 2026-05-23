---
status: accepted
date-created: 2026-04-29
date-modified: 2026-04-29
---
# 0014. Registry Distribution via Monorepo + Git Fetch (Tarball Fallback)

## Context and Problem Statement

Hosted index + on-demand fetch (decision 0013) requires a concrete transport for entry contents. Multiple shapes are viable: HTTP-served tarballs, git refs, direct file fetches. The choice affects authoring UX, versioning ergonomics, contribution flow, and `arc`'s dependency footprint.

A separate sub-question explored whether entries could collapse to a single inlined-SQL YAML for atomic distribution. Discovery rejected this — first-class `.sql` files preserve diff fidelity and SQL-aware tooling, which matters for both contributors and end users inspecting fetched entries.

## Considered Options

- **HTTP tarball (versioned)** — index entry has `url + sha256`. `arc` fetches and verifies. No git dependency; manual versioning via filename + index updates.
- **Git ref (URL @ commit/tag)** — index entry has `git_url + ref`. `arc` shells out to `git clone --depth=1`. Free versioning via tags; familiar contribution flow.
- **Direct file fetch** — index lists individual file URLs. Fine-grained but multiple round-trips and harder to atomically version a bundle.
- **Hybrid (tarball default, git allowed)** — tarball is the standard; index can also point to a git ref. Two code paths.

## Decision Outcome

Chosen option: "Monorepo + git fetch with tarball fallback", because:

1. **Free versioning** — git tags namespaced per entry (e.g. `brewtrend/v1.0`) avoid maintaining a parallel version index.
2. **Same form everywhere** — contributors author `arcform.yaml + sidecar .sql + README.md`; end users fetch the same shape. No compile or publish step in between.
3. **Familiar contribution flow** — PR to the registry repo, merge, tag, done.
4. **Tarball fallback removes hard dependency** — when `git` is unavailable on the user's machine, `arc registry fetch` falls back to `https://github.com/meridian-online/registry/archive/refs/tags/<tag>.tar.gz`.

Storage shape:
- Single monorepo: `meridian-online/registry`
- Subdirs per pillar: `practical/`, `foundational/`, `investigative/`
- Per entry: `<pillar>/<name>/{arcform.yaml, *.sql, README.md}`
- Index file (`registry.yaml`) at repo root catalogues canonical entries
- Per-entry git tags (e.g. `brewtrend/v1.0`) — enabled by Git's tag namespacing

Fetch protocol:
- Probe `git --version` once per session; cache the result
- If git is available: `git clone --depth=1 --filter=blob:none` + `git sparse-checkout` to entry subdir at requested ref
- If git is missing: HTTP fetch of the GitHub archive tarball at the requested ref, extract into cache
- Cache location: `~/.arcform/registry/<name>/<ref>/`

### Consequences

- Good, because git tags provide free per-entry versioning without a parallel version registry
- Good, because contributors and end users see the same on-disk shape (no compile step)
- Good, because GitHub-native contribution flow lowers barrier for community PRs
- Good, because tarball fallback removes the hard git dependency for Windows-without-git users
- Good, because monorepo provides a single locus for canonical entries (with the two-tier ownership model in 0016 covering off-monorepo entries)
- Bad, because shelling out to `git` binary adds a runtime dependency surface (must handle missing/old git versions)
- Bad, because monorepo growth requires sparse-checkout discipline to keep clones fast
- Bad, because per-entry tag conventions must be enforced (CI guard) to avoid accidental cross-entry version collisions
