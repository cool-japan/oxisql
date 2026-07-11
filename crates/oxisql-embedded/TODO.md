# oxisql-embedded — TODO

**Status:** Stable · v0.3.2 · 278 tests pass (`--all-features`).

In-memory SQL engine backed by GlueSQL `MemoryStorage`, with optional Pure-Rust persistent backends (fjall LSM-tree, redb B-tree, sled key-value). `EmbeddedConnection` implements `oxisql_core::Connection`; parameters are bound client-side via an `sqlparser` AST pass with a string-scan fallback. Full schema introspection, CSV/SQL import/export, host-side UDFs, virtual tables, full-text search, and B-tree secondary indexes are implemented.

## Done

### Connections & backends
- [x] `EmbeddedConnection` over GlueSQL `MemoryStorage` implementing the full `Connection` trait.
- [x] Constructors: `open_memory()`, `open_file(path)`, `from_glue(glue)`.
- [x] `RedbEmbeddedConnection` (`redb-storage`) — redb B-tree, ACID, order-preserving binary key encoding, persisted auto-increment counter.
- [x] `FjallEmbeddedConnection` (`fjall-storage`) — fjall LSM-tree with journal/WAL crash safety.
- [x] `SledEmbeddedConnection` (`sled-storage`) — sled key-value store; hand-rolled `serde_json` serialisation (no bincode, per COOLJAPAN policy).
- [x] `EmbeddedTransaction` and `EmbeddedPrepared` (transaction guard + prepared statement).
- [x] Facade URI routing: `memory://`, `redb://path`, `fjall://path`, `sled://path`.

### SQL surface
- [x] `Connection` methods: `execute`, `query`, `transaction`, `execute_batch`, `ping`, `prepare`, `query_stream`.
- [x] Schema introspection: `tables()`, `columns()`, `indexes()`, `foreign_keys()` from the GlueSQL catalog; `indexes()` merges catalog `SchemaIndex` entries with host `IndexRegistry` names.
- [x] Named parameters (`:name` / `$name` / `@name`) via the `oxisql_core` default `execute_named` / `query_named` methods — uniform across all backends.
- [x] Positional parameter binding: AST-level substitution (`params.rs`) with string-scan fallback; placeholders inside string literals preserved; `$10` vs `$1` boundaries handled.
- [x] BLOB parameters via `X'..'` hex literals.
- [x] Full GlueSQL `Value` ↔ OxiSQL `Value` mapping (20+ variants).

### Extensions
- [x] CSV import/export — `import_csv(table, csv)` / `export_table_to_csv(table)`; Pure-Rust RFC 4180 state-machine parser (`csv.rs`) handling quoted commas, `""` escapes, bare LF and CRLF.
- [x] SQL dump import/export — `import_from_sql(sql)` (via `execute_batch`) and `export_as_sql()` (via `fetch_all_schemas()` + `Schema::to_ddl()`).
- [x] `explain(sql)` — pattern-based plan string.
- [x] Scalar UDFs — `UdfRegistry`, `register_udf` / `call_udf` (`Arc<RwLock<…>>`, shared across clones).
- [x] Aggregate UDFs — `AggregateUdf`, `register_aggregate` / `apply_aggregate` (`init → step* → finalize`).
- [x] Virtual tables — `VirtualTableRegistry`; registered providers scanned at query time with a post-scan WHERE filter.
- [x] Full-text search — `FtsIndex` inverted index; intercepts `CREATE VIRTUAL TABLE … USING fts`, FTS inserts, and `MATCH` queries (AND semantics).
- [x] B-tree secondary indexes — `BTreeIndex` / `IndexKey` / `IndexRegistry`; `CREATE INDEX` / `DROP INDEX` intercepted and indexes auto-updated after INSERT.
- [x] `PRAGMA` and `ATTACH DATABASE` interception with meaningful synthetic responses / errors.
- [x] JSON helpers (`json_set` / `json_get`).

### Quality
- [x] 278 tests (`--all-features`) — CSV round-trips, schema introspection, persistence (write → close → reopen), parameter-binding edge cases, transactions/rollback, UDFs/aggregates, FTS, virtual tables, NULL handling, large result sets.
- [x] Criterion benchmarks (`benches/embedded_benchmarks.rs`) — query throughput, parameter-binding overhead, mutex contention, persistent vs in-memory I/O.

## Roadmap / next
- [ ] True concurrent reads — currently blocked by the GlueSQL API (`Glue::execute` needs `&mut` even for SELECT); would require a snapshot-isolation storage layer or a custom shared-state `Store`.
- [ ] Window functions and `ALTER TABLE ADD/DROP COLUMN` — track GlueSQL upstream support.
- [x] Cost-based `explain()` beyond the current pattern-based output. (done 2026-06-10)
  - **Goal:** `EmbeddedConnection::explain(sql)` emits a full optimizer cost-annotated plan tree for `SELECT` queries (rows/cost per node); INSERT/UPDATE/DELETE/DDL and parse failures retain the existing pattern-based summary.
  - **Design:** `oxisql-parse.workspace = true` is already a dep; all required APIs are public: `parse_one`, `plan_statement`, `optimize`, `explain_verbose(&plan, &CostModel::new())`. At `src/lib.rs:821-861`, for `Statement::Query`: parse SQL → plan → optimize → `explain_verbose`. Keep the existing pattern-based code as the fallback path (DML/DDL and parse errors). Schema-agnostic — works without materialized tables.
  - **Files:** `src/lib.rs` (explain @ ~L821)
  - **Tests:** `test_explain_select` and `test_explain_join` in `tests/memory_complex.rs` must still pass (both use loose `contains` assertions). Add: a SELECT explain contains `rows=`/`cost=`; an INSERT explain shows verb via fallback; malformed SQL does not panic.
  - **Risk:** Low — the two existing tests use loose assertions; the new path is gated to Query only; fallback handles the rest.
- [ ] SQL-level (not just host-side) UDF/virtual-table dispatch as GlueSQL extension points mature.
- [ ] Optional encryption-at-rest for the persistent backends.

## Known limitations
- **No nested-transaction savepoints on `MemoryStorage`.** `Transaction::savepoint` / `rollback_to_savepoint` / `release_savepoint` are accepted but are **no-ops** — GlueSQL `MemoryStorage` does not support nested transactions. The API stays compatible with backends that do.
- GlueSQL dialect gaps: window functions, `ALTER TABLE ADD/DROP COLUMN`, multi-row `VALUES` in a single INSERT, and `INFORMATION_SCHEMA` are not available; `BEGIN`/`COMMIT`/`ROLLBACK` are syntactically accepted but carry no MVCC.
- Concurrent access serialises through a single lock (see Roadmap).
