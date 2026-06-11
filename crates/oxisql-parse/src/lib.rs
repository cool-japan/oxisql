#![forbid(unsafe_code)]
#![warn(missing_docs)]

//! `oxisql-parse` — SQL parsing utilities for OxiSQL.
//!
//! Re-exports the [`sqlparser`] AST types most commonly needed by OxiSQL
//! backends and provides convenience functions for parsing, formatting,
//! analysing, and dialect-specific handling of SQL statements.
//!
//! # Quick start
//!
//! ```rust
//! use oxisql_parse::{parse, parse_one, is_read_only};
//!
//! let stmts = parse("SELECT 1; SELECT 2").unwrap();
//! assert_eq!(stmts.len(), 2);
//!
//! let stmt = parse_one("SELECT 42").unwrap();
//! assert!(is_read_only(&stmt));
//! ```

// ── Optimizer module ─────────────────────────────────────────────────────────
pub mod optimizer;
pub use optimizer::expr_util::{
    canonical_hash, collect_colrefs, equi_key, find_common_subexprs, join_conjuncts, parse_expr,
    parse_predicate, render, split_conjuncts, ColRef,
};
pub use optimizer::{
    CommonSubexprElimination, ConstantFolding, JoinAlgoHint, JoinAlgorithmPass, LimitPushThrough,
    OptPass, Optimizer, PredicatePushdown, PredicateSimplification, ProjectionPruning,
};

// ── Decorrelation ─────────────────────────────────────────────────────────────
pub mod decorrelate;
pub use decorrelate::PlannerOptions;

// ── Parameterization ──────────────────────────────────────────────────────────
pub mod parameterize;
pub use parameterize::{parameterize, ParameterizedSql};

// ── Plan cache ────────────────────────────────────────────────────────────────
pub mod plan_cache;
pub use plan_cache::PlanCache;
// ── Aggregate helpers ─────────────────────────────────────────────────────────
pub mod agg;
pub use agg::{extract_aggregates, is_aggregate_expr, AggFunc, AggregateExpr};
// ── DML / Validator / Cost / Builder modules ─────────────────────────────────
pub mod dml;
pub use dml::{plan_dml, DmlPlan};
pub mod validate;
pub use validate::{SchemaValidator, ValidationError};
pub mod cost;
pub use cost::{ColumnStats, CostEstimate, CostModel, NodeCost, TableStats};
pub mod builder;
mod columns;
mod setops;
mod window;
pub use builder::QueryBuilder;

// ── Plan types ───────────────────────────────────────────────────────────────
pub mod plan;
pub use plan::{JoinType, LogicalPlan, SetOpType, SortExpr, WindowFunctionDef};

// ── Planner ──────────────────────────────────────────────────────────────────
pub mod planner;
pub use planner::{plan_query, plan_statement, plan_statement_with_opts};

// ── Plan explanation ─────────────────────────────────────────────────────────
pub mod explain;
pub use explain::{explain, explain_json, explain_verbose};

// ── Analysis utilities ────────────────────────────────────────────────────────
pub mod analysis;
pub use analysis::{count_params, extract_columns, extract_tables, is_read_only, normalize};

// ── LRU parse cache ──────────────────────────────────────────────────────────
pub mod cache;
pub use cache::ParseCache;

// ── Re-exports ──────────────────────────────────────────────────────────────
use oxisql_core::OxiSqlError;
pub use sqlparser::ast::{Expr, JoinConstraint, ObjectName, SelectItem, Statement, TableFactor};
use sqlparser::dialect::Dialect;
pub use sqlparser::dialect::{GenericDialect, MySqlDialect, PostgreSqlDialect, SQLiteDialect};
use sqlparser::parser::Parser;

// ── SQL Dialect enum ────────────────────────────────────────────────────────
/// SQL dialect selection for parsing.
///
/// Different databases have dialect-specific syntax; selecting the correct
/// dialect improves parse accuracy for backend-specific SQL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SqlDialect {
    /// ANSI / generic SQL dialect.
    Generic,
    /// PostgreSQL dialect (supports `::` casts, `$1` params, etc.).
    Postgres,
    /// MySQL dialect (supports backtick identifiers, etc.).
    MySQL,
    /// SQLite dialect (supports `AUTOINCREMENT`, backtick/bracket identifiers, etc.).
    SQLite,
}

impl SqlDialect {
    /// Return the `sqlparser` [`Dialect`] implementation for this variant.
    pub(crate) fn to_sqlparser(self) -> Box<dyn Dialect> {
        match self {
            SqlDialect::Generic => Box::new(GenericDialect {}),
            SqlDialect::Postgres => Box::new(PostgreSqlDialect {}),
            SqlDialect::MySQL => Box::new(MySqlDialect {}),
            SqlDialect::SQLite => Box::new(SQLiteDialect {}),
        }
    }
}

