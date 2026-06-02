//! Fluent query builder for constructing SQL strings with positional parameters.

use crate::{ToSqlValue, Value};

/// Sort direction for ORDER BY clauses.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum SortDirection {
    /// Ascending order (default).
    #[default]
    Asc,
    /// Descending order.
    Desc,
}

impl std::fmt::Display for SortDirection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SortDirection::Asc => write!(f, "ASC"),
            SortDirection::Desc => write!(f, "DESC"),
        }
    }
}

/// A fully-built SQL query: the SQL string and its bound parameter values.
#[derive(Debug, Clone, PartialEq)]
pub struct BuiltQuery {
    /// The SQL string with `$1`, `$2`, … placeholders.
    pub sql: String,
    /// Parameter values in order.
    pub params: Vec<Value>,
}

/// Fluent builder for SELECT queries.
///
/// # Example
///
/// ```rust
/// use oxisql_core::query::{SelectBuilder, SortDirection};
///
/// let q = SelectBuilder::new()
///     .columns(&["id", "name", "email"])
///     .from("users")
///     .where_eq("active", &true)
///     .order_by("name", Default::default())
///     .limit(20)
///     .offset(40)
///     .build();
///
/// assert_eq!(q.sql, "SELECT id, name, email FROM users WHERE active = $1 ORDER BY name ASC LIMIT 20 OFFSET 40");
/// ```
#[derive(Debug, Default, Clone)]
pub struct SelectBuilder {
    columns: Vec<String>,
    table: Option<String>,
    conditions: Vec<String>,
    params: Vec<Value>,
    order_by: Vec<(String, SortDirection)>,
    limit: Option<usize>,
    offset: Option<usize>,
}

impl SelectBuilder {
    /// Create a new empty SELECT builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add columns to SELECT. Call multiple times or pass a slice.
    pub fn columns(mut self, cols: &[&str]) -> Self {
        self.columns.extend(cols.iter().map(|s| s.to_string()));
        self
    }

    /// Set the FROM table.
    pub fn from(mut self, table: &str) -> Self {
        self.table = Some(table.to_string());
        self
    }

    /// Add a raw WHERE condition (e.g. `"id > $1"`). Call multiple times to AND them.
    pub fn where_raw(mut self, condition: &str) -> Self {
        self.conditions.push(condition.to_string());
        self
    }

    /// Add a `WHERE col = $N` condition, binding the value.
    pub fn where_eq(mut self, col: &str, val: &dyn ToSqlValue) -> Self {
        let n = self.params.len() + 1;
        self.conditions.push(format!("{col} = ${n}"));
        self.params.push(val.to_value());
        self
    }

    /// Add an ORDER BY clause.
    pub fn order_by(mut self, col: &str, dir: SortDirection) -> Self {
        self.order_by.push((col.to_string(), dir));
        self
    }

    /// Set LIMIT.
    pub fn limit(mut self, n: usize) -> Self {
        self.limit = Some(n);
        self
    }

    /// Set OFFSET.
    pub fn offset(mut self, n: usize) -> Self {
        self.offset = Some(n);
        self
    }

    /// Build the query into a [`BuiltQuery`].
    pub fn build(self) -> BuiltQuery {
        let cols = if self.columns.is_empty() {
            "*".to_string()
        } else {
            self.columns.join(", ")
        };
        let table = self.table.as_deref().unwrap_or("unknown");
        let mut sql = format!("SELECT {cols} FROM {table}");
        if !self.conditions.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&self.conditions.join(" AND "));
        }
        if !self.order_by.is_empty() {
            sql.push_str(" ORDER BY ");
            sql.push_str(
                &self
                    .order_by
                    .iter()
                    .map(|(c, d)| format!("{c} {d}"))
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        }
        if let Some(l) = self.limit {
            sql.push_str(&format!(" LIMIT {l}"));
        }
        if let Some(o) = self.offset {
            sql.push_str(&format!(" OFFSET {o}"));
        }
        BuiltQuery {
            sql,
            params: self.params,
        }
    }
}

/// Fluent builder for INSERT queries.
///
/// # Example
///
/// ```rust
/// use oxisql_core::query::InsertBuilder;
///
/// let q = InsertBuilder::new()
///     .into_table("users")
///     .column("name", &"Alice")
///     .column("age", &30i64)
///     .build();
///
/// assert_eq!(q.sql, "INSERT INTO users (name, age) VALUES ($1, $2)");
/// ```
#[derive(Debug, Default, Clone)]
pub struct InsertBuilder {
    table: Option<String>,
    columns: Vec<String>,
    params: Vec<Value>,
}

