//! Rule-based SQL query optimizer for [`LogicalPlan`].
//!
//! The optimizer applies a configurable sequence of rewrite passes to a
//! [`LogicalPlan`] tree.  Each pass implements [`OptPass`] and is applied in
//! order.  The default pipeline (built by [`Optimizer::new`]) includes:
//!
//! 1. [`PredicatePushdown`]  — push filters toward leaf scans
//! 2. [`ProjectionPruning`]  — trim projections to the required column set
//! 3. [`ConstantFolding`]    — evaluate literal expressions at compile time
//! 4. [`LimitPushThrough`]   — push `LIMIT N` into single-table scans
//! 5. [`JoinAlgorithmPass`]  — annotate `Join` nodes with an algorithm hint

mod folding;
pub mod join_algo;
mod limit;
mod pruning;
mod pushdown;

pub use folding::ConstantFolding;
pub use join_algo::{JoinAlgoHint, JoinAlgorithmPass};
pub use limit::LimitPushThrough;
pub use pruning::ProjectionPruning;
pub use pushdown::PredicatePushdown;

use crate::LogicalPlan;

// ── Trait ────────────────────────────────────────────────────────────────────

/// A single optimizer rewrite rule.
pub trait OptPass: Send + Sync {
    /// Short name identifying this pass (for tracing / debugging).
    fn name(&self) -> &'static str;

    /// Apply the rewrite to `plan`, returning the (possibly modified) plan.
    fn apply(&self, plan: LogicalPlan) -> LogicalPlan;
}

// ── Optimizer ────────────────────────────────────────────────────────────────

/// Multi-pass rule-based optimizer.
///
/// Passes are applied left-to-right in the order they were registered.  The
/// default pipeline is constructed by [`Optimizer::new`].
pub struct Optimizer {
    passes: Vec<Box<dyn OptPass>>,
}

impl Optimizer {
    /// Build the default five-pass optimizer pipeline.
    ///
    /// The `ProjectionPruning` pass is created with an empty `required` set,
    /// which means it operates as a no-op unless a caller explicitly builds an
    /// optimizer with a non-empty requirement list via [`Optimizer::with_passes`].
    pub fn new() -> Self {
        let passes: Vec<Box<dyn OptPass>> = vec![
            Box::new(PredicatePushdown),
            Box::new(ProjectionPruning { required: vec![] }),
            Box::new(ConstantFolding),
            Box::new(LimitPushThrough),
            Box::new(JoinAlgorithmPass),
        ];
        Self { passes }
    }

    /// Build an optimizer with a custom ordered list of passes.
    pub fn with_passes(passes: Vec<Box<dyn OptPass>>) -> Self {
        Self { passes }
    }

    /// Run all registered passes over `plan` in registration order.
    pub fn optimize(&self, plan: LogicalPlan) -> LogicalPlan {
        self.passes.iter().fold(plan, |p, pass| pass.apply(p))
    }
}

impl Default for Optimizer {
    fn default() -> Self {
        Self::new()
    }
}

// ── Public convenience wrapper ────────────────────────────────────────────────

