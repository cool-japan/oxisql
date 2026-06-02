//! SQL statement planner: converts parsed AST statements to [`LogicalPlan`]s.

use oxisql_core::OxiSqlError;
use sqlparser::ast::Expr;

use crate::plan::{JoinType, LogicalPlan, SortExpr, WindowFunctionDef};
use crate::{agg, setops, window};

// ── Planner functions ────────────────────────────────────────────────────────

/// Transform a parsed [`sqlparser::ast::Statement`] into a [`LogicalPlan`].
///
/// Supports `SELECT` (via [`plan_query`]), `INSERT`, `UPDATE`, and `DELETE`.
/// All other statement types return [`LogicalPlan::Empty`].
///
/// # Errors
///
/// Returns [`OxiSqlError::Parse`] if the statement structure is unexpected.
pub fn plan_statement(stmt: &sqlparser::ast::Statement) -> Result<LogicalPlan, OxiSqlError> {
    match stmt {
        sqlparser::ast::Statement::Query(query) => plan_query(query),
        sqlparser::ast::Statement::Insert(insert) => {
            let columns: Vec<String> = insert
                .columns
                .iter()
                .filter_map(|col| {
                    col.0
                        .last()
                        .and_then(|p| p.as_ident())
                        .map(|id| id.value.clone())
                })
                .collect();
            let rows = insert
                .source
                .as_ref()
                .and_then(|src| {
                    if let sqlparser::ast::SetExpr::Values(ref vals) = *src.body {
                        Some(vals.rows.len())
                    } else {
                        None
                    }
                })
                .unwrap_or(0);
            Ok(LogicalPlan::Values { columns, rows })
        }
        sqlparser::ast::Statement::Update(update) => {
            let (table, alias) = table_factor_name_alias(&update.table.relation);
            let base = LogicalPlan::Scan {
                table,
                alias,
                limit: None,
            };
            Ok(match &update.selection {
                Some(pred) => LogicalPlan::Filter {
                    input: Box::new(base),
                    predicate: pred.to_string(),
                },
                None => base,
            })
        }
        sqlparser::ast::Statement::Delete(delete) => {
            let base = match &delete.from {
                sqlparser::ast::FromTable::WithFromKeyword(twjs) => build_scan_from_twjs(twjs),
                sqlparser::ast::FromTable::WithoutKeyword(twjs) => build_scan_from_twjs(twjs),
            };
            Ok(match &delete.selection {
                Some(pred) => LogicalPlan::Filter {
                    input: Box::new(base),
                    predicate: pred.to_string(),
                },
                None => base,
            })
        }
        _ => Ok(LogicalPlan::Empty),
    }
}

/// Build a [`LogicalPlan`] from a parsed `SELECT`/`UNION`/subquery.
///
/// # Errors
///
/// Returns [`OxiSqlError::Parse`] for unsupported `SetExpr` variants.
pub fn plan_query(query: &sqlparser::ast::Query) -> Result<LogicalPlan, OxiSqlError> {
    let mut plan = plan_set_expr(&query.body)?;

    // ORDER BY
    if let Some(ref order_by) = query.order_by {
        if let sqlparser::ast::OrderByKind::Expressions(ref exprs) = order_by.kind {
            if !exprs.is_empty() {
                let sort_keys: Vec<SortExpr> = exprs
                    .iter()
                    .map(|e| SortExpr {
                        column: e.expr.to_string(),
                        ascending: e.options.asc.unwrap_or(true),
                    })
                    .collect();
                plan = LogicalPlan::Sort {
                    input: Box::new(plan),
                    order_by: sort_keys,
                };
            }
        }
    }

    // LIMIT / OFFSET
    if let Some(ref limit_clause) = query.limit_clause {
        let (count, offset) = extract_limit_offset(limit_clause);
        if count.is_some() || offset.is_some() {
            plan = LogicalPlan::Limit {
                input: Box::new(plan),
                count,
                offset,
            };
        }
    }

    // WITH / CTE — wrap the already-built body plan in Cte nodes, one per CTE
    // in declaration order (last CTE wraps the body, each prior one wraps the next).
    if let Some(ref with) = query.with {
        let recursive = with.recursive;
        for cte in with.cte_tables.iter().rev() {
            let cte_name = cte.alias.name.value.clone();
            let inner_plan = plan_query(&cte.query)?;
            plan = LogicalPlan::Cte {
                name: cte_name,
                query: Box::new(inner_plan),
                recursive,
            };
        }
    }

    Ok(plan)
}

/// Walk a `SetExpr`, building the inner part of the plan (up to ORDER BY/LIMIT).
pub(crate) fn plan_set_expr(body: &sqlparser::ast::SetExpr) -> Result<LogicalPlan, OxiSqlError> {
    match body {
        sqlparser::ast::SetExpr::Select(sel) => plan_select(sel),
        sqlparser::ast::SetExpr::Query(inner) => plan_query(inner),
        sqlparser::ast::SetExpr::SetOperation {
            op,
            set_quantifier,
            left,
            right,
        } => setops::plan_set_operation(op, set_quantifier, left, right, &plan_set_expr),
        _ => Ok(LogicalPlan::Empty),
    }
}

