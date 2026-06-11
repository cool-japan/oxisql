//! Human-readable plan explanation for [`LogicalPlan`]s.

use crate::cost::{CostModel, NodeCost};
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

// ── Verbose explain ──────────────────────────────────────────────────────────

/// Like [`explain`], but appends `(rows=N, cost=M)` to each line.
///
/// Costs are computed by the supplied [`CostModel`].
pub fn explain_verbose(plan: &LogicalPlan, model: &CostModel) -> String {
    let costs = model.explain_costs(plan);
    let mut out = String::new();
    explain_verbose_inner(plan, &costs, 0, &mut out);
    if out.ends_with('\n') {
        out.pop();
    }
    out
}

fn explain_verbose_inner(plan: &LogicalPlan, cost: &NodeCost, depth: usize, out: &mut String) {
    // Capture the plain-explain line for this node, then strip its trailing
    // newline and append the cost annotation before adding our own newline.
    let mut line = String::new();
    explain_inner(plan, depth, &mut line);
    // `line` may end with '\n'; strip it.
    let line = line.trim_end_matches('\n');

    let rows = cost.estimate.rows as u64;
    let cost_val = cost.estimate.total_cost;
    out.push_str(&format!("{line} (rows={rows}, cost={cost_val:.1})\n"));

    // Recurse into children, pairing plan children with cost children.
    let plan_children = plan_children_ref(plan);
    for (child_plan, child_cost) in plan_children.iter().zip(cost.children.iter()) {
        explain_verbose_inner(child_plan, child_cost, depth + 1, out);
    }
}

/// Return references to the direct plan children of `plan` in the same order
/// that [`explain_inner`] recurses into them.
fn plan_children_ref(plan: &LogicalPlan) -> Vec<&LogicalPlan> {
    match plan {
        LogicalPlan::Filter { input, .. }
        | LogicalPlan::Project { input, .. }
        | LogicalPlan::Aggregate { input, .. }
        | LogicalPlan::Sort { input, .. }
        | LogicalPlan::Limit { input, .. }
        | LogicalPlan::Window { input, .. }
        | LogicalPlan::Cte { query: input, .. }
        | LogicalPlan::Subquery { query: input, .. }
        | LogicalPlan::Exists {
            subquery: input, ..
        }
        | LogicalPlan::Compute { input, .. } => vec![input.as_ref()],

        LogicalPlan::InSubquery { subquery, .. } => vec![subquery.as_ref()],

        LogicalPlan::Join { left, right, .. } | LogicalPlan::SetOp { left, right, .. } => {
            vec![left.as_ref(), right.as_ref()]
        }

        LogicalPlan::Scan { .. }
        | LogicalPlan::Values { .. }
        | LogicalPlan::Empty
        | LogicalPlan::CteRef { .. } => vec![],
    }
}

// ── JSON explain ─────────────────────────────────────────────────────────────

/// Emit the plan as JSON, optionally with per-node cost estimates.
///
/// Uses a hand-rolled JSON writer — no `serde_json` runtime dependency.
/// Example output:
/// ```json
/// {"op":"Scan","table":"orders","rows":50000,"cost":50000.0,"children":[]}
/// ```
pub fn explain_json(plan: &LogicalPlan, model: Option<&CostModel>) -> String {
    let costs = model.map(|m| m.explain_costs(plan));
    let mut out = String::new();
    emit_json(plan, costs.as_ref(), &mut out);
    out
}

fn emit_json(plan: &LogicalPlan, cost: Option<&NodeCost>, out: &mut String) {
    out.push('{');

    // "op": "<name>"
    let op_name = node_op_name(plan);
    out.push_str("\"op\":");
    json_string(op_name, out);

    // Per-variant extra fields.
    emit_json_fields(plan, out);

    // Cost fields when a model is supplied.
    if let Some(c) = cost {
        let rows = c.estimate.rows as u64;
        let cost_val = c.estimate.total_cost;
        out.push_str(&format!(",\"rows\":{rows},\"cost\":{cost_val:.1}"));
    }

    // Children.
    let plan_children = plan_children_ref(plan);
    let cost_children: Vec<Option<&NodeCost>> = if let Some(c) = cost {
        c.children.iter().map(Some).collect()
    } else {
        plan_children.iter().map(|_| None).collect()
    };

    out.push_str(",\"children\":[");
    for (i, (child_plan, child_cost)) in plan_children.iter().zip(cost_children.iter()).enumerate()
    {
        if i > 0 {
            out.push(',');
        }
        emit_json(child_plan, *child_cost, out);
    }
    out.push_str("]}");
}

/// Short operator name for the JSON `"op"` field.
fn node_op_name(plan: &LogicalPlan) -> &'static str {
    match plan {
        LogicalPlan::Scan { .. } => "Scan",
        LogicalPlan::Filter { .. } => "Filter",
        LogicalPlan::Project { .. } => "Project",
        LogicalPlan::Join { .. } => "Join",
        LogicalPlan::Aggregate { .. } => "Aggregate",
        LogicalPlan::Sort { .. } => "Sort",
        LogicalPlan::Limit { .. } => "Limit",
        LogicalPlan::Values { .. } => "Values",
        LogicalPlan::Empty => "Empty",
        LogicalPlan::SetOp { .. } => "SetOp",
        LogicalPlan::Cte { .. } => "Cte",
        LogicalPlan::CteRef { .. } => "CteRef",
        LogicalPlan::Window { .. } => "Window",
        LogicalPlan::Subquery { .. } => "Subquery",
        LogicalPlan::Exists { .. } => "Exists",
        LogicalPlan::InSubquery { .. } => "InSubquery",
        LogicalPlan::Compute { .. } => "Compute",
    }
}

