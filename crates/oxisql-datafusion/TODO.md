# oxisql-datafusion — TODO

**Status:** Alpha · v0.3.2 · 87 tests pass (4 ignored).

Apache DataFusion 53.x bridge exposing OxiSQL-backed tables to OLAP SQL. Ships a snapshot provider (`OxiSqlTableProvider`) and a live-streaming provider (`OxiSqlStreamProvider`), an `OxiSqlContext` `SessionContext` wrapper, filter/projection/limit/sort pushdown, multi-table catalog registration for cross-table joins, full `Value` ↔ Arrow type mapping, and range-based partitioning for parallel scans.

## Done

### Providers & context
- [x] `OxiSqlTableProvider` snapshot provider — `from_rows`, `from_connection`, `refresh`, `with_range_partition`.
- [x] `OxiSqlStreamProvider` live-streaming provider — `new`, `with_sort` (driving a real `Connection` at scan time, yielding batches incrementally).
- [x] `SortOrder::Asc` / `SortOrder::Desc` ordering for the streaming provider.
- [x] `OxiSqlContext` — `new`, `from_session_context`, `register_table`, `register_snapshot`, `register_view`, `deregister_table`, `execute_sql`, `sql` (→ `DataFrame`), `explain_plan`, `register_scalar_function`, `register_aggregate_function`, `register_parquet`, `session_context` / `into_session_context`.
- [x] Free functions `register_oxisql_table` and `register_embedded_table`.
- [x] Facade entry point: `oxisql::connect_datafusion("datafusion://")` (and the `memory://` alias) → `OxiSqlContext`.

### Pushdown & types
- [x] Filter pushdown — binary comparisons (`=`, `<>`, `<`, `<=`, `>`, `>=`) and `IS [NOT] NULL` → SQL `WHERE`; snapshot filters reported as `Inexact`.
- [x] Projection pushdown — `SELECT` only the requested columns.
- [x] Limit pushdown — append `LIMIT N`.
- [x] Sort pushdown — append `ORDER BY` (streaming provider).
- [x] Full `Value` ↔ Arrow mapping for all 13 variants (Null, Bool, I64, F64, Text, Blob, Timestamp, Date, Time, Uuid, Json, Decimal, Array).
- [x] `TableProvider::statistics()` reporting (row count and column-level stats).

### Parallelism & catalog
- [x] Range-based partitioning (`with_range_partition`) for parallel scans.
- [x] Multi-table catalog registration in a single `SessionContext` for cross-table joins.

### Features
- [x] `parse` — `plan_bridge` (`sql_to_datafusion_plan` / `to_datafusion_plan`): converts `oxisql_parse::LogicalPlan` → DataFusion `LogicalPlan` (Scan/Filter/Project/Limit/Empty structurally via `parse_sql_expr`; other nodes via SQL round-trip).
- [x] `columnar` — `ParquetTableProvider` over `oxistore-columnar`.
- [x] `mysql` / `postgres` / `sqlite` — optional backend wiring for cross-backend testing.

### Errors & quality
- [x] `OxiSqlFusionError` — `OxiSql`, `Arrow`, `DataFusion`, `SchemaMismatch`, `UnsupportedType` variants.
- [x] 83 tests — live streaming, filter/projection/limit pushdown, multi-table JOIN, aggregation, window functions, range partition, NULL handling, schema-mismatch detection, extended types, structural plan_bridge Filter+Project lowering.
- [x] Criterion benchmarks (`benches/datafusion_benchmarks.rs`) — snapshot register+plan+execute, filter-pushdown speedup, RecordBatch construction, multi-partition scan.

## Roadmap / next
- [~] Promote from **Alpha** to **Beta**: stabilise the provider/context API and reduce SQL round-trips in `plan_bridge`.
- [x] Push more `plan_bridge` node types down structurally (Projection, Filter, Aggregate, Join) instead of round-tripping through SQL. (done 2026-06-10)
  - **Goal:** `Filter` and `Project` oxisql-parse nodes lower to native DataFusion `LogicalPlan` nodes instead of emitting SQL; SQL round-trip remains as fallback for other shapes.
  - **Design:** In `src/plan_bridge.rs` (behind the existing `#[cfg(feature = "parse")]`): after building the structural input DF plan, use DataFusion 53.1's `SessionContext::parse_sql_expr(sql_str, &DFSchema)` (gated by the already-enabled `sql` feature) to parse each string predicate/column into a DataFusion `Expr`. Then apply `LogicalPlanBuilder::from(input).filter(expr)?.build()` (for Filter) and `LogicalPlanBuilder::from(input).project(exprs)?.build()` (for Project). Special-case wildcard `*` in column lists → skip structural lowering for that node, fall through to the SQL round-trip. Keep `sql_to_datafusion_plan` as the fallback for Join/Aggregate/Window/SetOp/CTE/subqueries and for any `parse_sql_expr` error. Implement as a "try-structural, catch-fallback" wrapper.
  - **Files:** `src/plan_bridge.rs`
  - **Tests:** Filter plan → DF Filter node (assert via DF plan display); Project plan → DF Project node; both produce identical rows to the SQL round-trip on an in-memory table; wildcard `SELECT *` falls back cleanly; unsupported shape (Aggregate) falls back cleanly. In-memory data only.
  - **Risk:** Medium — `parse_sql_expr` requires a `DFSchema` from the input; schema must be resolved before the call. Wildcard and unresolved column names are the main edge cases. The SQL round-trip fallback makes failures safe.
