//! Fluent SQL query builder.
//!
//! Provides [`QueryBuilder`] for constructing SELECT, INSERT, UPDATE, and
//! DELETE statements programmatically, with optional verification via
//! [`sqlparser`].

use crate::parse;
use sqlparser::ast::Statement;

// ── Internal clause types ────────────────────────────────────────────────────

struct JoinClause {
    table: String,
    on: String,
    join_type: &'static str, // "INNER", "LEFT", "RIGHT", "CROSS"
}

struct OrderByClause {
    expr: String,
    ascending: bool,
}

// ── QueryBuilder ─────────────────────────────────────────────────────────────

/// Fluent SQL query builder.
///
/// # Example
///
/// ```rust
/// use oxisql_parse::QueryBuilder;
///
/// let sql = QueryBuilder::select(&["id", "name", "email"])
///     .from("users")
///     .join("orders", "users.id = orders.user_id")
///     .where_clause("users.active = TRUE")
///     .where_clause("orders.status = 'pending'")
///     .order_by("users.name", true)
///     .limit(10)
///     .offset(20)
///     .build()
///     .expect("valid query");
/// assert!(sql.contains("SELECT id, name, email"));
/// ```
pub struct QueryBuilder {
    select_cols: Vec<String>,
    from_table: Option<String>,
    joins: Vec<JoinClause>,
    wheres: Vec<String>,
    group_by: Vec<String>,
    having: Option<String>,
    order_bys: Vec<OrderByClause>,
    limit_val: Option<u64>,
    offset_val: Option<u64>,
    distinct: bool,
}

impl QueryBuilder {
    // ── Constructors ─────────────────────────────────────────────────────────

    /// Start a SELECT query with the given columns.
    pub fn select(columns: &[&str]) -> Self {
        Self {
            select_cols: columns.iter().map(|c| c.to_string()).collect(),
            from_table: None,
            joins: Vec::new(),
            wheres: Vec::new(),
            group_by: Vec::new(),
            having: None,
            order_bys: Vec::new(),
            limit_val: None,
            offset_val: None,
            distinct: false,
        }
    }

    /// Start a `SELECT *` query.
    pub fn select_all() -> Self {
        Self::select(&["*"])
    }

    // ── Modifiers ────────────────────────────────────────────────────────────

    /// Add `DISTINCT` to the SELECT clause.
    pub fn distinct(mut self) -> Self {
        self.distinct = true;
        self
    }

    /// Set the `FROM` table.
    pub fn from(mut self, table: &str) -> Self {
        self.from_table = Some(table.to_string());
        self
    }

    /// Add an `INNER JOIN`.
    pub fn join(mut self, table: &str, on: &str) -> Self {
        self.joins.push(JoinClause {
            table: table.to_string(),
            on: on.to_string(),
            join_type: "INNER",
        });
        self
    }

    /// Add a `LEFT JOIN`.
    pub fn left_join(mut self, table: &str, on: &str) -> Self {
        self.joins.push(JoinClause {
            table: table.to_string(),
            on: on.to_string(),
            join_type: "LEFT",
        });
        self
    }

    /// Add a `RIGHT JOIN`.
    pub fn right_join(mut self, table: &str, on: &str) -> Self {
        self.joins.push(JoinClause {
            table: table.to_string(),
            on: on.to_string(),
            join_type: "RIGHT",
        });
        self
    }

    /// Add a `WHERE` condition.  Multiple calls are combined with `AND`.
    pub fn where_clause(mut self, condition: &str) -> Self {
        self.wheres.push(condition.to_string());
        self
    }

    /// Add a `GROUP BY` expression.
    pub fn group_by(mut self, expr: &str) -> Self {
        self.group_by.push(expr.to_string());
        self
    }

    /// Set the `HAVING` clause.
    pub fn having(mut self, condition: &str) -> Self {
        self.having = Some(condition.to_string());
        self
    }

    /// Add an `ORDER BY` expression.  `ascending = true` → `ASC`, `false` → `DESC`.
    pub fn order_by(mut self, expr: &str, ascending: bool) -> Self {
        self.order_bys.push(OrderByClause {
            expr: expr.to_string(),
            ascending,
        });
        self
    }

    /// Set `LIMIT`.
    pub fn limit(mut self, n: u64) -> Self {
        self.limit_val = Some(n);
        self
    }

    /// Set `OFFSET`.
    pub fn offset(mut self, n: u64) -> Self {
        self.offset_val = Some(n);
        self
    }

    // ── Build ────────────────────────────────────────────────────────────────

