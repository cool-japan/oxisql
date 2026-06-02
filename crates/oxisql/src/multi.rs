//! Multi-backend connection routing.
//!
//! [`MultiConnection`] holds multiple named database backends and can route
//! queries to a specific backend by name, or fan-out operations to all
//! registered backends concurrently.

use std::sync::Arc;

use futures::future::join_all;

use oxisql_core::{Connection, OxiSqlError, Row, ToSqlValue, Value};

use crate::connect;

// ── MultiConnection ──────────────────────────────────────────────────────────

/// A handle to multiple named database backends.
///
/// Useful for cross-database scenarios: read from one backend, write to
/// another, or fan-out queries to multiple replicas.
///
/// # Example
///
/// ```rust,no_run
/// # #[cfg(feature = "embedded")]
/// # #[tokio::main]
/// # async fn main() -> Result<(), oxisql::OxiSqlError> {
/// let mut multi = oxisql::MultiConnection::new();
/// multi.connect_as("primary", "memory://").await?;
/// multi.connect_as("replica", "memory://").await?;
/// multi.execute_on("primary", "CREATE TABLE t (id INT)", &[]).await?;
/// # Ok(())
/// # }
/// # #[cfg(not(feature = "embedded"))]
/// # fn main() {}
/// ```
pub struct MultiConnection {
    backends: Vec<(String, Arc<Box<dyn Connection>>)>,
}

impl Default for MultiConnection {
    fn default() -> Self {
        Self::new()
    }
}

impl MultiConnection {
    /// Create an empty [`MultiConnection`] with no backends registered.
    #[must_use]
    pub fn new() -> Self {
        Self {
            backends: Vec::new(),
        }
    }

    /// Add a named backend connection.
    ///
    /// If a backend with the same name is already registered it is replaced.
    pub fn add(&mut self, name: impl Into<String>, conn: Box<dyn Connection>) {
        let name = name.into();
        // Replace existing entry with the same name.
        for entry in &mut self.backends {
            if entry.0 == name {
                entry.1 = Arc::new(conn);
                return;
            }
        }
        self.backends.push((name, Arc::new(conn)));
    }

    /// Connect to a URI and register the resulting connection under `name`.
    ///
    /// This is a convenience wrapper around [`crate::connect`] and [`add`](Self::add).
    ///
    /// # Errors
    ///
    /// Propagates any error returned by [`crate::connect`].
    pub async fn connect_as(
        &mut self,
        name: impl Into<String>,
        uri: &str,
    ) -> Result<(), OxiSqlError> {
        let conn = connect(uri).await?;
        self.add(name, conn);
        Ok(())
    }

