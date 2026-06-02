//! Pass 3: Constant folding.
//!
//! Evaluates constant expressions in `Filter` predicates at compile time:
//!
//! * Always-true predicates (`1 = 1`, `TRUE`) cause the `Filter` node to be
//!   removed entirely.
//! * Always-false predicates (`1 = 2`, `FALSE`) replace the entire subtree
//!   with [`LogicalPlan::Empty`].
//! * Arithmetic on numeric literals (`5 + 3`) is folded to the result (`8`).

use crate::LogicalPlan;
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;

use super::OptPass;

// ── Public struct ─────────────────────────────────────────────────────────────

/// Evaluate constant expressions in `Filter` predicates at compile time.
pub struct ConstantFolding;

impl OptPass for ConstantFolding {
    fn name(&self) -> &'static str {
        "ConstantFolding"
    }

    fn apply(&self, plan: LogicalPlan) -> LogicalPlan {
        fold_constants(plan)
    }
}

// ── Implementation ────────────────────────────────────────────────────────────

pub(super) fn fold_constants(plan: LogicalPlan) -> LogicalPlan {
    match plan {
        LogicalPlan::Filter { input, predicate } => {
            // Recurse first so children are folded before this node.
            let inner = fold_constants(*input);

            match evaluate_constant_predicate(&predicate) {
                Some(true) => inner,               // always-true: drop the Filter
                Some(false) => LogicalPlan::Empty, // always-false: no rows possible
                None => LogicalPlan::Filter {
                    input: Box::new(inner),
                    predicate: fold_predicate_string(&predicate),
                },
            }
        }
        LogicalPlan::Project { input, columns } => LogicalPlan::Project {
            input: Box::new(fold_constants(*input)),
            columns,
        },
        LogicalPlan::Join {
            left,
            right,
            on,
            join_type,
            algo_hint,
        } => LogicalPlan::Join {
            left: Box::new(fold_constants(*left)),
            right: Box::new(fold_constants(*right)),
            on,
            join_type,
            algo_hint,
        },
        LogicalPlan::Aggregate {
            input,
            group_by,
            aggregates,
        } => LogicalPlan::Aggregate {
            input: Box::new(fold_constants(*input)),
            group_by,
            aggregates,
        },
        LogicalPlan::Sort { input, order_by } => LogicalPlan::Sort {
            input: Box::new(fold_constants(*input)),
            order_by,
        },
        LogicalPlan::Limit {
            input,
            count,
            offset,
        } => LogicalPlan::Limit {
            input: Box::new(fold_constants(*input)),
            count,
            offset,
        },
        other => other,
    }
}

// ── Predicate evaluation ──────────────────────────────────────────────────────

/// Try to evaluate a predicate string as a constant boolean.
///
/// Returns `Some(true)` for always-true, `Some(false)` for always-false,
/// `None` if the predicate is non-constant or cannot be parsed.
fn evaluate_constant_predicate(predicate: &str) -> Option<bool> {
    let dialect = GenericDialect {};
    let mut parser = Parser::new(&dialect).try_with_sql(predicate).ok()?;
    let expr = parser.parse_expr().ok()?;
    eval_expr_bool(&expr)
}

