//! Schema-aware validation of [`LogicalPlan`] trees.
//!
//! [`SchemaValidator`] walks a plan produced by [`crate::plan_statement`] and
//! checks that every table and column reference names a known entity in the
//! caller-supplied schema catalogue.

use std::collections::{HashMap, HashSet};

use crate::LogicalPlan;

// ── Error type ───────────────────────────────────────────────────────────────

/// Errors returned by [`SchemaValidator::validate`].
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ValidationError {
    /// A `Scan` or `InSubquery` references a table that does not exist.
    #[error("table not found: {0}")]
    TableNotFound(String),
    /// A `Project` references a column that does not exist in any accessible
    /// table.
    #[error("column not found: {table}.{column}")]
    ColumnNotFound {
        /// The table the column was expected in.
        table: String,
        /// The missing column name.
        column: String,
    },
    /// A bare column name matches more than one table in scope.
    #[error("ambiguous column: {0} (found in multiple tables)")]
    AmbiguousColumn(String),
}

// ── Validator ────────────────────────────────────────────────────────────────

/// Validates a [`LogicalPlan`] against a schema catalogue.
///
/// Build the validator with [`SchemaValidator::new`], register tables via
/// [`SchemaValidator::add_table`], then call [`SchemaValidator::validate`].
///
/// # Example
///
/// ```rust
/// use oxisql_parse::{parse_one, plan_statement, SchemaValidator};
///
/// let mut v = SchemaValidator::new();
/// v.add_table("users", &["id", "name", "age"]);
///
/// let stmt = parse_one("SELECT * FROM users").unwrap();
/// let plan = plan_statement(&stmt).unwrap();
/// assert!(v.validate(&plan).is_ok());
/// ```
pub struct SchemaValidator {
    /// Maps table name (lower-cased) to set of column names (lower-cased).
    schema: HashMap<String, HashSet<String>>,
}

impl SchemaValidator {
    /// Create a new, empty `SchemaValidator`.
    pub fn new() -> Self {
        Self {
            schema: HashMap::new(),
        }
    }

    /// Register a table with its column list.
    ///
    /// Both `name` and every element of `columns` are stored in lower-case so
    /// comparisons are case-insensitive.
    pub fn add_table(&mut self, name: &str, columns: &[&str]) {
        let cols: HashSet<String> = columns.iter().map(|c| c.to_ascii_lowercase()).collect();
        self.schema.insert(name.to_ascii_lowercase(), cols);
    }

    /// Validate `plan` against the registered schema.
    ///
    /// Returns `Ok(())` when all referenced tables and columns are known.
    ///
    /// # Errors
    ///
    /// Returns the first [`ValidationError`] encountered during the tree walk.
    pub fn validate(&self, plan: &LogicalPlan) -> Result<(), ValidationError> {
        self.validate_scans(plan)?;
        self.validate_projections(plan, &[])
    }

    /// Recursively check that every `Scan` node references a known table.
    fn validate_scans(&self, plan: &LogicalPlan) -> Result<(), ValidationError> {
        match plan {
            LogicalPlan::Scan { table, .. } => {
                let key = table.to_ascii_lowercase();
                if !self.schema.contains_key(&key) {
                    return Err(ValidationError::TableNotFound(table.clone()));
                }
            }
            LogicalPlan::Filter { input, .. }
            | LogicalPlan::Project { input, .. }
            | LogicalPlan::Sort { input, .. }
            | LogicalPlan::Limit { input, .. }
            | LogicalPlan::Aggregate { input, .. }
            | LogicalPlan::Window { input, .. } => {
                self.validate_scans(input)?;
            }
            LogicalPlan::Join { left, right, .. } | LogicalPlan::SetOp { left, right, .. } => {
                self.validate_scans(left)?;
                self.validate_scans(right)?;
            }
            LogicalPlan::Cte { query, .. } => {
                self.validate_scans(query)?;
            }
            LogicalPlan::Subquery { query, .. }
            | LogicalPlan::Exists {
                subquery: query, ..
            }
            | LogicalPlan::InSubquery {
                subquery: query, ..
            } => {
                self.validate_scans(query)?;
            }
            LogicalPlan::Values { .. } | LogicalPlan::Empty | LogicalPlan::CteRef { .. } => {}
        }
        Ok(())
    }

