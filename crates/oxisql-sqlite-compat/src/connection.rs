//! [`SqliteConnection`] — Limbo-backed implementation of [`oxisql_core::Connection`].
//!
//! # Concurrency model
//!
//! `limbo::Connection` is internally `Arc<Mutex<Arc<limbo_core::Connection>>>` with
//! `unsafe impl Send + Sync`, so it is safe to clone and share across async tasks.
//! `SqliteConnection` is a thin newtype that holds:
//!
//! - `conn: limbo::Connection` — the Limbo connection handle.
//! - `txn_lock: Arc<tokio::sync::Mutex<()>>` — a guard that prevents two async tasks
//!   from issuing `BEGIN` concurrently on the same logical connection.  SQLite does
//!   not support nested transactions, so only one task at a time may hold a
//!   transaction.
//! - `path: String` — the path supplied to [`Builder::new_local`], retained for
//!   diagnostics.
//!
//! # Affected-row count
//!
//! After each DML statement we call `conn.changes()` to read the row count that
//! was committed by the most-recent write transaction.  DDL statements and
//! `BEGIN`/`COMMIT`/`ROLLBACK` leave the counter at 0, which is the correct
//! contract per OxiSQL and `sqlite3_changes()` semantics.
//!
//! # Parameter binding
//!
//! OxiSQL passes `$1`, `$2`, … positional parameters.  SQLite / Limbo expects
//! `?` placeholders.  `types::rewrite_params` performs a quote-aware
//! translation before each statement is prepared.
//!
//! # Schema introspection
//!
//! [`Connection::tables`] queries `sqlite_master`.
//! [`Connection::columns`] uses `PRAGMA table_info`.
//! [`Connection::indexes`] parses `sqlite_master` DDL (PRAGMA index_list/index_info are not
//! yet implemented in Limbo 0.0.22).
//! [`Connection::foreign_keys`] uses `PRAGMA foreign_key_list` — the engine now
//! surfaces FK metadata from its in-memory schema.
//!
//! # Transactions
//!
//! [`Connection::transaction`] issues `BEGIN` and returns a [`SqliteTransaction`]
//! that wraps the same `limbo::Connection`.  The transaction holds a guard on
//! `txn_lock` so that no other task can start a concurrent `BEGIN`.
//! Dropping `SqliteTransaction` without calling `commit` or `rollback` will
//! execute `ROLLBACK` (best-effort, via `Drop`).
//!
//! # Prepared-statement cache
//!
//! All DML and DDL statements pass through an LRU cache keyed by the
//! **rewritten SQL** (after `$N`→`?` translation).  The cache holds up to
//! `STMT_CACHE_CAPACITY` (128) compiled `limbo::Statement` entries per connection
//! (shared across clones of the same connection via `Arc<StdMutex<…>>`).
//!
//! On a cache hit the existing `limbo::Statement` is taken out of the cache,
//! executed via `Statement::execute()` (which calls `reset()` before binding),
//! and returned to the cache after execution.  `Statement::reset()` now also
//! zeroes `Program::n_change` (fixed in oxisqlite-core), so cached statement
//! reuse produces correct per-execution change counts.
//!
//! # ROLLBACK
//!
//! `SqliteTransaction::rollback()` executes the SQL string `"ROLLBACK"` against
//! the engine, exactly mirroring how `commit()` executes `"COMMIT"`.  The engine
//! emits an `AutoCommit { auto_commit: true, rollback: true }` VDBE instruction
//! that discards all pending changes.  The `Drop` impl also fires a best-effort
//! ROLLBACK when the transaction is dropped without an explicit `commit()` or
//! `rollback()`.
//!
//! # Prepared-statement reuse (via SqlitePrepared)
//!
//! Limbo's `Statement` is consumed after a single `execute`/`query` cycle.
//! Our [`PreparedStatement`] wrapper therefore re-prepares on every call.  The
//! API contract (parse-once, bind-many) is satisfied at the OxiSQL trait level
//! even though Limbo does not yet expose a stable compiled-statement cache.

use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex as StdMutex};

use async_trait::async_trait;
use limbo::params::Params as LimboParams;
use limbo::Builder;
use tokio::sync::Mutex as TokioMutex;

// ── statement-cache capacity ───────────────────────────────────────────────────

