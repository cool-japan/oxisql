//! Middleware wrappers for [`Connection`] — logging and metrics.
//!
//! These are zero-cost wrappers in the sense that they delegate all operations
//! to the inner connection; the only overhead is the middleware logic itself
//! (timing, log formatting, counter increments).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;

use crate::schema::{ColumnInfo, ForeignKeyInfo, IndexInfo, TableInfo};
use crate::{Connection, OxiSqlError, PreparedStatement, Row, ToSqlValue, Transaction};

// ── LoggingConnection ─────────────────────────────────────────────────────────

/// A [`Connection`] wrapper that logs every SQL operation.
///
/// Uses the [`log`] crate at `DEBUG` level for successful operations and
/// `WARN` level for errors.  Enable logging in your application with any
/// `log`-compatible backend (e.g. `env_logger`, `tracing`).
///
/// # Example
///
/// ```rust,no_run
/// # async fn example() -> Result<(), oxisql_core::OxiSqlError> {
/// // use oxisql_core::middleware::LoggingConnection;
/// // let conn = LoggingConnection::new(backend_conn);
/// // conn.execute("INSERT INTO t VALUES ($1)", &[&42i64]).await?;
/// // Logs: [execute] INSERT INTO t VALUES ($1) — 123µs
/// # Ok(())
/// # }
/// ```
pub struct LoggingConnection<C> {
    inner: C,
    prefix: String,
}

impl<C: Connection> LoggingConnection<C> {
    /// Wrap `inner` with logging.  Log lines have no prefix.
    pub fn new(inner: C) -> Self {
        Self {
            inner,
            prefix: String::new(),
        }
    }

    /// Wrap `inner` with a label prepended to log lines.
    pub fn with_prefix(inner: C, prefix: impl Into<String>) -> Self {
        Self {
            inner,
            prefix: prefix.into(),
        }
    }

    /// Consume the wrapper and return the inner connection.
    pub fn into_inner(self) -> C {
        self.inner
    }

    /// Return a reference to the prefix string.
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    fn fmt_prefix(&self) -> String {
        if self.prefix.is_empty() {
            String::new()
        } else {
            format!("{} ", self.prefix)
        }
    }
}

#[async_trait]
impl<C: Connection + Send + Sync> Connection for LoggingConnection<C> {
    async fn execute(&self, sql: &str, params: &[&dyn ToSqlValue]) -> Result<u64, OxiSqlError> {
        let t = Instant::now();
        let result = self.inner.execute(sql, params).await;
        let elapsed = t.elapsed();
        match &result {
            Ok(n) => log::debug!(
                "[{}execute] {} row(s) affected — {:.3}ms{}",
                self.fmt_prefix(),
                n,
                elapsed.as_secs_f64() * 1000.0,
                truncate_sql(sql),
            ),
            Err(e) => log::warn!(
                "[{}execute] ERROR {} — {:.3}ms{}",
                self.fmt_prefix(),
                e,
                elapsed.as_secs_f64() * 1000.0,
                truncate_sql(sql),
            ),
        }
        result
    }

    async fn query(&self, sql: &str, params: &[&dyn ToSqlValue]) -> Result<Vec<Row>, OxiSqlError> {
        let t = Instant::now();
        let result = self.inner.query(sql, params).await;
        let elapsed = t.elapsed();
        match &result {
            Ok(rows) => log::debug!(
                "[{}query] {} row(s) — {:.3}ms{}",
                self.fmt_prefix(),
                rows.len(),
                elapsed.as_secs_f64() * 1000.0,
                truncate_sql(sql),
            ),
            Err(e) => log::warn!(
                "[{}query] ERROR {} — {:.3}ms{}",
                self.fmt_prefix(),
                e,
                elapsed.as_secs_f64() * 1000.0,
                truncate_sql(sql),
            ),
        }
        result
    }