    /// Collect all table names reachable from `plan` via `Scan` nodes.
    fn collect_scan_tables<'a>(&self, plan: &'a LogicalPlan, out: &mut Vec<&'a str>) {
        match plan {
            LogicalPlan::Scan { table, .. } => out.push(table.as_str()),
            LogicalPlan::Filter { input, .. }
            | LogicalPlan::Project { input, .. }
            | LogicalPlan::Sort { input, .. }
            | LogicalPlan::Limit { input, .. }
            | LogicalPlan::Aggregate { input, .. }
            | LogicalPlan::Window { input, .. } => {
                self.collect_scan_tables(input, out);
            }
            LogicalPlan::Join { left, right, .. } | LogicalPlan::SetOp { left, right, .. } => {
                self.collect_scan_tables(left, out);
                self.collect_scan_tables(right, out);
            }
            LogicalPlan::Cte { query, .. } => self.collect_scan_tables(query, out),
            LogicalPlan::Subquery { query, .. }
            | LogicalPlan::Exists {
                subquery: query, ..
            }
            | LogicalPlan::InSubquery {
                subquery: query, ..
            } => {
                self.collect_scan_tables(query, out);
            }
            LogicalPlan::Values { .. } | LogicalPlan::Empty | LogicalPlan::CteRef { .. } => {}
        }
    }

    /// Validate column references inside `Project` nodes.
    ///
    /// `accessible_tables` carries table names visible from ancestor nodes.
    fn validate_projections(
        &self,
        plan: &LogicalPlan,
        accessible_tables: &[&str],
    ) -> Result<(), ValidationError> {
        match plan {
            LogicalPlan::Project { input, columns } => {
                // Gather tables accessible to this project.
                let mut tables: Vec<&str> = accessible_tables.to_vec();
                self.collect_scan_tables(input, &mut tables);

                for col_expr in columns {
                    // Strip aliases (e.g. `"col AS c"` → check `"col"`).
                    let raw = col_expr
                        .split_whitespace()
                        .next()
                        .unwrap_or(col_expr.as_str());
                    // Qualified: `table.column`
                    if let Some((tbl, col)) = raw.split_once('.') {
                        let tbl_key = tbl.trim_matches('"').to_ascii_lowercase();
                        let col_key = col.trim_matches('"').to_ascii_lowercase();
                        if let Some(known_cols) = self.schema.get(&tbl_key) {
                            if !known_cols.contains(&col_key) {
                                return Err(ValidationError::ColumnNotFound {
                                    table: tbl.to_string(),
                                    column: col.to_string(),
                                });
                            }
                        }
                        // If table not in schema we skip (scan validation covers it).
                    } else {
                        // Unqualified column — skip wildcards and function calls.
                        let col_key = raw.trim_matches('"').to_ascii_lowercase();
                        if col_key == "*" || col_key.contains('(') {
                            continue;
                        }
                        // Count how many tables have this column.
                        let matches: Vec<&str> = tables
                            .iter()
                            .filter(|&&t| {
                                let tk = t.to_ascii_lowercase();
                                self.schema
                                    .get(&tk)
                                    .map(|cols| cols.contains(&col_key))
                                    .unwrap_or(false)
                            })
                            .copied()
                            .collect();
                        if matches.len() > 1 {
                            return Err(ValidationError::AmbiguousColumn(raw.to_string()));
                        }
                        // If zero matches and tables are known, report missing.
                        if matches.is_empty()
                            && !tables.is_empty()
                            && tables
                                .iter()
                                .all(|t| self.schema.contains_key(&t.to_ascii_lowercase()))
                        {
                            let first_table = tables.first().copied().unwrap_or("unknown");
                            return Err(ValidationError::ColumnNotFound {
                                table: first_table.to_string(),
                                column: raw.to_string(),
                            });
                        }
                    }
                }
                self.validate_projections(input, accessible_tables)
            }
            LogicalPlan::Filter { input, .. }
            | LogicalPlan::Sort { input, .. }
            | LogicalPlan::Limit { input, .. }
            | LogicalPlan::Aggregate { input, .. }
            | LogicalPlan::Window { input, .. } => {
                self.validate_projections(input, accessible_tables)
            }
            LogicalPlan::Join { left, right, .. } | LogicalPlan::SetOp { left, right, .. } => {
                self.validate_projections(left, accessible_tables)?;
                self.validate_projections(right, accessible_tables)
            }
            LogicalPlan::Cte { query, .. } => self.validate_projections(query, accessible_tables),
            LogicalPlan::Subquery { query, .. }
            | LogicalPlan::Exists {
                subquery: query, ..
            }
            | LogicalPlan::InSubquery {
                subquery: query, ..
            } => self.validate_projections(query, accessible_tables),
            LogicalPlan::Scan { .. }
            | LogicalPlan::Values { .. }
            | LogicalPlan::Empty
            | LogicalPlan::CteRef { .. } => Ok(()),
        }
    }
}

impl Default for SchemaValidator {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{parse_one, plan_statement};

    fn make_scan(table: &str) -> LogicalPlan {
        LogicalPlan::Scan {
            table: table.to_string(),
            alias: None,
            limit: None,
        }
    }

    /// Known table → Ok.
    #[test]
    fn test_validate_known_table() {
        let mut v = SchemaValidator::new();
        v.add_table("users", &["id", "name"]);
        let plan = make_scan("users");
        assert!(v.validate(&plan).is_ok());
    }

