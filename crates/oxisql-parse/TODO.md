# oxisql-parse TODO

## Status
Minimal SQL parsing utility crate. Re-exports `sqlparser::ast::Statement` and `GenericDialect`. Provides a single `parse(sql)` function wrapping `sqlparser::Parser::parse_sql` with `OxiSqlError` conversion. ~26 SLOC.

## Core Implementation
- [x] Add SQL query planner — transform parsed AST into a logical plan tree (Scan, Filter, Project, Join, Aggregate, Sort, Limit) (~200 SLOC)
- [x] Add query optimizer — implement rule-based optimization passes: predicate pushdown, projection pruning, constant folding, join reordering (~250 SLOC)
- [x] Add join algorithm selection — hash join for equi-joins, merge join for sorted inputs, nested-loop join as fallback — `JoinAlgorithmPass` in `optimizer/join_algo.rs` (~80 SLOC)
- [x] Add aggregate function support — COUNT, SUM, AVG, MIN, MAX, GROUP BY with HAVING clause — `src/agg.rs` with `AggregateExpr`, `AggFunc`, `extract_aggregates`, `is_aggregate_expr`; HAVING produces `Filter` over `Aggregate` in the planner (~170 SLOC, 6 tests)
- [x] Add window function support — ROW_NUMBER, RANK, DENSE_RANK, LAG, LEAD, NTILE with OVER(PARTITION BY ... ORDER BY ...) — `src/window.rs`
- [x] Add subquery support — scalar subqueries, EXISTS, IN, correlated subqueries with decorrelation — `src/lib.rs`
- [x] Add CTE (Common Table Expressions) support — WITH clause, recursive CTEs — `src/lib.rs`
- [x] Add UNION / INTERSECT / EXCEPT set operations — `src/setops.rs` with `SetOpType` and `plan_set_operation` (~40 SLOC)
- [x] Add INSERT ... SELECT support — `DmlPlan::InsertSelect` in `dml.rs` (~60 SLOC)
- [x] Add UPDATE … FROM support (Postgres-style multi-table update) — `DmlPlan::Update.from_table: Option<String>` populated from `UpdateTableFromKind` in `dml.rs` (~25 SLOC, 1 test)
- [x] Add UPSERT / INSERT ... ON CONFLICT (Postgres) / INSERT ... ON DUPLICATE KEY UPDATE (MySQL) — `DmlPlan::Upsert` in `dml.rs` (~60 SLOC)
- [x] Add dialect-specific parser wrappers — `parse_postgres(sql)`, `parse_mysql(sql)`, `parse_sqlite(sql)` using `sqlparser`'s dialect types (~25 SLOC)
- [x] Add SQL normalization — canonicalize whitespace, case, and identifier quoting for cache key generation (~30 SLOC)
- [x] Add parameter extraction — identify `$1`/`?` placeholders in parsed AST and return their count/positions (~25 SLOC)
- [x] Add SQL validation — check semantic correctness beyond syntax (column existence, type compatibility) given a schema — `src/validate.rs`
- [x] Add query plan visualization — `explain(plan)` returning a human-readable tree string (~30 SLOC)
- [x] Add cost model — estimate query cost based on table statistics (row count, column cardinality, index availability) — `src/cost.rs`

## API Improvements
- [x] Re-export additional sqlparser AST types commonly needed by backends (Expr, SelectItem, TableFactor, JoinConstraint) (~10 SLOC)
- [x] Add `parse_one(sql)` convenience function that returns a single `Statement` and errors if multiple statements found (~10 SLOC)
- [x] Add `format(statement)` — convert a parsed AST back to SQL text (~10 SLOC using sqlparser's Display)
- [x] Add dialect enum `SqlDialect { Generic, Postgres, MySQL, SQLite }` for explicit dialect selection (~15 SLOC)
- [x] Add `extract_tables(statement)` — return all table names referenced by a statement (~20 SLOC)
- [x] Add `extract_columns(statement)` — return all column references in a statement (~25 SLOC)
- [x] Add `is_read_only(statement)` — classify whether a statement modifies data (~10 SLOC)
- [x] Add fluent `QueryBuilder` — SELECT (with JOIN/WHERE/GROUP BY/HAVING/ORDER BY/LIMIT/OFFSET/DISTINCT), INSERT, UPDATE, DELETE with `build()` / `build_and_parse()` / `build_ref()` (~150 SLOC, 15 tests)

## Testing
- [x] Test parsing all SQL statement types — SELECT, INSERT, UPDATE, DELETE, CREATE TABLE, DROP, ALTER (~30 SLOC)
- [x] Test parsing with positional parameters ($1, $2, ?) (~15 SLOC)
- [x] Test parsing complex queries — multi-table JOINs, subqueries, CTEs, window functions (~25 SLOC)
- [x] Test error messages for malformed SQL — verify `OxiSqlError::Parse` contains useful context (~15 SLOC)
- [x] Test dialect-specific syntax — Postgres `::` cast, MySQL backtick identifiers, SQLite `AUTOINCREMENT` (~20 SLOC)
- [x] Test query planner correctness — verify logical plan structure for representative queries (~40 SLOC)
- [x] Test optimizer passes — verify predicate pushdown, join algo hint, DML planning (~30 SLOC)
- [x] Benchmark parse throughput for queries of varying complexity — `benches/parse_benchmarks.rs::bench_parse_throughput` group: simple_select, complex_join, insert_values, aggregate_groupby, cte_window_function (~40 SLOC)

## Performance
- [x] Cache parsed ASTs for repeated identical queries — `LruCache<String, Vec<Statement>>` (~20 SLOC)
- [x] Benchmark query planning overhead for simple vs complex queries — `benches/parse_benchmarks.rs::bench_plan_overhead` group: plan_simple_select, plan_join_with_limit, plan_aggregate_having, parse_and_plan_simple (~35 SLOC)
- [x] Benchmark optimizer pass chain — measure time per optimization rule — `benches/parse_benchmarks.rs::bench_optimizer_chain` group: optimize_simple_select/join/aggregate, full_pipeline_simple/complex (~40 SLOC)

## Integration
- [x] Integrate query planner with `oxisql-embedded` — `normalize_sql` and `is_read_only_sql` static methods added to `EmbeddedConnection` using `oxisql_parse::normalize` and `is_read_only`; full GlueSQL planner replacement deferred (~50 SLOC)
- [x] Integrate query planner with `oxisql-datafusion` — convert logical plan to DataFusion `LogicalPlan` via `plan_bridge` module in `oxisql-datafusion` (Scan/Limit/Empty structural; Filter/Project/Sort/Aggregate/Join via SQL round-trip) (~120 SLOC)
- [x] Ensure parsed AST is compatible with `oxisql-postgres` and `oxisql-mysql` query rewriting needs (~15 SLOC)
- [x] Use `oxisql-parse` normalization in connection pooling for prepared statement deduplication (~15 SLOC)
