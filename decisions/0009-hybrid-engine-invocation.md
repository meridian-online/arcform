---
status: accepted
date-created: 2026-03-31
date-modified: 2026-03-31
---
# 0009. Hybrid Engine Invocation: Defaults + Command Override

## Context and Problem Statement

ArcForm delegates SQL execution to engine CLIs. Hardcoding each engine's CLI interface limits extensibility. Dagu's approach (user declares the full command) is maximally extensible but verbose. How should ArcForm balance ergonomics and extensibility?

## Considered Options

- **Engine-aware only** — ArcForm constructs CLI calls for known engines. Every new engine needs code.
- **User-declared command (Dagu-style)** — User writes the full command string. Maximum extensibility.
- **Hybrid — defaults + command override** — Engine-aware defaults for known engines, raw `command:` for everything else.

## Decision Outcome

Chosen option: "Hybrid — defaults + command override". A step with `sql:` uses engine-aware invocation. A step with `command:` runs a raw shell command. This means:

1. 90% of steps use `sql:` — clean, minimal, engine-aware, introspectable
2. `command:` is the generic escape hatch — any CLI tool, no code changes needed
3. Popular `command:` patterns get observed and may be promoted to typed steps later

### Step Resolution Rules

A step MUST have exactly one of `sql` or `command` (not both, not neither).

### Promote-Patterns Principle

Use `command:` broadly. Observe which patterns repeat across pipelines. Promote recurring patterns to first-class step types with known input/output contracts for asset tracking.

### Consequences

- Good, because the common case stays clean
- Good, because any CLI tool works without code changes
- Good, because `command:` is a proving ground for future typed steps
- Bad, because `command:` steps can't be introspected for asset dependencies (require explicit declaration)
