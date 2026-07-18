# Changelog

All notable changes to OxiSQL will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.4.0] - 2026-07-18

### Changed

- **Inter-crate dependency floors promoted to the exact full-triple `0.4.0`** (all 17 crates): the whole family is released together at `0.4.0`, and every internal dependency requirement is pinned to `"0.4.0"` (`>= 0.4.0, < 0.5.0`) instead of a minor-only caret (`"0.4"` = `>= 0.4.0`). This carries the `0.3.4` stale-lock fix forward as the clean baseline for the `0.4.x` line: because each internal floor names the exact patch, the resolver can never pair a newer caller with an older family member left behind in a stale `Cargo.lock` (the `open_from_bytes` "resolves yet fails to compile" failure mode documented under `0.3.4`). The minor bump also gives the two internal breaking changes that shipped in `0.3.3` — `oxisqlite::Params::Named`'s key type (`String` → `Cow<'static, str>`) and `oxisqlite-sqlite3-parser`'s lexer `Token` losing its lifetime parameter — a proper semantic-version home. **No source-code changes since `0.3.3`** — engine behavior, wire protocols, and the public `oxisql` API are byte-for-byte identical; this is a packaging-metadata release.

## [0.3.4] - 2026-07-18

### Fixed

- **Inter-crate dependency lower bounds tightened to prevent stale-lock breakage** (all crates): internal dependency requirements that were declared with the minor-only caret `"0.3"` / `"0.4"` (i.e. `>= 0.3.0`) are now pinned to the exact patch floor `"0.3.4"` (`>= 0.3.4, < 0.4.0`). The loose lower bound let a stale `Cargo.lock` keep an older engine paired with a newer caller: `oxisql-sqlite-compat` / `oxisqlite` `0.3.3` call the `open_from_bytes` API introduced in `oxisqlite` / `oxisqlite-core` `0.3.3`, but the `"0.3"` requirement was still satisfied by a locked `0.3.2` engine that predates that API — producing a resolve that "met" the version requirement yet failed to compile (`open_from_bytes` not found). The same looseness existed one level down (`oxisqlite` → `oxisqlite-core`, `oxisqlite-core` → `oxisqlite-sqlite3-parser` / `oxisqlite-time` / `oxisqlite-uuid`). Pinning every internal floor to `0.3.4` makes the resolver reject any pre-`0.3.4` family member, so the whole set always resolves consistently. **No source-code changes** — engine behavior is identical to `0.3.3`; this is a packaging-metadata-only patch.

## [0.3.3] - 2026-07-17

### Added

- **`CREATE VIEW` / `DROP VIEW` and querying views** (`oxisqlite-core`): views are now first-class. `type='view'` rows in `sqlite_schema` are parsed and registered on open (name, stored SQL, `SELECT` body, and any explicit `CREATE VIEW v(a, b, c)` column list), and a malformed or cyclic view degrades gracefully — that single object becomes unavailable with a clear error on use, while schema loading never aborts and `type='trigger'` rows stay loadable-inert. Referencing a view in a `FROM` clause expands it (rename-safe, honoring table aliases) into the existing derived-table machinery, so plain `SELECT`s, `WHERE`/`ORDER BY`/`LIMIT`, `count(*)`/aggregates, multi-way `LEFT JOIN`s with repeated aliases, `UNION ALL` compound bodies, and views-referencing-views all work; a depth guard turns a reference cycle into a clean parse error instead of a stack overflow. Runtime `CREATE VIEW [IF NOT EXISTS]` / `DROP VIEW [IF EXISTS]` write and remove the `sqlite_schema` row and update the in-memory catalog (mirroring `CREATE`/`DROP TABLE`), with SQLite-matching diagnostics: duplicate name errors, `DROP TABLE` on a view says "use DROP VIEW …", `DROP VIEW` on a table says "use DROP TABLE …", and `INSERT`/`UPDATE`/`DELETE` through a view returns "cannot modify X because it is a view" (never a panic). Validated end-to-end against a real 9.5 MB PROJ database (7 views over 40 tables/21 indexes/37 `INSTEAD OF` triggers): all seven views' `count(*)` and representative content queries match `sqlite3` byte-for-byte.
- **Compound (`UNION [ALL]`/`INTERSECT`/`EXCEPT`) `SELECT` as a FROM-clause subquery / CTE / view body** (`oxisqlite-core`): a compound `SELECT` can now be used wherever a derived table can (`FROM (SELECT ... UNION ALL SELECT ...)`, `WITH c AS (... UNION ...)`, and view bodies), driven through a coroutine like a plain subquery — previously rejected with "Only non-compound SELECT queries are currently supported". A latent constant-hoisting bug that made a later compound arm's constant result-column clobber an earlier arm's (when arms share a coroutine's result registers) is fixed at the same time.
- **Open a database from an in-memory image** (`oxisqlite-core`, `oxisqlite`, `oxisql-sqlite-compat`): new `open_from_bytes` entry points that open a SQLite database directly from a byte buffer (e.g. `include_bytes!`, `VACUUM INTO`, or `sqlite3_serialize()` output) with no temporary file — enabling WASI/browser/read-only-filesystem use. `oxisqlite_core::Database::open_from_bytes(bytes, enable_mvcc)` copies the image into a fresh in-memory page store (`MemoryFile::from_bytes`, also newly public) and is **not** gated by the `fs` feature; `oxisqlite::Database::open_from_bytes(bytes)` mirrors SQLite's `sqlite3_deserialize()` and returns a shareable `Database` that can be `connect()`ed multiple times; `oxisql_sqlite_compat::SqliteConnection::open_from_bytes(bytes)` (async) and `SqliteConnectionBlocking::open_from_bytes(bytes)` (sync, `--features blocking`) expose it through the rusqlite-replacement layer. Malformed input (too short, wrong magic, or an invalid page size) returns a typed error and never panics. Any valid on-disk page size (512/1024/2048/4096/8192/16384/32768/65536) is accepted for reading; the 65536 page size remains write-limited by a pre-existing engine `u16` usable-space constraint.
- **PostgreSQL logical replication** (`oxisql-postgres`, new `replication` feature; `postgres-replication` on the `oxisql` facade, which implies `postgres`): CDC-style logical replication support via `PgReplicationConnection::{connect, identify_system, create_replication_slot, drop_replication_slot, start_logical_replication}`, returning a `ReplicationStream` (`futures::Stream<Item = Result<ReplicationEvent, PgError>>`) with `ack`/`standby_status_update`/`decode_tuple`. Includes pgoutput wire-protocol decoding (`Begin`/`Commit`/`Origin`/`Relation`/`Type`/`Insert`/`Update`/`Delete`/`Truncate`/`Message`), LSN tracking (parses/formats PostgreSQL's `"X/Y"` hex log-sequence-number form), `COPY BOTH` streaming (`XLogData`/keepalive decoding, `StandbyStatusUpdate` encoding via a background reader + keepalive task), binary-format tuple-value decoding, and PostgreSQL array-literal (`{...}`) text-format decoding (brace/comma/quote/escape-aware, not a naive `split(',')`). Brings in a new `postgres-protocol` workspace dependency (aliased as `fallible-iterator-02` internally to avoid a `fallible-iterator` major-version conflict with `oxisqlite-core`).
- **`ON CONFLICT` on `CREATE TABLE` constraints** (`oxisqlite-core`): column- and table-level `UNIQUE`/`PRIMARY KEY` constraints now accept a conflict-resolution clause, e.g. `CREATE TABLE t (id INTEGER PRIMARY KEY, x INTEGER UNIQUE ON CONFLICT REPLACE)` — previously an unconditional `unimplemented!()`.
- **`CREATE TABLE ... AS SELECT ...`** (`oxisqlite-core`): CTAS synthesizes a plain column-list table from the `SELECT`'s result set; supports aggregates, joins, and empty results, and survives reopen.
- **`EXCEPT` / `INTERSECT`** (`oxisqlite-core`): compound-`SELECT` set operators beyond `UNION`/`UNION ALL`, with `NULL`-equals-`NULL` membership semantics and strict left-to-right grouping of mixed chains (`A UNION B EXCEPT C` = `(A UNION B) EXCEPT C`).
- **`OFFSET` / `ORDER BY` / `WITH` on compound `SELECT`** (`oxisqlite-core`): all three clauses are now accepted on the outer compound statement (e.g. `WITH c AS (...) SELECT ... UNION SELECT ... ORDER BY id DESC`); `OFFSET` shares its countdown across arm boundaries and `ORDER BY` genuinely interleaves rather than sorting each arm independently.
- **`FROM (t1 JOIN t2 ON ...)`** (`oxisqlite-core`): parenthesized join sources in the `FROM` clause are now merged into the surrounding join order instead of panicking.
- **Virtual-table `ORDER BY` pushdown** (`oxisqlite-core`): a real `OrderByInfo` is now offered to a virtual table's `xBestIndex`, and the engine elides its own sorter whenever the vtab reports the ordering is already satisfied.
- **Named parameter binding** (`oxisqlite`): `:name` / `@name` / `$name` / `#name` placeholders are now implemented end-to-end (`bind_named_params`) in `Statement::query`/`execute` — previously an unconditional panic (`todo!()`) whenever named parameters were used.
- **Windows file locking** (`oxisqlite-core`, `io/windows.rs`): real `LockFileEx`/`UnlockFileEx`-backed locking via a new `windows-sys` dependency (Windows-only; never enters the dependency graph on other targets) — previously an `unimplemented!()` stub.
- **Server version accessors**: `PgConnection::server_version() -> Option<&str>` (`oxisql-postgres`) and `MyConnection::server_version() -> Result<String, MysqlError>` (`oxisql-mysql`), plus `BackendInfo::from_postgres_connection`/`from_mysql_connection` (`oxisql`) to populate `BackendInfo.version` from a live connection.
- **`display_error()` helper** (`oxisql`): renders a clean, user-facing error message (now used by the `oxisql-repl` binary) instead of a raw GlueSQL parser-error debug-dump.
- **New `PgError` variants** (`oxisql-postgres`): `Replication`, `Protocol`, and a `From<std::io::Error>` conversion.
- **`REGEXP` operator / `regexp()` function** (`oxisqlite-core`): `X REGEXP Y` (equivalently `regexp(Y, X)`) is now a recognized function — an unanchored regex search over the `regex` crate's dialect, three-valued `NULL` handling, and a clean constraint error (never a panic) on a malformed pattern; previously not implemented at all.
- **`printf()` format specifiers** (`oxisqlite-core`): `%i` (alias for `%d`), `%x`/`%X` (hex), `%o` (octal), `%c` (first character of the stringified argument), and `%e`/`%E` (C-style scientific notation, `d.dddddde±dd`) are now supported; previously only bare `%d`/`%s` (and literal `%%`) worked. Flag/width/precision modifiers (e.g. `%05d`, `%.3s`) remain a documented TODO.

