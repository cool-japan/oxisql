//! Standalone inverted-index full-text search for oxisql-embedded.
//!
//! Intercepts `CREATE VIRTUAL TABLE t USING fts5(col1, col2)` /
//! `USING fts4(...)` SQL, `INSERT INTO fts_table VALUES (row_id, text_val)`
//! writes, and `SELECT rowid FROM t WHERE t MATCH 'term'` queries.
//!
//! GlueSQL `MemoryStorage` has no FTS support.  This module maintains a
//! per-connection in-memory inverted index that is updated on every FTS INSERT
//! and queried on every FTS MATCH.  Non-FTS SQL is passed through unchanged.
//!
//! ## SQL patterns recognised
//!
//! | Pattern | Action |
//! |---|---|
//! | `CREATE VIRTUAL TABLE t USING fts5(col1,col2)` | register table + columns |
//! | `CREATE VIRTUAL TABLE t USING fts4(col1)` | same |
//! | `INSERT INTO t VALUES (row_id, 'text…')` | index row |
//! | `SELECT rowid FROM t WHERE t MATCH 'query'` | search |
//!
//! Row IDs must be integer literals in the `VALUES` clause.  Parameterised
//! row IDs (`$1`) are not supported in this version.

use std::collections::{HashMap, HashSet};

use oxisql_core::{OxiSqlError, Row, Value};

// ── FtsIndex ──────────────────────────────────────────────────────────────────

/// A simple in-memory inverted index for full-text search.
///
/// Maintains a per-table mapping from lowercased whitespace-split tokens
/// to the set of row IDs that contain that token.  Operations are O(k·t)
/// where k is the number of tokens and t is set size.
#[derive(Debug, Default, Clone)]
pub struct FtsIndex {
    /// table_name → token → set of row_ids
    indexes: HashMap<String, HashMap<String, HashSet<i64>>>,
    /// table_name → ordered list of searchable columns
    tables: HashMap<String, Vec<String>>,
}

impl FtsIndex {
    /// Create a new, empty index.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new FTS virtual table with the given searchable columns.
    ///
    /// Subsequent inserts to `table_name` will be directed into this index.
    /// Calling `register_table` a second time for the same name replaces the
    /// column list and clears all existing indexed data for that table.
    pub fn register_table(&mut self, table_name: &str, columns: Vec<String>) {
        let key = table_name.to_ascii_lowercase();
        self.indexes.insert(key.clone(), HashMap::new());
        self.tables.insert(key, columns);
    }

    /// Index a row: tokenize the concatenated column values and record by
    /// `row_id`.
    ///
    /// `column_values` should contain one entry per column that was registered
    /// via [`register_table`][Self::register_table], in the same order.  Extra
    /// or missing values are ignored gracefully.
    pub fn index_row(&mut self, table_name: &str, row_id: i64, column_values: &[&str]) {
        let key = table_name.to_ascii_lowercase();
        let entry = self.indexes.entry(key).or_default();
        for text in column_values {
            for token in tokenize(text) {
                entry.entry(token).or_default().insert(row_id);
            }
        }
    }

    /// Remove all indexed entries for the given row.
    ///
    /// After this call no token set for `table_name` will contain `row_id`.
    /// Empty token sets are left in place (they consume negligible memory and
    /// avoiding the scan is cheaper than pruning).
    ///
    /// This method is part of the public FTS API and is available for callers
    /// that maintain their own row lifecycle (e.g., custom DELETE handlers).
    #[allow(dead_code)]
    pub fn delete_row(&mut self, table_name: &str, row_id: i64) {
        let key = table_name.to_ascii_lowercase();
        if let Some(token_map) = self.indexes.get_mut(&key) {
            for set in token_map.values_mut() {
                set.remove(&row_id);
            }
        }
    }

    /// Search: return row IDs that contain **all** of the query terms.
    ///
    /// Query terms are produced by the same [`tokenize`] function used during
    /// indexing.  An empty query returns all row IDs that appear in any token
    /// set.  Returns an empty `Vec` if the table is not registered.
    pub fn search(&self, table_name: &str, query: &str) -> Vec<i64> {
        let key = table_name.to_ascii_lowercase();
        let token_map = match self.indexes.get(&key) {
            Some(m) => m,
            None => return Vec::new(),
        };

        let terms: Vec<String> = tokenize(query);
        if terms.is_empty() {
            // Return all indexed row IDs.
            let mut all: HashSet<i64> = HashSet::new();
            for set in token_map.values() {
                all.extend(set);
            }
            let mut result: Vec<i64> = all.into_iter().collect();
            result.sort_unstable();
            return result;
        }

        // Intersect posting sets for all terms (AND semantics).
        let mut iter = terms.iter();
        let first_term = iter.next().expect("checked non-empty above");
        let initial: HashSet<i64> = token_map.get(first_term).cloned().unwrap_or_default();

        let result_set = iter.fold(initial, |acc, term| match token_map.get(term) {
            Some(posting) => acc.intersection(posting).copied().collect(),
            None => HashSet::new(),
        });

        let mut result: Vec<i64> = result_set.into_iter().collect();
        result.sort_unstable();
        result
    }