impl InsertBuilder {
    /// Create a new empty INSERT builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the target table for the INSERT.
    pub fn into_table(mut self, table: &str) -> Self {
        self.table = Some(table.to_string());
        self
    }

    /// Add a column-value pair for the INSERT.
    pub fn column(mut self, col: &str, val: &dyn ToSqlValue) -> Self {
        self.columns.push(col.to_string());
        self.params.push(val.to_value());
        self
    }

    /// Build the query into a [`BuiltQuery`].
    pub fn build(self) -> BuiltQuery {
        let table = self.table.as_deref().unwrap_or("unknown");
        let n = self.columns.len();
        let cols = self.columns.join(", ");
        let placeholders = (1..=n)
            .map(|i| format!("${i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!("INSERT INTO {table} ({cols}) VALUES ({placeholders})");
        BuiltQuery {
            sql,
            params: self.params,
        }
    }
}

/// Fluent builder for UPDATE queries.
///
/// # Example
///
/// ```rust
/// use oxisql_core::query::UpdateBuilder;
///
/// let q = UpdateBuilder::new()
///     .table("users")
///     .set("email", &"new@example.com")
///     .where_eq("id", &42i64)
///     .build();
///
/// assert_eq!(q.sql, "UPDATE users SET email = $1 WHERE id = $2");
/// ```
#[derive(Debug, Default, Clone)]
pub struct UpdateBuilder {
    table: Option<String>,
    sets: Vec<String>,
    conditions: Vec<String>,
    params: Vec<Value>,
}

impl UpdateBuilder {
    /// Create a new empty UPDATE builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the target table for the UPDATE.
    pub fn table(mut self, t: &str) -> Self {
        self.table = Some(t.to_string());
        self
    }

    /// Add a `SET col = $N` assignment, binding the value.
    pub fn set(mut self, col: &str, val: &dyn ToSqlValue) -> Self {
        let n = self.params.len() + 1;
        self.sets.push(format!("{col} = ${n}"));
        self.params.push(val.to_value());
        self
    }

    /// Add a raw WHERE condition. Call multiple times to AND them.
    pub fn where_raw(mut self, cond: &str) -> Self {
        self.conditions.push(cond.to_string());
        self
    }

    /// Add a `WHERE col = $N` condition, binding the value.
    pub fn where_eq(mut self, col: &str, val: &dyn ToSqlValue) -> Self {
        let n = self.params.len() + 1;
        self.conditions.push(format!("{col} = ${n}"));
        self.params.push(val.to_value());
        self
    }

    /// Build the query into a [`BuiltQuery`].
    pub fn build(self) -> BuiltQuery {
        let table = self.table.as_deref().unwrap_or("unknown");
        let sets = self.sets.join(", ");
        let mut sql = format!("UPDATE {table} SET {sets}");
        if !self.conditions.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&self.conditions.join(" AND "));
        }
        BuiltQuery {
            sql,
            params: self.params,
        }
    }
}

/// Fluent builder for DELETE queries.
///
/// # Example
///
/// ```rust
/// use oxisql_core::query::DeleteBuilder;
///
/// let q = DeleteBuilder::new()
///     .from("users")
///     .where_eq("id", &99i64)
///     .build();
///
/// assert_eq!(q.sql, "DELETE FROM users WHERE id = $1");
/// ```
#[derive(Debug, Default, Clone)]
pub struct DeleteBuilder {
    table: Option<String>,
    conditions: Vec<String>,
    params: Vec<Value>,
}

impl DeleteBuilder {
    /// Create a new empty DELETE builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the target table for the DELETE.
    pub fn from(mut self, table: &str) -> Self {
        self.table = Some(table.to_string());
        self
    }

    /// Add a raw WHERE condition. Call multiple times to AND them.
    pub fn where_raw(mut self, cond: &str) -> Self {
        self.conditions.push(cond.to_string());
        self
    }

    /// Add a `WHERE col = $N` condition, binding the value.
    pub fn where_eq(mut self, col: &str, val: &dyn ToSqlValue) -> Self {
        let n = self.params.len() + 1;
        self.conditions.push(format!("{col} = ${n}"));
        self.params.push(val.to_value());
        self
    }

