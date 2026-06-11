//! Predicate-simplification pass.
//!
//! Rewrites `Filter` predicates to a simpler, semantically-equivalent form:
//!
//! * Boolean-algebra identities (`x AND TRUE → x`, `x AND FALSE → FALSE`, …)
//! * Constant numeric comparison folding (`1 = 1 → TRUE`)
//! * De-Morgan / NOT-elimination (`NOT(NOT x) → x`, `NOT(a < b) → a >= b`)
//! * Idempotence (`x AND x → x`)
//! * Complement laws (`x AND NOT x → FALSE`, `x OR NOT x → TRUE`)
//! * Single-element IN folding (`a IN (1) → a = 1`)
//! * Per-column range coalescing across conjuncts
//!   (`a > 5 AND a > 3 → a > 5`, `a > 10 AND a < 5 → FALSE`)
//! * Equality dominance (`a = 5 AND a > 3 → a = 5`)

use std::collections::HashMap;

use sqlparser::ast::{BinaryOperator, Expr, UnaryOperator, Value, ValueWithSpan};
use sqlparser::tokenizer::Span;

use super::expr_util::{canonical_hash, join_conjuncts, parse_predicate, render, split_conjuncts};
use super::OptPass;
use crate::LogicalPlan;

// ── Public pass struct ────────────────────────────────────────────────────────

/// Simplify predicates in `Filter` and `Join` nodes to their canonical form.
pub struct PredicateSimplification;

impl OptPass for PredicateSimplification {
    fn name(&self) -> &'static str {
        "PredicateSimplification"
    }

    fn apply(&self, plan: LogicalPlan) -> LogicalPlan {
        simplify_plan(plan)
    }
}

// ── Plan-level walk ──────────────────────────────────────────────────────────

pub(super) fn simplify_plan(plan: LogicalPlan) -> LogicalPlan {
    match plan {
        LogicalPlan::Filter { input, predicate } => {
            let inner = simplify_plan(*input);
            let (new_pred, always_true) = simplify_predicate_str(&predicate);
            if always_true {
                return inner;
            }
            // Detect always-false by checking if the simplified expr is FALSE literal.
            let is_always_false = parse_predicate(&new_pred)
                .and_then(|e| is_bool_lit(&e))
                .map(|b| !b)
                .unwrap_or(false);
            if is_always_false {
                return LogicalPlan::Empty;
            }
            LogicalPlan::Filter {
                input: Box::new(inner),
                predicate: new_pred,
            }
        }

        LogicalPlan::Join {
            left,
            right,
            on,
            join_type,
            algo_hint,
        } => {
            let new_on = if on.is_empty() {
                on
            } else {
                simplify_predicate_str(&on).0
            };
            LogicalPlan::Join {
                left: Box::new(simplify_plan(*left)),
                right: Box::new(simplify_plan(*right)),
                on: new_on,
                join_type,
                algo_hint,
            }
        }

        LogicalPlan::Project { input, columns } => LogicalPlan::Project {
            input: Box::new(simplify_plan(*input)),
            columns,
        },
        LogicalPlan::Aggregate {
            input,
            group_by,
            aggregates,
        } => LogicalPlan::Aggregate {
            input: Box::new(simplify_plan(*input)),
            group_by,
            aggregates,
        },
        LogicalPlan::Sort { input, order_by } => LogicalPlan::Sort {
            input: Box::new(simplify_plan(*input)),
            order_by,
        },
        LogicalPlan::Limit {
            input,
            count,
            offset,
        } => LogicalPlan::Limit {
            input: Box::new(simplify_plan(*input)),
            count,
            offset,
        },
        LogicalPlan::Window { input, functions } => LogicalPlan::Window {
            input: Box::new(simplify_plan(*input)),
            functions,
        },
        LogicalPlan::Cte {
            name,
            query,
            recursive,
        } => LogicalPlan::Cte {
            name,
            query: Box::new(simplify_plan(*query)),
            recursive,
        },
        LogicalPlan::Subquery { query, alias } => LogicalPlan::Subquery {
            query: Box::new(simplify_plan(*query)),
            alias,
        },
        LogicalPlan::Exists { subquery, negated } => LogicalPlan::Exists {
            subquery: Box::new(simplify_plan(*subquery)),
            negated,
        },
        LogicalPlan::InSubquery {
            expr,
            subquery,
            negated,
        } => LogicalPlan::InSubquery {
            expr,
            subquery: Box::new(simplify_plan(*subquery)),
            negated,
        },
        LogicalPlan::Compute { input, bindings } => LogicalPlan::Compute {
            input: Box::new(simplify_plan(*input)),
            bindings,
        },
        LogicalPlan::SetOp {
            op,
            all,
            left,
            right,
        } => LogicalPlan::SetOp {
            op,
            all,
            left: Box::new(simplify_plan(*left)),
            right: Box::new(simplify_plan(*right)),
        },
        // Leaves: Scan, Values, Empty, CteRef
        other => other,
    }
}