### Changed

- **`oxiarc-zstd` bumped to `0.3.6`** (`crates/zstd-shim`).
- **Large internal modules split via `splitrs`** (`oxisqlite-core`, `oxisqlite-sqlite3-parser`): `json/jsonb.rs`, `storage/pager.rs`, `functions/datetime.rs`, `translate/expr.rs`, `translate/insert.rs`, `types.rs`, `util.rs`, and the parser's `ast/fmt.rs`/`ast/mod.rs` were split into smaller per-concern files to stay under the workspace's 2000-line-per-file policy; no functional changes.
- **`chrono` feature footprint reduced** (`oxisqlite-ext`, `oxisqlite-time`): switched from the `"clock"` feature to `"now"`, dropping a direct pull of `iana-time-zone` (and, on macOS, `core-foundation-sys`) from these two crates. `oxisqlite-core` itself still requires `"clock"`, since `datetime('now','localtime')`/`datetime('now','utc')` perform a genuine host-timezone conversion.
- **`Params::Named` now stores `Cow<'static, str>` keys** (`oxisqlite`): changed from `Vec<(String, Value)>` to `Vec<(Cow<'static, str>, Value)>` so binding a `'static` placeholder-name literal (the common case, e.g. `[(":id", value)]`) borrows instead of allocating; owned `String` keys still work unchanged. Breaking for any code that pattern-matches on `Params::Named`'s inner tuple type directly.
- **SQL tokenizer `Token` type simplified** (`oxisqlite-sqlite3-parser`): the lifetime-parameterized `Token<'i>(usize, &'i [u8], usize)` is now the lifetime-free `Token(usize, Cow<'static, str>, usize)` — an exact-spelling keyword token borrows its canonical `'static` string at zero cost, everything else (identifiers, literals, differently-cased keywords) still allocates. Breaking for any direct consumer of the lexer's `Token` type.
- **`NATURAL JOIN` common-column detection** (`oxisqlite-core`): rewritten from an O(n²) nested loop to a `HashSet` precomputation; same results, better performance on wide joins.
- **Lazy view-column resolution keeps connection opens cheap** (`oxisqlite-core`): a `CREATE VIEW`'s output columns are now inferred on first `PRAGMA table_info` use rather than eagerly for every view on every connection open. Planning each view body at schema-load time (for a database with large views such as PROJ's `object_view`, a wide `UNION ALL`) added measurable per-open cost even though the body is re-planned from scratch whenever the view is actually referenced in a query, and the inferred column list is consulted only by `PRAGMA table_info`. Opening a bundled database with several views is now back to its pre-view-support cost; view query results and `PRAGMA table_info` output are unchanged.

### Fixed

