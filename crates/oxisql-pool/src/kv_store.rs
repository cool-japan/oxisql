//! SQL-backed key-value store using an `EmbeddedPool` or `OxidbPool`.
//!
//! Provides simple `get`/`set`/`delete`/`list_keys`/`contains_key` operations
//! backed by a single `kv_entries (k TEXT, v TEXT)` table.
//!
//! # SQL dialect notes (GlueSQL / embedded)
//!
//! - `CREATE TABLE IF NOT EXISTS` is **not** supported. `EmbeddedKvStore::init`
//!   tries `CREATE TABLE`, catches the "already exists" error, and ignores it.
//! - `INSERT OR REPLACE` / `ON CONFLICT` are **not** supported. Upsert is
//!   implemented as `DELETE … WHERE k = $1` followed by `INSERT … VALUES ($1, $2)`.
//! - Parameter placeholders use `$1`, `$2` style (handled by
//!   `oxisql_embedded`'s `bind_params` layer).
//!
//! # Example
//!
//! ```rust,no_run
//! # #[cfg(feature = "embedded")]
//! # {
//! use oxisql_pool::embedded::EmbeddedPool;
//! use oxisql_pool::kv_store::EmbeddedKvStore;
//!
//! # async fn example() {
//! let pool = EmbeddedPool::new();
//! let kv = EmbeddedKvStore::new(pool, None);
//! kv.init().await.unwrap();
//! kv.set("hello", "world").await.unwrap();
//! let val = kv.get("hello").await.unwrap();
//! assert_eq!(val, Some("world".to_string()));
//! # }
//! # }
//! ```

#[cfg(any(
    feature = "postgres",
    feature = "mysql",
    feature = "embedded",
    feature = "sqlite"
))]
use std::sync::Arc;

#[cfg(feature = "embedded")]
use oxisql_core::{ConnectionPool, ToSqlValue};

#[cfg(any(
    feature = "postgres",
    feature = "mysql",
    feature = "embedded",
    feature = "sqlite"
))]
use crate::PoolError;

#[cfg(feature = "embedded")]
use crate::embedded::EmbeddedPool;

// ── EmbeddedKvStore ───────────────────────────────────────────────────────────

/// A key-value store backed by an `EmbeddedPool`.
///
/// All keys and values are stored as `TEXT` in a single table (default name:
/// `kv_entries`).  Call `EmbeddedKvStore::init` once before first use to
/// ensure the table exists.
#[cfg(feature = "embedded")]
pub struct EmbeddedKvStore {
    pool: EmbeddedPool,
    table: String,
}

#[cfg(feature = "embedded")]
impl EmbeddedKvStore {
    /// Create a new `EmbeddedKvStore` backed by `pool`.
    ///
    /// `table_name` is the SQL table name used to persist key-value pairs.
    /// Defaults to `"kv_entries"` when `None` is passed.
    pub fn new(pool: EmbeddedPool, table_name: Option<&str>) -> Self {
        Self {
            pool,
            table: table_name.unwrap_or("kv_entries").to_owned(),
        }
    }

    /// Ensure the backing table exists. Safe to call multiple times.
    ///
    /// GlueSQL does not support `CREATE TABLE IF NOT EXISTS`, so this method
    /// attempts a `CREATE TABLE` and silently ignores "already exists" errors.
    ///
    /// # Errors
    ///
    /// Returns [`PoolError::Build`] for any error other than table-already-exists.
    pub async fn init(&self) -> Result<(), PoolError> {
        let sql = format!("CREATE TABLE {} (k TEXT, v TEXT)", self.table);
        let conn = <EmbeddedPool as ConnectionPool>::get(&self.pool)
            .await
            .map_err(|e| PoolError::Build(e.to_string()))?;
        match conn.execute(&sql, &[]).await {
            Ok(_) => Ok(()),
            Err(e) => {
                let msg = e.to_string().to_lowercase();
                if msg.contains("already exist") || msg.contains("table already") {
                    Ok(())
                } else {
                    Err(PoolError::Build(e.to_string()))
                }
            }
        }
    }

