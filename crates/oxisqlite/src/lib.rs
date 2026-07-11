// UPSTREAM: vendored Limbo fork — allow upstream style
//! Top-level facade of the C-free **oxisqlite** engine, a Pure-Rust fork of
//! limbo 0.0.22 and entry point for the `oxisql-sqlite-compat` backend.
//!
//! Re-exports `Connection`, `Statement`, `params`/`params_from_iter`, and
//! `Value` as a thin, ergonomic wrapper; bytecode execution, storage, and
//! SQL processing all live in `oxisqlite-core`.
#![allow(
    rustdoc::bare_urls,
    rustdoc::invalid_html_tags,
    rustdoc::broken_intra_doc_links
)]
#![allow(
    clippy::collapsible_match,
    clippy::doc_overindented_list_items,
    clippy::from_over_into
)]

pub mod params;
pub mod value;

pub use value::Value;

pub use params::params_from_iter;

use crate::params::*;
use std::fmt::Debug;
use std::num::NonZero;
use std::sync::{Arc, Mutex};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("SQL conversion failure: `{0}`")]
    ToSqlConversionFailure(BoxError),
    #[error("Mutex lock error: {0}")]
    MutexError(String),
    #[error("SQL execution failure: `{0}`")]
    SqlExecutionFailure(String),
    /// The database schema changed after this statement was compiled (SQLITE_SCHEMA).
    /// Re-prepare the statement and retry.
    #[error("database schema has changed")]
    SchemaChanged,
}

impl Error {
    /// Returns `true` if this error signals that the database schema changed after
    /// the statement was compiled.  Callers should re-prepare the statement
    /// against the refreshed schema and retry.
    pub fn is_schema_changed(&self) -> bool {
        matches!(self, Error::SchemaChanged)
    }
}

impl From<limbo_core::LimboError> for Error {
    fn from(err: limbo_core::LimboError) -> Self {
        match err {
            limbo_core::LimboError::SchemaChanged => Error::SchemaChanged,
            other => Error::SqlExecutionFailure(other.to_string()),
        }
    }
}

pub(crate) type BoxError = Box<dyn std::error::Error + Send + Sync>;

pub type Result<T> = std::result::Result<T, Error>;
pub struct Builder {
    path: String,
}

impl Builder {
    pub fn new_local(path: &str) -> Self {
        Self {
            path: path.to_string(),
        }
    }

    #[allow(unused_variables, clippy::arc_with_non_send_sync)]
    pub async fn build(self) -> Result<Database> {
        match self.path.as_str() {
            ":memory:" => {
                let io: Arc<dyn limbo_core::IO> = Arc::new(limbo_core::MemoryIO::new());
                let db = limbo_core::Database::open_file(io, self.path.as_str(), false)?;
                Ok(Database { inner: db })
            }
            path => {
                let io: Arc<dyn limbo_core::IO> = Arc::new(limbo_core::PlatformIO::new()?);
                let db = limbo_core::Database::open_file(io, path, false)?;
                Ok(Database { inner: db })
            }
        }
    }
}

#[derive(Clone)]
pub struct Database {
    inner: Arc<limbo_core::Database>,
}

unsafe impl Send for Database {}
unsafe impl Sync for Database {}

impl Debug for Database {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Database").finish()
    }
}

impl Database {
    pub fn connect(&self) -> Result<Connection> {
        let conn = self.inner.connect()?;
        #[allow(clippy::arc_with_non_send_sync)]
        let connection = Connection {
            inner: Arc::new(Mutex::new(conn)),
        };
        Ok(connection)
    }
}

pub struct Connection {
    inner: Arc<Mutex<Arc<limbo_core::Connection>>>,
}

impl Clone for Connection {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

unsafe impl Send for Connection {}
unsafe impl Sync for Connection {}

impl Connection {
    pub async fn query(&self, sql: &str, params: impl IntoParams) -> Result<Rows> {
        let mut stmt = self.prepare(sql).await?;
        stmt.query(params).await
    }

    pub async fn execute(&self, sql: &str, params: impl IntoParams) -> Result<u64> {
        let mut stmt = self.prepare(sql).await?;
        stmt.execute(params).await
    }

    pub async fn prepare(&self, sql: &str) -> Result<Statement> {
        let conn = self
            .inner
            .lock()
            .map_err(|e| Error::MutexError(e.to_string()))?;

        let stmt = conn.prepare(sql)?;

        #[allow(clippy::arc_with_non_send_sync)]
        let statement = Statement {
            inner: Arc::new(Mutex::new(stmt)),
        };
        Ok(statement)
    }

    /// Return the number of rows changed by the most recent DML statement on
    /// this connection.  Mirrors `sqlite3_changes()` semantics: DDL statements
    /// and `BEGIN`/`COMMIT`/`ROLLBACK` return 0.
    pub fn changes(&self) -> Result<i64> {
        let conn = self
            .inner
            .lock()
            .map_err(|e| Error::MutexError(e.to_string()))?;
        Ok(conn.changes())
    }

    pub fn pragma_query<F>(&self, pragma_name: &str, mut f: F) -> Result<()>
    where
        F: FnMut(&Row) -> limbo_core::Result<()>,
    {
        let conn = self
            .inner
            .lock()
            .map_err(|e| Error::MutexError(e.to_string()))?;

        let rows: Vec<Row> = conn
            .pragma_query(pragma_name)
            .map_err(|e| Error::SqlExecutionFailure(e.to_string()))?
            .iter()
            .map(|row| row.iter().collect::<Row>())
            .collect();

        rows.iter().try_for_each(|row| {
            f(row).map_err(|e| {
                Error::SqlExecutionFailure(format!("Error executing user defined function: {}", e))
            })
        })?;
        Ok(())
    }
}

pub struct Statement {
    inner: Arc<Mutex<limbo_core::Statement>>,
}

impl Clone for Statement {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

unsafe impl Send for Statement {}
unsafe impl Sync for Statement {}

impl Statement {
    pub async fn query(&mut self, params: impl IntoParams) -> Result<Rows> {
        let params = params.into_params()?;
        match params {
            params::Params::None => (),
            params::Params::Positional(values) => {
                for (i, value) in values.into_iter().enumerate() {
                    let mut stmt = self
                        .inner
                        .lock()
                        .map_err(|e| Error::MutexError(e.to_string()))?;
                    if let Some(idx) = NonZero::new(i + 1) {
                        stmt.bind_at(idx, value.into());
                    }
                }
            }
            params::Params::Named(_items) => todo!(),
        }
        #[allow(clippy::arc_with_non_send_sync)]
        let rows = Rows {
            inner: Arc::clone(&self.inner),
        };
        Ok(rows)
    }

