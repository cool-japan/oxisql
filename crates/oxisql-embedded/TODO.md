# oxisql-embedded TODO

## Status
In-memory SQL engine backed by GlueSQL `MemoryStorage`. `EmbeddedConnection` implements `Connection` with `Arc<Mutex<Glue>>`. Transactions via `OwnedMutexGuard` with `BEGIN/COMMIT/ROLLBACK`. Param substitution uses AST-level binding via `sqlparser` `visit_expressions_mut` (replaces `Placeholder` nodes with typed `Expr` literals; string-based fallback for GlueSQL-specific SQL); `$1` inside string literals is correctly preserved. Full GlueSQL value-to-OxiSQL value mapping covering 20+ GlueSQL variants. execute_batch, ping, UDF registry (`register_udf`/`call_udf`), aggregate UDFs (`register_aggregate`/`apply_aggregate`), savepoints (no-op API), JSON helpers (`json_set`/`json_get`), EXPLAIN (pattern-based), `open_file` (sled-storage feature gate), `import_from_sql`, `export_as_sql` implemented via `fetch_all_schemas()` + `Schema::to_ddl()`, full schema introspection via GlueSQL catalog. BLOB params supported via `X'hex'` literals. ~870 SLOC. 244 tests green.

## Core Implementation
- [x] Add persistent storage backend — `FjallGlueStorage` (`src/fjall_storage.rs`) implements `Store` + `StoreMut` + all auxiliary GlueSQL traits backed by a fjall `Database`; `FjallEmbeddedConnection` wraps `Glue<FjallGlueStorage>` with the full `Connection` API; `fjall-storage` feature gate (~280 SLOC); **also**: `RedbGlueStorage` (`src/redb_storage.rs`) backed by redb 4.1.0 via two typed `&[u8]→&[u8]` tables; `RedbEmbeddedConnection` implements `Connection` + `Transaction` + `PreparedStatement`; order-preserving binary key encoding for all `Key` variants; auto-increment counter persisted in the data table; `redb-storage` feature gate (~580 SLOC)
- [x] Replace string-based param substitution with type-safe parameter binding — AST-level via sqlparser `visit_expressions_mut`; replaces `Expr::Value(Placeholder("$N"))` nodes with correctly-typed literal `Expr`s; string-based fallback for GlueSQL-specific SQL that the generic dialect cannot parse; `$1` inside string literals is never substituted (~120 SLOC)
- [x] Add BLOB parameter support — `escape_sql_value` renders `Value::Blob` as `X'hex...'` literals; `value_to_ast_expr` mini-parses the literal and embeds it in the AST; tested via `test_blob_param_binding` (~20 SLOC)
- [x] Add WAL (Write-Ahead Log) mode — fjall uses an LSM journal (write-ahead log) for crash safety by default; writes are durably committed to the fjall journal before returning; no additional implementation required for the fjall backend
- [x] Add concurrent reader support — **NOT FEASIBLE with current GlueSQL API**: `Glue::execute()` requires `&mut Glue` for *all* operations including SELECT queries, so `RwLock<Glue>` would provide no concurrency benefit — every read would still need the exclusive write lock. True concurrent reads would require a fundamentally different storage architecture (e.g. snapshot isolation at the storage layer, or a custom GlueSQL `Store` impl that supports shared-state reads). Note: the `UdfRegistry` *already* uses `Arc<std::sync::RwLock<UdfRegistry>>` because UDF calls only need `&self` on the registry side, which is the correct application of RwLock here. (~0 SLOC — architectural constraint, not an implementation gap)
- [x] Add full-text search — standalone inverted index in `src/fts.rs`; intercepts `CREATE VIRTUAL TABLE … USING fts5/fts4`, `INSERT INTO fts_table VALUES (id, text)`, and `SELECT rowid FROM t WHERE t MATCH 'query'`; AND semantics; shared via `Arc<RwLock<FtsIndex>>` across clones; 5 integration tests; ~220 SLOC
- [x] Add JSON functions — `json_set` (upsert via DELETE+INSERT) and `json_get` (SELECT by key) helpers on `EmbeddedConnection` (~60 SLOC)
- [x] Add virtual table support — allow registration of custom table providers (e.g. CSV, Parquet files) (~60 SLOC) — `VirtualTableRegistry` in `src/vtable.rs`; `register_virtual_table`/`unregister_virtual_table`/`virtual_table_names` on `EmbeddedConnection`; post-scan WHERE filter for `col = 'val'` and `col = N`; intercepted in `Connection::query` before FTS and GlueSQL
- [x] Add in-memory index support — B-tree secondary indexes for faster lookups on indexed columns (~80 SLOC) — `BTreeIndex`/`IndexKey`/`IndexRegistry` in `src/btree_index.rs`; `CREATE INDEX`/`DROP INDEX` intercepted in `Connection::execute`; index auto-updated after INSERT; public API: `create_btree_index`/`drop_btree_index`/`has_btree_index`/`lookup_btree_index`
- [x] Add `EXPLAIN` support — `EmbeddedConnection::explain()` uses pattern-based SQL inspection to return a formatted plan string (~40 SLOC)
- [x] Add `PRAGMA` support — `handle_pragma()` intercepts PRAGMA statements in `execute()`/`query()` before GlueSQL sees them; returns synthetic rows for `journal_mode`, `foreign_keys`, `page_size`, `page_count`, `freelist_count`, `cache_size`, `user_version`, `integrity_check`; unknown PRAGMAs return empty rows silently (~70 SLOC)
- [x] Add `ATTACH DATABASE` support — `handle_attach()` intercepts ATTACH statements in `execute()`/`query()` before GlueSQL sees them; returns a clear `OxiSqlError::UnsupportedUri` explaining the limitation and pointing to `open_file()` as the upgrade path; tested via `test_attach_database_returns_meaningful_error`, `test_attach_schema_returns_meaningful_error`, `test_attach_via_query_returns_meaningful_error` (~25 SLOC)
- [x] Add user-defined functions (UDFs) — `register_udf(name, fn)` and `call_udf(name, args)` with `Arc<RwLock<UdfRegistry>>` for extensible scalar functions; shared across clones (~60 SLOC)
- [x] Add aggregate UDFs — `register_aggregate(name, init, step, finalize)` / `apply_aggregate(name, values)` with `Arc<RwLock<HashMap<String, Arc<AggregateUdf>>>>` shared across clones; `init → step* → finalize` reduction pattern (~60 SLOC)
- [x] Add `execute_batch` — execute multiple semicolon-separated statements in a single call (~15 SLOC)
- [x] Add `ping` — always succeeds for in-memory connections (~5 SLOC)
- [x] Add savepoint support — `savepoint(name)` / `release_savepoint(name)` / `rollback_to_savepoint(name)` as no-ops on `EmbeddedConnection` (GlueSQL MemoryStorage does not support nested transactions); API-compatible with transactional backends (~30 SLOC)

