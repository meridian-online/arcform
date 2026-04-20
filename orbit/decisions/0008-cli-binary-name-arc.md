---
status: accepted
date-created: 2026-03-31
date-modified: 2026-03-31
---
# 0008. CLI Binary Name is `arc`

## Context and Problem Statement

The project is called ArcForm, but the CLI binary name is what users type hundreds of times. It needs to be short and ergonomic.

## Considered Options

- **`arcform`** — matches the project name exactly. Unambiguous but verbose (7 chars).
- **`arc`** — short, memorable, fast to type (3 chars). Follows the pattern of `git`, `cargo`, `dbt`.

## Decision Outcome

Chosen option: "`arc`", because CLI-familiar analysts value brevity. `arc init`, `arc run` — clean and fast. The project remains "ArcForm" for branding; the binary is `arc`.

### Consequences

- Good, because 3 characters — minimal typing
- Good, because `arc` is evocative (arc of a pipeline, architectural form)
- Bad, because `arc` may conflict with other tools on PATH — low risk in practice