/// Extract a scalar subquery plan from a SELECT projection item, or `None`.
fn extract_subquery_from_select_item(
    item: &sqlparser::ast::SelectItem,
) -> Result<Option<LogicalPlan>, OxiSqlError> {
    let expr = match item {
        sqlparser::ast::SelectItem::UnnamedExpr(e) => e,
        sqlparser::ast::SelectItem::ExprWithAlias { expr: e, .. } => e,
        _ => return Ok(None),
    };
    if let Expr::Subquery(query) = expr {
        Ok(Some(plan_query(query)?))
    } else {
        Ok(None)
    }
}

/// Build the plan for a `SELECT` node.
pub(crate) fn plan_select(sel: &sqlparser::ast::Select) -> Result<LogicalPlan, OxiSqlError> {
    // 1 + 2. FROM + JOINs
    let mut plan = build_from_plan(&sel.from);

    // 3. WHERE — special-case Exists / InSubquery; fall through to Filter otherwise.
    if let Some(ref pred) = sel.selection {
        match pred {
            Expr::Exists { subquery, negated } => {
                let inner = plan_query(subquery)?;
                plan = LogicalPlan::Exists {
                    subquery: Box::new(inner),
                    negated: *negated,
                };
            }
            Expr::InSubquery {
                expr,
                subquery,
                negated,
            } => {
                let inner = plan_query(subquery)?;
                plan = LogicalPlan::InSubquery {
                    expr: expr.to_string(),
                    subquery: Box::new(inner),
                    negated: *negated,
                };
            }
            other => {
                plan = LogicalPlan::Filter {
                    input: Box::new(plan),
                    predicate: other.to_string(),
                };
            }
        }
    }

    // 4. GROUP BY / aggregates
    let group_by_exprs: Vec<String> = match &sel.group_by {
        sqlparser::ast::GroupByExpr::Expressions(exprs, _) => {
            exprs.iter().map(|e| e.to_string()).collect()
        }
        sqlparser::ast::GroupByExpr::All(_) => vec!["*".to_string()],
    };
    let aggregates: Vec<String> = sel
        .projection
        .iter()
        .filter(|item| agg::projection_item_is_aggregate(item))
        .map(|item| item.to_string())
        .collect();
    if !group_by_exprs.is_empty() || !aggregates.is_empty() {
        plan = LogicalPlan::Aggregate {
            input: Box::new(plan),
            group_by: group_by_exprs,
            aggregates,
        };
        // HAVING — wrap the Aggregate in a Filter node.
        if let Some(ref having_expr) = sel.having {
            plan = LogicalPlan::Filter {
                input: Box::new(plan),
                predicate: having_expr.to_string(),
            };
        }
    }

    // 5. Projection — skip if pure `SELECT *`
    let all_wildcard = sel.projection.iter().all(|item| {
        matches!(
            item,
            sqlparser::ast::SelectItem::Wildcard(_)
                | sqlparser::ast::SelectItem::QualifiedWildcard(_, _)
        )
    });
    if !all_wildcard
        && !matches!(plan, LogicalPlan::Aggregate { .. })
        && !matches!(plan, LogicalPlan::Filter {
            input: ref i, ..
        } if matches!(i.as_ref(), LogicalPlan::Aggregate { .. }))
    {
        // Collect scalar subqueries from the projection.
        let subquery_plans: Vec<LogicalPlan> = sel
            .projection
            .iter()
            .filter_map(|item| extract_subquery_from_select_item(item).ok().flatten())
            .collect();
        if !subquery_plans.is_empty() {
            for sub_plan in subquery_plans {
                let alias = sel.projection.iter().find_map(|it| match it {
                    sqlparser::ast::SelectItem::ExprWithAlias {
                        expr: Expr::Subquery(_),
                        alias,
                    } => Some(alias.value.clone()),
                    _ => None,
                });
                plan = LogicalPlan::Subquery {
                    query: Box::new(sub_plan),
                    alias,
                };
            }
        } else {
            let columns: Vec<String> = sel.projection.iter().map(|item| item.to_string()).collect();
            plan = LogicalPlan::Project {
                input: Box::new(plan),
                columns,
            };
        }
    }

    // 6. Window functions — wrap in a Window node if any OVER clauses are present.
    let window_defs: Vec<WindowFunctionDef> = sel
        .projection
        .iter()
        .filter(|item| window::select_item_is_windowed(item))
        .filter_map(window::extract_window_def)
        .collect();
    if !window_defs.is_empty() {
        plan = LogicalPlan::Window {
            input: Box::new(plan),
            functions: window_defs,
        };
    }

    Ok(plan)
}

