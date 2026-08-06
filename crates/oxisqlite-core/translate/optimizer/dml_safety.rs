//! Safety gating for index-based table access in DELETE/UPDATE plans.
//!
//! `optimize_table_access` (this module's parent) was originally written for SELECT,
//! where a cursor is never mutated while it is being traversed (SELECT never writes).
//! DELETE and UPDATE break that assumption, and allowing index-based access
//! unconditionally is a proven, historically real corruption bug class rather than a
//! hypothetical one:
//!
//! - Using a SECONDARY index to drive a DELETE's row loop, while also deleting from
//!   that very index cursor as part of per-row index maintenance, corrupts the
//!   traversal once a page rebalance happens mid-scan. This is not hypothetical: it
//!   is the exact upstream bug at <https://github.com/tursodatabase/limbo/issues/1714>
//!   ("Using index to iterate table in DELETE is buggy"). A follow-up minimal repro
//!   posted on that issue (`CREATE INDEX idx_name ON test(name); DELETE FROM test
//!   WHERE name > 'C';`) deleted every row in the table instead of only the matching
//!   ones -- i.e. this is a "which index" problem, not a "which column" problem:
//!   the index used in that repro has nothing to do with the delete's semantics
//!   (DELETE has no SET clause to make any column "touched"). Upstream's own fix was
//!   to unconditionally disable index-based access for DELETE; that remains the
//!   correct, conservative choice for that specific shape here too.
//! - Using an index to drive an UPDATE's row loop while the SET clause also mutates
//!   that index's own key columns has a different, but equally real, hazard:
//!   `BTreeCursor::insert()` always repositions the cursor onto the row's NEW key
//!   after `IdxDelete` + `IdxInsert`, so a subsequent `Next()` continues from the
//!   new position instead of the old one, silently skipping or re-visiting rows.
//!   This is the scenario the original FIXME comment described (`UPDATE t SET
//!   x=x+5 WHERE x>10` with an index on `x`).
//!
//! What IS safe, and what this module enables:
//!
//! - [`Search::RowidEq`] never loops -- it is a single `SeekRowid` -- so it can
//!   never hit either hazard above, for DELETE or UPDATE, unconditionally.
//! - A rowid/PK-range traversal (`Search::Seek { index: None, .. }`) loops over the
//!   table's OWN cursor: the exact cursor that already self-mutates via
//!   `Delete`/`Insert` on every row of today's mandatory full-table-scan fallback.
//!   For DELETE this is unconditionally safe: DELETE never changes column values,
//!   so the key this cursor is ordered by never shifts under it, and narrowing the
//!   scan's start/end bounds changes nothing about that self-mutation mechanism.
//!   For UPDATE it is safe as long as the SET clause does not itself change the
//!   table's rowid (or, for `WITHOUT ROWID` tables, any primary-key column) --
//!   otherwise the driving cursor's own key would shift under it exactly like the
//!   secondary-index case above, just applied to the table's own btree.
//! - A secondary-index traversal (`Operation::Scan { index: Some(_), .. }` /
//!   `Search::Seek { index: Some(_), .. }`) is never enabled for DELETE (see above).
//!   For UPDATE it is safe exactly when the index's columns are disjoint from the
//!   SET clause's target columns, i.e. the index is not in `indexes_to_update`:
//!   in that case nothing ever calls `IdxDelete`/`IdxInsert` against that index's
//!   cursor during the statement, so it never gets warped out from under the scan.
//!
//! Anything not covered above keeps falling back to the pre-existing, always-safe
//! `Operation::Scan { index: None, .. }` full table scan.

use limbo_sqlite3_parser::ast::{Expr, SortOrder};

use crate::schema::{Index, Table};
use crate::translate::plan::{JoinedTable, Operation, Search, UpdatePlan, WhereTerm};

/// True if `op` would drive its row loop by looping over a SECONDARY index
/// cursor, as opposed to a non-looping rowid point lookup or a loop over the
/// table's own rowid/PK cursor.
fn uses_secondary_index_traversal(op: &Operation) -> bool {
    matches!(
        op,
        Operation::Scan { index: Some(_), .. }
            | Operation::Search(Search::Seek { index: Some(_), .. })
    )
}

