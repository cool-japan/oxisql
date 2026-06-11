//! Logical plan type definitions for OxiSQL.
//!
//! This module defines the AST-level relational algebra types produced by the
//! planner.  All types are re-exported from the crate root.

use crate::optimizer::JoinAlgoHint;

// ── Logical Plan ─────────────────────────────────────────────────────────────

/// AST-level logical query plan produced by [`crate::plan_statement`].
///
/// Each variant represents a relational algebra operator.  The tree is
/// built bottom-up: leaf nodes (e.g. `Scan`) are wrapped by operator nodes
/// (`Filter`, `Project`, …) as query features are encountered.
#[derive(Debug, Clone)]
pub enum LogicalPlan {
    /// Table scan.  `alias` is the `AS` alias, if any.
    Scan {
        /// The fully-qualified table name as it appears in the query.
        table: String,
        /// Optional alias (`AS x`).
        alias: Option<String>,
        /// Optional row-count cap injected by the optimizer's limit push-through
        /// pass.  `None` means the scan is unbounded.
        limit: Option<usize>,
    },
    /// Row filter (`WHERE`).
    Filter {
        /// The plan being filtered.
        input: Box<LogicalPlan>,
        /// The `WHERE` expression rendered as a string.
        predicate: String,
    },
    /// Column projection (`SELECT col1, col2, …`).
    Project {
        /// The plan whose rows are projected.
        input: Box<LogicalPlan>,
        /// The projected column expressions as strings.
        columns: Vec<String>,
    },
    /// Binary join of two inputs.
    Join {
        /// Left input.
        left: Box<LogicalPlan>,
        /// Right input.
        right: Box<LogicalPlan>,
        /// The `ON` expression rendered as a string.
        on: String,
        /// The join type (INNER, LEFT, RIGHT, FULL, CROSS).
        join_type: JoinType,
        /// Algorithm hint set by [`crate::optimizer::JoinAlgorithmPass`].
        algo_hint: Option<JoinAlgoHint>,
    },
    /// Aggregation (`GROUP BY` / aggregate functions).
    Aggregate {
        /// The plan whose rows are aggregated.
        input: Box<LogicalPlan>,
        /// `GROUP BY` expressions rendered as strings.
        group_by: Vec<String>,
        /// Aggregate function expressions rendered as strings.
        aggregates: Vec<String>,
    },
    /// Row ordering (`ORDER BY`).
    Sort {
        /// The plan whose rows are sorted.
        input: Box<LogicalPlan>,
        /// The sort key expressions.
        order_by: Vec<SortExpr>,
    },
    /// Row count / offset restriction (`LIMIT … OFFSET …`).
    Limit {
        /// The plan being limited.
        input: Box<LogicalPlan>,
        /// Maximum number of rows, if specified.
        count: Option<u64>,
        /// Number of rows to skip, if specified.
        offset: Option<u64>,
    },
    /// Constant row source (`INSERT … VALUES …`).
    Values {
        /// Column names from the INSERT column list.
        columns: Vec<String>,
        /// Number of value rows.
        rows: usize,
    },
    /// Placeholder for statements that produce no relational plan.
    Empty,
    /// A SQL set operation: `UNION`, `INTERSECT`, or `EXCEPT`.
    SetOp {
        /// The set-operation kind.
        op: SetOpType,
        /// `true` for `UNION ALL` / `INTERSECT ALL` / `EXCEPT ALL`.
        all: bool,
        /// Left input plan.
        left: Box<LogicalPlan>,
        /// Right input plan.
        right: Box<LogicalPlan>,
    },
    /// A CTE definition node.
    ///
    /// The `query` field is the plan for the CTE's own defining body (`AS (...)`).
    /// The query that *uses* this CTE is built by the surrounding planner; this
    /// variant retains only the CTE definition itself.
    Cte {
        /// The CTE name as introduced before the `AS` keyword.
        name: String,
        /// Plan for the CTE-defining sub-query (`AS ( ... )`).
        query: Box<LogicalPlan>,
        /// `true` when the entire `WITH` clause was `WITH RECURSIVE`.
        recursive: bool,
    },
    /// Reference to a previously defined CTE by name.
    CteRef {
        /// The CTE name being referenced.
        name: String,
    },
    /// Window-function layer sitting over a base plan.
    Window {
        /// The input plan over which windows are computed.
        input: Box<LogicalPlan>,
        /// The window functions defined in the SELECT projection.
        functions: Vec<WindowFunctionDef>,
    },
    /// A scalar subquery that returns a single value (used in SELECT or WHERE).
    Subquery {
        /// The inner query plan.
        query: Box<LogicalPlan>,
        /// Optional alias applied to the subquery expression.
        alias: Option<String>,
    },
    /// `EXISTS (subquery)` — used in WHERE predicates.
    Exists {
        /// The inner subquery plan.
        subquery: Box<LogicalPlan>,
        /// `true` when the expression is `NOT EXISTS`.
        negated: bool,
    },
    /// `expr IN (subquery)` — used in WHERE predicates.
    InSubquery {
        /// The expression being tested (e.g. `"col1"`).
        expr: String,
        /// The inner subquery plan.
        subquery: Box<LogicalPlan>,
        /// `true` when the expression is `NOT IN`.
        negated: bool,
    },
    /// A plan node that materializes named expression bindings for CSE.
    ///
    /// Each `(alias, expr_string)` pair in `bindings` declares a named
    /// intermediate value that downstream nodes can reference by alias.
    /// A `Compute` node is an optimization artifact introduced by the
    /// common-subexpression-elimination pass; it adds zero compute overhead
    /// and is transparent to most plan traversals.
    Compute {
        /// The input plan whose output rows are annotated with bindings.
        input: Box<LogicalPlan>,
        /// Named expression bindings: `(alias, expr_string)` pairs.
        bindings: Vec<(String, String)>,
    },
}

/// Set-operation kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetOpType {
    /// `UNION`
    Union,
    /// `INTERSECT`
    Intersect,
    /// `EXCEPT` (or `MINUS`)
    Except,
}

/// A single window-function definition extracted from a SELECT projection.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowFunctionDef {
    /// The function name (e.g. `ROW_NUMBER`, `RANK`).
    pub name: String,
    /// The function arguments rendered as strings.
    pub args: Vec<String>,
    /// `PARTITION BY` expressions rendered as strings.
    pub partition_by: Vec<String>,
    /// `ORDER BY` keys.
    pub order_by: Vec<SortExpr>,
    /// The alias applied to the expression in the projection.
    pub alias: String,
}

/// Join algorithm / semantic type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinType {
    /// `INNER JOIN` — only matching rows.
    Inner,
    /// `LEFT [OUTER] JOIN` — all rows from the left side.
    Left,
    /// `RIGHT [OUTER] JOIN` — all rows from the right side.
    Right,
    /// `FULL [OUTER] JOIN` — all rows from both sides.
    Full,
    /// `CROSS JOIN` — cartesian product.
    Cross,
    /// Semi-join: return each left row that has at least one matching right row.
    /// Used internally by the decorrelation pass (e.g. `EXISTS` rewrites).
    LeftSemi,
    /// Anti-join: return each left row that has *no* matching right row.
    /// Used internally by the decorrelation pass (e.g. `NOT EXISTS` rewrites).
    LeftAnti,
}

/// A single `ORDER BY` sort key.
#[derive(Debug, Clone, PartialEq)]
pub struct SortExpr {
    /// The column / expression being sorted.
    pub column: String,
    /// `true` for ascending (`ASC`), `false` for descending (`DESC`).
    pub ascending: bool,
}
