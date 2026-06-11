# oxisql-parse — SQL parsing, planning, and optimization for OxiSQL

[![Crates.io](https://img.shields.io/crates/v/oxisql-parse.svg)](https://crates.io/crates/oxisql-parse)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](https://www.apache.org/licenses/LICENSE-2.0)

> sqlparser facade with dialect-aware parsing, a fluent query builder, logical planning, a rule-based optimizer, and an LRU parse cache.

## What it is

`oxisql-parse` wraps [`sqlparser`](https://crates.io/crates/sqlparser) with
OxiSQL-idiomatic APIs: dialect-aware parsing, a fluent `QueryBuilder`, a
logical planner, a rule-based optimizer, DML planning, a cost model, a schema
validator, aggregate / window helpers, an `explain` pretty-printer, and a
thread-safe LRU parse cache. It re-exports the most commonly needed
`sqlparser` AST types. The crate is Pure Rust and has **no feature flags**.

## Installation

```toml
[dependencies]
oxisql-parse = "0.1.2"
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
| `build()` / `build_and_parse()` | Produce SQL / build and validate via sqlparser |

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
| `plan_query(stmt)` / `plan_statement(stmt)` | Parsed `Statement` → `LogicalPlan` |
| `LogicalPlan` | `Scan`, `Filter`, `Projection`, `Join`, `Aggregate`, `Sort`, `Limit`, `SetOp`, `Subquery`, `Values`, `Empty` |
| `JoinType` | `Inner`, `Left`, `Right`, `Full`, `Cross` |

### Optimizer

| Item | Description |
|------|-------------|
| `optimize(plan)` | Apply the default pass pipeline |
| `Optimizer` | Builder for a custom multi-pass optimizer |
| `PredicatePushdown` | Push WHERE filters below joins |
| `ProjectionPruning` | Drop unused column projections |
| `ConstantFolding` | Evaluate constant expressions at plan time |
| `LimitPushThrough` | Push LIMIT through projections |
| `JoinAlgorithmPass` | Annotate join nodes with an algorithm hint |
| `OptPass` | Trait implemented by every pass |

### DML planning

| Item | Description |
|------|-------------|
| `plan_dml(stmt)` | INSERT / UPDATE / DELETE → `DmlPlan` |
| `DmlPlan` | `Insert`, `InsertSelect`, `Update`, `Upsert`, `Delete` |

### Cost model, validation, aggregates, windows, explain

| Item | Description |
|------|-------------|
| `CostModel` / `TableStats` / `CostEstimate` | Estimate query cost from table statistics |
| `SchemaValidator` / `ValidationError` | Validate references against a schema (`UnknownTable`, `UnknownColumn`, `AmbiguousColumn`) |
| `extract_aggregates(expr)` / `AggFunc` | Aggregate extraction; `Count`, `Sum`, `Avg`, `Min`, `Max` |
| `window` module | `ROW_NUMBER`, `RANK`, `DENSE_RANK`, `LAG`, `LEAD`, `NTILE` |
| `explain(plan, verbose)` | Human-readable plan tree |

### `ParseCache`

Thread-safe LRU cache keyed by `(sql, dialect)`.

| Method | Description |
|--------|-------------|
| `ParseCache::new(capacity)` | Create (capacity 0 is promoted to 1) |
| `parse(sql, dialect)` | Return cached result or parse and cache |
| `len()` / `is_empty()` / `clear()` | Inspect / evict |

```rust,no_run
use oxisql_parse::{plan_query, optimize, explain, parse_one};

let stmt = parse_one("SELECT id FROM users WHERE id = 1")?;
let plan = optimize(plan_query(&stmt));
println!("{}", explain(&plan, false)); // false = non-verbose
# Ok::<(), oxisql_core::OxiSqlError>(())
```

Re-exports common `sqlparser` AST types (`Statement`, `Expr`, `SelectItem`,
`TableFactor`, `JoinConstraint`, …) for downstream backends.

## Feature flags

None.

## Test coverage

**129 tests** pass.

## Part of the OxiSQL workspace

`oxisql-parse` is one of 17 crates in the OxiSQL workspace (10 facade/driver
crates plus a 7-crate C-free `oxisqlite-*` engine). See the
[workspace README](../../README.md) for the full architecture and the 1,720
workspace tests.

## License

Apache-2.0 — COOLJAPAN OU (Team Kitasan).
Copyright © 2024–2026 COOLJAPAN OU (Team Kitasan).
Repository: <https://github.com/cool-japan/oxisql>