    /// Build the final SQL string.
    ///
    /// Returns `Err` if no `FROM` table has been set.
    pub fn build(self) -> Result<String, String> {
        let from = self
            .from_table
            .ok_or_else(|| "QueryBuilder: no FROM table set".to_string())?;

        let mut parts: Vec<String> = Vec::new();

        // SELECT [DISTINCT] cols
        let cols = if self.select_cols.is_empty() {
            "*".to_string()
        } else {
            self.select_cols.join(", ")
        };
        if self.distinct {
            parts.push(format!("SELECT DISTINCT {cols}"));
        } else {
            parts.push(format!("SELECT {cols}"));
        }

        // FROM
        parts.push(format!("FROM {from}"));

        // JOINs
        for j in &self.joins {
            parts.push(format!("{} JOIN {} ON {}", j.join_type, j.table, j.on));
        }

        // WHERE
        if !self.wheres.is_empty() {
            parts.push(format!("WHERE {}", self.wheres.join(" AND ")));
        }

        // GROUP BY
        if !self.group_by.is_empty() {
            parts.push(format!("GROUP BY {}", self.group_by.join(", ")));
        }

        // HAVING
        if let Some(h) = &self.having {
            parts.push(format!("HAVING {h}"));
        }

        // ORDER BY
        if !self.order_bys.is_empty() {
            let order_exprs: Vec<String> = self
                .order_bys
                .iter()
                .map(|o| {
                    let dir = if o.ascending { "ASC" } else { "DESC" };
                    format!("{} {dir}", o.expr)
                })
                .collect();
            parts.push(format!("ORDER BY {}", order_exprs.join(", ")));
        }

        // LIMIT
        if let Some(n) = self.limit_val {
            parts.push(format!("LIMIT {n}"));
        }

        // OFFSET
        if let Some(n) = self.offset_val {
            parts.push(format!("OFFSET {n}"));
        }

        Ok(parts.join("\n"))
    }

    /// Build and immediately parse the SQL to verify it is syntactically valid.
    ///
    /// Returns `Err` if building fails or if the resulting SQL cannot be parsed.
    pub fn build_and_parse(self) -> Result<Vec<Statement>, String> {
        let sql = self.build()?;
        parse(&sql).map_err(|e| e.to_string())
    }

    /// Build the SQL string without consuming `self`.
    ///
    /// Internally clones the builder fields and delegates to [`Self::build`].
    /// Prefer [`build`][Self::build] when the builder is no longer needed.
    pub fn build_ref(&self) -> Result<String, String> {
        let cloned = QueryBuilder {
            select_cols: self.select_cols.clone(),
            from_table: self.from_table.clone(),
            joins: self
                .joins
                .iter()
                .map(|j| JoinClause {
                    table: j.table.clone(),
                    on: j.on.clone(),
                    join_type: j.join_type,
                })
                .collect(),
            wheres: self.wheres.clone(),
            group_by: self.group_by.clone(),
            having: self.having.clone(),
            order_bys: self
                .order_bys
                .iter()
                .map(|o| OrderByClause {
                    expr: o.expr.clone(),
                    ascending: o.ascending,
                })
                .collect(),
            limit_val: self.limit_val,
            offset_val: self.offset_val,
            distinct: self.distinct,
        };
        cloned.build()
    }

    // ── Static DML helpers ───────────────────────────────────────────────────

    /// Build an `INSERT INTO … VALUES (…)` statement.
    ///
    /// # Example
    ///
    /// ```rust
    /// use oxisql_parse::QueryBuilder;
    ///
    /// let sql = QueryBuilder::insert("users", &["id", "name"], &["1", "'Alice'"]);
    /// assert_eq!(sql, "INSERT INTO users (id, name) VALUES (1, 'Alice')");
    /// ```
    pub fn insert(table: &str, columns: &[&str], values: &[&str]) -> String {
        let cols = columns.join(", ");
        let vals = values.join(", ");
        format!("INSERT INTO {table} ({cols}) VALUES ({vals})")
    }

    /// Build an `UPDATE … SET … [WHERE …]` statement.
    ///
    /// # Example
    ///
    /// ```rust
    /// use oxisql_parse::QueryBuilder;
    ///
    /// let sql = QueryBuilder::update(
    ///     "users",
    ///     &[("name", "'Bob'"), ("active", "FALSE")],
    ///     Some("id = 42"),
    /// );
    /// assert!(sql.starts_with("UPDATE users SET"));
    /// assert!(sql.contains("WHERE id = 42"));
    /// ```
    pub fn update(table: &str, sets: &[(&str, &str)], where_clause: Option<&str>) -> String {
        let set_exprs: Vec<String> = sets.iter().map(|(k, v)| format!("{k} = {v}")).collect();
        let mut sql = format!("UPDATE {table} SET {}", set_exprs.join(", "));
        if let Some(w) = where_clause {
            sql.push_str(&format!(" WHERE {w}"));
        }
        sql
    }