- **Derived-table / view column affinity** (`oxisqlite-core`): a FROM-clause subquery (and hence a view) whose result column is a direct column reference now inherits that column's declared type, so a predicate like `WHERE code = '1074'` against a view still applies the base column's numeric affinity and matches the integer `1074` — previously every derived column defaulted to `BLOB`/no-affinity, silently returning no rows for such comparisons.
- **≥64-column index key sort-order overflow** (`oxisqlite-core` `types::IndexKeySortOrder`): building the ASC/DESC bitfield for an index (or covering-index key spec) with 64 or more columns did `1u64 << i` for `i ≥ 64`, panicking with "attempt to shift left with overflow" in debug (and silently wrapping in release). This reproduced on a plain `SELECT count(*)` over a wide (74-column) table whose covering scan spans every column. Columns past the 64th now fall back to `ASC` (the same value an unset bit already yields), matching the previous behavior for every ≤63-column index.
- **B-tree index-page balance panic / corruption** (`oxisqlite-core` storage/B-tree): inserting into a b-tree could panic (`attempt to subtract with overflow` in debug, an out-of-bounds slice index in release — corrupting the page silently) whenever `insert_into_cell` was asked to place a cell at a logical position beyond the page's physical cell count while the page already held overflow cells. This happened during parent balancing of an **interior index** page: a large index divider cell that did not fit was correctly deferred to `overflow_cells`, but a smaller following divider that still fit in the leftover free space then took the in-place insert path at `cell_idx > cell_count`, underflowing the `cell_count - cell_idx` pointer-shift count. In practice it reproduced on real, index-heavy databases (e.g. a plain `JOIN` whose planner-built transient automatic index over a large table grew deep enough to rebalance an interior index page). `insert_into_cell` now takes the in-place path only when the target position lies within the physical cell array (`cell_idx <= cell_count`), otherwise appending the cell to `overflow_cells` in logical order so it is re-merged when the page itself is balanced.
- **Index-based access for `DELETE`/`UPDATE`** (`oxisqlite-core`, `index_experimental` feature): `optimize_table_access` had been unconditionally disabled for `DELETE`/`UPDATE` plans ever since a real, upstream-reported corruption bug — using a secondary index to drive `DELETE`'s row loop while also deleting from that same index cursor as per-row maintenance could corrupt the traversal and delete the wrong rows once a page rebalance happened mid-scan (see [tursodatabase/limbo#1714](https://github.com/tursodatabase/limbo/issues/1714)); `UPDATE` had an analogous hazard whenever `SET` changed the driving index's own key columns. In the meantime, every `DELETE`/`UPDATE` fell back to a full table scan regardless of available indexes. The optimizer now runs its normal access-method selection and keeps the result only when proven safe (a non-looping rowid lookup, or a range scan/seek over a cursor whose own key the statement provably cannot shift under it); anything else still reverts to the previous full-table-scan fallback — so indexes are usable again for the common safe cases without reintroducing the corruption.
- **MVCC transaction-removal race** (`oxisqlite-core` mvcc): concurrent commit/rollback could access an already-removed transaction and panic; root-caused to `drop_unused_row_versions()` dropping a row version still owned by a live tracked transaction. All affected call sites now return typed errors instead of panicking.
- **MVCC commit-ordering durability bug** (`oxisqlite-core` mvcc): `commit_tx()` previously marked a transaction visible in-memory *before* persisting it to the write-ahead log, risking data loss on a crash between the two steps; persistence now happens first, and a failed persist reverts the transaction to `Active` and returns an error instead of silently losing it.
- **WAL checkpoint bookkeeping** (`oxisqlite-core` storage): a fully-backfilled `Passive` checkpoint used to skip resetting the WAL's in-memory frame bookkeeping (only `Restart`/`Truncate` did), leaving stale state around; the header's `checkpoint_seq`/`salt_1` now also evolve by increment (instead of only re-randomizing `salt_1`) on every mode once fully backfilled, matching SQLite's own WAL-generation convention. A new `trim_backfilled_frames` step also prevents `frame_cache`/`pages_in_frames` from growing without bound across repeated partial (non-fully-backfilled) `Passive` checkpoints on a long-running connection.
- **On-disk page-corruption handling** (`oxisqlite-core` storage/B-tree): reading a corrupt or maliciously-crafted database file could previously panic — via `assert!`/`.unwrap()` on an out-of-range cell/freeblock pointer, an untrusted page-type byte, or `defragment_page`'s `unimplemented!("corrupted page")`/`compute_free_space`'s `todo!("corrupt")` — instead of failing cleanly. These sites (and others reached by following an untrusted child/rightmost page pointer) now return `LimboError::Corrupt` instead.
- **`UPDATE ... RETURNING`** (`oxisqlite-core`): previously returned zero rows silently (the clause was parsed but `Insn::ResultRow` was never emitted); now returns the updated rows.
- **`ALTER TABLE ... RENAME`** (`oxisqlite-core`): previously crashed on any database containing a `VIEW`, `TRIGGER`, or virtual table anywhere in the schema, even ones unrelated to the table being renamed; now renames safely.
- **`ALTER TABLE ... ADD COLUMN ... DEFAULT <non-constant>`** (`oxisqlite-core`): the rejection is now classified as `LimboError::Constraint` ("Runtime error: ...") instead of `LimboError::ParseError`, matching real SQLite's own error classification for this case (diagnostic-classification only; the statement was already, and still is, rejected).
- **Virtual-table multi-row `INSERT`** (`oxisqlite-core`): `INSERT INTO vtab VALUES (...), (...), ...` previously silently kept only the last row; all rows are now inserted.
- **Virtual-table schema/column introspection** (`oxisqlite-core`, `oxisqlite-ext`): `CREATE VIRTUAL TABLE` no longer does a wasteful (and, for a module with side effects, unsound) extra `xCreate`-then-`xDestroy` round trip just to synthesize a column-name comment persisted into `sqlite_schema.sql`; column names are now resolved on demand from the single, live vtab instance the `VCreate` instruction already creates, the same way `PRAGMA table_info` and query compilation already read them.
- **`IN (...)` as a value expression** (`oxisqlite-core`): `IN` lists used outside `WHERE`/`HAVING`/`JOIN ... ON` (e.g. `SELECT x IN (1,2,3) FROM t`) previously crashed; now evaluates correctly. From the same pass: a schema-qualified `schema.table.column` reference, a parenthesized multi-expression used as a value (e.g. `(a, b)` outside an `IN`/row-value context), and the `MATCH` operator now fail with a clean parse error instead of panicking (still not evaluated, but no longer crash the process); `EXPLAIN`/`EXPLAIN QUERY PLAN` run through `Connection::prepare()` likewise no longer panic, `EXPLAIN QUERY PLAN` now also supports `UPDATE`/`DELETE` (previously `SELECT`-only), and `CREATE VIRTUAL TABLE` no longer panics with a `RefCell` "already borrowed" error.
- **CTEs referenced more than once** (`oxisqlite-core`): a `WITH` CTE referenced multiple times in the same query previously resolved correctly only on its first reference; each reference now resolves independently.
- **`SELECT COUNT(*)` on an MVCC-backed cursor** (`oxisqlite-core`): previously panicked (`todo!("Implement count for mvcc")`); now returns the row count.
- **`DO UPDATE SET` targeting a generated column** (`oxisqlite-core` upsert): an upsert's `DO UPDATE SET` clause could previously target a `GENERATED ALWAYS AS (...)` column (silently accepted, with no `is_generated` check in place); now rejected with the same "cannot UPDATE generated column" error real SQLite raises for a plain `UPDATE`.
- **Wide-row / large-header record encoding** (`oxisqlite-core`): records with a header longer than 126 bytes (roughly 127+ columns, or fewer columns with one large `TEXT`/`BLOB`) previously crashed during serialization (`todo!("calculate big header size extra bytes")`); a new fixed-point header-size resolution (mirroring SQLite's own fixup) now handles them correctly.
- **`BLOB`/`TEXT` coercion in `QUOTE()`, `||`, and `concat()`** (`oxisqlite-core`): all three previously crashed on `BLOB` operands; `QUOTE()` now renders uppercase `X'...'` hex, `||` keeps `Blob || Blob` as raw-byte `Blob` while coercing other pairings to `Text`, and `concat()` always coerces `Blob` to `Text` (matching SQLite 3.44+ semantics).
- **`strftime()` `%J` (Julian day) and pad-override flags** (`oxisqlite-core`): `%J` used to be substituted via a blind pre-processing string replace that could misfire inside an escaped `%%J` (producing the Julian day instead of the literal text `%J`); it is now handled inline during normal specifier parsing, correctly participating in `%`-escaping. Separately, a pad-override flag (e.g. `%0d`, `%_e`) on *any* specifier used to always resolve to `Item::Error` (the specifier character following the flag was never re-read); both are now fixed together.
- **`PRAGMA auto_vacuum = FULL`** (`oxisqlite-core`): the largest-root-page high-water mark was never advanced past the first table/index created in a database, so later `CREATE TABLE`/`CREATE INDEX` statements computed a stale root-page slot; it is now updated correctly on every B-tree creation.
- **PostgreSQL connection hangs** (`oxisql`): connecting to an unreachable PostgreSQL host via `oxisql::connect()` (also `connect_with_options`/`connect_with_tls`) previously hung indefinitely; it now applies a connection timeout (10s by default, configurable via `ConnectOptions::connect_timeout_ms`) and returns a typed timeout error.
- **Memory-safety hardening via Miri** (`oxisqlite-core`): fixed a pointer-provenance bug when materializing `BLOB`/`TEXT` values from on-disk pages, an unaligned-reference bug in the `VECTOR` column type's slice conversions (`Vector::as_f32_slice`/`as_f64_slice`, which reinterpret-cast a `Vec<u8>` with no alignment guarantee, are removed in favor of `to_f32_vec`/`to_f64_vec`, which decode via `from_le_bytes`), and a page-cache memory leak (evicted entries were destructed but never deallocated). Reworked 40+ page-access call sites across the storage/B-tree layers that could previously manufacture simultaneous mutable aliases into the same page buffer from a shared reference. Also fixes smaller pointer-provenance/aliasing bugs in result-row construction, virtual-table module registration, and virtual-table row-ID updates.

## [0.3.2] - 2026-07-11

### Added

- **`zstd-shim` crate** (`crates/zstd-shim/`): a local, non-published Pure-Rust `zstd`-API
  shim backed by `oxiarc-zstd`, wired in via `[patch.crates-io]`
  (`zstd = { path = "crates/zstd-shim" }`). Implements only the surface `arrow-ipc` (via
  DataFusion) actually uses — `bulk::{Compressor, Decompressor}` and
  `DEFAULT_COMPRESSION_LEVEL` — so the `--all-features` dependency closure no longer pulls
  the C-FFI `zstd-sys` crate (COOLJAPAN Pure Rust Policy v2 / OxiARC-only compression
  policy).

### Changed

- **`oxiarc-zstd` bumped to `0.3.5`** (`crates/zstd-shim`): two successive dependency
  version bumps for the new zstd shim.
- **`time` updated to `0.3.53`, `uuid` updated to `1.23.4`** (workspace): routine
  dependency bumps; `uuid` also bumped to match in `oxisqlite-uuid`.
- **`env_logger` updated to `0.11.11`, `io-uring` updated to `0.7.13`**
  (`oxisqlite-core`): dev-dependency and Linux target-specific dependency bumps.
- **`op_agg_step` accumulator initialization hardened** (`oxisqlite-core` VDBE): now
  re-initializes whenever a register does not already hold an `AggContext` (previously
  only on `Value::Null`), and `Max`/`Min` no longer type-match the first column value up
  front — both changes make accumulator init self-healing against stale state left over
  from a previous `GROUP BY` group. Internal invariant violations now return
  `LimboError::InternalError` instead of `panic!()`.

### Fixed

- **`GROUP BY` aggregate accumulator reset range** (`oxisqlite-core` query
  planner/VDBE): fixed a bug where `COUNT(*)` / `COUNT()` reached via `HAVING`,
  `ORDER BY`, or a nested expression (as opposed to a result column) was planned with
  zero arguments, undercounting its sorter-column span and leaving its accumulator
  register unreset across group boundaries — causing out-of-bounds sorter reads and
  stale-accumulator panics on multi-group `GROUP BY ... HAVING COUNT(*) ...` queries.
  `COUNT(*)` now carries the same synthetic literal-`1` argument the result-column path
  already used, and the accumulator-clear range in `group_by_emit_row_phase` now covers
  the full aggregate block rather than just the (potentially under-counted)
  sorter-column span. New regression coverage in `tests/group_by_having.rs`.

## [0.3.1] - 2026-06-23

### Added

- **`FlamegraphProfiler` benchmark utility** (`oxisqlite-core`): new `benches/common/profiler.rs`
  implements a custom Criterion 0.8-compatible `Profiler` backed by `pprof`. It emits a
  `flamegraph.svg` into each benchmark's output directory when run with `--profile-time`. This
  avoids the criterion version conflict introduced by `pprof`'s bundled `PProfProfiler` (still
  pinned to criterion 0.5).