    /// Build the query into a [`BuiltQuery`].
    pub fn build(self) -> BuiltQuery {
        let table = self.table.as_deref().unwrap_or("unknown");
        let mut sql = format!("DELETE FROM {table}");
        if !self.conditions.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&self.conditions.join(" AND "));
        }
        BuiltQuery {
            sql,
            params: self.params,
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_all_from_table() {
        let q = SelectBuilder::new().from("users").build();
        assert_eq!(q.sql, "SELECT * FROM users");
        assert!(q.params.is_empty());
    }

    #[test]
    fn select_columns_with_where_and_limit() {
        let q = SelectBuilder::new()
            .columns(&["id", "name"])
            .from("users")
            .where_eq("active", &true)
            .limit(10)
            .build();
        assert_eq!(
            q.sql,
            "SELECT id, name FROM users WHERE active = $1 LIMIT 10"
        );
        assert_eq!(q.params, vec![Value::Bool(true)]);
    }

    #[test]
    fn select_order_by_multiple() {
        let q = SelectBuilder::new()
            .from("items")
            .order_by("price", SortDirection::Desc)
            .order_by("name", SortDirection::Asc)
            .build();
        assert_eq!(q.sql, "SELECT * FROM items ORDER BY price DESC, name ASC");
    }

    #[test]
    fn select_with_offset() {
        let q = SelectBuilder::new()
            .from("logs")
            .limit(20)
            .offset(40)
            .build();
        assert_eq!(q.sql, "SELECT * FROM logs LIMIT 20 OFFSET 40");
    }

    #[test]
    fn insert_single_row() {
        let q = InsertBuilder::new()
            .into_table("users")
            .column("name", &"Alice")
            .column("age", &30i64)
            .build();
        assert_eq!(q.sql, "INSERT INTO users (name, age) VALUES ($1, $2)");
        assert_eq!(q.params.len(), 2);
    }

    #[test]
    fn update_with_where() {
        let q = UpdateBuilder::new()
            .table("users")
            .set("email", &"new@example.com")
            .where_eq("id", &42i64)
            .build();
        assert_eq!(q.sql, "UPDATE users SET email = $1 WHERE id = $2");
        assert_eq!(q.params.len(), 2);
    }

    #[test]
    fn delete_with_condition() {
        let q = DeleteBuilder::new()
            .from("users")
            .where_eq("id", &99i64)
            .build();
        assert_eq!(q.sql, "DELETE FROM users WHERE id = $1");
        assert_eq!(q.params[0], Value::I64(99));
    }

    #[test]
    fn multiple_where_conditions_joined_with_and() {
        let q = SelectBuilder::new()
            .from("orders")
            .where_eq("status", &"active")
            .where_eq("user_id", &5i64)
            .build();
        assert_eq!(
            q.sql,
            "SELECT * FROM orders WHERE status = $1 AND user_id = $2"
        );
        assert_eq!(q.params.len(), 2);
    }

    #[test]
    fn select_no_table_falls_back_to_unknown() {
        let q = SelectBuilder::new().columns(&["x"]).build();
        assert_eq!(q.sql, "SELECT x FROM unknown");
    }

    #[test]
    fn update_no_table_falls_back_to_unknown() {
        let q = UpdateBuilder::new().set("x", &1i64).build();
        assert_eq!(q.sql, "UPDATE unknown SET x = $1");
    }

    #[test]
    fn delete_no_conditions() {
        let q = DeleteBuilder::new().from("logs").build();
        assert_eq!(q.sql, "DELETE FROM logs");
        assert!(q.params.is_empty());
    }

    #[test]
    fn sort_direction_default_is_asc() {
        assert_eq!(SortDirection::default(), SortDirection::Asc);
    }

    #[test]
    fn sort_direction_display() {
        assert_eq!(format!("{}", SortDirection::Asc), "ASC");
        assert_eq!(format!("{}", SortDirection::Desc), "DESC");
    }

    #[test]
    fn where_raw_passthrough() {
        let q = SelectBuilder::new()
            .from("events")
            .where_raw("created_at > NOW()")
            .build();
        assert_eq!(q.sql, "SELECT * FROM events WHERE created_at > NOW()");
        assert!(q.params.is_empty());
    }

    #[test]
    fn insert_empty_columns() {
        let q = InsertBuilder::new().into_table("empty_table").build();
        assert_eq!(q.sql, "INSERT INTO empty_table () VALUES ()");
    }

    #[test]
    fn built_query_clone_and_eq() {
        let q1 = SelectBuilder::new().from("t").build();
        let q2 = q1.clone();
        assert_eq!(q1, q2);
    }
}
