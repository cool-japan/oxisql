//! LRU AST cache for parsed SQL statements.

use lru::LruCache;
use oxisql_core::OxiSqlError;
use sqlparser::ast::Statement;
use std::num::NonZeroUsize;
use std::sync::Mutex;

use crate::{parse_with_dialect, SqlDialect};

// ── LRU AST cache ───────────────────────────────────────────────────────────

/// A thread-safe LRU cache for parsed SQL ASTs.
///
/// Avoids re-parsing identical SQL strings. Cache capacity is configurable.
/// Both the SQL text and the [`SqlDialect`] are used as the cache key so that
/// the same query text parsed under different dialects is stored separately.
///
/// # Example
///
/// ```rust
/// use oxisql_parse::{ParseCache, SqlDialect};
///
/// let cache = ParseCache::new(32);
/// let stmts = cache.parse("SELECT 1", SqlDialect::Generic).unwrap();
/// assert_eq!(stmts.len(), 1);
/// assert_eq!(cache.len(), 1);
/// ```
pub struct ParseCache {
    inner: Mutex<LruCache<(String, SqlDialect), Vec<Statement>>>,
}

impl ParseCache {
    /// Create a new cache with the given capacity.
    ///
    /// If `capacity` is 0 it is silently promoted to 1 so the cache is always
    /// functional.
    pub fn new(capacity: usize) -> Self {
        let cap = NonZeroUsize::new(capacity.max(1)).unwrap_or(NonZeroUsize::MIN);
        Self {
            inner: Mutex::new(LruCache::new(cap)),
        }
    }

    /// Parse `sql` with `dialect`, returning the cached result if available.
    ///
    /// On a cache miss the SQL is parsed and the result stored before returning.
    pub fn parse(&self, sql: &str, dialect: SqlDialect) -> Result<Vec<Statement>, OxiSqlError> {
        let key = (sql.to_string(), dialect);
        {
            let mut cache = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(stmts) = cache.get(&key) {
                return Ok(stmts.clone());
            }
        }
        let result = parse_with_dialect(sql, key.1)?;
        {
            let mut cache = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            cache.put(key, result.clone());
        }
        Ok(result)
    }

    /// Return the number of currently cached entries.
    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }

    /// Return `true` if the cache holds no entries.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Evict all cached entries.
    pub fn clear(&self) {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
    }
}

impl std::fmt::Debug for ParseCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let len = self.len();
        write!(f, "ParseCache(len={len})")
    }
}
