//! Pass 4: Limit push-through.
//!
//! Push a `Limit` node down into a single-table `Scan` when there are no
//! aggregations or joins between the limit and the scan.  This allows
//! the execution engine to stop reading rows early at the storage level.
//!
//! The pushed limit is stored in `Scan.limit`.  If a `Scan` already carries a
//! limit (from a previous pass or a nested query), the more restrictive value
//! is kept.

use crate::LogicalPlan;

use super::OptPass;

// ── Public struct ─────────────────────────────────────────────────────────────

/// Push a `Limit` node down into a single-table `Scan`.
pub struct LimitPushThrough;

impl OptPass for LimitPushThrough {
    fn name(&self) -> &'static str {
        "LimitPushThrough"
    }

    fn apply(&self, plan: LogicalPlan) -> LogicalPlan {
        push_limit(plan)
    }
}

// ── Implementation ────────────────────────────────────────────────────────────

pub(super) fn push_limit(plan: LogicalPlan) -> LogicalPlan {
    match plan {
        LogicalPlan::Limit {
            input,
            count,
            offset,
        } => {
            let inner = push_limit(*input);
            // Only push when: count is specified, no offset, and the sub-tree
            // leads to a single scan with no aggregation or join.
            if let Some(n) = count {
                if offset.is_none() && can_push_limit_into(&inner) {
                    return inject_limit(inner, n);
                }
            }
            LogicalPlan::Limit {
                input: Box::new(inner),
                count,
                offset,
            }
        }
        LogicalPlan::Project { input, columns } => LogicalPlan::Project {
            input: Box::new(push_limit(*input)),
            columns,
        },
        LogicalPlan::Filter { input, predicate } => LogicalPlan::Filter {
            input: Box::new(push_limit(*input)),
            predicate,
        },
        LogicalPlan::Sort { input, order_by } => LogicalPlan::Sort {
            input: Box::new(push_limit(*input)),
            order_by,
        },
        // Do not push through aggregations or joins — semantics differ.
        other => other,
    }
}

/// Returns `true` when the plan is a simple path to a single scan with no
/// aggregations or joins.
fn can_push_limit_into(plan: &LogicalPlan) -> bool {
    match plan {
        LogicalPlan::Scan { .. } => true,
        LogicalPlan::Filter { input, .. }
        | LogicalPlan::Project { input, .. }
        | LogicalPlan::Sort { input, .. }
        | LogicalPlan::Limit { input, .. } => can_push_limit_into(input),
        LogicalPlan::Aggregate { .. }
        | LogicalPlan::Join { .. }
        | LogicalPlan::Values { .. }
        | LogicalPlan::Empty
        | LogicalPlan::SetOp { .. }
        | LogicalPlan::Cte { .. }
        | LogicalPlan::CteRef { .. }
        | LogicalPlan::Window { .. }
        | LogicalPlan::Subquery { .. }
        | LogicalPlan::Exists { .. }
        | LogicalPlan::InSubquery { .. }
        | LogicalPlan::Compute { .. } => false,
    }
}

/// Inject `limit` into the deepest `Scan` reachable through filter/project/sort
/// nodes.
fn inject_limit(plan: LogicalPlan, limit: u64) -> LogicalPlan {
    match plan {
        LogicalPlan::Scan {
            table,
            alias,
            limit: existing,
        } => {
            // Take the more restrictive limit.
            let new_limit = match existing {
                Some(e) => Some(e.min(limit as usize)),
                None => Some(limit as usize),
            };
            LogicalPlan::Scan {
                table,
                alias,
                limit: new_limit,
            }
        }
        LogicalPlan::Filter { input, predicate } => LogicalPlan::Filter {
            input: Box::new(inject_limit(*input, limit)),
            predicate,
        },
        LogicalPlan::Project { input, columns } => LogicalPlan::Project {
            input: Box::new(inject_limit(*input, limit)),
            columns,
        },
        LogicalPlan::Sort { input, order_by } => LogicalPlan::Sort {
            input: Box::new(inject_limit(*input, limit)),
            order_by,
        },
        other => other,
    }
}