    /// Set `key` to `value` (upsert).
    ///
    /// Implemented as `DELETE … WHERE k = $1` + `INSERT … VALUES ($1, $2)` because
    /// GlueSQL `MemoryStorage` does not support `ON CONFLICT` / `INSERT OR REPLACE`.
    ///
    /// # Errors
    ///
    /// Returns [`PoolError::Build`] if either SQL statement fails.
    pub async fn set(&self, key: &str, value: &str) -> Result<(), PoolError> {
        let conn = <EmbeddedPool as ConnectionPool>::get(&self.pool)
            .await
            .map_err(|e| PoolError::Build(e.to_string()))?;

        // Step 1: remove any existing row.
        let del = format!("DELETE FROM {} WHERE k = $1", self.table);
        conn.execute(&del, &[&key as &dyn ToSqlValue])
            .await
            .map_err(|e| PoolError::Build(e.to_string()))?;

        // Step 2: insert the new pair.
        let ins = format!("INSERT INTO {} (k, v) VALUES ($1, $2)", self.table);
        conn.execute(&ins, &[&key as &dyn ToSqlValue, &value as &dyn ToSqlValue])
            .await
            .map_err(|e| PoolError::Build(e.to_string()))?;

        Ok(())
    }

    /// Retrieve the value for `key`, or `None` if the key is absent.
    ///
    /// # Errors
    ///
    /// Returns [`PoolError::Build`] if the underlying SQL query fails.
    pub async fn get(&self, key: &str) -> Result<Option<String>, PoolError> {
        let conn = <EmbeddedPool as ConnectionPool>::get(&self.pool)
            .await
            .map_err(|e| PoolError::Build(e.to_string()))?;

        let sql = format!("SELECT v FROM {} WHERE k = $1", self.table);
        let rows = conn
            .query(&sql, &[&key as &dyn ToSqlValue])
            .await
            .map_err(|e| PoolError::Build(e.to_string()))?;

        if rows.is_empty() {
            return Ok(None);
        }
        let value = rows[0]
            .try_get::<String>("v")
            .map_err(|e| PoolError::Build(e.to_string()))?;
        Ok(Some(value))
    }

    /// Delete `key`. Returns `true` if the key existed, `false` otherwise.
    ///
    /// # Errors
    ///
    /// Returns [`PoolError::Build`] if the SQL query fails.
    pub async fn delete(&self, key: &str) -> Result<bool, PoolError> {
        // Check existence first (DELETE does not expose affected-row count via EmbeddedPool).
        let existed = self.contains_key(key).await?;
        if !existed {
            return Ok(false);
        }

        let conn = <EmbeddedPool as ConnectionPool>::get(&self.pool)
            .await
            .map_err(|e| PoolError::Build(e.to_string()))?;

        let sql = format!("DELETE FROM {} WHERE k = $1", self.table);
        conn.execute(&sql, &[&key as &dyn ToSqlValue])
            .await
            .map_err(|e| PoolError::Build(e.to_string()))?;

        Ok(true)
    }

    /// Return all keys currently stored, in unspecified order.
    ///
    /// # Errors
    ///
    /// Returns [`PoolError::Build`] if the SQL query fails.
    pub async fn list_keys(&self) -> Result<Vec<String>, PoolError> {
        let conn = <EmbeddedPool as ConnectionPool>::get(&self.pool)
            .await
            .map_err(|e| PoolError::Build(e.to_string()))?;

        let sql = format!("SELECT k FROM {}", self.table);
        let rows = conn
            .query(&sql, &[])
            .await
            .map_err(|e| PoolError::Build(e.to_string()))?;

        rows.iter()
            .map(|row| {
                row.try_get::<String>("k")
                    .map_err(|e| PoolError::Build(e.to_string()))
            })
            .collect()
    }

    /// Return `true` if `key` is present in the store.
    ///
    /// # Errors
    ///
    /// Returns [`PoolError::Build`] if the SQL query fails.
    pub async fn contains_key(&self, key: &str) -> Result<bool, PoolError> {
        self.get(key).await.map(|v| v.is_some())
    }
}

// ── OxidbKvStore ─────────────────────────────────────────────────────────────

