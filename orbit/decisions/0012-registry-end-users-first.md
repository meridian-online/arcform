---
status: accepted
date-created: 2026-04-29
date-modified: 2026-04-29
---
# 0012. ArcForm Registry is End-Users-First

## Context and Problem Statement

Card 0022 originally framed the work as "reference pipelines" — examples shipped in the repo for learning and integration testing. As discovery deepened, three plausible audiences emerged for the registry:

1. **Learners** — new ArcForm users studying real working examples to learn the tool
2. **End users** — analysts and operators who run registry pipelines for actual analytical value
3. **Contributors** — ArcForm maintainers using registry entries as integration test fixtures

Each audience implies different optimisation targets (readability vs working-out-of-box vs CI-friendliness). A primary audience must be chosen to avoid optimising for the average and serving none well.

## Considered Options

- **Learners first** — entries optimised for readability, comments, progressive complexity
- **End users first** — entries optimised for working out-of-the-box with real, current data
- **Contributors first** — entries optimised for feature coverage and CI-friendliness
- **All three equally weighted** — design decisions optimise for the union; broader value at higher upfront cost

## Decision Outcome

Chosen option: "End users first", because it makes the registry a *product surface* in its own right rather than a docs annex. Each entry must produce real, current outputs an analyst can act on. Learning by example and CI-fixture roles become useful side-effects, not primary goals.

Practical implications:
- Each registry entry must produce real, current value (not toy examples)
- Maintenance is part of the deal — entries must keep working as upstream sources evolve
- Test-fixture role is reduced to smoke-testing only (decision deferred to spec; see interview 2026-04-29-arcform-registry)
- Documentation depth is a side-effect, not a forcing function

### Consequences

- Good, because registry becomes a product surface, not a static documentation directory
- Good, because forces realistic data flows that demonstrate ArcForm's value
- Good, because aligns with local-first, remote-compatible design (decision 0006) — entries demonstrate both modes
- Bad, because maintenance burden grows with the catalogue (upstream source drift breaks entries)
- Bad, because may need investment in source resilience (retries, caching, fallback) that learners-first wouldn't require
