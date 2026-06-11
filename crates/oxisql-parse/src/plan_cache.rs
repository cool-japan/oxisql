//! Parameterized-plan cache: an LRU cache keyed by the parameterized SQL
//! template and a schema-generation counter.
//!
//! Two SQL strings that differ only in their literal values (e.g. `WHERE id = 1`
//! vs `WHERE id = 2`) map to the same template (`WHERE id = ?`) and therefore
//! share a single cache entry.  The generation counter allows all existing
//! entries to be cheaply invalidated whenever the schema changes.

use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use lru::LruCache;
use oxisql_core::OxiSqlError;

use crate::decorrelate::PlannerOptions;
use crate::parameterize::parameterize;
use crate::{optimizer, parse_one, plan_statement_with_opts, LogicalPlan};

// ── Cache key ─────────────────────────────────────────────────────────────────

/// The key used to look up a plan in the cache.
///
/// Two queries that differ only in literal values share the same `template`
/// (produced by [`parameterize`]).  The `generation` field is the value of
/// the schema-generation counter at the time the entry was stored; entries
/// from an older generation are never returned.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PlanCacheKey {
    template: String,
    generation: u64,
}

// ── PlanCache ─────────────────────────────────────────────────────────────────

/// Thread-safe LRU cache that maps parameterized SQL templates to
/// [`Arc<LogicalPlan>`] values.
///
/// # Schema invalidation
///
/// Call [`PlanCache::invalidate_schema`] whenever the schema changes (e.g. a
/// table is created or dropped).  This increments an internal generation
/// counter; all existing cache entries are associated with the previous
/// generation and will never be returned again, making them eligible for
/// eviction.
///
/// # Example
///
/// ```rust,no_run
/// use oxisql_parse::PlanCache;
///
/// let cache = PlanCache::new(64);
/// let plan1 = cache.plan("SELECT * FROM t WHERE id = 1").unwrap();
/// let plan2 = cache.plan("SELECT * FROM t WHERE id = 2").unwrap();
/// // Both queries parameterize to the same template, so only one cache entry
/// // was created.
/// assert_eq!(cache.len(), 1);
/// ```
pub struct PlanCache {
    inner: Mutex<LruCache<PlanCacheKey, Arc<LogicalPlan>>>,
    /// Monotonically increasing counter; invalidated entries belong to older
    /// generations.
    generation: AtomicU64,
}

impl PlanCache {
    /// Create a new cache with the given LRU capacity.
    ///
    /// If `capacity` is 0 it is silently promoted to 1 so the cache is always
    /// functional.
    pub fn new(capacity: usize) -> Self {
        let cap = NonZeroUsize::new(capacity.max(1)).unwrap_or(NonZeroUsize::MIN);
        Self {
            inner: Mutex::new(LruCache::new(cap)),
            generation: AtomicU64::new(0),
        }
    }

    /// Invalidate all existing cache entries by incrementing the schema
    /// generation counter.
    ///
    /// Existing entries are not removed immediately; they simply belong to the
    /// previous generation and will never be returned, so they age out of the
    /// LRU naturally.
    pub fn invalidate_schema(&self) {
        self.generation.fetch_add(1, Ordering::Release);
    }

    /// Parse, plan, optimize and cache `sql` using the default
    /// [`crate::PlannerOptions`] (decorrelation enabled).
    ///
    /// Returns a shared [`Arc<LogicalPlan>`] that may be cloned cheaply for
    /// concurrent readers.
    ///
    /// # Errors
    ///
    /// Returns [`OxiSqlError`] if parsing or planning fails.
    pub fn plan(&self, sql: &str) -> Result<Arc<LogicalPlan>, OxiSqlError> {
        self.plan_with(sql, &PlannerOptions::default())
    }