### Changed

- **`arrow` pinned to `58.3.0`** (workspace): downgraded from `59.0.0` to match the version
  DataFusion 54 re-exports; `arrow 59` has no compatible DataFusion release and
  `oxistore-columnar` also pins `58.3.0`.
- **`oxistore-columnar` updated to `0.2.0`** (workspace): pulls in the latest columnar
  Parquet-backend release.
- **`as_any()` overrides removed** (`oxisql-datafusion`): `TableProvider` and `ExecutionPlan`
  impls in `parquet.rs`, `provider.rs`, and `stream.rs` no longer override `as_any()` — the
  method was removed from both traits in DataFusion 54, so the overrides were dead code.

### Fixed

- **`rand` 0.9 API compatibility** (`oxisqlite-core` btree tests): replaced deprecated
  `Rng::gen()` with `RngExt::random()` in the B-Tree stress test.

## [0.3.0] - 2026-06-22

### Removed

- **`objc2-system-configuration` removed from default dependency closure** (`oxisqlite-core`): The
  macOS `SCDynamicStore` C binding previously pulled in transitively by `whoami`'s `std` feature
  has been excised. The default `cargo build --workspace` is now 100 % `objc2`-free on macOS.

### Added

- **`whoami-patched` vendored crate** (`crates/whoami-patched/`): Pure-Rust patch of `whoami`
  2.1.2 that drops `objc2-system-configuration` from the macOS code path; wired in via
  `[patch.crates-io]` in the workspace `Cargo.toml`. Not published to crates.io (vendored only).

### Changed

