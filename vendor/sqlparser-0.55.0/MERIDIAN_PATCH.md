# Meridian fork of sqlparser 0.55.0

This is a **vendored, minimal fork** of [`sqlparser`](https://crates.io/crates/sqlparser)
`0.55.0`, wired into `arc` via `[patch.crates-io]` in the top-level `Cargo.toml`.
It teaches the **DuckDB dialect** three grammars the published crate rejects, all of
which otherwise make `arc`'s SQL introspection degrade a whole SQL step to an *opaque*
node (undercutting the "0 opaque steps" legibility story).

The version stays `0.55.0` and the dependency set is unchanged, so the patch applies
cleanly and the resolved dep graph / licenses are identical to upstream (`Cargo.lock`
only drops the registry `source`/`checksum` lines for `sqlparser`).

## What the fork adds

### 1. Statement-form `PIVOT` / `UNPIVOT` (DuckDB "simplified" syntax)

DuckDB supports a statement form distinct from the SQL-standard
`FROM t PIVOT (…)` table-factor (which upstream already parses):

```sql
PIVOT   <source> ON <expr>[, …] [USING <aggregate>[, …]] [GROUP BY <expr>[, …]]
UNPIVOT <source> ON <expr>[, …] [INTO NAME <ident> VALUE <ident>[, …]]
```

These are queries in their own right — usable as a **top-level statement**, a
**CTAS body** (`CREATE TABLE x AS PIVOT …`), or a **subquery** — and auto-detect
the pivot values from the data (no explicit `IN (…)` list).

- `ast/query.rs`: two new `SetExpr` variants, `Pivot(Box<PivotStatement>)` and
  `Unpivot(Box<UnpivotStatement>)`, plus the `PivotStatement` / `UnpivotStatement` /
  `UnpivotInto` structs and their `Display` impls.
- `ast/mod.rs`: re-export the three new public types.
- `ast/spans.rs`: `Spanned` arms for the two variants (span of the source relation).
- `parser/mod.rs`:
  - `parse_statement`: route a leading `PIVOT`/`UNPIVOT` (DuckDB/Generic) through
    `parse_query`.
  - `parse_query_body`: parse the statement form when a query body begins with
    `PIVOT`/`UNPIVOT` (covers top-level, CTAS body, subquery, `WITH … PIVOT …`).
  - new `parse_pivot_statement` / `parse_unpivot_statement`. Both gated to
    `DuckDbDialect | GenericDialect`; other dialects are unaffected.

### 2. Multi-option DuckDB `COPY`

DuckDB's `COPY … (option …)` option set is open-ended
(`COMPRESSION`, `PARTITION_BY (…)`, `ROW_GROUP_SIZE`, `OVERWRITE_OR_IGNORE`, …),
but upstream only accepts a fixed PostgreSQL keyword list, so anything else errors:

```sql
COPY tbl TO 'f.parquet' (FORMAT parquet, COMPRESSION zstd);
COPY tbl TO 'out'       (FORMAT parquet, PARTITION_BY (year, month));
```

- `ast/mod.rs`: new `CopyOption::DuckDbOption { name: Ident, value: Option<Expr> }`
  variant (`None` = a bare flag) plus its `Display` arm.
- `parser/mod.rs` `parse_copy_option`: when no fixed PostgreSQL keyword matches and the
  dialect is `DuckDbDialect | GenericDialect`, capture the option generically as
  `name [value]` (`value` parsed as an `Expr`, so `(a, b)` becomes an `Expr::Tuple`).

### 3. DuckDB's `lambda x: expr` / `lambda x, y: expr` colon syntax

DuckDB is retiring its single-arrow lambda (`x -> x + 1`) in favour of a Python-style
colon form, and upstream only parses the arrow forms:

```sql
SELECT list_transform([1, 2, 3], lambda x: x + 1);      -- one param, no parens
SELECT list_reduce(xs, lambda acc, v: acc + v);          -- several params, no parens
```

Both existing arrow forms (`x -> …` and `(acc, v) -> …`) keep parsing unchanged —
this is additive, not a replacement.

- `keywords.rs`: new non-reserved `LAMBDA` keyword. Not added to any
  `RESERVED_FOR_*` list, so `SELECT lambda FROM t` (a column literally named
  `lambda`) still parses as an identifier — same fallback the existing `CASE`,
  `MAP`, `STRUCT` keyword-prefixed expressions already rely on.
- `dialect/mod.rs`: new `Dialect::supports_lambda_colon_syntax()` method
  (default `false`), kept **separate from** `supports_lambda_functions()`
  deliberately: that flag is also `true` for ClickHouse and Databricks, whose
  arrow-only lambdas have no colon form, so folding the new syntax into it
  would have made both dialects silently start accepting SQL neither engine
  runs.
- `dialect/duckdb.rs`: `supports_lambda_colon_syntax()` → `true`.
- `parser/mod.rs`:
  - `parse_expr_prefix_by_reserved_word`: a leading `LAMBDA` keyword, gated on
    `supports_lambda_colon_syntax()`, dispatches to `parse_lambda_colon_expr`.
  - new `parse_lambda_colon_expr`: parses a comma-separated identifier list,
    `:`, then the body expression. Produces the **existing** `Expr::Lambda(LambdaFunction)`
    node — `params: OneOrManyWithParens::One` for one identifier,
    `::Many` for several — rather than a new AST variant, since the two syntaxes
    describe the same function. `LambdaFunction`'s `Display` was already
    hardcoded to the arrow form (`{params} -> {body}`) before this change, so a
    colon-form parse canonicalizes to the arrow form on round-trip; there is no
    field recording which syntax the source used.

## Fidelity

All added forms round-trip: parse → `Display` → re-parse yields a structurally-equal
AST (verified against the DuckDB dialect). The lambda colon form round-trips to the
arrow form specifically (see above) rather than to itself — still structurally equal,
since the AST is the same either way. Existing behaviour for all other dialects is
untouched (the PIVOT/UNPIVOT and COPY grammars are gated behind
`DuckDbDialect | GenericDialect`; the lambda colon syntax behind `DuckDbDialect` alone —
see the gating rationale above), and the crate's inline unit tests pass (the
pre-existing `ast::visitor::tests::overflow` test overflows the stack in a standalone
debug build on upstream `0.55.0` too — unrelated).

