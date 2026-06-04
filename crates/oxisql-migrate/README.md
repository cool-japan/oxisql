# oxisql-migrate — Directory-based SQL migration runner for OxiSQL

[![Crates.io](https://img.shields.io/crates/v/oxisql-migrate.svg)](https://crates.io/crates/oxisql-migrate)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

`oxisql-migrate` applies ordered SQL migration files against any OxiSQL backend. Migrations are plain `.sql` files discovered from a directory, tracked in a `_oxisql_migrations` table, and applied in ascending version order.

## Installation

```toml
[dependencies]
oxisql-migrate = { version = "0.1.1", features = ["migrate"] }
```

The `migrate` feature enables the runner, tracker, and pool integration. Without it, only `MigrationError`, `MigrateOptions`, and the `scanner` module are compiled.

## Migration Filename Format

```
migrations/
  20230101000000__create_users.sql
  20230101000001__insert_seed_data.sql
  20230101000002__alter_users_add_email.sql
```

Pattern: `{14-digit-timestamp}__{descriptive_name}.sql`

The 14-digit timestamp is the migration version. Files are applied in ascending numeric version order.

## Quick Start

```rust
use gluesql::prelude::{Glue, MemoryStorage};
use oxisql_migrate::runner::MigrationRunner;

#[tokio::main]
async fn main() -> Result<(), oxisql_migrate::MigrationError> {
    let mut glue = Glue::new(MemoryStorage::default());
    let runner = MigrationRunner::new("migrations/");
    let applied = runner.run_embedded(&mut glue).await?;
    println!("Applied {applied} migration(s)");
    Ok(())
}
```

## API Overview

### `MigrationRunner`

| Method | Description |
|--------|-------------|
| `MigrationRunner::new(dir)` | Create a runner with default options pointing at `dir` |
| `MigrationRunner::new_with_options(dir, opts)` | Create a runner with custom `MigrateOptions` |
| `runner.run_embedded(&mut glue)` | Run against a `Glue<MemoryStorage>` directly |
| `runner.run_with_conn(conn)` | Run against any `&dyn Connection` backend |
| `runner.run_pooled(pool)` | Run via an `EmbeddedPool` (`pool` feature required) |
| `runner.run_with_pool(pool)` | Run via the unified `OxidbPool` enum (`pool` feature required) |
| `runner.status(conn)` | Return a list of `(MigrationFile, MigrationState)` pairs |
| `runner.invalidate_cache()` | Force re-scan of migration directory on next call |

All `run_*` methods are idempotent: they return `Ok(0)` when all migrations are already applied.

### `MigrationState`

| Variant | Description |
|---------|-------------|
| `Applied` | Migration has been applied (in tracker) |
| `Pending` | Migration is not yet applied |
| `Modified` | Applied but file checksum no longer matches |
| `Orphaned` | Applied but file no longer exists on disk |

### `MigrateOptions`

```rust
use oxisql_migrate::MigrateOptions;

let opts = MigrateOptions {
    // Table name for the migration tracker (default: "_oxisql_migrations")
    tracker_table: "_oxisql_migrations".to_string(),
    // When true: log pending migrations but skip execution
    dry_run: false,
    // When Some(v): only apply migrations with version <= v
    target_version: None,
};

let runner = MigrationRunner::new_with_options("migrations/", opts);
```

### `MigrationError`

| Variant | Condition |
|---------|-----------|
| `Io(std::io::Error)` | Filesystem read failure |
| `InvalidFilename(String)` | File doesn't match the expected naming pattern |
| `Execution(String)` | SQL execution failed |
| `Parse(String)` | SQL in a migration file has a syntax error |
| `ChecksumMismatch { version, name }` | Applied migration was modified after applying |
| `NoDownMigration { version, name }` | Rollback requested but no `.down.sql` file exists |
| `Connection(String)` | Pool checkout or connection failure |

### How the runner works

1. Scans `dir` with `scanner::scan_migrations` and caches the result.
2. Ensures the `_oxisql_migrations` tracking table exists (created on first run).
3. Queries which versions are already recorded.
4. Verifies checksums of all previously-applied migrations.
5. Filters to pending migrations (not in tracker), optionally bounded by `target_version`.
6. Validates SQL syntax using `sqlparser` before executing any statements.
7. Executes each pending file's SQL in ascending version order.
8. Records each file in the tracker after successful execution.

### Tracker table

The runner creates and manages a `_oxisql_migrations` table automatically:

```sql
CREATE TABLE IF NOT EXISTS _oxisql_migrations (
    version  BIGINT PRIMARY KEY,
    name     TEXT NOT NULL,
    checksum TEXT NOT NULL,
    applied_at TEXT NOT NULL
);
```

The table name is configurable via `MigrateOptions::tracker_table`.

## Test Status

As of 2026-05-30: **36 tests passing**.

## License

Apache-2.0 — COOLJAPAN OU (Team Kitasan)