/// Maximum number of compiled statements retained in the per-connection LRU
/// cache.  Statements are keyed by their rewritten SQL (`?`-placeholder form).
const STMT_CACHE_CAPACITY: usize = 128;

use oxisql_core::{
    ColumnInfo, Connection, ForeignKeyInfo, IndexInfo, OxiSqlError, PreparedStatement, Row,
    TableInfo, TableType, ToSqlValue, Transaction, Value,
};

use crate::error::SqliteCompatError;
use crate::types::{limbo_to_core_typed, rewrite_params, split_statements};

// ── helpers ───────────────────────────────────────────────────────────────────

/// A per-connection LRU cache from rewritten SQL → compiled `limbo::Statement`.
///
/// Wrapped in `Arc<StdMutex<…>>` so it can be cheaply shared when the
/// `SqliteConnection` is cloned.  The std `Mutex` is deliberately chosen over
/// `tokio::sync::Mutex`: the critical section is very short (single hash-lookup
/// or insertion) and never held across an `.await` point.
type StmtCache = Arc<StdMutex<lru::LruCache<String, limbo::Statement>>>;

/// Construct a new, empty [`StmtCache`] with [`STMT_CACHE_CAPACITY`] slots.
fn new_stmt_cache() -> StmtCache {
    // SAFETY: STMT_CACHE_CAPACITY is a positive compile-time constant (128).
    //         `NonZeroUsize::new` returns `None` only for 0, which this is not.
    let cap = NonZeroUsize::new(STMT_CACHE_CAPACITY).unwrap_or(NonZeroUsize::MIN);
    Arc::new(StdMutex::new(lru::LruCache::new(cap)))
}

/// Execute a SQL statement that has already been rewritten to `?` placeholders.
///
/// All statements (DML and DDL) pass through the statement cache uniformly.
/// On a cache miss the statement is compiled via `conn.prepare()`, executed,
/// and stored for future reuse.  On a cache hit the existing `limbo::Statement`
/// is retrieved, executed via `stmt.execute()` (which calls `reset()` before
/// binding, zeroing `n_change` so reuse produces correct per-execution change
/// counts), and returned to the cache.
///
/// If the cached statement was compiled before a schema change (DDL, ALTER,
/// CREATE INDEX, etc.), the engine's `op_transaction` cookie check fires on
/// the first `step()` and returns `SchemaChanged`.  This function catches that
/// error, discards the stale compiled program, re-prepares against the
/// refreshed schema, and retries exactly once.  This transparent re-prepare
/// replaces the old `is_ddl` keyword-prefix heuristic that failed on
/// comment-prefixed DDL and left DML statements stale after schema changes.
///
/// The affected-row count is read from `conn.changes()` after execution,
/// which reflects the count committed by the most recent write transaction on
/// this connection.  DDL and `BEGIN`/`COMMIT`/`ROLLBACK` return 0, which is the
/// correct value per the OxiSQL contract.
///
/// When no `cache` is provided (e.g., in unit tests that bypass the cache) the
/// function falls back to `conn.execute()` followed by `conn.changes()`.
async fn exec_rewritten(
    conn: &limbo::Connection,
    sql: &str,
    limbo_params: Vec<limbo::Value>,
    cache: Option<&StmtCache>,
) -> Result<u64, SqliteCompatError> {
    match cache {
        Some(c) => {
            // Clone before consuming so we can rebuild the parameter list for a
            // re-prepare-and-retry if the engine signals SchemaChanged.
            let retry_params = limbo_params.clone();
            let lp = if limbo_params.is_empty() {
                LimboParams::None
            } else {
                LimboParams::Positional(limbo_params)
            };

            // Take the compiled statement out of the cache (if present).
            // The lock is held only for this short lookup; never across `.await`.
            let cached = {
                let mut guard = c.lock().map_err(|e| {
                    SqliteCompatError::Other(format!("stmt_cache lock poisoned: {e}"))
                })?;
                guard.pop(sql)
            };

            let mut stmt = match cached {
                Some(s) => s,
                None => conn.prepare(sql).await.map_err(SqliteCompatError::from)?,
            };

            match stmt.execute(lp).await {
                Ok(_) => {
                    // Execution succeeded — return the statement to the cache.
                    c.lock()
                        .map_err(|e| {
                            SqliteCompatError::Other(format!("stmt_cache lock poisoned: {e}"))
                        })?
                        .put(sql.to_owned(), stmt);
                }
                Err(e) if e.is_schema_changed() => {
                    // The schema changed after this statement was compiled. Drop
                    // the stale program, re-compile against the refreshed schema,
                    // and retry exactly once.
                    drop(stmt);
                    let retry_lp = if retry_params.is_empty() {
                        LimboParams::None
                    } else {
                        LimboParams::Positional(retry_params)
                    };
                    let mut fresh = conn.prepare(sql).await.map_err(SqliteCompatError::from)?;
                    fresh
                        .execute(retry_lp)
                        .await
                        .map_err(SqliteCompatError::from)?;
                    c.lock()
                        .map_err(|e| {
                            SqliteCompatError::Other(format!("stmt_cache lock poisoned: {e}"))
                        })?
                        .put(sql.to_owned(), fresh);
                }
                Err(e) => return Err(SqliteCompatError::from(e)),
            }

            let n = conn
                .changes()
                .map_err(|e| SqliteCompatError::Other(format!("changes() failed: {e}")))?;
            Ok(n.max(0) as u64)
        }
        None => {
            // ── no-cache path (uncommon; bypasses the cache entirely) ──────────
            let lp = if limbo_params.is_empty() {
                LimboParams::None
            } else {
                LimboParams::Positional(limbo_params)
            };
            conn.execute(sql, lp)
                .await
                .map_err(SqliteCompatError::from)?;
            let n = conn
                .changes()
                .map_err(|e| SqliteCompatError::Other(format!("changes() failed: {e}")))?;
            Ok(n.max(0) as u64)
        }
    }
}