    pub async fn execute(&mut self, params: impl IntoParams) -> Result<u64> {
        {
            // Reset the statement before executing
            self.inner
                .lock()
                .map_err(|e| Error::MutexError(e.to_string()))?
                .reset();
        }
        let params = params.into_params()?;
        match params {
            params::Params::None => (),
            params::Params::Positional(values) => {
                for (i, value) in values.into_iter().enumerate() {
                    let mut stmt = self
                        .inner
                        .lock()
                        .map_err(|e| Error::MutexError(e.to_string()))?;
                    if let Some(idx) = NonZero::new(i + 1) {
                        stmt.bind_at(idx, value.into());
                    }
                }
            }
            params::Params::Named(_items) => todo!(),
        }
        loop {
            let mut stmt = self
                .inner
                .lock()
                .map_err(|e| Error::MutexError(e.to_string()))?;
            match stmt.step() {
                Ok(limbo_core::StepResult::Row) => {
                    // unexpected row during execution, error out.
                    return Ok(2);
                }
                Ok(limbo_core::StepResult::Done) => {
                    return Ok(0);
                }
                Ok(limbo_core::StepResult::IO) => {
                    let _ = stmt.run_once();
                    //return Ok(1);
                }
                Ok(limbo_core::StepResult::Busy) => {
                    return Ok(4);
                }
                Ok(limbo_core::StepResult::Interrupt) => {
                    return Ok(3);
                }
                Err(err) => {
                    return Err(err.into());
                }
            }
        }
    }

    pub fn columns(&self) -> Vec<Column> {
        let Ok(stmt) = self.inner.lock() else {
            return Vec::new();
        };

        let n = stmt.num_columns();

        let mut cols = Vec::with_capacity(n);

        for i in 0..n {
            let name = stmt.get_column_name(i).into_owned();
            let decl_type = stmt.get_column_decl_type(i).map(|s| s.into_owned());
            cols.push(Column { name, decl_type });
        }

        cols
    }
}

pub struct Column {
    name: String,
    decl_type: Option<String>,
}

impl Column {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn decl_type(&self) -> Option<&str> {
        self.decl_type.as_deref()
    }
}

pub trait IntoValue {
    fn into_value(self) -> Result<Value>;
}

#[derive(Debug, Clone)]
pub enum Params {
    None,
    Positional(Vec<Value>),
    Named(Vec<(String, Value)>),
}
pub struct Transaction {}

pub struct Rows {
    inner: Arc<Mutex<limbo_core::Statement>>,
}

impl Clone for Rows {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

unsafe impl Send for Rows {}
unsafe impl Sync for Rows {}

impl Rows {
    pub async fn next(&mut self) -> Result<Option<Row>> {
        loop {
            let mut stmt = self
                .inner
                .lock()
                .map_err(|e| Error::MutexError(e.to_string()))?;
            match stmt.step() {
                Ok(limbo_core::StepResult::Row) => {
                    let row = stmt.row().ok_or_else(|| {
                        Error::SqlExecutionFailure(
                            "row unavailable after Row step result".to_string(),
                        )
                    })?;
                    return Ok(Some(Row {
                        values: row.get_values().map(|v| v.to_owned()).collect(),
                    }));
                }
                Ok(limbo_core::StepResult::Done) => return Ok(None),
                Ok(limbo_core::StepResult::IO) => {
                    if let Err(e) = stmt.run_once() {
                        return Err(e.into());
                    }
                    continue;
                }
                Ok(limbo_core::StepResult::Busy) => return Ok(None),
                Ok(limbo_core::StepResult::Interrupt) => return Ok(None),
                _ => return Ok(None),
            }
        }
    }
}

#[derive(Debug)]
pub struct Row {
    values: Vec<limbo_core::Value>,
}

unsafe impl Send for Row {}
unsafe impl Sync for Row {}

impl Row {
    pub fn get_value(&self, index: usize) -> Result<Value> {
        let value = &self.values[index];
        match value {
            limbo_core::Value::Integer(i) => Ok(Value::Integer(*i)),
            limbo_core::Value::Null => Ok(Value::Null),
            limbo_core::Value::Float(f) => Ok(Value::Real(*f)),
            limbo_core::Value::Text(text) => Ok(Value::Text(text.to_string())),
            limbo_core::Value::Blob(items) => Ok(Value::Blob(items.to_vec())),
        }
    }

    pub fn column_count(&self) -> usize {
        self.values.len()
    }
}

impl<'a> FromIterator<&'a limbo_core::Value> for Row {
    fn from_iter<T: IntoIterator<Item = &'a limbo_core::Value>>(iter: T) -> Self {
        let values = iter
            .into_iter()
            .map(|v| match v {
                limbo_core::Value::Integer(i) => limbo_core::Value::Integer(*i),
                limbo_core::Value::Null => limbo_core::Value::Null,
                limbo_core::Value::Float(f) => limbo_core::Value::Float(*f),
                limbo_core::Value::Text(s) => limbo_core::Value::Text(s.clone()),
                limbo_core::Value::Blob(b) => limbo_core::Value::Blob(b.clone()),
            })
            .collect();

        Row { values }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[tokio::test]
    async fn test_database_persistence() -> Result<()> {
        let temp_file = NamedTempFile::new().unwrap();
        let db_path = temp_file.path().to_str().unwrap();

        // First, create the database, a table, and insert some data
        {
            let db = Builder::new_local(db_path).build().await?;
            let conn = db.connect()?;
            conn.execute(
                "CREATE TABLE test_persistence (id INTEGER PRIMARY KEY, name TEXT NOT NULL);",
                (),
            )
            .await?;
            conn.execute("INSERT INTO test_persistence (name) VALUES ('Alice');", ())
                .await?;
            conn.execute("INSERT INTO test_persistence (name) VALUES ('Bob');", ())
                .await?;
        } // db and conn are dropped here, simulating closing

        // Now, re-open the database and check if the data is still there
        let db = Builder::new_local(db_path).build().await?;
        let conn = db.connect()?;

        let mut rows = conn
            .query("SELECT name FROM test_persistence ORDER BY id;", ())
            .await?;

        let row1 = rows.next().await?.expect("Expected first row");
        assert_eq!(row1.get_value(0)?, Value::Text("Alice".to_string()));

        let row2 = rows.next().await?.expect("Expected second row");
        assert_eq!(row2.get_value(0)?, Value::Text("Bob".to_string()));

        assert!(rows.next().await?.is_none(), "Expected no more rows");

        Ok(())
    }

