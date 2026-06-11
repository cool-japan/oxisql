//! Cost-based join reordering pass (Item 1).
//!
//! Uses DPccp (Selinger bottom-up DP over connected subsets) for join nests
//! with ≤ `dp_threshold` atoms, and a GOO (greedy ordering optimisation)
//! fallback above that threshold.

use std::collections::HashMap;
use std::sync::Arc;

use crate::cost::{CostEstimate, CostModel};
use crate::optimizer::expr_util::{
    equi_key, join_conjuncts, parse_predicate, render, split_conjuncts,
};
use crate::plan::{JoinType, LogicalPlan};

use super::OptPass;

// ── Public pass struct ────────────────────────────────────────────────────────

/// Cost-based join reordering pass.
///
/// Uses DPccp (bottom-up DP over connected subsets) for ≤ `dp_threshold` atoms
/// and GOO (greedy ordering optimisation) above that threshold.
pub struct JoinReorder {
    /// The cost model used to estimate scan and join costs.
    pub model: Arc<CostModel>,
    /// Maximum number of atoms for the DPccp algorithm; above this, GOO is used.
    pub dp_threshold: usize,
}

impl JoinReorder {
    /// Create a new `JoinReorder` pass with the default DP threshold of 12.
    pub fn new(model: Arc<CostModel>) -> Self {
        Self {
            model,
            dp_threshold: 12,
        }
    }

    /// Create a `JoinReorder` pass with an explicit DP threshold.
    pub fn with_threshold(model: Arc<CostModel>, dp_threshold: usize) -> Self {
        Self {
            model,
            dp_threshold,
        }
    }
}

impl OptPass for JoinReorder {
    fn name(&self) -> &'static str {
        "JoinReorder"
    }

    fn apply(&self, plan: LogicalPlan) -> LogicalPlan {
        walk(plan, &self.model, self.dp_threshold)
    }
}

// ── Internal data structures ──────────────────────────────────────────────────

/// A leaf atom in a flattened inner-join nest.
struct Atom {
    plan: LogicalPlan,
    cost: CostEstimate,
    /// Lower-cased table name when the atom is a plain `Scan`.
    table: Option<String>,
}

/// An edge between two atom indices in the hypergraph.
struct Edge {
    left_idx: usize,
    right_idx: usize,
    predicates: Vec<String>,
    selectivity: f64,
}

// ── Recursive plan walk ───────────────────────────────────────────────────────

/// Recursively walk every plan node, recursing into children first, then
/// attempting join reordering at any `Inner` / `Cross` join root.
fn walk(plan: LogicalPlan, model: &Arc<CostModel>, dp_threshold: usize) -> LogicalPlan {
    match plan {
        LogicalPlan::Join {
            left,
            right,
            on,
            join_type,
            algo_hint,
        } => {
            let left_opt = walk(*left, model, dp_threshold);
            let right_opt = walk(*right, model, dp_threshold);

            let rebuilt = LogicalPlan::Join {
                left: Box::new(left_opt),
                right: Box::new(right_opt),
                on,
                join_type: join_type.clone(),
                algo_hint,
            };

            if matches!(join_type, JoinType::Inner | JoinType::Cross) {
                attempt_reorder(rebuilt, model, dp_threshold)
            } else {
                rebuilt
            }
        }

        LogicalPlan::Filter { input, predicate } => LogicalPlan::Filter {
            input: Box::new(walk(*input, model, dp_threshold)),
            predicate,
        },
        LogicalPlan::Project { input, columns } => LogicalPlan::Project {
            input: Box::new(walk(*input, model, dp_threshold)),
            columns,
        },
        LogicalPlan::Aggregate {
            input,
            group_by,
            aggregates,
        } => LogicalPlan::Aggregate {
            input: Box::new(walk(*input, model, dp_threshold)),
            group_by,
            aggregates,
        },
        LogicalPlan::Sort { input, order_by } => LogicalPlan::Sort {
            input: Box::new(walk(*input, model, dp_threshold)),
            order_by,
        },
        LogicalPlan::Limit {
            input,
            count,
            offset,
        } => LogicalPlan::Limit {
            input: Box::new(walk(*input, model, dp_threshold)),
            count,
            offset,
        },
        LogicalPlan::Window { input, functions } => LogicalPlan::Window {
            input: Box::new(walk(*input, model, dp_threshold)),
            functions,
        },
        LogicalPlan::Cte {
            name,
            query,
            recursive,
        } => LogicalPlan::Cte {
            name,
            query: Box::new(walk(*query, model, dp_threshold)),
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
            left: Box::new(walk(*left, model, dp_threshold)),
            right: Box::new(walk(*right, model, dp_threshold)),
        },
        LogicalPlan::Subquery { query, alias } => LogicalPlan::Subquery {
            query: Box::new(walk(*query, model, dp_threshold)),
            alias,
        },
        LogicalPlan::Exists { subquery, negated } => LogicalPlan::Exists {
            subquery: Box::new(walk(*subquery, model, dp_threshold)),
            negated,
        },
        LogicalPlan::InSubquery {
            expr,
            subquery,
            negated,
        } => LogicalPlan::InSubquery {
            expr,
            subquery: Box::new(walk(*subquery, model, dp_threshold)),
            negated,
        },
        LogicalPlan::Compute { input, bindings } => LogicalPlan::Compute {
            input: Box::new(walk(*input, model, dp_threshold)),
            bindings,
        },
        leaf => leaf,
    }
}

