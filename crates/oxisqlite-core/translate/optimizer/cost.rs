use super::constraints::{Constraint, ConstraintRef};
use limbo_sqlite3_parser::ast;

/// A simple newtype wrapper over a f64 that represents the cost of an operation.
///
/// This is used to estimate the cost of scans, seeks, and joins.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Cost(pub f64);

impl std::ops::Add for Cost {
    type Output = Cost;

    fn add(self, other: Cost) -> Cost {
        Cost(self.0 + other.0)
    }
}

impl std::ops::Deref for Cost {
    type Target = f64;

    fn deref(&self) -> &f64 {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexInfo {
    pub unique: bool,
    pub column_count: usize,
    pub covering: bool,
}

pub const ESTIMATED_HARDCODED_ROWS_PER_TABLE: usize = 1000000;
pub const ESTIMATED_HARDCODED_ROWS_PER_PAGE: usize = 50; // roughly 80 bytes per 4096 byte page

pub fn estimate_page_io_cost(rowcount: f64) -> Cost {
    Cost((rowcount as f64 / ESTIMATED_HARDCODED_ROWS_PER_PAGE as f64).ceil())
}

/// Estimate the cost of a scan or seek operation.
///
/// This is a very simple model that estimates the number of pages read
/// based on the number of rows read, ignoring any CPU costs.
pub fn estimate_cost_for_scan_or_seek(
    index_info: Option<IndexInfo>,
    constraints: &[Constraint],
    usable_constraint_refs: &[ConstraintRef],
    input_cardinality: f64,
    base_row_count: f64,
    index_stats: Option<&[i64]>,
) -> Cost {
    let Some(index_info) = index_info else {
        return estimate_page_io_cost(input_cardinality * base_row_count);
    };

    // Length of the leading run of equality constraints in the usable refs
    // (refs are pre-sorted equalities-first and truncated after the first range).
    let equality_prefix_len = usable_constraint_refs
        .iter()
        .take_while(|cref| constraints[cref.constraint_vec_pos].operator == ast::Operator::Equals)
        .count();

    let selectivity_multiplier: f64 = match index_stats {
        // Stat'd index with an equality prefix: use the average rows-per-distinct
        // value for the first `equality_prefix_len` columns from sqlite_stat1.
        Some(v) if !v.is_empty() && equality_prefix_len >= 1 => {
            let idx = equality_prefix_len.min(v.len()) - 1;
            v[idx] as f64 / base_row_count.max(1.0)
        }
        // Fallback (no stats, empty stat vector, or no equality prefix): the
        // original per-column selectivity product — bit-for-bit unchanged.
        _ => usable_constraint_refs
            .iter()
            .map(|cref| {
                let constraint = &constraints[cref.constraint_vec_pos];
                constraint.selectivity
            })
            .product(),
    };

    // little cheeky bonus for covering indexes
    let covering_multiplier = if index_info.covering { 0.9 } else { 1.0 };

    estimate_page_io_cost(
        selectivity_multiplier * base_row_count * input_cardinality * covering_multiplier,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// A smaller `base_row_count` must scale the base-table size estimate, so a
    /// 10-row table costs `estimate_page_io_cost(10.0)` = `Cost(1.0)`, NOT the
    /// hardcoded million-row cost.
    fn base_row_count_scales_base_size() {
        let cost = estimate_cost_for_scan_or_seek(None, &[], &[], 1.0, 10.0, None);
        assert_eq!(cost, estimate_page_io_cost(10.0));
        assert_eq!(cost, Cost((10.0_f64 / 50.0).ceil()));
        assert_eq!(cost, Cost(1.0));
        assert_ne!(cost, Cost((1_000_000.0_f64 / 50.0).ceil()));
    }

    #[test]
    /// Explicit fallback guard: with `base_row_count = ESTIMATED_HARDCODED_ROWS_PER_TABLE`
    /// and no index stats, the cost must equal the old hardcoded value
    /// bit-for-bit (`Cost(20000.0)`).
    fn no_stats_regression_matches_hardcoded() {
        let cost = estimate_cost_for_scan_or_seek(
            None,
            &[],
            &[],
            1.0,
            ESTIMATED_HARDCODED_ROWS_PER_TABLE as f64,
            None,
        );
        let expected = Cost(
            (ESTIMATED_HARDCODED_ROWS_PER_TABLE as f64 / ESTIMATED_HARDCODED_ROWS_PER_PAGE as f64)
                .ceil(),
        );
        assert_eq!(cost, expected);
        assert_eq!(cost, Cost(20000.0));
    }

    #[cfg(feature = "index_experimental")]
    #[test]
    /// A stat'd index with an equality prefix lowers the seek cost below both
    /// the un-stat'd seek and the full table scan, flipping the planner's choice.
    fn index_prefix_stat_lowers_cost() {
        use super::super::constraints::BinaryExprSide;
        use crate::translate::planner::TableMask;
        use limbo_sqlite3_parser::ast::SortOrder;

        let constraints = vec![Constraint {
            where_clause_pos: (0, BinaryExprSide::Rhs),
            operator: ast::Operator::Equals,
            table_col_pos: 0,
            lhs_mask: TableMask::new(),
            selectivity: 0.01,
        }];
        let refs = vec![ConstraintRef {
            constraint_vec_pos: 0,
            index_col_pos: 0,
            sort_order: SortOrder::Asc,
        }];
        let info = IndexInfo {
            unique: false,
            column_count: 1,
            covering: false,
        };

        let cost_no_stats =
            estimate_cost_for_scan_or_seek(Some(info), &constraints, &refs, 1.0, 1_000_000.0, None);
        let cost_stats = estimate_cost_for_scan_or_seek(
            Some(info),
            &constraints,
            &refs,
            1.0,
            1_000_000.0,
            Some(&[1]),
        );
        let scan_cost = estimate_cost_for_scan_or_seek(None, &[], &[], 1.0, 1_000_000.0, None);

        assert!(
            cost_stats < cost_no_stats,
            "stat'd seek {cost_stats:?} should beat un-stat'd seek {cost_no_stats:?}"
        );
        assert!(
            cost_stats < scan_cost,
            "stat'd seek {cost_stats:?} should beat full scan {scan_cost:?}"
        );
    }
}