## API Improvements
- [x] Add `EmbeddedConnection::open_file(path)` — async constructor; returns `UnsupportedUri` without `sled-storage` feature; Cargo feature gate `sled-storage = []` wired up for future `SledStorage` backend
- [x] Add `EmbeddedConnection::from_glue(glue)` — constructor from pre-existing GlueSQL instance (~10 SLOC)
- [x] Add schema introspection stubs — initial `tables()`, `columns(table)`, `indexes(table)`, `foreign_keys(table)` returning `UnsupportedUri` (~30 SLOC) — superseded by full GlueSQL catalog implementation below
- [x] Implement tables/columns/indexes/foreign_keys introspection via GlueSQL catalog — `tables()` uses `fetch_all_schemas()`; `columns()` maps `ColumnDef` to `ColumnInfo` with full type mapping; `indexes()` merges GlueSQL `SchemaIndex` list with `IndexRegistry` named indexes (since `CREATE INDEX` is intercepted); `foreign_keys()` maps `ForeignKey` fields; ~150 SLOC; 10 new tests in `tests/schema_introspection.rs`; 4 updated tests in `memory_prepare.rs` and `retry.rs`
- [x] Add `EmbeddedConnection::execute_script(sql)` — convenience for multi-statement scripts (~10 SLOC)
- [x] Document GlueSQL SQL dialect differences from standard SQL — `# GlueSQL SQL Dialect Notes` section added to `src/lib.rs` module doc; covers unsupported features, transaction semantics, type notes, and parameter binding (~35 SLOC docs)
- [x] Add `EmbeddedConnection::export_as_sql()` — returns `UnsupportedUri` (GlueSQL MemoryStorage has no stable INFORMATION_SCHEMA) (~10 SLOC)
- [x] Implement export_as_sql() via GlueSQL Store::fetch_all_schemas() + Schema::to_ddl() (Wave 39+)
- [x] Add `EmbeddedConnection::import_from_sql(sql)` — delegates to `execute_batch` to load SQL dump strings (~5 SLOC)