    async fn transaction(&self) -> Result<Box<dyn Transaction + '_>, OxiSqlError> {
        log::debug!("[{}transaction] BEGIN", self.fmt_prefix());
        self.inner.transaction().await
    }

    async fn execute_batch(&self, sql: &str) -> Result<u64, OxiSqlError> {
        let t = Instant::now();
        let result = self.inner.execute_batch(sql).await;
        log::debug!(
            "[{}execute_batch] {:.3}ms{}",
            self.fmt_prefix(),
            t.elapsed().as_secs_f64() * 1000.0,
            truncate_sql(sql),
        );
        result
    }

    async fn ping(&self) -> Result<(), OxiSqlError> {
        log::debug!("[{}ping]", self.fmt_prefix());
        self.inner.ping().await
    }

    async fn prepare(&self, sql: &str) -> Result<Box<dyn PreparedStatement + '_>, OxiSqlError> {
        log::debug!("[{}prepare]{}", self.fmt_prefix(), truncate_sql(sql));
        self.inner.prepare(sql).await
    }

    async fn tables(&self) -> Result<Vec<TableInfo>, OxiSqlError> {
        self.inner.tables().await
    }

    async fn columns(&self, table: &str) -> Result<Vec<ColumnInfo>, OxiSqlError> {
        self.inner.columns(table).await
    }

    async fn indexes(&self, table: &str) -> Result<Vec<IndexInfo>, OxiSqlError> {
        self.inner.indexes(table).await
    }

    async fn foreign_keys(&self, table: &str) -> Result<Vec<ForeignKeyInfo>, OxiSqlError> {
        self.inner.foreign_keys(table).await
    }
}

// ── TracingConnection ─────────────────────────────────────────────────────────

/// A [`Connection`] wrapper that emits [`tracing`](https://docs.rs/tracing) spans
/// and events for every SQL operation.
///
/// Mirrors [`LoggingConnection`] exactly but uses `tracing::debug!` /
/// `tracing::warn!` instead of `log::debug!` / `log::warn!`.
///
/// Available only when the `tracing` feature is enabled.
///
/// # Example
///
/// ```rust,no_run
/// # #[cfg(feature = "tracing")]
/// # async fn example() -> Result<(), oxisql_core::OxiSqlError> {
/// // use oxisql_core::TracingConnection;
/// // let conn = TracingConnection::new(backend_conn);
/// // conn.execute("INSERT INTO t VALUES ($1)", &[&42i64]).await?;
/// // Emits a tracing DEBUG event: [execute] 1 row(s) affected — …ms | …
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "tracing")]
pub struct TracingConnection<C> {
    inner: C,
    prefix: String,
}

#[cfg(feature = "tracing")]
impl<C: Connection> TracingConnection<C> {
    /// Wrap `inner` with tracing.  Events have no prefix.
    pub fn new(inner: C) -> Self {
        Self {
            inner,
            prefix: String::new(),
        }
    }

    /// Wrap `inner` with a label prepended to tracing event fields.
    pub fn with_prefix(inner: C, prefix: impl Into<String>) -> Self {
        Self {
            inner,
            prefix: prefix.into(),
        }
    }

    /// Consume the wrapper and return the inner connection.
    pub fn into_inner(self) -> C {
        self.inner
    }

    /// Return a reference to the prefix string.
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    fn fmt_prefix(&self) -> String {
        if self.prefix.is_empty() {
            String::new()
        } else {
            format!("{} ", self.prefix)
        }
    }
}

#[cfg(feature = "tracing")]
#[async_trait]
impl<C: Connection + Send + Sync> Connection for TracingConnection<C> {
    async fn execute(&self, sql: &str, params: &[&dyn ToSqlValue]) -> Result<u64, OxiSqlError> {
        let t = Instant::now();
        let result = self.inner.execute(sql, params).await;
        let elapsed = t.elapsed();
        match &result {
            Ok(n) => tracing::debug!(
                "[{}execute] {} row(s) affected — {:.3}ms{}",
                self.fmt_prefix(),
                n,
                elapsed.as_secs_f64() * 1000.0,
                truncate_sql(sql),
            ),
            Err(e) => tracing::warn!(
                "[{}execute] ERROR {} — {:.3}ms{}",
                self.fmt_prefix(),
                e,
                elapsed.as_secs_f64() * 1000.0,
                truncate_sql(sql),
            ),
        }
        result
    }

    async fn query(&self, sql: &str, params: &[&dyn ToSqlValue]) -> Result<Vec<Row>, OxiSqlError> {
        let t = Instant::now();
        let result = self.inner.query(sql, params).await;
        let elapsed = t.elapsed();
        match &result {
            Ok(rows) => tracing::debug!(
                "[{}query] {} row(s) — {:.3}ms{}",
                self.fmt_prefix(),
                rows.len(),
                elapsed.as_secs_f64() * 1000.0,
                truncate_sql(sql),
            ),
            Err(e) => tracing::warn!(
                "[{}query] ERROR {} — {:.3}ms{}",
                self.fmt_prefix(),
                e,
                elapsed.as_secs_f64() * 1000.0,
                truncate_sql(sql),
            ),
        }
        result
    }