/// Whether the access method `optimize_table_access` chose for a DELETE plan's
/// (single) table is safe to keep, per the module-level documentation above.
pub(super) fn delete_access_method_is_safe(op: &Operation) -> bool {
    !uses_secondary_index_traversal(op)
}

/// Whether the access method `optimize_table_access` chose for `table` is safe
/// to keep for `plan`, per the module-level documentation above.
pub(super) fn update_access_method_is_safe(table: &JoinedTable, plan: &UpdatePlan) -> bool {
    match &table.op {
        Operation::Scan { index: None, .. } => true,
        Operation::Search(Search::RowidEq { .. }) => true,
        Operation::Search(Search::Seek { index: None, .. }) => {
            set_clause_disjoint_from_own_key(table, plan)
        }
        Operation::Scan {
            index: Some(index), ..
        }
        | Operation::Search(Search::Seek {
            index: Some(index), ..
        }) => set_clause_disjoint_from_index(index, plan),
    }
}

/// True if none of `plan`'s SET-clause target columns are part of the physical
/// ordering key that a rowid/PK-range traversal over `table`'s own cursor would
/// depend on: the rowid-alias column for an ordinary rowid table, or the
/// primary-key column positions for a `WITHOUT ROWID` table.
fn set_clause_disjoint_from_own_key(table: &JoinedTable, plan: &UpdatePlan) -> bool {
    let Table::BTree(btree) = &table.table else {
        // Search::Seek{index: None} over a non-btree table is not a shape this
        // optimizer produces for an UpdatePlan today (virtual tables always
        // resolve to Operation::Scan{index: None}); be conservative if that
        // assumption is ever violated by a future change.
        return false;
    };
    if btree.has_rowid {
        match table.columns().iter().position(|c| c.is_rowid_alias) {
            Some(rowid_alias_pos) => !plan
                .set_clauses
                .iter()
                .any(|(pos, _)| *pos == rowid_alias_pos),
            // No addressable rowid-alias column: the bare rowid cannot appear as
            // a SET target through the normal column-name resolution path (see
            // translate::update::prepare_update_plan), so there is nothing for
            // the SET clause to shift.
            None => true,
        }
    } else {
        let pk_len = btree.primary_key_columns.len();
        !plan.set_clauses.iter().any(|(pos, _)| *pos < pk_len)
    }
}

/// True if none of `index`'s columns are targeted by `plan`'s SET clause, i.e.
/// `index` is not in `plan.indexes_to_update` and therefore never receives an
/// `IdxDelete`/`IdxInsert` during the statement.
fn set_clause_disjoint_from_index(index: &Index, plan: &UpdatePlan) -> bool {
    !plan
        .indexes_to_update
        .iter()
        .any(|updated| updated.name == index.name)
}

/// Captures the mutable state that `optimize_table_access` may change for a
/// single-table DELETE/UPDATE plan, so it can be restored bit-for-bit if the
/// access method it picked turns out to be unsafe.
///
/// `optimize_table_access` only ever mutates three things for a given table:
/// its `Operation` (`table.op`), the `consumed` flag of `WhereTerm`s it turned
/// into seek-key constraints, and `order_by`/`group_by` if it proved the chosen
/// access method already satisfies the required order (DELETE/UPDATE plans have
/// no `group_by`, so only `order_by` needs to be tracked here).
pub(super) struct AccessMethodSnapshot {
    op: Operation,
    consumed: Vec<bool>,
    order_by: Option<Vec<(Expr, SortOrder)>>,
}

impl AccessMethodSnapshot {
    pub(super) fn capture(
        table: &JoinedTable,
        where_clause: &[WhereTerm],
        order_by: &Option<Vec<(Expr, SortOrder)>>,
    ) -> Self {
        Self {
            op: table.op.clone(),
            consumed: where_clause
                .iter()
                .map(|term| term.consumed.get())
                .collect(),
            order_by: order_by.clone(),
        }
    }