/// Execute a query that has already been rewritten to `?` placeholders and
/// collect all result rows.
///
/// Column declared types (e.g. `"DATE"`, `"TIMESTAMP"`, `"UUID"`) are
/// collected from the prepared statement and forwarded to [`limbo_to_core_typed`]
/// so that richer [`Value`] variants are produced when appropriate.
async fn query_rewritten(
    conn: &limbo::Connection,
    sql: &str,
    limbo_params: Vec<limbo::Value>,
) -> Result<Vec<Row>, SqliteCompatError> {
    let lp = if limbo_params.is_empty() {
        LimboParams::None
    } else {
        LimboParams::Positional(limbo_params)
    };

    let mut stmt = conn.prepare(sql).await.map_err(SqliteCompatError::from)?;

    // Collect column names and declared types together.
    let col_info: Vec<(String, Option<String>)> = stmt
        .columns()
        .iter()
        .map(|c| (c.name().to_owned(), c.decl_type().map(str::to_owned)))
        .collect();

    let col_names: Vec<String> = col_info.iter().map(|(name, _)| name.clone()).collect();

    let mut rows_iter = stmt.query(lp).await.map_err(SqliteCompatError::from)?;

    let mut rows: Vec<Row> = Vec::new();
    while let Some(limbo_row) = rows_iter.next().await.map_err(SqliteCompatError::from)? {
        let mut values: Vec<Value> = Vec::with_capacity(col_info.len());
        for idx in 0..limbo_row.column_count() {
            let raw = limbo_row.get_value(idx).map_err(SqliteCompatError::from)?;
            let decl = col_info.get(idx).and_then(|(_, dt)| dt.as_deref());
            values.push(limbo_to_core_typed(raw, decl)?);
        }
        rows.push(Row::new(col_names.clone(), values));
    }
    Ok(rows)
}

// ── SqliteConnection ──────────────────────────────────────────────────────────

/// A Limbo-backed SQLite connection implementing [`Connection`].
///
/// Create via [`SqliteConnection::open`] (file path) or
/// [`SqliteConnection::open_memory`] (`:memory:`).
///
/// # Statement cache
///
/// Each `SqliteConnection` maintains an LRU cache of compiled `limbo::Statement`
/// objects (capacity: `STMT_CACHE_CAPACITY` = 128).  The cache is shared across
/// clones of the same connection (the clones share the underlying
/// `limbo::Connection`) and is updated on every DML/DDL execution.  Cache hits
/// save the per-statement parse-and-compile round-trip inside Limbo.
#[derive(Clone)]
pub struct SqliteConnection {
    conn: limbo::Connection,
    txn_lock: Arc<TokioMutex<()>>,
    stmt_cache: StmtCache,
    path: String,
}