    async fn transaction(&self) -> Result<Box<dyn Transaction + '_>, OxiSqlError> {
        tracing::debug!("[{}transaction] BEGIN", self.fmt_prefix());
        self.inner.transaction().await
    }

    async fn execute_batch(&self, sql: &str) -> Result<u64, OxiSqlError> {
        let t = Instant::now();
        let result = self.inner.execute_batch(sql).await;
        tracing::debug!(
            "[{}execute_batch] {:.3}ms{}",
            self.fmt_prefix(),
            t.elapsed().as_secs_f64() * 1000.0,
            truncate_sql(sql),
        );
        result
    }

    async fn ping(&self) -> Result<(), OxiSqlError> {
        tracing::debug!("[{}ping]", self.fmt_prefix());
        self.inner.ping().await
    }

    async fn prepare(&self, sql: &str) -> Result<Box<dyn PreparedStatement + '_>, OxiSqlError> {
        tracing::debug!("[{}prepare]{}", self.fmt_prefix(), truncate_sql(sql));
        self.inner.prepare(sql).await
    }

    async fn tables(&self) -> Result<Vec<TableInfo>, OxiSqlError> {
        self.inner.tables().await
    }

    async fn columns(&self, table: &str) -> Result<Vec<ColumnInfo>, OxiSqlError> {
        self.inner.columns(table).await
    }

    async fn indexes(&self, table: &str) -> Result<Vec<IndexInfo>, OxiSqlError> {
        self.inner.indexes(table).await
    }

    async fn foreign_keys(&self, table: &str) -> Result<Vec<ForeignKeyInfo>, OxiSqlError> {
        self.inner.foreign_keys(table).await
    }
}

#[cfg(all(test, feature = "tracing"))]
mod tracing_tests {
    use std::sync::{Arc, Mutex};

    use tracing_subscriber::fmt::MakeWriter;

    use crate::{Connection, OxiSqlError, Row, ToSqlValue, Transaction, Value};

    use super::TracingConnection;

    // ── Minimal in-memory writer for tracing_subscriber ──────────────────────

    #[derive(Clone, Default)]
    struct MemWriter(Arc<Mutex<Vec<u8>>>);

    impl MemWriter {
        fn contents(&self) -> String {
            let data = self.0.lock().expect("lock");
            String::from_utf8_lossy(&data).into_owned()
        }
    }

    impl std::io::Write for MemWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("lock").extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for MemWriter {
        type Writer = MemWriter;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    // ── Mock Connection ───────────────────────────────────────────────────────

    struct MockConn;

    #[async_trait::async_trait]
    impl Connection for MockConn {
        async fn execute(
            &self,
            _sql: &str,
            _params: &[&dyn ToSqlValue],
        ) -> Result<u64, OxiSqlError> {
            Ok(1)
        }

        async fn query(
            &self,
            _sql: &str,
            _params: &[&dyn ToSqlValue],
        ) -> Result<Vec<Row>, OxiSqlError> {
            Ok(vec![Row::new(vec!["n".into()], vec![Value::I64(42)])])
        }

        async fn transaction(&self) -> Result<Box<dyn Transaction + '_>, OxiSqlError> {
            Err(OxiSqlError::Other("no txn".into()))
        }
    }

    // ── Tests ─────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn tracing_execute_emits_event() {
        let writer = MemWriter::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(writer.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::DEBUG)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let conn = TracingConnection::new(MockConn);
        let rows_affected = conn
            .execute("INSERT INTO t VALUES ($1)", &[&42i64])
            .await
            .expect("execute ok");
        assert_eq!(rows_affected, 1);

        let output = writer.contents();
        assert!(
            output.contains("execute"),
            "expected 'execute' in: {output}"
        );
    }

    #[tokio::test]
    async fn tracing_query_emits_event() {
        let writer = MemWriter::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(writer.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::DEBUG)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let conn = TracingConnection::new(MockConn);
        let rows = conn.query("SELECT n FROM t", &[]).await.expect("query ok");
        assert_eq!(rows.len(), 1);

        let output = writer.contents();
        assert!(output.contains("query"), "expected 'query' in: {output}");
    }

    #[test]
    fn tracing_conn_prefix_and_accessors() {
        let conn = TracingConnection::with_prefix(MockConn, "mydb");
        assert_eq!(conn.prefix(), "mydb");
        // into_inner recovers the wrapped conn without panic
        let _inner: MockConn = conn.into_inner();
    }
}