    /// Restore `table`'s access method, the WHERE-term `consumed` flags, and
    /// `order_by` to exactly what they were when [`Self::capture`] was called.
    pub(super) fn restore(
        self,
        table: &mut JoinedTable,
        where_clause: &[WhereTerm],
        order_by: &mut Option<Vec<(Expr, SortOrder)>>,
    ) {
        table.op = self.op;
        for (term, was_consumed) in where_clause.iter().zip(self.consumed.iter()) {
            term.consumed.set(*was_consumed);
        }
        *order_by = self.order_by;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{Column, IndexColumn, Type};
    use crate::translate::plan::{ColumnUsedMask, IterationDirection, TableReferences};
    use limbo_sqlite3_parser::ast;
    use std::cell::Cell;
    use std::rc::Rc;
    use std::sync::Arc;

    fn column(name: &str, is_rowid_alias: bool) -> Column {
        Column {
            name: Some(name.to_string()),
            ty: Type::Integer,
            ty_str: "INTEGER".to_string(),
            primary_key: is_rowid_alias,
            is_rowid_alias,
            notnull: false,
            default: None,
            unique: false,
            unique_conflict: ast::ResolveType::Abort,
            collation: None,
            is_generated: false,
        }
    }

    fn rowid_table(name: &str, columns: Vec<Column>) -> Rc<crate::schema::BTreeTable> {
        Rc::new(crate::schema::BTreeTable {
            db_index: 0,
            root_page: 2,
            name: name.to_string(),
            primary_key_columns: vec![],
            columns,
            has_rowid: true,
            is_strict: false,
            unique_sets: None,
            primary_key_conflict: ast::ResolveType::Abort,
            foreign_keys: vec![],
        })
    }

    fn without_rowid_table(
        name: &str,
        columns: Vec<Column>,
        pk_count: usize,
    ) -> Rc<crate::schema::BTreeTable> {
        Rc::new(crate::schema::BTreeTable {
            db_index: 0,
            root_page: 2,
            name: name.to_string(),
            primary_key_columns: columns[..pk_count]
                .iter()
                .map(|c| (c.name.clone().unwrap_or_default(), SortOrder::Asc))
                .collect(),
            columns,
            has_rowid: false,
            is_strict: false,
            unique_sets: None,
            primary_key_conflict: ast::ResolveType::Abort,
            foreign_keys: vec![],
        })
    }

    fn joined_table(table: Rc<crate::schema::BTreeTable>, op: Operation) -> JoinedTable {
        JoinedTable {
            op,
            table: Table::BTree(table),
            identifier: "t".to_string(),
            internal_id: ast::TableInternalId::default(),
            join_info: None,
            col_used_mask: ColumnUsedMask::new(),
        }
    }

    fn index(name: &str, table_name: &str, col_positions: &[usize]) -> Arc<Index> {
        Arc::new(Index {
            name: name.to_string(),
            table_name: table_name.to_string(),
            root_page: 3,
            unique: false,
            ephemeral: false,
            has_rowid: true,
            columns: col_positions
                .iter()
                .map(|&pos| IndexColumn {
                    name: format!("c{pos}"),
                    order: SortOrder::Asc,
                    pos_in_table: pos,
                    collation: None,
                    default: None,
                })
                .collect(),
            on_conflict: ast::ResolveType::Abort,
        })
    }

    fn empty_update_plan(
        table_references: TableReferences,
        set_clauses: Vec<(usize, Expr)>,
    ) -> UpdatePlan {
        UpdatePlan {
            table_references,
            set_clauses,
            where_clause: vec![],
            order_by: None,
            limit: None,
            offset: None,
            limit_expr: None,
            offset_expr: None,
            returning: None,
            contains_constant_false_condition: false,
            indexes_to_update: vec![],
            or_conflict: None,
        }
    }

    fn literal(n: i64) -> Expr {
        Expr::Literal(ast::Literal::Numeric(n.to_string()))
    }

    #[test]
    fn rowid_eq_is_always_safe_for_delete_and_update() {
        let op = Operation::Search(Search::RowidEq {
            cmp_expr: literal(5),
        });
        assert!(delete_access_method_is_safe(&op));

        let table = joined_table(rowid_table("t", vec![column("x", false)]), op);
        let plan = empty_update_plan(TableReferences::new(vec![], vec![]), vec![(0, literal(1))]);
        assert!(update_access_method_is_safe(&table, &plan));
    }

    #[test]
    fn secondary_index_traversal_is_never_safe_for_delete() {
        let idx = index("idx_x", "t", &[0]);
        let seek = Operation::Search(Search::Seek {
            index: Some(idx.clone()),
            seek_def: dummy_seek_def(),
        });
        assert!(!delete_access_method_is_safe(&seek));

        let scan = Operation::Scan {
            iter_dir: IterationDirection::Forwards,
            index: Some(idx),
        };
        assert!(!delete_access_method_is_safe(&scan));
    }

    #[test]
    fn rowid_range_seek_is_always_safe_for_delete() {
        let op = Operation::Search(Search::Seek {
            index: None,
            seek_def: dummy_seek_def(),
        });
        assert!(delete_access_method_is_safe(&op));
    }

    #[test]
    fn plain_scan_is_safe_baseline_for_both() {
        let op = Operation::Scan {
            iter_dir: IterationDirection::Forwards,
            index: None,
        };
        assert!(delete_access_method_is_safe(&op));

        let table = joined_table(rowid_table("t", vec![column("x", false)]), op);
        let plan = empty_update_plan(TableReferences::new(vec![], vec![]), vec![(0, literal(1))]);
        assert!(update_access_method_is_safe(&table, &plan));
    }

    #[test]
    fn update_rowid_range_seek_unsafe_when_set_touches_rowid_alias() {
        // Table: rowid alias is column 0 ("id"). SET touches column 0 -> unsafe.
        let table_def = rowid_table("t", vec![column("id", true), column("x", false)]);
        let op = Operation::Search(Search::Seek {
            index: None,
            seek_def: dummy_seek_def(),
        });
        let table = joined_table(table_def, op);
        let plan = empty_update_plan(TableReferences::new(vec![], vec![]), vec![(0, literal(1))]);
        assert!(!update_access_method_is_safe(&table, &plan));
    }

    #[test]
    fn update_rowid_range_seek_safe_when_set_touches_other_column() {
        // Table: rowid alias is column 0 ("id"). SET touches column 1 ("x") -> safe.
        let table_def = rowid_table("t", vec![column("id", true), column("x", false)]);
        let op = Operation::Search(Search::Seek {
            index: None,
            seek_def: dummy_seek_def(),
        });
        let table = joined_table(table_def, op);
        let plan = empty_update_plan(TableReferences::new(vec![], vec![]), vec![(1, literal(1))]);
        assert!(update_access_method_is_safe(&table, &plan));
    }

    #[test]
    fn update_rowid_range_seek_safe_when_no_rowid_alias_column() {
        // No column is is_rowid_alias -> the bare rowid can't be a SET target.
        let table_def = rowid_table("t", vec![column("x", false), column("y", false)]);
        let op = Operation::Search(Search::Seek {
            index: None,
            seek_def: dummy_seek_def(),
        });
        let table = joined_table(table_def, op);
        let plan = empty_update_plan(TableReferences::new(vec![], vec![]), vec![(0, literal(1))]);
        assert!(update_access_method_is_safe(&table, &plan));
    }

    #[test]
    fn update_without_rowid_pk_range_seek_unsafe_when_set_touches_pk_column() {
        // WITHOUT ROWID table: first 2 columns are the PK. SET touches column 1 (a PK column) -> unsafe.
        let table_def = without_rowid_table(
            "t",
            vec![column("k1", false), column("k2", false), column("v", false)],
            2,
        );
        let op = Operation::Search(Search::Seek {
            index: None,
            seek_def: dummy_seek_def(),
        });
        let table = joined_table(table_def, op);
        let plan = empty_update_plan(TableReferences::new(vec![], vec![]), vec![(1, literal(1))]);
        assert!(!update_access_method_is_safe(&table, &plan));
    }

    #[test]
    fn update_without_rowid_pk_range_seek_safe_when_set_touches_non_pk_column() {
        let table_def = without_rowid_table(
            "t",
            vec![column("k1", false), column("k2", false), column("v", false)],
            2,
        );
        let op = Operation::Search(Search::Seek {
            index: None,
            seek_def: dummy_seek_def(),
        });
        let table = joined_table(table_def, op);
        let plan = empty_update_plan(TableReferences::new(vec![], vec![]), vec![(2, literal(1))]);
        assert!(update_access_method_is_safe(&table, &plan));
    }

    #[test]
    fn update_secondary_index_unsafe_when_its_columns_are_set_targets() {
        let idx = index("idx_x", "t", &[1]); // indexes column 1 ("x")
        let op = Operation::Search(Search::Seek {
            index: Some(idx.clone()),
            seek_def: dummy_seek_def(),
        });
        let table_def = rowid_table("t", vec![column("id", true), column("x", false)]);
        let table = joined_table(table_def, op);
        let plan_with_matching_index_to_update = UpdatePlan {
            indexes_to_update: vec![idx],
            ..empty_update_plan(TableReferences::new(vec![], vec![]), vec![(1, literal(1))])
        };
        assert!(!update_access_method_is_safe(
            &table,
            &plan_with_matching_index_to_update
        ));
    }

    #[test]
    fn update_secondary_index_safe_when_disjoint_from_set_clause() {
        // Index on column 1 ("x"); SET touches column 2 ("y") only, so idx_x is
        // NOT in indexes_to_update.
        let idx = index("idx_x", "t", &[1]);
        let op = Operation::Scan {
            iter_dir: IterationDirection::Forwards,
            index: Some(idx),
        };
        let table_def = rowid_table(
            "t",
            vec![column("id", true), column("x", false), column("y", false)],
        );
        let table = joined_table(table_def, op);
        // indexes_to_update is empty: no index's columns intersect the SET clause.
        let plan = empty_update_plan(TableReferences::new(vec![], vec![]), vec![(2, literal(1))]);
        assert!(update_access_method_is_safe(&table, &plan));
    }

    #[test]
    fn snapshot_restores_op_consumed_and_order_by() {
        let original_op = Operation::Scan {
            iter_dir: IterationDirection::Forwards,
            index: None,
        };
        let mut table = joined_table(
            rowid_table("t", vec![column("x", false)]),
            original_op.clone(),
        );
        let where_clause = vec![WhereTerm {
            expr: literal(1),
            from_outer_join: None,
            consumed: Cell::new(false),
        }];
        let mut order_by = Some(vec![(literal(1), SortOrder::Asc)]);

        let snapshot = AccessMethodSnapshot::capture(&table, &where_clause, &order_by);

        // Simulate what optimize_table_access would do: pick a different access
        // method, consume the where term, and eliminate the sort.
        let idx = index("idx_x", "t", &[0]);
        table.op = Operation::Search(Search::Seek {
            index: Some(idx),
            seek_def: dummy_seek_def(),
        });
        where_clause[0].consumed.set(true);
        order_by = None;

        snapshot.restore(&mut table, &where_clause, &mut order_by);

        assert!(matches!(table.op, Operation::Scan { index: None, .. }));
        assert!(!where_clause[0].consumed.get());
        assert!(order_by.is_some());
    }

    /// A minimal, semantically-unimportant [`crate::translate::plan::SeekDef`] for
    /// tests that only care about the `Operation`/`Search` shape, not the seek
    /// details.
    fn dummy_seek_def() -> crate::translate::plan::SeekDef {
        crate::translate::plan::SeekDef {
            key: vec![(literal(1), SortOrder::Asc)],
            seek: None,
            termination: None,
            iter_dir: IterationDirection::Forwards,
        }
    }
}

/// End-to-end tests proving that `optimize_plan` actually selects (or correctly
/// refuses to select) an index-based access method for real DELETE/UPDATE
/// statements against a real, `CREATE INDEX`-populated schema -- not just that
/// the pure predicates above are logically consistent. `CREATE INDEX` and
/// indexed-table DELETE/UPDATE both require `index_experimental`
/// (see translate/delete.rs, translate/update.rs, translate/index.rs), so these
/// only run under that feature, matching the rest of this codebase's convention
/// for such tests (e.g. tests/analyze.rs's `index_tests` module).
#[cfg(all(test, feature = "index_experimental"))]
mod plan_tests {
    use crate::translate::delete::prepare_delete_plan;
    use crate::translate::optimizer::optimize_plan;
    use crate::translate::plan::{Operation, Plan, Search};
    use crate::translate::update::prepare_update_plan;
    use crate::vdbe::builder::TableRefIdCounter;
    use crate::{Connection, Database, MemoryIO, StepResult, IO};
    use fallible_iterator::FallibleIterator;
    use limbo_sqlite3_parser::{ast, lexer::sql::Parser};
    use std::sync::Arc;

