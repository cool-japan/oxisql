//! Pass 5: Join algorithm selection.
//!
//! Annotates each [`LogicalPlan::Join`] node with a recommended physical
//! algorithm hint:
//!
//! - [`JoinAlgoHint::HashJoin`]  — for equi-joins (predicate contains `=`
//!   without `<` or `>`).
//! - [`JoinAlgoHint::MergeJoin`] — for equi-joins where *both* sides are
//!   already [`LogicalPlan::Sort`] nodes on the same key as the join column.
//! - [`JoinAlgoHint::NestedLoop`] — fallback for cross-joins or non-equi
//!   predicates.
//!
//! The hint is stored in the `algo_hint` field of each `Join` variant and can
//! be consumed by downstream physical planning stages.

use crate::{JoinType, LogicalPlan};

use super::OptPass;

// ── Types ─────────────────────────────────────────────────────────────────────

/// Physical join algorithm hint assigned by [`JoinAlgorithmPass`].
///
/// The hint is stored in [`LogicalPlan::Join::algo_hint`] after the optimizer
/// pass runs.  Downstream physical planners can inspect it to select the best
/// join implementation.
#[derive(Debug, Clone, PartialEq)]
pub enum JoinAlgoHint {
    /// Hash-join: build a hash table on one side and probe with the other.
    /// Suitable for equi-joins.
    HashJoin,
    /// Merge-join: requires both sides already sorted on the join key.
    MergeJoin,
    /// Nested-loop join: the fallback for cross-joins and non-equi predicates.
    NestedLoop,
}

// ── Public struct ────────────────────────────────────────────────────────────

/// Optimizer pass that annotates each `Join` node with an algorithm hint.
pub struct JoinAlgorithmPass;

impl OptPass for JoinAlgorithmPass {
    fn name(&self) -> &'static str {
        "join_algorithm"
    }

    fn apply(&self, plan: LogicalPlan) -> LogicalPlan {
        annotate_join(plan)
    }
}

// ── Implementation ────────────────────────────────────────────────────────────

/// Recursively walk `plan`; for each `Join` node set its `algo_hint`.
fn annotate_join(plan: LogicalPlan) -> LogicalPlan {
    match plan {
        LogicalPlan::Join {
            left,
            right,
            on,
            join_type,
            algo_hint: _,
        } => {
            // Recurse into children first.
            let left = Box::new(annotate_join(*left));
            let right = Box::new(annotate_join(*right));

            let hint = choose_hint(&on, &join_type, &left, &right);

            LogicalPlan::Join {
                left,
                right,
                on,
                join_type,
                algo_hint: Some(hint),
            }
        }

        // Recurse into all other plan variants that have children.
        LogicalPlan::Filter { input, predicate } => LogicalPlan::Filter {
            input: Box::new(annotate_join(*input)),
            predicate,
        },
        LogicalPlan::Project { input, columns } => LogicalPlan::Project {
            input: Box::new(annotate_join(*input)),
            columns,
        },
        LogicalPlan::Aggregate {
            input,
            group_by,
            aggregates,
        } => LogicalPlan::Aggregate {
            input: Box::new(annotate_join(*input)),
            group_by,
            aggregates,
        },
        LogicalPlan::Sort { input, order_by } => LogicalPlan::Sort {
            input: Box::new(annotate_join(*input)),
            order_by,
        },
        LogicalPlan::Limit {
            input,
            count,
            offset,
        } => LogicalPlan::Limit {
            input: Box::new(annotate_join(*input)),
            count,
            offset,
        },
        LogicalPlan::Window { input, functions } => LogicalPlan::Window {
            input: Box::new(annotate_join(*input)),
            functions,
        },
        LogicalPlan::Cte {
            name,
            query,
            recursive,
        } => LogicalPlan::Cte {
            name,
            query: Box::new(annotate_join(*query)),
            recursive,
        },
        LogicalPlan::SetOp {
            op,
            all,
            left,
            right,
        } => LogicalPlan::SetOp {
            op,
            all,
            left: Box::new(annotate_join(*left)),
            right: Box::new(annotate_join(*right)),
        },
        LogicalPlan::Subquery { query, alias } => LogicalPlan::Subquery {
            query: Box::new(annotate_join(*query)),
            alias,
        },
        LogicalPlan::Exists { subquery, negated } => LogicalPlan::Exists {
            subquery: Box::new(annotate_join(*subquery)),
            negated,
        },
        LogicalPlan::InSubquery {
            expr,
            subquery,
            negated,
        } => LogicalPlan::InSubquery {
            expr,
            subquery: Box::new(annotate_join(*subquery)),
            negated,
        },
        LogicalPlan::Compute { input, bindings } => LogicalPlan::Compute {
            input: Box::new(annotate_join(*input)),
            bindings,
        },
        // Leaf nodes — nothing to recurse into.
        leaf => leaf,
    }
}

