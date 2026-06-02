//! SQL statement analysis utilities: table extraction, column extraction,
//! parameter counting, and SQL normalization.

use sqlparser::ast::Statement;

use crate::columns;

// ── Analysis functions ──────────────────────────────────────────────────────

/// Returns `true` if the statement is read-only (does not modify data or
/// schema).
///
/// Read-only statements include `SELECT`, `EXPLAIN`, `SHOW`, and similar.
/// DML (`INSERT`, `UPDATE`, `DELETE`) and DDL (`CREATE`, `DROP`, `ALTER`)
/// are considered non-read-only.
pub fn is_read_only(stmt: &Statement) -> bool {
    matches!(
        stmt,
        Statement::Query(_)
            | Statement::Explain { .. }
            | Statement::ExplainTable { .. }
            | Statement::ShowTables { .. }
            | Statement::ShowColumns { .. }
            | Statement::ShowVariable { .. }
            | Statement::ShowVariables { .. }
            | Statement::ShowCreate { .. }
            | Statement::ShowDatabases { .. }
    )
}

// ── Table extraction ────────────────────────────────────────────────────────

/// Extract table names referenced in a statement using the SQL text.
///
/// This is a practical extraction that re-formats the parsed AST to text and
/// scans for table references.  For `SELECT` statements it inspects the `FROM`
/// and `JOIN` clauses.  For `INSERT`/`UPDATE`/`DELETE` it inspects the target
/// table.
///
/// Returns a `Vec<String>` of table names.  Names are deduplicated.
///
/// # Example
///
/// ```rust
/// use oxisql_parse::{parse_one, extract_tables};
///
/// let stmt = parse_one("SELECT * FROM users JOIN orders ON users.id = orders.user_id").unwrap();
/// let tables = extract_tables(&stmt);
/// assert!(tables.contains(&"users".to_string()));
/// assert!(tables.contains(&"orders".to_string()));
/// ```
pub fn extract_tables(stmt: &Statement) -> Vec<String> {
    let mut tables = Vec::new();

    match stmt {
        Statement::Query(query) => {
            extract_tables_from_query(query, &mut tables);
        }
        Statement::Insert(insert) => {
            tables.push(insert.table.to_string());
            if let Some(ref src) = insert.source {
                if let sqlparser::ast::SetExpr::Select(ref sel) = *src.body {
                    extract_tables_from_select(sel, &mut tables);
                }
            }
        }
        Statement::Update(update) => {
            extract_tables_from_table_with_joins(&update.table, &mut tables);
        }
        Statement::Delete(delete) => match &delete.from {
            sqlparser::ast::FromTable::WithFromKeyword(twjs) => {
                for twj in twjs {
                    extract_tables_from_table_with_joins(twj, &mut tables);
                }
            }
            sqlparser::ast::FromTable::WithoutKeyword(twjs) => {
                for twj in twjs {
                    extract_tables_from_table_with_joins(twj, &mut tables);
                }
            }
        },
        Statement::CreateTable(ct) => {
            tables.push(ct.name.to_string());
        }
        Statement::Drop { names, .. } => {
            for name in names {
                tables.push(name.to_string());
            }
        }
        Statement::AlterTable(alter_table) => {
            tables.push(alter_table.name.to_string());
        }
        _ => {}
    }

    // Deduplicate
    tables.sort();
    tables.dedup();
    tables
}

fn extract_tables_from_query(query: &sqlparser::ast::Query, tables: &mut Vec<String>) {
    if let sqlparser::ast::SetExpr::Select(ref sel) = *query.body {
        extract_tables_from_select(sel, tables);
    }
}

fn extract_tables_from_select(sel: &sqlparser::ast::Select, tables: &mut Vec<String>) {
    for item in &sel.from {
        extract_table_name_from_factor(&item.relation, tables);
        for join in &item.joins {
            extract_table_name_from_factor(&join.relation, tables);
        }
    }
}

fn extract_table_name_from_factor(factor: &sqlparser::ast::TableFactor, tables: &mut Vec<String>) {
    match factor {
        sqlparser::ast::TableFactor::Table { name, .. } => {
            tables.push(name.to_string());
        }
        sqlparser::ast::TableFactor::Derived { subquery, .. } => {
            extract_tables_from_query(subquery, tables);
        }
        sqlparser::ast::TableFactor::NestedJoin {
            table_with_joins, ..
        } => {
            extract_table_name_from_factor(&table_with_joins.relation, tables);
            for join in &table_with_joins.joins {
                extract_table_name_from_factor(&join.relation, tables);
            }
        }
        _ => {}
    }
}