    fn new_conn() -> (Arc<dyn IO>, Arc<Connection>) {
        let io: Arc<dyn IO> = Arc::new(MemoryIO::new());
        let db = Database::open_file(io.clone(), ":memory:", false).expect("open in-memory db");
        let conn = db.connect().expect("connect");
        (io, conn)
    }

    fn exec(io: &Arc<dyn IO>, conn: &Arc<Connection>, sql: &str) {
        let mut stmt = conn
            .prepare(sql)
            .unwrap_or_else(|e| panic!("prepare {sql:?}: {e:?}"));
        loop {
            match stmt
                .step()
                .unwrap_or_else(|e| panic!("step {sql:?}: {e:?}"))
            {
                StepResult::Done => return,
                StepResult::IO | StepResult::Busy => io.run_once().expect("io run_once"),
                StepResult::Row => {}
                StepResult::Interrupt => panic!("interrupted"),
            }
        }
    }

    fn parse_one(sql: &str) -> ast::Stmt {
        let mut parser = Parser::new(sql.as_bytes());
        match parser
            .next()
            .expect("parse")
            .expect("nonempty input yields a command")
        {
            ast::Cmd::Stmt(stmt) => stmt,
            other => panic!("expected Cmd::Stmt, got {other:?}"),
        }
    }