/// Build a plan from the `FROM` clause (a list of `TableWithJoins`).
///
/// Multiple FROM items are treated as a cross-join chain.
pub(crate) fn build_from_plan(from: &[sqlparser::ast::TableWithJoins]) -> LogicalPlan {
    if from.is_empty() {
        return LogicalPlan::Empty;
    }

    // Build first table's plan (including its explicit JOINs).
    let mut plan = build_table_with_joins_plan(&from[0]);

    // Additional FROM items → cross-join.
    for twj in from.iter().skip(1) {
        let right = build_table_with_joins_plan(twj);
        plan = LogicalPlan::Join {
            left: Box::new(plan),
            right: Box::new(right),
            on: String::new(),
            join_type: JoinType::Cross,
            algo_hint: None,
        };
    }

    plan
}

/// Build a plan from one `TableWithJoins` entry (base relation + explicit joins).
fn build_table_with_joins_plan(twj: &sqlparser::ast::TableWithJoins) -> LogicalPlan {
    let (table, alias) = table_factor_name_alias(&twj.relation);
    let mut plan = LogicalPlan::Scan {
        table,
        alias,
        limit: None,
    };

    for join in &twj.joins {
        let (jt, alias) = table_factor_name_alias(&join.relation);
        let right = LogicalPlan::Scan {
            table: jt,
            alias,
            limit: None,
        };

        let (join_type, on_expr) = join_operator_info(&join.join_operator);

        plan = LogicalPlan::Join {
            left: Box::new(plan),
            right: Box::new(right),
            on: on_expr,
            join_type,
            algo_hint: None,
        };
    }

    plan
}

/// Extract table name and alias from a `TableFactor`.
pub(crate) fn table_factor_name_alias(
    factor: &sqlparser::ast::TableFactor,
) -> (String, Option<String>) {
    match factor {
        sqlparser::ast::TableFactor::Table { name, alias, .. } => {
            let tname = name.to_string();
            let talias = alias.as_ref().map(|a| a.name.value.clone());
            (tname, talias)
        }
        other => (other.to_string(), None),
    }
}

/// Extract `JoinType` and ON expression string from a `JoinOperator`.
fn join_operator_info(op: &sqlparser::ast::JoinOperator) -> (JoinType, String) {
    let constraint_on = |c: &sqlparser::ast::JoinConstraint| -> String {
        match c {
            sqlparser::ast::JoinConstraint::On(expr) => expr.to_string(),
            _ => String::new(),
        }
    };

    match op {
        sqlparser::ast::JoinOperator::Join(c)
        | sqlparser::ast::JoinOperator::Inner(c)
        | sqlparser::ast::JoinOperator::Semi(c)
        | sqlparser::ast::JoinOperator::LeftSemi(c)
        | sqlparser::ast::JoinOperator::RightSemi(c)
        | sqlparser::ast::JoinOperator::Anti(c)
        | sqlparser::ast::JoinOperator::LeftAnti(c)
        | sqlparser::ast::JoinOperator::RightAnti(c)
        | sqlparser::ast::JoinOperator::StraightJoin(c) => (JoinType::Inner, constraint_on(c)),
        sqlparser::ast::JoinOperator::Left(c) | sqlparser::ast::JoinOperator::LeftOuter(c) => {
            (JoinType::Left, constraint_on(c))
        }
        sqlparser::ast::JoinOperator::Right(c) | sqlparser::ast::JoinOperator::RightOuter(c) => {
            (JoinType::Right, constraint_on(c))
        }
        sqlparser::ast::JoinOperator::FullOuter(c) => (JoinType::Full, constraint_on(c)),
        sqlparser::ast::JoinOperator::CrossJoin(c) => (JoinType::Cross, constraint_on(c)),
        _ => (JoinType::Inner, String::new()),
    }
}

/// Build a scan from a slice of `TableWithJoins` (used by DELETE).
pub(crate) fn build_scan_from_twjs(twjs: &[sqlparser::ast::TableWithJoins]) -> LogicalPlan {
    build_from_plan(twjs)
}

/// Extract `(count, offset)` from a [`sqlparser::ast::LimitClause`].
fn extract_limit_offset(clause: &sqlparser::ast::LimitClause) -> (Option<u64>, Option<u64>) {
    match clause {
        sqlparser::ast::LimitClause::LimitOffset { limit, offset, .. } => {
            let count = limit.as_ref().and_then(expr_to_u64);
            let off = offset.as_ref().and_then(|o| expr_to_u64(&o.value));
            (count, off)
        }
        sqlparser::ast::LimitClause::OffsetCommaLimit { offset, limit } => {
            let count = expr_to_u64(limit);
            let off = expr_to_u64(offset);
            (count, Some(off.unwrap_or(0)))
        }
    }
}

/// Try to extract a `u64` from a literal `Expr::Value(Number(..))`.
fn expr_to_u64(expr: &Expr) -> Option<u64> {
    if let Expr::Value(sqlparser::ast::ValueWithSpan {
        value: sqlparser::ast::Value::Number(ref s, _),
        ..
    }) = expr
    {
        s.parse::<u64>().ok()
    } else {
        None
    }
}
