//! B-tree secondary index for oxisql-embedded.
//!
//! Provides an in-memory secondary index layer on top of GlueSQL
//! `MemoryStorage`, which performs full linear scans on every query.
//! [`BTreeIndex`] wraps a `std::collections::BTreeMap` keyed on [`IndexKey`]
//! (a comparable wrapper around [`oxisql_core::Value`]) and maps each key to
//! the set of row IDs that have that value.
//!
//! # Row ID strategy
//!
//! GlueSQL does not expose stable primary-key row IDs through its public API.
//! This implementation synthesises row IDs as a monotonic counter per table.
//! When a row is indexed (after a successful INSERT), the counter is incremented
//! and the new ID is associated with the provided column value.  On DELETE the
//! caller must supply the same row ID (stored externally or by re-scanning).
//!
//! Because the IDs are synthetic, the B-tree is most useful as an equality
//! fast-path that avoids GlueSQL entirely when the virtual-table path is also
//! active.  For GlueSQL-backed tables the index is maintained on a best-effort
//! basis; queries without a matching index fall through to GlueSQL's linear
//! scan.
//!
//! # `CREATE INDEX` / `DROP INDEX`
//!
//! The [`IndexRegistry`] intercepts these SQL statements in
//! `EmbeddedConnection::execute` *before* forwarding to GlueSQL,
//! which does not support that syntax natively.

use oxisql_core::Value;
use std::collections::{BTreeMap, HashMap, HashSet};

// ── IndexKey ─────────────────────────────────────────────────────────────────

/// A totally-ordered key wrapping [`Value`] for use inside `BTreeMap`.
///
/// Ordering rules:
/// - `Null < Integer < Text` (distinct variants are ordered by kind).
/// - Within `Integer`, ordered by numeric value.
/// - Within `Text`, ordered lexicographically.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IndexKey {
    /// Represents a SQL `NULL` value.
    Null,
    /// Represents an integer (i64) value.
    Integer(i64),
    /// Represents a UTF-8 text value.
    Text(String),
}

impl IndexKey {
    /// Convert an [`oxisql_core::Value`] into an [`IndexKey`].
    ///
    /// Variants that cannot be represented (blobs, decimals, etc.) fall back
    /// to [`IndexKey::Text`] via `Display`.
    pub fn from_value(v: &Value) -> Self {
        match v {
            Value::Null => IndexKey::Null,
            Value::I64(n) => IndexKey::Integer(*n),
            Value::F64(_) => {
                // Store floats as text for ordering purposes — exact equality
                // lookups still work; range queries may have edge-case precision
                // issues, which is acceptable for a best-effort index.
                IndexKey::Text(format!("{v:?}"))
            }
            Value::Text(s) => IndexKey::Text(s.clone()),
            Value::Bool(b) => IndexKey::Integer(i64::from(*b)),
            _ => IndexKey::Text(format!("{v:?}")),
        }
    }
}

// ── BTreeIndex ────────────────────────────────────────────────────────────────

/// A single B-tree secondary index on one column of one table.
///
/// Maps `IndexKey → HashSet<row_id>` so that multiple rows can share the same
/// column value (non-unique index).
#[derive(Debug, Default, Clone)]
pub struct BTreeIndex {
    /// Maps column value → set of row IDs with that value.
    map: BTreeMap<IndexKey, HashSet<i64>>,
}

impl BTreeIndex {
    /// Create an empty index.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that `row_id` has `value` in the indexed column.
    pub fn insert(&mut self, value: IndexKey, row_id: i64) {
        self.map.entry(value).or_default().insert(row_id);
    }

    /// Remove `row_id` from the entry for `value`.
    /// If no rows remain for that value, the entry is dropped.
    pub fn delete(&mut self, value: &IndexKey, row_id: i64) {
        if let Some(ids) = self.map.get_mut(value) {
            ids.remove(&row_id);
            if ids.is_empty() {
                self.map.remove(value);
            }
        }
    }

    /// Return all row IDs that exactly match `key`.
    pub fn lookup_eq(&self, key: &IndexKey) -> HashSet<i64> {
        self.map.get(key).cloned().unwrap_or_default()
    }