    /// Parses and fully optimizes a `DELETE` statement against `conn`'s current
    /// schema, returning the access method chosen for its (only) table.
    fn optimized_delete_op(conn: &Arc<Connection>, sql: &str) -> Operation {
        let ast::Stmt::Delete(delete) = parse_one(sql) else {
            panic!("expected a DELETE statement: {sql:?}");
        };
        let ast::Delete {
            tbl_name,
            where_clause,
            limit,
            ..
        } = *delete;
        let schema = conn.schema.read();
        let mut counter = TableRefIdCounter::new();
        let mut plan = prepare_delete_plan(&schema, &tbl_name, where_clause, limit, &mut counter)
            .expect("prepare_delete_plan");
        optimize_plan(&mut plan, &schema).expect("optimize_plan");
        let Plan::Delete(delete_plan) = plan else {
            panic!("prepare_delete_plan did not return Plan::Delete");
        };
        delete_plan
            .table_references
            .joined_tables()
            .first()
            .expect("delete plan has exactly one table reference")
            .op
            .clone()
    }

    /// Parses and fully optimizes an `UPDATE` statement against `conn`'s current
    /// schema, returning the access method chosen for its (only) table.
    fn optimized_update_op(conn: &Arc<Connection>, sql: &str) -> Operation {
        let ast::Stmt::Update(update) = parse_one(sql) else {
            panic!("expected an UPDATE statement: {sql:?}");
        };
        let mut update = *update;
        let schema = conn.schema.read();
        let mut counter = TableRefIdCounter::new();
        let mut plan =
            prepare_update_plan(&schema, &mut update, &mut counter).expect("prepare_update_plan");
        optimize_plan(&mut plan, &schema).expect("optimize_plan");
        let Plan::Update(update_plan) = plan else {
            panic!("prepare_update_plan did not return Plan::Update");
        };
        update_plan
            .table_references
            .joined_tables()
            .first()
            .expect("update plan has exactly one table reference")
            .op
            .clone()
    }

