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
//! Limbo's `execute()` returns a status code rather than an affected-row count
//! (0 = Done, 2 = unexpected Row, 3 = Interrupt, 4 = Busy).  After each DML
//! statement we issue `SELECT changes()` to retrieve the actual count from the
//! engine.  DDL statements and `BEGIN`/`COMMIT`/`ROLLBACK` return 0 from
//! `changes()`, which is the correct contract.
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
//! [`Connection::foreign_keys`] parses `sqlite_master` DDL (PRAGMA foreign_key_list is not
//! yet implemented in Limbo 0.0.22).
//!
//! # Transactions
//!
//! [`Connection::transaction`] issues `BEGIN` and returns a [`SqliteTransaction`]
//! that wraps the same `limbo::Connection`.  The transaction holds a guard on
//! `txn_lock` so that no other task can start a concurrent `BEGIN`.
//! Dropping `SqliteTransaction` without calling `commit` or `rollback` will
//! execute `ROLLBACK` (best-effort, via `Drop`).
//!
//! # Prepared-statement cache (Phase B)
//!
//! All DML and DDL statements pass through an LRU cache keyed by the
//! **rewritten SQL** (after `$N`→`?` translation).  The cache holds up to
//! `STMT_CACHE_CAPACITY` (128) compiled `limbo::Statement` entries per connection
//! (shared across clones of the same connection via `Arc<StdMutex<…>>`).
//!
//! ## Limbo 0.0.22 correctness note
//!
//! Ideally, cache hits would skip `conn.prepare()` entirely by calling
//! `Statement::execute()` on the cached (cloned) statement.  However, limbo
//! 0.0.22 has a bug: `limbo_core::Statement::reset()` clears `ProgramState`
//! but NOT `Program::n_change`.  Because `n_change` accumulates across resets,
//! reusing a statement via `stmt.execute()` causes `SELECT changes()` to
//! return an inflated count on every subsequent execution.  Until limbo
//! exposes a full-reset API (expected in 0.1+), this crate:
//!
//! 1. Populates the cache on the first compilation of each SQL string (miss path).
//! 2. Still calls `conn.execute()` on every execution — which re-prepares
//!    internally — to guarantee a fresh `n_change = 0` counter.
//!
//! The cache is therefore a **readiness infrastructure** for the moment limbo
//! fixes the reset bug.  Once that is fixed, step 2 can be replaced with
//! `stmt.execute()` on the cached clone for a full parse-skip optimisation.
//!
//! # ROLLBACK limitation
//!
//! Limbo 0.0.22 does not implement ROLLBACK (`bail_parse_error!("ROLLBACK not
//! supported yet")`).  `SqliteTransaction::rollback()` returns
//! `Err(OxiSqlError::Other("ROLLBACK is not supported by the limbo 0.0.22
//! engine …"))` immediately rather than propagating a cryptic parse-error.
//! The `Drop` impl continues to attempt a best-effort ROLLBACK (fire-and-forget)
//! so that resources are released when the transaction is dropped without an
//! explicit `commit()` or `rollback()`.
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
use crate::types::{limbo_to_core, rewrite_params, split_statements};

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
/// If `cache` is provided the function looks up the compiled statement by `sql`
/// (a cache hit), re-uses it (Limbo's `Statement::execute()` calls `reset()`
/// before binding so reuse is safe), and inserts a fresh compiled statement on
/// a cache miss.
///
/// Returns the number of rows affected via `SELECT changes()`.
async fn exec_rewritten(
    conn: &limbo::Connection,
    sql: &str,
    limbo_params: Vec<limbo::Value>,
    cache: Option<&StmtCache>,
) -> Result<u64, SqliteCompatError> {
    let lp = if limbo_params.is_empty() {
        LimboParams::None
    } else {
        LimboParams::Positional(limbo_params)
    };

    // ── Execute the statement ──────────────────────────────────────────────────
    //
    // IMPORTANT — limbo 0.0.22 bug: `limbo_core::Statement::reset()` only
    // resets `ProgramState`, NOT `Program::n_change`.  Because `n_change`
    // accumulates across resets, reusing a cached statement via
    // `stmt.execute()` causes `SELECT changes()` to return an inflated count
    // (N instead of 1 on the Nth reuse).  Until limbo fixes this we always
    // execute through `conn.execute()` — which internally prepares a fresh
    // statement — to guarantee correct change-count semantics.
    //
    // The cache IS still populated (on miss) so the compiled bytecode is
    // retained and this code path can be upgraded to stmt.execute() once
    // limbo adds a reset that also clears n_change.
    if let Some(c) = cache {
        // Populate the cache on first sight of this SQL.
        let is_cached = c
            .lock()
            .map_err(|e| SqliteCompatError::Other(format!("stmt_cache lock poisoned: {e}")))?
            .contains(sql);

        if !is_cached {
            // Cache miss — compile and store for future reference.
            let fresh = conn.prepare(sql).await.map_err(SqliteCompatError::from)?;
            c.lock()
                .map_err(|e| SqliteCompatError::Other(format!("stmt_cache lock poisoned: {e}")))?
                .put(sql.to_owned(), fresh);
        }
    }

    // Always execute via conn.execute() to obtain a fresh n_change counter.
    conn.execute(sql, lp)
        .await
        .map_err(SqliteCompatError::from)?;

    // Retrieve the affected-row count from SQLite's `changes()` function.
    // DDL and `BEGIN`/`COMMIT`/`ROLLBACK` return 0 from `changes()`, which is
    // the correct value per the OxiSQL contract.
    let changes = fetch_scalar_i64(conn, "SELECT changes()").await?;
    Ok(changes.max(0) as u64)
}

