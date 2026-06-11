//! Decorrelation of correlated subqueries into joins.
//!
//! Rewrites correlated `EXISTS` / `NOT EXISTS` → `LeftSemi` / `LeftAnti` joins
//! and correlated `IN` → `LeftSemi` joins by pulling the correlation predicates
//! out of the inner subquery's `WHERE` clause and using them as the join `ON`
//! condition.

use oxisql_core::OxiSqlError;
use sqlparser::ast::Expr;

use crate::optimizer::expr_util::{collect_colrefs, join_conjuncts, render, split_conjuncts};
use crate::plan::{JoinType, LogicalPlan};

// ── PlannerOptions ────────────────────────────────────────────────────────────

/// Options that influence how `plan_query_with` builds the logical plan.
#[derive(Debug, Clone)]
pub struct PlannerOptions {
    /// When `true`, correlated `EXISTS` / `NOT EXISTS` / `IN` subqueries are
    /// rewritten into `LeftSemi` / `LeftAnti` joins at plan time.
    pub decorrelate: bool,
}

impl Default for PlannerOptions {
    fn default() -> Self {
        Self { decorrelate: true }
    }
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Post-facto decorrelation pass over an already-built `LogicalPlan`.
///
/// For `Filter` nodes whose predicate contains correlated `EXISTS` or
/// `InSubquery` conjuncts, the conjuncts are replaced by `LeftSemi` /
/// `LeftAnti` joins against the inner plan.  Uncorrelated subqueries and
/// structural `Exists` / `InSubquery` plan nodes without an accessible outer
/// plan are left unchanged.
///
/// `outer_scope` carries the table/alias names that are in scope from an
/// enclosing query block; pass `&[]` at the top level.
pub fn decorrelate_plan(plan: LogicalPlan, outer_scope: &[String]) -> LogicalPlan {
    match plan {
        LogicalPlan::Filter { input, predicate } => {
            let input = decorrelate_plan(*input, outer_scope);
            // Collect outer scope from the (already-decorrelated) input plan.
            let mut scope: Vec<String> = collect_plan_table_names(&input);
            scope.extend_from_slice(outer_scope);

            match apply_decorrelated_filter(input, &predicate, &scope) {
                Ok(new_plan) => new_plan,
                Err(_) => LogicalPlan::Filter {
                    input: Box::new(decorrelate_plan(
                        LogicalPlan::Scan {
                            table: "__err__".to_string(),
                            alias: None,
                            limit: None,
                        },
                        outer_scope,
                    )),
                    predicate,
                },
            }
        }

        // Recursively decorrelate children of all other variants.
        LogicalPlan::Project { input, columns } => LogicalPlan::Project {
            input: Box::new(decorrelate_plan(*input, outer_scope)),
            columns,
        },
        LogicalPlan::Aggregate {
            input,
            group_by,
            aggregates,
        } => LogicalPlan::Aggregate {
            input: Box::new(decorrelate_plan(*input, outer_scope)),
            group_by,
            aggregates,
        },
        LogicalPlan::Sort { input, order_by } => LogicalPlan::Sort {
            input: Box::new(decorrelate_plan(*input, outer_scope)),
            order_by,
        },
        LogicalPlan::Limit {
            input,
            count,
            offset,
        } => LogicalPlan::Limit {
            input: Box::new(decorrelate_plan(*input, outer_scope)),
            count,
            offset,
        },
        LogicalPlan::Window { input, functions } => LogicalPlan::Window {
            input: Box::new(decorrelate_plan(*input, outer_scope)),
            functions,
        },
        LogicalPlan::Compute { input, bindings } => LogicalPlan::Compute {
            input: Box::new(decorrelate_plan(*input, outer_scope)),
            bindings,
        },
        LogicalPlan::Join {
            left,
            right,
            on,
            join_type,
            algo_hint,
        } => LogicalPlan::Join {
            left: Box::new(decorrelate_plan(*left, outer_scope)),
            right: Box::new(decorrelate_plan(*right, outer_scope)),
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
            left: Box::new(decorrelate_plan(*left, outer_scope)),
            right: Box::new(decorrelate_plan(*right, outer_scope)),
        },
        LogicalPlan::Cte {
            name,
            query,
            recursive,
        } => LogicalPlan::Cte {
            name,
            query: Box::new(decorrelate_plan(*query, outer_scope)),
            recursive,
        },
        LogicalPlan::Subquery { query, alias } => LogicalPlan::Subquery {
            query: Box::new(decorrelate_plan(*query, outer_scope)),
            alias,
        },
        LogicalPlan::Exists { subquery, negated } => LogicalPlan::Exists {
            subquery: Box::new(decorrelate_plan(*subquery, outer_scope)),
            negated,
        },
        LogicalPlan::InSubquery {
            expr,
            subquery,
            negated,
        } => LogicalPlan::InSubquery {
            expr,
            subquery: Box::new(decorrelate_plan(*subquery, outer_scope)),
            negated,
        },
        // Leaves
        other => other,
    }
}

/// Try to rewrite correlated EXISTS / IN conjuncts in `predicate` into
/// LeftSemi / LeftAnti joins on top of `outer_plan`.
///
/// Returns the (possibly modified) plan with the decorrelated subqueries
/// turned into joins and any remaining regular predicates wrapped in a Filter.
fn apply_decorrelated_filter(
    outer_plan: LogicalPlan,
    predicate: &str,
    outer_scope: &[String],
) -> Result<LogicalPlan, OxiSqlError> {
    let Some(pred_expr) = crate::parse_predicate(predicate) else {
        // Can't parse; keep as-is.
        return Ok(LogicalPlan::Filter {
            input: Box::new(outer_plan),
            predicate: predicate.to_string(),
        });
    };

    let conjuncts = split_conjuncts(pred_expr);
    let mut plan = outer_plan;
    let mut remaining: Vec<Expr> = Vec::new();

    for conjunct in conjuncts {
        match conjunct {
            Expr::Exists {
                ref subquery,
                negated,
            } => {
                let inner = crate::planner::plan_query(subquery)?;
                let (non_corr_plan, corr_preds) = extract_correlated_filter(inner, outer_scope);
                if corr_preds.is_empty() {
                    // Uncorrelated — keep as a structural filter expression.
                    remaining.push(conjunct);
                } else {
                    let join_on = corr_preds.join(" AND ");
                    let join_type = if negated {
                        JoinType::LeftAnti
                    } else {
                        JoinType::LeftSemi
                    };
                    plan = LogicalPlan::Join {
                        left: Box::new(plan),
                        right: Box::new(non_corr_plan),
                        on: join_on,
                        join_type,
                        algo_hint: None,
                    };
                }
            }

            Expr::InSubquery {
                ref expr,
                ref subquery,
                negated,
            } => {
                let inner = crate::planner::plan_query(subquery)?;
                let proj_cols = extract_projection_cols(&inner);
                let (non_corr_plan, mut corr_preds) = extract_correlated_filter(inner, outer_scope);

                if corr_preds.is_empty() {
                    // Uncorrelated.
                    remaining.push(conjunct);
                } else {
                    let expr_str = expr.to_string();
                    // Prepend the equality between the outer expr and the inner projected column.
                    if let Some(proj_col) = proj_cols.first() {
                        corr_preds.insert(0, format!("{expr_str} = {proj_col}"));
                    }
                    let join_on = corr_preds.join(" AND ");
                    let join_type = if negated {
                        JoinType::LeftAnti
                    } else {
                        JoinType::LeftSemi
                    };
                    plan = LogicalPlan::Join {
                        left: Box::new(plan),
                        right: Box::new(non_corr_plan),
                        on: join_on,
                        join_type,
                        algo_hint: None,
                    };
                }
            }

            other => remaining.push(other),
        }
    }

    // Wrap with a Filter for any remaining (non-decorrelatable) conjuncts.
    if let Some(e) = join_conjuncts(remaining) {
        plan = LogicalPlan::Filter {
            input: Box::new(plan),
            predicate: render(&e),
        };
    }

    Ok(plan)
}

// ── Internals ─────────────────────────────────────────────────────────────────

/// Entry point called from `plan_select_with_opts` in `planner.rs`.
///
/// Applies decorrelation to the WHERE expression `pred` given the outer
/// table scope `outer_scope`.
pub(crate) fn apply_decorrelated_where(
    outer_plan: LogicalPlan,
    pred: &Expr,
    outer_scope: &[String],
) -> Result<LogicalPlan, OxiSqlError> {
    let conjuncts = split_conjuncts(pred.clone());
    let mut plan = outer_plan;
    let mut remaining: Vec<Expr> = Vec::new();

    for conjunct in conjuncts {
        match conjunct {
            Expr::Exists {
                ref subquery,
                negated,
            } => {
                let opts_no_decorr = PlannerOptions { decorrelate: false };
                let inner = crate::planner::plan_query_with_opts(subquery, &opts_no_decorr)?;
                let (non_corr_plan, corr_preds) = extract_correlated_filter(inner, outer_scope);

                if corr_preds.is_empty() {
                    remaining.push(conjunct);
                } else {
                    let join_on = corr_preds.join(" AND ");
                    let join_type = if negated {
                        JoinType::LeftAnti
                    } else {
                        JoinType::LeftSemi
                    };
                    plan = LogicalPlan::Join {
                        left: Box::new(plan),
                        right: Box::new(non_corr_plan),
                        on: join_on,
                        join_type,
                        algo_hint: None,
                    };
                }
            }

            Expr::InSubquery {
                ref expr,
                ref subquery,
                negated,
            } => {
                let opts_no_decorr = PlannerOptions { decorrelate: false };
                let inner = crate::planner::plan_query_with_opts(subquery, &opts_no_decorr)?;
                let proj_cols = extract_projection_cols(&inner);
                let (non_corr_plan, mut corr_preds) = extract_correlated_filter(inner, outer_scope);

                if corr_preds.is_empty() {
                    remaining.push(conjunct);
                } else {
                    let expr_str = expr.to_string();
                    if let Some(proj_col) = proj_cols.first() {
                        corr_preds.insert(0, format!("{expr_str} = {proj_col}"));
                    }
                    let join_on = corr_preds.join(" AND ");
                    let join_type = if negated {
                        JoinType::LeftAnti
                    } else {
                        JoinType::LeftSemi
                    };
                    plan = LogicalPlan::Join {
                        left: Box::new(plan),
                        right: Box::new(non_corr_plan),
                        on: join_on,
                        join_type,
                        algo_hint: None,
                    };
                }
            }

            other => remaining.push(other),
        }
    }

    if let Some(e) = join_conjuncts(remaining) {
        plan = LogicalPlan::Filter {
            input: Box::new(plan),
            predicate: render(&e),
        };
    }

    Ok(plan)
}

/// Split a filter's predicate into correlated and non-correlated conjuncts.
///
/// Returns `(modified_plan_without_corr_predicates, correlated_conjuncts_as_strings)`.
/// Recursion descends through `Project` to find the first `Filter`.
fn extract_correlated_filter(
    plan: LogicalPlan,
    outer_scope: &[String],
) -> (LogicalPlan, Vec<String>) {
    match plan {
        LogicalPlan::Filter { input, predicate } => {
            let Some(e) = crate::parse_predicate(&predicate) else {
                return (LogicalPlan::Filter { input, predicate }, vec![]);
            };
            let conjuncts = split_conjuncts(e);
            let mut corr: Vec<String> = Vec::new();
            let mut non_corr: Vec<Expr> = Vec::new();

            for c in conjuncts {
                let refs = collect_colrefs(&c);
                let is_corr = refs.iter().any(|r| {
                    r.qualifier
                        .as_deref()
                        .map(|q| outer_scope.iter().any(|s| s == q))
                        .unwrap_or(false)
                });
                if is_corr {
                    corr.push(render(&c));
                } else {
                    non_corr.push(c);
                }
            }

            let new_plan = if let Some(e) = join_conjuncts(non_corr) {
                LogicalPlan::Filter {
                    input,
                    predicate: render(&e),
                }
            } else {
                *input
            };
            (new_plan, corr)
        }

        LogicalPlan::Project { input, columns } => {
            let (new_input, corr) = extract_correlated_filter(*input, outer_scope);
            (
                LogicalPlan::Project {
                    input: Box::new(new_input),
                    columns,
                },
                corr,
            )
        }

        // For other variants, no correlation found at this level.
        other => (other, vec![]),
    }
}

/// Extract projected column expressions from the top-level of a plan.
fn extract_projection_cols(plan: &LogicalPlan) -> Vec<String> {
    match plan {
        LogicalPlan::Project { columns, .. } => columns.clone(),
        LogicalPlan::Filter { input, .. } => extract_projection_cols(input),
        _ => vec![],
    }
}

/// Collect all table names and aliases reachable from `plan`.
fn collect_plan_table_names(plan: &LogicalPlan) -> Vec<String> {
    let mut out = Vec::new();
    collect_names_inner(plan, &mut out);
    out
}

fn collect_names_inner(plan: &LogicalPlan, out: &mut Vec<String>) {
    match plan {
        LogicalPlan::Scan { table, alias, .. } => {
            out.push(table.clone());
            if let Some(a) = alias {
                out.push(a.clone());
            }
        }
        LogicalPlan::Filter { input, .. }
        | LogicalPlan::Project { input, .. }
        | LogicalPlan::Aggregate { input, .. }
        | LogicalPlan::Sort { input, .. }
        | LogicalPlan::Limit { input, .. }
        | LogicalPlan::Window { input, .. }
        | LogicalPlan::Compute { input, .. } => collect_names_inner(input, out),
        LogicalPlan::Join { left, right, .. } | LogicalPlan::SetOp { left, right, .. } => {
            collect_names_inner(left, out);
            collect_names_inner(right, out);
        }
        _ => {}
    }
}