    /// Unknown table → TableNotFound.
    #[test]
    fn test_validate_unknown_table() {
        let v = SchemaValidator::new();
        let plan = make_scan("orders");
        assert_eq!(
            v.validate(&plan),
            Err(ValidationError::TableNotFound("orders".to_string()))
        );
    }

    /// Scan with Filter over a known table → Ok.
    #[test]
    fn test_validate_filter_known() {
        let mut v = SchemaValidator::new();
        v.add_table("users", &["id", "name", "age"]);
        let plan = LogicalPlan::Filter {
            input: Box::new(make_scan("users")),
            predicate: "age > 18".to_string(),
        };
        assert!(v.validate(&plan).is_ok());
    }

    /// Parse → plan → validate: all tables known → Ok.
    #[test]
    fn test_validate_full_pipeline() {
        let mut v = SchemaValidator::new();
        v.add_table("users", &["id", "name", "age"]);

        let stmt = parse_one("SELECT * FROM users WHERE age > 18").expect("parse");
        let plan = plan_statement(&stmt).expect("plan");
        assert!(v.validate(&plan).is_ok());
    }

    /// Parse → plan → validate: table not in schema → TableNotFound.
    #[test]
    fn test_validate_full_pipeline_unknown() {
        let v = SchemaValidator::new(); // empty schema

        let stmt = parse_one("SELECT * FROM orders").expect("parse");
        let plan = plan_statement(&stmt).expect("plan");
        assert_eq!(
            v.validate(&plan),
            Err(ValidationError::TableNotFound("orders".to_string()))
        );
    }

    // ── Subquery plan tests ─────────────────────────────────────────────────

    /// Scalar subquery in SELECT projection → Subquery node in plan.
    #[test]
    fn test_plan_subquery_in_select() {
        let sql = "SELECT (SELECT COUNT(*) FROM orders \
                   WHERE orders.user_id = users.id) AS cnt FROM users";
        let stmt = parse_one(sql).expect("parse");
        let plan = plan_statement(&stmt).expect("plan");
        fn has_subquery(p: &LogicalPlan) -> bool {
            match p {
                LogicalPlan::Subquery { query, .. } => {
                    // Count as found even if we recurse deeper.
                    let _ = has_subquery(query);
                    true
                }
                LogicalPlan::Project { input, .. }
                | LogicalPlan::Filter { input, .. }
                | LogicalPlan::Sort { input, .. }
                | LogicalPlan::Limit { input, .. }
                | LogicalPlan::Aggregate { input, .. }
                | LogicalPlan::Window { input, .. } => has_subquery(input),
                _ => false,
            }
        }
        let found = matches!(&plan, LogicalPlan::Subquery { .. }) || has_subquery(&plan);
        assert!(found, "expected a Subquery node in: {plan:?}");
    }

    /// EXISTS subquery → Exists { negated: false }.
    #[test]
    fn test_plan_exists() {
        let sql = "SELECT * FROM users WHERE EXISTS \
                   (SELECT 1 FROM orders WHERE orders.user_id = users.id)";
        let stmt = parse_one(sql).expect("parse");
        let plan = plan_statement(&stmt).expect("plan");
        match &plan {
            LogicalPlan::Exists { negated, .. } => {
                assert!(!negated, "EXISTS should have negated=false");
            }
            other => panic!("expected Exists at root, got: {other:?}"),
        }
    }

    /// NOT EXISTS subquery → Exists { negated: true }.
    #[test]
    fn test_plan_not_exists() {
        let sql = "SELECT * FROM users WHERE NOT EXISTS \
                   (SELECT 1 FROM orders WHERE orders.user_id = users.id)";
        let stmt = parse_one(sql).expect("parse");
        let plan = plan_statement(&stmt).expect("plan");
        match &plan {
            LogicalPlan::Exists { negated, .. } => {
                assert!(negated, "NOT EXISTS should have negated=true");
            }
            other => panic!("expected Exists at root, got: {other:?}"),
        }
    }

    /// IN (subquery) → InSubquery { negated: false }.
    #[test]
    fn test_plan_in_subquery() {
        let sql = "SELECT * FROM users WHERE id IN (SELECT user_id FROM orders)";
        let stmt = parse_one(sql).expect("parse");
        let plan = plan_statement(&stmt).expect("plan");
        match &plan {
            LogicalPlan::InSubquery { negated, .. } => {
                assert!(!negated, "IN should have negated=false");
            }
            other => panic!("expected InSubquery at root, got: {other:?}"),
        }
    }

    /// explain() on a hand-built Subquery plan produces non-empty output.
    #[test]
    fn test_explain_subquery() {
        let plan = LogicalPlan::Subquery {
            query: Box::new(LogicalPlan::Scan {
                table: "orders".to_string(),
                alias: None,
                limit: None,
            }),
            alias: Some("cnt".to_string()),
        };
        let text = crate::explain(&plan);
        assert!(!text.is_empty(), "explain output should not be empty");
        assert!(text.contains("Subquery"), "expected 'Subquery' in: {text}");
    }
}