/// Execute a query that has already been rewritten to `?` placeholders and
/// collect all result rows.
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
    let cols: Vec<String> = stmt.columns().iter().map(|c| c.name().to_owned()).collect();
    let mut rows_iter = stmt.query(lp).await.map_err(SqliteCompatError::from)?;

    let mut rows: Vec<Row> = Vec::new();
    while let Some(limbo_row) = rows_iter.next().await.map_err(SqliteCompatError::from)? {
        let mut values: Vec<Value> = Vec::with_capacity(cols.len());
        for idx in 0..limbo_row.column_count() {
            let raw = limbo_row.get_value(idx).map_err(SqliteCompatError::from)?;
            values.push(limbo_to_core(raw)?);
        }
        rows.push(Row::new(cols.clone(), values));
    }
    Ok(rows)
}

/// Fetch a single `i64` from the first column of the first row of `sql`.
async fn fetch_scalar_i64(conn: &limbo::Connection, sql: &str) -> Result<i64, SqliteCompatError> {
    let rows = query_rewritten(conn, sql, vec![]).await?;
    if let Some(row) = rows.first() {
        match row.get_by_index(0) {
            Some(Value::I64(n)) => return Ok(*n),
            Some(Value::Null) => return Ok(0),
            Some(other) => {
                return Err(SqliteCompatError::TypeMap(format!(
                    "expected i64 from scalar query, got {other:?}"
                )))
            }
            None => {}
        }
    }
    Ok(0)
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
        // PRAGMA foreign_key_list is not yet implemented in limbo 0.0.22.
        // Fall back to sqlite_master for the CREATE TABLE DDL, then parse FK
        // constraints from that text — the same approach used by indexes().
        let sql = "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?";
        let rows = query_rewritten(&self.conn, sql, vec![limbo::Value::Text(table.into())])
            .await
            .map_err(OxiSqlError::from)?;

        let ddl = match rows.first() {
            Some(row) => match row.get_by_index(0) {
                Some(Value::Text(s)) if !s.is_empty() => s.clone(),
                _ => return Ok(vec![]),
            },
            None => return Ok(vec![]),
        };

        Ok(parse_foreign_keys(&ddl, table))
    }
}

// ── DDL parser: foreign keys ───────────────────────────────────────────────────

/// Strip one level of SQL identifier quoting from `s`.
///
/// Handles `"double"`, `` `backtick` ``, and `[bracket]` — returns the inner
/// text.  Unquoted identifiers are returned as-is.
fn strip_sql_quotes(s: &str) -> &str {
    let s = s.trim();
    let bytes = s.as_bytes();
    let len = bytes.len();
    if len >= 2 {
        let (open, close): (u8, u8) = match bytes[0] {
            b'"' => (b'"', b'"'),
            b'`' => (b'`', b'`'),
            b'[' => (b'[', b']'),
            _ => return s,
        };
        if bytes[0] == open && bytes[len - 1] == close {
            return &s[1..len - 1];
        }
    }
    s
}