// ── Predicate-string simplification ─────────────────────────────────────────

/// Simplify a predicate string to a fixpoint.
///
/// Returns `(simplified_predicate, is_always_true)`.
/// On parse failure the original string is returned unchanged with `false`.
pub(super) fn simplify_predicate_str(pred: &str) -> (String, bool) {
    let Some(mut e) = parse_predicate(pred) else {
        return (pred.to_string(), false);
    };
    for _ in 0..20 {
        let new_e = simplify_expr(e.clone());
        let same = canonical_hash(&new_e) == canonical_hash(&e);
        e = new_e;
        if same {
            break;
        }
    }
    let is_true = is_bool_lit(&e) == Some(true);
    (render(&e), is_true)
}

// ── Expression simplifier ────────────────────────────────────────────────────

/// Recursively simplify an expression bottom-up to a fixpoint.
pub(crate) fn simplify_expr(e: Expr) -> Expr {
    // Step 1: recurse into children so rules apply bottom-up.
    let e = apply_to_children(e);
    // Step 2: apply single-node rules.
    let e = apply_bool_rules(e);
    // Step 3: conjunct-level range reasoning if this is an AND tree.
    let conjuncts = split_conjuncts(e.clone());
    if conjuncts.len() > 1 {
        let (simplified, always_false) = simplify_conjuncts(conjuncts);
        if always_false {
            return make_bool(false);
        }
        match join_conjuncts(simplified) {
            Some(joined) => joined,
            None => make_bool(true),
        }
    } else {
        e
    }
}

/// Apply `simplify_expr` to every direct child of `e`.
fn apply_to_children(e: Expr) -> Expr {
    match e {
        Expr::BinaryOp { left, op, right } => Expr::BinaryOp {
            left: Box::new(simplify_expr(*left)),
            op,
            right: Box::new(simplify_expr(*right)),
        },
        Expr::UnaryOp { op, expr } => Expr::UnaryOp {
            op,
            expr: Box::new(simplify_expr(*expr)),
        },
        Expr::Nested(inner) => Expr::Nested(Box::new(simplify_expr(*inner))),
        Expr::Between {
            expr,
            negated,
            low,
            high,
        } => Expr::Between {
            expr: Box::new(simplify_expr(*expr)),
            negated,
            low: Box::new(simplify_expr(*low)),
            high: Box::new(simplify_expr(*high)),
        },
        Expr::InList {
            expr,
            list,
            negated,
        } => Expr::InList {
            expr: Box::new(simplify_expr(*expr)),
            list: list.into_iter().map(simplify_expr).collect(),
            negated,
        },
        other => other,
    }
}

// ── Boolean algebra rules ────────────────────────────────────────────────────