    /// Get a reference to a named backend connection.
    ///
    /// Returns `None` when no backend with the given name has been registered.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&dyn Connection> {
        self.backends
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, arc)| arc.as_ref().as_ref())
    }

    /// Execute a statement on a specific named backend.
    ///
    /// # Errors
    ///
    /// Returns [`OxiSqlError::Other`] with the message `"backend not found: {name}"`
    /// when no backend matching `name` is registered.
    /// Propagates any backend-specific execution error otherwise.
    pub async fn execute_on(
        &self,
        backend_name: &str,
        sql: &str,
        params: &[&dyn ToSqlValue],
    ) -> Result<u64, OxiSqlError> {
        let conn = self
            .backends
            .iter()
            .find(|(n, _)| n == backend_name)
            .map(|(_, arc)| arc.clone())
            .ok_or_else(|| OxiSqlError::Other(format!("backend not found: {backend_name}")))?;
        conn.execute(sql, params).await
    }

    /// Query a specific named backend and return the result rows.
    ///
    /// # Errors
    ///
    /// Returns [`OxiSqlError::Other`] with the message `"backend not found: {name}"`
    /// when no backend matching `backend_name` is registered.
    /// Propagates any backend-specific query error otherwise.
    pub async fn query_on(
        &self,
        backend_name: &str,
        sql: &str,
        params: &[&dyn ToSqlValue],
    ) -> Result<Vec<Row>, OxiSqlError> {
        let conn = self
            .backends
            .iter()
            .find(|(n, _)| n == backend_name)
            .map(|(_, arc)| arc.clone())
            .ok_or_else(|| OxiSqlError::Other(format!("backend not found: {backend_name}")))?;
        conn.query(sql, params).await
    }

    /// Execute a statement on **all** backends concurrently.
    ///
    /// Returns a `Vec` of `(name, Result<u64, OxiSqlError>)` in the order
    /// the backends were added.  Individual backend errors are captured in
    /// their result slot rather than propagated immediately.
    pub async fn execute_all(
        &self,
        sql: &str,
        params: &[&dyn ToSqlValue],
    ) -> Vec<(String, Result<u64, OxiSqlError>)> {
        // Materialise params as owned Values so they can be moved into async
        // blocks (the original &dyn ToSqlValue references are not 'static).
        let owned_params: Vec<Value> = params.iter().map(|p| p.to_value()).collect();

        let futures: Vec<_> = self
            .backends
            .iter()
            .map(|(name, conn)| {
                let name = name.clone();
                let conn = conn.clone();
                let params_snapshot = owned_params.clone();
                async move {
                    let refs: Vec<&dyn ToSqlValue> = params_snapshot
                        .iter()
                        .map(|v| v as &dyn ToSqlValue)
                        .collect();
                    let result = conn.execute(sql, &refs).await;
                    (name, result)
                }
            })
            .collect();

        join_all(futures).await
    }

    /// Query **all** backends concurrently and merge the resulting rows.
    ///
    /// Rows from all backends are concatenated into a single `Vec<Row>`.
    /// Backends that return an error are skipped silently (their error is
    /// logged at debug level).  If every backend fails, an empty `Vec` is
    /// returned (not an error).
    ///
    /// # Errors
    ///
    /// This method itself never returns an `Err`.  The `Result` wrapper is
    /// kept so callers can use `?` in async contexts and future implementations
    /// can add error aggregation.
    pub async fn query_all_merged(
        &self,
        sql: &str,
        params: &[&dyn ToSqlValue],
    ) -> Result<Vec<Row>, OxiSqlError> {
        let owned_params: Vec<Value> = params.iter().map(|p| p.to_value()).collect();

        let futures: Vec<_> = self
            .backends
            .iter()
            .map(|(name, conn)| {
                let name = name.clone();
                let conn = conn.clone();
                let params_snapshot = owned_params.clone();
                async move {
                    let refs: Vec<&dyn ToSqlValue> = params_snapshot
                        .iter()
                        .map(|v| v as &dyn ToSqlValue)
                        .collect();
                    let result = conn.query(sql, &refs).await;
                    (name, result)
                }
            })
            .collect();

        let results = join_all(futures).await;

        let mut merged = Vec::new();
        for (name, result) in results {
            match result {
                Ok(rows) => merged.extend(rows),
                Err(e) => {
                    log::debug!("MultiConnection::query_all_merged: backend '{name}' failed: {e}");
                }
            }
        }
        Ok(merged)
    }

    /// Return the names of all registered backends in the order they were added.
    #[must_use]
    pub fn backend_names(&self) -> Vec<&str> {
        self.backends.iter().map(|(n, _)| n.as_str()).collect()
    }

    /// Remove a backend by name.
    ///
    /// Returns `true` if the backend was present and has been removed,
    /// `false` if no backend with that name was registered.
    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.backends.len();
        self.backends.retain(|(n, _)| n != name);
        self.backends.len() < before
    }

    /// Return the number of registered backends.
    #[must_use]
    pub fn len(&self) -> usize {
        self.backends.len()
    }

    /// Return `true` when no backends are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.backends.is_empty()
    }
}