/// Split `text` on commas that are **not** inside nested parentheses.
///
/// `DECIMAL(10,2)` has a comma inside parens; this function skips it.
fn split_top_level_commas(text: &str) -> Vec<&str> {
    let mut depth: usize = 0;
    let mut parts: Vec<&str> = Vec::new();
    let mut start = 0usize;
    for (i, ch) in text.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(&text[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts
}

/// Find the byte range `[start, end)` of the content inside the **first** `(`
/// `)`  pair found in `s` starting from `offset`.
///
/// Returns `None` if no opening paren is found.
fn find_paren_content(s: &str, offset: usize) -> Option<(usize, usize)> {
    let slice = &s[offset..];
    let rel_open = slice.find('(')?;
    let abs_open = offset + rel_open;
    // Walk forward to find the matching close paren (depth-aware).
    let mut depth: usize = 0;
    for (i, ch) in s[abs_open..].char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some((abs_open + 1, abs_open + i));
                }
            }
            _ => {}
        }
    }
    None
}

/// Parse `REFERENCES target_table [(col1 [, col2 ...])]` from `upper` (the
/// upper-cased DDL) / `original` (original-case DDL) starting at `pos`.
///
/// Returns `(foreign_table, Vec<foreign_col>)` on success.
fn parse_references_clause(
    upper: &str,
    original: &str,
    pos: usize,
) -> Option<(String, Vec<String>)> {
    let rest_upper = upper[pos..].trim_start();
    if !rest_upper.starts_with("REFERENCES") {
        return None;
    }
    let consumed_ws = upper[pos..].len() - upper[pos..].trim_start().len();
    let after_ref = pos + consumed_ws + "REFERENCES".len();

    // Extract the target table name token (possibly quoted).
    let rest_orig = original[after_ref..].trim_start();
    let ws_skip = original[after_ref..].len() - original[after_ref..].trim_start().len();
    let table_start = after_ref + ws_skip;

    // Find where the identifier ends: whitespace, `(`, `,`, `)`.
    let table_end = rest_orig
        .find(|c: char| c.is_whitespace() || c == '(' || c == ',' || c == ')')
        .map(|p| table_start + p)
        .unwrap_or(original.len());

    let raw_table = strip_sql_quotes(&original[table_start..table_end]).to_owned();
    if raw_table.is_empty() {
        return None;
    }

    // Check for optional column list `(...)`.
    let mut cols: Vec<String> = Vec::new();
    let paren_search_start = table_end;
    let rest_after_table = upper[paren_search_start..].trim_start();
    if rest_after_table.starts_with('(') {
        let ws2 =
            upper[paren_search_start..].len() - upper[paren_search_start..].trim_start().len();
        let abs_open_search = paren_search_start + ws2;
        if let Some((inner_start, inner_end)) = find_paren_content(original, abs_open_search) {
            let inner = &original[inner_start..inner_end];
            for part in split_top_level_commas(inner) {
                let col = strip_sql_quotes(part.trim()).to_owned();
                if !col.is_empty() {
                    cols.push(col);
                }
            }
        }
    }

    Some((raw_table, cols))
}