fn extract_tables_from_table_with_joins(
    twj: &sqlparser::ast::TableWithJoins,
    tables: &mut Vec<String>,
) {
    extract_table_name_from_factor(&twj.relation, tables);
    for join in &twj.joins {
        extract_table_name_from_factor(&join.relation, tables);
    }
}

// ── Parameter counting ──────────────────────────────────────────────────────

/// Count the number of positional parameter placeholders in a SQL string.
///
/// Recognises both `$N` (Postgres-style) and `?` (MySQL/SQLite-style)
/// placeholders.  For `$N` parameters, returns the highest N found.
/// For `?` parameters, returns the count.
///
/// Note: this is a simple lexical scan, not an AST analysis.  It does not
/// distinguish placeholders inside string literals.
pub fn count_params(sql: &str) -> usize {
    let mut max_dollar = 0usize;
    let mut question_count = 0usize;

    let bytes = sql.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\'' {
            // Skip string literal
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\'' {
                    if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                        i += 2; // escaped quote
                    } else {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            i += 1; // skip closing quote
        } else if bytes[i] == b'$' {
            // Parse $N parameter
            i += 1;
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if i > start {
                if let Ok(n) = std::str::from_utf8(&bytes[start..i])
                    .unwrap_or("0")
                    .parse::<usize>()
                {
                    if n > max_dollar {
                        max_dollar = n;
                    }
                }
            }
        } else if bytes[i] == b'?' {
            question_count += 1;
            i += 1;
        } else {
            i += 1;
        }
    }

    if max_dollar > 0 {
        max_dollar
    } else {
        question_count
    }
}

// ── Normalization ────────────────────────────────────────────────────────────

/// Normalize a SQL string for use as a cache key.
///
/// Applies:
/// - Collapse any sequence of whitespace (spaces, tabs, newlines) to a
///   single ASCII space.
/// - Trim leading and trailing whitespace.
/// - Uppercase all characters outside of string literals (so `select` and
///   `SELECT` produce the same key).
///
/// This is a lightweight lexical normalization, not a full AST round-trip.
/// It is suitable for prepared-statement cache key generation but does not
/// guarantee two semantically identical queries will produce the same key
/// (e.g. different identifier quoting styles will still differ).
///
/// # Example
///
/// ```rust
/// use oxisql_parse::normalize;
/// assert_eq!(normalize("select  id  from  users"), "SELECT ID FROM USERS");
/// ```
pub fn normalize(sql: &str) -> String {
    let mut result = String::with_capacity(sql.len());
    let mut in_string = false;
    let mut prev_was_space = false;
    let mut chars = sql.chars().peekable();

    while let Some(ch) = chars.next() {
        if in_string {
            result.push(ch);
            if ch == '\'' {
                // Check for escaped quote ('')
                if chars.peek() == Some(&'\'') {
                    result.push('\'');
                    chars.next();
                } else {
                    in_string = false;
                }
            }
        } else if ch == '\'' {
            in_string = true;
            prev_was_space = false;
            result.push(ch);
        } else if ch.is_whitespace() {
            if !prev_was_space && !result.is_empty() {
                result.push(' ');
                prev_was_space = true;
            }
        } else {
            prev_was_space = false;
            for uc in ch.to_uppercase() {
                result.push(uc);
            }
        }
    }

    // Remove any trailing space (e.g. from trailing whitespace in input).
    if result.ends_with(' ') {
        result.pop();
    }

    result
}

// ── Column extraction ────────────────────────────────────────────────────────

/// Extract column names referenced in a statement.
///
/// Returns a deduplicated, sorted list of column names (without table prefix).
/// Only bare column references (`table.column` is normalized to `column`)
/// from SELECT, WHERE, ORDER BY, GROUP BY, and JOIN ON clauses are included.
/// Wildcard `*` selections are excluded.
///
/// # Example
///
/// ```rust
/// use oxisql_parse::{parse_one, extract_columns};
///
/// let stmt = parse_one("SELECT id, name FROM users WHERE age > 18").unwrap();
/// let cols = extract_columns(&stmt);
/// assert!(cols.contains(&"id".to_string()));
/// assert!(cols.contains(&"name".to_string()));
/// assert!(cols.contains(&"age".to_string()));
/// ```
pub fn extract_columns(stmt: &Statement) -> Vec<String> {
    let mut cols = Vec::new();
    columns::collect_columns_from_statement(stmt, &mut cols);
    cols.sort();
    cols.dedup();
    cols
}
