//! Virtual table registration and query dispatch for oxisql-embedded.
//!
//! A virtual table is a named, read-only data source backed by a Rust closure
//! that produces rows on demand.  When a `SELECT` query references a registered
//! virtual table name in its `FROM` clause, the interceptor calls the closure
//! and returns the resulting rows — bypassing GlueSQL entirely for that table.
//!
//! # Lifecycle
//!
//! 1. Call [`VirtualTableRegistry::register`] with a name and [`VirtualTableFn`]
//!    on an [`crate::EmbeddedConnection`] that was created with
//!    [`crate::EmbeddedConnection::open_memory`].
//! 2. Issue a `SELECT` query against the registered name.  The interceptor runs
//!    the closure and optionally applies a simple post-scan filter.
//! 3. Call [`VirtualTableRegistry::unregister`] to remove the table.
//!
//! # Limitations
//!
//! - Only `SELECT * FROM name` and `SELECT * FROM name WHERE col = 'val'` patterns
//!   are accelerated.  Complex predicates fall through to the caller.
//! - Virtual tables cannot participate in JOINs via this interceptor.
//! - The registry is stored as a plain field on `EmbeddedConnection` (not inside
//!   an `Arc<RwLock<…>>`), so registrations made on one clone are not visible to
//!   other clones.  This is intentional — the spec requires `&mut self` on the
//!   registration methods.

use std::collections::HashMap;
use std::sync::Arc;

use oxisql_core::{Row, Value};

/// A virtual table provider function: called on every query that touches this
/// table.  The closure must be `Send + Sync` so that the connection can be used
/// from multiple threads.
pub type VirtualTableFn = Arc<dyn Fn() -> Vec<Row> + Send + Sync>;

/// Registry of virtual table providers.
///
/// Each entry maps a lower-cased table name to a [`VirtualTableFn`].
#[derive(Default, Clone)]
pub struct VirtualTableRegistry {
    providers: HashMap<String, VirtualTableFn>,
}

impl VirtualTableRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register (or replace) a virtual table with `name`.
    ///
    /// `name` is stored lower-cased so that SQL identifiers are matched
    /// case-insensitively.
    pub fn register(&mut self, name: &str, f: VirtualTableFn) {
        self.providers.insert(name.to_lowercase(), f);
    }

    /// Remove a virtual table by name.  No-op if `name` is not registered.
    pub fn unregister(&mut self, name: &str) {
        self.providers.remove(&name.to_lowercase());
    }

    /// Look up a virtual table provider by name.
    pub fn get(&self, name: &str) -> Option<&VirtualTableFn> {
        self.providers.get(&name.to_lowercase())
    }

    /// Return all registered virtual table names (lower-cased, sorted).
    pub fn names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.providers.keys().cloned().collect();
        v.sort();
        v
    }

    /// Check whether any registered name is referenced in `sql` as a table.
    ///
    /// Returns the first matching name found by scanning tokens following
    /// `FROM` or `JOIN` keywords.  Matching is case-insensitive.
    ///
    /// Returns `None` if no registered table is referenced.
    pub fn find_referenced(&self, sql: &str) -> Option<String> {
        if self.providers.is_empty() {
            return None;
        }
        let lower = sql.to_lowercase();
        let tokens: Vec<&str> = lower.split_whitespace().collect();
        for (i, tok) in tokens.iter().enumerate() {
            if (*tok == "from" || *tok == "join") && i + 1 < tokens.len() {
                // Strip any trailing punctuation (e.g. comma or semicolon).
                let raw = tokens[i + 1];
                let candidate = raw.trim_matches(|c: char| !c.is_alphanumeric() && c != '_');
                if self.providers.contains_key(candidate) {
                    return Some(candidate.to_string());
                }
            }
        }
        None
    }

    /// Invoke the provider for `name` and apply a best-effort post-scan filter
    /// derived from the `WHERE` clause in `sql`.
    ///
    /// Supports:
    /// - `WHERE col = 'text'`
    /// - `WHERE col = number`
    ///
    /// All other predicates return all rows unfiltered.
    ///
    /// Returns `None` if `name` is not registered.
    pub fn scan_with_filter(&self, name: &str, sql: &str) -> Option<Vec<Row>> {
        let provider = self.get(name)?;
        let rows = provider();
        let filtered = apply_where_filter(rows, sql);
        Some(filtered)
    }
}

// ── Simple WHERE filter ──────────────────────────────────────────────────────

/// Parse a trivial `WHERE col = 'text'` or `WHERE col = number` predicate from
/// `sql` and apply it to `rows`.  Returns all rows when no simple predicate is
/// found or when parsing fails.
fn apply_where_filter(rows: Vec<Row>, sql: &str) -> Vec<Row> {
    let upper = sql.to_uppercase();
    let where_pos = match upper.find(" WHERE ") {
        Some(p) => p,
        None => return rows,
    };

    // Extract the text after WHERE (strip trailing clauses like ORDER BY, LIMIT).
    let after_where = &sql[where_pos + 7..];
    let clause = strip_trailing_clauses(after_where);

    // Attempt to parse `col = 'val'` (string equality).
    if let Some((col, val)) = parse_eq_string(clause) {
        return rows
            .into_iter()
            .filter(|row| {
                row.get(&col)
                    .is_some_and(|v| matches!(v, Value::Text(t) if t.eq_ignore_ascii_case(&val)))
            })
            .collect();
    }

    // Attempt to parse `col = number` (integer equality).
    if let Some((col, num)) = parse_eq_integer(clause) {
        return rows
            .into_iter()
            .filter(|row| {
                row.get(&col)
                    .is_some_and(|v| matches!(v, Value::I64(n) if *n == num))
            })
            .collect();
    }

    // Complex predicate — return all rows; caller can filter further.
    rows
}