- **`oxisqlite-core` default I/O backend is now pure-Rust generic**: The native epoll/kqueue
  event-loop is now gated behind the `native-io` feature (opt-in). The `load-extension` feature
  (which pulls `libloading`) is likewise opt-in. Default builds remain 100 % C-free.
- **`oxitls` dependency bumped to `^0.2.0`**: Resolves the `PENDING-REPUBLISH` dependency block
  now that `oxitls 0.2.0` has been published to crates.io.

### Security

- Clears `PENDING-REPUBLISH` status: `objc2-system-configuration` (a macOS C/ObjC binding) no
  longer appears in the `--all-features` dependency closure. COOLJAPAN Pure Rust Policy v2 §3
  Role-A compliance restored for the macOS target.

## [0.2.1] - 2026-06-20

### Added

#### WITHOUT ROWID table support (`oxisqlite-core`)
- `CREATE TABLE … WITHOUT ROWID` now fully supported: uses an index-format B-Tree where the PRIMARY KEY columns are the B-Tree key and the full row is stored as the record payload.
- `Index::synthetic_for_without_rowid(table)` in `schema/index.rs` builds a synthetic index object (PK columns + all table columns) used to open cursors as `CursorType::BTreeIndex` with `has_rowid = false`.
- `translate_create_table` in `translate/schema.rs` detects `WITHOUT ROWID` and emits `CreateBtree` with `CreateBTreeFlags::new_index()` instead of `new_table()` — the pager initialises the root page as an index-leaf page.
- `validate_without_rowid_table` enforces: (1) an explicit PRIMARY KEY is present; (2) the PK column(s) occupy the first declared positions — required for correct B-Tree key comparison.
- `translate_insert_without_rowid` in `translate/insert.rs`: dedicated INSERT code path for WITHOUT ROWID tables; opens the cursor as `BTreeIndex`, populates all column registers, enforces NOT NULL on PK columns, emits `NoConflict` for the PK uniqueness check, supports `OR IGNORE` (skip) and `OR REPLACE` (delete + re-insert), then writes via `MakeRecord` + `IdxInsert`; multi-row (`VALUES(…),(…)`) and `INSERT … SELECT` use the standard coroutine path.
- `translate/plan.rs` updated: for WITHOUT ROWID tables without an explicit index hint, `CursorType::BTreeIndex(synthetic)` is allocated automatically so that `SELECT` / full-scans use the correct B-Tree page format.
- `crates/oxisqlite-core/tests/without_rowid.rs` — 397-line integration test suite (registered in `Cargo.toml` as the `without_rowid` test target) covering: CREATE success/failure, basic INSERT + SELECT round-trip, PK NOT NULL enforcement, PK uniqueness (ABORT / IGNORE / REPLACE), multi-row INSERT, text PK, composite PK, validation of missing PK, and validation of PK-column-not-first.

#### `BorrowedValue<'a>` — zero-allocation borrowed view of SQL values (`oxisql-core`)
- New `BorrowedValue<'a>` enum in `oxisql-core` provides a lifetime-parametric mirror of `Value` where `Text`, `Blob`, `Json`, and `Decimal` borrow from existing storage instead of owning heap allocations; all scalar variants (`Null`, `Bool`, `I64`, `F64`, `Timestamp`, `Date`, `Time`, `Uuid`) are copied inline.
- `BorrowedValue::to_owned(&self) -> Value` converts back to an owned `Value` by cloning borrowed bytes.
- `From<&'a Value> for BorrowedValue<'a>` allows zero-cost borrowing of any `Value`; `Array` / `TypedArray` fall back to `Null` (documented limitation, callers iterate `elems` manually).
- `BorrowedValue` implements `Debug`, `Clone`, `PartialEq`, `Display` (UUID formatted as `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`), and `type_name() -> &'static str`.
- Re-exported from `oxisql-core` root (`pub use value::{ArrayElementType, BorrowedValue, Value}`).
- 15 unit tests in `borrowed_value_tests` module covering: type names, is_null, text/blob zero-allocation round-trips, scalar round-trips, `From<&Value>` for all variants, Display output, and full owned round-trip.

### Fixed
- `INSERT` into WITHOUT ROWID tables previously returned `"INSERT into WITHOUT ROWID table is not supported"` at parse time; now correctly routed to the index-format insert path.
- `CHECK_automatic_pk_index_required` no longer returns an unsupported error for WITHOUT ROWID tables; instead returns `Ok(None)` (no separate auto-index needed — the table IS the index).

[0.2.1]: https://github.com/cool-japan/oxisql/releases/tag/v0.2.1

## [0.2.0] - 2026-06-17

### Added

#### ANALYZE statement (`oxisqlite-core`)
- Full `ANALYZE [target]` statement support that writes cardinality statistics to `sqlite_stat1` (`CREATE TABLE sqlite_stat1(tbl,idx,stat)`).
- `translate_analyze` in `translate/analyze.rs` generates bytecode that: creates `sqlite_stat1` if absent, clears prior rows for the target, walks each table/index b-tree via the new `Insn::IdxStat` opcode, inserts fresh `(tbl, idx, stat)` rows, bumps the schema cookie, and re-parses the schema.
- `ANALYZE` — bare, `ANALYZE main`, `ANALYZE <table>`, `ANALYZE <index>` — all forms supported with correct `ClearMode` semantics.
- `Insn::IdxStat { cursor_id, num_cols, dest }` opcode in `vdbe/insn.rs`; `op_idx_stat` handler in `vdbe/execute/txn_schema.rs` walks the b-tree and writes `"N a1 … ak"` statistics strings (NULL for empty tables/indexes so inserts are skipped).
- 6 integration tests in `crates/oxisqlite-core/tests/analyze.rs` covering: row-count write, empty-table skip, re-analyze replaces stale rows, named-table targeting, error on unknown table, and query-correctness check via the in-memory stats side-map.

#### System-R optimizer with real ANALYZE statistics (`oxisqlite-core`)
- `SchemaStats` side-map (`statistics.rs`) — in-memory mirror of `sqlite_stat1`, loaded after schema parsing; exposes `num_rows(table)` and `index_stats(table, index)`.
- `parse_stat1_line` utility parses SQLite's `"N a1 … ak"` format (tolerates trailing non-integer tokens such as `unordered`); 8 unit tests inline.
- `Schema` gains a `stats: SchemaStats` field; `load_persistent_stats()` on `Connection` populates it by scanning `sqlite_stat1` after the schema is loaded — completely backwards compatible (empty map preserves the old hardcoded-estimate code path).
- `estimate_cost_for_scan_or_seek` in `translate/optimizer/cost.rs` updated to accept `base_row_count: f64` and `index_stats: Option<&[i64]>`; when stats are present the equality-prefix selectivity is derived from `avg_rows_per_distinct / base_row_count` instead of the per-column selectivity product, giving the System-R planner real cardinality estimates.
- `optimize_table_access` passes `schema.stats` down through `constraints_from_where_clause` to the cost estimator; databases without an ANALYZE run are unaffected (stats are `None` → hardcoded estimates preserved bit-for-bit).
- `db_tests` module in `statistics.rs` provides an end-to-end proof that `ANALYZE` populates `conn.schema.stats` with the correct row count.

