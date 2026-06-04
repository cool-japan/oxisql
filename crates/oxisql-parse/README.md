# oxisql-parse — SQL parsing utilities for OxiSQL

[![Crates.io](https://img.shields.io/crates/v/oxisql-parse.svg)](https://crates.io/crates/oxisql-parse)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

`oxisql-parse` provides SQL parsing, normalization, query building, logical planning, optimization, and an LRU parse cache. It re-exports the most commonly needed `sqlparser` AST types and wraps them with OxiSQL-idiomatic APIs.

## Installation

```toml
[dependencies]
oxisql-parse = "0.1.1"
```

## Quick Start

```rust
use oxisql_parse::{parse, parse_one, is_read_only, QueryBuilder, ParseCache, SqlDialect};

// Parse multiple statements
let stmts = parse("SELECT 1; SELECT 2").unwrap();
assert_eq!(stmts.len(), 2);

// Parse a single statement
let stmt = parse_one("SELECT 42").unwrap();
assert!(is_read_only(&stmt));

// Fluent query builder
let sql = QueryBuilder::select(&["id", "name", "email"])
    .from("users")
    .join("orders", "users.id = orders.user_id")
    .where_clause("users.active = TRUE")
    .order_by("users.name", true)
    .limit(10)
    .offset(20)
    .build()
    .expect("valid query");

// LRU parse cache
let cache = ParseCache::new(32);
let stmts = cache.parse("SELECT 1", SqlDialect::Generic).unwrap();
assert_eq!(cache.len(), 1);
```

## API Overview

### Parsing functions

| Function | Description |
|----------|-------------|
| `parse(sql)` | Parse SQL using the generic dialect, return `Vec<Statement>` |
| `parse_one(sql)` | Parse exactly one statement, error if 0 or 2+ |
| `parse_with_dialect(sql, dialect)` | Parse with a specific `SqlDialect` |
| `parse_one_with_dialect(sql, dialect)` | Single-statement parse with dialect |
| `parse_postgres(sql)` | Shorthand for PostgreSQL dialect |
| `parse_mysql(sql)` | Shorthand for MySQL dialect |
| `parse_sqlite(sql)` | Shorthand for SQLite dialect |
| `format(stmt)` | Serialize a parsed `Statement` back to SQL text |

### `SqlDialect` enum

`Generic`, `Postgres`, `MySQL`, `SQLite` — selects the sqlparser dialect for parsing.

### Analysis utilities (from `analysis` module)

| Function | Description |
|----------|-------------|
| `is_read_only(stmt)` | True if statement is a SELECT (no side effects) |
| `normalize(sql)` | Normalize whitespace and casing |
| `extract_tables(stmt)` | Extract referenced table names |
| `extract_columns(stmt)` | Extract referenced column names |
| `count_params(sql)` | Count `$N` positional parameters in SQL text |

### `QueryBuilder`

Fluent builder for SELECT statements. All methods return `Self` for chaining.

| Method | Description |
|--------|-------------|
| `QueryBuilder::select(columns)` | Start a SELECT with given columns |
| `QueryBuilder::select_all()` | Start a `SELECT *` |
| `.distinct()` | Add `DISTINCT` |
| `.from(table)` | Set the FROM table |
| `.join(table, on)` | Add `INNER JOIN` |
| `.left_join(table, on)` | Add `LEFT JOIN` |
| `.right_join(table, on)` | Add `RIGHT JOIN` |
| `.where_clause(condition)` | Add a WHERE condition (multiple → AND) |
| `.group_by(expr)` | Add a GROUP BY expression |
| `.having(condition)` | Set the HAVING clause |
| `.order_by(expr, ascending)` | Add ORDER BY (true=ASC, false=DESC) |
| `.limit(n)` | Set LIMIT |
| `.offset(n)` | Set OFFSET |
| `.build()` | Produce the SQL string |
| `.build_and_parse()` | Build and immediately validate with sqlparser |
| `.build_ref()` | Build without consuming `self` |

Static DML helpers (return `String` directly):

```rust
QueryBuilder::insert("users", &["id", "name"], &["1", "'Alice'"])
// → "INSERT INTO users (id, name) VALUES (1, 'Alice')"

QueryBuilder::update("users", &[("name", "'Bob'")], Some("id = 42"))
// → "UPDATE users SET name = 'Bob' WHERE id = 42"

QueryBuilder::delete("users", Some("id = 99"))
// → "DELETE FROM users WHERE id = 99"
```

### `ParseCache`

Thread-safe LRU cache for parsed SQL ASTs. Cache key is `(sql_text, SqlDialect)`.

| Method | Description |
|--------|-------------|
| `ParseCache::new(capacity)` | Create cache; capacity 0 is promoted to 1 |
| `cache.parse(sql, dialect)` | Return cached result or parse and cache |
| `cache.len()` | Number of cached entries |
| `cache.is_empty()` | True when no entries cached |
| `cache.clear()` | Evict all entries |

### Logical planner

| Function / Type | Description |
|-----------------|-------------|
| `plan_query(stmt)` | Convert a parsed `Statement` into `LogicalPlan` |
| `plan_statement(stmt)` | Alias for `plan_query` |
| `LogicalPlan` | Enum: `Scan`, `Filter`, `Projection`, `Join`, `Aggregate`, `Sort`, `Limit`, `SetOp`, `Subquery`, `Values`, `Empty` |
| `JoinType` | `Inner`, `Left`, `Right`, `Full`, `Cross` |
| `SortExpr` | `{ expr, ascending, nulls_first }` |

### Optimizer

| Type / Function | Description |
|-----------------|-------------|
| `optimize(plan)` | Apply the default four-pass optimizer |
| `Optimizer::new()` | Build a custom multi-pass optimizer |
| `PredicatePushdown` | Push WHERE filters below joins (predicate pushdown) |
| `ProjectionPruning` | Remove unused column projections |
| `ConstantFolding` | Evaluate constant expressions at plan time |
| `LimitPushThrough` | Push LIMIT through projections |
| `JoinAlgorithmPass` | Apply `JoinAlgoHint` to join nodes |
| `OptPass` | Trait implemented by all optimization passes |

### DML planning

| Type / Function | Description |
|-----------------|-------------|
| `plan_dml(stmt)` | Convert INSERT/UPDATE/DELETE to `DmlPlan` |
| `DmlPlan` | `Insert { table, columns, values }`, `Update { ... }`, `Delete { ... }` |

### Cost model

| Type | Description |
|------|-------------|
| `CostModel` | Estimates query cost from a `TableStats` map |
| `TableStats` | `{ row_count, avg_row_bytes, index_names }` |
| `CostEstimate` | `{ total_cost, estimated_rows }` |

### Schema validator

| Type | Description |
|------|-------------|
| `SchemaValidator` | Validates column/table references against a schema map |
| `ValidationError` | `UnknownTable`, `UnknownColumn`, `AmbiguousColumn` |

### Aggregate helpers

| Function / Type | Description |
|-----------------|-------------|
| `extract_aggregates(expr)` | Extract aggregate expressions from an AST node |
| `is_aggregate_expr(expr)` | True if expression contains an aggregate call |
| `AggFunc` | `Count`, `Sum`, `Avg`, `Min`, `Max` |
| `AggregateExpr` | `{ func, arg, alias }` |

### Explain

```rust
use oxisql_parse::{plan_query, explain, parse_one};
let stmt = parse_one("SELECT id FROM users WHERE id = 1").unwrap();
let plan = plan_query(&stmt);
let text = explain(&plan, false); // false = no verbose output
```

## Test Status

As of 2026-05-30: **118 tests passing**.

## License

Apache-2.0 — COOLJAPAN OU (Team Kitasan)