/// Emit variant-specific JSON fields (after `"op"`, before `"rows"`/`"children"`).
fn emit_json_fields(plan: &LogicalPlan, out: &mut String) {
    match plan {
        LogicalPlan::Scan {
            table,
            alias,
            limit,
        } => {
            out.push_str(",\"table\":");
            json_string(table, out);
            if let Some(a) = alias {
                out.push_str(",\"alias\":");
                json_string(a, out);
            }
            if let Some(l) = limit {
                out.push_str(&format!(",\"limit\":{l}"));
            }
        }
        LogicalPlan::Filter { predicate, .. } => {
            out.push_str(",\"predicate\":");
            json_string(predicate, out);
        }
        LogicalPlan::Project { columns, .. } => {
            out.push_str(",\"columns\":[");
            for (i, c) in columns.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                json_string(c, out);
            }
            out.push(']');
        }
        LogicalPlan::Join { on, join_type, .. } => {
            let jt = match join_type {
                JoinType::Inner => "INNER",
                JoinType::Left => "LEFT",
                JoinType::Right => "RIGHT",
                JoinType::Full => "FULL",
                JoinType::Cross => "CROSS",
                JoinType::LeftSemi => "SEMI",
                JoinType::LeftAnti => "ANTI",
            };
            out.push_str(",\"join_type\":");
            json_string(jt, out);
            if !on.is_empty() {
                out.push_str(",\"on\":");
                json_string(on, out);
            }
        }
        LogicalPlan::Aggregate {
            group_by,
            aggregates,
            ..
        } => {
            out.push_str(",\"group_by\":[");
            for (i, g) in group_by.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                json_string(g, out);
            }
            out.push_str("],\"aggregates\":[");
            for (i, a) in aggregates.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                json_string(a, out);
            }
            out.push(']');
        }
        LogicalPlan::Sort { order_by, .. } => {
            out.push_str(",\"order_by\":[");
            for (i, s) in order_by.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                let dir = if s.ascending { "ASC" } else { "DESC" };
                out.push('{');
                out.push_str("\"column\":");
                json_string(&s.column, out);
                out.push_str(",\"dir\":");
                json_string(dir, out);
                out.push('}');
            }
            out.push(']');
        }
        LogicalPlan::Limit { count, offset, .. } => {
            if let Some(c) = count {
                out.push_str(&format!(",\"count\":{c}"));
            }
            if let Some(o) = offset {
                out.push_str(&format!(",\"offset\":{o}"));
            }
        }
        LogicalPlan::Values { columns, rows } => {
            out.push_str(&format!(",\"row_count\":{rows}"));
            out.push_str(",\"columns\":[");
            for (i, c) in columns.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                json_string(c, out);
            }
            out.push(']');
        }
        LogicalPlan::SetOp { op, all, .. } => {
            let op_str = match op {
                SetOpType::Union => "UNION",
                SetOpType::Intersect => "INTERSECT",
                SetOpType::Except => "EXCEPT",
            };
            out.push_str(",\"set_op\":");
            json_string(op_str, out);
            out.push_str(&format!(",\"all\":{all}"));
        }
        LogicalPlan::Cte {
            name, recursive, ..
        } => {
            out.push_str(",\"name\":");
            json_string(name, out);
            out.push_str(&format!(",\"recursive\":{recursive}"));
        }
        LogicalPlan::CteRef { name } => {
            out.push_str(",\"name\":");
            json_string(name, out);
        }
        LogicalPlan::Window { functions, .. } => {
            out.push_str(",\"functions\":[");
            for (i, f) in functions.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push('{');
                out.push_str("\"name\":");
                json_string(&f.name, out);
                out.push_str(",\"alias\":");
                json_string(&f.alias, out);
                out.push('}');
            }
            out.push(']');
        }
        LogicalPlan::Subquery { alias, .. } => {
            if let Some(a) = alias {
                out.push_str(",\"alias\":");
                json_string(a, out);
            }
        }
        LogicalPlan::Exists { negated, .. } => {
            out.push_str(&format!(",\"negated\":{negated}"));
        }
        LogicalPlan::InSubquery { expr, negated, .. } => {
            out.push_str(",\"expr\":");
            json_string(expr, out);
            out.push_str(&format!(",\"negated\":{negated}"));
        }
        LogicalPlan::Compute { bindings, .. } => {
            out.push_str(",\"bindings\":[");
            for (i, (alias, expr)) in bindings.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push('{');
                out.push_str("\"alias\":");
                json_string(alias, out);
                out.push_str(",\"expr\":");
                json_string(expr, out);
                out.push('}');
            }
            out.push(']');
        }
        LogicalPlan::Empty => {}
    }
}

/// Write `s` as a JSON string literal into `out`, escaping special characters.
fn json_string(s: &str, out: &mut String) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
}

// ── Plain explain ────────────────────────────────────────────────────────────

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
                JoinType::LeftSemi => "SEMI",
                JoinType::LeftAnti => "ANTI",
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
        LogicalPlan::Compute { input, bindings } => {
            let bind_str: Vec<String> = bindings
                .iter()
                .map(|(alias, expr)| format!("{alias} = {expr}"))
                .collect();
            out.push_str(&format!("{indent}Compute [{}]\n", bind_str.join(", ")));
            explain_inner(input, depth + 1, out);
        }
    }
}