#### `application_id` and `synchronous` pragmas (`oxisqlite-core`)
- `PRAGMA application_id [= N]` — read and set the 32-bit application-ID header field (cookie offset 68); new `Cookie::ApplicationId` variant in `vdbe/insn.rs`.
- `PRAGMA synchronous [= N]` — read/set WAL synchronous-mode flag; now registered in the pragma table.

#### Schema module split via splitrs (`oxisqlite-core`)
- `schema.rs` (1 920 lines) replaced by a 7-module sub-tree: `schema/mod.rs`, `schema/bootstrap.rs`, `schema/column.rs`, `schema/container.rs`, `schema/index.rs`, `schema/table.rs`, `schema/tests.rs`. All public types re-exported from `schema/mod.rs`; no API breakage.

#### VDBE execute module split via splitrs (`oxisqlite-core`)
- `vdbe/execute.rs` (8 361 lines) replaced by a 10-module sub-tree: `execute/mod.rs`, `execute/aggregate.rs`, `execute/arith_logic.rs`, `execute/cursor.rs`, `execute/function.rs`, `execute/mutate.rs`, `execute/numeric.rs`, `execute/txn_schema.rs`, `execute/values.rs`, `execute/tests.rs`.
- `values.rs` consolidates all `Value::exec_*` methods (`exec_lower`, `exec_upper`, `exec_length`, `exec_octet_length`, `exec_sign`, `exec_soundex`, regex operations, math functions, etc.) as inherent `Value` methods — previously scattered inline across `execute.rs`.
- `txn_schema.rs` consolidates transaction, savepoint, cookie, checkpoint, `ParseSchema`, `IntegrityCheck`, and the new `op_idx_stat` opcode handlers.

#### UPSERT `ON CONFLICT DO UPDATE` (`oxisqlite-core`)
- `translate/upsert.rs` — `emit_upsert_do_update` helper extracted from `translate/insert.rs` to keep both files under the 2000-line workspace policy.
- Integration tests in `crates/oxisqlite-core/tests/upsert.rs` and `crates/oxisql-sqlite-compat/tests/` for UPSERT and schema versioning scenarios.

#### Conflict-clause handling (`oxisqlite-core`)
- 5 integration tests in `crates/oxisqlite-core/tests/conflict.rs`: `INSERT OR FAIL`, `INSERT OR ABORT`, `INSERT OR ROLLBACK`, `INSERT OR IGNORE`, and the default-ABORT behaviour.

#### Correlated sub-query tests (`oxisqlite-core`)
- 19 integration tests in `crates/oxisqlite-core/tests/correlated.rs` covering scalar, `EXISTS`, `NOT EXISTS`, `IN`, `NOT IN`, nested, and multi-subquery patterns including an arithmetic-context regression.

#### Durability & WAL tests (`oxisqlite-core`)
- `crates/oxisqlite-core/tests/durability.rs` — file-backed durability tests exercising WAL commit/crash-recovery.

#### Schema-cookie and LIMIT/params tests (`oxisqlite-core`)
- `crates/oxisqlite-core/tests/schema_cookie.rs` — schema-cookie bump and reprepare lifecycle tests.
- `crates/oxisqlite-core/tests/limit_params.rs` — LIMIT/OFFSET with bound parameters.

#### `CREATE INDEX IF NOT EXISTS` (`oxisqlite-core`)
- `translate_create_index` now respects the `IF NOT EXISTS` flag: silently succeeds when the index already exists rather than raising a parse error.

#### Schema-change cookie emission (`oxisqlite-core`)
- `program.emit_schema_change()` added after DDL operations in `translate/alter.rs` (ADD COLUMN, RENAME COLUMN, RENAME TABLE, DROP COLUMN), `translate/index.rs` (CREATE INDEX, DROP INDEX), and `translate/analyze.rs` — ensures the schema cookie is bumped and cached compiled statements are invalidated consistently.

#### Transparent schema re-prepare in statement cache (`oxisql-sqlite-compat`)
- `exec_rewritten` in `oxisql-sqlite-compat/src/connection.rs` now catches `SchemaChanged` errors from the engine, discards the stale compiled program, re-prepares against the refreshed schema, and retries exactly once — replacing the fragile `is_ddl` keyword-prefix heuristic that failed on comment-prefixed DDL and left DML statements stale after schema changes.
- `schema_reprepare.rs` test suite (`crates/oxisql-sqlite-compat/tests/schema_reprepare.rs`) with tests for: comment-prefixed DDL replay, DML reuse after schema change, `CREATE INDEX` invalidation, and `ALTER TABLE` invalidation.

#### `connect_or_create` — auto-create missing databases (`oxisql`)
- New `connect_or_create(uri)` façade function connects to the target URI; when the database does not yet exist on a wire-protocol backend (PostgreSQL/MySQL) it issues `CREATE DATABASE <name>`, then connects to the freshly created database.
- `split_db_name` helper parses any `scheme://authority/db?query` URI into `(authority, db_name)`.
- `CreateScheme` enum classifies `postgres://` / `postgresql://` vs `mysql://` schemes.
- Integration tests in `crates/oxisql/tests/auto_create.rs` (ignored by default; require a running server).

#### Blocking connection API (`oxisql-sqlite-compat`)
- `BlockingSqliteConnection` — synchronous (non-async) wrapper around `SqliteConnection` via a single-threaded `tokio` runtime; exposes `execute`, `query`, `begin`, `commit`, `rollback`.
- `blocking.rs` test suite covering basic CRUD, transaction commit/rollback, and multi-row queries.

#### Orphaned WAL protection (`oxisqlite-core`)
- `maybe_init_database_file` now returns `bool` indicating whether the database file was freshly created; `Database::open_file` passes this flag to `WalFileShared::open_shared_inner`.
- `WalFileShared::open_shared_inner` discards (truncates) any pre-existing WAL when the main database file was freshly created, preventing stale WAL frames from a previous database incarnation from being replayed.

#### WAL header refresh on open (`oxisqlite-core`)
- `conn.pager.refresh_header_from_wal()` called during `Database::open_*` after WAL recovery to ensure the in-memory header reflects the latest committed cookie values (e.g. `application_id`, `user_version`) that may have been committed to the WAL without a checkpoint.

#### `checkpoint_truncate` API (`oxisqlite-core`)
- `Connection::checkpoint_truncate()` exposes a TRUNCATE-mode WAL checkpoint, resetting the WAL file to empty.
- `Connection::close()` is now idempotent (guarded by a `closed: Cell<bool>` flag).

### Changed
- **Version bump 0.1.2 → 0.2.0** across the entire workspace (`[workspace.package].version` in root `Cargo.toml`); all intra-workspace dependency version strings updated accordingly.
- `Schema` struct gains a `stats: SchemaStats` field (default-constructed; zero-cost when ANALYZE has never run).
- `optimize_table_access` signature changed from `available_indexes: &HashMap<…>` to `schema: &Schema` to pass statistics through to the cost estimator.
- `estimate_cost_for_scan_or_seek` signature extended with `base_row_count: f64` and `index_stats: Option<&[i64]>` parameters; all callers updated.
- `Transaction { write }` instruction now carries `schema_cookie` for correct cookie-mismatch detection in `BEGIN IMMEDIATE` / `EXCLUSIVE`.