/// Parse FK constraints out of a `CREATE TABLE` DDL string.
///
/// Handles both table-level `FOREIGN KEY (cols) REFERENCES ...` and column-level
/// `col_name type REFERENCES ...` forms.  Emits one [`ForeignKeyInfo`] entry per
/// column pair (composite FKs produce multiple entries sharing the same
/// `constraint_name`).
///
/// This is a pure-string function — no database access.
fn parse_foreign_keys(ddl: &str, table: &str) -> Vec<ForeignKeyInfo> {
    // Normalise line-endings then find the body between the first `(` and the
    // matching final `)`.
    let ddl = ddl.replace('\r', " ");

    // Locate the opening paren of the column-definition list.
    let body_range = match find_paren_content(&ddl, 0) {
        Some(r) => r,
        None => return vec![],
    };
    let body = &ddl[body_range.0..body_range.1];
    let body_upper = body.to_ascii_uppercase();

    let mut results: Vec<ForeignKeyInfo> = Vec::new();

    // ── Pass 1: table-level FOREIGN KEY constraints ────────────────────────────
    // We scan for `FOREIGN KEY` (optionally preceded by `CONSTRAINT name`).
    let mut search_pos = 0usize;
    while let Some(rel) = body_upper[search_pos..].find("FOREIGN KEY") {
        let fk_pos = search_pos + rel;

        // Look back for `CONSTRAINT <name>` immediately before this position.
        let constraint_name: Option<String> = {
            let before = body[..fk_pos].trim_end();
            let before_upper = before.to_ascii_uppercase();
            if let Some(c_rel) = before_upper.rfind("CONSTRAINT") {
                let after_constraint = before[c_rel + "CONSTRAINT".len()..].trim_start();
                let name_end = after_constraint
                    .find(|c: char| c.is_whitespace() || c == '(' || c == ',')
                    .unwrap_or(after_constraint.len());
                let raw = strip_sql_quotes(&after_constraint[..name_end]);
                if !raw.is_empty() {
                    Some(raw.to_owned())
                } else {
                    None
                }
            } else {
                None
            }
        };

        // After "FOREIGN KEY" there is a `(col_list)`.
        let after_fk = fk_pos + "FOREIGN KEY".len();
        let paren_start_search = {
            let ws = body[after_fk..].len() - body[after_fk..].trim_start().len();
            after_fk + ws
        };

        let (local_cols, refs_search_start) =
            if let Some((inner_s, inner_e)) = find_paren_content(body, paren_start_search) {
                let cols: Vec<String> = split_top_level_commas(&body[inner_s..inner_e])
                    .into_iter()
                    .map(|c| strip_sql_quotes(c.trim()).to_owned())
                    .filter(|c| !c.is_empty())
                    .collect();
                (cols, inner_e + 1)
            } else {
                search_pos = fk_pos + "FOREIGN KEY".len();
                continue;
            };

        // Now parse REFERENCES.
        let refs_pos = {
            let ws = body_upper[refs_search_start..].len()
                - body_upper[refs_search_start..].trim_start().len();
            refs_search_start + ws
        };

        // Use the full-body-relative strings.
        let (foreign_table, foreign_cols) =
            match parse_references_clause(&body_upper, body, refs_pos) {
                Some(v) => v,
                None => {
                    search_pos = fk_pos + "FOREIGN KEY".len();
                    continue;
                }
            };

        // Synthesise a single constraint name for all columns in this FK.
        // For composite FKs all entries share the same name, derived from the
        // first column in the local list so that it is deterministic.
        let first_col = local_cols.first().map(String::as_str).unwrap_or("col");
        let shared_cname = constraint_name
            .clone()
            .unwrap_or_else(|| format!("fk_{table}_{first_col}"));

        // Emit one entry per local column.
        for (idx, local_col) in local_cols.iter().enumerate() {
            let foreign_col = foreign_cols.get(idx).cloned().unwrap_or_default();
            results.push(ForeignKeyInfo {
                constraint_name: shared_cname.clone(),
                column: local_col.clone(),
                foreign_table: foreign_table.clone(),
                foreign_column: foreign_col,
            });
        }

        search_pos = fk_pos + "FOREIGN KEY".len();
    }

    // ── Pass 2: column-level inline REFERENCES ─────────────────────────────────
    // Split the body on top-level commas to get individual column definitions.
    // Skip any that look like table-level constraints (FOREIGN KEY, PRIMARY KEY,
    // UNIQUE, CHECK).
    for segment in split_top_level_commas(body) {
        let seg_trimmed = segment.trim();
        let seg_upper = seg_trimmed.to_ascii_uppercase();

        // Skip table-level constraint clauses we already handled above.
        if seg_upper.trim_start().starts_with("FOREIGN KEY")
            || seg_upper.trim_start().starts_with("CONSTRAINT")
            || seg_upper.trim_start().starts_with("PRIMARY KEY")
            || seg_upper.trim_start().starts_with("UNIQUE")
            || seg_upper.trim_start().starts_with("CHECK")
        {
            continue;
        }

        // Locate "REFERENCES" in this segment.
        let ref_rel = match seg_upper.find("REFERENCES") {
            Some(p) => p,
            None => continue,
        };

        // Extract column name: the first token of the segment.
        let col_name = {
            let first_token_end = seg_trimmed
                .find(|c: char| c.is_whitespace())
                .unwrap_or(seg_trimmed.len());
            strip_sql_quotes(&seg_trimmed[..first_token_end]).to_owned()
        };
        if col_name.is_empty() {
            continue;
        }

        // Parse the REFERENCES clause.
        let (foreign_table, foreign_cols) =
            match parse_references_clause(&seg_upper, seg_trimmed, ref_rel) {
                Some(v) => v,
                None => continue,
            };

        let foreign_col = foreign_cols.into_iter().next().unwrap_or_default();
        let cname = format!("fk_{table}_{col_name}");

        // Deduplicate: don't re-add if table-level FK already covers this column.
        let already = results.iter().any(|r| r.column == col_name);
        if !already {
            results.push(ForeignKeyInfo {
                constraint_name: cname,
                column: col_name,
                foreign_table,
                foreign_column: foreign_col,
            });
        }
    }

    results
}