impl std::fmt::Debug for SqliteConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let cache_len = self.stmt_cache.lock().map(|g| g.len()).unwrap_or(0);
        f.debug_struct("SqliteConnection")
            .field("path", &self.path)
            .field("stmt_cache_len", &cache_len)
            .finish_non_exhaustive()
    }
}

impl SqliteConnection {
    /// Open a Limbo database at the given file path.
    ///
    /// Pass `":memory:"` for an in-memory database, or use
    /// [`open_memory`][Self::open_memory] for clarity.
    ///
    /// # Errors
    ///
    /// Returns [`OxiSqlError`] if the file cannot be opened or created.
    pub async fn open(path: &str) -> Result<Self, OxiSqlError> {
        let db = Builder::new_local(path)
            .build()
            .await
            .map_err(|e| OxiSqlError::Other(format!("limbo open error: {e}")))?;
        let conn = db
            .connect()
            .map_err(|e| OxiSqlError::Other(format!("limbo connect error: {e}")))?;
        Ok(Self {
            conn,
            txn_lock: Arc::new(TokioMutex::new(())),
            stmt_cache: new_stmt_cache(),
            path: path.to_owned(),
        })
    }

    /// Open a fresh in-memory Limbo database.
    ///
    /// # Errors
    ///
    /// Returns [`OxiSqlError`] if the engine cannot be initialised.
    pub async fn open_memory() -> Result<Self, OxiSqlError> {
        Self::open(":memory:").await
    }

    /// Open a Limbo-backed connection from an in-memory SQLite database image.
    ///
    /// The `bytes` are copied into an in-memory page store; no temporary file
    /// is ever created, so this works on WASI, in the browser, and on
    /// read-only filesystems. Mirrors SQLite's `sqlite3_deserialize()` /
    /// rusqlite's `Connection::deserialize`. See
    /// [`limbo::Database::open_from_bytes`].
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # async fn run() -> Result<(), oxisql_core::OxiSqlError> {
    /// use oxisql_core::Connection;
    /// use oxisql_sqlite_compat::SqliteConnection;
    ///
    /// // `image` is a complete SQLite database file loaded into memory,
    /// // e.g. `include_bytes!("../data/app.db")`.
    /// let image: &[u8] = get_database_image();
    /// let conn = SqliteConnection::open_from_bytes(image).await?;
    /// let rows = conn.query("SELECT count(*) FROM sqlite_master", &[]).await?;
    /// # let _ = rows;
    /// # Ok(())
    /// # }
    /// # fn get_database_image() -> &'static [u8] { &[] }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`OxiSqlError`] if `bytes` is not a valid SQLite database image
    /// (too short, wrong magic header, or an invalid page size). Never panics
    /// on malformed input.
    pub async fn open_from_bytes(bytes: &[u8]) -> Result<Self, OxiSqlError> {
        let db = limbo::Database::open_from_bytes(bytes)
            .map_err(|e| OxiSqlError::Other(format!("limbo open_from_bytes error: {e}")))?;
        let conn = db
            .connect()
            .map_err(|e| OxiSqlError::Other(format!("limbo connect error: {e}")))?;
        Ok(Self {
            conn,
            txn_lock: Arc::new(TokioMutex::new(())),
            stmt_cache: new_stmt_cache(),
            path: "<memory:bytes>".to_owned(),
        })
    }

    /// Return the path this connection was opened with.
    pub fn path(&self) -> &str {
        &self.path
    }
}

// ── Connection impl ───────────────────────────────────────────────────────────

#[async_trait]
impl Connection for SqliteConnection {
    async fn execute(&self, sql: &str, params: &[&dyn ToSqlValue]) -> Result<u64, OxiSqlError> {
        let (rewritten, limbo_params) = rewrite_params(sql, params).map_err(OxiSqlError::from)?;
        exec_rewritten(&self.conn, &rewritten, limbo_params, Some(&self.stmt_cache))
            .await
            .map_err(OxiSqlError::from)
    }

    async fn query(&self, sql: &str, params: &[&dyn ToSqlValue]) -> Result<Vec<Row>, OxiSqlError> {
        let (rewritten, limbo_params) = rewrite_params(sql, params).map_err(OxiSqlError::from)?;
        query_rewritten(&self.conn, &rewritten, limbo_params)
            .await
            .map_err(OxiSqlError::from)
    }