    #[tokio::test]
    async fn test_database_persistence_many_frames() -> Result<()> {
        let temp_file = NamedTempFile::new().unwrap();
        let db_path = temp_file.path().to_str().unwrap();

        const NUM_INSERTS: usize = 100;
        const TARGET_STRING_LEN: usize = 1024; // 1KB

        let mut original_data = Vec::with_capacity(NUM_INSERTS);
        for i in 0..NUM_INSERTS {
            let prefix = format!("test_string_{:04}_", i);
            let padding_len = TARGET_STRING_LEN.saturating_sub(prefix.len());
            let padding: String = "A".repeat(padding_len);
            original_data.push(format!("{}{}", prefix, padding));
        }

        // First, create the database, a table, and insert many large strings
        {
            let db = Builder::new_local(db_path).build().await?;
            let conn = db.connect()?;
            conn.execute(
                "CREATE TABLE test_large_persistence (id INTEGER PRIMARY KEY AUTOINCREMENT, data TEXT NOT NULL);",
                (),
            )
            .await?;

            for data_val in &original_data {
                conn.execute(
                    "INSERT INTO test_large_persistence (data) VALUES (?);",
                    params::Params::Positional(vec![Value::Text(data_val.clone())]),
                )
                .await?;
            }
        } // db and conn are dropped here, simulating closing

        // Now, re-open the database and check if the data is still there
        let db = Builder::new_local(db_path).build().await?;
        let conn = db.connect()?;

        let mut rows = conn
            .query("SELECT data FROM test_large_persistence ORDER BY id;", ())
            .await?;

        for (i, expected) in original_data.iter().enumerate().take(NUM_INSERTS) {
            let row = rows
                .next()
                .await?
                .unwrap_or_else(|| panic!("Expected row {} but found None", i));
            assert_eq!(
                row.get_value(0)?,
                Value::Text(expected.clone()),
                "Mismatch in retrieved data for row {}",
                i
            );
        }

        assert!(
            rows.next().await?.is_none(),
            "Expected no more rows after retrieving all inserted data"
        );

        // Delete the WAL file only and try to re-open and query
        let wal_path = format!("{}-wal", db_path);
        std::fs::remove_file(&wal_path)
            .map_err(|e| eprintln!("Warning: Failed to delete WAL file for test: {}", e))
            .unwrap();

        // Re-open the database after deleting the WAL and assert the data is still
        // fully intact. The clean close above (dropping the connection) triggers a
        // checkpoint-on-close, which writes all WAL frames into the main `.db` file
        // and truncates the WAL. As a result the `-wal` file is no longer
        // load-bearing after a clean close: deleting it must NOT lose any data.
        let db_after_wal_delete = Builder::new_local(db_path).build().await?;
        let conn_after_wal_delete = db_after_wal_delete.connect()?;

        let mut rows_after_wal_delete = conn_after_wal_delete
            .query("SELECT data FROM test_large_persistence ORDER BY id;", ())
            .await?;

        for (i, expected) in original_data.iter().enumerate().take(NUM_INSERTS) {
            let row = rows_after_wal_delete.next().await?.unwrap_or_else(|| {
                panic!(
                    "Expected row {} after WAL deletion but found None; \
                         checkpoint-on-close should have persisted it into the main DB",
                    i
                )
            });
            assert_eq!(
                row.get_value(0)?,
                Value::Text(expected.clone()),
                "Mismatch in retrieved data for row {} after WAL deletion",
                i
            );
        }

        assert!(
            rows_after_wal_delete.next().await?.is_none(),
            "Expected no more rows after WAL deletion once all checkpointed data was retrieved"
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_database_persistence_write_one_frame_many_times() -> Result<()> {
        let temp_file = NamedTempFile::new().unwrap();
        let db_path = temp_file.path().to_str().unwrap();

        for i in 0..100 {
            {
                let db = Builder::new_local(db_path).build().await?;
                let conn = db.connect()?;

                conn.execute("CREATE TABLE IF NOT EXISTS test_persistence (id INTEGER PRIMARY KEY, name TEXT NOT NULL);", ()).await?;
                conn.execute("INSERT INTO test_persistence (name) VALUES ('Alice');", ())
                    .await?;
            }
            {
                let db = Builder::new_local(db_path).build().await?;
                let conn = db.connect()?;

                let mut rows_iter = conn
                    .query("SELECT count(*) FROM test_persistence;", ())
                    .await?;
                let rows = rows_iter.next().await?.unwrap();
                assert_eq!(rows.get_value(0)?, Value::Integer(i as i64 + 1));
                assert!(rows_iter.next().await?.is_none());
            }
        }

        Ok(())
    }

    // ------------------------------------------------------------------
    // A1: PRAGMA application_id
    // ------------------------------------------------------------------

    /// Read a single scalar integer value produced by a query (e.g. a PRAGMA).
    async fn query_scalar_i64(conn: &Connection, sql: &str) -> Result<i64> {
        let mut rows = conn.query(sql, ()).await?;
        let row = rows
            .next()
            .await?
            .unwrap_or_else(|| panic!("expected a row from `{sql}`"));
        match row.get_value(0)? {
            Value::Integer(i) => Ok(i),
            other => panic!("expected Integer from `{sql}`, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_application_id_write_read_round_trip() -> Result<()> {
        let db = Builder::new_local(":memory:").build().await?;
        let conn = db.connect()?;

        // Default is 0.
        assert_eq!(query_scalar_i64(&conn, "PRAGMA application_id;").await?, 0);

        // GPKG magic (0x47504B47 = 1196444487), a large positive identifier.
        conn.execute("PRAGMA application_id = 1196444487;", ())
            .await?;
        assert_eq!(
            query_scalar_i64(&conn, "PRAGMA application_id;").await?,
            1196444487
        );

        // Overwrite with another value.
        conn.execute("PRAGMA application_id = 42;", ()).await?;
        assert_eq!(query_scalar_i64(&conn, "PRAGMA application_id;").await?, 42);

        Ok(())
    }

    #[tokio::test]
    async fn test_application_id_negative_round_trip() -> Result<()> {
        // SQLite presents application_id as a SIGNED 32-bit integer, so -1 must
        // round-trip as -1 (not 4294967295).
        let db = Builder::new_local(":memory:").build().await?;
        let conn = db.connect()?;

        conn.execute("PRAGMA application_id = -1;", ()).await?;
        assert_eq!(query_scalar_i64(&conn, "PRAGMA application_id;").await?, -1);

        conn.execute("PRAGMA application_id = -2147483648;", ())
            .await?;
        assert_eq!(
            query_scalar_i64(&conn, "PRAGMA application_id;").await?,
            -2147483648
        );

        Ok(())
    }

    /// Build a unique, file-backed database path under the OS temp directory.
    ///
    /// Uses [`std::env::temp_dir`] plus the process id and an atomically
    /// incrementing counter so concurrently-running tests never collide, and
    /// cleans up the database file together with its `-wal` sidecar on drop.
    struct TempDbPath {
        path: std::path::PathBuf,
    }

    impl TempDbPath {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let mut path = std::env::temp_dir();
            path.push(format!(
                "oxisqlite_dur_{}_{}_{}.db",
                tag,
                std::process::id(),
                n
            ));
            // Ensure a clean slate even if a previous run left files behind.
            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_file(format!("{}-wal", path.display()));
            Self { path }
        }

        fn as_str(&self) -> &str {
            self.path
                .to_str()
                .expect("temp db path is valid UTF-8 on the test platforms")
        }
    }

    impl Drop for TempDbPath {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
            let _ = std::fs::remove_file(format!("{}-wal", self.path.display()));
        }
    }

    /// `application_id` survives a real close/reopen cycle for a file-backed
    /// database (regression test for the header-cookie durability bug: the
    /// in-memory header was previously re-read straight from the main DB file
    /// at open time, bypassing the WAL, so a cookie that lived only in the WAL
    /// reset to 0 on reopen).
    #[tokio::test]
    async fn test_application_id_persistence() -> Result<()> {
        let temp = TempDbPath::new("app_id_persist");
        let db_path = temp.as_str();

        {
            let db = Builder::new_local(db_path).build().await?;
            let conn = db.connect()?;
            conn.execute("CREATE TABLE t (id INTEGER PRIMARY KEY);", ())
                .await?;
            conn.execute("PRAGMA application_id = -12345;", ()).await?;

            // Within the same open database the value (and its sign) is retained.
            assert_eq!(
                query_scalar_i64(&conn, "PRAGMA application_id;").await?,
                -12345
            );
        } // connection + database dropped here, simulating a close.

        // Reopen and assert the value is durably restored from the WAL.
        let db = Builder::new_local(db_path).build().await?;
        let conn = db.connect()?;
        assert_eq!(
            query_scalar_i64(&conn, "PRAGMA application_id;").await?,
            -12345,
            "application_id must survive close/reopen"
        );

        Ok(())
    }

    /// `application_id` set to a large positive identifier round-trips across a
    /// close/reopen for a file-backed database.
    #[tokio::test]
    async fn test_application_id_durable_reopen() -> Result<()> {
        let temp = TempDbPath::new("app_id_reopen");
        let db_path = temp.as_str();

        {
            let db = Builder::new_local(db_path).build().await?;
            let conn = db.connect()?;
            conn.execute("PRAGMA application_id = 12345;", ()).await?;
            assert_eq!(
                query_scalar_i64(&conn, "PRAGMA application_id;").await?,
                12345
            );
        }

        let db = Builder::new_local(db_path).build().await?;
        let conn = db.connect()?;
        assert_eq!(
            query_scalar_i64(&conn, "PRAGMA application_id;").await?,
            12345,
            "application_id = 12345 must survive close/reopen"
        );

        Ok(())
    }

    /// `user_version` (the canonical cookie mirror of `application_id`) survives
    /// a close/reopen identically.
    #[tokio::test]
    async fn test_user_version_durable_reopen() -> Result<()> {
        let temp = TempDbPath::new("user_version_reopen");
        let db_path = temp.as_str();

        {
            let db = Builder::new_local(db_path).build().await?;
            let conn = db.connect()?;
            conn.execute("PRAGMA user_version = 12345;", ()).await?;
            assert_eq!(
                query_scalar_i64(&conn, "PRAGMA user_version;").await?,
                12345
            );
        }

        let db = Builder::new_local(db_path).build().await?;
        let conn = db.connect()?;
        assert_eq!(
            query_scalar_i64(&conn, "PRAGMA user_version;").await?,
            12345,
            "user_version = 12345 must survive close/reopen"
        );

        Ok(())
    }

    /// A negative `application_id` (e.g. -1) is stored on disk as 0xFFFFFFFF but
    /// must read back as the signed value -1 after a durable close/reopen, just
    /// like SQLite.
    #[tokio::test]
    async fn test_application_id_negative_durable_reopen() -> Result<()> {
        let temp = TempDbPath::new("app_id_negative_reopen");
        let db_path = temp.as_str();

        {
            let db = Builder::new_local(db_path).build().await?;
            let conn = db.connect()?;
            conn.execute("PRAGMA application_id = -1;", ()).await?;
            assert_eq!(query_scalar_i64(&conn, "PRAGMA application_id;").await?, -1);
        }

        let db = Builder::new_local(db_path).build().await?;
        let conn = db.connect()?;
        assert_eq!(
            query_scalar_i64(&conn, "PRAGMA application_id;").await?,
            -1,
            "application_id = -1 must survive close/reopen as the signed value -1"
        );

        // And the on-disk bytes (after a checkpoint flushes the WAL into the
        // main file) must be the 32-bit two's-complement big-endian 0xFFFFFFFF.
        let conn = db.connect()?;
        let _ = conn.execute("PRAGMA wal_checkpoint;", ()).await;
        drop(conn);
        drop(db);
        let bytes = std::fs::read(db_path).expect("read database file");
        assert!(bytes.len() >= 72, "database file shorter than the header");
        assert_eq!(
            &bytes[68..72],
            &0xFFFF_FFFFu32.to_be_bytes(),
            "application_id = -1 must be encoded as 0xFFFFFFFF at offset 68"
        );

        Ok(())
    }

    /// Byte-level GeoPackage check: writing the GPKG magic via
    /// `PRAGMA application_id = 1196444487` (0x47504B47) and checkpointing must
    /// land the big-endian magic at file offset 68, and a `user_version` write
    /// must land at offset 60 — the exact layout GeoPackage requires.
    #[tokio::test]
    async fn test_application_id_byte_level_on_disk() -> Result<()> {
        const GPKG_MAGIC: u32 = 1196444487; // 0x47504B47, "GPKG".
        const USER_VERSION: i32 = 10201; // arbitrary GeoPackage-style version.

        let temp = TempDbPath::new("app_id_bytes");
        let db_path = temp.as_str();

        {
            let db = Builder::new_local(db_path).build().await?;
            let conn = db.connect()?;
            // A table forces real page allocation so the file is a valid db.
            conn.execute("CREATE TABLE gpkg_contents (id INTEGER PRIMARY KEY);", ())
                .await?;
            conn.execute(&format!("PRAGMA application_id = {GPKG_MAGIC};"), ())
                .await?;
            conn.execute(&format!("PRAGMA user_version = {USER_VERSION};"), ())
                .await?;
            // Checkpoint so the WAL's page-1 frame is copied into the main
            // database file: in WAL mode the header bytes only reach the main
            // file after a checkpoint (this is the same requirement SQLite has
            // for a byte-valid GeoPackage on disk).
            let _ = conn.execute("PRAGMA wal_checkpoint;", ()).await;
        }

        let bytes = std::fs::read(db_path).expect("read database file");
        assert!(
            bytes.len() >= 72,
            "database file is shorter than the 100-byte header"
        );

        // application_id at offset [68..72], big-endian == 0x47504B47.
        assert_eq!(
            &bytes[68..72],
            &GPKG_MAGIC.to_be_bytes(),
            "GPKG magic must be stored big-endian at file offset 68"
        );
        assert_eq!(
            u32::from_be_bytes([bytes[68], bytes[69], bytes[70], bytes[71]]),
            0x4750_4B47,
            "application_id bytes must decode to 0x47504B47"
        );

        // user_version at offset [60..64], big-endian.
        assert_eq!(
            &bytes[60..64],
            &USER_VERSION.to_be_bytes(),
            "user_version must be stored big-endian at file offset 60"
        );

        // The value is also readable through PRAGMA after reopen.
        let db = Builder::new_local(db_path).build().await?;
        let conn = db.connect()?;
        assert_eq!(
            query_scalar_i64(&conn, "PRAGMA application_id;").await?,
            GPKG_MAGIC as i64
        );
        assert_eq!(
            query_scalar_i64(&conn, "PRAGMA user_version;").await?,
            USER_VERSION as i64
        );

        Ok(())
    }

    // ------------------------------------------------------------------
    // A2: INSERT OR IGNORE
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn test_insert_or_ignore_rowid_conflict_skipped() -> Result<()> {
        let db = Builder::new_local(":memory:").build().await?;
        let conn = db.connect()?;
        conn.execute(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT NOT NULL);",
            (),
        )
        .await?;
        conn.execute("INSERT INTO t (id, name) VALUES (1, 'Alice');", ())
            .await?;

        // Conflicting rowid is silently ignored, not an error.
        conn.execute("INSERT OR IGNORE INTO t (id, name) VALUES (1, 'Bob');", ())
            .await?;

        // Original row is untouched.
        assert_eq!(query_scalar_i64(&conn, "SELECT count(*) FROM t;").await?, 1);
        let mut rows = conn.query("SELECT name FROM t WHERE id = 1;", ()).await?;
        let row = rows.next().await?.expect("row");
        assert_eq!(row.get_value(0)?, Value::Text("Alice".to_string()));

        Ok(())
    }

    #[tokio::test]
    async fn test_insert_or_ignore_multi_row_other_rows_land() -> Result<()> {
        let db = Builder::new_local(":memory:").build().await?;
        let conn = db.connect()?;
        conn.execute(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT NOT NULL);",
            (),
        )
        .await?;
        conn.execute("INSERT INTO t (id, name) VALUES (2, 'Two');", ())
            .await?;

        // Multi-row INSERT OR IGNORE: row id=2 conflicts and is skipped, but ids
        // 1 and 3 must still land.
        conn.execute(
            "INSERT OR IGNORE INTO t (id, name) VALUES (1, 'One'), (2, 'Dup'), (3, 'Three');",
            (),
        )
        .await?;

        assert_eq!(query_scalar_i64(&conn, "SELECT count(*) FROM t;").await?, 3);
        // The conflicting row keeps its original value.
        let mut rows = conn.query("SELECT name FROM t WHERE id = 2;", ()).await?;
        assert_eq!(
            rows.next().await?.expect("row").get_value(0)?,
            Value::Text("Two".to_string())
        );
        // The non-conflicting rows are present.
        let mut rows = conn.query("SELECT id FROM t ORDER BY id;", ()).await?;
        assert_eq!(
            rows.next().await?.expect("row").get_value(0)?,
            Value::Integer(1)
        );
        assert_eq!(
            rows.next().await?.expect("row").get_value(0)?,
            Value::Integer(2)
        );
        assert_eq!(
            rows.next().await?.expect("row").get_value(0)?,
            Value::Integer(3)
        );

        Ok(())
    }