- [x] Pushdown for `IN`, `BETWEEN`, `LIKE`, and conjunctive/disjunctive predicate trees. (done 2026-06-10)
  - **Goal:** Both the streaming and snapshot provider paths handle `Expr::InList`, `Expr::Between`, `Expr::Like`, and recursive `AND`/`OR`/`NOT` predicate trees.
  - **Design:** **Streaming path** (`stream.rs:389-449`, `can_push_filter`/`expr_to_sql`): already handles AND/OR/NOT. Add arms for `Expr::InList { expr, list, negated }` → SQL `x IN (a,b,c)` / `x NOT IN (...)`; `Expr::Between { expr, low, high, negated }` → `x BETWEEN low AND high` / `x NOT BETWEEN`; `Expr::Like { expr, pattern, escape_char, negated, case_insensitive }` → SQL `x [I]LIKE 'p'` / `x NOT [I]LIKE 'p'`. **Snapshot path** (`provider.rs:182-281`, `is_simple_filter`/`eval_filter_on_row`): currently handles only 6 binary comparisons + IS [NOT] NULL. Add `And`/`Or`/`Not` recursive arms to both `is_simple_filter` and `eval_filter_on_row`; add `InList`, `Between`, `Like` evaluation against `oxisql_core::Value` using its existing comparison semantics.
  - **Files:** `src/stream.rs` (+InList/Between/Like arms in can_push_filter + expr_to_sql), `src/provider.rs` (+And/Or/Not recursion + InList/Between/Like eval in is_simple_filter + eval_filter_on_row)
  - **Tests:** streaming: IN list, BETWEEN range, LIKE pattern, NOT IN, NOT BETWEEN, NOT LIKE; snapshot: AND/OR/NOT combinations, IN/BETWEEN/LIKE eval; proptest: streaming SQL and snapshot result agree for same predicate + random rows
  - **Risk:** Like evaluation against `oxisql_core::Value::Text` — implement `%`/`_` wildcard matching with proper SQL semantics. ILIKE (case-insensitive) handled by lowercasing both sides.
- [x] Hash partitioning in addition to range partitioning. (done 2026-06-10)
  - **Goal:** `OxiSqlTableProvider::with_hash_partition(key_column, n)` distributes rows into n buckets by key hash; `scan()` consumes partitions generically (already does so — purely additive).
  - **Design:** `with_range_partition` at `provider.rs:135-169` sorts rows by key column and fills `self.partitions: Vec<Arc<Vec<Row>>>`. Add sibling `with_hash_partition(key_column: &str, n: usize) -> Result<Self>`: for each row, compute `hash(row[key_column]) % n` and append to `partitions[bucket]`. Hash over `oxisql_core::Value`: match each variant and hash its repr (I64 via its bits, F64 via its bits, Text via bytes, Blob via bytes, Null as a fixed sentinel, UUID as its u128, etc.) using `std::hash::DefaultHasher` or a simple FNV. `scan()` at `provider.rs:335-385` consumes `self.partitions` generically — no change.
  - **Files:** `src/provider.rs` (+with_hash_partition method)
  - **Tests:** n=3 partitions cover all rows (union = original, no duplication); uniform-ish distribution assertion (no empty bucket for >n rows); hash stability across calls; scan over hash partitions returns all rows
  - **Risk:** `Value::F64` hashing: NaN != NaN in IEEE 754 but should hash consistently — treat NaN as a canonical bit pattern. Null hashes consistently via a fixed constant.
- [x] Statistics-driven partition sizing for the streaming provider. (`with_auto_partition` added to both `OxiSqlTableProvider` and `OxiSqlStreamProvider`; done 2026-06-10)
- [ ] Richer Arrow type coverage (e.g. nested/struct columns) as upstream support firms up.

## Known limitations
- **Alpha.** APIs may shift before the first stable release.
- Some logical-plan operators in the `parse` bridge fall back to a SQL round-trip rather than a native DataFusion plan node (only Scan/Limit/Empty are mapped structurally today).
- Snapshot-provider filters are `Inexact`: DataFusion re-applies its own post-filter, so a pushed predicate is an optimisation, not the sole correctness guarantee.
- The `mysql` / `postgres` integration tests require live servers and are ignored by default.
