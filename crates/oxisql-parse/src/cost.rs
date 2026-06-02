//! Query cost model for [`LogicalPlan`] trees.
//!
//! [`CostModel`] estimates the relative cost of executing a logical plan,
//! returning a [`CostEstimate`] that breaks down rows, CPU cost, and I/O cost.
//!
//! # Example
//!
//! ```rust
//! use oxisql_parse::{CostModel, TableStats, plan_statement, parse_one};
//!
//! let stmt = parse_one("SELECT id FROM users WHERE active = true").unwrap();
//! let plan = plan_statement(&stmt).unwrap();
//! let model = CostModel::new()
//!     .with_table_stats("users", TableStats { row_count: 50_000, avg_row_size_bytes: 80, index_on: vec![] });
//! let est = model.estimate(&plan);
//! assert!(est.rows < 50_000.0);
//! ```

use crate::LogicalPlan;
use std::collections::HashMap;

// ── CostEstimate ─────────────────────────────────────────────────────────────

/// The estimated cost breakdown for a [`LogicalPlan`] node.
#[derive(Debug, Clone, PartialEq)]
pub struct CostEstimate {
    /// Estimated number of output rows.
    pub rows: f64,
    /// Relative CPU cost units.
    pub cpu_cost: f64,
    /// Relative I/O cost units (page reads).
    pub io_cost: f64,
    /// Weighted total: `io_cost + cpu_weight * cpu_cost`.
    pub total_cost: f64,
}

impl CostEstimate {
    /// A zero-cost estimate (empty plan / leaf with no work).
    pub fn zero() -> Self {
        Self {
            rows: 0.0,
            cpu_cost: 0.0,
            io_cost: 0.0,
            total_cost: 0.0,
        }
    }

    /// Combine two estimates by summing all fields component-wise.
    ///
    /// Used for set operations and parallel branches.
    pub fn combine(a: &Self, b: &Self) -> Self {
        Self {
            rows: a.rows + b.rows,
            cpu_cost: a.cpu_cost + b.cpu_cost,
            io_cost: a.io_cost + b.io_cost,
            total_cost: a.total_cost + b.total_cost,
        }
    }
}

// ── TableStats ───────────────────────────────────────────────────────────────

/// Per-table statistics used to calibrate cost estimates.
#[derive(Debug, Clone)]
pub struct TableStats {
    /// Approximate number of rows in the table.
    pub row_count: u64,
    /// Average row size in bytes (used to estimate page reads).
    pub avg_row_size_bytes: u64,
    /// Column names that have an index.
    pub index_on: Vec<String>,
}

impl Default for TableStats {
    fn default() -> Self {
        Self {
            row_count: 10_000,
            avg_row_size_bytes: 100,
            index_on: vec![],
        }
    }
}

// ── CostModel ────────────────────────────────────────────────────────────────

/// Query cost model that estimates relative execution cost of a [`LogicalPlan`].
///
/// Missing tables fall back to [`TableStats::default`].
pub struct CostModel {
    /// Per-table statistics keyed by lowercase table name.
    stats: HashMap<String, TableStats>,
    /// Weight of CPU cost in `total_cost`. Default `0.01` (I/O dominates).
    cpu_weight: f64,
    /// Fraction of rows passing a single filter predicate. Default `0.1`.
    filter_selectivity: f64,
}

impl Default for CostModel {
    fn default() -> Self {
        Self::new()
    }
}

impl CostModel {
    /// Create a new [`CostModel`] with default weights and no table statistics.
    pub fn new() -> Self {
        Self {
            stats: HashMap::new(),
            cpu_weight: 0.01,
            filter_selectivity: 0.1,
        }
    }

    /// Register per-table statistics for a specific table.
    ///
    /// Table names are compared case-insensitively.
    pub fn with_table_stats(mut self, table: &str, stats: TableStats) -> Self {
        self.stats.insert(table.to_lowercase(), stats);
        self
    }

    /// Override the CPU-cost weight used to compute `total_cost`.
    pub fn with_cpu_weight(mut self, w: f64) -> Self {
        self.cpu_weight = w;
        self
    }

    /// Override the per-predicate filter selectivity (fraction of rows passing).
    pub fn with_filter_selectivity(mut self, s: f64) -> Self {
        self.filter_selectivity = s;
        self
    }