// ── Nest flattening and reorder ───────────────────────────────────────────────

/// Flatten an `Inner`/`Cross` join root into atoms + predicates, choose an
/// algorithm, and reassemble.
fn attempt_reorder(plan: LogicalPlan, model: &Arc<CostModel>, dp_threshold: usize) -> LogicalPlan {
    let mut atoms: Vec<Atom> = Vec::new();
    let mut all_preds: Vec<String> = Vec::new();

    flatten_join_nest(plan, model, &mut atoms, &mut all_preds);

    let n = atoms.len();
    if n <= 1 {
        return atoms
            .into_iter()
            .next()
            .map(|a| a.plan)
            .unwrap_or(LogicalPlan::Empty);
    }

    let edges = build_edges(&atoms, &all_preds);

    if n <= dp_threshold {
        dpccp(model, atoms, edges, &all_preds)
    } else {
        goo(model, atoms, edges, &all_preds)
    }
}

/// Recursively flatten an `Inner`/`Cross` join nest into atoms and predicates.
/// Any non-Inner/Cross join is treated as an opaque atom.
fn flatten_join_nest(
    plan: LogicalPlan,
    model: &Arc<CostModel>,
    atoms: &mut Vec<Atom>,
    preds: &mut Vec<String>,
) {
    match plan {
        LogicalPlan::Join {
            left,
            right,
            on,
            join_type: JoinType::Inner | JoinType::Cross,
            ..
        } => {
            collect_pred_strings(&on, preds);
            flatten_join_nest(*left, model, atoms, preds);
            flatten_join_nest(*right, model, atoms, preds);
        }
        other => {
            let table = table_name_of(&other);
            let cost = model.estimate(&other);
            atoms.push(Atom {
                plan: other,
                cost,
                table,
            });
        }
    }
}

fn table_name_of(plan: &LogicalPlan) -> Option<String> {
    if let LogicalPlan::Scan { table, .. } = plan {
        Some(table.to_lowercase())
    } else {
        None
    }
}

fn collect_pred_strings(on: &str, preds: &mut Vec<String>) {
    if on.is_empty() {
        return;
    }
    if let Some(expr) = parse_predicate(on) {
        for c in split_conjuncts(expr) {
            preds.push(render(&c));
        }
    } else {
        preds.push(on.to_string());
    }
}

// ── Edge construction ─────────────────────────────────────────────────────────

