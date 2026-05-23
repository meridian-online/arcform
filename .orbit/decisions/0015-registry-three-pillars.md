---
status: accepted
date-created: 2026-04-29
date-modified: 2026-04-29
---
# 0015. Registry is Organised Around Three Pillars: Practical, Foundational, Investigative

## Context and Problem Statement

The registry needs an organising structure. Without one, growth produces a flat catalogue where users cannot reason about *why* an entry exists or *which* entries belong together. The structure should articulate distinct reasons to use ArcForm and provide growth slots that scale beyond v1's three canonical entries.

HubSpot-style content pillars (Educational / Inspirational / Promotional / Community-Building / Behind-the-Scenes) provided the linguistic model: plain-English descriptors that tell the audience what a content unit *is*, in their language. The pillar names should follow the same form — single adjectives, parallel construction, no jargon.

## Considered Options

- **No pillars (flat catalogue)** — entries listed by name only. Simplest to implement; loses navigational structure; growth produces a long undifferentiated list.
- **Two pillars (e.g. Personal / Public)** — minimal taxonomy; risks forcing entries that don't fit either bucket.
- **Three pillars: Practical / Foundational / Investigative** — three distinct reasons to reach for the registry; parallel adjective form; plain-English.
- **Five+ pillars (HubSpot's full set adapted)** — finer-grained categories; risks pillar proliferation and ambiguous boundaries.

Pillar naming itself went through several iterations during discovery (Useful/Streamline/Public Interest → Pulse/Foundation/Inquiry → Everyday/Foundational/Civic → Practical/Foundational/Investigative). The final form was chosen for parallel adjective construction and plain-English clarity matching HubSpot's pattern.

## Decision Outcome

Chosen option: "Three pillars: Practical / Foundational / Investigative", because each pillar maps to a distinct "why" for using ArcForm:

| Pillar           | Tagline                              | v1 anchor                          | Demonstrates                       |
|------------------|--------------------------------------|------------------------------------|------------------------------------|
| Practical        | Everyday signals worth checking      | brewtrend                          | Daily-use, fast iteration, local   |
| Foundational     | Hard data made tractable             | gnaf                               | Heavy bulk loads with cheap reruns |
| Investigative    | Analytical inquiry that matters      | fred (deferred to v1.1, gated on card 0021 — secrets management) | Civic inquiry, multi-source joins  |

Pillar definitions:
- **Practical** — entries serving everyday personal/operational needs. Small data, frequent runs, fast iteration. Local-primary.
- **Foundational** — entries that take notoriously painful data sources (multi-file dumps, complex schemas) and make them tractable substrate for downstream work. Expensive first run, cheap incrementals via preconditions.
- **Investigative** — entries that perform analytical inquiry into matters of public consequence. Civic monitoring, accountability work, macroeconomic tracking. Cloud-schedule-friendly.

Bellingcat (referenced as inspiration during discovery) is treated as informing the *character* of the Investigative pillar — analytical inquiry, evidence-driven — not as a registry entry itself.

Pillar tagging is a required field on every registry entry's metadata. Entries belong to exactly one pillar.

### Consequences

- Good, because each pillar maps to a distinct "why" — users can articulate which pillar fits their need
- Good, because parallel adjective form is plain-English and HubSpot-cadence (familiar to a non-engineering audience)
- Good, because pillars provide growth slots without prescribing exact entries (the registry can add `hn-trending` to Practical, `osm-extract` to Foundational, `parl-voting` to Investigative)
- Good, because pillars shape navigational structure for the index, the docs site, and any future registry UI
- Bad, because pillar boundaries are occasionally fuzzy (some entries straddle Practical and Investigative depending on framing)
- Bad, because locks in three categories — adding a fourth later requires a successor decision and migration of any orphan entries