/// Determine the best algorithm hint for a join given its ON predicate and
/// the physical shape of each input.
fn choose_hint(
    on: &str,
    join_type: &JoinType,
    left: &LogicalPlan,
    right: &LogicalPlan,
) -> JoinAlgoHint {
    // Cross joins have no predicate → always nested-loop.
    if matches!(join_type, JoinType::Cross) || on.is_empty() {
        return JoinAlgoHint::NestedLoop;
    }

    // Equi-join heuristic: contains '=' but no inequality operators.
    let is_equi = on.contains('=') && !on.contains('<') && !on.contains('>');

    if !is_equi {
        return JoinAlgoHint::NestedLoop;
    }

    // Try to promote to MergeJoin when both sides are already Sort nodes on
    // a column that appears in the join predicate.
    if sides_sorted_on_join_key(on, left) && sides_sorted_on_join_key(on, right) {
        return JoinAlgoHint::MergeJoin;
    }

    JoinAlgoHint::HashJoin
}

/// Returns `true` when `plan` is a `Sort` whose first key column appears in
/// the join predicate string — a simple heuristic that the data is sorted on
/// the join key.
fn sides_sorted_on_join_key(on: &str, plan: &LogicalPlan) -> bool {
    if let LogicalPlan::Sort { order_by, .. } = plan {
        if let Some(first) = order_by.first() {
            // Extract the bare column name (strip table-qualifier if present).
            let col = first.column.rsplit('.').next().unwrap_or(&first.column);
            return on.contains(col);
        }
    }
    false
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{parse_one, plan_statement, JoinType, LogicalPlan, Optimizer};

    /// Build a simple equi-join plan directly (bypasses the SQL parser so the
    /// test is deterministic about which plan tree is built).
    fn equi_join_plan() -> LogicalPlan {
        LogicalPlan::Join {
            left: Box::new(LogicalPlan::Scan {
                table: "a".to_string(),
                alias: None,
                limit: None,
            }),
            right: Box::new(LogicalPlan::Scan {
                table: "b".to_string(),
                alias: None,
                limit: None,
            }),
            on: "a.id = b.id".to_string(),
            join_type: JoinType::Inner,
            algo_hint: None,
        }
    }

    /// Build a cross-join plan (no ON predicate).
    fn cross_join_plan() -> LogicalPlan {
        LogicalPlan::Join {
            left: Box::new(LogicalPlan::Scan {
                table: "a".to_string(),
                alias: None,
                limit: None,
            }),
            right: Box::new(LogicalPlan::Scan {
                table: "b".to_string(),
                alias: None,
                limit: None,
            }),
            on: String::new(),
            join_type: JoinType::Cross,
            algo_hint: None,
        }
    }

    /// An equi-join plan produced from SQL gets `HashJoin` hint.
    #[test]
    fn test_equi_join_gets_hash_hint() {
        let plan = equi_join_plan();
        let result = JoinAlgorithmPass.apply(plan);
        match result {
            LogicalPlan::Join { algo_hint, .. } => {
                assert_eq!(algo_hint, Some(JoinAlgoHint::HashJoin));
            }
            other => panic!("expected Join, got {other:?}"),
        }
    }

    /// A cross join (no ON clause) gets `NestedLoop` hint.
    #[test]
    fn test_cross_join_gets_nested_loop() {
        let plan = cross_join_plan();
        let result = JoinAlgorithmPass.apply(plan);
        match result {
            LogicalPlan::Join { algo_hint, .. } => {
                assert_eq!(algo_hint, Some(JoinAlgoHint::NestedLoop));
            }
            other => panic!("expected Join, got {other:?}"),
        }
    }

    /// The pass name is exactly "join_algorithm".
    #[test]
    fn test_join_algorithm_pass_name() {
        assert_eq!(JoinAlgorithmPass.name(), "join_algorithm");
    }

    /// The full optimizer pipeline (including JoinAlgorithmPass) sets a hint
    /// on a join produced by `plan_statement`.
    #[test]
    fn test_optimizer_pipeline_annotates_join() {
        let stmt = parse_one("SELECT * FROM a JOIN b ON a.id = b.id").expect("parse");
        let plan = plan_statement(&stmt).expect("plan");
        let result = Optimizer::new().optimize(plan);

        fn find_join_hint(p: &LogicalPlan) -> Option<Option<JoinAlgoHint>> {
            match p {
                LogicalPlan::Join {
                    algo_hint,
                    left,
                    right,
                    ..
                } => {
                    // Return as soon as we find a Join.
                    let _ = (left, right); // used only for structure check
                    Some(algo_hint.clone())
                }
                LogicalPlan::Filter { input, .. }
                | LogicalPlan::Project { input, .. }
                | LogicalPlan::Sort { input, .. }
                | LogicalPlan::Limit { input, .. }
                | LogicalPlan::Aggregate { input, .. } => find_join_hint(input),
                _ => None,
            }
        }

        let hint = find_join_hint(&result);
        assert!(
            matches!(hint, Some(Some(JoinAlgoHint::HashJoin))),
            "expected HashJoin hint in optimized plan, got {hint:?}"
        );
    }
}