## Maintenance path — upstream this fork

The fork is the interim home; the intended path is an upstream PR to
`apache/datafusion-sqlparser-rs`, after which this vendored copy can be dropped and the
`[patch.crates-io]` entry removed in favour of a released version. **Do not open the PR
without an explicit go-ahead** (it is an outward-facing action). Draft below.

---

### Draft upstream PR (do not submit without sign-off)

**Title:** Support DuckDB statement-form `PIVOT`/`UNPIVOT` and open-ended `COPY` options

**Summary**

DuckDB has a "simplified" statement form of `PIVOT`/`UNPIVOT` that is a query in its own
right (top-level, CTAS body, or subquery), distinct from the SQL-standard
`FROM t PIVOT (…)` table-factor form this crate already supports:

```sql
PIVOT   Cities ON Year USING SUM(Population) GROUP BY Country;
UNPIVOT monthly_sales ON jan, feb, mar INTO NAME month VALUE sales;
```

It also accepts an open-ended set of `COPY` options beyond the fixed PostgreSQL list
(`COMPRESSION`, `PARTITION_BY`, `ROW_GROUP_SIZE`, `OVERWRITE_OR_IGNORE`, …):

```sql
COPY tbl TO 'f.parquet' (FORMAT parquet, COMPRESSION zstd);
```

Refs: <https://duckdb.org/docs/stable/sql/statements/pivot>,
<https://duckdb.org/docs/stable/sql/statements/unpivot>,
<https://duckdb.org/docs/stable/sql/statements/copy>.

**Changes**

- Add `SetExpr::Pivot` / `SetExpr::Unpivot` (new `PivotStatement` / `UnpivotStatement` /
  `UnpivotInto` AST nodes) with `Display`, `Spanned`, and `Visit`/`VisitMut` support;
  parse them in `parse_query_body` and dispatch a leading `PIVOT`/`UNPIVOT` from
  `parse_statement`. Gated to `DuckDbDialect | GenericDialect`.
- Add `CopyOption::DuckDbOption { name, value }` and a generic fallback in
  `parse_copy_option` for DuckDB/Generic dialects.

**Tests** (to add before submission): `sqlparser_duckdb.rs` cases for each form,
including CTAS-body and `WITH … PIVOT …`, and round-trip (`verified_stmt`) coverage.

**Compatibility:** additive; other dialects are unchanged. New public enum variants
are a semver-minor API addition.

---

### Second draft upstream PR (do not submit without sign-off)

A separate PR from the one above — different feature, same "**Do not open the PR
without an explicit go-ahead**" rule.

**Title:** Support DuckDB's `lambda x: expr` colon syntax

**Summary**

DuckDB is retiring its single-arrow lambda (`x -> x + 1`) in favour of a Python-style
`lambda x: expr` / `lambda x, y: expr` form. Upstream currently only parses the arrow
forms (`x -> …`, `(acc, v) -> …`), so a model written in the syntax DuckDB is moving
users to fails to parse:

```sql
SELECT list_transform([1, 2, 3], lambda x: x + 1);
SELECT list_reduce(xs, lambda acc, v: acc + v);
```

Refs: <https://duckdb.org/docs/stable/sql/functions/lambda.html>.

**Changes**

- Add a non-reserved `LAMBDA` keyword.
- Add `Dialect::supports_lambda_colon_syntax()` (default `false`), separate from
  `supports_lambda_functions()` so ClickHouse and Databricks — which have arrow-only
  lambdas — are unaffected; enable it for `DuckDbDialect`.
- Parse `lambda <ident>[, <ident>]*: <expr>` into the **existing** `Expr::Lambda(LambdaFunction)`
  node (no new AST variant): one identifier → `OneOrManyWithParens::One`, several →
  `::Many` — the same shape the arrow forms already produce.

**Tests** (to add before submission): `sqlparser_duckdb.rs` cases for the one- and
multi-param colon forms, a dialect-gating case (ClickHouse/Databricks/Generic reject
it), and round-trip coverage noting the colon form canonicalizes to the arrow form on
`Display` (there is no AST field for which syntax the source used).

**Compatibility:** additive; other dialects are unchanged. New `Dialect` trait method
with a default impl is a semver-minor addition.
