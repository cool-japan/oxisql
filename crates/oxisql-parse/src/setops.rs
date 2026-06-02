//! Set-operation helpers for the `oxisql-parse` query planner.
//!
//! Provides the mapping from sqlparser's [`SetOperator`] / [`SetQuantifier`]
//! to OxiSQL's [`SetOpType`] and helper to recursively build a [`LogicalPlan`]
//! for `UNION`, `INTERSECT`, and `EXCEPT` expressions.

use oxisql_core::OxiSqlError;
use sqlparser::ast::{SetExpr, SetOperator, SetQuantifier};

use crate::{LogicalPlan, SetOpType};

/// Map a sqlparser [`SetOperator`] to our [`SetOpType`].
///
/// Returns an error for `Minus` (non-standard) since we have no equivalent.
pub(crate) fn map_set_operator(op: &SetOperator) -> Result<SetOpType, OxiSqlError> {
    match op {
        SetOperator::Union => Ok(SetOpType::Union),
        SetOperator::Intersect => Ok(SetOpType::Intersect),
        SetOperator::Except => Ok(SetOpType::Except),
        SetOperator::Minus => Ok(SetOpType::Except), // MINUS is an alias for EXCEPT
    }
}

/// Return `true` when the [`SetQuantifier`] indicates `ALL` semantics.
///
/// Only `SetQuantifier::All` maps to `all = true`; `Distinct` and `None`
/// both map to `false`.
pub(crate) fn quantifier_is_all(q: &SetQuantifier) -> bool {
    matches!(q, SetQuantifier::All | SetQuantifier::AllByName)
}

/// Recursively build a [`LogicalPlan`] from a [`SetExpr`] that is a
/// `SetOperation` node.
///
/// The left and right sub-expressions are planned with the supplied `planner`
/// callback (passed in to avoid a circular module dependency).
pub(crate) fn plan_set_operation(
    op: &SetOperator,
    set_quantifier: &SetQuantifier,
    left: &SetExpr,
    right: &SetExpr,
    planner: &dyn Fn(&SetExpr) -> Result<LogicalPlan, OxiSqlError>,
) -> Result<LogicalPlan, OxiSqlError> {
    let set_op = map_set_operator(op)?;
    let all = quantifier_is_all(set_quantifier);
    let left_plan = planner(left)?;
    let right_plan = planner(right)?;
    Ok(LogicalPlan::SetOp {
        op: set_op,
        all,
        left: Box::new(left_plan),
        right: Box::new(right_plan),
    })
}