    #[cfg(feature = "index_experimental")]
    #[tokio::test]
    async fn test_insert_or_ignore_unique_index_conflict_skipped() -> Result<()> {
        let db = Builder::new_local(":memory:").build().await?;
        let conn = db.connect()?;
        conn.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, email TEXT);", ())
            .await?;
        conn.execute("CREATE UNIQUE INDEX idx_email ON t (email);", ())
            .await?;
        conn.execute("INSERT INTO t (id, email) VALUES (1, 'a@example.com');", ())
            .await?;

        // Different rowid but conflicting unique-index value -> skipped, no error
        // and crucially no partial index/table state.
        conn.execute(
            "INSERT OR IGNORE INTO t (id, email) VALUES (2, 'a@example.com');",
            (),
        )
        .await?;

        assert_eq!(query_scalar_i64(&conn, "SELECT count(*) FROM t;").await?, 1);
        // Row id=2 must NOT exist.
        let mut rows = conn.query("SELECT id FROM t ORDER BY id;", ()).await?;
        assert_eq!(
            rows.next().await?.expect("row").get_value(0)?,
            Value::Integer(1)
        );
        assert!(rows.next().await?.is_none());

        Ok(())
    }

    // ------------------------------------------------------------------
    // A3: INSERT OR REPLACE
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn test_insert_or_replace_rowid_conflict_replaces() -> Result<()> {
        let db = Builder::new_local(":memory:").build().await?;
        let conn = db.connect()?;
        conn.execute(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT NOT NULL);",
            (),
        )
        .await?;
        conn.execute("INSERT INTO t (id, name) VALUES (1, 'Alice');", ())
            .await?;

