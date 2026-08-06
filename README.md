# OxiSQL — Pure-Rust unified SQL layer

[![crates.io](https://img.shields.io/crates/v/oxisql.svg)](https://crates.io/crates/oxisql)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![MSRV: 1.89](https://img.shields.io/badge/MSRV-1.89-orange.svg)](https://blog.rust-lang.org/2025/08/07/Rust-1.89.0.html)
[![Pure Rust: C-free](https://img.shields.io/badge/Pure%20Rust-C--free-brightgreen.svg)](#the-c-free-oxisqlite-engine)

OxiSQL is the COOLJAPAN-blessed Pure-Rust database layer: a unified SQL surface
that spans embedded engines, OLTP wire-protocol clients (PostgreSQL, MySQL), and
a SQLite-compatible embedded path — without `libpq`, `libmysqlclient`,
`libsqlite3`, or any C/C++ DB driver. It exists because every COOLJAPAN service
today either reaches for `rusqlite` (C SQLite), `tokio-postgres` against
`native-tls`/`ring`, or `sqlx` with a `*-sys` TLS provider — each of which drags
`libssl-dev`, `libpq-dev`, or `libsqlite3-dev` into the ecosystem's CI critical
path. OxiSQL collapses these into one facade, defaults to Pure Rust drivers
(`tokio-postgres`, `mysql_async`, GlueSQL, and the C-free **oxisqlite** engine),
and routes all TLS through OxiTLS with the rustcrypto provider (no `ring`, no
`openssl-sys`).

As of 0.3.0 the default workspace build is additionally **`objc2`-free on macOS**: `objc2-system-configuration` (previously pulled in transitively by `whoami`) has been excised via a vendored `whoami-patched` crate. As of 0.2.0 the SQLite path is **genuinely C-free**. It is served by an in-tree
fork of the limbo engine (`oxisqlite-*`) from which every C touchpoint has been
removed — no `libsqlite3`, no `mimalloc`, no `lemon` parser generator. The
default build of the entire workspace compiles cleanly with the C compiler
disabled:

```text
CC=/usr/bin/false cargo build --workspace   # → exit 0
cargo build --workspace                      # → 0 warnings
```

**Version 0.4.1 — 2026-08-07.**
18 workspace crates (plus 2 non-published, internal patch-shim crates) · 2,261 tests passing (2,755 with `--all-features`) · 0 failing · 0 clippy warnings.
~183,534 lines of Rust across 481 source files (verified via `cargo nextest run --workspace [--all-features]`, `cargo clippy --all-targets --all-features`, and `tokei`).

---

## What's new in 0.4.1

- **`CREATE TRIGGER` / `DROP TRIGGER` and full row-trigger execution.** All six row-trigger kinds fire (`BEFORE`/`AFTER` × `INSERT`/`UPDATE`/`DELETE`), with `WHEN` guards, `UPDATE OF (cols)` filtering, `OLD.*`/`NEW.*` (including `rowid`), and `INSERT`/`UPDATE`/`DELETE`/`SELECT RAISE(...)` body commands — persisted as real `sqlite_schema` objects, not an in-memory-only feature.
- **`TEMP` objects and `ATTACH`/`DETACH DATABASE`.** A new per-connection database registry (modelled on upstream's `sqlite3.aDb[]`) backs `CREATE TEMP TABLE`/`VIEW`/`TRIGGER` and real multi-database `ATTACH`/`DETACH`, closing all six former `todo!("temp databases not implemented yet")` process-abort sites.
- **`PRAGMA index_list` / `PRAGMA index_info`** surface real index metadata from the in-memory schema; `oxisql-sqlite-compat`'s `Connection::indexes()` now uses them instead of string-parsing `CREATE INDEX` SQL text.
- **New crate `oxisql-cache`** — `SqlQueryCache`, `SqlPlanCache<P>`, and the `CachedQueryRunner` read-through adapter, available via the `oxisql` facade's new `cache` feature (`oxisql::cache`). This inverts a former oxisql⇄oxistore repo-level dependency cycle: the SQL-layer caching that used to live in `oxistore-cache`'s `sql` feature now lives here, depending *on* `oxistore-cache` rather than the reverse.
- **Fuzz targets** (new detached `fuzz/` workspace) and **runnable quickstart examples** for every facade/driver crate (`oxisql-sqlite-compat`, `oxisql-embedded`, `oxisql-postgres`, `oxisql-mysql`, `oxisql`).
- **Process-abort and undefined-behavior fixes**: `PRAGMA page_size`/`auto_vacuum = 2` no longer `todo!()`/`unimplemented!()`; three release-active B-tree balancing `assert!()`s converted to typed `Corrupt` errors; JSONB malformed-blob `str::from_utf8_unchecked` and unchecked size-field reads fixed; four unmessaged `Table` root-page/drop panics now typed errors (breaking change to `oxisqlite-core`'s `Table::get_root_page()` signature, not reachable through the `oxisql` facade).
- **Dependency bumps**: `oxiarc-zstd` → `0.4.1`, `oxitls` → `0.3.0`, `oxistore-cache` → `0.3.0`.

See `CHANGELOG.md` for the full list, including additional engine correctness fixes and the `unwrap`/`expect` sweep in the storage layer.

## What's new in 0.4.0

Version 0.4.0 is a **packaging / release-hygiene** minor bump — there are **no
source-code changes since 0.3.3**. Every inter-crate dependency floor across the
17-crate workspace is now pinned to the exact full-triple `0.4.0` (rather than a
loose minor-only caret), carrying the 0.3.4 fix forward as the `0.4.x` baseline:
the resolver can no longer pair a newer caller with an older family member held
back by a stale `Cargo.lock`. The minor bump also gives the two internal breaking
changes that shipped in 0.3.3 (`oxisqlite::Params::Named`'s key type;
`oxisqlite-sqlite3-parser`'s lexer `Token` lifetime) a proper semver home. Engine
behavior, wire protocols, and the public `oxisql` API are identical to 0.3.3.

## What's new in 0.3.3

- **Open a database from an in-memory byte buffer.** New `open_from_bytes`
  entry points open a SQLite database directly from a byte slice (e.g.
  `include_bytes!`, `VACUUM INTO`, or `sqlite3_serialize()` output) with no
  temporary file — enabling WASI/browser/read-only-filesystem use.
  `oxisqlite_core::Database::open_from_bytes(bytes, enable_mvcc)` copies the
  image into a fresh in-memory page store and is not gated by the `fs`
  feature; `oxisqlite::Database::open_from_bytes(bytes)` mirrors SQLite's
  `sqlite3_deserialize()`; `SqliteConnection::open_from_bytes(bytes)`
  (async) and `SqliteConnectionBlocking::open_from_bytes(bytes)` (sync,
  `blocking` feature) expose it through the `oxisql-sqlite-compat` layer.
  Malformed input (too short, wrong magic, invalid page size) returns a
  typed error and never panics; any standard on-disk page size is accepted
  for reading.
- **PostgreSQL logical replication.** `oxisql-postgres` gains an opt-in
  `replication` feature (surfaced on the facade as `postgres-replication`)
  with CDC-style streaming via `PgReplicationConnection::connect`,
  `identify_system`, `create_replication_slot`, `drop_replication_slot`, and
  `start_logical_replication`, which hands back a `ReplicationStream` you
  `ack()` against as you consume it. Includes full `pgoutput` wire-protocol
  decoding (Begin/Commit/Origin/Relation/Type/Insert/Update/Delete/Truncate/
  Message), LSN parsing/formatting, `COPY BOTH` streaming (background reader
  task plus a periodic Standby Status Update keepalive task), and a
  brace/quote/escape-aware decoder for PostgreSQL's `{...}` array-literal
  text format. Not routed through `tokio-postgres` — it cannot negotiate
  replication mode — so this drives the wire protocol directly via a new
  `postgres-protocol` dependency.
- **SQL surface growth (`oxisqlite-core`).** `CREATE TABLE ... AS SELECT`;
  `EXCEPT`/`INTERSECT` compound-`SELECT` operators alongside the existing
  `UNION`/`UNION ALL`, with `NULL`-aware set semantics and strict
  left-to-right grouping of mixed chains (`A UNION B EXCEPT C` =
  `(A UNION B) EXCEPT C`); `ON CONFLICT` conflict-resolution clauses on
  `CREATE TABLE` column/table `UNIQUE`/`PRIMARY KEY` constraints; `OFFSET`,
  `ORDER BY`, and `WITH` on the outer compound `SELECT`; parenthesized join
  sources (`FROM (t1 JOIN t2 ON ...)`); the `REGEXP` operator / `regexp()`
  function (unanchored regex search over the `regex` crate's dialect,
  three-valued `NULL` handling, a clean constraint error — never a panic —
  on a malformed pattern); more `printf()` format specifiers (`%i`, `%x`/
  `%X`, `%o`, `%c`, `%e`/`%E`; flag/width/precision modifiers such as `%05d`
  remain a documented TODO); and real `ORDER BY` pushdown into a virtual
  table's `xBestIndex`, so the engine skips its own sort step when the vtab
  reports the ordering is already satisfied.
- **Index-based access restored for `DELETE`/`UPDATE` (`oxisqlite-core`,
  `index_experimental` feature).** Both plans had unconditionally fallen
  back to a full table scan ever since a real, upstream-reported corruption
  hazard was found in driving `DELETE`'s row loop from a secondary index
  while also maintaining that same index as per-row cursor work (see
  [tursodatabase/limbo#1714](https://github.com/tursodatabase/limbo/issues/1714));
  `UPDATE` had an analogous hazard whenever `SET` changed the driving
  index's own key columns. The optimizer now runs its normal access-method
  selection and keeps the result only when proven safe (a non-looping rowid
  lookup, or a range scan/seek over a cursor whose own key the statement
  provably cannot shift under it) — anything else still falls back to the
  previous full-table-scan behavior, so indexes are usable again for the
  common safe cases without reintroducing the corruption.
- **B-tree index-page balance panic/corruption fix (`oxisqlite-core`).**
  Inserting into a b-tree could panic (or silently corrupt the page in
  release builds) whenever parent balancing of an interior **index** page
  placed a cell at a logical position beyond the page's physical cell count
  while the page already held deferred overflow cells — reachable on real,
  index-heavy databases (e.g. a `JOIN` whose planner-built transient
  automatic index grew deep enough to rebalance an interior index page).
  `insert_into_cell` now only takes the in-place path when the target
  position lies within the physical cell array, otherwise appending to
  `overflow_cells` in logical order.
- **Named parameter binding in the raw engine (`oxisqlite`).** `:name`,
  `@name`, `$name`, and `#name` placeholders now bind correctly in
  `Statement::query`/`execute` — previously an unconditional panic whenever
  named parameters reached the engine directly.
- **Windows file locking (`oxisqlite-core`).** Real `LockFileEx`/
  `UnlockFileEx`-backed locking (`io/windows.rs`) via a new `windows-sys`
  dependency that is Windows-only and never enters the dependency graph on
  other targets — previously an unconditional panic on that path.
- **Server version accessors.** `PgConnection::server_version()`
  (`oxisql-postgres`) and `MyConnection::server_version()` (`oxisql-mysql`),
  plus `BackendInfo::from_postgres_connection`/`from_mysql_connection`
  (`oxisql`) to populate `BackendInfo.version` from a live connection instead
  of only the static, connectionless dispatcher.
- **Cleaner REPL error output.** A new `display_error()` helper (`oxisql`),
  now used by the `oxisql-repl` binary, renders a readable, user-facing
  error message instead of a raw GlueSQL parser-error debug-dump.
- **MVCC correctness and durability fixes (`oxisqlite-core`).** A
  transaction-removal race that could panic on concurrent commit/rollback is
  fixed — the affected call sites now return typed errors instead of
  panicking. Separately, `commit_tx()` now persists to the write-ahead log
  *before* marking a transaction visible in memory (it previously did the
  reverse), closing a crash-window data-loss gap; a failed persist reverts
  the transaction to `Active` and returns an error rather than silently
  losing it. WAL checkpoint bookkeeping was also tightened: `checkpoint_seq`/
  `salt_1` now evolve consistently on every fully-backfilled checkpoint
  (not just `Restart`/`Truncate`), and a new frame-trimming step prevents
  frame-cache bookkeeping from growing unboundedly across repeated partial
  checkpoints on a long-running connection.
- **Correctness fixes across the engine (`oxisqlite-core`).**
  `UPDATE ... RETURNING` (previously returned zero rows silently),
  `ALTER TABLE ... RENAME` (previously crashed on any schema containing a
  view, trigger, or virtual table anywhere — not just the table being
  renamed), multi-row `INSERT` into virtual tables (previously kept only the
  last row), `IN (...)` used as a plain value expression outside
  `WHERE`/`HAVING`/`JOIN ... ON` (previously crashed), a `WITH` CTE
  referenced more than once in the same query (previously only the first
  reference resolved correctly), wide-row records with a header over 126
  bytes — roughly 127+ columns, or fewer with one large `TEXT`/`BLOB`
  (previously crashed during serialization), `BLOB`/`TEXT` coercion in
  `QUOTE()`/`||`/`concat()` (previously crashed on `BLOB` operands), and a
  `PRAGMA auto_vacuum = FULL` high-water-mark bug that stalled after the
  first table/index are all fixed. Reading a corrupt or maliciously-crafted
  database file (untrusted cell/freeblock pointers, an untrusted page-type
  byte, a malformed free-space computation) now returns `LimboError::Corrupt`
  at several more sites instead of panicking.
- **PostgreSQL connection-timeout hang (`oxisql`).** Connecting to an
  unreachable PostgreSQL host via `connect()` (also `connect_with_options`/
  `connect_with_tls`) previously hung indefinitely; a connection timeout now
  applies by default (10s, configurable via
  `ConnectOptions::connect_timeout_ms`) and returns a typed timeout error.
- **Memory-safety hardening via Miri (`oxisqlite-core`).** Fixed a
  pointer-provenance bug materializing `BLOB`/`TEXT` values from on-disk
  pages, an unaligned-reference bug in the `VECTOR` column type's slice
  conversions, and a page-cache memory leak (evicted entries were destructed
  but never deallocated). Reworked 40+ page-access call sites across the
  storage/B-tree layers that could previously manufacture simultaneous
  mutable aliases into the same page buffer from a shared reference, plus
  smaller pointer-provenance fixes in result-row construction and
  virtual-table module/row-ID handling.
- **Large modules split via `splitrs`.** `json/jsonb.rs`, `storage/pager.rs`,
  `functions/datetime.rs`, `translate/expr.rs`, and `translate/insert.rs`
  (`oxisqlite-core`), plus `ast/fmt.rs`/`ast/mod.rs`
  (`oxisqlite-sqlite3-parser`), were split into smaller per-concern files to
  stay under the workspace's 2000-line file policy — no functional changes.
- **Two internal breaking changes — neither touches the `oxisql` facade.**
  `oxisqlite`'s `Params::Named` now stores `Cow<'static, str>` keys (was
  `Vec<(String, Value)>`, now `Vec<(Cow<'static, str>, Value)>`), so binding
  a `'static` placeholder-name literal borrows instead of allocating; and
  `oxisqlite-sqlite3-parser`'s lexer `Token` type dropped its lifetime
  parameter (`Token<'i>(usize, &'i [u8], usize)` → `Token(usize,
  Cow<'static, str>, usize)`). Both are internal to the low-level
  `oxisqlite`/parser crates beneath `oxisql-sqlite-compat` — the
  facade-level `Connection::query_named`/`execute_named` API used in the
  Quick Start examples below is unaffected.
- **Routine dependency bump.** `oxiarc-zstd` (backing the `zstd-shim` patch
  crate) bumped to `0.3.6`.

---

## What's new in 0.3.2

- **`zstd-shim` — a local Pure-Rust `zstd`-API patch crate.** A new, unpublished
  `crates/zstd-shim/` implements just the `bulk::{Compressor, Decompressor}` /
  `DEFAULT_COMPRESSION_LEVEL` surface that `arrow-ipc` (via DataFusion) actually
  calls, backed by `oxiarc-zstd`, and is wired in via `[patch.crates-io]`
  (`zstd = { path = "crates/zstd-shim" }`). The `--all-features` dependency
  closure no longer pulls the C-FFI `zstd-sys` crate, closing the last gap under
  the COOLJAPAN OxiARC-only compression policy (no `zip`, `flate2`, `zstd`,
  `bzip2`, `lz4`, `tar`, `snap`, `brotli`, or `miniz_oxide` — everything routes
  through `oxiarc-*`).
- **`GROUP BY … HAVING COUNT(*)` accumulator fix (`oxisqlite-core`).**
  `COUNT(*)`/`COUNT()` reached via `HAVING`, `ORDER BY`, or a nested expression
  — rather than a result column — was planned with zero arguments, undercounting
  its sorter-column span and leaving its accumulator register unreset across
  group boundaries; multi-group `GROUP BY … HAVING COUNT(*) …` queries could
  read the sorter out of bounds or panic on a stale accumulator. `COUNT(*)` now
  carries the same synthetic literal-`1` argument the result-column path already
  used, and the accumulator-clear range now covers the full aggregate block.
  `op_agg_step` initialization is also self-healing (re-initializes on any
  non-`AggContext` register, not just `Value::Null`) and returns
  `LimboError::InternalError` instead of panicking on internal invariant
  violations. New coverage in `tests/group_by_having.rs`.
- **Routine dependency bumps.** `oxiarc-zstd` to `0.3.5` (backing the new shim),
  `time` to `0.3.53`, `uuid` to `1.23.4` (also in `oxisqlite-uuid`), and, in
  `oxisqlite-core`, `env_logger` to `0.11.11` and the Linux-only `io-uring` to
  `0.7.13`.

---

## What's new in 0.3.1

- **DataFusion 54 compatibility (`oxisql-datafusion`).** Removed `as_any()`
  overrides from `TableProvider` and `ExecutionPlan` impls in `parquet.rs`,
  `provider.rs`, and `stream.rs` — the method was removed from both traits in
  DataFusion 54, so the overrides were dead code. `arrow` pinned to `58.3.0` to
  match DataFusion 54's re-exported version (`oxistore-columnar` also pins
  `58.3.0`; arrow 59 has no compatible DataFusion 54 release).
- **`FlamegraphProfiler` benchmark utility (`oxisqlite-core`).** New
  `benches/common/profiler.rs` implements a Criterion 0.8-compatible `Profiler`
  backed by `pprof`, emitting per-benchmark flamegraph SVGs when run with
  `--profile-time`. Avoids the `criterion` 0.5 ↔ 0.8 version conflict that
  `pprof`'s bundled integration introduced.

---

## What's new in 0.3.0

- **`objc2-system-configuration` removed from default dep closure.** The macOS
  `SCDynamicStore` C/ObjC binding previously pulled in transitively by `whoami`'s
  `std` feature has been excised. The default `cargo build --workspace` is now
  100 % `objc2`-free on macOS, closing the last non-pure-Rust gap on Apple
  platforms under COOLJAPAN Pure Rust Policy v2 §3 (Role-A compliance restored).
- **`whoami-patched` vendored crate.** A Pure-Rust patch of `whoami` 2.1.2 drops
  `objc2-system-configuration` from the macOS code path; wired in via
  `[patch.crates-io]` in the workspace `Cargo.toml`. Not published to crates.io
  (vendored only).
- **`oxisqlite-core` pure-Rust I/O backend by default.** The native epoll/kqueue
  event-loop is now gated behind the `native-io` feature (opt-in). The
  `load-extension` feature (which pulls `libloading`) is likewise opt-in.
  Default builds remain 100 % C-free.
- **`oxitls` dependency bumped to `^0.2.0`.** Resolves the `PENDING-REPUBLISH`
  dependency block now that `oxitls 0.2.0` has been published to crates.io.

---

## What's new in 0.2.1

- **WITHOUT ROWID table support.** `CREATE TABLE … WITHOUT ROWID` is now fully
  implemented. The engine uses an index-format B-Tree where the PRIMARY KEY
  columns are the B-Tree key and the full row is the stored record payload.
  Supports `INSERT` (single-row, multi-row, `INSERT … SELECT`), `SELECT`
  (full-scan), `OR IGNORE`, and `OR REPLACE` conflict resolution. PK NOT NULL and
  uniqueness are enforced. A synthetic index object drives cursor allocation so the
  execution layer automatically uses the correct B-Tree page format.
  16 integration tests in `crates/oxisqlite-core/tests/without_rowid.rs`.
- **`BorrowedValue<'a>` — zero-allocation SQL value view.** `oxisql-core` gains a
  lifetime-parametric mirror of `Value` where `Text`, `Blob`, `Json`, and
  `Decimal` borrow from existing storage instead of owning heap allocations. Scalar
  variants are copied inline. `BorrowedValue::to_owned()` converts back to an
  owned `Value`. Re-exported from `oxisql-core` root. 15 unit tests.
- **B-tree split via splitrs (`oxisqlite-core`).** `storage/btree.rs` (8 864 lines)
  replaced by a 6-module sub-tree: `btree/mod.rs`, `btree/cursor_core.rs`,
  `btree/cursor_write.rs`, `btree/cursor_nav.rs`, `btree/page_ops.rs`,
  `btree/tests.rs`. All existing tests pass; 0 warnings.

---

## What's new in 0.2.0

- **C-free oxisqlite engine fork.** The SQLite-compatible path no longer pulls in
  any C code. A vendored, de-C'd fork of limbo 0.0.22 (`oxisqlite`,
  `oxisqlite-core`, `oxisqlite-sqlite3-parser`, and four support crates) replaces
  the previous build, making the SQLite backend Pure Rust for the first time.
- **Full-transaction ROLLBACK.** `BEGIN; INSERT; ROLLBACK` now correctly discards
  changes, `COMMIT` persists them, and WAL integrity is preserved. The rollback
  machinery was ported from `turso_core` 0.7.0-pre.5 (MIT). The old "ROLLBACK not
  supported" limitation is gone.
- **Apache-2.0 compliance + security-patched TLS.** A GPL-licensed Julian-day
  helper was replaced by an inline pure-Rust implementation, license auditing
  (`cargo deny`) passes, a root [`NOTICE`](NOTICE) records the full fork lineage,
  and the TLS stack was hardened against RUSTSEC-2026-0104 (CRL-parsing panic;
  see [Pure Rust — FFI eliminated](#pure-rust--ffi-eliminated) for the
  current, since-evolved mechanism).
- **ANALYZE statement + System-R optimizer with real statistics.** `ANALYZE`,
  `ANALYZE <table>`, and `ANALYZE <index>` write cardinality rows to
  `sqlite_stat1`; the query optimizer consumes them via the new `SchemaStats`
  side-map, replacing hardcoded estimates. Un-analyzed databases are unaffected
  (backwards compatible). 6 ANALYZE integration tests + end-to-end stats test.
- **UPSERT `ON CONFLICT DO UPDATE/DO NOTHING` fully implemented.** All forms of
  `INSERT … ON CONFLICT (target) DO UPDATE SET … [WHERE …]` and `DO NOTHING`
  work, including `excluded.*` reads, multi-row inserts, and the
  `index_experimental` unique-index-target path.
- **VDBE execute module split (8 361 → 10 sub-modules).** `vdbe/execute.rs` was
  split via `splitrs` into a 10-file `vdbe/execute/` subtree; `values.rs`
  consolidates all `Value::exec_*` methods.
- **Schema module split (1 920 → 7 sub-modules).** `schema.rs` was split via
  `splitrs` into `schema/mod.rs`, `schema/bootstrap.rs`, `schema/column.rs`,
  `schema/container.rs`, `schema/index.rs`, `schema/table.rs`, `schema/tests.rs`.
- **Schema-cookie invalidation + transparent re-prepare.** DDL now bumps the
  schema cookie; stale cached statements raise `SchemaChanged`; the compat layer
  re-prepares and retries transparently (replacing the fragile `is_ddl` heuristic).
- **Blocking API + `connect_or_create`.** `BlockingSqliteConnection` provides a
  synchronous wrapper; `connect_or_create(uri)` auto-creates missing PostgreSQL/MySQL
  databases.
- **Correlated subqueries, conflict-clause tests, durability tests.** 19 correlated
  subquery tests, 5 conflict-clause tests, WAL durability tests, schema-cookie tests,
  and LIMIT/OFFSET bound-parameter tests all added.

---

## Highlights

- **One facade, many backends.** A single `oxisql::connect(uri)` call dispatches
  to in-memory/persistent embedded engines, PostgreSQL, MySQL, the C-free SQLite
  engine, or Apache DataFusion — by URI scheme.
- **Pure Rust by default.** The default feature set is 100% C-free. C/Fortran
  dependencies are not merely avoided — they are eliminated and proven absent
  (`CC=/usr/bin/false`).
- **Async, trait-based core.** `Connection`, `Transaction`, `ConnectionPool`,
  `Row`, and a 13-variant `Value` type unify every backend behind one ergonomic
  async API.
- **Named parameters everywhere.** `:name`, `$name`, and `@name` work on *all*
  backends as default `Connection` methods — no per-backend implementation
  required.
- **Composable middleware.** `LoggingConnection`, `MetricsConnection`, and
  `RetryConnection` wrap any `Box<dyn Connection>`.
- **TLS without C.** OxiTLS + its `oxitls-rustcrypto-provider` fork for
  PostgreSQL and MySQL — no `ring`, no `openssl-sys`, no `native-tls`.
- **Pooling & migrations.** deadpool-backed pools for every backend and a
  file-based, timestamped migration runner.
- **Optional REPL.** A `oxisql-repl` binary (`repl` feature) with `.help`,
  `.tables`, `.schema <t>`, and `.quit`.

---

## Crate Status

OxiSQL ships as **17 workspace-member crates** — 10 facade/driver crates plus
a 7-crate, C-free `oxisqlite-*` engine — plus **2 non-published, internal
patch-shim crates** (`whoami-patched`, `zstd-shim`) applied via
`[patch.crates-io]`. The patch shims exist solely to satisfy the COOLJAPAN
Pure-Rust policy (dropping `objc2-system-configuration` and the C-FFI `zstd`)
— they are not COOLJAPAN products with their own feature roadmap; see the
note below the tables. (A third shim, `rustls-rustcrypto-patched`, existed
through 0.3.x to fix RUSTSEC-2026-0104; it was retired once the published
`oxitls` crate's own `oxitls-rustcrypto-provider` fork became webpki-free by
construction — see [Pure Rust — FFI eliminated](#pure-rust--ffi-eliminated).)

### Facade & drivers (11)

| Crate | Status | Tests (default / `--all-features`) | Description |
|-------|--------|-------|-------------|
| `oxisql` | Stable | 60 / 135 | Unified facade: `connect` / `connect_pooled` / `connect_pool` / `connect_datafusion` |
| `oxisql-core` | Stable | 126 / 142 | `Connection` / `Transaction` / `ConnectionPool` / `Value` traits; named-parameter default methods; middleware |
| `oxisql-cache` | Stable | 23 / 23 | SQL query-result / prepared-plan caching (`SqlQueryCache`, `SqlPlanCache`, `CachedQueryRunner`); LRU + TTL via `oxistore-cache` |
| `oxisql-parse` | Stable | 178 / 178 | SQL parsing, fluent query builder, logical planner, optimizer |
| `oxisql-embedded` | Stable | 263 / 281 | GlueSQL in-memory + persistent (redb / fjall / sled); schema introspection |
| `oxisql-postgres` | Stable | 53 / 393¹ | Pure Rust `tokio-postgres`, no libpq; OxiTLS/rustcrypto; opt-in logical replication |
| `oxisql-mysql` | Stable | 96 / 96 | Pure Rust `mysql_async`, no libmysqlclient |
| `oxisql-datafusion` | Alpha | 67 / 87 | Apache DataFusion `TableProvider` bridge |
| `oxisql-pool` | Stable | 35 / 52 | deadpool-based pooling for all backends |
| `oxisql-migrate` | Stable | 47 / 49 | File-based SQL migrations, 14-digit timestamps |
| `oxisql-sqlite-compat` | Alpha | 88 / 97 | C-free SQLite engine on top of `oxisqlite-*`; ROLLBACK, UPSERT, transparent schema re-prepare |

¹ The large jump under `--all-features` is the opt-in `replication` feature
(PostgreSQL logical replication / CDC), which brings in a substantial
live-server-gated test suite.

### oxisqlite engine (7)

These crates form the in-tree, C-free fork of limbo. They are **internal** —
consumed by `oxisql-sqlite-compat` and not part of OxiSQL's public surface.

| Crate | Status | Tests (default / `--all-features`) | Description |
|-------|--------|-------|-------------|
| `oxisqlite` | Internal | 38 / 38 | Top-level engine facade / connection entry point |
| `oxisqlite-core` | Internal | 966 / 963 | Storage engine: B-tree (split), pager, WAL, VDBE, transactions, ROLLBACK, ANALYZE, System-R optimizer, WITHOUT ROWID, triggers, TEMP/ATTACH multi-database |
| `oxisqlite-ext` | Internal | ² | Built-in extensions / virtual-table glue |
| `oxisqlite-macros` | Internal | ² | Procedural macros for the engine |
| `oxisqlite-sqlite3-parser` | Internal | 221 / 221 | SQL parser (pre-generated, no `lemon` C generator) |
| `oxisqlite-time` | Internal | ² | Pure-Rust date/time helpers (chrono-based) |
| `oxisqlite-uuid` | Internal | ² | Pure-Rust UUID support |

² No dedicated nextest unit tests of its own — a thin wrapper/extension crate
over `oxisqlite-core`, validated via `oxisqlite-core`'s integration test suite
instead (`oxisqlite-macros` additionally carries 3 ignored doctests).

> Two vendored crates are applied via `[patch.crates-io]`: `whoami-patched`
> (drops `objc2-system-configuration` from the macOS code path, restoring
> Pure Rust Policy v2 §3 compliance) and `zstd-shim` (a Pure-Rust
> `bulk::{Compressor, Decompressor}` shim backed by `oxiarc-zstd`, dropping
> the C-FFI `zstd-sys` crate from the `--all-features` dependency closure).
> Neither is a workspace member nor published to crates.io — see
> [The C-free oxisqlite engine](#the-c-free-oxisqlite-engine) and
> [Pure Rust — FFI eliminated](#pure-rust--ffi-eliminated). (A third shim,
> `rustls-rustcrypto-patched`, fixed RUSTSEC-2026-0104 through 0.3.x; it was
> removed once the published `oxitls` crate's `oxitls-rustcrypto-provider`
> fork became webpki-free by construction, closing the advisory without a
> local patch.)

---

## Installation

Add to your workspace's root `Cargo.toml`:

```toml
# Workspace root Cargo.toml
[workspace.dependencies]
oxisql = { version = "0.4.1", features = ["embedded"] }
```

Or add to a single crate:

```toml
[dependencies]
oxisql = { version = "0.4.1", features = ["embedded", "postgres", "pool-embedded", "migrate"] }
```

### Downstream `[patch.crates-io]` requirement (whoami / zstd)

`[patch.crates-io]` entries only take effect at the **root** `Cargo.toml` of
the final workspace being built — Cargo does not apply a dependency's own
patches on your behalf. OxiSQL's own build is Pure Rust because its
workspace root patches two crates to unpublished, in-tree shim crates
(`crates/whoami-patched`, `crates/zstd-shim`; see
[Pure Rust — FFI eliminated](#pure-rust--ffi-eliminated)). If your
application depends on `oxisql` (directly or transitively, e.g. via
`oxisql-postgres`, `oxisql-mysql`, or `oxisql-datafusion` with the
`columnar`/`parquet` feature) **and** you build with `CC=/usr/bin/false` or
otherwise require a genuinely C-free closure, you must repeat both patches in
*your own* workspace root — they cannot be inherited from OxiSQL's
`Cargo.toml`, and neither shim is published to crates.io (removal of the
underlying C dependency at the source, i.e. getting `whoami` or `arrow-ipc`'s
`zstd` dependency to drop it upstream, was investigated and found not
currently possible — see the version history above for
`objc2-system-configuration`/`zstd-sys` context).

Vendor (copy) `crates/whoami-patched/` and `crates/zstd-shim/` from this
repository into your own workspace, then add:

```toml
# Your application's workspace root Cargo.toml
[patch.crates-io]
whoami = { path = "path/to/whoami-patched" }
zstd   = { path = "path/to/zstd-shim" }
```

Without this, a downstream workspace that merely `[dependencies]`-pulls
`oxisql` will still resolve the upstream `whoami`/`zstd` crates (and their
`objc2-system-configuration`/`zstd-sys` C dependencies) for its *own* build,
even though OxiSQL's own `cargo build --workspace` inside this repository is
C-free. This is a Cargo `[patch]`-scoping limitation, not an OxiSQL defect.

---

## Quick Start

### In-memory embedded (GlueSQL)

```rust,no_run
#[tokio::main]
async fn main() -> Result<(), oxisql::OxiSqlError> {
    let conn = oxisql::connect("memory://").await?;
    conn.execute("CREATE TABLE users (id INTEGER, name TEXT)", &[]).await?;
    conn.execute("INSERT INTO users VALUES (1, 'Alice')", &[]).await?;
    let rows = conn.query("SELECT id, name FROM users", &[]).await?;
    for row in &rows {
        println!("{:?}", row);
    }
    Ok(())
}
```

### Named parameters

`execute_named` and `query_named` are default methods on the `Connection` trait
and are available to all backends with no per-backend implementation. Use
`:name`, `$name`, or `@name` placeholder syntax.

```rust,no_run
use oxisql::prelude::*;

#[tokio::main]
async fn main() -> Result<(), oxisql::OxiSqlError> {
    let conn = oxisql::connect("memory://").await?;
    conn.execute("CREATE TABLE users (id INTEGER, name TEXT)", &[]).await?;
    conn.execute("INSERT INTO users VALUES (1, 'Alice')", &[]).await?;

    let rows = conn.query_named(
        "SELECT id, name FROM users WHERE id = :id",
        &[("id", &1i64 as &dyn ToSqlValue)],
    ).await?;
    for row in &rows {
        println!("{:?}", row);
    }
    Ok(())
}
```

### SQLite with full ROLLBACK (now Pure Rust)

As of 0.1.2 the SQLite path runs on the C-free `oxisqlite` engine and supports
real transactional `ROLLBACK`. The example below opens an in-memory database via
the `sqlite` feature and verifies that a rolled-back `INSERT` leaves no rows.

```rust,no_run
use oxisql::SqliteConnection;
use oxisql::prelude::*;

#[tokio::main]
async fn main() -> Result<(), oxisql::OxiSqlError> {
    let conn = SqliteConnection::open_memory().await?;
    conn.execute("CREATE TABLE t (id INTEGER, name TEXT)", &[]).await?;

    // BEGIN ... ROLLBACK discards the INSERT.
    conn.execute("BEGIN", &[]).await?;
    conn.execute("INSERT INTO t VALUES (1, 'Alice')", &[]).await?;
    conn.execute("ROLLBACK", &[]).await?;

    let rows = conn.query("SELECT COUNT(*) FROM t", &[]).await?;
    println!("rows after ROLLBACK: {:?}", rows); // → one row holding COUNT(*) = 0
    Ok(())
}
```

You can reach the same backend through the facade with
`oxisql::connect("sqlite::memory:")` or `oxisql::connect("sqlite://path/to/file.db")`.

### Pooled connections

```rust,no_run
#[tokio::main]
async fn main() -> Result<(), oxisql::OxiSqlError> {
    // Returns Box<dyn ConnectionPool> — works with any backend URI
    let pool = oxisql::connect_pooled("memory://", 4).await?;
    let conn = pool.get().await?;
    conn.execute("CREATE TABLE t (id INTEGER, name TEXT)", &[]).await?;
    Ok(())
}
```

### Running migrations

```rust,no_run
use oxisql::migrate::{MigrationRunner, scan_migrations};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let conn = oxisql::connect("memory://").await?;
    let migrations = scan_migrations("migrations/")?;
    let mut runner = MigrationRunner::new(migrations);
    runner.run_with_pool(conn.as_ref()).await?;
    Ok(())
}
```

### PostgreSQL with TLS (OxiTLS / rustcrypto)

```rust,no_run
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Plain-text via the facade:
    let conn = oxisql::connect("postgres://user:pass@localhost/mydb").await?;

    // With rustls/OxiTLS (no ring, no openssl-sys):
    let tls_cfg = std::sync::Arc::new(
        rustls::ClientConfig::builder()
            .with_root_certificates(rustls::RootCertStore::empty())
            .with_no_client_auth(),
    );
    let conn_tls = oxisql::connect_with_tls(
        "postgres://user:pass@localhost/mydb",
        Some(tls_cfg),
    ).await?;
    Ok(())
}
```

---

## Backends

### Embedded — GlueSQL (`memory://`)

| Property | Value |
|----------|-------|
| URI | `memory://` |
| Feature flag | `embedded` |
| Storage | In-memory (reset on drop) |
| Pure Rust | Yes |
| Persistent variants | `redb://path`, `fjall://path`, `sled://path` |

The embedded backend wraps [GlueSQL](https://github.com/gluesql/gluesql) for
in-memory SQL. Three persistent variants are available:

| URI | Engine | Feature |
|-----|--------|---------|
| `redb://path/to/file.db` | redb B-tree | `redb` |
| `fjall://path/to/dir` | fjall LSM-tree | `fjall` |
| `sled://path/to/dir` | sled key-value | `sled` |

`export_as_sql()` / `import_from_sql()` round-trip a database as SQL text.

### PostgreSQL (`postgres://`)

| Property | Value |
|----------|-------|
| URI | `postgres://user:pass@host/db` or `postgresql://...` |
| Feature flag | `postgres` |
| Driver | `tokio-postgres` (Pure Rust) |
| TLS | OxiTLS + rustcrypto (no ring, no libssl) |
| libpq | Never required |

Supports prepared statements, transactions, COPY bulk ingestion, LISTEN/NOTIFY,
pipeline batching, extended type mapping (DATE, TIMESTAMP, UUID, JSONB,
NUMERIC, ARRAY), and — behind the opt-in `replication` feature
(`postgres-replication` on the facade) — logical replication (CDC) via
`pgoutput`.

> A C-linked `libpq` path does **not** exist today. If legacy parity is ever
> required it could be added behind an opt-in feature in a future release, but no
> such flag is shipped.

### MySQL (`mysql://`)

| Property | Value |
|----------|-------|
| URI | `mysql://user:pass@host/db` |
| Feature flag | `mysql` |
| Driver | `mysql_async` (Pure Rust) |
| TLS | OxiTLS + rustls-tls (no libssl) |
| libmysqlclient | Never required |

Supports prepared statements, transactions, multi-result-sets for stored
procedures, LOAD DATA bulk ingestion, the binary protocol, and extended type
mapping (DECIMAL, DATETIME(6), JSON, ENUM).

### SQLite-compat via oxisqlite (`sqlite://`)

| Property | Value |
|----------|-------|
| URI | `sqlite://path/to/file.db` or `sqlite::memory:` |
| Feature flag | `sqlite` |
| Engine | [oxisqlite](#the-c-free-oxisqlite-engine) — C-free fork of [limbo](https://github.com/tursodatabase/limbo) 0.0.22 |
| Status | Alpha |
| ROLLBACK | **Supported** (BEGIN / COMMIT / ROLLBACK, WAL-safe) |
| Pure Rust | **Yes** — no `libsqlite3`, no `mimalloc`, no `lemon` |

The SQLite-compatible backend sits on top of the in-tree `oxisqlite-*` engine.
Full transactional rollback and UPSERT work. `SAVEPOINT` is fully implemented
with WAL-based page-state restoration. As of 0.3.3,
`SqliteConnection::open_from_bytes(bytes)` / `SqliteConnectionBlocking::open_from_bytes(bytes)`
open a database directly from an in-memory byte buffer — no temporary file —
mirroring SQLite's `sqlite3_deserialize()`. See
[Known Limitations](#known-limitations) for current open items.

### DataFusion OLAP (`datafusion://`)

| Property | Value |
|----------|-------|
| URI | `datafusion://` (not a `Connection` — use `connect_datafusion`) |
| Feature flag | `datafusion` |
| Engine | Apache DataFusion |
| Status | Alpha |

Use `oxisql::connect_datafusion("datafusion://")` to obtain an `OxiSqlContext`.
Tables from any backend can be registered via `datafusion::register_table` for
cross-backend OLAP queries (filter / projection / limit pushdown).

---

## Feature Flags

| Feature | URI scheme | Backend | Notes |
|---------|------------|---------|-------|
| `embedded` | `memory://` | GlueSQL in-memory | Base embedded feature |
| `postgres` | `postgres://` / `postgresql://` | tokio-postgres | Pure Rust, no libpq |
| `postgres-replication` | — | `PgReplicationConnection` (not `connect()`-dispatched) | Logical replication (CDC) via `pgoutput`; implies `postgres` |
| `mysql` | `mysql://` | mysql_async | Pure Rust, no libmysqlclient |
| `sqlite` | `sqlite://` / `sqlite::memory:` | oxisqlite (C-free) | Pure Rust, no libsqlite3; Alpha |
| `redb` | `redb://` | redb B-tree | Persistent embedded; implies `embedded` |
| `fjall` | `fjall://` | fjall LSM-tree | Persistent embedded; implies `embedded` |
| `sled` | `sled://` | sled key-value | Persistent embedded; implies `embedded` |
| `datafusion` | `datafusion://` | DataFusion OLAP | Use `connect_datafusion`; Alpha |
| `pool-postgres` | — | deadpool + tokio-postgres | Pulls in `postgres` behaviour |
| `pool-mysql` | — | deadpool + mysql_async | Pulls in `mysql` behaviour |
| `pool-embedded` | — | EmbeddedPool | In-memory pool |
| `pool-sqlite-compat` | — | SqliteCompatPool | Alpha; pulls in `sqlite` |
| `migrate` | — | MigrationRunner | File-based SQL migrations |
| `cache` | — | `oxisql::cache` (`SqlQueryCache` / `SqlPlanCache` / `CachedQueryRunner`) | LRU + TTL query-result / prepared-plan caching via `oxisql-cache` |
| `repl` | — | `oxisql-repl` binary | `.help` / `.tables` / `.schema <t>` / `.quit` |

### Common feature combinations

```toml
# In-memory only
oxisql = { version = "0.4.1", features = ["embedded"] }

# PostgreSQL + pooling
oxisql = { version = "0.4.1", features = ["postgres", "pool-postgres"] }

# MySQL + migrations
oxisql = { version = "0.4.1", features = ["mysql", "pool-mysql", "migrate"] }

# C-free SQLite + pooling
oxisql = { version = "0.4.1", features = ["sqlite", "pool-sqlite-compat"] }

# All OLTP backends + pooling + migrations
oxisql = { version = "0.4.1", features = [
    "embedded", "postgres", "mysql", "sqlite",
    "pool-embedded", "pool-postgres", "pool-mysql", "pool-sqlite-compat",
    "migrate",
] }

# Full stack including DataFusion OLAP, logical replication, and the REPL
oxisql = { version = "0.4.1", features = [
    "embedded", "postgres", "postgres-replication", "mysql", "sqlite", "datafusion",
    "pool-embedded", "pool-postgres", "pool-mysql",
    "migrate", "repl",
] }
```

---

## Architecture

```
oxisql (facade crate)
  |
  +-- oxisql-core          traits: Connection, Transaction, Row, Value, OxiSqlError
  |                        PreparedStatement, ConnectionPool, SchemaInspector,
  |                        LoggingConnection, RetryConnection, MetricsConnection
  |                        named-parameter default methods (:name / $name / @name)
  |
  +-- oxisql-cache         SqlQueryCache / SqlPlanCache / CachedQueryRunner
  |                        LRU + TTL query-result / prepared-plan caching,
  |                        built on oxistore-cache
  |
  +-- oxisql-parse         SQL parsing (sqlparser), fluent QueryBuilder,
  |                        logical planner (Scan/Filter/Project/Join/Aggregate),
  |                        optimizer (predicate pushdown, join reordering)
  |
  +-- oxisql-embedded      GlueSQL in-memory + fjall/redb/sled persistent backends
  |                        export_as_sql() / import_from_sql()
  |
  +-- oxisql-postgres      tokio-postgres wire client, OxiTLS/rustcrypto TLS,
  |                        COPY, LISTEN/NOTIFY, pipeline batching
  |
  +-- oxisql-mysql         mysql_async wire client, rustls TLS,
  |                        bulk LOAD DATA, multi-result-sets, binary protocol
  |
  +-- oxisql-sqlite-compat C-free SQLite-compat (Alpha) — ROLLBACK supported
  |       |                LRU prepared-statement cache
  |       |
  |       +-- oxisqlite                 engine facade / connection entry point
  |             |
  |             +-- oxisqlite-core              B-tree, pager, WAL, VDBE, ROLLBACK, ANALYZE, System-R optimizer
  |             +-- oxisqlite-sqlite3-parser    SQL parser (no lemon C generator)
  |             +-- oxisqlite-ext               extensions / vtab glue
  |             +-- oxisqlite-macros            engine procedural macros
  |             +-- oxisqlite-time              pure-Rust date/time helpers
  |             +-- oxisqlite-uuid              pure-Rust UUID support
  |
  +-- oxisql-pool          deadpool-based pools for all backends
  |                        OxidbPool enum, PoolMetrics, PoolConfig
  |
  +-- oxisql-migrate       File-based SQL migrations, 14-digit timestamps
  |                        MigrationRunner, run_with_pool(), status(), pending()
  |
  +-- oxisql-datafusion    DataFusion TableProvider bridge,
                           OxiSqlContext, filter/projection/limit pushdown
```

### Value type system

The `Value` enum (in `oxisql-core`) has 13 variants covering the full type
surface of all supported backends. A companion `BorrowedValue<'a>` type provides
a zero-allocation borrowed view for high-throughput row iteration.

| Variant | SQL types |
|---------|-----------|
| `Value::Null` | NULL |
| `Value::Bool` | BOOLEAN |
| `Value::I64` | INTEGER, BIGINT, SMALLINT |
| `Value::F64` | REAL, DOUBLE PRECISION, FLOAT |
| `Value::Text` | TEXT, VARCHAR, CHAR |
| `Value::Blob` | BYTEA, BLOB, VARBINARY |
| `Value::Decimal` | NUMERIC, DECIMAL |
| `Value::Timestamp` | TIMESTAMP, TIMESTAMPTZ, DATETIME |
| `Value::Date` | DATE |
| `Value::Time` | TIME |
| `Value::Uuid` | UUID |
| `Value::Json` | JSON, JSONB |
| `Value::Array` | ARRAY types (PostgreSQL) |

Full round-trip mapping is implemented for all RDBMS backends.

### Named parameters

`Connection::execute_named` and `Connection::query_named` are default methods on
the `Connection` trait (defined in `oxisql-core`). Every backend inherits them
automatically — no per-backend implementation is required. Placeholder syntax:
`:name`, `$name`, or `@name`. The default methods rewrite named placeholders to
positional form before dispatch; on a missing binding they return
`OxiSqlError::Params`. Parameter values implement the `ToSqlValue` trait. Import
via `use oxisql::prelude::*`.

### Query middleware

OxiSQL provides composable middleware over any `Box<dyn Connection>`:

- `LoggingConnection` — logs every SQL operation with timing via the `log` crate.
- `RetryConnection` — retries transient failures with a configurable `RetryPolicy`.
- `MetricsConnection` — collects per-operation counters and latencies.

A `MultiConnection` wrapper additionally lets a single handle fan out across
several backend connections.

### Inter-Oxi dependencies

- **Depends on:** OxiTLS (transport / TLS), `oxicode` (row serde),
  OxiCrypto (encryption-at-rest), OxiStore (lower storage layer).
- **Depended on by:** `oxirouter`, `oxirs`, `oxify`, `oxigdal-db-connectors`,
  `oximedia`, `oxigaf`, `oxirag`.

---

## The C-free oxisqlite engine

Before 0.1.2, OxiSQL's "Pure Rust SQLite" claim was not actually true: the
upstream limbo engine transitively pulled in C code. `oxisqlite` fixes that. It
is an in-tree fork of **limbo 0.0.22**
(commit `e59c5185ddc2b6451307324042efd81115376df1`, MIT) from which every C
touchpoint has been excised:

1. **`mimalloc` C allocator** — removed; the engine uses the system/Rust
   allocator.
2. **`lemon.c` parser generator** — removed; the generated `parse.rs` /
   `keywords.rs` are pre-generated and committed, so no C generator runs at build
   time.
3. **`built` / `git2` build-info** — removed; build metadata is hardcoded as
   `const`s instead of being collected by a C-backed build script.

Because all three are gone, the workspace builds with the C compiler disabled:

```text
CC=/usr/bin/false cargo build --workspace   # → exit 0
```

### Engine crates

| Crate | Role | ~LOC |
|-------|------|-----:|
| `oxisqlite` | Engine facade / connection entry point | 2,400 |
| `oxisqlite-core` | Storage engine: B-tree, pager, WAL, VDBE, transactions, ROLLBACK | 88,000 |
| `oxisqlite-ext` | Built-in extensions / virtual-table glue | 1,300 |
| `oxisqlite-macros` | Procedural macros for the engine | 1,000 |
| `oxisqlite-sqlite3-parser` | SQL parser (pre-generated, no `lemon`) | 16,400 |
| `oxisqlite-time` | Pure-Rust date/time helpers | 1,600 |
| `oxisqlite-uuid` | Pure-Rust UUID support | 137 |

### Fork lineage & licensing

- Base: limbo 0.0.22 (`e59c5185`, MIT).
- ROLLBACK machinery: ported from `turso_core` 0.7.0-pre.5 (MIT).
- The GPL `julian_day_converter` dependency was replaced by an inline,
  chrono-based pure-Rust implementation
  (`oxisqlite-core/functions/julian_day.rs`); the `cfg_block` crate was dropped.
- `deny.toml` allows the engine's licenses (Zlib, Unicode-3.0, MPL-2.0,
  CDLA-Permissive-2.0); `cargo deny check licenses bans sources` passes.

The full lineage and third-party attributions are recorded in the root
[`NOTICE`](NOTICE).

---

## Pure Rust — FFI eliminated

OxiSQL's default build contains no C, C++, or Fortran. Each conventional native
dependency is replaced by a Pure-Rust equivalent:

| Native library | Replaced by |
|----------------|-------------|
| `libpq` | `tokio-postgres` (Pure Rust) |
| `libmysqlclient` | `mysql_async` (Pure Rust) |
| `libsqlite3` (via `rusqlite-sys`) | `oxisqlite` (C-free fork of limbo; Pure Rust) |
| `libssl` / `native-tls` / `ring` | OxiTLS (`oxitls`) + its `oxitls-rustcrypto-provider` fork |

The TLS provider is additionally hardened against RUSTSEC-2026-0104 (a
`rustls-webpki` CRL-parsing panic) — but no local patch is needed for this
anymore: `oxitls`'s published `oxitls-rustcrypto-provider` fork is
webpki-free by construction, so the vulnerable code path simply isn't in the
dependency graph. (Through 0.3.x this was closed by a vendored
`rustls-rustcrypto-patched` shim applied via `[patch.crates-io]`; that shim
was retired once `oxitls-rustcrypto-provider` absorbed the fix upstream of
this workspace. `rustls-webpki` itself also now resolves to 0.103.13 via the
normal dependency graph — RUSTSEC-2026-0104 affected the 0.102.x line —
confirmed via `Cargo.lock`.)

Build-time proof (the default workspace build, with the C compiler forced to
fail):

```text
CC=/usr/bin/false cargo build --workspace   # → exit 0
cargo build --workspace                      # → 0 warnings
cargo deny check licenses bans sources       # → PASS
```

`cargo audit` currently reports 3 vulnerabilities and 4 unmaintained-crate
warnings, none with a fix applied yet. Only one touches OxiSQL's production
surface: `rsa` 0.9.10's Marvin Attack timing side-channel (RUSTSEC-2023-0071,
medium severity, no safe upgrade exists), pulled in via `oxitls`'s
`oxitls-rustcrypto-provider` fork for the PostgreSQL/MySQL TLS path. The
other two vulnerabilities — `quick-xml`
0.26.0 (RUSTSEC-2026-0194, RUSTSEC-2026-0195, both high severity) — are
reachable only through `oxisqlite-core`'s dev-only `pprof`/`inferno` benchmark
dependency (the `FlamegraphProfiler` utility), never through a production
build. The 4 unmaintained-but-not-vulnerable crates are `paste`,
`rustls-pemfile`, `fxhash`, and `instant` (the latter two via `sled` →
`oxisql-embedded`).

#### SECURITY: RUSTSEC-2023-0071 (Marvin Attack) is unreachable in OxiSQL

`deny.toml` carries an explicit `[advisories] ignore` entry for
RUSTSEC-2023-0071 rather than a code workaround, because the vulnerable code
path cannot be reached from any OxiSQL API:

- The advisory's timing side-channel is only observable during an RSA
  PKCS#1 v1.5 **private-key** decrypt/sign operation (e.g. a TLS server
  performing an RSA key-exchange, or a TLS server validating a client
  certificate signed with RSA).
- OxiSQL is TLS-**client**-only: `oxisql-postgres` and `oxisql-mysql` both
  build their `rustls::ClientConfig` with `.with_no_client_auth()`
  (`crates/oxisql-postgres/src/builder.rs`,
  `crates/oxisql-mysql/src/lib.rs`) — no client certificate, hence no RSA
  client-auth private-key operation, is ever performed.
- OxiSQL never runs as a TLS server, so no RSA key-exchange or
  certificate-signing private-key operation happens there either.
- The only RSA operation actually exercised anywhere in the dependency
  closure is **public-key signature verification** of the server's
  certificate chain, which is not what Marvin's timing side-channel
  attacks (verification uses the public exponent, not the private key, and
  has no secret-dependent timing to leak).
- `rsa` 0.9.10 reaches the graph solely via `oxitls-rustcrypto-provider` /
  `oxitls-adapter-rustls-rustcrypto` (`oxitls`'s crypto provider for
  `rustls`), confirmed with `cargo tree -i rsa` — there is no other path
  into the workspace.

No safe upgrade exists upstream yet; this entry will be revisited if
`oxitls-rustcrypto-provider`/`rsa` publish a fix, or if OxiSQL ever grows a
TLS-server or mTLS-client-auth code path.

---

## Known Limitations

These are OxiSQL's own roadmap items, not upstream blockers:

- **SAVEPOINT** is fully implemented in oxisqlite with WAL-based page-state restoration.
- **Row triggers** (`CREATE TRIGGER` / `DROP TRIGGER`) are implemented: all six
  `BEFORE`/`AFTER` × `INSERT`/`UPDATE`/`DELETE` combinations fire per row, with
  `WHEN` guards, `UPDATE OF (cols)` filtering, `OLD.*`/`NEW.*` references, and
  `RAISE(ABORT|FAIL|ROLLBACK|IGNORE)`. Triggers persist in `sqlite_schema` and
  survive a reopen. Four limitations remain, each a typed error rather than a
  silent difference: a `SELECT` body command is supported only in the
  `SELECT RAISE(...)` form; `CREATE TEMP TRIGGER` needs the not-yet-implemented
  temp schema; `INSTEAD OF` triggers are stored but cannot fire while writing
  through a view is itself rejected; and `ABORT`/`FAIL`/`ROLLBACK` all discard
  the same amount of uncommitted work, because the engine has no statement
  journal (which is already true of its `NOT NULL`/`UNIQUE` failures).
- **`ATTACH` / `DETACH DATABASE` and `TEMP` tables** are not implemented; both
  are clean parse-time rejections. They share one prerequisite (a
  per-connection multi-database pager plus a second catalog namespace) and are
  tracked to land together — see `TODO.md` Q11b/Q11d.
- **Prepared-statement cache** is active; `Statement::reset()` correctly clears change counts; the compat layer re-prepares transparently on `SchemaChanged`.
- **PostgreSQL / MySQL live-server tests** are `#[ignore]`-gated and require an
  accessible server. All non-live unit and compile-time tests pass without
  external services (31 skipped tests workspace-wide under default features,
  90 under `--all-features` — the difference is mostly the newly-gated
  PostgreSQL logical-replication tests — almost all live-server-gated, with a
  handful of fuzz/stress cases).

See [`TODO.md`](TODO.md) for the full, tracked roadmap.

---

## License, NOTICE & Authors

Licensed under the **Apache License, Version 2.0**.
See [`LICENSE`](LICENSE) for the full text and [`NOTICE`](NOTICE) for the
oxisqlite fork lineage and third-party attributions.

Copyright © 2024–2026 COOLJAPAN OU (Team Kitasan).
Repository: <https://github.com/cool-japan/oxisql>