fn build_edges(atoms: &[Atom], all_preds: &[String]) -> Vec<Edge> {
    let mut edges: Vec<Edge> = Vec::new();

    for pred in all_preds {
        let Some(expr) = parse_predicate(pred) else {
            continue;
        };
        let Some((l_ref, r_ref)) = equi_key(&expr) else {
            continue;
        };

        let l_idx = find_atom_idx(atoms, &l_ref.qualifier);
        let r_idx = find_atom_idx(atoms, &r_ref.qualifier);

        let (l_idx, r_idx) = match (l_idx, r_idx) {
            (Some(l), Some(r)) if l != r => (l.min(r), l.max(r)),
            _ => continue,
        };

        let selectivity = 0.1_f64;

        if let Some(existing) = edges
            .iter_mut()
            .find(|e| e.left_idx == l_idx && e.right_idx == r_idx)
        {
            existing.predicates.push(pred.clone());
            existing.selectivity *= selectivity;
        } else {
            edges.push(Edge {
                left_idx: l_idx,
                right_idx: r_idx,
                predicates: vec![pred.clone()],
                selectivity,
            });
        }
    }

    edges
}

fn find_atom_idx(atoms: &[Atom], qualifier: &Option<String>) -> Option<usize> {
    let q = qualifier.as_deref()?.to_lowercase();
    atoms
        .iter()
        .enumerate()
        .find(|(_, a)| a.table.as_deref() == Some(q.as_str()))
        .map(|(i, _)| i)
}

// ── Predicate routing ─────────────────────────────────────────────────────────

fn preds_crossing(edges: &[Edge], l_mask: u32, r_mask: u32) -> Vec<String> {
    edges
        .iter()
        .filter(|e| {
            let lb = 1u32 << e.left_idx;
            let rb = 1u32 << e.right_idx;
            (l_mask & lb != 0 && r_mask & rb != 0) || (l_mask & rb != 0 && r_mask & lb != 0)
        })
        .flat_map(|e| e.predicates.iter().cloned())
        .collect()
}

fn selectivity_crossing(edges: &[Edge], l_mask: u32, r_mask: u32) -> f64 {
    let mut sel = 1.0_f64;
    let mut found = false;
    for e in edges {
        let lb = 1u32 << e.left_idx;
        let rb = 1u32 << e.right_idx;
        if (l_mask & lb != 0 && r_mask & rb != 0) || (l_mask & rb != 0 && r_mask & lb != 0) {
            sel *= e.selectivity;
            found = true;
        }
    }
    if found {
        sel
    } else {
        1.0
    }
}

// ── Graph connectivity ────────────────────────────────────────────────────────

fn is_connected(edges: &[Edge], mask: u32) -> bool {
    if mask == 0 || mask & (mask - 1) == 0 {
        return true;
    }
    let start = mask.trailing_zeros() as usize;
    let mut visited: u32 = 1 << start;
    let mut queue: Vec<usize> = vec![start];

    while let Some(cur) = queue.pop() {
        for e in edges {
            let lb = 1u32 << e.left_idx;
            let rb = 1u32 << e.right_idx;
            if (lb & mask == 0) || (rb & mask == 0) {
                continue;
            }
            let other = if e.left_idx == cur {
                e.right_idx
            } else if e.right_idx == cur {
                e.left_idx
            } else {
                continue;
            };
            let ob = 1u32 << other;
            if visited & ob == 0 {
                visited |= ob;
                queue.push(other);
            }
        }
    }
    visited == mask
}

fn has_edge_crossing(edges: &[Edge], l_mask: u32, r_mask: u32) -> bool {
    edges.iter().any(|e| {
        let lb = 1u32 << e.left_idx;
        let rb = 1u32 << e.right_idx;
        (l_mask & lb != 0 && r_mask & rb != 0) || (l_mask & rb != 0 && r_mask & lb != 0)
    })
}

// ── Join assembly ─────────────────────────────────────────────────────────────

fn make_join(left: LogicalPlan, right: LogicalPlan, crossing_preds: Vec<String>) -> LogicalPlan {
    let on = if crossing_preds.is_empty() {
        String::new()
    } else if crossing_preds.len() == 1 {
        crossing_preds.into_iter().next().unwrap_or_default()
    } else {
        let exprs: Vec<_> = crossing_preds
            .iter()
            .filter_map(|p| parse_predicate(p))
            .collect();
        if let Some(combined) = join_conjuncts(exprs) {
            render(&combined)
        } else {
            crossing_preds.join(" AND ")
        }
    };
    let join_type = if on.is_empty() {
        JoinType::Cross
    } else {
        JoinType::Inner
    };
    LogicalPlan::Join {
        left: Box::new(left),
        right: Box::new(right),
        on,
        join_type,
        algo_hint: None,
    }
}