// ── MetricsConnection ─────────────────────────────────────────────────────────

/// Counters tracked by [`MetricsConnection`].
#[derive(Debug, Default)]
pub struct ConnectionMetrics {
    /// Total number of `execute` calls.
    pub executes: AtomicU64,
    /// Total number of `query` calls.
    pub queries: AtomicU64,
    /// Total number of errors (execute + query).
    pub errors: AtomicU64,
    /// Total microseconds spent in `execute` calls.
    pub execute_us: AtomicU64,
    /// Total microseconds spent in `query` calls.
    pub query_us: AtomicU64,
}

impl ConnectionMetrics {
    /// Return a snapshot of the metrics as plain integers.
    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            executes: self.executes.load(Ordering::Relaxed),
            queries: self.queries.load(Ordering::Relaxed),
            errors: self.errors.load(Ordering::Relaxed),
            execute_us: self.execute_us.load(Ordering::Relaxed),
            query_us: self.query_us.load(Ordering::Relaxed),
        }
    }
}

/// A point-in-time snapshot of [`ConnectionMetrics`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetricsSnapshot {
    /// Total number of `execute` calls recorded so far.
    pub executes: u64,
    /// Total number of `query` calls recorded so far.
    pub queries: u64,
    /// Total number of errors (execute + query) recorded so far.
    pub errors: u64,
    /// Total microseconds spent in `execute` calls.
    pub execute_us: u64,
    /// Total microseconds spent in `query` calls.
    pub query_us: u64,
}

/// A [`Connection`] wrapper that counts operations and measures latency.
///
/// Access metrics through [`MetricsConnection::metrics`].  The metrics
/// counters are `AtomicU64` so they are safe to read concurrently.
///
/// # Example
///
/// ```rust,no_run
/// # async fn example() -> Result<(), oxisql_core::OxiSqlError> {
/// // use oxisql_core::middleware::MetricsConnection;
/// // use std::sync::Arc;
/// // let metrics = Arc::new(oxisql_core::middleware::ConnectionMetrics::default());
/// // let conn = MetricsConnection::new(backend_conn, Arc::clone(&metrics));
/// // conn.execute("INSERT INTO t VALUES ($1)", &[&42i64]).await?;
/// // println!("{:?}", metrics.snapshot());
/// # Ok(())
/// # }
/// ```
pub struct MetricsConnection<C> {
    inner: C,
    metrics: Arc<ConnectionMetrics>,
}

impl<C: Connection> MetricsConnection<C> {
    /// Wrap `inner` with the given shared metrics store.
    pub fn new(inner: C, metrics: Arc<ConnectionMetrics>) -> Self {
        Self { inner, metrics }
    }

    /// Return a reference to the shared metrics.
    pub fn metrics(&self) -> &Arc<ConnectionMetrics> {
        &self.metrics
    }

    /// Consume the wrapper and return the inner connection.
    pub fn into_inner(self) -> C {
        self.inner
    }
}

#[async_trait]
impl<C: Connection + Send + Sync> Connection for MetricsConnection<C> {
    async fn execute(&self, sql: &str, params: &[&dyn ToSqlValue]) -> Result<u64, OxiSqlError> {
        let t = Instant::now();
        let result = self.inner.execute(sql, params).await;
        let us = t.elapsed().as_micros() as u64;
        self.metrics.executes.fetch_add(1, Ordering::Relaxed);
        self.metrics.execute_us.fetch_add(us, Ordering::Relaxed);
        if result.is_err() {
            self.metrics.errors.fetch_add(1, Ordering::Relaxed);
        }
        result
    }

    async fn query(&self, sql: &str, params: &[&dyn ToSqlValue]) -> Result<Vec<Row>, OxiSqlError> {
        let t = Instant::now();
        let result = self.inner.query(sql, params).await;
        let us = t.elapsed().as_micros() as u64;
        self.metrics.queries.fetch_add(1, Ordering::Relaxed);
        self.metrics.query_us.fetch_add(us, Ordering::Relaxed);
        if result.is_err() {
            self.metrics.errors.fetch_add(1, Ordering::Relaxed);
        }
        result
    }