    /// Return all row IDs with keys in the range `[from, to)`.
    ///
    /// `from` is inclusive (or unbounded when `None`); `to` is exclusive (or
    /// unbounded when `None`).
    pub fn lookup_range(&self, from: Option<&IndexKey>, to: Option<&IndexKey>) -> HashSet<i64> {
        use std::ops::Bound;
        let lower = match from {
            Some(k) => Bound::Included(k.clone()),
            None => Bound::Unbounded,
        };
        let upper = match to {
            Some(k) => Bound::Excluded(k.clone()),
            None => Bound::Unbounded,
        };
        self.map
            .range((lower, upper))
            .flat_map(|(_, ids)| ids.iter().copied())
            .collect()
    }

    /// Return the total number of indexed entries (not unique values).
    #[cfg(test)]
    pub fn entry_count(&self) -> usize {
        self.map.values().map(|s| s.len()).sum()
    }
}

// ── IndexRegistry ─────────────────────────────────────────────────────────────

/// Metadata for a named B-tree index stored in [`IndexRegistry`].
#[derive(Debug, Clone)]
pub struct NamedIndex {
    /// User-visible index name (e.g. `idx_name`).
    pub index_name: String,
    /// Column the index was created on.
    pub column: String,
}

/// Registry of B-tree indexes across all tables.
///
/// Maintains a two-level map: `table_name → column_name → BTreeIndex`.
/// Also tracks a per-table monotonic row-ID counter used when new rows are
/// indexed via [`IndexRegistry::index_row`].
/// Additionally, `named_indexes` maps `table_name → Vec<NamedIndex>` so that
/// `EmbeddedConnection::indexes` can return human-readable index metadata.
#[derive(Debug, Default, Clone)]
pub struct IndexRegistry {
    /// `table_name → column_name → BTreeIndex`
    indexes: HashMap<String, HashMap<String, BTreeIndex>>,
    /// Per-table monotonic counter for synthetic row IDs.
    row_counters: HashMap<String, i64>,
    /// `table_name (lowercase) → Vec<NamedIndex>` for introspection.
    named_indexes: HashMap<String, Vec<NamedIndex>>,
}