### Fixed
- `CREATE INDEX … IF NOT EXISTS` no longer raises a parse error when the index already exists.
- `ALTER TABLE` (ADD COLUMN, RENAME COLUMN, RENAME TABLE, DROP COLUMN) and `CREATE/DROP INDEX` now correctly bump the schema cookie, preventing stale cached statements from being reused across DDL boundaries.
- Comment-prefixed DDL statements (e.g. `/* migration 0001 */ CREATE TABLE …`) no longer silently corrupt the statement cache — the new SchemaChanged-based re-prepare path handles them correctly.
- Opening a new database file alongside an orphaned `-wal` file no longer replays stale WAL frames from a previous database; the orphaned WAL is discarded and a fresh WAL is started.
- WAL-committed `PRAGMA application_id` / `PRAGMA user_version` changes are now visible immediately after open (previously required a checkpoint to become visible via `PRAGMA` reads).

---

## [0.1.2] - 2026-06-10

### Added

#### C-free `oxisqlite-*` engine fork (Wave 1)
- Replaced the C-pulling `limbo` dependency with a 7-crate pure-Rust fork of limbo 0.0.22
  (`oxisqlite`, `oxisqlite-core`, `oxisqlite-ext`, `oxisqlite-macros`,
  `oxisqlite-sqlite3-parser`, `oxisqlite-time`, `oxisqlite-uuid`).
- Removed all 3 C touchpoints: `mimalloc` allocator, `lemon.c` parser generator,
  and `built`/`git2` build-info crates.
- `CC=/usr/bin/false cargo build --workspace` → exit 0 (C-free proven).
- Inline pure-Rust Julian-day helper in `oxisqlite-core` (replaces GPL-licensed
  `julian_day_converter`).

#### Full-transaction ROLLBACK support (Wave 2, `oxisql-sqlite-compat`)
- `BEGIN / INSERT / ROLLBACK` now correctly discards changes; `COMMIT` persists them.
- WAL integrity preserved. Ported rollback machinery from `turso_core` 0.7.0-pre.5 (MIT).
- New `oxisql-sqlite-compat/tests/rollback.rs` (5 tests), `savepoint.rs`,
  `change_counts.rs`, `type_mapping.rs`, `rollback_error.rs` (updated).

#### TLS security patch (Wave 3)
- Vendored `rustls-rustcrypto-patched` crate fixes RUSTSEC-2026-0104 (CRL-parsing panic
  in `rustls-webpki 0.102.x`) via `[patch.crates-io]`.
- Root `NOTICE` file created recording full fork lineage.
- `deny.toml` allowlist extended: Zlib, Unicode-3.0, MPL-2.0, CDLA-Permissive-2.0.

#### Query cancellation (`oxisql-postgres`)
- `PostgresCancelToken` — cancel a running query without closing the connection.
- `PgConnection::cancel_token()` returns a token usable from any async context.
- New `PgError::ConnectionError` variant for connection-level failures.
- `TypedArray` replaces raw array handling in `Value` for richer type representation.

#### Advisory migration locking (`oxisql-migrate`)
- `MigrationLock` trait with `NoopMigrationLock` and `PostgresAdvisoryLock`
  implementations — prevents concurrent schema migrations.
- Migration `rechecksum` support and `--recheck-hash` CLI flag.
- Migration directives: `-- oxi:no-tx`, `-- oxi:skip-if-exists`, `-- oxi:require-version`.
- `lock.rs` module and `tracker_generic.rs` for backend-agnostic migration tracking.

#### SQL optimizer enhancements (`oxisql-parse`)
- `decorrelate.rs` — correlated-subquery decorrelation pass.
- `explain.rs` — `EXPLAIN`-compatible query plan renderer.
- `optimizer/cse.rs` — Common Sub-expression Elimination (CSE) pass.
- `optimizer/join_reorder.rs` — cost-based join reordering (842 lines).
- `optimizer/simplify.rs` — constant folding and predicate simplification (833 lines).
- `parameterize.rs` — SQL literal parameterization for LRU plan cache.
- `plan_cache.rs` — `PlanCache` struct with schema-invalidation for repeated queries.
- `planner.rs` — extended logical planner with new plan node types.

#### DataFusion bridge improvements (`oxisql-datafusion`)
- `plan_bridge.rs` — structural lowering of `Filter` and `Project` nodes.
- `stream.rs` — async streaming rowset adapter refactored and extended.
- New tests: `plan_bridge_structural.rs` (503 lines), `pushdown_extra.rs` (335 lines),
  `query_provider.rs` (47 lines).
- TPC-H benchmark queries (Q3, Q5–Q9, Q19) added under `crates/perf/tpc-h/`.

#### Connection options (`oxisql-core` / `oxisql-postgres`)
- `ConnectOptions` now parses query-string parameters from connection URIs
  (`application_name`, `sslmode`, extra KV pairs).
- `BackendInfo` documents that server versions for PostgreSQL/MySQL are not known
  until after the connection handshake; SQLite-compat backend reports a static version.
- `Middleware` trait and `LoggingConnection`/`MetricsConnection`/`RetryConnection`
  wrappers added to `oxisql-core`.
- `Warning` type for server-side diagnostic messages (MySQL warning forwarding).

#### SQLite-compat type mapping (`oxisql-sqlite-compat`)
- Columns typed `DATE`, `TIMESTAMP`, `TIME`, `UUID` are now mapped to rich `Value`
  variants; plain TEXT/INTEGER columns are not false-retyped.
- `change_count()` on connection reflects `changes()` from the underlying engine.

### Changed
- `oxisql-sqlite-compat` dependency changed from `limbo` to in-tree `oxisqlite`
  workspace path (`crates/oxisqlite`).
- `oxisql-pool/sqlite_rusqlite.rs` removed; `sqlite_compat.rs` extended in its place.
- `cfg_block` dependency dropped (unused).

### Fixed
- Removed `unsafe transmute` in `const_concat_slices` macro — replaced with safe
  const-generic array construction.
- `oxisql-embedded` `memory_complex.rs` integration test — added missing `NULL`
  assertion edge cases.
- LEMON parser template file (`lemon.c`-generated artefact) removed from tree.

## [0.1.1] - 2026-06-04

### Added

#### CSV Import / Export (`oxisql-embedded`)
- `EmbeddedConnection::import_csv(table_name, csv_data)` — import RFC 4180 CSV directly into a new table; first row is treated as the header, column names are sanitised (spaces/hyphens → underscores, leading digits get `col_` prefix), empty fields become `NULL`, all values stored as `TEXT`
- `EmbeddedConnection::export_table_to_csv(table_name)` — export any table to RFC 4180-compliant CSV (CRLF line endings); values containing commas, double-quotes, or newlines are properly quoted; `NULL` exports as an empty field
- `oxisql_embedded::csv` module (public) — standalone CSV utilities: `parse_csv`, `build_csv_output`, `value_to_csv_field`, `sanitise_column_name`, `build_create_table_sql`, `build_insert_sql`, `quote_csv_field`; zero external `csv`-crate dependency, hand-rolled state-machine parser handles quoted fields, `""` escapes, bare LF, CRLF, and embedded newlines