    /// Build a `DELETE FROM … [WHERE …]` statement.
    ///
    /// # Example
    ///
    /// ```rust
    /// use oxisql_parse::QueryBuilder;
    ///
    /// let sql = QueryBuilder::delete("users", Some("id = 99"));
    /// assert_eq!(sql, "DELETE FROM users WHERE id = 99");
    /// ```
    pub fn delete(table: &str, where_clause: Option<&str>) -> String {
        let mut sql = format!("DELETE FROM {table}");
        if let Some(w) = where_clause {
            sql.push_str(&format!(" WHERE {w}"));
        }
        sql
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::QueryBuilder;

    #[test]
    fn test_simple_select() {
        let sql = QueryBuilder::select(&["id", "name"])
            .from("users")
            .build()
            .expect("build");
        assert_eq!(sql, "SELECT id, name\nFROM users");
    }

    #[test]
    fn test_select_with_where() {
        let sql = QueryBuilder::select(&["id", "name"])
            .from("users")
            .where_clause("active = TRUE")
            .build()
            .expect("build");
        assert!(sql.contains("WHERE active = TRUE"), "got: {sql}");
    }

    #[test]
    fn test_multiple_where_anded() {
        let sql = QueryBuilder::select(&["*"])
            .from("t")
            .where_clause("a = 1")
            .where_clause("b = 2")
            .build()
            .expect("build");
        assert!(sql.contains("WHERE a = 1 AND b = 2"), "got: {sql}");
    }

    #[test]
    fn test_join() {
        let sql = QueryBuilder::select(&["u.id", "o.id"])
            .from("users u")
            .join("orders o", "u.id = o.user_id")
            .build()
            .expect("build");
        assert!(
            sql.contains("INNER JOIN orders o ON u.id = o.user_id"),
            "got: {sql}"
        );
    }

    #[test]
    fn test_left_join() {
        let sql = QueryBuilder::select(&["u.id"])
            .from("users u")
            .left_join("orders o", "u.id = o.user_id")
            .build()
            .expect("build");
        assert!(
            sql.contains("LEFT JOIN orders o ON u.id = o.user_id"),
            "got: {sql}"
        );
    }

    #[test]
    fn test_order_limit_offset() {
        let sql = QueryBuilder::select(&["id"])
            .from("users")
            .order_by("name", true)
            .order_by("id", false)
            .limit(10)
            .offset(20)
            .build()
            .expect("build");
        assert!(sql.contains("ORDER BY name ASC, id DESC"), "got: {sql}");
        assert!(sql.contains("LIMIT 10"), "got: {sql}");
        assert!(sql.contains("OFFSET 20"), "got: {sql}");
    }

    #[test]
    fn test_group_by_having() {
        let sql = QueryBuilder::select(&["dept", "COUNT(*)"])
            .from("employees")
            .group_by("dept")
            .having("COUNT(*) > 5")
            .build()
            .expect("build");
        assert!(sql.contains("GROUP BY dept"), "got: {sql}");
        assert!(sql.contains("HAVING COUNT(*) > 5"), "got: {sql}");
    }

    #[test]
    fn test_distinct() {
        let sql = QueryBuilder::select(&["email"])
            .distinct()
            .from("users")
            .build()
            .expect("build");
        assert!(sql.starts_with("SELECT DISTINCT email"), "got: {sql}");
    }

    #[test]
    fn test_insert() {
        let sql = QueryBuilder::insert("users", &["id", "name"], &["1", "'Alice'"]);
        assert_eq!(sql, "INSERT INTO users (id, name) VALUES (1, 'Alice')");
    }

    #[test]
    fn test_update() {
        let sql = QueryBuilder::update(
            "users",
            &[("name", "'Bob'"), ("active", "FALSE")],
            Some("id = 42"),
        );
        assert!(sql.starts_with("UPDATE users SET"), "got: {sql}");
        assert!(sql.contains("name = 'Bob'"), "got: {sql}");
        assert!(sql.contains("active = FALSE"), "got: {sql}");
        assert!(sql.contains("WHERE id = 42"), "got: {sql}");
    }

    #[test]
    fn test_delete_with_where() {
        let sql = QueryBuilder::delete("users", Some("id = 99"));
        assert_eq!(sql, "DELETE FROM users WHERE id = 99");
    }

    #[test]
    fn test_build_and_parse() {
        let result = QueryBuilder::select(&["u.id", "u.name", "o.total"])
            .from("users u")
            .join("orders o", "u.id = o.user_id")
            .where_clause("u.active = TRUE")
            .order_by("u.name", true)
            .limit(50)
            .build_and_parse();
        assert!(result.is_ok(), "build_and_parse failed: {:?}", result.err());
        let stmts = result.expect("stmts");
        assert_eq!(stmts.len(), 1);
    }

    #[test]
    fn test_build_no_from_errors() {
        let result = QueryBuilder::select_all().build();
        assert!(result.is_err(), "expected Err when no FROM is set");
    }

    #[test]
    fn test_select_all() {
        let sql = QueryBuilder::select_all()
            .from("products")
            .build()
            .expect("build");
        assert!(sql.starts_with("SELECT *"), "got: {sql}");
    }

    #[test]
    fn test_build_ref_matches_build() {
        let qb = QueryBuilder::select(&["id", "name"])
            .from("users")
            .where_clause("active = TRUE")
            .limit(10);
        let via_ref = qb.build_ref().expect("build_ref");
        let via_owned = QueryBuilder::select(&["id", "name"])
            .from("users")
            .where_clause("active = TRUE")
            .limit(10)
            .build()
            .expect("build");
        assert_eq!(via_ref, via_owned);
    }
}
