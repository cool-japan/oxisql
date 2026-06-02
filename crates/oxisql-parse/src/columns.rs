//! Column-extraction helpers for the `oxisql-parse` crate.
//!
//! Walks the sqlparser AST to collect column name references from various
//! statement types (SELECT, INSERT, UPDATE, DELETE).  Used by the public
//! [`extract_columns`](crate::extract_columns) function.

use sqlparser::ast::Statement;

/// Collect all column references from `stmt` into `cols`.
pub(crate) fn collect_columns_from_statement(stmt: &Statement, cols: &mut Vec<String>) {
    match stmt {
        Statement::Query(query) => collect_columns_from_query(query, cols),
        Statement::Insert(insert) => {
            for col in &insert.columns {
                if let Some(last) = col.0.last() {
                    if let Some(ident) = last.as_ident() {
                        cols.push(ident.value.clone());
                    }
                }
            }
            if let Some(ref src) = insert.source {
                if let sqlparser::ast::SetExpr::Select(ref sel) = *src.body {
                    collect_columns_from_select(sel, cols);
                }
            }
        }
        Statement::Update(update) => {
            for assignment in &update.assignments {
                if let sqlparser::ast::AssignmentTarget::ColumnName(col) = &assignment.target {
                    if let Some(last) = col.0.last() {
                        if let Some(ident) = last.as_ident() {
                            cols.push(ident.value.clone());
                        }
                    }
                }
            }
            if let Some(ref sel) = update.selection {
                collect_columns_from_expr(sel, cols);
            }
        }
        Statement::Delete(delete) => {
            if let Some(ref sel) = delete.selection {
                collect_columns_from_expr(sel, cols);
            }
        }
        _ => {}
    }
}

fn collect_columns_from_query(query: &sqlparser::ast::Query, cols: &mut Vec<String>) {
    if let sqlparser::ast::SetExpr::Select(ref sel) = *query.body {
        collect_columns_from_select(sel, cols);
    }
    if let Some(ref order_by) = query.order_by {
        if let sqlparser::ast::OrderByKind::Expressions(ref exprs) = order_by.kind {
            for expr in exprs {
                collect_columns_from_expr(&expr.expr, cols);
            }
        }
    }
}

fn collect_columns_from_select(sel: &sqlparser::ast::Select, cols: &mut Vec<String>) {
    for item in &sel.projection {
        match item {
            sqlparser::ast::SelectItem::UnnamedExpr(expr) => {
                collect_columns_from_expr(expr, cols);
            }
            sqlparser::ast::SelectItem::ExprWithAlias { expr, .. } => {
                collect_columns_from_expr(expr, cols);
            }
            sqlparser::ast::SelectItem::ExprWithAliases { expr, .. } => {
                collect_columns_from_expr(expr, cols);
            }
            sqlparser::ast::SelectItem::QualifiedWildcard(_, _) => {}
            sqlparser::ast::SelectItem::Wildcard(_) => {}
        }
    }
    if let Some(ref where_expr) = sel.selection {
        collect_columns_from_expr(where_expr, cols);
    }
    match &sel.group_by {
        sqlparser::ast::GroupByExpr::Expressions(exprs, _) => {
            for group_expr in exprs {
                collect_columns_from_expr(group_expr, cols);
            }
        }
        sqlparser::ast::GroupByExpr::All(_) => {}
    }
    if let Some(ref having_expr) = sel.having {
        collect_columns_from_expr(having_expr, cols);
    }
    for item in &sel.from {
        for join in &item.joins {
            let constraint = match &join.join_operator {
                sqlparser::ast::JoinOperator::Join(c)
                | sqlparser::ast::JoinOperator::Inner(c)
                | sqlparser::ast::JoinOperator::Left(c)
                | sqlparser::ast::JoinOperator::LeftOuter(c)
                | sqlparser::ast::JoinOperator::Right(c)
                | sqlparser::ast::JoinOperator::RightOuter(c)
                | sqlparser::ast::JoinOperator::FullOuter(c)
                | sqlparser::ast::JoinOperator::CrossJoin(c)
                | sqlparser::ast::JoinOperator::Semi(c)
                | sqlparser::ast::JoinOperator::LeftSemi(c)
                | sqlparser::ast::JoinOperator::RightSemi(c)
                | sqlparser::ast::JoinOperator::Anti(c)
                | sqlparser::ast::JoinOperator::LeftAnti(c)
                | sqlparser::ast::JoinOperator::RightAnti(c)
                | sqlparser::ast::JoinOperator::StraightJoin(c) => Some(c),
                _ => None,
            };
            if let Some(sqlparser::ast::JoinConstraint::On(expr)) = constraint {
                collect_columns_from_expr(expr, cols);
            }
        }
    }
}

fn collect_columns_from_expr(expr: &sqlparser::ast::Expr, cols: &mut Vec<String>) {
    match expr {
        sqlparser::ast::Expr::Identifier(ident) => {
            cols.push(ident.value.clone());
        }
        sqlparser::ast::Expr::CompoundIdentifier(parts) => {
            if let Some(last) = parts.last() {
                cols.push(last.value.clone());
            }
        }
        sqlparser::ast::Expr::BinaryOp { left, right, .. } => {
            collect_columns_from_expr(left, cols);
            collect_columns_from_expr(right, cols);
        }
        sqlparser::ast::Expr::UnaryOp { expr, .. } => {
            collect_columns_from_expr(expr, cols);
        }
        sqlparser::ast::Expr::IsNull(inner) | sqlparser::ast::Expr::IsNotNull(inner) => {
            collect_columns_from_expr(inner, cols);
        }
        sqlparser::ast::Expr::Between {
            expr, low, high, ..
        } => {
            collect_columns_from_expr(expr, cols);
            collect_columns_from_expr(low, cols);
            collect_columns_from_expr(high, cols);
        }
        sqlparser::ast::Expr::InList { expr, list, .. } => {
            collect_columns_from_expr(expr, cols);
            for item in list {
                collect_columns_from_expr(item, cols);
            }
        }
        sqlparser::ast::Expr::Cast { expr, .. } => {
            collect_columns_from_expr(expr, cols);
        }
        sqlparser::ast::Expr::Function(f) => {
            if let sqlparser::ast::FunctionArguments::List(ref arg_list) = f.args {
                for arg in &arg_list.args {
                    if let sqlparser::ast::FunctionArg::Unnamed(
                        sqlparser::ast::FunctionArgExpr::Expr(ref e),
                    ) = arg
                    {
                        collect_columns_from_expr(e, cols);
                    }
                }
            }
        }
        sqlparser::ast::Expr::Nested(inner) => {
            collect_columns_from_expr(inner, cols);
        }
        _ => {}
    }
}