    /// Check whether `table_name` is registered as an FTS virtual table.
    pub fn is_fts_table(&self, table_name: &str) -> bool {
        self.tables.contains_key(&table_name.to_ascii_lowercase())
    }

    /// Return the number of registered FTS tables.
    ///
    /// Intended for diagnostics (e.g., `Debug` formatting of the connection).
    pub fn tables_len(&self) -> usize {
        self.tables.len()
    }
}

/// Tokenize a string: lowercase, split on whitespace and common punctuation.
///
/// Returns only non-empty tokens.  The same function is used for both
/// indexing and query tokenization so that token equality is guaranteed.
pub fn tokenize(s: &str) -> Vec<String> {
    s.split(|c: char| c.is_whitespace() || c.is_ascii_punctuation())
        .map(|t| t.to_ascii_lowercase())
        .filter(|t| !t.is_empty())
        .collect()
}

// ── SQL interception helpers ──────────────────────────────────────────────────

/// Try to detect and handle a `CREATE VIRTUAL TABLE … USING fts5/fts4(…)`.
///
/// Returns `Some(Ok(0))` when the statement was recognised and registered,
/// `None` when the SQL is not a virtual-table creation.
pub fn handle_create_virtual_table(
    sql: &str,
    fts: &mut FtsIndex,
) -> Option<Result<u64, OxiSqlError>> {
    // Normalise: collapse whitespace, trim trailing semicolon.
    let upper = sql.trim().trim_end_matches(';').to_ascii_uppercase();
    if !upper.starts_with("CREATE VIRTUAL TABLE") {
        return None;
    }

    // Detect USING fts4 / fts5.
    let (using_pos, engine_end) = if let Some(p) = upper.find("USING FTS5") {
        (p, p + "USING FTS5".len())
    } else if let Some(p) = upper.find("USING FTS4") {
        (p, p + "USING FTS4".len())
    } else {
        return None;
    };

    // Extract the table name: token between "TABLE" and "USING".
    let after_table = &upper["CREATE VIRTUAL TABLE".len()..using_pos];
    let table_name_raw = &sql.trim().trim_end_matches(';')["CREATE VIRTUAL TABLE".len()..using_pos];
    let _ = after_table; // we only needed it for length calculation above
    let table_name = table_name_raw.trim().to_string();

    if table_name.is_empty() {
        return Some(Err(OxiSqlError::Parse(
            "CREATE VIRTUAL TABLE: missing table name".into(),
        )));
    }

    // Extract column list inside parentheses after USING fts5(...).
    let rest_upper = &upper[engine_end..];
    let paren_open = match rest_upper.find('(') {
        Some(p) => engine_end + p,
        None => {
            return Some(Err(OxiSqlError::Parse(
                "CREATE VIRTUAL TABLE … USING fts5: missing '('".into(),
            )));
        }
    };
    let paren_close = match upper[paren_open..].find(')') {
        Some(p) => paren_open + p,
        None => {
            return Some(Err(OxiSqlError::Parse(
                "CREATE VIRTUAL TABLE … USING fts5: missing ')'".into(),
            )));
        }
    };

    let cols_str = &sql.trim().trim_end_matches(';')[paren_open + 1..paren_close];
    let columns: Vec<String> = cols_str
        .split(',')
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
        .collect();

    fts.register_table(&table_name, columns);
    Some(Ok(0))
}