    async fn transaction(&self) -> Result<Box<dyn Transaction + '_>, OxiSqlError> {
        self.inner.transaction().await
    }

    async fn execute_batch(&self, sql: &str) -> Result<u64, OxiSqlError> {
        self.inner.execute_batch(sql).await
    }

    async fn ping(&self) -> Result<(), OxiSqlError> {
        self.inner.ping().await
    }

    async fn prepare(&self, sql: &str) -> Result<Box<dyn PreparedStatement + '_>, OxiSqlError> {
        self.inner.prepare(sql).await
    }

    async fn tables(&self) -> Result<Vec<TableInfo>, OxiSqlError> {
        self.inner.tables().await
    }

    async fn columns(&self, table: &str) -> Result<Vec<ColumnInfo>, OxiSqlError> {
        self.inner.columns(table).await
    }

    async fn indexes(&self, table: &str) -> Result<Vec<IndexInfo>, OxiSqlError> {
        self.inner.indexes(table).await
    }

    async fn foreign_keys(&self, table: &str) -> Result<Vec<ForeignKeyInfo>, OxiSqlError> {
        self.inner.foreign_keys(table).await
    }
}

// ── RetryConnection ──────────────────────────────────────────────────────────

/// A retry predicate function type: takes an error, returns `true` if it is
/// considered transient (i.e., worth retrying).
pub type RetryPredicate = Arc<dyn Fn(&OxiSqlError) -> bool + Send + Sync>;

/// Policy governing how [`RetryConnection`] retries failed operations.
///
/// The delay sequence for retries is:
/// `initial_delay_ms * backoff_factor^attempt`, capped at `max_delay_ms`.
#[derive(Clone)]
pub struct RetryPolicy {
    /// Maximum number of retry attempts after the first failure.
    pub max_retries: u32,
    /// Initial delay in milliseconds before the first retry.
    pub initial_delay_ms: u64,
    /// Multiplicative backoff factor applied to each successive delay.
    pub backoff_factor: f64,
    /// Upper bound on the delay between retries in milliseconds.
    pub max_delay_ms: u64,
    /// Predicate that returns `true` for transient errors worth retrying.
    pub predicate: RetryPredicate,
}

fn default_retry_predicate() -> RetryPredicate {
    Arc::new(|e: &OxiSqlError| match e {
        OxiSqlError::Timeout(_) => true,
        OxiSqlError::Execution(msg) => {
            msg.contains("connection reset")
                || msg.contains("broken pipe")
                || msg.contains("connection refused")
                || msg.contains("timed out")
                || msg.contains("temporarily unavailable")
        }
        _ => false,
    })
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_delay_ms: 100,
            backoff_factor: 2.0,
            max_delay_ms: 5_000,
            predicate: default_retry_predicate(),
        }
    }
}

impl std::fmt::Debug for RetryPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RetryPolicy")
            .field("max_retries", &self.max_retries)
            .field("initial_delay_ms", &self.initial_delay_ms)
            .field("backoff_factor", &self.backoff_factor)
            .field("max_delay_ms", &self.max_delay_ms)
            .finish()
    }
}

/// A [`Connection`] wrapper that retries transient failures according to a
/// [`RetryPolicy`].
///
/// On each operation, if the inner connection returns an error that the policy
/// predicate considers transient, the wrapper waits an exponentially increasing
/// delay and retries up to `max_retries` times.
///
/// Introspection methods (`tables`, `columns`, `indexes`, `foreign_keys`) and
/// `prepare` are delegated directly to the inner connection without retrying,
/// as they are either non-transient or stateful operations.
///
/// # Example
///
/// ```rust,no_run
/// # use oxisql_core::middleware::{RetryConnection, RetryPolicy};
/// # use oxisql_core::Connection;
/// # async fn example<C: Connection>(inner: C) {
/// let conn = RetryConnection::new(inner, RetryPolicy::default());
/// # }
/// ```
pub struct RetryConnection<C> {
    inner: C,
    policy: RetryPolicy,
}

impl<C: Connection> RetryConnection<C> {
    /// Wrap `inner` with the given retry `policy`.
    pub fn new(inner: C, policy: RetryPolicy) -> Self {
        Self { inner, policy }
    }

    /// Return a reference to the inner connection.
    pub fn inner(&self) -> &C {
        &self.inner
    }

    /// Consume the wrapper and return the inner connection.
    pub fn into_inner(self) -> C {
        self.inner
    }