    async fn transaction(&self) -> Result<Box<dyn Transaction + '_>, OxiSqlError> {
        // Acquire the exclusive transaction lock before issuing BEGIN.
        // This prevents a second task from starting a concurrent transaction
        // on the same SqliteConnection clone.
        let guard = self.txn_lock.lock().await;
        self.conn
            .execute("BEGIN", LimboParams::None)
            .await
            .map_err(|e| OxiSqlError::Other(format!("BEGIN failed: {e}")))?;
        Ok(Box::new(SqliteTransaction {
            conn: self.conn.clone(),
            // Share the connection-level stmt_cache so that DML executed inside
            // a transaction also benefits from cached compiled statements.
            stmt_cache: Arc::clone(&self.stmt_cache),
            // Transfer ownership of the mutex guard into the transaction.
            // The guard is released when SqliteTransaction is dropped.
            _guard: guard,
            done: false,
        }))
    }

    async fn execute_batch(&self, sql: &str) -> Result<u64, OxiSqlError> {
        // Token-aware split: honours `;` inside string literals, quoted
        // identifiers, block comments, and line comments.
        let stmts = split_statements(sql);
        let mut total = 0u64;
        for stmt in stmts {
            total += self.execute(stmt, &[]).await?;
        }
        Ok(total)
    }

    async fn ping(&self) -> Result<(), OxiSqlError> {
        self.query("SELECT 1", &[]).await?;
        Ok(())
    }

    async fn prepare(&self, sql: &str) -> Result<Box<dyn PreparedStatement + '_>, OxiSqlError> {
        Ok(Box::new(SqlitePrepared {
            conn: &self.conn,
            stmt_cache: Arc::clone(&self.stmt_cache),
            sql: sql.to_owned(),
        }))
    }

    // ── Schema introspection ──────────────────────────────────────────────────

    async fn tables(&self) -> Result<Vec<TableInfo>, OxiSqlError> {
        let rows = self
            .query(
                "SELECT name, type FROM sqlite_master \
                 WHERE type IN ('table','view') AND name NOT LIKE 'sqlite_%' \
                 ORDER BY name",
                &[],
            )
            .await?;

        let infos = rows
            .into_iter()
            .map(|row| {
                let name = row
                    .get_by_index(0)
                    .and_then(|v| {
                        if let Value::Text(s) = v {
                            Some(s.clone())
                        } else {
                            None
                        }
                    })
                    .unwrap_or_default();
                let ttype_str = row
                    .get_by_index(1)
                    .and_then(|v| {
                        if let Value::Text(s) = v {
                            Some(s.as_str())
                        } else {
                            None
                        }
                    })
                    .unwrap_or("table");
                let table_type = match ttype_str {
                    "view" => TableType::View,
                    _ => TableType::Base,
                };
                TableInfo {
                    name,
                    schema: None,
                    table_type,
                }
            })
            .collect();
        Ok(infos)
    }

    async fn columns(&self, table: &str) -> Result<Vec<ColumnInfo>, OxiSqlError> {
        // PRAGMA table_info returns: cid, name, type, notnull, dflt_value, pk
        let sql = format!("PRAGMA table_info(\"{table}\")");
        let rows = self.query(&sql, &[]).await?;

        let infos = rows
            .into_iter()
            .map(|row| {
                // Helper: get column by index as string or empty string.
                let text_at = |r: &Row, idx: usize| -> String {
                    r.get_by_index(idx)
                        .and_then(|v| match v {
                            Value::Text(s) => Some(s.clone()),
                            Value::I64(n) => Some(n.to_string()),
                            Value::Null => Some(String::new()),
                            _ => None,
                        })
                        .unwrap_or_default()
                };
                let i64_at = |r: &Row, idx: usize| -> i64 {
                    r.get_by_index(idx)
                        .and_then(|v| {
                            if let Value::I64(n) = v {
                                Some(*n)
                            } else {
                                None
                            }
                        })
                        .unwrap_or(0)
                };

                let ordinal = i64_at(&row, 0) as u32 + 1; // cid is 0-based
                let name = text_at(&row, 1);
                let data_type = text_at(&row, 2);
                let notnull = i64_at(&row, 3) != 0;
                let default_val = row.get_by_index(4).and_then(|v| match v {
                    Value::Text(s) => Some(s.clone()),
                    Value::Null => None,
                    other => Some(format!("{other:?}")),
                });

                ColumnInfo {
                    name,
                    ordinal_position: ordinal,
                    data_type,
                    nullable: !notnull,
                    default: default_val,
                    max_length: None,
                    numeric_precision: None,
                    numeric_scale: None,
                }
            })
            .collect();
        Ok(infos)
    }

    async fn indexes(&self, table: &str) -> Result<Vec<IndexInfo>, OxiSqlError> {
        // PRAGMA index_list and PRAGMA index_info are not yet implemented in limbo 0.0.22.
        // Fall back to sqlite_master for index names and uniqueness, then parse
        // the index SQL to extract column names.  This is best-effort: multi-column
        // indexes and expression indexes may not parse perfectly.
        let sql = "SELECT name, sql FROM sqlite_master \
                   WHERE type='index' AND tbl_name=$1 AND name NOT LIKE 'sqlite_%'";
        let rows = self.query(sql, &[&table]).await?;

        let mut infos: Vec<IndexInfo> = Vec::new();
        for row in rows {
            let name = row
                .get_by_index(0)
                .and_then(|v| {
                    if let Value::Text(s) = v {
                        Some(s.clone())
                    } else {
                        None
                    }
                })
                .unwrap_or_default();
            let idx_sql = row
                .get_by_index(1)
                .and_then(|v| {
                    if let Value::Text(s) = v {
                        Some(s.clone())
                    } else {
                        None
                    }
                })
                .unwrap_or_default();

            // Detect UNIQUE from the CREATE INDEX / CREATE UNIQUE INDEX statement.
            let upper = idx_sql.to_ascii_uppercase();
            let unique = upper.contains("UNIQUE");

            // Extract column list between the last `(` and `)`.
            let columns: Vec<String> =
                if let (Some(open), Some(close)) = (idx_sql.rfind('('), idx_sql.rfind(')')) {
                    idx_sql[open + 1..close]
                        .split(',')
                        .map(|c| c.trim().to_string())
                        .filter(|c| !c.is_empty())
                        .collect()
                } else {
                    vec![]
                };

            infos.push(IndexInfo {
                name,
                columns,
                unique,
                primary: false,
            });
        }
        Ok(infos)
    }

    async fn foreign_keys(&self, table: &str) -> Result<Vec<ForeignKeyInfo>, OxiSqlError> {
        // Use PRAGMA foreign_key_list — the engine now surfaces FK metadata
        // directly from the in-memory schema, avoiding brittle DDL text parsing.
        let escaped = table.replace('"', "\"\"");
        let sql = format!("PRAGMA foreign_key_list(\"{}\")", escaped);
        let rows = query_rewritten(&self.conn, &sql, vec![])
            .await
            .map_err(OxiSqlError::from)?;

        // PRAGMA foreign_key_list columns (by index):
        //  0: id INTEGER   — FK index within the table
        //  1: seq INTEGER  — column position within a composite FK
        //  2: table TEXT   — parent table name
        //  3: from TEXT    — child column name
        //  4: to TEXT/NULL — parent column name (NULL = implicit PK ref)
        //  5: on_update TEXT
        //  6: on_delete TEXT
        //  7: match TEXT
        let mut infos: Vec<ForeignKeyInfo> = Vec::with_capacity(rows.len());
        for row in &rows {
            let id = match row.get_by_index(0) {
                Some(Value::I64(v)) => *v,
                _ => 0,
            };
            let from_col = match row.get_by_index(3) {
                Some(Value::Text(s)) => s.clone(),
                _ => continue,
            };
            let foreign_table = match row.get_by_index(2) {
                Some(Value::Text(s)) => s.clone(),
                _ => continue,
            };
            let foreign_column = match row.get_by_index(4) {
                Some(Value::Text(s)) => s.clone(),
                _ => String::new(),
            };
            let on_update = match row.get_by_index(5) {
                Some(Value::Text(s)) => Some(s.clone()),
                _ => None,
            };
            let on_delete = match row.get_by_index(6) {
                Some(Value::Text(s)) => Some(s.clone()),
                _ => None,
            };
            let constraint_name = format!("fk_{table}_{id}");
            infos.push(ForeignKeyInfo {
                constraint_name,
                column: from_col,
                foreign_table,
                foreign_column,
                on_update,
                on_delete,
            });
        }
        Ok(infos)
    }
}

