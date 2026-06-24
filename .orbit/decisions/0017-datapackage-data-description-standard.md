---
status: proposed
date-created: 2026-06-21
date-modified: 2026-06-21
---
# 0017. Frictionless Data Package is meridian's Data-Description Standard (not the pipeline format)

## Context and Problem Statement

meridian is four packages, each touching data at a different stage: finetype (profile / infer types), dovetail (model how to load and how datasets relate), arcform (execute pipelines), brightfield (visualise). Each package needs to *describe data* — fields, types, relationships, provenance — and today each does so in ad hoc, package-local ways. Without a shared contract, a schema discovered by finetype, a relationship found by dovetail, and the inputs/outputs of an arcform pipeline cannot interoperate, and every package re-invents the same metadata.

A standard already exists: the Frictionless **Data Package** (`datapackage.json`), published as JSON Schema profiles, with Table Schema for fields/types and `foreignKeys` for relationships. dovetail has already adopted it as its canonical emitted artifact (dovetail choice 0002).

The trap to avoid: a Data Package is a **description of data**. It is not an executable. It can carry a *load recipe* for ingestion, but it cannot express download → pre-process → transform → analyse → export. That lifecycle is the arcform pipeline (`arcform.yaml` + SQL). Conflating the two — treating `datapackage.json` as a runnable unit, or `arc run datapackage.json` as equivalent to `arc run pipeline.yaml` — would be a category error, and one we nearly made when scoping the registry (card 0022).

## Considered Options

- **Bespoke per-package descriptors** — each package defines its own JSON/YAML schema for data. Maximum local fit; zero interoperability; perpetual re-invention.
- **Plain JSON Schema only** — describe each table's shape with JSON Schema. Validates per-table structure, but cannot express cross-table relationships (`foreignKeys`) or carry provenance/identity in a standard way.
- **Frictionless Data Package as the shared data-description standard** — adopt it across meridian for describing data: Table Schema for per-resource shape, `foreignKeys` for relationships, standard identity/provenance fields, `x-`/namespaced custom properties for meridian-specific metadata.
- **Data Package as the pipeline/execution format** — *rejected*. Data Package describes data, not workflows; it cannot represent the transform/analyse/export lifecycle. The pipeline format stays arcform's yaml+sql.

## Decision Outcome

Chosen option: "Frictionless Data Package as the shared data-description standard", scoped strictly to *describing data*.

**What this commits us to:**

- Wherever a meridian package describes data — finetype's inferred schemas, dovetail's load/relate outputs, the input/output contract a pipeline publishes — the canonical serialisation is a Frictionless Data Package descriptor.
- Per-resource shape uses **Table Schema** (fields, types, constraints). Cross-resource structure uses **`foreignKeys`**. Identity uses `name` / `id` / `version`; provenance uses `sources` / `created` / `contributors`.
- meridian-specific metadata rides as **custom properties** under a reserved prefix (`x-` / namespaced, e.g. `x-dovetailLoadRecipe`, `x-dovetailSemanticType`), never by forking the schema. Where stronger validation is wanted, add a versioned **profile** via `$schema` rather than inventing a new format.
- This aligns arcform with dovetail choice 0002, so dovetail's emitted descriptors and arcform's data contracts speak the same language end-to-end.

**The boundary (load-bearing):**

- A Data Package **describes** data; it does **not** execute. The unit of execution stays the arcform pipeline (`arcform.yaml` + SQL).
- A pipeline **MAY ship a `datapackage.json`** describing its inputs and/or outputs (this is what makes a future catalogue entry browsable — "here is the schema brewtrend produces"). The datapackage.json is companion metadata, not the runnable artifact.
- `arc run` continues to take a pipeline manifest. There is no `arc run datapackage.json` that means "execute"; at most a Data Package's *load recipe* is one ingestion step a pipeline references.

This decision is **meridian-wide in intent**, recorded here because arcform is where the registry (card 0022) forced the question; it should be mirrored as a cross-package commitment (dovetail choice 0002 already aligns).

### Consequences

- Good, because finetype, dovetail, and arcform describe data in one interoperable, JSON-Schema-validated standard instead of three bespoke ones.
- Good, because relationships (`foreignKeys`) and provenance are first-class and standard, not meridian inventions to maintain.
- Good, because it keeps a clean separation of concerns: Data Package = *what the data is*; arcform pipeline = *what to do with it*. The registry's earlier near-conflation is closed off explicitly.
- Good, because a pipeline that ships a datapackage.json IO contract becomes self-describing — the foundation a browsable catalogue needs (cards 0023, 0022).
- Good, because adopting an external standard with published JSON Schema means off-the-shelf validation and ecosystem tooling.
- Bad, because Data Package is designed for *data* description; some meridian needs will live in custom `x-` properties or profiles, which we must govern (naming, what graduates into a profile) to avoid an accreting bespoke dialect by the back door.
- Bad, because coarse Frictionless types lose finetype's semantic precision; we carry the richer type as a custom property (`x-...SemanticType`) and accept the descriptor's standard `type` is a lossy projection.
- Neutral, because this says nothing about the pipeline format, distribution, or the catalogue — those are separate decisions (registry stack 0011–0016, milestone sequencing in cards 0023/0022).
