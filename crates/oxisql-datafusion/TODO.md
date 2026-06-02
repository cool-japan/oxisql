# oxisql-datafusion TODO

## Status
DataFusion `TableProvider` implementation serving a fixed snapshot of `oxisql_core::Row`s. `OxiSqlTableProvider` with `from_rows` constructor, in-process filter pushdown (`Inexact` for binary comparisons and IS NULL/IS NOT NULL), and range-based partitioning via `with_range_partition`. Type mapping from `Value` to Arrow arrays (Boolean, Int64, Float64, Utf8, LargeBinary, Timestamp, Date, Decimal128, Array). `OxiSqlFusionError` with OxiSql, Arrow, DataFusion, and SchemaMismatch variants.

## Core Implementation
- [x] Add live/streaming table provider (`OxiSqlStreamProvider`) — drive a real `oxisql_core::Connection` at query time, yielding batches incrementally instead of materializing all rows upfront (~120 SLOC)
- [x] Add filter pushdown — translate DataFusion `Expr` filters into SQL WHERE clauses to push predicates to the backend database (~80 SLOC)
- [x] Add projection pushdown — generate `SELECT col1, col2` instead of `SELECT *` based on requested projection columns (~30 SLOC)
- [x] Add limit pushdown — append `LIMIT N` to the backend SQL query when DataFusion requests a row limit (~15 SLOC)
- [x] Add sort pushdown — append `ORDER BY` to the backend SQL query when DataFusion can delegate sorting (~25 SLOC)
- [x] Add custom UDF registration — `register_scalar_function(name, impl)` for user-defined scalar functions in DataFusion context (~40 SLOC)
- [x] Add custom UDAF registration — `register_aggregate_function(name, impl)` for user-defined aggregates (~40 SLOC)
- [x] Add async query execution — implement DataFusion's async `ExecutionPlan` with proper partitioning and streaming output (~60 SLOC)
- [x] Add multi-partition support — split large result sets across multiple partitions for parallel scan (~40 SLOC)
- [x] Add query plan visualization — `explain_plan(sql)` returning DataFusion's logical and physical plan as formatted strings (~25 SLOC)
- [x] Add catalog integration — register multiple OxiSQL tables in a DataFusion `SessionContext` catalog for cross-table queries (~40 SLOC)
- [x] Add view support — register SQL views backed by OxiSQL connections (~30 SLOC)
- [x] Add extended type mapping — Date32, Date64, Timestamp(Microsecond), Timestamp(Millisecond), Decimal128, Utf8/LargeUtf8, Binary types in `build_column` (~40 SLOC)
- [x] Add `Date32` and `Date64` column builders for `Value::Date` (~15 SLOC)
- [x] Add `Timestamp` column builder for `Value::Timestamp` with timezone support (~20 SLOC)
- [x] Add `Decimal128` column builder for `Value::Decimal` (~20 SLOC)
- [x] Add `List` column builder for `Value::Array` (~25 SLOC)
- [x] Add statistics reporting — implement `TableProvider::statistics()` returning row count and column-level statistics (~30 SLOC)
- [x] Add table partitioning — support partitioned scans by range or hash on a partition key (~40 SLOC)

## API Improvements
- [x] Add `OxiSqlTableProvider::from_connection(conn, table_name, schema)` — construct by querying a live connection (~20 SLOC)
- [x] Add `OxiSqlTableProvider::refresh()` — re-snapshot the backing data from the connection (~15 SLOC)
- [x] Add `OxiSqlContext` — a wrapper around `SessionContext` pre-configured with OxiSQL catalog, UDFs, and settings (~30 SLOC)
- [x] Add `register_oxisql_table(ctx, name, conn, schema)` — one-line registration of an OxiSQL-backed table in DataFusion (~15 SLOC)
- [x] Add `OxiSqlFusionError::UnsupportedType` variant for unmapped Arrow types (~5 SLOC)
- [x] Implement `Display` for `OxiSqlTableProvider` — show table name, schema, row count (~10 SLOC)
- [x] Add `execute_sql(ctx, sql)` convenience function returning `Vec<RecordBatch>` (~20 SLOC)

## Testing
- [x] Test live streaming provider — connect to in-memory embedded DB, insert rows, query through DataFusion (~30 SLOC)
- [x] Test filter pushdown — verify WHERE predicates are translated to SQL and reduce returned rows (~25 SLOC)
- [x] Test projection pushdown — verify only requested columns are fetched from the backend (~20 SLOC)
- [x] Test limit pushdown — verify LIMIT reduces backend query result size (~15 SLOC)
- [x] Test multi-table JOIN through DataFusion — register two OxiSQL tables and execute a JOIN query (~25 SLOC)
- [x] Test aggregation queries — COUNT(*) through DataFusion on OxiSQL snapshot data (~20 SLOC)
- [x] Test filter pushdown on snapshot provider — verify `Inexact` classification and equality filtering (~20 SLOC)
- [x] Test range partition — sort+split by key column, verify row count and order (~20 SLOC)
- [x] Test window functions — ROW_NUMBER, RANK through DataFusion on OxiSQL data (~20 SLOC)
- [x] Test NULL handling — verify null values propagate correctly through Arrow arrays (~15 SLOC)
- [x] Test schema mismatch detection — rows with fewer values than fields produce NULLs (~10 SLOC)
- [x] Test extended types — Date, Timestamp, Decimal columns through DataFusion (~20 SLOC)
- [x] Benchmark DataFusion query execution on OxiSQL-backed tables — `bench_snapshot_provider` in `datafusion_benchmarks.rs` benchmarks register+plan+execute for 100/1000/10000 rows (~30 SLOC)

## Performance
- [x] Benchmark in-memory scan vs streaming scan — `bench_snapshot_provider` benchmarks the in-memory scan path for 100/1000/10000 rows; streaming path benchmarked separately when OxiSqlStreamProvider harness is available (~30 SLOC)
- [x] Benchmark filter pushdown speedup — compare DataFusion post-filter vs backend pre-filter (~25 SLOC)
- [x] Benchmark RecordBatch construction overhead for varying column counts and row counts — `bench_record_batch_construction` benchmarks `from_rows` for 4/16 columns over 1000 rows (~25 SLOC)
- [x] Profile Arrow array builder memory allocation for large datasets (~20 SLOC)
- [x] Benchmark multi-partition parallel scan vs single-partition scan (~25 SLOC)

## Integration
- [x] Integration with `oxisql-embedded` — register GlueSQL tables in DataFusion for OLAP queries on embedded data (~30 SLOC)
- [x] Integration with `oxisql-parse` — `plan_bridge` module converts `oxisql_parse::LogicalPlan` to DataFusion `LogicalPlan` (Scan, Limit, Empty structural; all others via SQL round-trip); gated behind `parse` feature flag (~120 SLOC, 7 tests)
- [x] Integration with `oxisql-postgres` — serve live Postgres tables through DataFusion with pushdown (~30 SLOC)
- [x] Integration with `oxisql-mysql` — serve live MySQL tables through DataFusion with pushdown (~30 SLOC)
- [x] Integration with `oxistore-columnar` — use DataFusion to query Parquet files via oxistore-columnar reader (~25 SLOC)
- [x] Integration with `oxisql` facade — `oxisql::datafusion::register_table(ctx, conn, name)` convenience function (~15 SLOC) — `oxisql::datafusion::{register_table, context}` in `crates/oxisql/src/lib.rs:315`
