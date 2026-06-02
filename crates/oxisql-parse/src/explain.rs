//! Human-readable plan explanation for [`LogicalPlan`]s.

use crate::plan::{JoinType, LogicalPlan, SetOpType};

// ── Plan explanation ─────────────────────────────────────────────────────────

/// Render a [`LogicalPlan`] as a human-readable indented tree string.
///
/// Each level is indented by two spaces per depth.  Example:
///
/// ```text
/// Limit [count=10]
///   Sort [age ASC]
///     Filter [age > 18]
///       Scan [users]
/// ```
pub fn explain(plan: &LogicalPlan) -> String {
    let mut out = String::new();
    explain_inner(plan, 0, &mut out);
    // Trim trailing newline for a clean return value.
    if out.ends_with('\n') {
        out.pop();
    }
    out
}

pub(crate) fn explain_inner(plan: &LogicalPlan, depth: usize, out: &mut String) {
    let indent = "  ".repeat(depth);
    match plan {
        LogicalPlan::Scan {
            table,
            alias,
            limit,
        } => {
            let limit_str = limit.map(|l| format!(" limit={l}")).unwrap_or_default();
            if let Some(a) = alias {
                out.push_str(&format!("{indent}Scan [{table} AS {a}{limit_str}]\n"));
            } else {
                out.push_str(&format!("{indent}Scan [{table}{limit_str}]\n"));
            }
        }
        LogicalPlan::Filter { input, predicate } => {
            out.push_str(&format!("{indent}Filter [{predicate}]\n"));
            explain_inner(input, depth + 1, out);
        }
        LogicalPlan::Project { input, columns } => {
            let cols = columns.join(", ");
            out.push_str(&format!("{indent}Project [{cols}]\n"));
            explain_inner(input, depth + 1, out);
        }
        LogicalPlan::Join {
            left,
            right,
            on,
            join_type,
            ..
        } => {
            let jt = match join_type {
                JoinType::Inner => "INNER",
                JoinType::Left => "LEFT",
                JoinType::Right => "RIGHT",
                JoinType::Full => "FULL",
                JoinType::Cross => "CROSS",
            };
            if on.is_empty() {
                out.push_str(&format!("{indent}Join [{jt}]\n"));
            } else {
                out.push_str(&format!("{indent}Join [{jt} ON {on}]\n"));
            }
            explain_inner(left, depth + 1, out);
            explain_inner(right, depth + 1, out);
        }
        LogicalPlan::Aggregate {
            input,
            group_by,
            aggregates,
        } => {
            let gb = group_by.join(", ");
            let agg = aggregates.join(", ");
            out.push_str(&format!(
                "{indent}Aggregate [group_by=[{gb}] agg=[{agg}]]\n"
            ));
            explain_inner(input, depth + 1, out);
        }
        LogicalPlan::Sort { input, order_by } => {
            let keys: Vec<String> = order_by
                .iter()
                .map(|s| {
                    if s.ascending {
                        format!("{} ASC", s.column)
                    } else {
                        format!("{} DESC", s.column)
                    }
                })
                .collect();
            out.push_str(&format!("{indent}Sort [{}]\n", keys.join(", ")));
            explain_inner(input, depth + 1, out);
        }
        LogicalPlan::Limit {
            input,
            count,
            offset,
        } => {
            let mut parts = Vec::new();
            if let Some(c) = count {
                parts.push(format!("count={c}"));
            }
            if let Some(o) = offset {
                parts.push(format!("offset={o}"));
            }
            out.push_str(&format!("{indent}Limit [{}]\n", parts.join(", ")));
            explain_inner(input, depth + 1, out);
        }
        LogicalPlan::Values { columns, rows } => {
            let cols = if columns.is_empty() {
                String::new()
            } else {
                format!(" cols=[{}]", columns.join(", "))
            };
            out.push_str(&format!("{indent}Values [rows={rows}{cols}]\n"));
        }
        LogicalPlan::Empty => {
            out.push_str(&format!("{indent}Empty\n"));
        }
        LogicalPlan::SetOp {
            op,
            all,
            left,
            right,
        } => {
            let op_str = match op {
                SetOpType::Union => "UNION",
                SetOpType::Intersect => "INTERSECT",
                SetOpType::Except => "EXCEPT",
            };
            let all_str = if *all { " ALL" } else { "" };
            out.push_str(&format!("{indent}SetOp [{op_str}{all_str}]\n"));
            explain_inner(left, depth + 1, out);
            explain_inner(right, depth + 1, out);
        }
        LogicalPlan::Cte {
            name,
            query,
            recursive,
        } => {
            let rec = if *recursive { " RECURSIVE" } else { "" };
            out.push_str(&format!("{indent}Cte [{name}{rec}]\n"));
            explain_inner(query, depth + 1, out);
        }
        LogicalPlan::CteRef { name } => {
            out.push_str(&format!("{indent}CteRef [{name}]\n"));
        }
        LogicalPlan::Window { input, functions } => {
            let names: Vec<&str> = functions.iter().map(|f| f.name.as_str()).collect();
            out.push_str(&format!("{indent}Window [{}]\n", names.join(", ")));
            explain_inner(input, depth + 1, out);
        }
        LogicalPlan::Subquery { query, alias } => {
            if let Some(a) = alias {
                out.push_str(&format!("{indent}Subquery [alias={a}]\n"));
            } else {
                out.push_str(&format!("{indent}Subquery\n"));
            }
            explain_inner(query, depth + 1, out);
        }
        LogicalPlan::Exists { subquery, negated } => {
            let neg = if *negated { "NOT " } else { "" };
            out.push_str(&format!("{indent}Exists [{neg}EXISTS]\n"));
            explain_inner(subquery, depth + 1, out);
        }
        LogicalPlan::InSubquery {
            expr,
            subquery,
            negated,
        } => {
            let neg = if *negated { "NOT " } else { "" };
            out.push_str(&format!("{indent}InSubquery [{expr} {neg}IN]\n"));
            explain_inner(subquery, depth + 1, out);
        }
    }
}