fn apply_bool_rules(e: Expr) -> Expr {
    use BinaryOperator as B;
    use UnaryOperator as U;

    match e {
        // Unwrap nested (already recursed, just strip the wrapper)
        Expr::Nested(inner) => *inner,

        Expr::BinaryOp { left, op, right } => {
            let l = *left;
            let r = *right;

            // Numeric literal comparison folding: 1 = 1 → TRUE, 1 = 2 → FALSE, etc.
            if let (Some(lv), Some(rv)) = (numeric_val(&l), numeric_val(&r)) {
                let folded = match &op {
                    B::Eq => Some((lv - rv).abs() < f64::EPSILON),
                    B::NotEq => Some((lv - rv).abs() >= f64::EPSILON),
                    B::Lt => Some(lv < rv),
                    B::LtEq => Some(lv <= rv),
                    B::Gt => Some(lv > rv),
                    B::GtEq => Some(lv >= rv),
                    _ => None,
                };
                if let Some(b) = folded {
                    return make_bool(b);
                }
            }

            match op {
                B::And => {
                    if is_bool_lit(&l) == Some(true) {
                        return r;
                    }
                    if is_bool_lit(&r) == Some(true) {
                        return l;
                    }
                    if is_bool_lit(&l) == Some(false) {
                        return make_bool(false);
                    }
                    if is_bool_lit(&r) == Some(false) {
                        return make_bool(false);
                    }
                    // Idempotence: x AND x → x
                    if canonical_hash(&l) == canonical_hash(&r) {
                        return l;
                    }
                    // Complement: x AND NOT(x) → FALSE
                    if let Expr::UnaryOp {
                        op: U::Not,
                        expr: ref neg,
                    } = r
                    {
                        if canonical_hash(&l) == canonical_hash(neg) {
                            return make_bool(false);
                        }
                    }
                    if let Expr::UnaryOp {
                        op: U::Not,
                        expr: ref neg,
                    } = l
                    {
                        if canonical_hash(&r) == canonical_hash(neg) {
                            return make_bool(false);
                        }
                    }
                    Expr::BinaryOp {
                        left: Box::new(l),
                        op: B::And,
                        right: Box::new(r),
                    }
                }
                B::Or => {
                    if is_bool_lit(&l) == Some(false) {
                        return r;
                    }
                    if is_bool_lit(&r) == Some(false) {
                        return l;
                    }
                    if is_bool_lit(&l) == Some(true) {
                        return make_bool(true);
                    }
                    if is_bool_lit(&r) == Some(true) {
                        return make_bool(true);
                    }
                    // Idempotence: x OR x → x
                    if canonical_hash(&l) == canonical_hash(&r) {
                        return l;
                    }
                    // Complement: x OR NOT(x) → TRUE
                    if let Expr::UnaryOp {
                        op: U::Not,
                        expr: ref neg,
                    } = r
                    {
                        if canonical_hash(&l) == canonical_hash(neg) {
                            return make_bool(true);
                        }
                    }
                    if let Expr::UnaryOp {
                        op: U::Not,
                        expr: ref neg,
                    } = l
                    {
                        if canonical_hash(&r) == canonical_hash(neg) {
                            return make_bool(true);
                        }
                    }
                    Expr::BinaryOp {
                        left: Box::new(l),
                        op: B::Or,
                        right: Box::new(r),
                    }
                }
                _ => Expr::BinaryOp {
                    left: Box::new(l),
                    op,
                    right: Box::new(r),
                },
            }
        }

        Expr::UnaryOp { op, expr } => {
            let inner = *expr;
            match op {
                U::Not => {
                    // NOT NOT x → x
                    if let Expr::UnaryOp {
                        op: U::Not,
                        expr: inner2,
                    } = inner
                    {
                        return *inner2;
                    }
                    // NOT TRUE → FALSE, NOT FALSE → TRUE
                    if let Some(b) = is_bool_lit(&inner) {
                        return make_bool(!b);
                    }
                    // NOT comparison → flip
                    if let Expr::BinaryOp {
                        ref left,
                        ref op,
                        ref right,
                    } = inner
                    {
                        if let Some(neg_op) = negate_comparison(op) {
                            return Expr::BinaryOp {
                                left: left.clone(),
                                op: neg_op,
                                right: right.clone(),
                            };
                        }
                    }
                    Expr::UnaryOp {
                        op: U::Not,
                        expr: Box::new(inner),
                    }
                }
                _ => Expr::UnaryOp {
                    op,
                    expr: Box::new(inner),
                },
            }
        }

        // IN (single item) → equality or inequality
        Expr::InList {
            expr,
            mut list,
            negated,
        } if list.len() == 1 => {
            let item = list.remove(0);
            Expr::BinaryOp {
                left: expr,
                op: if negated {
                    BinaryOperator::NotEq
                } else {
                    BinaryOperator::Eq
                },
                right: Box::new(item),
            }
        }

        other => other,
    }
}

