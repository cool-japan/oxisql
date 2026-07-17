# oxisql-migrate — Directory-based SQL migration runner for OxiSQL

[![Crates.io](https://img.shields.io/crates/v/oxisql-migrate.svg)](https://crates.io/crates/oxisql-migrate)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

Directory-based SQL migration runner with 14-digit timestamps, SHA-256 checksum
tracking, transactional apply, and multi-backend support.

**Status: Stable.**

## What it is

`oxisql-migrate` applies ordered `.sql` migration files against any OxiSQL backend.
Migrations are plain SQL files discovered from a directory, identified by a 14-digit
timestamp version, tracked in a `_oxisql_migrations` table, and applied in ascending
version order. Each migration runs inside its own transaction
(`BEGIN` … `COMMIT`, with automatic `ROLLBACK` on failure), and a SHA-256 checksum of
every applied file is recorded so later runs can detect post-apply modification.
Re-runs are idempotent, and an optional `.down.sql` file enables rollback to a target
version.

## Installation (0.3.3)

```toml
[dependencies]
oxisql-migrate = { version = "0.3.3", features = ["migrate"] }
```

The `migrate` feature enables the runner, tracker, and the `sqlparser`/`gluesql`/
`tokio` dependencies. Without it, only `MigrationError`, `MigrateOptions`, and the
`scanner` module are compiled.

- MSRV: **1.89** · edition **2021** · `#![forbid(unsafe_code)]`

## Migration filename format

```
migrations/
  20230101000000__create_users.sql
  20230101000000__create_users.down.sql      # optional, enables rollback
  20230101000001__insert_seed_data.sql
  20230101000002__alter_users_add_email.sql
```

Pattern: `{14-digit-timestamp}__{descriptive_name}.sql` (with an optional matching
`.down.sql` for reverse migrations). The 14-digit timestamp is the migration version;
files apply in ascending numeric version order.

## Quick start

```rust
use gluesql::prelude::{Glue, MemoryStorage};
use oxisql_migrate::runner::MigrationRunner;

#[tokio::main]
async fn main() -> Result<(), oxisql_migrate::MigrationError> {
    let mut glue = Glue::new(MemoryStorage::default());

    let runner = MigrationRunner::new("migrations/");
    let applied = runner.run_embedded(&mut glue).await?;
    println!("Applied {applied} migration(s)");

    // Re-running is idempotent: nothing pending → Ok(0).
    let again = runner.run_embedded(&mut glue).await?;
    assert_eq!(again, 0);
    Ok(())
}
```

### Rollback to a target version

```rust
# use gluesql::prelude::{Glue, MemoryStorage};
# use oxisql_migrate::runner::MigrationRunner;
# async fn demo(glue: &mut Glue<MemoryStorage>) -> Result<(), oxisql_migrate::MigrationError> {
let runner = MigrationRunner::new("migrations/");
runner.run_embedded(glue).await?;

// Revert every migration with a version greater than 20230101000000,
// applying each migration's `.down.sql` in descending order.
runner.rollback(glue, 20230101000000).await?;
# Ok(())
# }
```

## Key API

| Item | Description |
|------|-------------|
| `MigrationRunner::new(dir)` | Runner with default options pointing at `dir` |
| `MigrationRunner::new_with_options(dir, opts)` | Runner with custom `MigrateOptions` |
| `runner.run_embedded(&mut glue)` | Run against a `Glue<MemoryStorage>` directly |
| `runner.run_with_conn(&dyn Connection)` | Run against any `oxisql_core::Connection` backend |
| `runner.run_pooled(&EmbeddedPool)` | Run via an `EmbeddedPool` (`pool` feature) |
| `runner.run_with_pool(&OxidbPool)` | Run via the unified `OxidbPool` enum (`pool` feature) |
| `runner.status(conn)` | `Vec<(MigrationFile, MigrationState)>` for every known migration |
| `runner.dry_run()` | Scan and report pending migrations without executing — `Vec<MigrationFile>` |
| `runner.rollback(glue, target_version)` | Revert applied migrations down to `target_version` via `.down.sql` |
| `runner.invalidate_cache()` | Force a re-scan of the migration directory on the next call |
| `MigrationFile` | `version: i64`, `name`, `up_path`, `down_path: Option<_>`, `checksum` |
| `MigrationState` | `Applied`, `Pending`, `Modified`, `Orphaned` |
| `MigrateOptions` | `tracker_table` (default `"_oxisql_migrations"`), `dry_run`, `target_version` |
| `MigrationError` | `Io`, `InvalidFilename`, `Execution`, `Parse`, `ChecksumMismatch`, `NoDownMigration`, `Connection` |
| `scanner::scan_migrations(dir)` | Standalone directory scanner → sorted `MigrationFile` list |
| `TrackerBackend` (trait) | Pluggable tracker so embedded/Postgres/MySQL trackers are interchangeable |

All `run_*` methods are idempotent: they return `Ok(0)` when every migration is
already applied.

### `MigrationState`

| Variant | Description |
|---------|-------------|
| `Applied` | Migration is recorded in the tracker and its checksum still matches |
| `Pending` | Migration is on disk but not yet applied |
| `Modified` | Applied, but the file's checksum no longer matches what was recorded |
| `Orphaned` | Recorded as applied, but the `.sql` file no longer exists on disk |

### `MigrateOptions`

```rust
use oxisql_migrate::MigrateOptions;

let opts = MigrateOptions {
    tracker_table: "_oxisql_migrations".to_string(), // tracker table name
    dry_run: false,                                  // log pending, skip execution
    target_version: None,                            // Some(v) ⇒ only apply version <= v
};

let runner = oxisql_migrate::runner::MigrationRunner::new_with_options("migrations/", opts);
```

### `MigrationError`

| Variant | Condition |
|---------|-----------|
| `Io(std::io::Error)` | Filesystem read failure |
| `InvalidFilename(String)` | File does not match the expected naming pattern |
| `Execution(String)` | SQL execution failed |
| `Parse(String)` | SQL in a migration file has a syntax error |
| `ChecksumMismatch { version, name }` | An applied migration was modified after applying |
| `NoDownMigration { version, name }` | Rollback requested but no `.down.sql` file exists |
| `Connection(String)` | Pool checkout or connection failure |

### How the runner works

1. Scans `dir` with `scanner::scan_migrations` and caches the result.
2. Ensures the `_oxisql_migrations` tracking table exists (created on first run).
3. Queries which versions are already recorded.
4. Verifies SHA-256 checksums of all previously-applied migrations (→ `ChecksumMismatch` on drift).
5. Filters to pending migrations, optionally bounded by `target_version`.
6. Validates SQL syntax with `sqlparser` before executing any statements.
7. Executes each pending file inside its own transaction (`BEGIN` … `COMMIT`), with automatic `ROLLBACK` on failure, in ascending version order.
8. Records each file (version, name, checksum, `applied_at`) in the tracker after success.

### Tracker table

The runner creates and manages a tracker table automatically (name configurable via
`MigrateOptions::tracker_table`):

```sql
CREATE TABLE IF NOT EXISTS _oxisql_migrations (
    version    BIGINT PRIMARY KEY,
    name       TEXT NOT NULL,
    checksum   TEXT NOT NULL,
    applied_at TEXT NOT NULL
);
```

## Feature flags

| Feature | Effect |
|---------|--------|
| `migrate` | Enables the runner, tracker, and `sqlparser`/`gluesql`/`tokio` deps |
| `pool` | Adds `run_pooled(&EmbeddedPool)` and `run_with_pool(&OxidbPool)` (implies `embedded`) |
| `cli` | Builds the `oxisql-migrate` binary: `run`, `status`, `rollback --version <v>` |

## CLI

With the `cli` feature, a small binary is produced:

```bash
oxisql-migrate run                       # apply all pending migrations
oxisql-migrate status                    # print each migration's state
oxisql-migrate rollback --version 20230101000000   # revert down to a target version
```

## Test coverage

**47 tests pass** with default features; **51 tests pass** with `--all-features`
(49 integration + 2 doc), **1 ignored** (a live-server-gated PostgreSQL
advisory-lock test). Coverage includes multi-file ordering, idempotent re-runs,
checksum-mismatch detection, orphaned-migration detection, concurrent runs (no
double-apply), malformed-SQL handling, empty-directory handling, down-migration
rollback, distributed locking, and pooled execution.

## See also

This crate is one of a 17-crate Pure-Rust workspace. See the
[workspace README](../../README.md); pooling lives in
[`oxisql-pool`](../oxisql-pool/README.md).

## License

Apache-2.0 — Copyright © 2024–2026 COOLJAPAN OU (Team Kitasan).