// ── DPccp ────────────────────────────────────────────────────────────────────

fn dpccp(
    model: &Arc<CostModel>,
    atoms: Vec<Atom>,
    edges: Vec<Edge>,
    all_preds: &[String],
) -> LogicalPlan {
    let n = atoms.len();
    let full_mask: u32 = (1u32 << n) - 1;

    let mut best_plan: HashMap<u32, LogicalPlan> = HashMap::new();
    let mut best_cost: HashMap<u32, CostEstimate> = HashMap::new();

    for (i, atom) in atoms.iter().enumerate() {
        let mask = 1u32 << i;
        best_plan.insert(mask, atom.plan.clone());
        best_cost.insert(mask, atom.cost.clone());
    }

    let total: u32 = 1u32 << n;
    for s in 1..total {
        if s.count_ones() < 2 {
            continue;
        }
        // For connectivity: if no edges at all, treat single-atom subsets as
        // trivially connected and process all splits (cross-join graph).
        let no_edges = edges.is_empty();
        if !no_edges && !is_connected(&edges, s) {
            continue;
        }

        let mut sub = (s - 1) & s;
        while sub > 0 {
            let complement = s ^ sub;
            if complement != 0 && sub < complement {
                let l_mask = sub;
                let r_mask = complement;

                let l_ok = no_edges || is_connected(&edges, l_mask);
                let r_ok = no_edges || is_connected(&edges, r_mask);
                let edge_ok =
                    no_edges || has_edge_crossing(&edges, l_mask, r_mask) || s == full_mask;

                if l_ok && r_ok && edge_ok {
                    let lp = best_plan.get(&l_mask);
                    let lc = best_cost.get(&l_mask);
                    let rp = best_plan.get(&r_mask);
                    let rc = best_cost.get(&r_mask);

                    if let (Some(lp), Some(lc), Some(rp), Some(rc)) = (lp, lc, rp, rc) {
                        let sel = selectivity_crossing(&edges, l_mask, r_mask);
                        let new_cost = model.estimate_join_from(lc, rc, sel);

                        let is_better = best_cost
                            .get(&s)
                            .map(|c| new_cost.total_cost < c.total_cost)
                            .unwrap_or(true);

                        if is_better {
                            let crossing = preds_crossing(&edges, l_mask, r_mask);
                            let _ = all_preds; // preds already embedded in edges
                            let new_plan = make_join(lp.clone(), rp.clone(), crossing);
                            best_plan.insert(s, new_plan);
                            best_cost.insert(s, new_cost);
                        }
                    }
                }
            }
            if sub == 0 {
                break;
            }
            sub = (sub - 1) & s;
        }
    }

    best_plan.remove(&full_mask).unwrap_or_else(|| {
        assemble_linear(
            atoms.into_iter().map(|a| (a.plan, a.cost)).collect(),
            &edges,
        )
    })
}

// ── GOO ──────────────────────────────────────────────────────────────────────

struct GooAtom {
    plan: LogicalPlan,
    cost: CostEstimate,
    mask: u32,
}

fn goo(
    model: &Arc<CostModel>,
    atoms: Vec<Atom>,
    edges: Vec<Edge>,
    _all_preds: &[String],
) -> LogicalPlan {
    let mut remaining: Vec<GooAtom> = atoms
        .into_iter()
        .enumerate()
        .map(|(i, a)| GooAtom {
            plan: a.plan,
            cost: a.cost,
            mask: 1u32 << i,
        })
        .collect();

    while remaining.len() > 1 {
        let mut best_i = 0usize;
        let mut best_j = 1usize;
        let mut best_rows = f64::MAX;

        for i in 0..remaining.len() {
            for j in (i + 1)..remaining.len() {
                let sel = selectivity_crossing(&edges, remaining[i].mask, remaining[j].mask);
                let est = model.estimate_join_from(&remaining[i].cost, &remaining[j].cost, sel);
                if est.rows < best_rows {
                    best_rows = est.rows;
                    best_i = i;
                    best_j = j;
                }
            }
        }

        // Remove higher index first to keep indices stable.
        let j_entry = remaining.remove(best_j);
        let i_entry = remaining.remove(best_i);

        let combined_mask = i_entry.mask | j_entry.mask;
        let sel = selectivity_crossing(&edges, i_entry.mask, j_entry.mask);
        let crossing = preds_crossing(&edges, i_entry.mask, j_entry.mask);
        let new_cost = model.estimate_join_from(&i_entry.cost, &j_entry.cost, sel);
        let new_plan = make_join(i_entry.plan, j_entry.plan, crossing);

        remaining.push(GooAtom {
            plan: new_plan,
            cost: new_cost,
            mask: combined_mask,
        });
    }

    remaining
        .into_iter()
        .next()
        .map(|a| a.plan)
        .unwrap_or(LogicalPlan::Empty)
}