// ── SqliteTransaction ─────────────────────────────────────────────────────────

/// A SQLite transaction backed by raw `BEGIN`/`COMMIT`/`ROLLBACK` statements.
///
/// Holds a guard on the connection-level transaction mutex so that no other
/// async task can start a concurrent `BEGIN` on the same `SqliteConnection`.
/// When dropped without an explicit `commit` or `rollback`, the transaction
/// attempts a best-effort `ROLLBACK` via a background task.
///
/// # ROLLBACK limitation
///
/// Limbo 0.0.22 does not support `ROLLBACK` (`bail_parse_error!("ROLLBACK not
/// supported yet")`).  Calling [`rollback()`][SqliteTransaction::rollback]
/// returns `Err(OxiSqlError::Other("ROLLBACK is not supported by the limbo
/// 0.0.22 engine …"))` rather than propagating a raw parse error.
///
/// The `Drop` impl still fires a fire-and-forget background task so that any
/// future limbo release that adds ROLLBACK support will clean up implicitly.
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
            //
            // NOTE: Limbo 0.0.22 does not implement ROLLBACK, so this will
            // produce a SqlExecutionFailure error.  That error is intentionally
            // swallowed here because `Drop` cannot return a `Result`.  The
            // warning below signals callers who inspect logs.
            let conn = self.conn.clone();
            tokio::spawn(async move {
                if let Err(e) = conn.execute("ROLLBACK", LimboParams::None).await {
                    log::warn!(
                        "SqliteTransaction drop: ROLLBACK failed (expected with limbo \
                         0.0.22 which does not implement ROLLBACK): {e}"
                    );
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
        // Limbo 0.0.22 rejects ROLLBACK at parse time with
        // `bail_parse_error!("ROLLBACK not supported yet")`.  Rather than
        // propagating a cryptic parse-error we return a clear diagnostic.
        // Behaviour is identical to what limbo would do (the transaction is
        // NOT rolled back either way), but the error message is honest.
        Err(OxiSqlError::Other(
            "ROLLBACK is not supported by the limbo 0.0.22 engine; \
             this transaction cannot be rolled back — upgrade to limbo 0.1+ \
             when available"
                .to_owned(),
        ))
    }
}

// ── SqlitePrepared ────────────────────────────────────────────────────────────

/// A pseudo-prepared statement that stores the SQL and re-prepares on each call.
///
/// Limbo 0.0.22 does not cache compiled bytecode between executions; this wrapper
/// satisfies the [`PreparedStatement`] trait contract at the OxiSQL API level.
/// The `execute()` path benefits from the connection-level statement cache so
/// repeated calls with the same SQL avoid re-parsing inside Limbo.
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

// ── Unit tests: parse_foreign_keys ────────────────────────────────────────────

#[cfg(test)]
mod fk_tests {
    use super::parse_foreign_keys;

    #[test]
    fn test_single_column_level_fk() {
        let ddl = "CREATE TABLE orders (\
            id INTEGER PRIMARY KEY,\
            customer_id INTEGER REFERENCES customers(id)\
        )";
        let fks = parse_foreign_keys(ddl, "orders");
        assert_eq!(fks.len(), 1, "expected 1 FK, got {fks:?}");
        assert_eq!(fks[0].column, "customer_id");
        assert_eq!(fks[0].foreign_table, "customers");
        assert_eq!(fks[0].foreign_column, "id");
    }

    #[test]
    fn test_table_level_fk() {
        let ddl = "CREATE TABLE orders (\
            id INTEGER PRIMARY KEY,\
            cust_id INTEGER,\
            FOREIGN KEY (cust_id) REFERENCES customers(id)\
        )";
        let fks = parse_foreign_keys(ddl, "orders");
        assert_eq!(fks.len(), 1, "expected 1 FK, got {fks:?}");
        assert_eq!(fks[0].column, "cust_id");
        assert_eq!(fks[0].foreign_table, "customers");
        assert_eq!(fks[0].foreign_column, "id");
    }

    #[test]
    fn test_composite_fk() {
        let ddl = "CREATE TABLE orders (\
            a INTEGER,\
            b INTEGER,\
            FOREIGN KEY (a, b) REFERENCES parent(x, y)\
        )";
        let fks = parse_foreign_keys(ddl, "orders");
        assert_eq!(
            fks.len(),
            2,
            "expected 2 entries for composite FK, got {fks:?}"
        );
        assert_eq!(fks[0].column, "a");
        assert_eq!(fks[0].foreign_column, "x");
        assert_eq!(fks[1].column, "b");
        assert_eq!(fks[1].foreign_column, "y");
        // Both share the same synthesised constraint name.
        assert_eq!(fks[0].constraint_name, fks[1].constraint_name);
    }

    #[test]
    fn test_multiple_fks() {
        let ddl = "CREATE TABLE items (\
            id INTEGER PRIMARY KEY,\
            category_id INTEGER REFERENCES categories(id),\
            supplier_id INTEGER REFERENCES suppliers(sid)\
        )";
        let fks = parse_foreign_keys(ddl, "items");
        assert_eq!(fks.len(), 2, "expected 2 FKs, got {fks:?}");
        let col_names: Vec<&str> = fks.iter().map(|f| f.column.as_str()).collect();
        assert!(col_names.contains(&"category_id"), "missing category_id FK");
        assert!(col_names.contains(&"supplier_id"), "missing supplier_id FK");
    }

    #[test]
    fn test_quoted_identifiers() {
        let ddl = r#"CREATE TABLE "orders" (
            "cust_id" INTEGER REFERENCES `customers`("id")
        )"#;
        let fks = parse_foreign_keys(ddl, "orders");
        assert_eq!(
            fks.len(),
            1,
            "expected 1 FK from quoted identifiers, got {fks:?}"
        );
        assert_eq!(fks[0].column, "cust_id");
        assert_eq!(fks[0].foreign_table, "customers");
        assert_eq!(fks[0].foreign_column, "id");
    }

    #[test]
    fn test_on_delete_cascade() {
        let ddl = "CREATE TABLE orders (\
            id INTEGER PRIMARY KEY,\
            customer_id INTEGER NOT NULL REFERENCES customers(id) ON DELETE CASCADE\
        )";
        let fks = parse_foreign_keys(ddl, "orders");
        assert_eq!(
            fks.len(),
            1,
            "ON DELETE CASCADE must not corrupt output; got {fks:?}"
        );
        assert_eq!(fks[0].column, "customer_id");
        assert_eq!(fks[0].foreign_table, "customers");
        assert_eq!(fks[0].foreign_column, "id");
    }

    #[test]
    fn test_constraint_name() {
        let ddl = "CREATE TABLE orders (\
            id INTEGER PRIMARY KEY,\
            cust_id INTEGER,\
            CONSTRAINT fk_orders_cust FOREIGN KEY (cust_id) REFERENCES customers(id)\
        )";
        let fks = parse_foreign_keys(ddl, "orders");
        assert_eq!(fks.len(), 1, "expected 1 FK, got {fks:?}");
        assert_eq!(
            fks[0].constraint_name, "fk_orders_cust",
            "constraint name should be preserved"
        );
    }

    #[test]
    fn test_implicit_pk_ref() {
        // REFERENCES parent with no column list → foreign_column should be "".
        let ddl = "CREATE TABLE orders (\
            id INTEGER PRIMARY KEY,\
            customer_id INTEGER REFERENCES customers\
        )";
        let fks = parse_foreign_keys(ddl, "orders");
        assert_eq!(
            fks.len(),
            1,
            "expected 1 FK for implicit PK ref, got {fks:?}"
        );
        assert_eq!(fks[0].foreign_table, "customers");
        assert_eq!(
            fks[0].foreign_column, "",
            "implicit PK ref should have empty foreign_column"
        );
    }

    #[test]
    fn test_decimal_type_no_false_fk() {
        // DECIMAL(10,2) must not be mistaken for a REFERENCES clause.
        let ddl = "CREATE TABLE products (\
            id INTEGER PRIMARY KEY,\
            price DECIMAL(10,2) NOT NULL\
        )";
        let fks = parse_foreign_keys(ddl, "products");
        assert!(
            fks.is_empty(),
            "DECIMAL(10,2) must not be mistaken for a FK, got {fks:?}"
        );
    }
}