impl IndexRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a new (empty) B-tree index on `(table, column)`.
    ///
    /// Idempotent: calling twice does not clear an existing index.
    pub fn create_index(&mut self, table: &str, column: &str) {
        let tbl = self.indexes.entry(table.to_lowercase()).or_default();
        tbl.entry(column.to_lowercase()).or_default();
    }

    /// Create a new (empty) B-tree index on `(table, column)` with a given
    /// index name.
    ///
    /// Stores the `index_name` in `named_indexes` for schema introspection in
    /// addition to creating the B-tree data structure.  Idempotent on the
    /// data structure; duplicate names are not re-added to `named_indexes`.
    pub fn create_named_index(&mut self, index_name: &str, table: &str, column: &str) {
        let tbl_key = table.to_lowercase();
        let col_key = column.to_lowercase();
        // Create the B-tree entry.
        self.indexes
            .entry(tbl_key.clone())
            .or_default()
            .entry(col_key.clone())
            .or_default();
        // Record the name for introspection (avoid duplicates).
        let names = self.named_indexes.entry(tbl_key).or_default();
        if !names.iter().any(|n| n.index_name == index_name) {
            names.push(NamedIndex {
                index_name: index_name.to_owned(),
                column: col_key,
            });
        }
    }

    /// Return all named indexes on `table`, in registration order.
    ///
    /// Returns an empty slice when no named indexes exist for the table.
    pub fn named_indexes_for_table(&self, table: &str) -> &[NamedIndex] {
        self.named_indexes
            .get(&table.to_lowercase())
            .map(|v| v.as_slice())
            .unwrap_or_default()
    }

    /// Remove the B-tree index on `(table, column)` if it exists.
    pub fn drop_index(&mut self, table: &str, column: &str) {
        let tbl_key = table.to_lowercase();
        let col_key = column.to_lowercase();
        if let Some(tbl) = self.indexes.get_mut(&tbl_key) {
            tbl.remove(&col_key);
        }
        // Also remove from named_indexes if present.
        if let Some(names) = self.named_indexes.get_mut(&tbl_key) {
            names.retain(|n| n.column != col_key);
        }
    }

    /// Return `true` if an index exists on `(table, column)`.
    pub fn has_index(&self, table: &str, column: &str) -> bool {
        self.indexes
            .get(&table.to_lowercase())
            .is_some_and(|t| t.contains_key(&column.to_lowercase()))
    }

    /// Record that a row was inserted into `table` with `value` in `column`.
    ///
    /// Increments the per-table row counter and inserts the new ID into the
    /// index (if one exists for that `(table, column)` pair).  Returns the
    /// synthesised row ID so callers can store it for later deletion.
    ///
    /// If no index exists for `column`, this is a no-op and returns `None`.
    pub fn index_row(&mut self, table: &str, column: &str, value: IndexKey) -> Option<i64> {
        let tbl_key = table.to_lowercase();
        let col_key = column.to_lowercase();
        let idx = self.indexes.get_mut(&tbl_key)?.get_mut(&col_key)?;
        let counter = self.row_counters.entry(tbl_key).or_insert(0);
        *counter += 1;
        let row_id = *counter;
        idx.insert(value, row_id);
        Some(row_id)
    }

    /// Remove all index entries that refer to `row_id` in `table`.
    ///
    /// This scans all indexed columns for the table, which is acceptable given
    /// the expected small number of indexes per table.
    pub fn delete_row(&mut self, table: &str, row_id: i64, column: &str, value: &IndexKey) {
        let tbl_key = table.to_lowercase();
        let col_key = column.to_lowercase();
        if let Some(tbl) = self.indexes.get_mut(&tbl_key) {
            if let Some(idx) = tbl.get_mut(&col_key) {
                idx.delete(value, row_id);
            }
        }
    }

    /// Look up row IDs that exactly match `key` in `(table, column)`.
    ///
    /// Returns `None` if no index exists; returns an empty set when the index
    /// exists but has no matching entries.
    pub fn lookup(&self, table: &str, column: &str, key: &IndexKey) -> Option<HashSet<i64>> {
        let idx = self
            .indexes
            .get(&table.to_lowercase())?
            .get(&column.to_lowercase())?;
        Some(idx.lookup_eq(key))
    }

    /// Look up row IDs in `(table, column)` for the given key range.
    ///
    /// Returns `None` if no index exists for that `(table, column)` pair.
    pub fn lookup_range(
        &self,
        table: &str,
        column: &str,
        from: Option<&IndexKey>,
        to: Option<&IndexKey>,
    ) -> Option<HashSet<i64>> {
        let idx = self
            .indexes
            .get(&table.to_lowercase())?
            .get(&column.to_lowercase())?;
        Some(idx.lookup_range(from, to))
    }
}

// ── CREATE / DROP INDEX SQL parsing ──────────────────────────────────────────

/// Parse a `CREATE [UNIQUE] INDEX [IF NOT EXISTS] name ON table(col)` statement.
///
/// Returns `(index_name, table, column)` on success, or `None` if the SQL does
/// not match the expected pattern.
pub fn parse_create_index(sql: &str) -> Option<(String, String, String)> {
    let upper = sql.to_uppercase();
    let trimmed = upper.trim_start();
    let is_unique = trimmed.starts_with("CREATE UNIQUE INDEX");
    if !is_unique && !trimmed.starts_with("CREATE INDEX") {
        return None;
    }

    // Extract index name: text between the INDEX keyword and the ON keyword.
    // `index_kw_pos` is the byte offset (in the original `sql`) right after
    // "CREATE INDEX" / "CREATE UNIQUE INDEX".
    let index_kw_pos = if is_unique {
        upper.find("UNIQUE INDEX")? + "UNIQUE INDEX".len()
    } else {
        upper.find("CREATE INDEX")? + "CREATE INDEX".len()
    };
    // Find " ON " in the uppercased copy so we know where the index name ends.
    let on_pos = upper.find(" ON ")?;
    // The index name lives between index_kw_pos and on_pos in the original sql.
    let index_name = sql[index_kw_pos..on_pos].trim().to_owned();

    // After ON: table name then (column).
    let after_on = sql[on_pos + 4..].trim_start();
    let paren_pos = after_on.find('(')?;
    let table = after_on[..paren_pos].trim().to_lowercase();

    let close_paren = after_on.find(')')?;
    let cols_str = &after_on[paren_pos + 1..close_paren];
    let column = cols_str.split(',').next()?.trim().to_lowercase();

    if index_name.is_empty() || table.is_empty() || column.is_empty() {
        return None;
    }
    Some((index_name, table, column))
}