    /// Parse, plan, optimize and cache `sql` using the supplied
    /// [`crate::PlannerOptions`].
    ///
    /// Two calls with the same `sql` structure but different literal values
    /// will map to the same cache key and share one entry.
    ///
    /// # Errors
    ///
    /// Returns [`OxiSqlError`] if parsing or planning fails.
    pub fn plan_with(
        &self,
        sql: &str,
        opts: &PlannerOptions,
    ) -> Result<Arc<LogicalPlan>, OxiSqlError> {
        let template = parameterize(sql).template;
        let generation = self.generation.load(Ordering::Acquire);
        let key = PlanCacheKey {
            template,
            generation,
        };

        // Cache lookup (short critical section).
        {
            let mut cache = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(plan) = cache.get(&key) {
                return Ok(Arc::clone(plan));
            }
        }

        // Cache miss: parse + plan + optimize (outside the lock).
        let stmt = parse_one(sql)?;
        let raw = plan_statement_with_opts(&stmt, opts)?;
        let optimized = Arc::new(optimizer::optimize(raw));

        // Store and return.
        {
            let mut cache = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            // Another thread may have raced us; prefer the cached version if so.
            let entry = cache.get_or_insert(key, || Arc::clone(&optimized));
            Ok(Arc::clone(entry))
        }
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

impl std::fmt::Debug for PlanCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let len = self.len();
        let gen = self.generation.load(Ordering::Relaxed);
        write!(f, "PlanCache(len={len}, generation={gen})")
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Two queries differing only in literals share one cache entry.
    #[test]
    fn test_same_template_single_entry() {
        let cache = PlanCache::new(16);
        cache.plan("SELECT * FROM t WHERE id = 1").unwrap();
        cache.plan("SELECT * FROM t WHERE id = 2").unwrap();
        assert_eq!(
            cache.len(),
            1,
            "both queries should map to the same template"
        );
    }

    // Two structurally different queries → two entries.
    #[test]
    fn test_different_structure_two_entries() {
        let cache = PlanCache::new(16);
        cache.plan("SELECT * FROM t WHERE id = 1").unwrap();
        cache.plan("SELECT * FROM t WHERE name = 'x'").unwrap();
        // Both produce template "SELECT * FROM t WHERE ... = ?" but with different
        // column names (id vs name), so the templates differ.
        assert_eq!(cache.len(), 2);
    }

    // invalidate_schema causes a cache miss on the next call.
    #[test]
    fn test_invalidate_schema_causes_miss() {
        let cache = PlanCache::new(16);
        cache.plan("SELECT * FROM t WHERE id = 1").unwrap();
        assert_eq!(cache.len(), 1);

        cache.invalidate_schema();
        cache.plan("SELECT * FROM t WHERE id = 1").unwrap();
        // The new entry has a different generation, so there are now 2 entries.
        assert_eq!(cache.len(), 2);
    }

    // Thread safety: 8 threads calling plan() concurrently.
    #[test]
    fn test_concurrent_access() {
        use std::sync::Arc;
        use std::thread;

        let cache = Arc::new(PlanCache::new(16));
        let sql = "SELECT * FROM t WHERE id = 42";

        let handles: Vec<_> = (0..8)
            .map(|_| {
                let c = Arc::clone(&cache);
                thread::spawn(move || c.plan(sql).expect("plan should succeed in thread"))
            })
            .collect();

        for h in handles {
            h.join().expect("thread should not panic");
        }
        // All 8 threads raced on the same template → exactly 1 entry.
        assert_eq!(cache.len(), 1);
    }

    // Clear removes all entries.
    #[test]
    fn test_clear() {
        let cache = PlanCache::new(16);
        cache.plan("SELECT * FROM t WHERE id = 1").unwrap();
        assert_eq!(cache.len(), 1);
        cache.clear();
        assert!(cache.is_empty());
    }

    // plan_with with decorrelate=false should still cache.
    #[test]
    fn test_plan_with_custom_opts() {
        let cache = PlanCache::new(16);
        let opts = PlannerOptions { decorrelate: false };
        cache
            .plan_with("SELECT * FROM t WHERE id = 99", &opts)
            .unwrap();
        assert_eq!(cache.len(), 1);
    }
}