/// Try to detect and handle `INSERT INTO fts_table VALUES (row_id, 'text…')`.
///
/// Extracts the row ID (integer) and text values, calls `fts.index_row`, then
/// returns `Some(Ok(1))`.  Returns `None` when the table is not an FTS table.
///
/// `params_resolved_sql` must already have `$N` placeholders substituted with
/// their literal values before this function is called.
pub fn handle_fts_insert(sql: &str, fts: &mut FtsIndex) -> Option<Result<u64, OxiSqlError>> {
    let trimmed = sql.trim().trim_end_matches(';');
    let upper = trimmed.to_ascii_uppercase();

    if !upper.starts_with("INSERT INTO ") {
        return None;
    }

    // Extract table name: first token after "INSERT INTO ".
    let after_into = trimmed["INSERT INTO ".len()..].trim_start();
    let table_name = after_into
        .split_ascii_whitespace()
        .next()
        .unwrap_or("")
        .to_string();

    if !fts.is_fts_table(&table_name) {
        return None;
    }

    // Locate VALUES clause.
    let values_pos = upper.find("VALUES")?;

    let after_values = trimmed[values_pos + "VALUES".len()..].trim_start();
    let paren_open = after_values.find('(')?;
    let inner_start = paren_open + 1;
    // Walk to the matching close paren, skipping string literals.
    let after_open = &after_values[inner_start..];
    let paren_close = find_closing_paren(after_open)?;

    let values_inner = &after_values[inner_start..inner_start + paren_close];
    let fields = split_values_csv(values_inner);

    if fields.is_empty() {
        return None;
    }

    // First field is the row ID — must be a bare integer literal.
    let row_id: i64 = match fields[0].trim().parse() {
        Ok(n) => n,
        Err(_) => {
            // Row ID not parseable as i64 — could be a type we don't handle.
            return Some(Err(OxiSqlError::Parse(format!(
                "FTS INSERT: row_id must be an integer literal, got '{}'",
                fields[0].trim()
            ))));
        }
    };

    // Remaining fields are the text values for the registered columns.
    let text_values: Vec<String> = fields[1..]
        .iter()
        .map(|f| strip_string_literal(f.trim()))
        .collect();
    let text_refs: Vec<&str> = text_values.iter().map(String::as_str).collect();

    fts.index_row(&table_name, row_id, &text_refs);
    Some(Ok(1))
}

/// Try to detect and handle `SELECT rowid FROM t WHERE t MATCH 'query'`.
///
/// Returns matching rows as `Row`s with a single `rowid` column of type
/// `Value::I64`.  Returns `None` when the query does not match the pattern
/// or the table is not an FTS table.
pub fn handle_fts_match(sql: &str, fts: &FtsIndex) -> Option<Result<Vec<Row>, OxiSqlError>> {
    let trimmed = sql.trim().trim_end_matches(';');
    let upper = trimmed.to_ascii_uppercase();

    // Must be a SELECT … MATCH … query.
    if !upper.starts_with("SELECT") || !upper.contains(" MATCH ") {
        return None;
    }

    // Extract FROM table name.
    let from_pos = upper.find(" FROM ")?;
    let after_from = trimmed[from_pos + " FROM ".len()..].trim_start();
    let table_name = after_from
        .split(|c: char| c.is_whitespace() || c == ',' || c == ')')
        .next()
        .unwrap_or("")
        .to_string();

    if !fts.is_fts_table(&table_name) {
        return None;
    }

    // Extract the match query string from `WHERE t MATCH 'query'`.
    let match_pos = upper.find(" MATCH ")?;
    let after_match = trimmed[match_pos + " MATCH ".len()..].trim_start();
    let match_query = strip_string_literal(after_match.trim_end_matches(';').trim());

    let row_ids = fts.search(&table_name, &match_query);
    let rows: Vec<Row> = row_ids
        .into_iter()
        .map(|id| Row::new(vec!["rowid".to_string()], vec![Value::I64(id)]))
        .collect();

    Some(Ok(rows))
}

// ── private parsing helpers ───────────────────────────────────────────────────

