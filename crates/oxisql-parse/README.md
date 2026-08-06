# oxisql-parse — SQL parsing, planning, and optimization for OxiSQL

[![Crates.io](https://img.shields.io/crates/v/oxisql-parse.svg)](https://crates.io/crates/oxisql-parse)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](https://www.apache.org/licenses/LICENSE-2.0)

> sqlparser facade with dialect-aware parsing, a fluent query builder, logical planning (with subquery decorrelation), a cost-based optimizer, and LRU parse/plan caches.

## What it is

`oxisql-parse` wraps [`sqlparser`](https://crates.io/crates/sqlparser) with
OxiSQL-idiomatic APIs: dialect-aware parsing, a fluent `QueryBuilder`, a
logical planner with correlated-subquery decorrelation, a rule-based (and
optionally cost-based, statistics-driven join-reordering) optimizer, DML
planning, a cost model, a schema validator, aggregate / window helpers, plain
and verbose/JSON `explain` pretty-printers, and thread-safe LRU caches for
both parsed ASTs (`ParseCache`) and optimized plans (`PlanCache`). It
re-exports the most commonly needed `sqlparser` AST types. The crate is Pure
Rust and has **no feature flags**.

## Installation

```toml
[dependencies]
oxisql-parse = "0.4.1"
```

MSRV 1.89 · edition 2021 · Apache-2.0.

## Quick start

```rust,no_run
use oxisql_parse::{parse, parse_one, is_read_only, QueryBuilder, ParseCache, SqlDialect};

// Parse multiple statements.
let stmts = parse("SELECT 1; SELECT 2")?;
assert_eq!(stmts.len(), 2);

// Parse a single statement and classify it.
let stmt = parse_one("SELECT 42")?;
assert!(is_read_only(&stmt));

// Fluent query builder.
let sql = QueryBuilder::select(&["id", "name", "email"])
    .from("users")
    .join("orders", "users.id = orders.user_id")
    .where_clause("users.active = TRUE")
    .order_by("users.name", true)
    .limit(10)
    .offset(20)
    .build()?;

// Thread-safe LRU parse cache, keyed by (sql, dialect).
let cache = ParseCache::new(32);
let _ = cache.parse("SELECT 1", SqlDialect::Generic)?;
assert_eq!(cache.len(), 1);
# Ok::<(), oxisql_core::OxiSqlError>(())
```

## Key API

### Parsing

| Function | Description |
|----------|-------------|
| `parse(sql)` | Parse with the generic dialect → `Vec<Statement>` |
| `parse_one(sql)` | Parse exactly one statement (error if 0 or 2+) |
| `parse_with_dialect(sql, dialect)` | Parse with a specific `SqlDialect` |
| `parse_one_with_dialect(sql, dialect)` | Single-statement parse with a dialect |
| `parse_postgres` / `parse_mysql` / `parse_sqlite` | Dialect shorthands |
| `format(stmt)` | Serialize a parsed `Statement` back to SQL text |

`SqlDialect`: `Generic`, `Postgres`, `MySQL`, `SQLite`.

### Analysis (`analysis` module)

| Function | Description |
|----------|-------------|
| `is_read_only(stmt)` | True if the statement is a SELECT (no side effects) |
| `normalize(sql)` | Canonicalize whitespace and casing |
| `extract_tables(stmt)` | Referenced table names |
| `extract_columns(stmt)` | Referenced column names |
| `count_params(sql)` | Count `$N` positional parameters |

### `QueryBuilder`

Fluent SELECT builder; chaining methods return `Self`.

| Method | Description |
|--------|-------------|
| `select(columns)` / `select_all()` | Start a SELECT / `SELECT *` |
| `distinct()` | Add `DISTINCT` |
| `from(table)` | Set the FROM table |
| `join` / `left_join` / `right_join` | Add INNER / LEFT / RIGHT JOIN |
| `where_clause(cond)` | Add a WHERE condition (multiple → AND) |
| `group_by(expr)` / `having(cond)` | GROUP BY / HAVING |
| `order_by(expr, ascending)` | ORDER BY (`true` = ASC) |
| `limit(n)` / `offset(n)` | LIMIT / OFFSET |
| `build()` / `build_and_parse()` | Produce SQL (consumes the builder) / build and validate via sqlparser |
| `build_ref()` | Produce SQL from `&self`, without consuming the builder |

Static DML helpers return a `String` directly:

```rust
# use oxisql_parse::QueryBuilder;
QueryBuilder::insert("users", &["id", "name"], &["1", "'Alice'"]);
// → "INSERT INTO users (id, name) VALUES (1, 'Alice')"
QueryBuilder::update("users", &[("name", "'Bob'")], Some("id = 42"));
// → "UPDATE users SET name = 'Bob' WHERE id = 42"
QueryBuilder::delete("users", Some("id = 99"));
// → "DELETE FROM users WHERE id = 99"
```

### Logical planner

| Item | Description |
|------|-------------|
| `plan_query(stmt)` / `plan_statement(stmt)` | Parsed `Statement` → `LogicalPlan` (default `PlannerOptions`, decorrelation on) |
| `plan_query_with(sql, opts)` / `plan_statement_with_opts(stmt, opts)` | Parse-and-plan / plan with explicit `PlannerOptions` (e.g. `PlannerOptions { decorrelate: false }` to keep subqueries structural) |
| `LogicalPlan` | `Scan`, `Filter`, `Projection`, `Join`, `Aggregate`, `Sort`, `Limit`, `SetOp`, `Subquery`, `Values`, `Empty` |
| `JoinType` | `Inner`, `Left`, `Right`, `Full`, `Cross`, `LeftSemi`, `LeftAnti` (the last two are produced by decorrelating correlated `EXISTS`/`IN`) |

### Optimizer

| Item | Description |
|------|-------------|
| `optimize(plan)` | Apply the default pass pipeline |
| `Optimizer` | Builder for a custom multi-pass optimizer |
| `PredicatePushdown` | Push WHERE filters below joins |
| `ProjectionPruning` | Drop unused column projections |
| `ConstantFolding` | Evaluate constant expressions at plan time |
| `PredicateSimplification` | Simplify boolean/range predicates to a fixpoint (`x AND TRUE → x`, `a>5 AND a>3 → a>5`, contradictions → `Empty`) |
| `CommonSubexprElimination` | Hoist identical subquery bodies — and repeated intra-expression subexpressions — into a shared binding |
| `LimitPushThrough` | Push LIMIT through projections |
| `JoinAlgorithmPass` | Annotate join nodes with an algorithm hint |
| `Optimizer::with_cost_model(model)` | Cost-aware pipeline adding statistics-driven join reordering (DPccp for ≤12 relations, greedy fallback above) |
| `OptPass` | Trait implemented by every pass |

### DML planning

| Item | Description |
|------|-------------|
| `plan_dml(stmt)` | INSERT / UPDATE / DELETE → `DmlPlan` |
| `DmlPlan` | `Insert`, `InsertSelect`, `Update`, `Upsert`, `Delete` |

### Cost model, validation, aggregates, windows, explain

| Item | Description |
|------|-------------|
| `CostModel` / `TableStats` / `CostEstimate` / `ColumnStats` | Estimate query cost from table statistics, optionally refined with per-column `{ ndv, null_fraction, min, max }` |
| `SchemaValidator` / `ValidationError` | Validate references against a schema (`TableNotFound`, `ColumnNotFound { table, column }`, `AmbiguousColumn`) |
| `extract_aggregates(expr)` / `AggFunc` | Aggregate extraction; `Count`, `Sum`, `Avg`, `Min`, `Max` |
| `WindowFunctionDef` | Captures any `OVER (PARTITION BY … ORDER BY …)` windowed call (`ROW_NUMBER`, `RANK`, `DENSE_RANK`, `LAG`, `LEAD`, `NTILE`, …) with its args/partition/order/alias |
| `explain(plan)` | Human-readable plan tree |
| `explain_verbose(plan, &CostModel)` | Same, with a `rows=…, cost=…` estimate annotated on every node |
| `explain_json(plan, Option<&CostModel>)` | Plan tree as JSON, with cost estimates when a `CostModel` is supplied |

### `ParseCache`

Thread-safe LRU cache keyed by `(sql, dialect)`.

| Method | Description |
|--------|-------------|
| `ParseCache::new(capacity)` | Create (capacity 0 is promoted to 1) |
| `parse(sql, dialect)` | Return cached result or parse and cache |
| `len()` / `is_empty()` / `clear()` | Inspect / evict |

### `PlanCache`

Thread-safe LRU cache of **optimized logical plans** — distinct from
`ParseCache` above, which caches raw parsed ASTs. Keyed by a
literal-normalized SQL template (via `parameterize`) plus a schema
generation counter, so two queries differing only in literal values
(`WHERE id = 1` vs `WHERE id = 2`) share one cache entry.

| Method | Description |
|--------|-------------|
| `PlanCache::new(capacity)` | Create (capacity 0 is promoted to 1) |
| `plan(sql)` | Parse + plan + optimize with default `PlannerOptions`, or return the cached `Arc<LogicalPlan>` |
| `plan_with(sql, opts)` | Same, with explicit `PlannerOptions` |
| `invalidate_schema()` | Bump the generation counter so every existing key misses and rebuilds |
| `len()` / `is_empty()` / `clear()` | Inspect / evict |

`parameterize(sql) -> ParameterizedSql` extracts the `{ template, literals }`
pair used as `PlanCache`'s cache key; it is also usable standalone for any
literal-normalization need.

```rust,no_run
use oxisql_parse::{explain, optimize, parse_one, plan_query};

let stmt = parse_one("SELECT id FROM users WHERE id = 1")?;
let plan = optimize(plan_query(&stmt));
println!("{}", explain(&plan));
# Ok::<(), oxisql_core::OxiSqlError>(())
```

Re-exports common `sqlparser` AST types (`Statement`, `Expr`, `SelectItem`,
`TableFactor`, `JoinConstraint`, …) for downstream backends.

## Feature flags

None.

## Test coverage

**191 tests** pass (178 unit + 13 doc).

## Part of the OxiSQL workspace

`oxisql-parse` is one of 18 crates in the OxiSQL workspace (11 facade/driver
crates plus a 7-crate C-free `oxisqlite-*` engine). See the
[workspace README](../../README.md) for the full architecture and the 2,261
workspace tests (2,755 with `--all-features`).

## License

Apache-2.0 — COOLJAPAN OU (Team Kitasan).
Copyright © 2024–2026 COOLJAPAN OU (Team Kitasan).
Repository: <https://github.com/cool-japan/oxisql>