/// Parse a `DROP INDEX [IF EXISTS] name` or `DROP INDEX name ON table` statement.
///
/// Returns `(table, column)` extracted from the canonical `ON table(col)` form,
/// or `None` if the SQL does not match.
///
/// Because this implementation stores indexes by `(table, column)` we need the
/// `ON table(col)` form.  Plain `DROP INDEX name` is not supported.
pub fn parse_drop_index(sql: &str) -> Option<(String, String)> {
    let upper = sql.to_uppercase();
    let trimmed = upper.trim_start();
    if !trimmed.starts_with("DROP INDEX") {
        return None;
    }
    // Require the ON table(col) form.
    let on_pos = upper.find(" ON ")?;
    let after_on = sql[on_pos + 4..].trim_start();
    let paren_pos = after_on.find('(')?;
    let table = after_on[..paren_pos].trim().to_lowercase();
    let close_paren = after_on.find(')')?;
    let cols_str = &after_on[paren_pos + 1..close_paren];
    let column = cols_str.split(',').next()?.trim().to_lowercase();
    if table.is_empty() || column.is_empty() {
        return None;
    }
    Some((table, column))
}

// ── INSERT value extraction ──────────────────────────────────────────────────

/// A very lightweight representation of a parsed INSERT column→value mapping.
///
/// Extracts data from `INSERT INTO table (col1, col2) VALUES (v1, v2)` to
/// allow the index to be updated after a successful INSERT.
pub struct InsertInfo {
    /// Lower-cased table name.
    pub table: String,
    /// Ordered (column_name, value_string) pairs.
    pub pairs: Vec<(String, String)>,
}

/// Try to extract table name and column→value pairs from an INSERT statement.
///
/// Returns `None` when the statement does not match the expected pattern
/// `INSERT INTO table (cols) VALUES (vals)`.
pub fn parse_insert_values(sql: &str) -> Option<InsertInfo> {
    let upper = sql.to_uppercase();
    let trimmed = upper.trim_start();
    if !trimmed.starts_with("INSERT INTO") {
        return None;
    }

    // Extract table name: text between "INTO " and the next whitespace or '('.
    let after_into = sql[upper.find("INTO ")? + 5..].trim_start();
    let table_end = after_into
        .find(|c: char| c.is_whitespace() || c == '(')
        .unwrap_or(after_into.len());
    let table = after_into[..table_end].trim().to_lowercase();

    // Find column list in parentheses.
    let col_start = upper.find('(')?;
    let col_end = upper[col_start..].find(')')? + col_start;
    let col_str = &sql[col_start + 1..col_end];

    // Find VALUES keyword.
    let values_pos = upper[col_end..].find("VALUES")? + col_end;
    let after_values = &sql[values_pos + 6..].trim_start();
    let val_start = after_values.find('(')?;
    let val_end = after_values[val_start..].find(')')? + val_start;
    let val_str = &after_values[val_start + 1..val_end];

    let columns: Vec<String> = col_str
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .collect();
    let values: Vec<String> = split_values(val_str);

    if columns.len() != values.len() || columns.is_empty() {
        return None;
    }

    let pairs = columns.into_iter().zip(values).collect();
    Some(InsertInfo { table, pairs })
}

/// Split a comma-separated VALUES list, respecting single-quoted strings.
fn split_values(s: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    for ch in s.chars() {
        match ch {
            '\'' => {
                in_quotes = !in_quotes;
                current.push(ch);
            }
            ',' if !in_quotes => {
                values.push(current.trim().to_string());
                current = String::new();
            }
            _ => current.push(ch),
        }
    }
    if !current.trim().is_empty() {
        values.push(current.trim().to_string());
    }
    values
}

