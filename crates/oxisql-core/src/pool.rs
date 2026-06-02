//! Connection pool trait for OxiSQL backends.

use async_trait::async_trait;

use crate::{Connection, OxiSqlError};

/// A pool of database connections.
///
/// Implementors manage a bounded set of backend connections and hand them
/// out to callers on demand.  When the returned [`Box<dyn Connection>`] is
/// dropped, the underlying connection is returned to the pool automatically
/// (handled by each backend's pool implementation).
///
/// The trait is object-safe: `Box<dyn ConnectionPool>` is a valid type.
#[async_trait]
pub trait ConnectionPool: Send + Sync {
    /// Check out a connection from the pool.
    ///
    /// Blocks until a connection is available or returns an error if the
    /// pool is exhausted / timed out.
    async fn get(&self) -> Result<Box<dyn Connection + Send>, OxiSqlError>;

    /// Maximum number of connections the pool will hold.
    fn pool_size(&self) -> usize;

    /// Number of connections currently idle (available for checkout).
    fn idle_count(&self) -> usize;

    /// Number of connections currently checked out (active).
    fn active_count(&self) -> usize;

    /// Verify that the pool is healthy by probing the backend.
    ///
    /// For RDBMS backends this typically executes `SELECT 1` on an idle
    /// connection.  For in-memory backends it is a no-op.
    async fn health_check(&self) -> Result<(), OxiSqlError>;

    /// Drain all idle connections and prevent new checkouts.
    ///
    /// Active (checked-out) connections are allowed to finish normally;
    /// they are closed when returned to the pool after this call.
    async fn close(&self);
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile-time assertion that `ConnectionPool` is object-safe.
    ///
    /// If this function compiles, `dyn ConnectionPool` is a valid type.
    fn _assert_object_safe(_p: &dyn ConnectionPool) {}
}