    #[test]
    fn delete_uses_rowid_eq_for_pk_equality() {
        let (io, conn) = new_conn();
        exec(
            &io,
            &conn,
            "CREATE TABLE t(id INTEGER PRIMARY KEY, x INTEGER)",
        );
        let op = optimized_delete_op(&conn, "DELETE FROM t WHERE id = 5");
        assert!(
            matches!(op, Operation::Search(Search::RowidEq { .. })),
            "expected RowidEq, got {op:?}"
        );
    }

    #[test]
    fn delete_uses_rowid_range_seek_for_pk_range() {
        let (io, conn) = new_conn();
        exec(
            &io,
            &conn,
            "CREATE TABLE t(id INTEGER PRIMARY KEY, x INTEGER)",
        );
        let op = optimized_delete_op(&conn, "DELETE FROM t WHERE id > 10 AND id < 100");
        assert!(
            matches!(op, Operation::Search(Search::Seek { index: None, .. })),
            "expected Seek{{index: None}} (rowid range), got {op:?}"
        );
    }

    #[test]
    fn delete_still_falls_back_to_full_scan_for_secondary_index() {
        let (io, conn) = new_conn();
        exec(
            &io,
            &conn,
            "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT)",
        );
        exec(&io, &conn, "CREATE INDEX idx_name ON t(name)");
        // This is (a small variant of) the exact upstream repro from
        // tursodatabase/limbo#1714: DELETE driven by a secondary index must
        // never be selected, regardless of how selective/unrelated the index is.
        let op = optimized_delete_op(&conn, "DELETE FROM t WHERE name > 'C'");
        assert!(
            matches!(op, Operation::Scan { index: None, .. }),
            "expected full table scan (secondary-index DELETE must stay disabled), got {op:?}"
        );
    }