// ── SqliteTransaction ─────────────────────────────────────────────────────────

/// A SQLite transaction backed by raw `BEGIN`/`COMMIT`/`ROLLBACK` statements.
///
/// Holds a guard on the connection-level transaction mutex so that no other
/// async task can start a concurrent `BEGIN` on the same `SqliteConnection`.
/// When dropped without an explicit `commit` or `rollback`, the transaction
/// attempts a best-effort `ROLLBACK` via a background task.
pub struct SqliteTransaction<'a> {
    conn: limbo::Connection,
    stmt_cache: StmtCache,
    _guard: tokio::sync::MutexGuard<'a, ()>,
    done: bool,
}

impl<'a> Drop for SqliteTransaction<'a> {
    fn drop(&mut self) {
        if !self.done {
            // Best-effort rollback on implicit drop.  We cannot `.await` inside
            // `drop`, so we spawn a fire-and-forget task.  The mutex guard is
            // released when `SqliteTransaction` is fully dropped (after this
            // function body returns).
            let conn = self.conn.clone();
            tokio::spawn(async move {
                if let Err(e) = conn.execute("ROLLBACK", LimboParams::None).await {
                    log::warn!("SqliteTransaction drop: ROLLBACK failed: {e}");
                }
            });
        }
    }
}

#[async_trait]
impl<'a> Transaction for SqliteTransaction<'a> {
    async fn execute(&mut self, sql: &str, params: &[&dyn ToSqlValue]) -> Result<u64, OxiSqlError> {
        let (rewritten, limbo_params) = rewrite_params(sql, params).map_err(OxiSqlError::from)?;
        exec_rewritten(&self.conn, &rewritten, limbo_params, Some(&self.stmt_cache))
            .await
            .map_err(OxiSqlError::from)
    }

