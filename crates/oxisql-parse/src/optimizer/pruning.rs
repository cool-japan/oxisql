//! Pass 2: Projection pruning.
//!
//! Given a required column set propagated downward from the top of the plan,
//! trim each `Project` node's column list to only the columns in the required
//! set.  When `required` is empty, this pass is a no-op.

use crate::LogicalPlan;

use super::OptPass;

// ── Public struct ─────────────────────────────────────────────────────────────

/// Prune `Project` column lists to only those columns required by the parent.
///
/// When `required` is empty, this pass is a no-op (all columns are kept).
pub struct ProjectionPruning {
    /// Column names (bare names, not qualified) needed by the consumer of this
    /// plan.  An empty list means "all columns required" (no pruning).
    pub required: Vec<String>,
}

impl OptPass for ProjectionPruning {
    fn name(&self) -> &'static str {
        "ProjectionPruning"
    }

    fn apply(&self, plan: LogicalPlan) -> LogicalPlan {
        if self.required.is_empty() {
            return plan;
        }
        prune_projections(plan, &self.required)
    }
}

// ── Implementation ────────────────────────────────────────────────────────────

pub(super) fn prune_projections(plan: LogicalPlan, required: &[String]) -> LogicalPlan {
    match plan {
        LogicalPlan::Project { input, columns } => {
            // Keep only columns that appear in the required set.
            let pruned: Vec<String> = columns
                .into_iter()
                .filter(|col| {
                    required
                        .iter()
                        .any(|r| col == r || col.contains(r.as_str()))
                })
                .collect();

            // Propagate the (possibly pruned) requirement downward.
            let new_required = if pruned.is_empty() {
                required.to_vec()
            } else {
                pruned.clone()
            };
            let final_cols = if pruned.is_empty() {
                required.to_vec()
            } else {
                pruned
            };

            LogicalPlan::Project {
                input: Box::new(prune_projections(*input, &new_required)),
                columns: final_cols,
            }
        }
        LogicalPlan::Filter { input, predicate } => LogicalPlan::Filter {
            input: Box::new(prune_projections(*input, required)),
            predicate,
        },
        LogicalPlan::Aggregate {
            input,
            group_by,
            aggregates,
        } => LogicalPlan::Aggregate {
            input: Box::new(prune_projections(*input, required)),
            group_by,
            aggregates,
        },
        LogicalPlan::Sort { input, order_by } => LogicalPlan::Sort {
            input: Box::new(prune_projections(*input, required)),
            order_by,
        },
        LogicalPlan::Limit {
            input,
            count,
            offset,
        } => LogicalPlan::Limit {
            input: Box::new(prune_projections(*input, required)),
            count,
            offset,
        },
        LogicalPlan::Join {
            left,
            right,
            on,
            join_type,
            algo_hint,
        } => LogicalPlan::Join {
            left: Box::new(prune_projections(*left, required)),
            right: Box::new(prune_projections(*right, required)),
            on,
            join_type,
            algo_hint,
        },
        other => other,
    }
}