    #[test]
    fn update_uses_index_when_disjoint_from_set_clause() {
        let (io, conn) = new_conn();
        exec(
            &io,
            &conn,
            "CREATE TABLE t(id INTEGER PRIMARY KEY, x INTEGER, y INTEGER)",
        );
        exec(&io, &conn, "CREATE INDEX idx_x ON t(x)");
        let op = optimized_update_op(&conn, "UPDATE t SET y = y + 1 WHERE x > 10");
        match &op {
            Operation::Search(Search::Seek {
                index: Some(index), ..
            }) => assert_eq!(index.name, "idx_x"),
            other => panic!("expected a Seek over idx_x, got {other:?}"),
        }
    }

    #[test]
    fn update_falls_back_to_full_scan_when_set_touches_indexed_column() {
        let (io, conn) = new_conn();
        exec(
            &io,
            &conn,
            "CREATE TABLE t(id INTEGER PRIMARY KEY, x INTEGER)",
        );
        exec(&io, &conn, "CREATE INDEX idx_x ON t(x)");
        // This is the exact scenario the original FIXME comment described:
        // `UPDATE t SET x=x+5 WHERE x>10` with an index on `x`. It must keep
        // falling back to a full table scan.
        let op = optimized_update_op(&conn, "UPDATE t SET x = x + 5 WHERE x > 10");
        assert!(
            matches!(op, Operation::Scan { index: None, .. }),
            "expected full table scan (index key is also the SET target), got {op:?}"
        );
    }

    #[test]
    fn update_uses_rowid_eq_for_pk_equality() {
        let (io, conn) = new_conn();
        exec(
            &io,
            &conn,
            "CREATE TABLE t(id INTEGER PRIMARY KEY, x INTEGER)",
        );
        let op = optimized_update_op(&conn, "UPDATE t SET x = 1 WHERE id = 5");
        assert!(
            matches!(op, Operation::Search(Search::RowidEq { .. })),
            "expected RowidEq, got {op:?}"
        );
    }

    #[test]
    fn update_uses_rowid_range_seek_when_set_does_not_touch_rowid() {
        let (io, conn) = new_conn();
        exec(
            &io,
            &conn,
            "CREATE TABLE t(id INTEGER PRIMARY KEY, x INTEGER)",
        );
        let op = optimized_update_op(&conn, "UPDATE t SET x = x + 1 WHERE id > 10 AND id < 100");
        assert!(
            matches!(op, Operation::Search(Search::Seek { index: None, .. })),
            "expected Seek{{index: None}} (rowid range), got {op:?}"
        );
    }
}
