//! Pass 1: Predicate pushdown.
//!
//! Push `Filter` nodes as close to `Scan` leaves as possible.  A filter
//! sitting directly above an `INNER` or `CROSS` join can be moved below the
//! join onto whichever side owns all referenced columns.  Filters above
//! projections are pushed through when all predicate column names are in scope
//! below the projection.

use crate::LogicalPlan;
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;

use super::OptPass;

// ── Public struct ────────────────────────────────────────────────────────────

/// Push `Filter` nodes toward leaf `Scan` nodes.
pub struct PredicatePushdown;

impl OptPass for PredicatePushdown {
    fn name(&self) -> &'static str {
        "PredicatePushdown"
    }

    fn apply(&self, plan: LogicalPlan) -> LogicalPlan {
        push_predicate(plan)
    }
}

// ── Implementation ───────────────────────────────────────────────────────────

/// Recursive predicate-pushdown rewrite.
pub(super) fn push_predicate(plan: LogicalPlan) -> LogicalPlan {
    match plan {
        LogicalPlan::Filter { input, predicate } => {
            // Recurse into the input first so inner filters are also pushed.
            let inner = push_predicate(*input);

            match inner {
                // Filter over a Join — try to push into one side.
                // NOTE: Only Inner and Cross joins preserve the semantics of
                // pushing a single-side filter below the join.  For outer joins
                // the non-preserved side must not be filtered before the join or
                // we would lose rows that should appear as NULLs.
                LogicalPlan::Join {
                    left,
                    right,
                    on,
                    join_type,
                    ..
                } => {
                    let can_push =
                        matches!(join_type, crate::JoinType::Inner | crate::JoinType::Cross);

                    let left_tables = collect_table_names(&left);
                    let right_tables = collect_table_names(&right);

                    let only_left =
                        can_push && predicate_refs_only(&predicate, &left_tables, &right_tables);
                    let only_right =
                        can_push && predicate_refs_only(&predicate, &right_tables, &left_tables);

                    if only_left {
                        let new_left = push_predicate(LogicalPlan::Filter {
                            input: left,
                            predicate,
                        });
                        LogicalPlan::Join {
                            left: Box::new(new_left),
                            right,
                            on,
                            join_type,
                            algo_hint: None,
                        }
                    } else if only_right {
                        let new_right = push_predicate(LogicalPlan::Filter {
                            input: right,
                            predicate,
                        });
                        LogicalPlan::Join {
                            left,
                            right: Box::new(new_right),
                            on,
                            join_type,
                            algo_hint: None,
                        }
                    } else {
                        // Cannot push — leave filter above the join.
                        LogicalPlan::Filter {
                            input: Box::new(LogicalPlan::Join {
                                left,
                                right,
                                on,
                                join_type,
                                algo_hint: None,
                            }),
                            predicate,
                        }
                    }
                }

                // Filter over a Project — push through if all predicate columns
                // are reachable below the projection.
                LogicalPlan::Project { input, columns } => {
                    let pred_cols = extract_identifier_names(&predicate);
                    let proj_cols: Vec<&str> = columns.iter().map(|c| c.as_str()).collect();
                    let all_in_scope = pred_cols
                        .iter()
                        .all(|pc| proj_cols.iter().any(|prc| prc.contains(pc.as_str())));

                    if all_in_scope {
                        LogicalPlan::Project {
                            input: Box::new(push_predicate(LogicalPlan::Filter {
                                input,
                                predicate,
                            })),
                            columns,
                        }
                    } else {
                        LogicalPlan::Filter {
                            input: Box::new(LogicalPlan::Project { input, columns }),
                            predicate,
                        }
                    }
                }

                // No further push opportunity — keep filter where it is.
                other => LogicalPlan::Filter {
                    input: Box::new(other),
                    predicate,
                },
            }
        }

        // Recursively apply to all child plans.
        LogicalPlan::Project { input, columns } => LogicalPlan::Project {
            input: Box::new(push_predicate(*input)),
            columns,
        },
        LogicalPlan::Join {
            left,
            right,
            on,
            join_type,
            algo_hint,
        } => LogicalPlan::Join {
            left: Box::new(push_predicate(*left)),
            right: Box::new(push_predicate(*right)),
            on,
            join_type,
            algo_hint,
        },
        LogicalPlan::Aggregate {
            input,
            group_by,
            aggregates,
        } => LogicalPlan::Aggregate {
            input: Box::new(push_predicate(*input)),
            group_by,
            aggregates,
        },
        LogicalPlan::Sort { input, order_by } => LogicalPlan::Sort {
            input: Box::new(push_predicate(*input)),
            order_by,
        },
        LogicalPlan::Limit {
            input,
            count,
            offset,
        } => LogicalPlan::Limit {
            input: Box::new(push_predicate(*input)),
            count,
            offset,
        },
        // Leaf / terminal nodes: nothing to recurse into.
        other => other,
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Collect all table names and aliases reachable from `plan`.
fn collect_table_names(plan: &LogicalPlan) -> Vec<String> {
    let mut names = Vec::new();
    collect_table_names_inner(plan, &mut names);
    names
}

fn collect_table_names_inner(plan: &LogicalPlan, out: &mut Vec<String>) {
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
        | LogicalPlan::Limit { input, .. } => collect_table_names_inner(input, out),
        LogicalPlan::Join { left, right, .. } => {
            collect_table_names_inner(left, out);
            collect_table_names_inner(right, out);
        }
        LogicalPlan::SetOp { left, right, .. } => {
            collect_table_names_inner(left, out);
            collect_table_names_inner(right, out);
        }
        LogicalPlan::Cte { query, .. } => collect_table_names_inner(query, out),
        LogicalPlan::Window { input, .. } => collect_table_names_inner(input, out),
        LogicalPlan::Subquery { query, .. }
        | LogicalPlan::Exists {
            subquery: query, ..
        }
        | LogicalPlan::InSubquery {
            subquery: query, ..
        } => {
            collect_table_names_inner(query, out);
        }
        LogicalPlan::Compute { input, .. } => collect_table_names_inner(input, out),
        LogicalPlan::CteRef { .. } | LogicalPlan::Values { .. } | LogicalPlan::Empty => {}
    }
}

/// Return `true` when the predicate references at least one name from `owns`
/// and no name from `excludes`.
fn predicate_refs_only(predicate: &str, owns: &[String], excludes: &[String]) -> bool {
    let refs_any_exclude = excludes.iter().any(|name| {
        predicate.contains(&format!("{name}.")) || predicate_has_bare_word(predicate, name)
    });
    if refs_any_exclude {
        return false;
    }
    owns.iter().any(|name| {
        predicate.contains(&format!("{name}.")) || predicate_has_bare_word(predicate, name)
    })
}

/// Check whether `word` appears as a token boundary in `text`.
fn predicate_has_bare_word(text: &str, word: &str) -> bool {
    if word.is_empty() {
        return false;
    }
    let bytes = text.as_bytes();
    let word_bytes = word.as_bytes();
    let wlen = word_bytes.len();
    let mut start = 0;
    while start + wlen <= bytes.len() {
        if &bytes[start..start + wlen] == word_bytes {
            let before_ok = start == 0
                || !bytes
                    .get(start - 1)
                    .map(|b| b.is_ascii_alphanumeric() || *b == b'_')
                    .unwrap_or(false);
            let after_ok = (start + wlen) >= bytes.len()
                || !bytes
                    .get(start + wlen)
                    .map(|b| b.is_ascii_alphanumeric() || *b == b'_')
                    .unwrap_or(false);
            if before_ok && after_ok {
                return true;
            }
        }
        start += 1;
    }
    false
}

/// Extract simple identifier names from a predicate string via the sqlparser.
fn extract_identifier_names(predicate: &str) -> Vec<String> {
    let dialect = GenericDialect {};
    let result = Parser::new(&dialect)
        .try_with_sql(predicate)
        .ok()
        .and_then(|mut p| p.parse_expr().ok());

    let mut names = Vec::new();
    if let Some(expr) = result {
        collect_identifiers_from_expr(&expr, &mut names);
    }
    names
}

fn collect_identifiers_from_expr(expr: &sqlparser::ast::Expr, out: &mut Vec<String>) {
    use sqlparser::ast::Expr;
    match expr {
        Expr::Identifier(id) => out.push(id.value.clone()),
        Expr::CompoundIdentifier(parts) => {
            for part in parts {
                out.push(part.value.clone());
            }
        }
        Expr::BinaryOp { left, right, .. } => {
            collect_identifiers_from_expr(left, out);
            collect_identifiers_from_expr(right, out);
        }
        Expr::UnaryOp { expr, .. } => collect_identifiers_from_expr(expr, out),
        Expr::Nested(inner) => collect_identifiers_from_expr(inner, out),
        Expr::Between {
            expr, low, high, ..
        } => {
            collect_identifiers_from_expr(expr, out);
            collect_identifiers_from_expr(low, out);
            collect_identifiers_from_expr(high, out);
        }
        Expr::InList { expr, list, .. } => {
            collect_identifiers_from_expr(expr, out);
            for item in list {
                collect_identifiers_from_expr(item, out);
            }
        }
        _ => {}
    }
}