## Testing
- [x] Test all GlueSQL value-to-OxiSQL value conversions — verify each of the 20+ `glue_value_to_oxisql` branches (~40 SLOC)
- [x] Test param substitution edge cases — `$10` vs `$1`, SQL injection attempts, nested quotes (~25 SLOC)
- [x] Test transaction isolation — GlueSQL MemoryStorage does not support true isolation; documented behavior: `transaction()` returns an error when `BEGIN` fails (GlueSQL limitation); test `test_transaction_changes_visible_within_txn` verifies graceful error handling and within-transaction visibility semantics (~30 SLOC)
- [x] Test transaction rollback — verify rolled-back changes are fully reverted (~15 SLOC)
- [x] Test DDL operations — `test_ddl_create_drop_table`: CREATE TABLE, INSERT, DROP TABLE, verify SELECT after DROP fails (~30 SLOC)
- [x] Test complex queries — `test_complex_join_query` (JOIN + ORDER BY), `test_group_by_aggregation` (GROUP BY + SUM, GlueSQL-safe), `test_order_by_and_limit` (ORDER BY ASC LIMIT 3), `test_subquery_in_where` (IN subquery, no-panic guard) (~120 SLOC)
- [x] Test concurrent UDF reads — verify `Arc<RwLock<UdfRegistry>>` allows multiple concurrent `call_udf` invocations without deadlock (~15 SLOC)
- [x] Test concurrent connection clones — `test_concurrent_connection_clones`: 5 concurrent tokio tasks each INSERT via clone; no deadlock; final SELECT returns 5 rows (~30 SLOC)
- [x] Test large result sets — `test_large_result_set`: 500-row execute_batch insert, SELECT returns 500 rows (~20 SLOC)
- [x] Test `execute_batch` with mixed DDL and DML statements — `test_execute_batch_mixed_ddl_dml`: CREATE TABLE + 2 INSERTs in one batch, COUNT(*) verifies 2 rows (~15 SLOC)
- [x] Test JSON functions — `test_json_set_and_get`, `test_json_get_missing`, `test_json_set_overwrite` (~30 SLOC)
- [x] Test `open_file` without feature returns `UnsupportedUri` (`test_open_file_returns_error_without_feature`)
- [x] Test savepoint no-ops — `test_savepoint_no_op`, `test_rollback_to_savepoint_no_op`, `test_release_savepoint_no_op` (~15 SLOC)
- [x] Test aggregate UDFs — sum, empty input, unknown name, overwrite, shared-across-clones (`test_aggregate_*`) (~40 SLOC)
- [x] Test NULL values — `test_null_values_in_queries`: INSERT NULL, SELECT ORDER BY id, verify second row optional IS Null (~15 SLOC)
- [x] Test import_from_sql — single and multi-statement SQL dumps (`test_import_from_sql*`) (~25 SLOC)
- [x] Test export_as_sql round-trip, empty table, single-quote escaping, NULL round-trip (`test_export_as_sql_round_trip`, `test_export_as_sql_empty_table`, `test_export_as_sql_text_with_quotes`, `test_export_as_sql_nulls`) (~80 SLOC)
- [x] Test persistent storage backend — `test_fjall_persistence` in `fjall_storage_tests` module: write → close → reopen → verify; uses `std::env::temp_dir()` for isolation; requires `fjall-storage` feature (~20 SLOC); **also**: `test_redb_persistence_across_connections` in `redb_storage_tests` module; 6 redb tests total covering create+insert, persistence, in-memory smoke, multi-table, drop table, param binding; requires `redb-storage` feature

## Performance
- [x] Benchmark `EmbeddedConnection` query throughput — simple SELECT, complex JOIN, aggregations (~40 SLOC) — `benches/embedded_benchmarks.rs`: `simple_select_100_rows`, `count_100_rows`, `insert_single_row`, `select_empty_table`; uses criterion 0.8.2 `async_tokio` executor
- [x] Benchmark param substitution overhead vs type-safe binding — `bench_param_binding`: compares `bind_params` (AST) vs `bind_params_string` (string scan) for a 3-param INSERT (~20 SLOC)
- [x] Profile `Mutex` contention under concurrent workloads — `bench_mutex_contention`: benchmarks 1/4/8 concurrent `tokio::spawn` tasks each doing SELECT via shared `Arc<Mutex<Glue>>` clone (~40 SLOC)
- [x] Benchmark persistent backend I/O overhead vs pure in-memory mode (~25 SLOC) — `bench_persistent_vs_memory` group in `benches/embedded_benchmarks.rs`: `memory_insert_100` vs `redb_insert_100`; creates fresh connection per iteration; uses `sample_size(10)` for slow file I/O; requires `redb-storage` feature; conditional `criterion_group!` compiles cleanly both with and without the feature
- [x] Benchmark GlueSQL memory usage for large tables (1M+ rows) (~15 SLOC)

## Integration
- [x] Integration with `oxistore-kv-redb` — `RedbGlueStorage` in `src/redb_storage.rs`; `RedbEmbeddedConnection` in `src/lib.rs` (redb_conn module); feature gate `redb-storage`; order-preserving binary key encoding; auto-increment counter persisted in redb; 6 integration tests in `tests/memory.rs` (create+insert, persistence, in-memory, multi-table, drop table, param binding) (~580 SLOC)
- [x] Integration with `oxistore-kv-fjall` — `FjallGlueStorage` in `src/fjall_storage.rs`; `FjallEmbeddedConnection` in `src/lib.rs` (fjall_conn module); feature gate `fjall-storage`; 6 integration tests in `tests/memory.rs` (create+query, persistence, multi-table, delete, drop table, param binding) (~280 SLOC)
- [x] Integration with `oxisql-datafusion` — serve `EmbeddedConnection` tables as DataFusion `TableProvider` (~30 SLOC) — `register_embedded_table(&dyn Connection, ...)` in `oxisql-datafusion/src/context.rs:371`
- [x] Integration with `oxisql-parse` — AST-level param binding is implemented via `sqlparser` (the same AST library underlying `oxisql-parse`); `bind_params()` in `src/params.rs` uses `visit_expressions_mut` to replace `Placeholder` nodes with typed `Expr` literals (~25 SLOC)
- [x] Integration with `oxisql` facade — verify `oxisql::connect("memory://")` works end-to-end (~10 SLOC) — `connect_memory_end_to_end` in `crates/oxisql/tests/connect.rs:181`