/// Run all five default optimizer passes over `plan`.
///
/// This is the top-level convenience wrapper that creates a default
/// [`Optimizer`] and applies it to the given plan.
pub fn optimize(plan: LogicalPlan) -> LogicalPlan {
    Optimizer::new().optimize(plan)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{JoinType, LogicalPlan};

    // Helper: build an unbounded Scan node.
    fn scan(table: &str) -> LogicalPlan {
        LogicalPlan::Scan {
            table: table.to_string(),
            alias: None,
            limit: None,
        }
    }

    // Helper: build a named-alias Scan.
    fn scan_alias(table: &str, alias: &str) -> LogicalPlan {
        LogicalPlan::Scan {
            table: table.to_string(),
            alias: Some(alias.to_string()),
            limit: None,
        }
    }

    // Helper: build a Filter.
    fn filter(input: LogicalPlan, pred: &str) -> LogicalPlan {
        LogicalPlan::Filter {
            input: Box::new(input),
            predicate: pred.to_string(),
        }
    }

    // Helper: build an inner Join.
    fn join(left: LogicalPlan, right: LogicalPlan, on: &str) -> LogicalPlan {
        LogicalPlan::Join {
            left: Box::new(left),
            right: Box::new(right),
            on: on.to_string(),
            join_type: JoinType::Inner,
            algo_hint: None,
        }
    }

    /// 1. A predicate referencing only the left side of an INNER join is pushed
    ///    below the join onto the left scan.
    #[test]
    fn test_predicate_pushdown_moves_filter_below_join() {
        // Filter(Join(Scan(a AS a), Scan(b AS b)), "a.x > 1")
        let plan = filter(
            join(scan_alias("a", "a"), scan_alias("b", "b"), "a.id = b.id"),
            "a.x > 1",
        );
        let result = PredicatePushdown.apply(plan);

        match result {
            LogicalPlan::Join { left, .. } => match *left {
                LogicalPlan::Filter { predicate, .. } => {
                    assert_eq!(predicate, "a.x > 1");
                }
                other => panic!("expected Filter on left side, got {other:?}"),
            },
            other => panic!("expected Join at root after pushdown, got {other:?}"),
        }
    }

    /// 2. Projection pruning reduces the column list to only required columns.
    #[test]
    fn test_projection_pruning_reduces_columns() {
        let plan = LogicalPlan::Project {
            input: Box::new(scan("t")),
            columns: vec!["col1".to_string(), "col2".to_string(), "col3".to_string()],
        };
        let pass = ProjectionPruning {
            required: vec!["col1".to_string()],
        };
        let result = pass.apply(plan);
        match result {
            LogicalPlan::Project { columns, .. } => {
                assert_eq!(columns, vec!["col1".to_string()]);
            }
            other => panic!("expected Project, got {other:?}"),
        }
    }

    /// 3. Constant-folding a `1 = 1` predicate removes the Filter entirely.
    #[test]
    fn test_constant_folding_true_removes_filter() {
        let plan = filter(scan("t"), "1 = 1");
        let result = ConstantFolding.apply(plan);
        match result {
            LogicalPlan::Scan { table, .. } => assert_eq!(table, "t"),
            other => panic!("expected Scan after folding 1=1, got {other:?}"),
        }
    }

    /// 4. Constant-folding a `1 = 2` predicate replaces the subtree with Empty.
    #[test]
    fn test_constant_folding_false_yields_empty() {
        let plan = filter(scan("t"), "1 = 2");
        let result = ConstantFolding.apply(plan);
        assert!(
            matches!(result, LogicalPlan::Empty),
            "expected Empty after folding 1=2, got {result:?}"
        );
    }

    /// 5. LimitPushThrough pushes `Limit(10)` into the Scan's `limit` field,
    ///    removing the Limit wrapper node.
    #[test]
    fn test_limit_push_through_scan() {
        let plan = LogicalPlan::Limit {
            input: Box::new(scan("t")),
            count: Some(10),
            offset: None,
        };
        let result = LimitPushThrough.apply(plan);
        match result {
            LogicalPlan::Scan { limit, .. } => assert_eq!(limit, Some(10)),
            other => panic!("expected Scan with limit after push-through, got {other:?}"),
        }
    }

    /// 6. The full four-pass pipeline composes without errors.
    #[test]
    fn test_optimizer_pipeline_composes() {
        // Build: Limit(Filter(Join(Scan(a AS a), Scan(b AS b)), "a.id > 0"), 5)
        let base = filter(
            join(scan_alias("a", "a"), scan_alias("b", "b"), "a.id = b.id"),
            "a.id > 0",
        );
        let limited = LogicalPlan::Limit {
            input: Box::new(base),
            count: Some(5),
            offset: None,
        };
        let result = Optimizer::new().optimize(limited);
        let text = crate::explain(&result);
        assert!(!text.is_empty(), "explain should produce non-empty output");
        assert!(
            text.contains("Join") || text.contains("Scan"),
            "expected join or scan in plan: {text}"
        );
    }
}
