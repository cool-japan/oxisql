//! Aggregate function analysis helpers.
//!
//! Provides structured types and extraction functions for aggregate expressions
//! found in SQL SELECT projections.  The types here complement the string-based
//! `aggregates: Vec<String>` stored in [`crate::LogicalPlan::Aggregate`] by
//! giving callers a richer, typed view of the aggregate expressions.

use sqlparser::ast::{Expr, FunctionArguments, SelectItem};

// ── Public types ──────────────────────────────────────────────────────────────

/// Named aggregate function with an output alias.
#[derive(Debug, Clone, PartialEq)]
pub struct AggregateExpr {
    /// The aggregate function kind (COUNT, SUM, …).
    pub func: AggFunc,
    /// The column or expression being aggregated (e.g. `"*"`, `"amount"`).
    pub input: String,
    /// The output column alias (or a synthetic name when no alias is given).
    pub alias: String,
}

/// Aggregate function variant.
#[derive(Debug, Clone, PartialEq)]
pub enum AggFunc {
    /// `COUNT(*)` or `COUNT(col)`.
    Count,
    /// `SUM(expr)`.
    Sum,
    /// `AVG(expr)`.
    Avg,
    /// `MIN(expr)`.
    Min,
    /// `MAX(expr)`.
    Max,
    /// `COUNT(DISTINCT expr)`.
    CountDistinct,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Extract aggregate expressions from a sqlparser SELECT item list.
///
/// Only items that contain a top-level aggregate function call are returned.
/// Items that do not reference an aggregate are silently skipped.
pub fn extract_aggregates(select_items: &[SelectItem]) -> Vec<AggregateExpr> {
    select_items
        .iter()
        .filter_map(extract_agg_from_item)
        .collect()
}

/// Check if a sqlparser `Expr` is (or contains) an aggregate function call.
///
/// Returns `true` for direct aggregate function references.  Nested
/// expressions inside `BinaryOp` and `Nested` are also inspected.
pub fn is_aggregate_expr(expr: &Expr) -> bool {
    expr_is_aggregate(expr)
}

// ── Helpers (also pub(crate) for use by lib.rs) ───────────────────────────────

/// Return `true` if a SELECT projection item contains an aggregate function call.
pub(crate) fn projection_item_is_aggregate(item: &SelectItem) -> bool {
    match item {
        SelectItem::UnnamedExpr(e)
        | SelectItem::ExprWithAlias { expr: e, .. }
        | SelectItem::ExprWithAliases { expr: e, .. } => expr_is_aggregate(e),
        _ => false,
    }
}

/// Recursively check whether `expr` contains an aggregate function call.
pub(crate) fn expr_is_aggregate(expr: &Expr) -> bool {
    match expr {
        Expr::Function(f) => {
            let name = f.name.to_string().to_ascii_uppercase();
            matches!(
                name.as_str(),
                "COUNT"
                    | "SUM"
                    | "AVG"
                    | "MIN"
                    | "MAX"
                    | "STDDEV"
                    | "VARIANCE"
                    | "ARRAY_AGG"
                    | "STRING_AGG"
            )
        }
        Expr::BinaryOp { left, right, .. } => expr_is_aggregate(left) || expr_is_aggregate(right),
        Expr::Nested(inner) => expr_is_aggregate(inner),
        _ => false,
    }
}

// ── Private helpers ───────────────────────────────────────────────────────────

fn extract_agg_from_item(item: &SelectItem) -> Option<AggregateExpr> {
    match item {
        SelectItem::UnnamedExpr(expr) => {
            let agg = extract_agg_from_expr(expr)?;
            // Synthesize alias from the raw string representation.
            Some(AggregateExpr {
                alias: agg_synthetic_alias(&agg.func, &agg.input),
                func: agg.func,
                input: agg.input,
            })
        }
        SelectItem::ExprWithAlias { expr, alias } => {
            let agg = extract_agg_from_expr(expr)?;
            Some(AggregateExpr {
                func: agg.func,
                input: agg.input,
                alias: alias.value.clone(),
            })
        }
        SelectItem::ExprWithAliases { expr, aliases } => {
            let agg = extract_agg_from_expr(expr)?;
            let alias = aliases
                .first()
                .map(|a| a.value.clone())
                .unwrap_or_else(|| agg_synthetic_alias(&agg.func, &agg.input));
            Some(AggregateExpr {
                func: agg.func,
                input: agg.input,
                alias,
            })
        }
        _ => None,
    }
}

/// Partial aggregate expression (before alias is resolved).
struct PartialAgg {
    func: AggFunc,
    input: String,
}

fn extract_agg_from_expr(expr: &Expr) -> Option<PartialAgg> {
    if let Expr::Function(f) = expr {
        let name = f.name.to_string().to_ascii_uppercase();
        let func = match name.as_str() {
            "COUNT" => {
                // Detect COUNT(DISTINCT …).
                if is_distinct_args(&f.args) {
                    AggFunc::CountDistinct
                } else {
                    AggFunc::Count
                }
            }
            "SUM" => AggFunc::Sum,
            "AVG" => AggFunc::Avg,
            "MIN" => AggFunc::Min,
            "MAX" => AggFunc::Max,
            _ => return None,
        };
        let input = extract_first_arg_string(&f.args);
        Some(PartialAgg { func, input })
    } else {
        None
    }
}

/// Returns `true` when the function arguments include a `DISTINCT` qualifier.
fn is_distinct_args(args: &FunctionArguments) -> bool {
    match args {
        FunctionArguments::List(list) => list.duplicate_treatment.is_some(),
        _ => false,
    }
}

/// Extract the string representation of the first argument, or `"*"` for
/// wildcard / empty argument lists.
fn extract_first_arg_string(args: &FunctionArguments) -> String {
    match args {
        FunctionArguments::List(list) => {
            if let Some(first) = list.args.first() {
                first.to_string()
            } else {
                "*".to_string()
            }
        }
        FunctionArguments::None => "*".to_string(),
        FunctionArguments::Subquery(q) => q.to_string(),
    }
}

/// Build a synthetic alias like `count_star` or `sum_amount`.
fn agg_synthetic_alias(func: &AggFunc, input: &str) -> String {
    let fn_part = match func {
        AggFunc::Count | AggFunc::CountDistinct => "count",
        AggFunc::Sum => "sum",
        AggFunc::Avg => "avg",
        AggFunc::Min => "min",
        AggFunc::Max => "max",
    };
    let col_part = if input == "*" {
        "star".to_string()
    } else {
        input.replace(|c: char| !c.is_alphanumeric(), "_")
    };
    format!("{fn_part}_{col_part}")
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse;

    fn select_items(sql: &str) -> Vec<SelectItem> {
        let stmts = parse(sql).expect("parse");
        let stmt = stmts.into_iter().next().expect("one stmt");
        if let sqlparser::ast::Statement::Query(q) = stmt {
            if let sqlparser::ast::SetExpr::Select(sel) = *q.body {
                return sel.projection;
            }
        }
        vec![]
    }

    #[test]
    fn test_extract_count_star() {
        let items = select_items("SELECT COUNT(*) FROM t");
        let aggs = extract_aggregates(&items);
        assert_eq!(aggs.len(), 1);
        assert_eq!(aggs[0].func, AggFunc::Count);
        assert_eq!(aggs[0].input, "*");
    }

    #[test]
    fn test_extract_sum() {
        let items = select_items("SELECT SUM(amount) AS total FROM orders");
        let aggs = extract_aggregates(&items);
        assert_eq!(aggs.len(), 1);
        assert_eq!(aggs[0].func, AggFunc::Sum);
        assert_eq!(aggs[0].alias, "total");
    }

    #[test]
    fn test_extract_count_distinct() {
        let items = select_items("SELECT COUNT(DISTINCT user_id) FROM events");
        let aggs = extract_aggregates(&items);
        assert_eq!(aggs.len(), 1, "expected one aggregate, got {:?}", aggs);
        assert_eq!(aggs[0].func, AggFunc::CountDistinct);
    }

    #[test]
    fn test_is_aggregate_true() {
        let items = select_items("SELECT COUNT(*) FROM t");
        let item = items.into_iter().next().expect("item");
        if let SelectItem::UnnamedExpr(expr) = item {
            assert!(is_aggregate_expr(&expr));
        } else {
            panic!("expected UnnamedExpr");
        }
    }

    #[test]
    fn test_is_aggregate_false() {
        let items = select_items("SELECT id FROM t");
        let item = items.into_iter().next().expect("item");
        if let SelectItem::UnnamedExpr(expr) = item {
            assert!(!is_aggregate_expr(&expr));
        } else {
            panic!("expected UnnamedExpr");
        }
    }

    #[test]
    fn test_having_produces_filter_over_aggregate() {
        use crate::{parse_one, plan_statement};

        let stmt = parse_one(
            "SELECT dept, AVG(salary) FROM employees GROUP BY dept HAVING AVG(salary) > 50000",
        )
        .expect("parse");
        let plan = plan_statement(&stmt).expect("plan");

        // The outermost plan node should be Filter (from HAVING).
        // It wraps Aggregate.
        match &plan {
            crate::LogicalPlan::Filter { input, predicate } => {
                assert!(
                    predicate.contains("50000") || !predicate.is_empty(),
                    "HAVING predicate should be non-empty: {predicate}"
                );
                assert!(
                    matches!(input.as_ref(), crate::LogicalPlan::Aggregate { .. }),
                    "Filter should wrap Aggregate, got: {input:?}"
                );
            }
            other => panic!("expected Filter over Aggregate, got: {other:?}"),
        }
    }
}