#### Interactive SQL REPL (`oxisql` facade — `repl` feature)
- `oxisql-repl` binary — interactive Read-Eval-Print Loop over any OxiSQL backend; supports `memory://`, `postgres://`, `mysql://`, and `sqlite://` URIs; multi-line statement accumulation (flush on `;` or blank line); tabular result rendering with auto-sized columns and truncation
- Dot commands: `.help`, `.tables`, `.schema <table>`, `.quit` / `.exit` / `.q`
- New `repl` feature flag in `oxisql/Cargo.toml` (activates `embedded` + `tokio` + `anyhow`); binary is conditionally compiled (`required-features = ["repl"]`)

### Fixed
- `unique_test_dir` helper in `oxisql-embedded/tests/memory_persistent.rs` is now guarded with `#[cfg(any(feature = "fjall-storage", feature = "redb-storage"))]`, eliminating the `dead_code` warning when building with default features

## [0.1.0] - 2026-06-01

### Added

#### Core (`oxisql-core`)
- `Connection` trait — unified async database connection abstraction
- `Transaction` trait — ACID transaction management with commit/rollback
- `PreparedStatement` trait — parameterized query execution
- `ConnectionPool` trait — generic connection pool abstraction
- `Migrator` trait — schema migration lifecycle management
- `Value` enum with 13 variants: `Integer`, `Float`, `Bool`, `Text`, `Blob`, `Null`, `Decimal`, `Timestamp`, `Date`, `Time`, `Uuid`, `Json`, `Array`
- `Row` and `RowSet` types for query result representation
- `FromValue` trait for ergonomic value extraction
- `SchemaInfo`, `ColumnInfo`, `IndexInfo`, `ForeignKeyInfo` for schema introspection
- `Middleware` trait and query middleware pipeline for cross-cutting query concerns

#### Embedded Backend (`oxisql-embedded`)
- GlueSQL in-memory engine with full `Connection` + `Transaction` support
- `export_as_sql()` / `import_from_sql()` for portable data serialization
- Zero external native dependencies — 100% Pure Rust

#### PostgreSQL Backend (`oxisql-postgres`)
- Pure-Rust `tokio-postgres` driver with `rustls` TLS (no `libpq` dependency)
- Extended type mapping: `DATE` → `Value::Date`, `TIMESTAMP` / `TIMESTAMPTZ` → `Value::Timestamp`, `UUID` → `Value::Uuid`, `JSONB` / `JSON` → `Value::Json`, `NUMERIC` → `Value::Decimal`, `ARRAY` → `Value::Array`
- Async connection and transaction support via Tokio

#### MySQL Backend (`oxisql-mysql`)
- Pure-Rust `mysql_async` driver with `rustls` TLS (no `libmysqlclient` dependency)
- Extended type mapping: `DATE`, `DATETIME`, `TIMESTAMP`, `DECIMAL`, `JSON` → proper `Value` variants
- Async connection and transaction support via Tokio

#### SQLite-Compatible Backend (`oxisql-sqlite-compat`)
- Pure-Rust SQLite-compatible engine backed by Limbo (no `libsqlite3` dependency)
- `foreign_keys()` — introspect foreign key constraints via DDL parsing
- `indexes()` — introspect indexes via DDL parsing

#### Connection Pooling (`oxisql-pool`)
- `OxidbPgPool` — PostgreSQL connection pool
- `MysqlPool` — MySQL connection pool
- `EmbeddedPool` — GlueSQL in-memory connection pool
- `SqliteCompatPool` — SQLite-compatible connection pool
- All pools implement the `ConnectionPool` trait
- `connect_pooled(uri, size)` — URI-scheme-based pool dispatch (auto-selects backend)

#### SQL Parsing & Planning (`oxisql-parse`)
- `QueryBuilder` — programmatic query construction
- Query planner with predicate pushdown optimization
- Join algorithm selection
- Aggregate processing pipeline
- Statement validation and normalization
- LRU parse cache for repeated query patterns

#### Migrations (`oxisql-migrate`)
- `MigrationRunner` — file-based migration execution
- 14-digit timestamp migration filenames for deterministic ordering
- `run_with_pool()` / `run_pooled()` for pooled execution
- `status()` — report applied vs. pending migrations
- `pending()` — list unapplied migration files

#### DataFusion Integration (`oxisql-datafusion`)
- `OxiSqlTableProvider` — expose any OxiSQL backend as a DataFusion `TableProvider`
- `OxiSqlContext` — unified OLAP query context over all backends
- Enables analytical SQL (window functions, complex aggregations) across all supported engines

#### Unified Facade (`oxisql`)
- `connect(uri)` — single entry point; dispatches to the correct backend by URI scheme
- `connect_pooled(uri, size)` — pooled variant with configurable pool size
- `connect_pool(uri)` — returns a type-erased `ConnectionPool`
- Feature flags: `postgres`, `mysql`, `embedded`, `sqlite`, `sqlite-compat`, `pool-postgres`, `pool-mysql`, `pool-embedded`, `pool-sqlite`, `datafusion`
- All backends are 100% Pure Rust with no C/C++/Fortran native dependencies

### Added (second ultra pass — 2026-05-30)

#### Named Parameters (`oxisql-core`)
- `Connection::execute_named` and `Connection::query_named` — default trait methods
  providing named-placeholder support (`:name`, `$name`, `@name`) across all backends
  with zero per-backend code. Implemented in `oxisql-core::params`.
- `OxiSqlError::Params` — new error variant returned on named-parameter binding failures.
- Available via `use oxisql::prelude::*` or `use oxisql_core::Connection`.

#### EmbeddedConnection Schema Introspection (`oxisql-embedded`)
- `EmbeddedConnection` now fully implements `tables()`, `columns()`, `indexes()`, and
  `foreign_keys()` via the GlueSQL catalog. Previously these returned `Err("not supported")`.

#### SQLite-compat improvements (`oxisql-sqlite-compat`)
- `SqliteTransaction::rollback()` now returns a clear
  `OxiSqlError::Other("ROLLBACK is not supported by the limbo 0.0.22 engine…")` instead
  of a cryptic parse error.
- Statement cache infrastructure: 128-slot LRU cache keyed by rewritten SQL text is in
  place; activates once limbo fixes the `Statement::reset()` / `Program::n_change` bug.

[0.3.3]: https://github.com/cool-japan/oxisql/compare/v0.3.2...v0.3.3
[0.3.2]: https://github.com/cool-japan/oxisql/compare/v0.3.1...v0.3.2
[0.3.1]: https://github.com/cool-japan/oxisql/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/cool-japan/oxisql/releases/tag/v0.3.0
[0.2.1]: https://github.com/cool-japan/oxisql/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/cool-japan/oxisql/compare/v0.1.2...v0.2.0
[0.1.2]: https://github.com/cool-japan/oxisql/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/cool-japan/oxisql/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/cool-japan/oxisql/releases/tag/v0.1.0
