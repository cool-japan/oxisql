//! In-memory side-map of persisted `sqlite_stat1` statistics.
//!
//! Loaded after schema parsing; consumed by the System-R optimizer. When empty
//! (no ANALYZE has run, or no `sqlite_stat1` table exists) every lookup returns
//! `None`, so the optimizer falls back to its hardcoded estimates and behaves
//! bit-for-bit as before.

use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct SchemaStats {
    tables: HashMap<String, TableStat>,
}

#[derive(Debug, Clone, Default)]
pub struct TableStat {
    pub num_rows: i64,
    pub index_stats: HashMap<String, Vec<i64>>,
}

impl SchemaStats {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn clear(&mut self) {
        self.tables.clear();
    }
    pub fn is_empty(&self) -> bool {
        self.tables.is_empty()
    }

    /// The table's row count, filtered to `> 0` (so a zero/garbage count falls
    /// back to the optimizer's hardcoded estimate).
    pub fn num_rows(&self, table: &str) -> Option<i64> {
        self.tables
            .get(table)
            .map(|t| t.num_rows)
            .filter(|n| *n > 0)
    }

    pub fn index_stats(&self, table: &str, index: &str) -> Option<&[i64]> {
        self.tables
            .get(table)?
            .index_stats
            .get(index)
            .map(|v| v.as_slice())
    }

    /// Record one `sqlite_stat1` row. `idx` is the index name (None for the
    /// table-level row-count row). Malformed `stat` strings are skipped.
    pub fn record(&mut self, tbl: &str, idx: Option<&str>, stat: &str) {
        let Some((n, avgs)) = parse_stat1_line(stat) else {
            return;
        };
        let entry = self.tables.entry(tbl.to_string()).or_default();
        entry.num_rows = entry.num_rows.max(n);
        if let Some(index_name) = idx {
            entry.index_stats.insert(index_name.to_string(), avgs);
        }
    }
}

/// Parse a `sqlite_stat1.stat` string `"N a1 a2 … ak"`.
/// First whitespace token => `N` (None if missing/unparseable -> skip whole line).
/// Following tokens parse to i64 and are pushed; the FIRST non-integer token
/// stops parsing (tolerates trailing flags like `unordered`).
pub fn parse_stat1_line(stat: &str) -> Option<(i64, Vec<i64>)> {
    let mut it = stat.split_whitespace();
    let n: i64 = it.next()?.parse().ok()?;
    let mut avgs = Vec::new();
    for tok in it {
        match tok.parse::<i64>() {
            Ok(v) => avgs.push(v),
            Err(_) => break,
        }
    }
    Some((n, avgs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic() {
        assert_eq!(parse_stat1_line("6 3 1"), Some((6, vec![3, 1])));
    }
    #[test]
    fn parse_table_only() {
        assert_eq!(parse_stat1_line("42"), Some((42, vec![])));
    }
    #[test]
    fn parse_empty_is_none() {
        assert_eq!(parse_stat1_line(""), None);
        assert_eq!(parse_stat1_line("   "), None);
    }
    #[test]
    fn parse_garbage_is_none() {
        assert_eq!(parse_stat1_line("abc"), None);
    }
    #[test]
    fn parse_trailing_flag_tolerated() {
        assert_eq!(parse_stat1_line("100 10 unordered"), Some((100, vec![10])));
    }
    #[test]
    fn record_keeps_max_n_and_index() {
        let mut s = SchemaStats::new();
        s.record("t", None, "5");
        s.record("t", Some("idx_t_x"), "8 2");
        assert_eq!(s.num_rows("t"), Some(8));
        assert_eq!(s.index_stats("t", "idx_t_x"), Some(&[2i64][..]));
        assert_eq!(s.index_stats("t", "missing"), None);
        assert_eq!(s.num_rows("missing"), None);
    }
    #[test]
    fn record_malformed_skipped() {
        let mut s = SchemaStats::new();
        s.record("t", None, "not_a_number");
        assert!(s.is_empty());
        assert_eq!(s.num_rows("t"), None);
    }
}

/// Crate-internal end-to-end proof that `ANALYZE` populates the in-memory
/// side-map on the issuing connection's [`crate::schema::Schema`]. This reaches
/// the otherwise-private `Connection::schema` field (only possible from inside
/// the crate), so it asserts the spec's preferred `num_rows("t") == Some(N)`.
#[cfg(test)]
mod db_tests {
    use crate::{Connection, Database, MemoryIO, StepResult, IO};
    use std::sync::Arc;

    fn exec(io: &Arc<dyn IO>, conn: &Arc<Connection>, sql: &str) {
        let mut stmt = conn.prepare(sql).expect("prepare");
        loop {
            match stmt.step().expect("step") {
                StepResult::Done => break,
                StepResult::IO | StepResult::Busy => io.run_once().expect("io run_once"),
                StepResult::Row => {}
                StepResult::Interrupt => panic!("interrupted"),
            }
        }
    }

    #[test]
    fn analyze_populates_schema_stats_sidemap() {
        let io: Arc<dyn IO> = Arc::new(MemoryIO::new());
        let db = Database::open_file(io.clone(), ":memory:", false).expect("open in-memory db");
        let conn = db.connect().expect("connect");

        exec(&io, &conn, "CREATE TABLE t(x)");
        for i in 0..7 {
            exec(&io, &conn, &format!("INSERT INTO t VALUES ({i})"));
        }

        // Before ANALYZE the side-map is empty -> optimizer uses its fallback.
        assert!(conn.schema.read().stats.is_empty());
        assert_eq!(conn.schema.read().stats.num_rows("t"), None);

        exec(&io, &conn, "ANALYZE");

        // After ANALYZE, op_parse_schema reloaded sqlite_stat1 into the side-map.
        assert!(!conn.schema.read().stats.is_empty());
        assert_eq!(conn.schema.read().stats.num_rows("t"), Some(7));
    }
}