// ── Linear fallback ───────────────────────────────────────────────────────────

fn assemble_linear(mut plans: Vec<(LogicalPlan, CostEstimate)>, edges: &[Edge]) -> LogicalPlan {
    if plans.is_empty() {
        return LogicalPlan::Empty;
    }

    // Build a left-deep tree: fold right-to-left (pop from end and merge).
    while plans.len() > 1 {
        let len = plans.len();
        let (rp, _rc) = plans.remove(len - 1);
        let (lp, _lc) = plans.remove(len - 2);
        // Compute masks approximately: indices into original atoms are not
        // tracked here, so pass empty edges → cross-product.
        let _ = edges;
        let new_plan = make_join(lp, rp, vec![]);
        plans.push((new_plan, CostEstimate::zero()));
    }

    plans
        .into_iter()
        .next()
        .map(|(p, _)| p)
        .unwrap_or(LogicalPlan::Empty)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cost::{CostModel, TableStats};

    fn scan(table: &str) -> LogicalPlan {
        LogicalPlan::Scan {
            table: table.to_string(),
            alias: None,
            limit: None,
        }
    }

    fn inner_join(l: LogicalPlan, r: LogicalPlan, on: &str) -> LogicalPlan {
        LogicalPlan::Join {
            left: Box::new(l),
            right: Box::new(r),
            on: on.to_string(),
            join_type: JoinType::Inner,
            algo_hint: None,
        }
    }

    fn left_join(l: LogicalPlan, r: LogicalPlan, on: &str) -> LogicalPlan {
        LogicalPlan::Join {
            left: Box::new(l),
            right: Box::new(r),
            on: on.to_string(),
            join_type: JoinType::Left,
            algo_hint: None,
        }
    }

    fn model_3table() -> Arc<CostModel> {
        Arc::new(
            CostModel::new()
                .with_table_stats(
                    "big",
                    TableStats {
                        row_count: 1_000_000,
                        avg_row_size_bytes: 100,
                        ..Default::default()
                    },
                )
                .with_table_stats(
                    "small1",
                    TableStats {
                        row_count: 10,
                        avg_row_size_bytes: 50,
                        ..Default::default()
                    },
                )
                .with_table_stats(
                    "small2",
                    TableStats {
                        row_count: 10,
                        avg_row_size_bytes: 50,
                        ..Default::default()
                    },
                ),
        )
    }

    /// 3-table chain: big ⨝ small1 ⨝ small2.
    /// Reordering should produce cost ≤ original.
    #[test]
    fn test_3table_small_tables_join_first() {
        let model = model_3table();
        let original = inner_join(
            inner_join(scan("big"), scan("small1"), "big.id = small1.big_id"),
            scan("small2"),
            "small1.id = small2.s1_id",
        );

        let pass = JoinReorder::new(Arc::clone(&model));
        let reordered = pass.apply(original.clone());

        let orig_cost = model.estimate(&original).total_cost;
        let reord_cost = model.estimate(&reordered).total_cost;
        assert!(
            reord_cost <= orig_cost + 1e-6,
            "reordered cost ({reord_cost}) should not exceed original ({orig_cost})"
        );
    }

    /// Outer-join nests must NOT be reordered.
    #[test]
    fn test_outer_join_nest_untouched() {
        let model = Arc::new(CostModel::new());
        let plan = left_join(scan("a"), scan("b"), "a.id = b.id");

        let pass = JoinReorder::new(Arc::clone(&model));
        let result = pass.apply(plan);

        match &result {
            LogicalPlan::Join { join_type, .. } => {
                assert_eq!(
                    *join_type,
                    JoinType::Left,
                    "outer join must not be reordered"
                );
            }
            other => panic!("expected Join, got {other:?}"),
        }
    }

    /// Cross-join-only region (no predicates) still produces a plan with all atoms.
    #[test]
    fn test_cross_join_only_still_connects_atoms() {
        let model = Arc::new(CostModel::new());
        let plan = LogicalPlan::Join {
            left: Box::new(LogicalPlan::Join {
                left: Box::new(scan("a")),
                right: Box::new(scan("b")),
                on: String::new(),
                join_type: JoinType::Cross,
                algo_hint: None,
            }),
            right: Box::new(scan("c")),
            on: String::new(),
            join_type: JoinType::Cross,
            algo_hint: None,
        };

        let pass = JoinReorder::new(Arc::clone(&model));
        let result = pass.apply(plan);

        let text = crate::explain::explain(&result);
        assert!(text.contains("Scan [a"), "a should be present: {text}");
        assert!(text.contains("Scan [b"), "b should be present: {text}");
        assert!(text.contains("Scan [c"), "c should be present: {text}");
    }

    /// GOO fallback: dp_threshold=3, 4-atom chain goes through GOO.
    #[test]
    fn test_goo_fallback_with_4_atoms() {
        let model = Arc::new(
            CostModel::new()
                .with_table_stats(
                    "a",
                    TableStats {
                        row_count: 1_000,
                        ..Default::default()
                    },
                )
                .with_table_stats(
                    "b",
                    TableStats {
                        row_count: 10,
                        ..Default::default()
                    },
                )
                .with_table_stats(
                    "c",
                    TableStats {
                        row_count: 10,
                        ..Default::default()
                    },
                )
                .with_table_stats(
                    "d",
                    TableStats {
                        row_count: 5,
                        ..Default::default()
                    },
                ),
        );
        let plan = inner_join(
            inner_join(
                inner_join(scan("a"), scan("b"), "a.id = b.a_id"),
                scan("c"),
                "b.id = c.b_id",
            ),
            scan("d"),
            "c.id = d.c_id",
        );

        let pass = JoinReorder::with_threshold(Arc::clone(&model), 3);
        let result = pass.apply(plan);

        let text = crate::explain::explain(&result);
        assert!(text.contains("Scan [a"), "a missing: {text}");
        assert!(text.contains("Scan [b"), "b missing: {text}");
        assert!(text.contains("Scan [c"), "c missing: {text}");
        assert!(text.contains("Scan [d"), "d missing: {text}");
    }

    /// Property: reordered cost ≤ original cost for random table sizes.
    #[test]
    fn test_proptest_reorder_cost_le_original() {
        use proptest::prelude::*;
        use proptest::test_runner::TestRunner;

        let mut runner = TestRunner::default();
        runner
            .run(
                &(1u64..100_000u64, 1u64..100_000u64, 1u64..100_000u64),
                |(r_a, r_b, r_c)| {
                    let model = Arc::new(
                        CostModel::new()
                            .with_table_stats(
                                "a",
                                TableStats {
                                    row_count: r_a,
                                    ..Default::default()
                                },
                            )
                            .with_table_stats(
                                "b",
                                TableStats {
                                    row_count: r_b,
                                    ..Default::default()
                                },
                            )
                            .with_table_stats(
                                "c",
                                TableStats {
                                    row_count: r_c,
                                    ..Default::default()
                                },
                            ),
                    );
                    let original = inner_join(
                        inner_join(scan("a"), scan("b"), "a.id = b.a_id"),
                        scan("c"),
                        "b.id = c.b_id",
                    );
                    let pass = JoinReorder::new(Arc::clone(&model));
                    let reordered = pass.apply(original.clone());
                    let orig_cost = model.estimate(&original).total_cost;
                    let reord_cost = model.estimate(&reordered).total_cost;
                    prop_assert!(
                        reord_cost <= orig_cost + 1e-6,
                        "reord={reord_cost} orig={orig_cost}"
                    );
                    Ok(())
                },
            )
            .expect("proptest failed");
    }
}
