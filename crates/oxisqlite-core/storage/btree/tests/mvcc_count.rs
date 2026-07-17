//! Regression test for `BTreeCursor::count()` under MVCC.
//!
//! `count()` used to `todo!("Implement count for mvcc")` whenever the cursor
//! had an attached `mv_cursor`. The MVCC scan cursor (`ScanCursor` / the
//! `MvCursor` alias) already eagerly collects the full `row_ids: Vec<RowID>`
//! in its constructor (`ScanCursor::new` -> `MvStore::scan_row_ids_for_table`),
//! so `count()` is implemented as `mv_cursor.borrow().row_ids.len()` --
//! directly analogous to the on-disk-btree `count()` a few lines below it,
//! which walks btree pages summing `cell_count()` without any MVCC
//! visibility filtering either. This test inserts a known set of rows
//! directly into the `MvStore` (bypassing SQL), then confirms `count()`
//! matches both the expected row count and an independent manual
//! enumeration via `scan_row_ids_for_table`.

use super::tests_2::empty_btree;
use super::*;
use crate::mvcc::clock::LocalClock;
use crate::mvcc::database::{Row, RowID};
use crate::mvcc::persistent_storage::Storage;
use crate::{MvCursor, MvStore};
use std::cell::RefCell;
use std::rc::Rc;

#[test]
fn test_count_mvcc_matches_manual_row_id_enumeration() {
    // `count()` unconditionally seeks to the physical root page once per
    // fresh cursor (`if self.count == 0 { self.move_to_root(); }`), even for
    // MVCC cursors, so a minimal real pager + root page is still required
    // even though row storage itself goes through the `MvStore`.
    let (pager, root_page) = empty_btree();
    let table_id = root_page as u64;

    let mv_store = Rc::new(MvStore::new(LocalClock::new(), Storage::new_noop()));
    let tx_id = mv_store.begin_tx();

    let row_ids = [10i64, 20, 30, 40, 50];
    for &rid in &row_ids {
        mv_store
            .insert(tx_id, Row::new(RowID::new(table_id, rid), vec![1, 2, 3]))
            .unwrap();
    }

    // Ground truth, enumerated independently of the cursor under test:
    // `scan_row_ids_for_table` is exactly what `ScanCursor::new` calls to
    // populate `row_ids`, and (like `count()`'s non-MVCC path) it does not
    // filter by transaction visibility -- it returns every row id present
    // for the table, committed or not.
    let manually_enumerated = mv_store.scan_row_ids_for_table(table_id).unwrap().len();
    assert_eq!(manually_enumerated, row_ids.len());

    let mv_cursor = Rc::new(RefCell::new(
        MvCursor::new(mv_store.clone(), tx_id, table_id).unwrap(),
    ));
    let mut cursor = BTreeCursor::new_table(Some(mv_cursor), pager, root_page);

    let CursorResult::Ok(count) = cursor.count().unwrap() else {
        panic!("BTreeCursor::count() unexpectedly returned CursorResult::IO for an MVCC cursor");
    };
    assert_eq!(count, manually_enumerated);
    assert_eq!(count, row_ids.len());
}
