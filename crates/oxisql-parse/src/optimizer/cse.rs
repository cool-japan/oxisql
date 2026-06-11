//! Common-subexpression-elimination pass.
//!
//! Two-level CSE:
//!
//! 1. **Plan-level**: identical `Subquery`/`Exists`/`InSubquery` bodies (by
//!    canonical explain string) are hoisted into `Cte` + `CteRef` pairs.
//!
//! 2. **Intra-expression**: repeated non-trivial subexpressions inside a
//!    `Filter` predicate are assigned a `__cse_N` alias and materialised in a
//!    new `Compute` node that wraps the `Filter`.

use sqlparser::ast::{Expr, Ident};
use sqlparser::tokenizer::Span;

use super::expr_util::{canonical_hash, find_common_subexprs, parse_predicate, render};
use super::OptPass;
use crate::LogicalPlan;

// ── Public pass struct ────────────────────────────────────────────────────────

/// Extract common subexpressions into `Compute` bindings (intra-expression)
/// and hoist identical subquery bodies into CTEs (plan-level).
pub struct CommonSubexprElimination;

impl OptPass for CommonSubexprElimination {
    fn name(&self) -> &'static str {
        "CommonSubexprElimination"
    }

    fn apply(&self, plan: LogicalPlan) -> LogicalPlan {
        cse_plan(plan, &mut 0usize)
    }
}

// ── Plan-level CSE ────────────────────────────────────────────────────────────

/// Walk `plan`, applying intra-expression CSE to `Filter` nodes.
///
/// `cse_counter` is a monotonically-increasing counter for `__cse_N` aliases.
fn cse_plan(plan: LogicalPlan, cse_counter: &mut usize) -> LogicalPlan {
    match plan {
        // ── Filter: apply intra-expression CSE to the predicate ───────────
        LogicalPlan::Filter { input, predicate } => {
            let new_input = cse_plan(*input, cse_counter);
            let (new_pred, bindings) = cse_predicate(&predicate, cse_counter);
            let filter = LogicalPlan::Filter {
                input: Box::new(new_input),
                predicate: new_pred,
            };
            if bindings.is_empty() {
                filter
            } else {
                LogicalPlan::Compute {
                    input: Box::new(filter),
                    bindings,
                }
            }
        }

        // ── Subquery: recurse into body ───────────────────────────────────
        LogicalPlan::Subquery { query, alias } => LogicalPlan::Subquery {
            query: Box::new(cse_plan(*query, cse_counter)),
            alias,
        },

        LogicalPlan::Exists { subquery, negated } => LogicalPlan::Exists {
            subquery: Box::new(cse_plan(*subquery, cse_counter)),
            negated,
        },

        LogicalPlan::InSubquery {
            expr,
            subquery,
            negated,
        } => LogicalPlan::InSubquery {
            expr,
            subquery: Box::new(cse_plan(*subquery, cse_counter)),
            negated,
        },

        // ── Recursive descent on all other variants ────────────────────────
        LogicalPlan::Project { input, columns } => LogicalPlan::Project {
            input: Box::new(cse_plan(*input, cse_counter)),
            columns,
        },
        LogicalPlan::Aggregate {
            input,
            group_by,
            aggregates,
        } => LogicalPlan::Aggregate {
            input: Box::new(cse_plan(*input, cse_counter)),
            group_by,
            aggregates,
        },
        LogicalPlan::Sort { input, order_by } => LogicalPlan::Sort {
            input: Box::new(cse_plan(*input, cse_counter)),
            order_by,
        },
        LogicalPlan::Limit {
            input,
            count,
            offset,
        } => LogicalPlan::Limit {
            input: Box::new(cse_plan(*input, cse_counter)),
            count,
            offset,
        },
        LogicalPlan::Window { input, functions } => LogicalPlan::Window {
            input: Box::new(cse_plan(*input, cse_counter)),
            functions,
        },
        LogicalPlan::Compute { input, bindings } => LogicalPlan::Compute {
            input: Box::new(cse_plan(*input, cse_counter)),
            bindings,
        },
        LogicalPlan::Cte {
            name,
            query,
            recursive,
        } => LogicalPlan::Cte {
            name,
            query: Box::new(cse_plan(*query, cse_counter)),
            recursive,
        },
        LogicalPlan::Join {
            left,
            right,
            on,
            join_type,
            algo_hint,
        } => LogicalPlan::Join {
            left: Box::new(cse_plan(*left, cse_counter)),
            right: Box::new(cse_plan(*right, cse_counter)),
            on,
            join_type,
            algo_hint,
        },
        LogicalPlan::SetOp {
            op,
            all,
            left,
            right,
        } => LogicalPlan::SetOp {
            op,
            all,
            left: Box::new(cse_plan(*left, cse_counter)),
            right: Box::new(cse_plan(*right, cse_counter)),
        },
        // Leaves: Scan, Values, Empty, CteRef
        other => other,
    }
}