// ── Per-column range reasoning ────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum BoundKind {
    Gt,
    GtEq,
    Lt,
    LtEq,
    Eq,
}

#[derive(Debug, Clone)]
struct ColNumericBound {
    kind: BoundKind,
    value: f64,
    original: Expr,
}

/// Simplify a list of conjuncts using per-column range reasoning.
///
/// Returns `(simplified_conjuncts, always_false)`.
fn simplify_conjuncts(conjuncts: Vec<Expr>) -> (Vec<Expr>, bool) {
    let mut col_groups: HashMap<String, Vec<ColNumericBound>> = HashMap::new();
    let mut other: Vec<Expr> = Vec::new();

    for e in conjuncts {
        if let Some((col_key, bound)) = extract_numeric_col_bound(&e) {
            col_groups.entry(col_key).or_default().push(bound);
        } else {
            other.push(e);
        }
    }

    let mut result = other;
    for (_col_key, bounds) in col_groups {
        let (simplified_exprs, is_false) = merge_numeric_bounds(bounds);
        if is_false {
            return (vec![], true);
        }
        result.extend(simplified_exprs);
    }

    (result, false)
}

/// Try to extract a single-column numeric bound from `e`.
///
/// Returns `Some((col_key, bound))` for comparisons of the form `col op lit`
/// or `lit op col`, where `col` is a bare or qualified identifier and `lit` is
/// a numeric literal.
fn extract_numeric_col_bound(e: &Expr) -> Option<(String, ColNumericBound)> {
    let Expr::BinaryOp { left, op, right } = e else {
        return None;
    };

    // col op lit (normal orientation)
    if let Some(col_key) = expr_col_key(left) {
        if let Some(val) = numeric_val(right) {
            let kind = op_to_bound_kind(op, false)?;
            return Some((
                col_key,
                ColNumericBound {
                    kind,
                    value: val,
                    original: e.clone(),
                },
            ));
        }
    }

    // lit op col (flipped orientation, e.g. 5 < a ≡ a > 5)
    if let Some(col_key) = expr_col_key(right) {
        if let Some(val) = numeric_val(left) {
            let kind = op_to_bound_kind(op, true)?;
            return Some((
                col_key,
                ColNumericBound {
                    kind,
                    value: val,
                    original: e.clone(),
                },
            ));
        }
    }

    None
}

fn expr_col_key(e: &Expr) -> Option<String> {
    match e {
        Expr::Identifier(id) => Some(id.value.clone()),
        Expr::CompoundIdentifier(parts) => Some(
            parts
                .iter()
                .map(|p| p.value.as_str())
                .collect::<Vec<_>>()
                .join("."),
        ),
        _ => None,
    }
}

fn op_to_bound_kind(op: &BinaryOperator, flipped: bool) -> Option<BoundKind> {
    let kind = match op {
        BinaryOperator::Gt => {
            if flipped {
                BoundKind::Lt
            } else {
                BoundKind::Gt
            }
        }
        BinaryOperator::GtEq => {
            if flipped {
                BoundKind::LtEq
            } else {
                BoundKind::GtEq
            }
        }
        BinaryOperator::Lt => {
            if flipped {
                BoundKind::Gt
            } else {
                BoundKind::Lt
            }
        }
        BinaryOperator::LtEq => {
            if flipped {
                BoundKind::GtEq
            } else {
                BoundKind::LtEq
            }
        }
        BinaryOperator::Eq => BoundKind::Eq,
        _ => return None,
    };
    Some(kind)
}