        // Same rowid -> old row replaced by new one.
        conn.execute("INSERT OR REPLACE INTO t (id, name) VALUES (1, 'Bob');", ())
            .await?;

        assert_eq!(query_scalar_i64(&conn, "SELECT count(*) FROM t;").await?, 1);
        let mut rows = conn.query("SELECT name FROM t WHERE id = 1;", ()).await?;
        assert_eq!(
            rows.next().await?.expect("row").get_value(0)?,
            Value::Text("Bob".to_string())
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_insert_or_replace_multi_row_conflict_with_prior_row() -> Result<()> {
        let db = Builder::new_local(":memory:").build().await?;
        let conn = db.connect()?;
        conn.execute(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT NOT NULL);",
            (),
        )
        .await?;

        // Row N (id=1, 'Second') conflicts with just-inserted row N-1 (id=1,
        // 'First') within the same multi-row statement -> the later one wins.
        conn.execute(
            "INSERT OR REPLACE INTO t (id, name) VALUES (1, 'First'), (1, 'Second'), (2, 'Other');",
            (),
        )
        .await?;

        assert_eq!(query_scalar_i64(&conn, "SELECT count(*) FROM t;").await?, 2);
        let mut rows = conn.query("SELECT name FROM t WHERE id = 1;", ()).await?;
        assert_eq!(
            rows.next().await?.expect("row").get_value(0)?,
            Value::Text("Second".to_string())
        );

        Ok(())
    }