/// A key-value store backed by any [`crate::OxidbPool`] variant.
///
/// For the `Embedded` variant all operations are handled via the
/// `ConnectionPool` trait path.  Postgres and MySQL variants require a live
/// database and behave identically at the SQL level (DELETE + INSERT upsert,
/// `k TEXT / v TEXT` schema).
///
/// Wrap an [`crate::OxidbPool`] in an [`Arc`] and pass it to
/// [`OxidbKvStore::new`].  All clones share the same pool and backing table.
#[cfg(any(
    feature = "postgres",
    feature = "mysql",
    feature = "embedded",
    feature = "sqlite"
))]
pub struct OxidbKvStore {
    pool: Arc<crate::OxidbPool>,
    table: String,
}

#[cfg(any(
    feature = "postgres",
    feature = "mysql",
    feature = "embedded",
    feature = "sqlite"
))]
impl OxidbKvStore {
    /// Create a new `OxidbKvStore` backed by `pool`.
    ///
    /// `table_name` defaults to `"kv_entries"` when `None`.
    pub fn new(pool: Arc<crate::OxidbPool>, table_name: Option<&str>) -> Self {
        Self {
            pool,
            table: table_name.unwrap_or("kv_entries").to_owned(),
        }
    }

    /// Ensure the backing table exists.
    ///
    /// For the `Embedded` variant this delegates to `EmbeddedKvStore::init`
    /// logic (ignores "table already exists").  For Postgres / MySQL the
    /// standard `CREATE TABLE IF NOT EXISTS` syntax is used.
    ///
    /// # Errors
    ///
    /// Returns [`PoolError::Build`] if the DDL fails for a reason other than
    /// the table already existing.
    pub async fn init(&self) -> Result<(), PoolError> {
        match self.pool.as_ref() {
            #[cfg(feature = "embedded")]
            crate::OxidbPool::Embedded(p) => {
                let kv = EmbeddedKvStore::new(p.clone(), Some(&self.table));
                kv.init().await
            }
            #[cfg(feature = "postgres")]
            crate::OxidbPool::Postgres(p) => {
                let client = p.get().await?;
                client
                    .simple_query(&format!(
                        "CREATE TABLE IF NOT EXISTS {} (k TEXT, v TEXT)",
                        self.table
                    ))
                    .await
                    .map_err(|e| PoolError::Build(e.to_string()))?;
                Ok(())
            }
            #[cfg(feature = "mysql")]
            crate::OxidbPool::Mysql(p) => {
                use mysql_async::prelude::Queryable;
                let mut conn = p.get().await.map_err(|e| PoolError::Build(e.to_string()))?;
                conn.query_drop(format!(
                    "CREATE TABLE IF NOT EXISTS {} (k TEXT, v TEXT)",
                    self.table
                ))
                .await
                .map_err(|e| PoolError::Build(e.to_string()))?;
                Ok(())
            }
            #[cfg(feature = "sqlite")]
            crate::OxidbPool::Sqlite(p) => {
                use oxisql_core::Connection as _;
                let conn = p.get().await.map_err(crate::PoolError::Sqlite)?;
                conn.execute_batch(&format!(
                    "CREATE TABLE IF NOT EXISTS {} (k TEXT NOT NULL, v TEXT NOT NULL)",
                    self.table
                ))
                .await
                .map(|_| ())
                .map_err(|e| PoolError::Build(e.to_string()))
            }
            #[cfg(not(any(
                feature = "postgres",
                feature = "mysql",
                feature = "embedded",
                feature = "sqlite"
            )))]
            _ => Err(PoolError::NoBackend),
        }
    }

    /// Set `key` to `value` (upsert: DELETE + INSERT).
    ///
    /// # Errors
    ///
    /// Returns [`PoolError::Build`] if the pool is exhausted or either SQL
    /// statement fails.
    pub async fn set(&self, key: &str, value: &str) -> Result<(), PoolError> {
        match self.pool.as_ref() {
            #[cfg(feature = "embedded")]
            crate::OxidbPool::Embedded(p) => {
                let kv = EmbeddedKvStore::new(p.clone(), Some(&self.table));
                kv.set(key, value).await
            }
            #[cfg(feature = "postgres")]
            crate::OxidbPool::Postgres(p) => {
                let client = p.get().await?;
                client
                    .execute(&format!("DELETE FROM {} WHERE k = $1", self.table), &[&key])
                    .await
                    .map_err(|e| PoolError::Build(e.to_string()))?;
                client
                    .execute(
                        &format!("INSERT INTO {} (k, v) VALUES ($1, $2)", self.table),
                        &[&key, &value],
                    )
                    .await
                    .map_err(|e| PoolError::Build(e.to_string()))?;
                Ok(())
            }
            #[cfg(feature = "mysql")]
            crate::OxidbPool::Mysql(p) => {
                use mysql_async::prelude::Queryable;
                let mut conn = p.get().await.map_err(|e| PoolError::Build(e.to_string()))?;
                conn.exec_drop(format!("DELETE FROM {} WHERE k = ?", self.table), (&key,))
                    .await
                    .map_err(|e| PoolError::Build(e.to_string()))?;
                conn.exec_drop(
                    format!("INSERT INTO {} (k, v) VALUES (?, ?)", self.table),
                    (&key, &value),
                )
                .await
                .map_err(|e| PoolError::Build(e.to_string()))?;
                Ok(())
            }
            #[cfg(feature = "sqlite")]
            crate::OxidbPool::Sqlite(p) => {
                use oxisql_core::Connection as _;
                let conn = p.get().await.map_err(crate::PoolError::Sqlite)?;
                let table = &self.table;
                conn.execute(&format!("DELETE FROM {table} WHERE k = $1"), &[&key])
                    .await
                    .map_err(|e| PoolError::Build(e.to_string()))?;
                conn.execute(
                    &format!("INSERT INTO {table} (k, v) VALUES ($1, $2)"),
                    &[&key, &value],
                )
                .await
                .map_err(|e| PoolError::Build(e.to_string()))?;
                Ok(())
            }
            #[cfg(not(any(
                feature = "postgres",
                feature = "mysql",
                feature = "embedded",
                feature = "sqlite"
            )))]
            _ => Err(PoolError::NoBackend),
        }
    }

    /// Retrieve the value for `key`, or `None` if absent.
    ///
    /// # Errors
    ///
    /// Returns [`PoolError::Build`] if the SQL query fails.
    pub async fn get(&self, key: &str) -> Result<Option<String>, PoolError> {
        match self.pool.as_ref() {
            #[cfg(feature = "embedded")]
            crate::OxidbPool::Embedded(p) => {
                let kv = EmbeddedKvStore::new(p.clone(), Some(&self.table));
                kv.get(key).await
            }
            #[cfg(feature = "postgres")]
            crate::OxidbPool::Postgres(p) => {
                let client = p.get().await?;
                let rows = client
                    .query(
                        &format!("SELECT v FROM {} WHERE k = $1", self.table),
                        &[&key],
                    )
                    .await
                    .map_err(|e| PoolError::Build(e.to_string()))?;
                if rows.is_empty() {
                    return Ok(None);
                }
                let v: String = rows[0].get(0);
                Ok(Some(v))
            }
            #[cfg(feature = "mysql")]
            crate::OxidbPool::Mysql(p) => {
                use mysql_async::prelude::Queryable;
                let mut conn = p.get().await.map_err(|e| PoolError::Build(e.to_string()))?;
                let result: Option<String> = conn
                    .exec_first(format!("SELECT v FROM {} WHERE k = ?", self.table), (&key,))
                    .await
                    .map_err(|e| PoolError::Build(e.to_string()))?;
                Ok(result)
            }
            #[cfg(feature = "sqlite")]
            crate::OxidbPool::Sqlite(p) => {
                use oxisql_core::{Connection as _, Value};
                let conn = p.get().await.map_err(crate::PoolError::Sqlite)?;
                let table = &self.table;
                let rows = conn
                    .query(&format!("SELECT v FROM {table} WHERE k = $1"), &[&key])
                    .await
                    .map_err(|e| PoolError::Build(e.to_string()))?;
                if rows.is_empty() {
                    return Ok(None);
                }
                match rows[0].get_by_index(0) {
                    Some(Value::Text(s)) => Ok(Some(s.clone())),
                    _ => Ok(None),
                }
            }
            #[cfg(not(any(
                feature = "postgres",
                feature = "mysql",
                feature = "embedded",
                feature = "sqlite"
            )))]
            _ => Err(PoolError::NoBackend),
        }
    }

    /// Delete `key`. Returns `true` if the key existed, `false` otherwise.
    ///
    /// # Errors
    ///
    /// Returns [`PoolError::Build`] if the SQL query fails.
    pub async fn delete(&self, key: &str) -> Result<bool, PoolError> {
        match self.pool.as_ref() {
            #[cfg(feature = "embedded")]
            crate::OxidbPool::Embedded(p) => {
                let kv = EmbeddedKvStore::new(p.clone(), Some(&self.table));
                kv.delete(key).await
            }
            #[cfg(feature = "postgres")]
            crate::OxidbPool::Postgres(p) => {
                let client = p.get().await?;
                let affected = client
                    .execute(&format!("DELETE FROM {} WHERE k = $1", self.table), &[&key])
                    .await
                    .map_err(|e| PoolError::Build(e.to_string()))?;
                Ok(affected > 0)
            }
            #[cfg(feature = "mysql")]
            crate::OxidbPool::Mysql(p) => {
                use mysql_async::prelude::Queryable;
                // Check existence first since affected_rows is not directly accessible.
                let existed = self.get(key).await?.is_some();
                if !existed {
                    return Ok(false);
                }
                let mut conn = p.get().await.map_err(|e| PoolError::Build(e.to_string()))?;
                conn.exec_drop(format!("DELETE FROM {} WHERE k = ?", self.table), (&key,))
                    .await
                    .map_err(|e| PoolError::Build(e.to_string()))?;
                Ok(true)
            }
            #[cfg(feature = "sqlite")]
            crate::OxidbPool::Sqlite(p) => {
                use oxisql_core::Connection as _;
                let conn = p.get().await.map_err(crate::PoolError::Sqlite)?;
                let table = &self.table;
                let affected = conn
                    .execute(&format!("DELETE FROM {table} WHERE k = $1"), &[&key])
                    .await
                    .map_err(|e| PoolError::Build(e.to_string()))?;
                Ok(affected > 0)
            }
            #[cfg(not(any(
                feature = "postgres",
                feature = "mysql",
                feature = "embedded",
                feature = "sqlite"
            )))]
            _ => Err(PoolError::NoBackend),
        }
    }

    /// Return all keys currently stored, in unspecified order.
    ///
    /// # Errors
    ///
    /// Returns [`PoolError::Build`] if the SQL query fails.
    pub async fn list_keys(&self) -> Result<Vec<String>, PoolError> {
        match self.pool.as_ref() {
            #[cfg(feature = "embedded")]
            crate::OxidbPool::Embedded(p) => {
                let kv = EmbeddedKvStore::new(p.clone(), Some(&self.table));
                kv.list_keys().await
            }
            #[cfg(feature = "postgres")]
            crate::OxidbPool::Postgres(p) => {
                let client = p.get().await?;
                let rows = client
                    .query(&format!("SELECT k FROM {}", self.table), &[])
                    .await
                    .map_err(|e| PoolError::Build(e.to_string()))?;
                Ok(rows.iter().map(|r| r.get::<_, String>(0)).collect())
            }
            #[cfg(feature = "mysql")]
            crate::OxidbPool::Mysql(p) => {
                use mysql_async::prelude::Queryable;
                let mut conn = p.get().await.map_err(|e| PoolError::Build(e.to_string()))?;
                let keys: Vec<String> = conn
                    .exec(format!("SELECT k FROM {}", self.table), ())
                    .await
                    .map_err(|e| PoolError::Build(e.to_string()))?;
                Ok(keys)
            }
            #[cfg(feature = "sqlite")]
            crate::OxidbPool::Sqlite(p) => {
                use oxisql_core::{Connection as _, Value};
                let conn = p.get().await.map_err(crate::PoolError::Sqlite)?;
                let table = &self.table;
                let rows = conn
                    .query(&format!("SELECT k FROM {table}"), &[])
                    .await
                    .map_err(|e| PoolError::Build(e.to_string()))?;
                let keys = rows
                    .into_iter()
                    .filter_map(|r| {
                        r.get_by_index(0).and_then(|v| {
                            if let Value::Text(s) = v {
                                Some(s.clone())
                            } else {
                                None
                            }
                        })
                    })
                    .collect();
                Ok(keys)
            }
            #[cfg(not(any(
                feature = "postgres",
                feature = "mysql",
                feature = "embedded",
                feature = "sqlite"
            )))]
            _ => Err(PoolError::NoBackend),
        }
    }
}
