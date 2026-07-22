# Arcform

> Local-first data pipeline engine for analytical workflows.

Arcform is a Rust-based workflow engine built for data analysts who want the power of a structured pipeline without the overhead of cloud infrastructure. It runs as a single binary, orchestrates multi-stage dataflows, and understands the internal structure of its steps — not just whether they succeeded.

---

## Design Principles

**Local-first.** Arcform runs on your machine with no external dependencies. No managed services, no cloud accounts, no ops overhead. Suitable for air-gapped environments and reproducible local development.

**Asset-aware.** Inspired by Dagster's software-defined asset model, Arcform treats data outputs — not tasks — as the primary unit of work. The pipeline graph reflects data dependencies, not just execution order.

**Structurally transparent.** A SQL step is not a black box. Using the DataFusion SQL parser, Arcform can inspect and decompose queries into their constituent parts — load operations, CTE dependencies, and export targets — enabling fine-grained lineage and partial re-execution.

**Composable by design.** Pipelines are defined in YAML and composed from discrete, reusable steps. Each stage has a clear input contract and output contract.

---

## Pipeline Stages

Arcform pipelines are organised into four stage types, reflecting the natural flow of an analytical workflow.

### Pre-SQL
Preparation steps that operate outside the database.

- File retrieval via `curl`
- JSON and YAML transformation via `jq` / `yq`
- Data validation via JSON Schema (integrated with FineType)
- Remote storage sync via `rclone`

### SQL
Structured query steps executed against DuckDB.

- Data loading via DuckDB `read_*` functions
- Data modelling with standard SQL and CTEs
- Data export via DuckDB `COPY` functions

### Advanced Analytics
Compute-intensive steps for ML workflows.

- Vector embedding generation
- CatBoost model training and inference

### Export and Activation
Output steps that deliver results beyond the database.

- File exports via DuckDB `COPY`
- Remote sync via `rclone`
- Chart rendering
- Markdown report generation

---

## Architecture

Arcform models each pipeline as a directed acyclic graph (DAG) of data assets. Edges in the graph represent data dependencies between assets, not just task sequencing.

This distinction matters: when an upstream asset changes, Arcform knows which downstream assets are stale and can trigger selective re-materialisation rather than a full pipeline re-run.

For SQL steps, the DataFusion SQL parser provides structural introspection — allowing Arcform to surface CTE-level dependencies and treat individual load and export operations as discrete graph nodes.

---

## Relationship to the Meridian Ecosystem

Arcform is part of the [Meridian](https://github.com/meridian) project family, alongside [FineType](https://github.com/meridian/finetype).

- **FineType** classifies and validates text data types, providing a transformation contract from raw text to typed DuckDB expressions.
- **Arcform** orchestrates the pipelines in which that data flows — from ingestion through modelling to output.

The two libraries are designed to complement each other. FineType's JSON Schema validation integrates directly into Arcform's Pre-SQL stage, enabling data quality checks as a first-class pipeline step.

---

## Using Arcform as a library

`arc` ships a library target alongside the binary, for one purpose: a tool that edits an
`arcform.yaml` should not carry its own copy of the schema. Two schemas drift; one cannot.

```toml
[dependencies]
arc = { version = "=0.1.0", default-features = false }
```

`default-features = false` turns off the `cli` feature. That feature exists for the `arc`
binary; it also compiles a doc-hidden `cli_main` into the crate root, and `cli_main` parses
the *calling* process's `argv` and can exit it. With the feature off the crate root is
`spec` and nothing else, and `clap` leaves the dependency graph.

```rust
use arc::spec::{Manifest, Result};

fn example(edited: &str) -> Result<()> {
    // A spec on disk.
    let manifest = Manifest::load(std::path::Path::new("examples/brewtrend"))?;
    println!("{} — {} steps", manifest.name, manifest.steps.len());

    // Or candidate bytes that are not a file yet — validate before you write.
    Manifest::from_yaml_str(edited)?;
    Ok(())
}
```

`arc::spec` is the entire public surface: the spec schema types, two entry points, and the
error type. Validation is a **gate, not a transform** — it parses raw YAML to reach a verdict
and hands nothing back to write, so an editor passes its own bytes through and then writes
those same bytes. That is the only way comments, key order and formatting survive. The schema
types do currently derive `serde::Serialize` (used by `arc init` to emit a *generated*
manifest into an empty directory), but that derive is **out of contract**: serialising a
loaded spec and writing it back destroys the author's bytes, and a later release is expected
to withdraw the impl. Everything else — the runner, the engine, SQL introspection, the
operator catalog, the registry — is private and moves without notice.

The surface is documented as a contract in `src/spec.rs` and enumerated by
`tests/public_surface.rs`, which fails the build if it widens. Arcform is pre-1.0: **pin an
exact version** (or, inside the same organisation, a git revision).

---

## Status

Early development. The project is in the discovery and design phase.

---

## Credits

Part of the [Meridian](https://meridian.online) project.

Built with [DuckDB](https://duckdb.org), [sqlparser-rs](https://github.com/sqlparser-rs/sqlparser-rs) (SQL introspection), and [Serde](https://serde.rs).

Pipeline model and step execution inspired by [Dagu](https://github.com/dagu-org/dagu). Asset-centric design influenced by [Dagster](https://dagster.io/)'s software-defined asset model. SQL-first approach informed by [SQLMesh](https://sqlmesh.com/) and [dbt](https://www.getdbt.com/). Local-remote parity pattern drawn from [nektos/act](https://github.com/nektos/act).

