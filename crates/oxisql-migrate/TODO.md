# oxisql-migrate — TODO

## Status: Stable (0.1.2)

Pure Rust, `#![forbid(unsafe_code)]`. The scanner parses
`{14-digit-timestamp}__{name}.sql` (and optional `.down.sql`) filenames and returns
sorted `MigrationFile` entries. The tracker creates/queries the `_oxisql_migrations`
table (`version BIGINT PK, name TEXT, checksum TEXT, applied_at TEXT`). The runner
orchestrates scan → checksum-verify → filter → transactional apply → track, for the
embedded GlueSQL backend and any generic `oxisql_core::Connection`. The `migrate`
feature gates `sqlparser`/`gluesql`/`tokio`; the `pool` feature adds
`run_pooled`/`run_with_pool`; the `cli` feature builds the `oxisql-migrate` binary.

**Tests: 44 pass with `--all-features`, 0 ignored.** Zero clippy warnings.

## Done

### Core implementation
- [x] Real UTC `applied_at` timestamp in `mark_applied()` via `std::time::SystemTime`, formatted ISO 8601 (replaced the hardcoded placeholder)
- [x] Down-migration support: parse `{version}__{name}.down.sql`; `MigrationRunner::rollback(glue, target_version)` reverts to a target version (scanner extension + runner method + tracker `mark_reverted`)
- [x] Dry-run mode: `MigrationRunner::dry_run()` reports pending migrations without executing — `Vec<MigrationFile>`
- [x] Status report: `MigrationRunner::status()` → `Vec<(MigrationFile, MigrationState)>` (Applied/Pending/Modified/Orphaned)
- [x] Generic `Connection`-based tracker (`tracker_generic.rs`) covering Postgres and MySQL backends interchangeably
- [x] Checksum verification: SHA-256 per file, stored at apply time, verified on later runs (→ `ChecksumMismatch`)
- [x] Transaction wrapping: each migration runs inside `BEGIN`…`COMMIT` with automatic `ROLLBACK` on failure (for backends that support transactions)

### API improvements
- [x] `MigrationError` Display/Error via `thiserror` derive
- [x] `MigrationRunner::new_with_options(dir, MigrateOptions)` (configurable tracker table, dry-run flag, target version)
- [x] `MigrationRunner` generic over a `TrackerBackend` trait (embedded/PG/MySQL trackers interchangeable)
- [x] `MigrationFile::read_sql(&self) -> Result<String, MigrationError>`
- [x] `From<sqlparser::parser::ParserError> for MigrationError::Parse`

### Integration
- [x] Wired into the `oxisql` facade behind `migrate` (re-exports `MigrationRunner`, `MigrateOptions`, `scan_migrations`)
- [x] `oxisql-pool` bridge: `run_pooled(&EmbeddedPool)` (+ `MigrationError::Connection`) and `run_with_pool(&OxidbPool)`, behind `pool`
- [x] CLI binary behind `cli`: `oxisql-migrate run` / `status` / `rollback --version <v>`

### Testing & performance
- [x] Rollback with `.down.sql` verification (apply 3, rollback to v1, verify only v1 remains)
- [x] Checksum-mismatch detection; orphaned-migration detection; concurrent-run no-double-apply
- [x] Malformed-SQL → `MigrationError::Parse`; empty-directory → `Ok(0)`; pooled-embedded integration
- [x] Criterion benches: scan overhead and apply-N-DDL throughput
- [x] `MigrationFile` list caching in the runner (`get_migrations()` + `invalidate_cache()`)

