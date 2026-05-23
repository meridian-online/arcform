---
status: accepted
date-created: 2026-03-31
date-modified: 2026-03-31
---
# 0006. Local-First, Remote-Compatible Design

## Context and Problem Statement

Data pipeline tools typically target either local development or remote execution. Users want both. GCP CloudBuild is excellent but untestable locally. `nektos/act` solves this for GitHub Actions. What's ArcForm's model?

## Considered Options

- **Local-only** — optimise for local analytical workflows
- **Remote-first** — build for scheduling and deployment
- **Local-first, remote-compatible** — develop locally with full fidelity, same manifest runs remotely

## Decision Outcome

Chosen option: "Local-first, remote-compatible", because the key value proposition is "what runs locally also works remotely." Achieved by:
1. Delegating execution to engine CLIs (same CLI locally and remotely)
2. Preflight version assertions (verify engine version matches before execution)
3. No local-only features — everything in the manifest is portable

### Consequences

- Good, because fast local iteration with confidence it works in production
- Good, because preflight assertions catch version mismatches
- Bad, because some remote capabilities (secrets, networking) are outside scope