/// Recursively evaluate an expression to a boolean, returning `None` if it
/// involves non-constant sub-expressions.
fn eval_expr_bool(expr: &sqlparser::ast::Expr) -> Option<bool> {
    use sqlparser::ast::{BinaryOperator, Expr, Value, ValueWithSpan};

    match expr {
        // `TRUE` / `FALSE` literal
        Expr::Value(ValueWithSpan {
            value: Value::Boolean(b),
            ..
        }) => Some(*b),

        // Comparison or boolean operator.
        Expr::BinaryOp { left, op, right } => {
            // Try numeric comparison first.
            if let (Some(lv), Some(rv)) = (expr_to_f64(left), expr_to_f64(right)) {
                return Some(match op {
                    BinaryOperator::Eq => (lv - rv).abs() < f64::EPSILON,
                    BinaryOperator::NotEq => (lv - rv).abs() >= f64::EPSILON,
                    BinaryOperator::Lt => lv < rv,
                    BinaryOperator::LtEq => lv <= rv,
                    BinaryOperator::Gt => lv > rv,
                    BinaryOperator::GtEq => lv >= rv,
                    _ => return None,
                });
            }
            // Try string equality.
            if let (Some(ls), Some(rs)) = (expr_to_str(left), expr_to_str(right)) {
                return Some(match op {
                    BinaryOperator::Eq => ls == rs,
                    BinaryOperator::NotEq => ls != rs,
                    _ => return None,
                });
            }
            // Boolean AND / OR with known sub-values.
            match op {
                BinaryOperator::And => {
                    let lv = eval_expr_bool(left);
                    let rv = eval_expr_bool(right);
                    match (lv, rv) {
                        (Some(false), _) | (_, Some(false)) => Some(false),
                        (Some(true), Some(true)) => Some(true),
                        _ => None,
                    }
                }
                BinaryOperator::Or => {
                    let lv = eval_expr_bool(left);
                    let rv = eval_expr_bool(right);
                    match (lv, rv) {
                        (Some(true), _) | (_, Some(true)) => Some(true),
                        (Some(false), Some(false)) => Some(false),
                        _ => None,
                    }
                }
                _ => None,
            }
        }

        Expr::Nested(inner) => eval_expr_bool(inner),
        _ => None,
    }
}

// ── Arithmetic folding ────────────────────────────────────────────────────────

/// Fold arithmetic in a predicate string and return the rewritten string.
///
/// Parses the expression, folds constant arithmetic, and serialises back.
/// If parsing fails the original string is returned unchanged.
fn fold_predicate_string(predicate: &str) -> String {
    let dialect = GenericDialect {};
    let Ok(mut parser) = Parser::new(&dialect).try_with_sql(predicate) else {
        return predicate.to_string();
    };
    let Ok(expr) = parser.parse_expr() else {
        return predicate.to_string();
    };
    fold_expr(expr).to_string()
}

/// Recursively fold constant arithmetic inside an expression tree.
fn fold_expr(expr: sqlparser::ast::Expr) -> sqlparser::ast::Expr {
    use sqlparser::ast::{BinaryOperator, Expr, Value, ValueWithSpan};

    match expr {
        Expr::BinaryOp { left, op, right } => {
            let left = fold_expr(*left);
            let right = fold_expr(*right);

            // Arithmetic folding on numeric literals.
            if let (Some(lv), Some(rv)) = (expr_to_f64(&left), expr_to_f64(&right)) {
                let result = match op {
                    BinaryOperator::Plus => Some(lv + rv),
                    BinaryOperator::Minus => Some(lv - rv),
                    BinaryOperator::Multiply => Some(lv * rv),
                    BinaryOperator::Divide if rv != 0.0 => Some(lv / rv),
                    _ => None,
                };
                if let Some(v) = result {
                    let s = if v.fract() == 0.0 && v.abs() < 1e15 {
                        format!("{}", v as i64)
                    } else {
                        format!("{v}")
                    };
                    return Expr::Value(ValueWithSpan {
                        value: Value::Number(s, false),
                        span: sqlparser::tokenizer::Span::empty(),
                    });
                }
            }

            Expr::BinaryOp {
                left: Box::new(left),
                op,
                right: Box::new(right),
            }
        }
        Expr::Nested(inner) => Expr::Nested(Box::new(fold_expr(*inner))),
        other => other,
    }
}

// ── Literal extraction helpers ────────────────────────────────────────────────

/// Extract a numeric value from a literal expression if possible.
fn expr_to_f64(expr: &sqlparser::ast::Expr) -> Option<f64> {
    use sqlparser::ast::{Expr, Value, ValueWithSpan};
    match expr {
        Expr::Value(ValueWithSpan {
            value: Value::Number(s, _),
            ..
        }) => s.parse::<f64>().ok(),
        _ => None,
    }
}

/// Extract a string literal value from an expression if possible.
fn expr_to_str(expr: &sqlparser::ast::Expr) -> Option<&str> {
    use sqlparser::ast::{Expr, Value, ValueWithSpan};
    match expr {
        Expr::Value(ValueWithSpan {
            value: Value::SingleQuotedString(s),
            ..
        }) => Some(s.as_str()),
        _ => None,
    }
}