    #[cfg(feature = "index_experimental")]
    #[tokio::test]
    async fn test_insert_or_replace_single_unique_index_conflict() -> Result<()> {
        let db = Builder::new_local(":memory:").build().await?;
        let conn = db.connect()?;
        conn.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, email TEXT);", ())
            .await?;
        conn.execute("CREATE UNIQUE INDEX idx_email ON t (email);", ())
            .await?;
        conn.execute("INSERT INTO t (id, email) VALUES (1, 'a@example.com');", ())
            .await?;

        // New rowid (2) but conflicting unique-index value -> the victim (id=1)
        // is deleted and replaced by the new row (id=2).
        conn.execute(
            "INSERT OR REPLACE INTO t (id, email) VALUES (2, 'a@example.com');",
            (),
        )
        .await?;

        assert_eq!(query_scalar_i64(&conn, "SELECT count(*) FROM t;").await?, 1);
        // Only id=2 remains, and the unique index still resolves it.
        let mut rows = conn
            .query("SELECT id FROM t WHERE email = 'a@example.com';", ())
            .await?;
        assert_eq!(
            rows.next().await?.expect("row").get_value(0)?,
            Value::Integer(2)
        );
        assert!(rows.next().await?.is_none());

        Ok(())
    }

    #[cfg(feature = "index_experimental")]
    #[tokio::test]
    async fn test_insert_or_replace_multiple_unique_indexes_different_victims() -> Result<()> {
        // SQLite OR REPLACE semantics: a new row that conflicts on MULTIPLE
        // unique indexes pointing at DIFFERENT existing rows must delete EVERY
        // victim, leaving exactly the new row.
        let db = Builder::new_local(":memory:").build().await?;
        let conn = db.connect()?;
        conn.execute(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, a TEXT, b TEXT);",
            (),
        )
        .await?;
        conn.execute("CREATE UNIQUE INDEX idx_a ON t (a);", ())
            .await?;
        conn.execute("CREATE UNIQUE INDEX idx_b ON t (b);", ())
            .await?;

        // Two distinct existing rows; the new row collides with row 1 on column a
        // and with row 2 on column b.
        conn.execute("INSERT INTO t (id, a, b) VALUES (1, 'A1', 'B1');", ())
            .await?;
        conn.execute("INSERT INTO t (id, a, b) VALUES (2, 'A2', 'B2');", ())
            .await?;

        conn.execute(
            "INSERT OR REPLACE INTO t (id, a, b) VALUES (3, 'A1', 'B2');",
            (),
        )
        .await?;

        // Both victims (id=1 and id=2) are gone; exactly the new row remains.
        assert_eq!(query_scalar_i64(&conn, "SELECT count(*) FROM t;").await?, 1);
        let mut rows = conn.query("SELECT id, a, b FROM t;", ()).await?;
        let row = rows.next().await?.expect("row");
        assert_eq!(row.get_value(0)?, Value::Integer(3));
        assert_eq!(row.get_value(1)?, Value::Text("A1".to_string()));
        assert_eq!(row.get_value(2)?, Value::Text("B2".to_string()));
        assert!(rows.next().await?.is_none());

        // Indexes resolve only the surviving row.
        assert_eq!(
            query_scalar_i64(&conn, "SELECT id FROM t WHERE a = 'A1';").await?,
            3
        );
        assert_eq!(
            query_scalar_i64(&conn, "SELECT id FROM t WHERE b = 'B2';").await?,
            3
        );

        Ok(())
    }

    // ------------------------------------------------------------------
    // Regression: plain INSERT conflict must still error (no Halt regression).
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn test_plain_insert_rowid_conflict_still_errors() -> Result<()> {
        let db = Builder::new_local(":memory:").build().await?;
        let conn = db.connect()?;
        conn.execute(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT NOT NULL);",
            (),
        )
        .await?;
        conn.execute("INSERT INTO t (id, name) VALUES (1, 'Alice');", ())
            .await?;

        // A plain INSERT (no OR clause) on a duplicate rowid must still fail.
        let result = conn
            .execute("INSERT INTO t (id, name) VALUES (1, 'Bob');", ())
            .await;
        assert!(
            result.is_err(),
            "plain INSERT on duplicate PRIMARY KEY must error, got Ok"
        );

        // The original row is intact and no second row was written.
        assert_eq!(query_scalar_i64(&conn, "SELECT count(*) FROM t;").await?, 1);