    async fn query(
        &mut self,
        sql: &str,
        params: &[&dyn ToSqlValue],
    ) -> Result<Vec<Row>, OxiSqlError> {
        let (rewritten, limbo_params) = rewrite_params(sql, params).map_err(OxiSqlError::from)?;
        query_rewritten(&self.conn, &rewritten, limbo_params)
            .await
            .map_err(OxiSqlError::from)
    }

    async fn commit(mut self: Box<Self>) -> Result<(), OxiSqlError> {
        self.done = true;
        self.conn
            .execute("COMMIT", LimboParams::None)
            .await
            .map_err(|e| OxiSqlError::Other(format!("COMMIT failed: {e}")))?;
        Ok(())
    }

    async fn rollback(mut self: Box<Self>) -> Result<(), OxiSqlError> {
        // Mark done so that Drop does not attempt a second ROLLBACK.
        self.done = true;
        self.conn
            .execute("ROLLBACK", LimboParams::None)
            .await
            .map_err(|e| OxiSqlError::Other(format!("ROLLBACK failed: {e}")))?;
        Ok(())
    }
}

// ── SqlitePrepared ────────────────────────────────────────────────────────────

/// A prepared statement backed by the connection-level LRU cache.
///
/// On each `execute()` call the cached `limbo::Statement` is retrieved (or
/// compiled fresh on a miss), executed, and returned to the cache.  Because
/// `Statement::reset()` now zeroes `n_change`, every execution sees a correct
/// change count without re-parsing the SQL.
pub struct SqlitePrepared<'a> {
    conn: &'a limbo::Connection,
    stmt_cache: StmtCache,
    sql: String,
}

#[async_trait]
impl<'a> PreparedStatement for SqlitePrepared<'a> {
    async fn execute(&mut self, params: &[&dyn ToSqlValue]) -> Result<u64, OxiSqlError> {
        let (rewritten, limbo_params) =
            rewrite_params(&self.sql, params).map_err(OxiSqlError::from)?;
        exec_rewritten(self.conn, &rewritten, limbo_params, Some(&self.stmt_cache))
            .await
            .map_err(OxiSqlError::from)
    }

    async fn query(&mut self, params: &[&dyn ToSqlValue]) -> Result<Vec<Row>, OxiSqlError> {
        let (rewritten, limbo_params) =
            rewrite_params(&self.sql, params).map_err(OxiSqlError::from)?;
        query_rewritten(self.conn, &rewritten, limbo_params)
            .await
            .map_err(OxiSqlError::from)
    }

    fn sql(&self) -> &str {
        &self.sql
    }
}