/// Strip trailing SQL clauses (`ORDER BY`, `LIMIT`, `GROUP BY`, `;`) from a
/// fragment that starts right after `WHERE`.
fn strip_trailing_clauses(s: &str) -> &str {
    let upper = s.to_uppercase();
    let cut_at = ["ORDER BY", "LIMIT", "GROUP BY", "HAVING", ";"]
        .iter()
        .filter_map(|kw| upper.find(kw))
        .min();
    match cut_at {
        Some(pos) => s[..pos].trim_end(),
        None => s.trim_end(),
    }
}

/// Try to parse `col = 'string'` from a WHERE clause fragment.
fn parse_eq_string(clause: &str) -> Option<(String, String)> {
    // Pattern: <word> = '<content>'
    let eq_pos = clause.find('=')?;
    let col = clause[..eq_pos].trim().to_lowercase();
    if col.is_empty() || col.contains(' ') {
        return None;
    }
    let rhs = clause[eq_pos + 1..].trim();
    if rhs.starts_with('\'') && rhs.ends_with('\'') && rhs.len() >= 2 {
        let val = rhs[1..rhs.len() - 1].to_string();
        return Some((col, val));
    }
    None
}

/// Try to parse `col = integer` from a WHERE clause fragment.
fn parse_eq_integer(clause: &str) -> Option<(String, i64)> {
    let eq_pos = clause.find('=')?;
    let col = clause[..eq_pos].trim().to_lowercase();
    if col.is_empty() || col.contains(' ') {
        return None;
    }
    let rhs = clause[eq_pos + 1..].trim();
    let num: i64 = rhs.parse().ok()?;
    Some((col, num))
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxisql_core::Value;

    fn make_row(id: i64, name: &str) -> Row {
        Row::new(
            vec!["id".into(), "name".into()],
            vec![Value::I64(id), Value::Text(name.into())],
        )
    }

    #[test]
    fn registry_register_and_get() {
        let mut reg = VirtualTableRegistry::new();
        let rows = vec![make_row(1, "Alice")];
        let rows_clone = rows.clone();
        reg.register("my_table", Arc::new(move || rows_clone.clone()));
        assert!(reg.get("my_table").is_some());
        assert!(reg.get("MY_TABLE").is_some(), "case-insensitive lookup");
    }

    #[test]
    fn registry_unregister() {
        let mut reg = VirtualTableRegistry::new();
        reg.register("t", Arc::new(Vec::new));
        reg.unregister("t");
        assert!(reg.get("t").is_none());
    }

    #[test]
    fn registry_names_sorted() {
        let mut reg = VirtualTableRegistry::new();
        reg.register("zzz", Arc::new(Vec::new));
        reg.register("aaa", Arc::new(Vec::new));
        assert_eq!(reg.names(), vec!["aaa", "zzz"]);
    }

    #[test]
    fn find_referenced_from_clause() {
        let mut reg = VirtualTableRegistry::new();
        reg.register("csv_data", Arc::new(Vec::new));
        let name = reg.find_referenced("SELECT * FROM csv_data WHERE id = 1");
        assert_eq!(name.as_deref(), Some("csv_data"));
    }

    #[test]
    fn find_referenced_case_insensitive() {
        let mut reg = VirtualTableRegistry::new();
        reg.register("csv_data", Arc::new(Vec::new));
        let name = reg.find_referenced("SELECT * FROM CSV_DATA");
        assert_eq!(name.as_deref(), Some("csv_data"));
    }

    #[test]
    fn find_referenced_unregistered() {
        let reg = VirtualTableRegistry::new();
        assert!(reg.find_referenced("SELECT * FROM other_table").is_none());
    }

    #[test]
    fn scan_with_filter_string_eq() {
        let mut reg = VirtualTableRegistry::new();
        let rows = vec![make_row(1, "Alice"), make_row(2, "Bob")];
        reg.register("t", Arc::new(move || rows.clone()));
        let result = reg
            .scan_with_filter("t", "SELECT * FROM t WHERE name = 'Alice'")
            .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].get("id"), Some(&Value::I64(1)));
    }

    #[test]
    fn scan_with_filter_integer_eq() {
        let mut reg = VirtualTableRegistry::new();
        let rows = vec![make_row(1, "Alice"), make_row(2, "Bob")];
        reg.register("t", Arc::new(move || rows.clone()));
        let result = reg
            .scan_with_filter("t", "SELECT * FROM t WHERE id = 2")
            .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].get("name"), Some(&Value::Text("Bob".into())));
    }

    #[test]
    fn scan_with_filter_no_where_returns_all() {
        let mut reg = VirtualTableRegistry::new();
        let rows = vec![make_row(1, "Alice"), make_row(2, "Bob")];
        reg.register("t", Arc::new(move || rows.clone()));
        let result = reg.scan_with_filter("t", "SELECT * FROM t").unwrap();
        assert_eq!(result.len(), 2);
    }
}