    /// Look up (or default) statistics for a table.
    fn table_stats(&self, table: &str) -> TableStats {
        self.stats
            .get(&table.to_lowercase())
            .cloned()
            .unwrap_or_default()
    }

    /// Compute the `total_cost` from component costs.
    fn total(&self, cpu_cost: f64, io_cost: f64) -> f64 {
        io_cost + self.cpu_weight * cpu_cost
    }

    /// Estimate the cost of executing `plan`.
    pub fn estimate(&self, plan: &LogicalPlan) -> CostEstimate {
        match plan {
            // ── Scan ──────────────────────────────────────────────────────
            LogicalPlan::Scan { table, .. } => {
                let s = self.table_stats(table);
                let rows = s.row_count as f64;
                let io_cost = rows * s.avg_row_size_bytes as f64 / 8192.0;
                let cpu_cost = rows * self.cpu_weight;
                CostEstimate {
                    rows,
                    cpu_cost,
                    io_cost,
                    total_cost: self.total(cpu_cost, io_cost),
                }
            }

            // ── Filter ────────────────────────────────────────────────────
            LogicalPlan::Filter { input, .. } => {
                let inp = self.estimate(input);
                let rows = inp.rows * self.filter_selectivity;
                let cpu_cost = inp.cpu_cost + inp.rows * 0.1;
                let io_cost = inp.io_cost;
                CostEstimate {
                    rows,
                    cpu_cost,
                    io_cost,
                    total_cost: self.total(cpu_cost, io_cost),
                }
            }

            // ── Project ───────────────────────────────────────────────────
            LogicalPlan::Project { input, .. } => {
                let inp = self.estimate(input);
                let rows = inp.rows;
                let cpu_cost = inp.cpu_cost + rows * 0.01;
                let io_cost = inp.io_cost;
                CostEstimate {
                    rows,
                    cpu_cost,
                    io_cost,
                    total_cost: self.total(cpu_cost, io_cost),
                }
            }

            // ── Join ──────────────────────────────────────────────────────
            LogicalPlan::Join { left, right, .. } => {
                let l = self.estimate(left);
                let r = self.estimate(right);
                let rows = l.rows * r.rows * 0.1;
                let cpu_cost = l.cpu_cost + r.cpu_cost + l.rows + r.rows + rows;
                let io_cost = l.io_cost + r.io_cost;
                CostEstimate {
                    rows,
                    cpu_cost,
                    io_cost,
                    total_cost: self.total(cpu_cost, io_cost),
                }
            }

            // ── Aggregate ─────────────────────────────────────────────────
            LogicalPlan::Aggregate { input, .. } => {
                let inp = self.estimate(input);
                let rows = inp.rows * 0.1;
                let cpu_cost = inp.cpu_cost + inp.rows * 0.5;
                let io_cost = inp.io_cost;
                CostEstimate {
                    rows,
                    cpu_cost,
                    io_cost,
                    total_cost: self.total(cpu_cost, io_cost),
                }
            }

            // ── Sort ──────────────────────────────────────────────────────
            LogicalPlan::Sort { input, .. } => {
                let inp = self.estimate(input);
                let rows = inp.rows;
                let sort_cpu = rows * rows.ln().max(1.0) * 0.01;
                let cpu_cost = inp.cpu_cost + sort_cpu;
                let io_cost = inp.io_cost;
                CostEstimate {
                    rows,
                    cpu_cost,
                    io_cost,
                    total_cost: self.total(cpu_cost, io_cost),
                }
            }

            // ── Limit ─────────────────────────────────────────────────────
            LogicalPlan::Limit { input, count, .. } => {
                let inp = self.estimate(input);
                let rows = match count {
                    Some(c) => inp.rows.min(*c as f64),
                    None => inp.rows,
                };
                let cpu_cost = inp.cpu_cost;
                let io_cost = inp.io_cost;
                CostEstimate {
                    rows,
                    cpu_cost,
                    io_cost,
                    total_cost: self.total(cpu_cost, io_cost),
                }
            }

            // ── Values / Empty ────────────────────────────────────────────
            LogicalPlan::Values { .. } | LogicalPlan::Empty => CostEstimate::zero(),

            // ── SetOp (UNION / INTERSECT / EXCEPT) ───────────────────────
            LogicalPlan::SetOp { left, right, .. } => {
                let l = self.estimate(left);
                let r = self.estimate(right);
                CostEstimate::combine(&l, &r)
            }

            // ── CTE definition ────────────────────────────────────────────
            LogicalPlan::Cte { query, .. } => {
                let inner = self.estimate(query);
                let cpu_cost = inner.cpu_cost + inner.rows * 0.05;
                let io_cost = inner.io_cost;
                let rows = inner.rows;
                CostEstimate {
                    rows,
                    cpu_cost,
                    io_cost,
                    total_cost: self.total(cpu_cost, io_cost),
                }
            }

            // ── CteRef — no child plan; use default table stats ──────────
            LogicalPlan::CteRef { name } => {
                let s = self.table_stats(name);
                let rows = s.row_count as f64;
                let io_cost = rows * s.avg_row_size_bytes as f64 / 8192.0;
                let cpu_cost = rows * self.cpu_weight + rows * 0.05;
                CostEstimate {
                    rows,
                    cpu_cost,
                    io_cost,
                    total_cost: self.total(cpu_cost, io_cost),
                }
            }

            // ── Window ────────────────────────────────────────────────────
            LogicalPlan::Window { input, .. } => {
                let inp = self.estimate(input);
                let rows = inp.rows;
                let cpu_cost = inp.cpu_cost + rows * 0.1;
                let io_cost = inp.io_cost;
                CostEstimate {
                    rows,
                    cpu_cost,
                    io_cost,
                    total_cost: self.total(cpu_cost, io_cost),
                }
            }

            // ── Subquery ──────────────────────────────────────────────────
            LogicalPlan::Subquery { query, .. } => {
                let inner = self.estimate(query);
                let rows = inner.rows;
                let cpu_cost = inner.cpu_cost + rows * 0.2;
                let io_cost = inner.io_cost;
                CostEstimate {
                    rows,
                    cpu_cost,
                    io_cost,
                    total_cost: self.total(cpu_cost, io_cost),
                }
            }

            // ── Exists ────────────────────────────────────────────────────
            LogicalPlan::Exists { subquery, .. } => {
                let inner = self.estimate(subquery);
                // EXISTS short-circuits; treat output as at most 1 row probe.
                let rows = 1.0_f64.min(inner.rows);
                let cpu_cost = inner.cpu_cost + 0.1;
                let io_cost = inner.io_cost;
                CostEstimate {
                    rows,
                    cpu_cost,
                    io_cost,
                    total_cost: self.total(cpu_cost, io_cost),
                }
            }

            // ── InSubquery ────────────────────────────────────────────────
            LogicalPlan::InSubquery { subquery, .. } => {
                let inner = self.estimate(subquery);
                let rows = inner.rows * self.filter_selectivity;
                let cpu_cost = inner.cpu_cost + inner.rows * 0.15;
                let io_cost = inner.io_cost;
                CostEstimate {
                    rows,
                    cpu_cost,
                    io_cost,
                    total_cost: self.total(cpu_cost, io_cost),
                }
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{parse_one, plan_statement};

    /// Scan with no registered stats uses `TableStats::default` (10_000 rows).
    #[test]
    fn test_cost_scan_default() {
        let stmt = parse_one("SELECT * FROM users").expect("parse");
        let plan = plan_statement(&stmt).expect("plan");
        let model = CostModel::new();
        let est = model.estimate(&plan);
        // Project preserves rows, so the top-level estimate rows should match scan rows.
        assert!(
            (est.rows - 10_000.0).abs() < 1e-9,
            "expected 10_000 rows from default stats, got {}",
            est.rows
        );
    }

    /// Scan with custom stats (50_000 rows) uses registered statistics.
    #[test]
    fn test_cost_scan_custom() {
        let stmt = parse_one("SELECT * FROM orders").expect("parse");
        let plan = plan_statement(&stmt).expect("plan");
        let model = CostModel::new().with_table_stats(
            "orders",
            TableStats {
                row_count: 50_000,
                avg_row_size_bytes: 128,
                index_on: vec![],
            },
        );
        let est = model.estimate(&plan);
        assert!(
            (est.rows - 50_000.0).abs() < 1e-9,
            "expected 50_000 rows, got {}",
            est.rows
        );
    }

    /// Filter over Scan yields fewer rows than the raw scan.
    #[test]
    fn test_cost_filter_reduces_rows() {
        let stmt = parse_one("SELECT * FROM users WHERE active = true").expect("parse");
        let plan = plan_statement(&stmt).expect("plan");
        let model = CostModel::new();
        let est = model.estimate(&plan);
        // Default scan = 10_000 rows; filter with 0.1 selectivity => 1_000 rows (then project).
        assert!(
            est.rows < 10_000.0,
            "filter should reduce rows below scan rows, got {}",
            est.rows
        );
    }

    /// Join of two default-stats scans: rows = 10_000 * 10_000 * 0.1 = 10_000_000.
    #[test]
    fn test_cost_join_two_scans() {
        let stmt = parse_one("SELECT a.id, b.name FROM a JOIN b ON a.id = b.a_id").expect("parse");
        let plan = plan_statement(&stmt).expect("plan");
        let model = CostModel::new();
        let est = model.estimate(&plan);
        let expected = 10_000.0_f64 * 10_000.0 * 0.1;
        assert!(
            (est.rows - expected).abs() < 1.0,
            "join rows expected ~{expected}, got {}",
            est.rows
        );
    }

    /// Limit caps the row count.
    #[test]
    fn test_cost_limit_caps_rows() {
        let stmt = parse_one("SELECT * FROM users LIMIT 100").expect("parse");
        let plan = plan_statement(&stmt).expect("plan");
        let model = CostModel::new();
        let est = model.estimate(&plan);
        assert!(
            (est.rows - 100.0).abs() < 1e-9,
            "limit should cap rows to 100, got {}",
            est.rows
        );
    }

    /// Aggregate reduces row count relative to scan.
    #[test]
    fn test_cost_aggregate_reduces_rows() {
        let stmt = parse_one("SELECT dept, COUNT(*) FROM employees GROUP BY dept").expect("parse");
        let plan = plan_statement(&stmt).expect("plan");
        let model = CostModel::new();
        let est = model.estimate(&plan);
        assert!(
            est.rows < 10_000.0,
            "aggregate should reduce rows, got {}",
            est.rows
        );
    }

    /// Sort preserves row count and increases CPU cost.
    #[test]
    fn test_cost_sort_preserves_rows() {
        let stmt = parse_one("SELECT * FROM products ORDER BY price DESC").expect("parse");
        let plan = plan_statement(&stmt).expect("plan");
        let model = CostModel::new();
        let full_est = model.estimate(&plan);

        // Build a plan without ORDER BY to get a baseline scan/project estimate.
        let stmt_no_sort = parse_one("SELECT * FROM products").expect("parse_no_sort");
        let plan_no_sort = plan_statement(&stmt_no_sort).expect("plan_no_sort");
        let base_est = model.estimate(&plan_no_sort);

        // Rows unchanged.
        assert!(
            (full_est.rows - base_est.rows).abs() < 1e-9,
            "sort should preserve row count: {} vs {}",
            full_est.rows,
            base_est.rows
        );
        // Sort adds CPU cost.
        assert!(
            full_est.cpu_cost > base_est.cpu_cost,
            "sort should increase cpu_cost: {} vs {}",
            full_est.cpu_cost,
            base_est.cpu_cost
        );
    }

    /// `CostEstimate::combine` sums all fields correctly.
    #[test]
    fn test_cost_combines_correctly() {
        let a = CostEstimate {
            rows: 100.0,
            cpu_cost: 5.0,
            io_cost: 10.0,
            total_cost: 10.05,
        };
        let b = CostEstimate {
            rows: 200.0,
            cpu_cost: 3.0,
            io_cost: 7.0,
            total_cost: 7.03,
        };
        let c = CostEstimate::combine(&a, &b);
        assert!((c.rows - 300.0).abs() < 1e-9, "rows: {}", c.rows);
        assert!((c.cpu_cost - 8.0).abs() < 1e-9, "cpu_cost: {}", c.cpu_cost);
        assert!((c.io_cost - 17.0).abs() < 1e-9, "io_cost: {}", c.io_cost);
        assert!(
            (c.total_cost - 17.08).abs() < 1e-9,
            "total_cost: {}",
            c.total_cost
        );
    }
}
