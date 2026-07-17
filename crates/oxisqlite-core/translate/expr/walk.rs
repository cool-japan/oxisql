//! Auto-generated module
//!
//! 🤖 Generated with [SplitRS](https://github.com/cool-japan/splitrs)

use crate::schema::Affinity;
use crate::translate::plan::TableReferences;
use crate::Result;
use limbo_sqlite3_parser::ast;

/// Recursively walks an immutable expression, applying a function to each sub-expression.
pub fn walk_expr<'a, F>(expr: &'a ast::Expr, func: &mut F) -> Result<()>
where
    F: FnMut(&'a ast::Expr) -> Result<()>,
{
    func(expr)?;
    match expr {
        ast::Expr::Between {
            lhs, start, end, ..
        } => {
            walk_expr(lhs, func)?;
            walk_expr(start, func)?;
            walk_expr(end, func)?;
        }
        ast::Expr::Binary(lhs, _, rhs) => {
            walk_expr(lhs, func)?;
            walk_expr(rhs, func)?;
        }
        ast::Expr::Case {
            base,
            when_then_pairs,
            else_expr,
        } => {
            if let Some(base_expr) = base {
                walk_expr(base_expr, func)?;
            }
            for (when_expr, then_expr) in when_then_pairs {
                walk_expr(when_expr, func)?;
                walk_expr(then_expr, func)?;
            }
            if let Some(else_expr) = else_expr {
                walk_expr(else_expr, func)?;
            }
        }
        ast::Expr::Cast { expr, .. } => {
            walk_expr(expr, func)?;
        }
        ast::Expr::Collate(expr, _) => {
            walk_expr(expr, func)?;
        }
        ast::Expr::Exists(_select) | ast::Expr::Subquery(_select) => {
            // TODO: Walk through select statements if needed
        }
        ast::Expr::FunctionCall {
            args,
            order_by,
            filter_over,
            ..
        } => {
            if let Some(args) = args {
                for arg in args {
                    walk_expr(arg, func)?;
                }
            }
            if let Some(order_by) = order_by {
                for sort_col in order_by {
                    walk_expr(&sort_col.expr, func)?;
                }
            }
            if let Some(filter_over) = filter_over {
                if let Some(filter_clause) = &filter_over.filter_clause {
                    walk_expr(filter_clause, func)?;
                }
                if let Some(over_clause) = &filter_over.over_clause {
                    match over_clause.as_ref() {
                        ast::Over::Window(window) => {
                            if let Some(partition_by) = &window.partition_by {
                                for part_expr in partition_by {
                                    walk_expr(part_expr, func)?;
                                }
                            }
                            if let Some(order_by_clause) = &window.order_by {
                                for sort_col in order_by_clause {
                                    walk_expr(&sort_col.expr, func)?;
                                }
                            }
                            if let Some(frame_clause) = &window.frame_clause {
                                walk_expr_frame_bound(&frame_clause.start, func)?;
                                if let Some(end_bound) = &frame_clause.end {
                                    walk_expr_frame_bound(end_bound, func)?;
                                }
                            }
                        }
                        ast::Over::Name(_) => {}
                    }
                }
            }
        }
        ast::Expr::FunctionCallStar { filter_over, .. } => {
            if let Some(filter_over) = filter_over {
                if let Some(filter_clause) = &filter_over.filter_clause {
                    walk_expr(filter_clause, func)?;
                }
                if let Some(over_clause) = &filter_over.over_clause {
                    match over_clause.as_ref() {
                        ast::Over::Window(window) => {
                            if let Some(partition_by) = &window.partition_by {
                                for part_expr in partition_by {
                                    walk_expr(part_expr, func)?;
                                }
                            }
                            if let Some(order_by_clause) = &window.order_by {
                                for sort_col in order_by_clause {
                                    walk_expr(&sort_col.expr, func)?;
                                }
                            }
                            if let Some(frame_clause) = &window.frame_clause {
                                walk_expr_frame_bound(&frame_clause.start, func)?;
                                if let Some(end_bound) = &frame_clause.end {
                                    walk_expr_frame_bound(end_bound, func)?;
                                }
                            }
                        }
                        ast::Over::Name(_) => {}
                    }
                }
            }
        }
        ast::Expr::InList { lhs, rhs, .. } => {
            walk_expr(lhs, func)?;
            if let Some(rhs_exprs) = rhs {
                for expr in rhs_exprs {
                    walk_expr(expr, func)?;
                }
            }
        }
        ast::Expr::InSelect { lhs, rhs: _, .. } => {
            walk_expr(lhs, func)?;
            // TODO: Walk through select statements if needed
        }
        ast::Expr::InTable { lhs, args, .. } => {
            walk_expr(lhs, func)?;
            if let Some(arg_exprs) = args {
                for expr in arg_exprs {
                    walk_expr(expr, func)?;
                }
            }
        }
        ast::Expr::IsNull(expr) | ast::Expr::NotNull(expr) => {
            walk_expr(expr, func)?;
        }
        ast::Expr::Like {
            lhs, rhs, escape, ..
        } => {
            walk_expr(lhs, func)?;
            walk_expr(rhs, func)?;
            if let Some(esc_expr) = escape {
                walk_expr(esc_expr, func)?;
            }
        }
        ast::Expr::Parenthesized(exprs) => {
            for expr in exprs {
                walk_expr(expr, func)?;
            }
        }
        ast::Expr::Raise(_, expr) => {
            if let Some(raise_expr) = expr {
                walk_expr(raise_expr, func)?;
            }
        }
        ast::Expr::Unary(_, expr) => {
            walk_expr(expr, func)?;
        }
        ast::Expr::Id(_)
        | ast::Expr::Column { .. }
        | ast::Expr::RowId { .. }
        | ast::Expr::Literal(_)
        | ast::Expr::DoublyQualified(..)
        | ast::Expr::Name(_)
        | ast::Expr::Qualified(..)
        | ast::Expr::Variable(_) => {
            // No nested expressions
        }
    }
    Ok(())
}

fn walk_expr_frame_bound<'a, F>(bound: &'a ast::FrameBound, func: &mut F) -> Result<()>
where
    F: FnMut(&'a ast::Expr) -> Result<()>,
{
    match bound {
        ast::FrameBound::Following(expr) | ast::FrameBound::Preceding(expr) => {
            walk_expr(expr, func)?;
        }
        ast::FrameBound::CurrentRow
        | ast::FrameBound::UnboundedFollowing
        | ast::FrameBound::UnboundedPreceding => {}
    }

    Ok(())
}

/// Recursively walks a mutable expression, applying a function to each sub-expression.
pub fn walk_expr_mut<F>(expr: &mut ast::Expr, func: &mut F) -> Result<()>
where
    F: FnMut(&mut ast::Expr) -> Result<()>,
{
    func(expr)?;
    match expr {
        ast::Expr::Between {
            lhs, start, end, ..
        } => {
            walk_expr_mut(lhs, func)?;
            walk_expr_mut(start, func)?;
            walk_expr_mut(end, func)?;
        }
        ast::Expr::Binary(lhs, _, rhs) => {
            walk_expr_mut(lhs, func)?;
            walk_expr_mut(rhs, func)?;
        }
        ast::Expr::Case {
            base,
            when_then_pairs,
            else_expr,
        } => {
            if let Some(base_expr) = base {
                walk_expr_mut(base_expr, func)?;
            }
            for (when_expr, then_expr) in when_then_pairs {
                walk_expr_mut(when_expr, func)?;
                walk_expr_mut(then_expr, func)?;
            }
            if let Some(else_expr) = else_expr {
                walk_expr_mut(else_expr, func)?;
            }
        }
        ast::Expr::Cast { expr, .. } => {
            walk_expr_mut(expr, func)?;
        }
        ast::Expr::Collate(expr, _) => {
            walk_expr_mut(expr, func)?;
        }
        ast::Expr::Exists(_) | ast::Expr::Subquery(_) => {
            // TODO: Walk through select statements if needed
        }
        ast::Expr::FunctionCall {
            args,
            order_by,
            filter_over,
            ..
        } => {
            if let Some(args) = args {
                for arg in args {
                    walk_expr_mut(arg, func)?;
                }
            }
            if let Some(order_by) = order_by {
                for sort_col in order_by {
                    walk_expr_mut(&mut sort_col.expr, func)?;
                }
            }
            if let Some(filter_over) = filter_over {
                if let Some(filter_clause) = &mut filter_over.filter_clause {
                    walk_expr_mut(filter_clause, func)?;
                }
                if let Some(over_clause) = &mut filter_over.over_clause {
                    match over_clause.as_mut() {
                        ast::Over::Window(window) => {
                            if let Some(partition_by) = &mut window.partition_by {
                                for part_expr in partition_by {
                                    walk_expr_mut(part_expr, func)?;
                                }
                            }
                            if let Some(order_by_clause) = &mut window.order_by {
                                for sort_col in order_by_clause {
                                    walk_expr_mut(&mut sort_col.expr, func)?;
                                }
                            }
                            if let Some(frame_clause) = &mut window.frame_clause {
                                walk_expr_mut_frame_bound(&mut frame_clause.start, func)?;
                                if let Some(end_bound) = &mut frame_clause.end {
                                    walk_expr_mut_frame_bound(end_bound, func)?;
                                }
                            }
                        }
                        ast::Over::Name(_) => {}
                    }
                }
            }
        }
        ast::Expr::FunctionCallStar { filter_over, .. } => {
            if let Some(filter_over) = filter_over {
                if let Some(filter_clause) = &mut filter_over.filter_clause {
                    walk_expr_mut(filter_clause, func)?;
                }
                if let Some(over_clause) = &mut filter_over.over_clause {
                    match over_clause.as_mut() {
                        ast::Over::Window(window) => {
                            if let Some(partition_by) = &mut window.partition_by {
                                for part_expr in partition_by {
                                    walk_expr_mut(part_expr, func)?;
                                }
                            }
                            if let Some(order_by_clause) = &mut window.order_by {
                                for sort_col in order_by_clause {
                                    walk_expr_mut(&mut sort_col.expr, func)?;
                                }
                            }
                            if let Some(frame_clause) = &mut window.frame_clause {
                                walk_expr_mut_frame_bound(&mut frame_clause.start, func)?;
                                if let Some(end_bound) = &mut frame_clause.end {
                                    walk_expr_mut_frame_bound(end_bound, func)?;
                                }
                            }
                        }
                        ast::Over::Name(_) => {}
                    }
                }
            }
        }
        ast::Expr::InList { lhs, rhs, .. } => {
            walk_expr_mut(lhs, func)?;
            if let Some(rhs_exprs) = rhs {
                for expr in rhs_exprs {
                    walk_expr_mut(expr, func)?;
                }
            }
        }
        ast::Expr::InSelect { lhs, rhs: _, .. } => {
            walk_expr_mut(lhs, func)?;
            // TODO: Walk through select statements if needed
        }
        ast::Expr::InTable { lhs, args, .. } => {
            walk_expr_mut(lhs, func)?;
            if let Some(arg_exprs) = args {
                for expr in arg_exprs {
                    walk_expr_mut(expr, func)?;
                }
            }
        }
        ast::Expr::IsNull(expr) | ast::Expr::NotNull(expr) => {
            walk_expr_mut(expr, func)?;
        }
        ast::Expr::Like {
            lhs, rhs, escape, ..
        } => {
            walk_expr_mut(lhs, func)?;
            walk_expr_mut(rhs, func)?;
            if let Some(esc_expr) = escape {
                walk_expr_mut(esc_expr, func)?;
            }
        }
        ast::Expr::Parenthesized(exprs) => {
            for expr in exprs {
                walk_expr_mut(expr, func)?;
            }
        }
        ast::Expr::Raise(_, expr) => {
            if let Some(raise_expr) = expr {
                walk_expr_mut(raise_expr, func)?;
            }
        }
        ast::Expr::Unary(_, expr) => {
            walk_expr_mut(expr, func)?;
        }
        ast::Expr::Id(_)
        | ast::Expr::Column { .. }
        | ast::Expr::RowId { .. }
        | ast::Expr::Literal(_)
        | ast::Expr::DoublyQualified(..)
        | ast::Expr::Name(_)
        | ast::Expr::Qualified(..)
        | ast::Expr::Variable(_) => {
            // No nested expressions
        }
    }

    Ok(())
}

fn walk_expr_mut_frame_bound<F>(bound: &mut ast::FrameBound, func: &mut F) -> Result<()>
where
    F: FnMut(&mut ast::Expr) -> Result<()>,
{
    match bound {
        ast::FrameBound::Following(expr) | ast::FrameBound::Preceding(expr) => {
            walk_expr_mut(expr, func)?;
        }
        ast::FrameBound::CurrentRow
        | ast::FrameBound::UnboundedFollowing
        | ast::FrameBound::UnboundedPreceding => {}
    }

    Ok(())
}

pub fn get_expr_affinity(
    expr: &ast::Expr,
    referenced_tables: Option<&TableReferences>,
) -> Affinity {
    match expr {
        ast::Expr::Column { table, column, .. } => {
            if let Some(tables) = referenced_tables {
                if let Some(table_ref) = tables.find_table_by_internal_id(*table) {
                    if let Some(col) = table_ref.get_column_at(*column) {
                        return col.affinity();
                    }
                }
            }
            Affinity::Blob
        }
        ast::Expr::Cast { type_name, .. } => {
            if let Some(type_name) = type_name {
                crate::schema::affinity(&type_name.name)
            } else {
                Affinity::Blob
            }
        }
        ast::Expr::Collate(expr, _) => get_expr_affinity(expr, referenced_tables),
        // Literals have NO affinity in SQLite!
        ast::Expr::Literal(_) => Affinity::Blob, // No affinity!
        _ => Affinity::Blob,                     // This may need to change. For now this works.
    }
}

pub fn comparison_affinity(
    lhs_expr: &ast::Expr,
    rhs_expr: &ast::Expr,
    referenced_tables: Option<&TableReferences>,
) -> Affinity {
    let mut aff = get_expr_affinity(lhs_expr, referenced_tables);

    aff = compare_affinity(rhs_expr, aff, referenced_tables);

    // If no affinity determined (both operands are literals), default to BLOB
    if !aff.has_affinity() {
        Affinity::Blob
    } else {
        aff
    }
}

pub fn compare_affinity(
    expr: &ast::Expr,
    other_affinity: Affinity,
    referenced_tables: Option<&TableReferences>,
) -> Affinity {
    let expr_affinity = get_expr_affinity(expr, referenced_tables);

    if expr_affinity.has_affinity() && other_affinity.has_affinity() {
        // Both sides have affinity - use numeric if either is numeric
        if expr_affinity.is_numeric() || other_affinity.is_numeric() {
            Affinity::Numeric
        } else {
            Affinity::Blob
        }
    } else {
        // One or both sides have no affinity - use the one that does, or Blob if neither
        if expr_affinity.has_affinity() {
            expr_affinity
        } else if other_affinity.has_affinity() {
            other_affinity
        } else {
            Affinity::Blob
        }
    }
}