        Ok(())
    }

    // ------------------------------------------------------------------
    // Regression: orphaned-WAL row duplication.
    //
    // A previous session leaves a populated `-wal` behind; the main `.db`
    // file is then deleted (or otherwise recreated empty) while the `-wal`
    // survives. On reopen the engine must NOT replay that orphaned WAL — doing
    // so resurrects the previous session's committed pages on top of the fresh
    // database, so every row count grows by the stale content on each reopen.
    //
    // This mirrors the downstream `oxiaero-ros2` rosbag2 roundtrip failure:
    // AUTOINCREMENT PRIMARY KEY + two NON-UNIQUE secondary indexes + a BLOB
    // column, two single-row INSERTs, then a `SELECT ... ORDER BY <indexed col>`
    // read-back that returned 4 (then 6, 8, ...) rows instead of 2 because the
    // index-driven scan walked the resurrected + new index entries.
    //
    // Index maintenance only runs under `index_experimental` (a plain INSERT
    // into an indexed table is rejected without it — exactly the feature the
    // `oxisql-sqlite-compat` consumer enables), so these tests are gated on it.
    // ------------------------------------------------------------------

    /// Count rows returned by `sql` by draining the cursor (works for both
    /// table scans and index-driven scans like `ORDER BY <indexed column>`).
    #[cfg(feature = "index_experimental")]
    async fn query_row_count(conn: &Connection, sql: &str) -> Result<i64> {
        let mut rows = conn.query(sql, ()).await?;
        let mut n = 0i64;
        while rows.next().await?.is_some() {
            n += 1;
        }
        Ok(n)
    }

    /// Apply the exact consumer schema (idempotent — `IF NOT EXISTS`) to `conn`.
    #[cfg(feature = "index_experimental")]
    async fn create_messages_schema(conn: &Connection) -> Result<()> {
        conn.execute(
            "CREATE TABLE IF NOT EXISTS messages (id INTEGER PRIMARY KEY AUTOINCREMENT, topic TEXT NOT NULL, timestamp INTEGER NOT NULL, data BLOB NOT NULL);",
            (),
        )
        .await?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_messages_topic ON messages (topic);",
            (),
        )
        .await?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_messages_timestamp ON messages (timestamp);",
            (),
        )
        .await?;
        Ok(())
    }

    /// Insert one message row via positional parameters (mirrors the consumer's
    /// `INSERT INTO messages (topic, timestamp, data) VALUES (?, ?, ?)`).
    #[cfg(feature = "index_experimental")]
    async fn insert_message(
        conn: &Connection,
        topic: &str,
        timestamp: i64,
        data: Vec<u8>,
    ) -> Result<()> {
        conn.execute(
            "INSERT INTO messages (topic, timestamp, data) VALUES (?, ?, ?);",
            params::Params::Positional(vec![
                Value::Text(topic.to_string()),
                Value::Integer(timestamp),
                Value::Blob(data),
            ]),
        )
        .await?;
        Ok(())
    }

    /// The full downstream consumer reproduction: write 2 rows to a file-backed
    /// DB, delete ONLY the main `.db` (leaving the populated `-wal`, exactly what
    /// the consumer's test harness does between runs), recreate + write 2 rows
    /// again, then read back. Must be exactly 2 rows — both via a plain table
    /// scan AND via the consumer's `ORDER BY timestamp` (index-driven) read — and
    /// must not accumulate across repeated cycles.
    #[cfg(feature = "index_experimental")]
    #[tokio::test]
    async fn test_orphaned_wal_does_not_duplicate_rows_two_indexes() -> Result<()> {
        let dir = std::env::temp_dir().join(format!(
            "oxisqlite_orphan_wal_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let db_path = dir.join("messages.db3");
        let p = db_path.to_str().expect("utf-8 path").to_string();

        // One write-then-readback cycle that recreates the DB while leaving any
        // pre-existing `-wal` in place.
        async fn cycle(p: &str) -> Result<(i64, i64)> {
            // Recreate the main DB file but keep a stale `-wal` if present.
            let _ = std::fs::remove_file(p);
            {
                let db = Builder::new_local(p).build().await?;
                let conn = db.connect()?;
                create_messages_schema(&conn).await?;
                insert_message(&conn, "/imu", 1_000_000_000, vec![0xDE, 0xAD]).await?;
                insert_message(&conn, "/gps", 2_000_000_000, vec![0xBE, 0xEF]).await?;
            }
            let db = Builder::new_local(p).build().await?;
            let conn = db.connect()?;
            create_messages_schema(&conn).await?; // consumer re-runs schema on open
            let scan =
                query_row_count(&conn, "SELECT timestamp, topic, data FROM messages;").await?;
            let ordered = query_row_count(
                &conn,
                "SELECT timestamp, topic, data FROM messages ORDER BY timestamp;",
            )
            .await?;
            Ok((scan, ordered))
        }

        // First cycle starts clean.
        let (scan1, ord1) = cycle(&p).await?;
        assert_eq!(scan1, 2, "cycle 1 table scan");
        assert_eq!(ord1, 2, "cycle 1 ORDER BY timestamp (index scan)");

        // Subsequent cycles each find a populated stale `-wal`; the orphaned WAL
        // must be discarded, so counts stay at 2 (pre-fix they were 4, 6, ...).
        for c in 2..=3 {
            let (scan, ord) = cycle(&p).await?;
            assert_eq!(scan, 2, "cycle {c} table scan must stay 2");
            assert_eq!(ord, 2, "cycle {c} ORDER BY timestamp must stay 2");
        }

        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    /// Consumer-equivalent roundtrip on a clean DB: AUTOINCREMENT + 2 non-unique
    /// indexes + BLOB, 2 writes -> exactly 2 rows, with correct column values via
    /// the index-driven `ORDER BY timestamp` read.
    #[cfg(feature = "index_experimental")]
    #[tokio::test]
    async fn test_two_non_unique_indexes_roundtrip_values() -> Result<()> {
        let db = Builder::new_local(":memory:").build().await?;
        let conn = db.connect()?;
        create_messages_schema(&conn).await?;
        insert_message(&conn, "/imu", 1_000_000_000, vec![0xDE, 0xAD]).await?;
        insert_message(&conn, "/gps", 2_000_000_000, vec![0xBE, 0xEF]).await?;

        assert_eq!(
            query_scalar_i64(&conn, "SELECT count(*) FROM messages;").await?,
            2
        );

        let mut rows = conn
            .query(
                "SELECT timestamp, topic, data FROM messages ORDER BY timestamp;",
                (),
            )
            .await?;
        let r0 = rows.next().await?.expect("row 0");
        assert_eq!(r0.get_value(0)?, Value::Integer(1_000_000_000));
        assert_eq!(r0.get_value(1)?, Value::Text("/imu".to_string()));
        assert_eq!(r0.get_value(2)?, Value::Blob(vec![0xDE, 0xAD]));
        let r1 = rows.next().await?.expect("row 1");
        assert_eq!(r1.get_value(0)?, Value::Integer(2_000_000_000));
        assert_eq!(r1.get_value(1)?, Value::Text("/gps".to_string()));
        assert_eq!(r1.get_value(2)?, Value::Blob(vec![0xBE, 0xEF]));
        assert!(rows.next().await?.is_none(), "exactly two rows");

        Ok(())
    }

    /// Index-count matrix: 0/1/2/3 NON-UNIQUE secondary indexes, two single-row
    /// INSERTs each -> exactly 2 rows, verified by BOTH a plain table scan and an
    /// index-driven `ORDER BY a` scan (which is what surfaces duplicate index
    /// entries).
    #[cfg(feature = "index_experimental")]
    #[tokio::test]
    async fn test_index_count_matrix_single_inserts() -> Result<()> {
        for n_idx in 0..=3usize {
            let db = Builder::new_local(":memory:").build().await?;
            let conn = db.connect()?;
            conn.execute(
                "CREATE TABLE t (id INTEGER PRIMARY KEY AUTOINCREMENT, a INTEGER, b INTEGER, c INTEGER);",
                (),
            )
            .await?;
            let cols = ["a", "b", "c"];
            for col in cols.iter().take(n_idx) {
                conn.execute(&format!("CREATE INDEX idx_{col} ON t ({col});"), ())
                    .await?;
            }
            conn.execute("INSERT INTO t (a, b, c) VALUES (1, 1, 1);", ())
                .await?;
            conn.execute("INSERT INTO t (a, b, c) VALUES (2, 2, 2);", ())
                .await?;

            assert_eq!(
                query_scalar_i64(&conn, "SELECT count(*) FROM t;").await?,
                2,
                "n_idx={n_idx}: count(*)"
            );
            assert_eq!(
                query_row_count(&conn, "SELECT a FROM t;").await?,
                2,
                "n_idx={n_idx}: table scan"
            );
            assert_eq!(
                query_row_count(&conn, "SELECT a FROM t ORDER BY a;").await?,
                2,
                "n_idx={n_idx}: ORDER BY a (index scan)"
            );
        }
        Ok(())
    }

    /// Multi-row `INSERT ... VALUES (..),(..),(..)` into a table with two
    /// non-unique indexes -> exactly 3 rows (table scan and index scan agree).
    #[cfg(feature = "index_experimental")]
    #[tokio::test]
    async fn test_multi_row_insert_two_indexes() -> Result<()> {
        let db = Builder::new_local(":memory:").build().await?;
        let conn = db.connect()?;
        conn.execute(
            "CREATE TABLE t (id INTEGER PRIMARY KEY AUTOINCREMENT, a INTEGER, b INTEGER);",
            (),
        )
        .await?;
        conn.execute("CREATE INDEX idx_a ON t (a);", ()).await?;
        conn.execute("CREATE INDEX idx_b ON t (b);", ()).await?;
        conn.execute("INSERT INTO t (a, b) VALUES (1, 10), (2, 20), (3, 30);", ())
            .await?;

        assert_eq!(query_scalar_i64(&conn, "SELECT count(*) FROM t;").await?, 3);
        assert_eq!(query_row_count(&conn, "SELECT a FROM t;").await?, 3);
        assert_eq!(
            query_row_count(&conn, "SELECT a FROM t ORDER BY a;").await?,
            3
        );
        assert_eq!(
            query_row_count(&conn, "SELECT b FROM t ORDER BY b;").await?,
            3
        );
        Ok(())
    }

    /// `INSERT OR IGNORE` into a table with two non-unique indexes: a plain
    /// (non-unique) secondary index never causes a conflict, so all rows land
    /// exactly once.
    #[cfg(feature = "index_experimental")]
    #[tokio::test]
    async fn test_insert_or_ignore_two_non_unique_indexes() -> Result<()> {
        let db = Builder::new_local(":memory:").build().await?;
        let conn = db.connect()?;
        conn.execute(
            "CREATE TABLE t (id INTEGER PRIMARY KEY AUTOINCREMENT, a INTEGER, b INTEGER);",
            (),
        )
        .await?;
        conn.execute("CREATE INDEX idx_a ON t (a);", ()).await?;
        conn.execute("CREATE INDEX idx_b ON t (b);", ()).await?;

        // Duplicate (a,b) values are fine for non-unique indexes; nothing is
        // ignored and nothing is duplicated.
        conn.execute("INSERT OR IGNORE INTO t (a, b) VALUES (1, 10);", ())
            .await?;
        conn.execute("INSERT OR IGNORE INTO t (a, b) VALUES (1, 10);", ())
            .await?;

        assert_eq!(query_scalar_i64(&conn, "SELECT count(*) FROM t;").await?, 2);
        assert_eq!(
            query_row_count(&conn, "SELECT a FROM t ORDER BY a;").await?,
            2
        );
        Ok(())
    }

    /// `INSERT OR REPLACE` into a table that has both a UNIQUE index and a
    /// secondary NON-UNIQUE index: replacing on the unique-index conflict must
    /// delete the victim's entry from EVERY index, leaving exactly one row and
    /// no duplicate index entries.
    #[cfg(feature = "index_experimental")]
    #[tokio::test]
    async fn test_insert_or_replace_unique_plus_non_unique_index() -> Result<()> {
        let db = Builder::new_local(":memory:").build().await?;
        let conn = db.connect()?;
        conn.execute(
            "CREATE TABLE t (id INTEGER PRIMARY KEY AUTOINCREMENT, email TEXT, tag INTEGER);",
            (),
        )
        .await?;
        conn.execute("CREATE UNIQUE INDEX idx_email ON t (email);", ())
            .await?;
        conn.execute("CREATE INDEX idx_tag ON t (tag);", ()).await?;
        conn.execute(
            "INSERT INTO t (id, email, tag) VALUES (1, 'a@example.com', 7);",
            (),
        )
        .await?;

        // New rowid, same unique email -> victim id=1 replaced by id=2.
        conn.execute(
            "INSERT OR REPLACE INTO t (id, email, tag) VALUES (2, 'a@example.com', 9);",
            (),
        )
        .await?;

        assert_eq!(query_scalar_i64(&conn, "SELECT count(*) FROM t;").await?, 1);
        // The non-unique secondary index must resolve only the surviving row
        // (no orphaned victim entry left behind).
        assert_eq!(
            query_row_count(&conn, "SELECT id FROM t ORDER BY tag;").await?,
            1
        );
        assert_eq!(
            query_scalar_i64(&conn, "SELECT id FROM t WHERE email = 'a@example.com';").await?,
            2
        );
        assert_eq!(
            query_scalar_i64(&conn, "SELECT id FROM t WHERE tag = 9;").await?,
            2
        );
        // The old tag value must no longer resolve any row.
        assert_eq!(
            query_row_count(&conn, "SELECT id FROM t WHERE tag = 7;").await?,
            0
        );
        Ok(())
    }
}
