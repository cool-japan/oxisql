//! Window-function helpers for the `oxisql-parse` query planner.
//!
//! Provides utilities for detecting window functions in a SELECT projection
//! and building [`WindowFunctionDef`] from sqlparser's [`Function`] and
//! [`WindowSpec`] types.

use sqlparser::ast::{
    Expr, FunctionArg, FunctionArgExpr, FunctionArguments, OrderByExpr, SelectItem, WindowType,
};

use crate::{SortExpr, WindowFunctionDef};

/// Return `true` if this `SelectItem` contains a windowed function call
/// (i.e. `Expr::Function { over: Some(_), .. }`).
pub(crate) fn select_item_is_windowed(item: &SelectItem) -> bool {
    match item {
        SelectItem::UnnamedExpr(e)
        | SelectItem::ExprWithAlias { expr: e, .. }
        | SelectItem::ExprWithAliases { expr: e, .. } => expr_is_windowed(e),
        _ => false,
    }
}

/// Recursively check if `expr` is (or contains) a window function call.
fn expr_is_windowed(expr: &Expr) -> bool {
    match expr {
        Expr::Function(f) => f.over.is_some(),
        Expr::BinaryOp { left, right, .. } => expr_is_windowed(left) || expr_is_windowed(right),
        Expr::Nested(inner) => expr_is_windowed(inner),
        _ => false,
    }
}

/// Extract a [`WindowFunctionDef`] from a windowed `SelectItem`.
///
/// Returns `None` if the item does not contain a window function at the
/// top-level expression (nested windows are not extracted).
pub(crate) fn extract_window_def(item: &SelectItem) -> Option<WindowFunctionDef> {
    let (expr, alias_override) = match item {
        SelectItem::UnnamedExpr(e) => (e, None),
        SelectItem::ExprWithAlias { expr: e, alias: a } => (e, Some(a.value.clone())),
        SelectItem::ExprWithAliases {
            expr: e,
            aliases: a,
        } => (e, a.first().map(|id| id.value.clone())),
        _ => return None,
    };

    let func = match expr {
        Expr::Function(f) if f.over.is_some() => f,
        _ => return None,
    };

    let name = func.name.to_string();

    // Extract function argument expressions as strings.
    let args: Vec<String> = match &func.args {
        FunctionArguments::List(arg_list) => arg_list
            .args
            .iter()
            .filter_map(|arg| match arg {
                FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => Some(e.to_string()),
                FunctionArg::Unnamed(FunctionArgExpr::Wildcard) => Some("*".to_string()),
                FunctionArg::Named {
                    arg: FunctionArgExpr::Expr(e),
                    ..
                } => Some(e.to_string()),
                _ => None,
            })
            .collect(),
        _ => vec![],
    };

    // Extract window spec from the OVER clause.
    let (partition_by, order_by) = match func.over.as_ref() {
        Some(WindowType::WindowSpec(spec)) => {
            let pb: Vec<String> = spec.partition_by.iter().map(|e| e.to_string()).collect();
            let ob: Vec<SortExpr> = spec
                .order_by
                .iter()
                .map(order_by_expr_to_sort_expr)
                .collect();
            (pb, ob)
        }
        Some(WindowType::NamedWindow(ident)) => {
            // Named window — we only capture the name as a single partition_by entry
            // for now; full named-window resolution is out of scope.
            (vec![ident.value.clone()], vec![])
        }
        None => (vec![], vec![]),
    };

    let alias = alias_override.unwrap_or_else(|| name.clone());

    Some(WindowFunctionDef {
        name,
        args,
        partition_by,
        order_by,
        alias,
    })
}

/// Convert a sqlparser [`OrderByExpr`] to an OxiSQL [`SortExpr`].
fn order_by_expr_to_sort_expr(e: &OrderByExpr) -> SortExpr {
    SortExpr {
        column: e.expr.to_string(),
        ascending: e.options.asc.unwrap_or(true),
    }
}