/// Find the index of the first `)` at depth 0, skipping string literals.
///
/// `s` is the content *after* the opening `(`.
fn find_closing_paren(s: &str) -> Option<usize> {
    let mut depth: usize = 1;
    let mut in_single = false;
    let mut prev_char = '\0';
    for (i, c) in s.char_indices() {
        if in_single {
            if c == '\'' && prev_char != '\\' {
                in_single = false;
            }
        } else {
            match c {
                '\'' => in_single = true,
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
        }
        prev_char = c;
    }
    None
}

/// Split a `VALUES` inner string by commas, respecting single-quoted strings.
///
/// Each returned element is trimmed but may contain surrounding quotes if the
/// value was a string literal.
fn split_values_csv(s: &str) -> Vec<String> {
    let mut result: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut prev_char = '\0';

    for c in s.chars() {
        if in_single {
            current.push(c);
            if c == '\'' && prev_char != '\\' {
                in_single = false;
            }
        } else if c == '\'' {
            in_single = true;
            current.push(c);
        } else if c == ',' {
            result.push(current.trim().to_string());
            current = String::new();
        } else {
            current.push(c);
        }
        prev_char = c;
    }
    if !current.trim().is_empty() {
        result.push(current.trim().to_string());
    }
    result
}

/// Strip outer single quotes from a SQL string literal.
///
/// `'hello world'` → `hello world`.  If the value is not quoted it is
/// returned unchanged.  Escaped single quotes (`''`) are collapsed to `'`.
fn strip_string_literal(s: &str) -> String {
    let s = s.trim();
    if s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2 {
        s[1..s.len() - 1].replace("''", "'")
    } else {
        s.to_string()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_basic() {
        let tokens = tokenize("Hello, World!");
        assert!(tokens.contains(&"hello".to_string()));
        assert!(tokens.contains(&"world".to_string()));
    }

    #[test]
    fn tokenize_empty() {
        assert!(tokenize("").is_empty());
    }

    #[test]
    fn fts_index_register_and_search() {
        let mut idx = FtsIndex::new();
        idx.register_table("docs", vec!["content".to_string()]);
        idx.index_row("docs", 1, &["hello world"]);
        idx.index_row("docs", 2, &["rust programming"]);
        idx.index_row("docs", 3, &["hello rust"]);

        let res = idx.search("docs", "hello");
        assert_eq!(res, vec![1, 3]);

        let res2 = idx.search("docs", "rust programming");
        assert_eq!(res2, vec![2]);
    }

    #[test]
    fn fts_index_delete_row() {
        let mut idx = FtsIndex::new();
        idx.register_table("t", vec!["body".to_string()]);
        idx.index_row("t", 1, &["foo bar"]);
        idx.index_row("t", 2, &["foo baz"]);

        idx.delete_row("t", 1);
        let res = idx.search("t", "foo");
        assert_eq!(res, vec![2]);
    }

    #[test]
    fn fts_is_fts_table() {
        let mut idx = FtsIndex::new();
        assert!(!idx.is_fts_table("docs"));
        idx.register_table("docs", vec![]);
        assert!(idx.is_fts_table("docs"));
        assert!(idx.is_fts_table("DOCS")); // case-insensitive
    }

    #[test]
    fn parse_create_virtual_table_fts5() {
        let mut idx = FtsIndex::new();
        let r =
            handle_create_virtual_table("CREATE VIRTUAL TABLE docs USING fts5(content)", &mut idx);
        assert!(matches!(r, Some(Ok(0))));
        assert!(idx.is_fts_table("docs"));
    }

    #[test]
    fn parse_create_virtual_table_fts4() {
        let mut idx = FtsIndex::new();
        let r = handle_create_virtual_table(
            "CREATE VIRTUAL TABLE notes USING fts4(title, body)",
            &mut idx,
        );
        assert!(matches!(r, Some(Ok(0))));
        assert!(idx.is_fts_table("notes"));
    }

    #[test]
    fn parse_non_virtual_table_returns_none() {
        let mut idx = FtsIndex::new();
        let r = handle_create_virtual_table("CREATE TABLE t (id INT)", &mut idx);
        assert!(r.is_none());
    }

    #[test]
    fn parse_fts_insert_and_match() {
        let mut idx = FtsIndex::new();
        idx.register_table("docs", vec!["content".to_string()]);

        let r = handle_fts_insert("INSERT INTO docs VALUES (1, 'hello world')", &mut idx);
        assert!(matches!(r, Some(Ok(1))));

        let r2 = handle_fts_match("SELECT rowid FROM docs WHERE docs MATCH 'hello'", &idx);
        match r2 {
            Some(Ok(rows)) => assert_eq!(rows.len(), 1),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parse_fts_match_returns_none_for_non_fts_table() {
        let idx = FtsIndex::new();
        let r = handle_fts_match(
            "SELECT rowid FROM regular_table WHERE regular_table MATCH 'x'",
            &idx,
        );
        assert!(r.is_none());
    }

    #[test]
    fn split_values_csv_basic() {
        let fields = split_values_csv("1, 'hello world', 'foo'");
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0], "1");
        assert_eq!(fields[1], "'hello world'");
        assert_eq!(fields[2], "'foo'");
    }

    #[test]
    fn strip_string_literal_basic() {
        assert_eq!(strip_string_literal("'hello'"), "hello");
        assert_eq!(strip_string_literal("hello"), "hello");
        assert_eq!(strip_string_literal("'it''s'"), "it's");
    }
}