// ── Optimizer convenience wrapper ────────────────────────────────────────────

/// Apply the default four-pass optimizer to `plan` and return the result.
///
/// This is a convenience shorthand for `Optimizer::new().optimize(plan)`.
/// The passes run are: predicate pushdown, projection pruning (no-op when no
/// required columns are specified), constant folding, and limit push-through.
pub fn optimize(plan: LogicalPlan) -> LogicalPlan {
    optimizer::optimize(plan)
}

// ── Dialect convenience wrappers ─────────────────────────────────────────────

/// Parse SQL using the PostgreSQL dialect.
///
/// # Errors
///
/// Returns [`OxiSqlError::Parse`] if the string cannot be parsed.
pub fn parse_postgres(sql: &str) -> Result<Vec<Statement>, OxiSqlError> {
    parse_with_dialect(sql, SqlDialect::Postgres)
}

/// Parse SQL using the MySQL dialect.
///
/// # Errors
///
/// Returns [`OxiSqlError::Parse`] if the string cannot be parsed.
pub fn parse_mysql(sql: &str) -> Result<Vec<Statement>, OxiSqlError> {
    parse_with_dialect(sql, SqlDialect::MySQL)
}

/// Parse SQL using the SQLite dialect.
///
/// # Errors
///
/// Returns [`OxiSqlError::Parse`] if the string cannot be parsed.
pub fn parse_sqlite(sql: &str) -> Result<Vec<Statement>, OxiSqlError> {
    parse_with_dialect(sql, SqlDialect::SQLite)
}

// ── Parsing functions ───────────────────────────────────────────────────────

/// Parse a SQL string into a list of [`Statement`]s using the generic SQL
/// dialect.
///
/// # Errors
///
/// Returns [`OxiSqlError::Parse`] if the string cannot be parsed.
pub fn parse(sql: &str) -> Result<Vec<Statement>, OxiSqlError> {
    let dialect = GenericDialect {};
    Parser::parse_sql(&dialect, sql).map_err(|e| OxiSqlError::Parse(e.to_string()))
}

/// Parse a SQL string using a specific [`SqlDialect`].
///
/// # Errors
///
/// Returns [`OxiSqlError::Parse`] if the string cannot be parsed.
pub fn parse_with_dialect(sql: &str, dialect: SqlDialect) -> Result<Vec<Statement>, OxiSqlError> {
    let d = dialect.to_sqlparser();
    Parser::parse_sql(d.as_ref(), sql).map_err(|e| OxiSqlError::Parse(e.to_string()))
}

/// Parse a SQL string that is expected to contain exactly one statement.
///
/// Returns [`OxiSqlError::Parse`] if parsing fails or if the string contains
/// zero or more than one statement.
pub fn parse_one(sql: &str) -> Result<Statement, OxiSqlError> {
    let mut stmts = parse(sql)?;
    if stmts.len() != 1 {
        return Err(OxiSqlError::Parse(format!(
            "expected exactly 1 statement, got {}",
            stmts.len()
        )));
    }
    Ok(stmts.remove(0))
}

/// Parse a single statement using a specific dialect.
pub fn parse_one_with_dialect(sql: &str, dialect: SqlDialect) -> Result<Statement, OxiSqlError> {
    let mut stmts = parse_with_dialect(sql, dialect)?;
    if stmts.len() != 1 {
        return Err(OxiSqlError::Parse(format!(
            "expected exactly 1 statement, got {}",
            stmts.len()
        )));
    }
    Ok(stmts.remove(0))
}

// ── Formatting ──────────────────────────────────────────────────────────────

/// Convert a parsed AST statement back to a SQL string.
///
/// Uses sqlparser's `Display` implementation for round-trip formatting.
pub fn format(stmt: &Statement) -> String {
    stmt.to_string()
}

// ── plan_query_with convenience wrapper ──────────────────────────────────────

/// Parse `sql`, then plan it using the given [`PlannerOptions`].
///
/// This is the top-level entry point for callers that want to control
/// decorrelation behaviour without going through a [`PlanCache`].
///
/// # Errors
///
/// Returns [`OxiSqlError::Parse`] if parsing or planning fails.
pub fn plan_query_with(sql: &str, opts: &PlannerOptions) -> Result<LogicalPlan, OxiSqlError> {
    let stmt = parse_one(sql)?;
    plan_statement_with_opts(&stmt, opts)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