/// Convert a raw SQL value string (e.g. `'hello'`, `42`, `NULL`) into an
/// [`IndexKey`].
pub fn sql_literal_to_index_key(s: &str) -> IndexKey {
    let trimmed = s.trim();
    if trimmed.eq_ignore_ascii_case("null") {
        return IndexKey::Null;
    }
    if trimmed.starts_with('\'') && trimmed.ends_with('\'') && trimmed.len() >= 2 {
        return IndexKey::Text(trimmed[1..trimmed.len() - 1].to_string());
    }
    if let Ok(n) = trimmed.parse::<i64>() {
        return IndexKey::Integer(n);
    }
    // Fallback: store raw string.
    IndexKey::Text(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_key_ordering() {
        let null = IndexKey::Null;
        let int1 = IndexKey::Integer(1);
        let int2 = IndexKey::Integer(10);
        let text = IndexKey::Text("a".into());
        assert!(null < int1);
        assert!(int1 < int2);
        assert!(int2 < text);
    }

    #[test]
    fn btree_index_insert_lookup_eq() {
        let mut idx = BTreeIndex::new();
        idx.insert(IndexKey::Integer(42), 1);
        idx.insert(IndexKey::Integer(42), 2);
        idx.insert(IndexKey::Integer(99), 3);
        let result = idx.lookup_eq(&IndexKey::Integer(42));
        assert!(result.contains(&1));
        assert!(result.contains(&2));
        assert!(!result.contains(&3));
    }

    #[test]
    fn btree_index_delete() {
        let mut idx = BTreeIndex::new();
        idx.insert(IndexKey::Integer(1), 10);
        idx.delete(&IndexKey::Integer(1), 10);
        assert!(idx.lookup_eq(&IndexKey::Integer(1)).is_empty());
        assert_eq!(idx.entry_count(), 0);
    }

    #[test]
    fn btree_index_range() {
        let mut idx = BTreeIndex::new();
        for i in 0_i64..10 {
            idx.insert(IndexKey::Integer(i), i);
        }
        // Range [3, 6)
        let result = idx.lookup_range(Some(&IndexKey::Integer(3)), Some(&IndexKey::Integer(6)));
        assert_eq!(result, [3, 4, 5].iter().copied().collect::<HashSet<_>>());
    }

    #[test]
    fn registry_create_has_drop() {
        let mut reg = IndexRegistry::new();
        reg.create_index("users", "age");
        assert!(reg.has_index("users", "age"));
        assert!(reg.has_index("USERS", "AGE"), "case-insensitive");
        reg.drop_index("users", "age");
        assert!(!reg.has_index("users", "age"));
    }

    #[test]
    fn registry_index_row_lookup() {
        let mut reg = IndexRegistry::new();
        reg.create_index("people", "age");
        reg.index_row("people", "age", IndexKey::Integer(30));
        reg.index_row("people", "age", IndexKey::Integer(25));
        reg.index_row("people", "age", IndexKey::Integer(30));

        let ids = reg.lookup("people", "age", &IndexKey::Integer(30)).unwrap();
        assert_eq!(ids.len(), 2, "two rows with age=30");

        let ids25 = reg.lookup("people", "age", &IndexKey::Integer(25)).unwrap();
        assert_eq!(ids25.len(), 1, "one row with age=25");
    }

    #[test]
    fn registry_no_index_returns_none() {
        let reg = IndexRegistry::new();
        assert!(reg.lookup("t", "col", &IndexKey::Integer(1)).is_none());
    }

    #[test]
    fn parse_create_index_basic() {
        let result = parse_create_index("CREATE INDEX idx_age ON people(age)");
        assert_eq!(
            result,
            Some(("idx_age".into(), "people".into(), "age".into()))
        );
    }

    #[test]
    fn parse_create_index_unique() {
        let result = parse_create_index("CREATE UNIQUE INDEX idx ON orders(order_id)");
        assert_eq!(
            result,
            Some(("idx".into(), "orders".into(), "order_id".into()))
        );
    }

    #[test]
    fn parse_drop_index_on_form() {
        let result = parse_drop_index("DROP INDEX idx_age ON people(age)");
        assert_eq!(result, Some(("people".into(), "age".into())));
    }

    #[test]
    fn parse_insert_values_basic() {
        let info = parse_insert_values("INSERT INTO users (id, name) VALUES (1, 'Alice')").unwrap();
        assert_eq!(info.table, "users");
        assert_eq!(
            info.pairs,
            vec![
                ("id".to_string(), "1".to_string()),
                ("name".to_string(), "'Alice'".to_string()),
            ]
        );
    }

    #[test]
    fn sql_literal_conversions() {
        assert_eq!(sql_literal_to_index_key("NULL"), IndexKey::Null);
        assert_eq!(sql_literal_to_index_key("42"), IndexKey::Integer(42));
        assert_eq!(
            sql_literal_to_index_key("'hello'"),
            IndexKey::Text("hello".into())
        );
    }
}