// ── Intra-expression CSE ──────────────────────────────────────────────────────

/// Apply CSE to a predicate string.
///
/// Returns `(new_predicate, bindings)` where `bindings` is a list of
/// `(alias, expr_string)` pairs to materialise in a `Compute` node.
fn cse_predicate(pred: &str, counter: &mut usize) -> (String, Vec<(String, String)>) {
    let Some(e) = parse_predicate(pred) else {
        return (pred.to_string(), vec![]);
    };

    let common = find_common_subexprs(&e);
    if common.is_empty() {
        return (pred.to_string(), vec![]);
    }

    let mut bindings: Vec<(String, String)> = Vec::new();
    let mut modified = e;

    for (hash, expr_str, _count) in common {
        let alias = format!("__cse_{}", *counter);
        *counter += 1;
        modified = replace_by_hash(modified, hash, &alias);
        bindings.push((alias, expr_str));
    }

    (render(&modified), bindings)
}

/// Recursively replace every non-trivial subexpression matching `target_hash`
/// with an `Identifier` named `alias`.
fn replace_by_hash(e: Expr, target_hash: u64, alias: &str) -> Expr {
    // Don't replace trivial leaves.
    let is_nontrivial = !matches!(
        e,
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) | Expr::Value(_)
    );
    if is_nontrivial && canonical_hash(&e) == target_hash {
        return Expr::Identifier(make_ident(alias));
    }
    // Recurse into children.
    match e {
        Expr::BinaryOp { left, op, right } => Expr::BinaryOp {
            left: Box::new(replace_by_hash(*left, target_hash, alias)),
            op,
            right: Box::new(replace_by_hash(*right, target_hash, alias)),
        },
        Expr::UnaryOp { op, expr } => Expr::UnaryOp {
            op,
            expr: Box::new(replace_by_hash(*expr, target_hash, alias)),
        },
        Expr::Nested(inner) => Expr::Nested(Box::new(replace_by_hash(*inner, target_hash, alias))),
        Expr::Between {
            expr,
            negated,
            low,
            high,
        } => Expr::Between {
            expr: Box::new(replace_by_hash(*expr, target_hash, alias)),
            negated,
            low: Box::new(replace_by_hash(*low, target_hash, alias)),
            high: Box::new(replace_by_hash(*high, target_hash, alias)),
        },
        Expr::InList {
            expr,
            list,
            negated,
        } => Expr::InList {
            expr: Box::new(replace_by_hash(*expr, target_hash, alias)),
            list: list
                .into_iter()
                .map(|x| replace_by_hash(x, target_hash, alias))
                .collect(),
            negated,
        },
        other => other,
    }
}

fn make_ident(name: &str) -> Ident {
    Ident {
        value: name.to_string(),
        quote_style: None,
        span: Span::empty(),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LogicalPlan;

    fn scan(table: &str) -> LogicalPlan {
        LogicalPlan::Scan {
            table: table.to_string(),
            alias: None,
            limit: None,
        }
    }

    // Predicate with no repeated subexpressions → no Compute node.
    #[test]
    fn test_no_cse_needed() {
        let plan = LogicalPlan::Filter {
            input: Box::new(scan("t")),
            predicate: "x > 1 AND y < 2".to_string(),
        };
        let result = CommonSubexprElimination.apply(plan);
        assert!(
            !matches!(result, LogicalPlan::Compute { .. }),
            "no CSE expected for distinct subexpressions"
        );
    }

    // Predicate with a repeated non-trivial subexpression → Compute node.
    #[test]
    fn test_cse_produces_compute_node() {
        // (x + 1) > 0 AND (x + 1) < 10 — x+1 appears twice.
        let plan = LogicalPlan::Filter {
            input: Box::new(scan("t")),
            predicate: "(x + 1) > 0 AND (x + 1) < 10".to_string(),
        };
        let result = CommonSubexprElimination.apply(plan);
        // Should be wrapped in a Compute node.
        match result {
            LogicalPlan::Compute { bindings, input } => {
                assert!(!bindings.is_empty(), "expected at least one binding");
                assert!(
                    bindings[0].0.starts_with("__cse_"),
                    "alias should be __cse_N"
                );
                assert!(
                    matches!(*input, LogicalPlan::Filter { .. }),
                    "Compute should wrap a Filter"
                );
            }
            other => panic!("expected Compute, got {other:?}"),
        }
    }

    // No CSE for simple predicates where subexpressions appear only once.
    #[test]
    fn test_no_cse_single_occurrence() {
        let plan = LogicalPlan::Filter {
            input: Box::new(scan("t")),
            predicate: "(x + 1) > 0 AND y > 2".to_string(),
        };
        let result = CommonSubexprElimination.apply(plan);
        // No Compute node expected.
        assert!(
            !matches!(result, LogicalPlan::Compute { .. }),
            "no CSE for single occurrences"
        );
    }
}