    /// Compute the delay in milliseconds for a given retry attempt (0-indexed).
    pub(crate) fn delay_ms(&self, attempt: u32) -> u64 {
        let delay =
            self.policy.initial_delay_ms as f64 * self.policy.backoff_factor.powi(attempt as i32);
        (delay as u64).min(self.policy.max_delay_ms)
    }
}

#[async_trait]
impl<C: Connection + Send + Sync> Connection for RetryConnection<C> {
    async fn execute(&self, sql: &str, params: &[&dyn ToSqlValue]) -> Result<u64, OxiSqlError> {
        let mut last_err: Option<OxiSqlError> = None;
        for attempt in 0..=self.policy.max_retries {
            match self.inner.execute(sql, params).await {
                Ok(n) => return Ok(n),
                Err(e) => {
                    if attempt < self.policy.max_retries && (self.policy.predicate)(&e) {
                        let delay = self.delay_ms(attempt);
                        tokio::time::sleep(tokio::time::Duration::from_millis(delay)).await;
                        last_err = Some(e);
                    } else {
                        return Err(e);
                    }
                }
            }
        }
        Err(last_err.unwrap_or_else(|| OxiSqlError::Other("retry exhausted".into())))
    }

    async fn query(&self, sql: &str, params: &[&dyn ToSqlValue]) -> Result<Vec<Row>, OxiSqlError> {
        let mut last_err: Option<OxiSqlError> = None;
        for attempt in 0..=self.policy.max_retries {
            match self.inner.query(sql, params).await {
                Ok(rows) => return Ok(rows),
                Err(e) => {
                    if attempt < self.policy.max_retries && (self.policy.predicate)(&e) {
                        let delay = self.delay_ms(attempt);
                        tokio::time::sleep(tokio::time::Duration::from_millis(delay)).await;
                        last_err = Some(e);
                    } else {
                        return Err(e);
                    }
                }
            }
        }
        Err(last_err.unwrap_or_else(|| OxiSqlError::Other("retry exhausted".into())))
    }

    async fn transaction(&self) -> Result<Box<dyn crate::traits::Transaction + '_>, OxiSqlError> {
        // Transactions involve state that cannot be safely replayed; no retry.
        self.inner.transaction().await
    }

    async fn execute_batch(&self, sql: &str) -> Result<u64, OxiSqlError> {
        // Delegate to inner; individual statements within the batch are
        // not safe to retry automatically as a unit.
        self.inner.execute_batch(sql).await
    }

    async fn ping(&self) -> Result<(), OxiSqlError> {
        let mut last_err: Option<OxiSqlError> = None;
        for attempt in 0..=self.policy.max_retries {
            match self.inner.ping().await {
                Ok(()) => return Ok(()),
                Err(e) => {
                    if attempt < self.policy.max_retries && (self.policy.predicate)(&e) {
                        let delay = self.delay_ms(attempt);
                        tokio::time::sleep(tokio::time::Duration::from_millis(delay)).await;
                        last_err = Some(e);
                    } else {
                        return Err(e);
                    }
                }
            }
        }
        Err(last_err.unwrap_or_else(|| OxiSqlError::Other("retry exhausted".into())))
    }

    async fn prepare(
        &self,
        sql: &str,
    ) -> Result<Box<dyn crate::PreparedStatement + '_>, OxiSqlError> {
        self.inner.prepare(sql).await
    }

    async fn tables(&self) -> Result<Vec<TableInfo>, OxiSqlError> {
        self.inner.tables().await
    }

    async fn columns(&self, table: &str) -> Result<Vec<ColumnInfo>, OxiSqlError> {
        self.inner.columns(table).await
    }

    async fn indexes(&self, table: &str) -> Result<Vec<IndexInfo>, OxiSqlError> {
        self.inner.indexes(table).await
    }

    async fn foreign_keys(&self, table: &str) -> Result<Vec<ForeignKeyInfo>, OxiSqlError> {
        self.inner.foreign_keys(table).await
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Truncate SQL for display, taking care not to split on a UTF-8 char boundary.
fn truncate_sql(sql: &str) -> String {
    const MAX: usize = 80;
    let trimmed = sql.trim();
    if trimmed.len() <= MAX {
        format!(" | {trimmed}")
    } else {
        // Find the char boundary at or before MAX bytes
        let cut = trimmed
            .char_indices()
            .nth(MAX)
            .map(|(i, _)| i)
            .unwrap_or(trimmed.len());
        format!(" | {}…", &trimmed[..cut])
    }
}