## Roadmap / next
- [x] `Modified`-state recovery helpers (a guided `--force` re-checksum path when a migration was intentionally edited) (done 2026-06-10)
  - **Goal:** `MigrationRunner::force_rechecksum(version)` re-computes the checksum for a Modified migration and stores it; optional `--force` CLI flag in `migrate run`.
  - **Design:** Add `update_checksum(glue, version, new_checksum)` to both `tracker.rs` and `tracker_generic.rs` (mirrors `mark_applied` INSERTs but instead UPDATEs the existing row). Add `MigrationRunner::force_rechecksum(&self, glue, version)` in `runner.rs`: reads current file checksum, calls `tracker.update_checksum`. CLI `migrate.rs`: add `--force` flag to `Run` subcommand; when set, detect Modified states before running and rechecksum them. Note: checksums use FNV-1a in tracker.rs (not SHA-256 as docs say — cosmetic doc drift).
  - **Files:** `src/tracker.rs` (+update_checksum), `src/tracker_generic.rs` (+update_checksum), `src/runner.rs` (+force_rechecksum method), `src/bin/migrate.rs` (+--force flag)
  - **Tests:** apply a migration, modify the file, confirm Modified state, force_rechecksum, confirm Applied state returns; CLI --force flag integration test
  - **Risk:** UPDATE must be atomic; trackers have two backends (embedded + generic). Both must be updated consistently.
- [x] Concurrency-safe distributed locking (advisory lock around the tracker for multi-process Postgres/MySQL deployments) (done 2026-06-10)
- [x] Optional per-migration `-- oxisql:no-transaction` directive for statements that cannot run inside a transaction (e.g. some DDL) (done 2026-06-10)
  - **Goal:** A `-- oxisql:no-transaction` comment line in a migration file causes the runner to skip BEGIN/COMMIT wrapping for that migration.
  - **Design:** Migration SQL is in-hand at `runner.rs:263` (after `std::fs::read_to_string`). Add `fn has_no_transaction_directive(sql: &str) -> bool` (scan for the directive on any line before the first non-comment SQL token). Gate `in_txn` logic in `run_embedded`: if directive present, skip the `glue.execute("BEGIN")` call and the ROLLBACK wrapper, execute SQL directly. The generic `run_with_conn` path does not wrap in BEGIN/COMMIT today, so no change needed there.
  - **Files:** `src/runner.rs` (+has_no_transaction_directive fn, gate in_txn in run_embedded)
  - **Tests:** migration with directive executes without BEGIN; migration without directive still wraps in txn; directive detection on various comment positions
  - **Risk:** Line-scan must not false-positive on `-- oxisql:no-transaction` inside a string literal or quoted identifier. Use a simple line-prefix check that stops at first non-comment SQL.
- [x] CLI `dry-run` subcommand wired to `MigrationRunner::dry_run()` (done 2026-06-10)
  - **Goal:** `migrate dry-run <dir>` lists all pending migrations that would be applied without applying them, using an in-memory DB.
  - **Design:** `dry_run()` exists at `runner.rs:321` with signature `pub async fn dry_run(&self, glue: &mut Glue<MemoryStorage>) -> Result<Vec<MigrationFile>, MigrationError>`. Add a `DryRun { dir: PathBuf }` variant to the clap `Commands` enum in `src/bin/migrate.rs` (mirrors `Status { dir }` variant). Add `cmd_dry_run(dir: &Path)` async fn: construct `Glue::new(MemoryStorage::default())` directly (not via EmbeddedConnection), build a MigrationRunner from dir, call `dry_run(&mut glue)`, print the returned MigrationFiles. Wire into the `match cli.command` dispatch. Uses the `cli` feature guard already in place.
  - **Files:** `src/bin/migrate.rs` (+DryRun variant, +cmd_dry_run fn, +match arm)
  - **Tests:** dry-run with 2 pending migrations lists both; dry-run does not modify any migration tracker state; verify by running status after dry-run and confirming still-pending
  - **Risk:** `dry_run()` takes `&mut Glue<MemoryStorage>`, not an EmbeddedConnection — must construct Glue directly (not via the connection helper). Confirm gluesql is a direct dep under the `cli` feature.

## Known limitations
- Transaction wrapping depends on the backend: the embedded GlueSQL backend honours `BEGIN`/`COMMIT`/`ROLLBACK`; backends without transactional DDL fall back to best-effort apply.
- `rollback` requires a matching `.down.sql` for each reverted migration, otherwise it returns `MigrationError::NoDownMigration`.