/// Merge a set of single-column numeric bounds into the strictest satisfiable set.
///
/// Returns `(simplified_exprs, always_false)`.
fn merge_numeric_bounds(bounds: Vec<ColNumericBound>) -> (Vec<Expr>, bool) {
    let mut equalities: Vec<(f64, Expr)> = Vec::new();
    // (value, is_strict, original)
    let mut lower_bounds: Vec<(f64, bool, Expr)> = Vec::new();
    let mut upper_bounds: Vec<(f64, bool, Expr)> = Vec::new();

    for b in bounds {
        match b.kind {
            BoundKind::Gt => lower_bounds.push((b.value, true, b.original)),
            BoundKind::GtEq => lower_bounds.push((b.value, false, b.original)),
            BoundKind::Lt => upper_bounds.push((b.value, true, b.original)),
            BoundKind::LtEq => upper_bounds.push((b.value, false, b.original)),
            BoundKind::Eq => equalities.push((b.value, b.original)),
        }
    }

    // Equality contradiction: two distinct equality values
    if equalities.len() > 1 {
        let first = equalities[0].0;
        if equalities
            .iter()
            .any(|(v, _)| (*v - first).abs() > f64::EPSILON)
        {
            return (vec![], true);
        }
    }

    // Strictest lower bound: highest value; prefer strict (Gt) over inclusive (GtEq) at same value.
    let best_lower = lower_bounds.into_iter().max_by(|a, b| {
        a.0.partial_cmp(&b.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| match (a.1, b.1) {
                (true, false) => std::cmp::Ordering::Greater,
                (false, true) => std::cmp::Ordering::Less,
                _ => std::cmp::Ordering::Equal,
            })
    });

    // Strictest upper bound: lowest value; prefer strict (Lt) over inclusive (LtEq) at same value.
    let best_upper = upper_bounds.into_iter().min_by(|a, b| {
        a.0.partial_cmp(&b.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| match (a.1, b.1) {
                (true, false) => std::cmp::Ordering::Greater,
                (false, true) => std::cmp::Ordering::Less,
                _ => std::cmp::Ordering::Equal,
            })
    });

    // Equality dominance: equality must be within [lower, upper]
    if let Some((eq_val, eq_expr)) = equalities.into_iter().next() {
        if let Some((lb, strict, _)) = &best_lower {
            let violates = if *strict { eq_val <= *lb } else { eq_val < *lb };
            if violates {
                return (vec![], true);
            }
        }
        if let Some((ub, strict, _)) = &best_upper {
            let violates = if *strict { eq_val >= *ub } else { eq_val > *ub };
            if violates {
                return (vec![], true);
            }
        }
        // Equality dominates all bounds.
        return (vec![eq_expr], false);
    }

    // Check lower-upper contradiction
    if let (Some((lb, lb_strict, _)), Some((ub, ub_strict, _))) = (&best_lower, &best_upper) {
        if lb > ub {
            return (vec![], true);
        }
        if (lb - ub).abs() < f64::EPSILON && (*lb_strict || *ub_strict) {
            return (vec![], true);
        }
    }

    let mut result = Vec::new();
    if let Some((_, _, orig)) = best_lower {
        result.push(orig);
    }
    if let Some((_, _, orig)) = best_upper {
        result.push(orig);
    }
    (result, false)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn make_bool(b: bool) -> Expr {
    Expr::Value(ValueWithSpan {
        value: Value::Boolean(b),
        span: Span::empty(),
    })
}

fn is_bool_lit(e: &Expr) -> Option<bool> {
    match e {
        Expr::Value(ValueWithSpan {
            value: Value::Boolean(b),
            ..
        }) => Some(*b),
        _ => None,
    }
}

fn numeric_val(e: &Expr) -> Option<f64> {
    match e {
        Expr::Value(ValueWithSpan {
            value: Value::Number(s, _),
            ..
        }) => s.parse::<f64>().ok(),
        _ => None,
    }
}

fn negate_comparison(op: &BinaryOperator) -> Option<BinaryOperator> {
    match op {
        BinaryOperator::Eq => Some(BinaryOperator::NotEq),
        BinaryOperator::NotEq => Some(BinaryOperator::Eq),
        BinaryOperator::Lt => Some(BinaryOperator::GtEq),
        BinaryOperator::LtEq => Some(BinaryOperator::Gt),
        BinaryOperator::Gt => Some(BinaryOperator::LtEq),
        BinaryOperator::GtEq => Some(BinaryOperator::Lt),
        _ => None,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::optimizer::expr_util::parse_predicate;

    fn simplify_str(pred: &str) -> String {
        simplify_predicate_str(pred).0
    }

    fn simplify_is_true(pred: &str) -> bool {
        simplify_predicate_str(pred).1
    }

    fn filter(pred: &str) -> LogicalPlan {
        LogicalPlan::Filter {
            input: Box::new(LogicalPlan::Scan {
                table: "t".to_string(),
                alias: None,
                limit: None,
            }),
            predicate: pred.to_string(),
        }
    }

    // 1. TRUE identity eliminates
    #[test]
    fn test_true_identity() {
        let pred = "1 = 1 AND x > 0";
        assert_eq!(simplify_str(pred), "x > 0");
        assert!(!simplify_is_true(pred));
    }

    // 2. AND FALSE → Empty
    #[test]
    fn test_and_false_empty() {
        let plan = filter("x > 0 AND FALSE");
        let result = PredicateSimplification.apply(plan);
        assert!(matches!(result, LogicalPlan::Empty));
    }

    // 3. Range coalescing: a > 5 AND a > 3 → a > 5
    #[test]
    fn test_range_coalesce_lower_bound() {
        let simplified = simplify_str("a > 5 AND a > 3");
        // Should be just a > 5
        assert!(
            simplified.contains("> 5") || simplified.contains(">5"),
            "got: {simplified}"
        );
        assert!(
            !simplified.contains("> 3"),
            "should not contain > 3: {simplified}"
        );
    }

    // 4. Contradiction: a > 10 AND a < 5 → Empty
    #[test]
    fn test_contradiction_empty() {
        let plan = filter("a > 10 AND a < 5");
        let result = PredicateSimplification.apply(plan);
        assert!(matches!(result, LogicalPlan::Empty), "got {result:?}");
    }

    // 5. Equality contradiction: a = 5 AND a = 6 → Empty
    #[test]
    fn test_equality_contradiction() {
        let plan = filter("a = 5 AND a = 6");
        let result = PredicateSimplification.apply(plan);
        assert!(matches!(result, LogicalPlan::Empty), "got {result:?}");
    }

    // 6. Idempotence: x > 0 AND x > 0 → x > 0
    #[test]
    fn test_idempotence() {
        let simplified = simplify_str("x > 0 AND x > 0");
        let e = parse_predicate(&simplified).expect("parse");
        // Should be a single expression (no AND)
        assert!(
            !matches!(
                e,
                Expr::BinaryOp {
                    op: BinaryOperator::And,
                    ..
                }
            ),
            "expected single conjunct, got: {simplified}"
        );
    }

    // 7. IN (1) → equality
    #[test]
    fn test_in_single_element() {
        let simplified = simplify_str("a IN (1)");
        // Should be rewritten to a = 1
        assert!(
            simplified.contains('=') && !simplified.contains("IN"),
            "expected equality, got: {simplified}"
        );
    }

    // 8. NOT NOT elimination
    #[test]
    fn test_not_not_elimination() {
        let simplified = simplify_str("NOT (NOT (x > 0))");
        assert!(
            simplified.contains('>'),
            "expected x > 0, got: {simplified}"
        );
    }

    // 9. NOT comparison flip
    #[test]
    fn test_not_comparison_flip() {
        let simplified = simplify_str("NOT (a < b)");
        // NOT(a < b) → a >= b
        assert!(simplified.contains(">="), "expected >=, got: {simplified}");
    }

    // 10. Equality dominance: a = 5 AND a > 3 → a = 5
    #[test]
    fn test_equality_dominance() {
        let simplified = simplify_str("a = 5 AND a > 3");
        assert!(
            simplified.contains("= 5"),
            "expected a = 5, got: {simplified}"
        );
        assert!(
            !simplified.contains("> 3"),
            "should not contain > 3: {simplified}"
        );
    }
}
