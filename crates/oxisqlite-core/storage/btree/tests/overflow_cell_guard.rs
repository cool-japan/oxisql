//! Regression tests for `balance_non_root`'s overflowed-parent guards.
//!
//! `PageContent::overflow_cells` is a purely in-memory side vector: cells that
//! did not physically fit on the page and are waiting for `balance()` to
//! redistribute them. Balancing a child whose *parent* still holds pending
//! overflow cells is not implemented, and upstream limbo marked that with three
//! release-active `assert!`/`assert_eq!` calls — which abort the whole host
//! process, an unacceptable failure mode for an embedded library. They are now
//! typed `LimboError::Corrupt` returns, and the first of them additionally runs
//! *before* the parent is marked dirty so that a caught error leaves the pager's
//! dirty bookkeeping untouched.
//!
//! # Why there is no "refuse to commit a page with pending overflow cells" guard
//!
//! Investigated 2026-08-04 and deliberately rejected. `edit_page` clears
//! `overflow_cells` only on the pages it rewrites, and `balance()` stops as soon
//! as the cursor's own page is clean, so a *stale* entry — one whose payload has
//! already been written into some page's physical image — routinely survives to
//! commit. `SEED=33 VALIDATE_BTREE=true btree_insert_fuzz_run_overflow` reaches
//! `end_tx` with one such entry on page 47, and full per-insert validation still
//! passes: every key present, tree well-formed. A commit-time guard therefore
//! rejects perfectly valid commits (it tripped roughly 1 fuzz run in 40) and was
//! removed again; see the note in `Pager::cacheflush`.

use super::tests_2::{empty_btree, run_until_done};
use super::*;
use crate::vdbe::Register;
use std::ops::Deref;

/// Grow a table b-tree until its root is an interior page, so that a cursor
/// positioned on a leaf has a parent to balance against.
fn btree_with_interior_root() -> (Rc<Pager>, usize, BTreeCursor) {
    let (pager, root_page) = empty_btree();
    let mut cursor = BTreeCursor::new_table(None, pager.clone(), root_page);
    let mut key = 1i64;
    loop {
        let value =
            ImmutableRecord::from_registers(&[Register::Value(Value::Blob(vec![0u8; 256]))]);
        run_until_done(
            || cursor.seek(SeekKey::TableRowId(key), SeekOp::GE { eq_only: true }),
            pager.deref(),
        )
        .expect("seek");
        run_until_done(
            || cursor.insert(&BTreeKey::new_table_rowid(key, Some(&value)), true),
            pager.deref(),
        )
        .expect("insert");
        let root_is_interior = matches!(
            pager
                .read_page(root_page)
                .expect("root page")
                .get_contents()
                .page_type(),
            PageType::TableInterior
        );
        if root_is_interior {
            break;
        }
        key += 1;
        assert!(key < 10_000, "root never became interior");
    }
    (pager, root_page, cursor)
}

/// A parent page holding pending overflow cells must produce a typed
/// `Corrupt` error, not a process abort.
#[test]
fn balance_non_root_reports_an_overflowed_parent_as_corrupt() {
    let (pager, root_page, mut cursor) = btree_with_interior_root();

    // Position on a leaf so the stack is [root, leaf], then pop to the parent:
    // this is exactly the stack shape `balance()` hands to `balance_non_root`.
    run_until_done(
        || cursor.seek(SeekKey::TableRowId(1), SeekOp::GE { eq_only: false }),
        pager.deref(),
    )
    .expect("seek to the first leaf");
    assert!(
        cursor.stack.has_parent(),
        "the cursor must be on a leaf below the interior root"
    );
    cursor.stack.pop();

    // Fabricate the unimplemented shape: a parent with a pending overflow cell.
    let parent = pager.read_page(root_page).expect("root page");
    parent.get_contents_mut().overflow_cells.push(OverflowCell {
        index: 0,
        payload: std::pin::Pin::new(vec![0u8, 1, 2, 3]),
    });

    let dirty_before = pager.dirty_page_count();
    cursor.state = CursorState::Write(WriteInfo {
        state: WriteState::BalanceNonRoot,
        balance_info: RefCell::new(None),
    });

    let err = cursor
        .balance_non_root()
        .expect_err("balancing a child of an overflowed parent must be an error, not a panic");
    let message = format!("{err}");
    assert!(
        matches!(err, LimboError::Corrupt(_)),
        "expected a typed Corrupt error, got: {message}"
    );
    assert!(
        message.contains("pending overflow cell"),
        "expected the overflowed-parent guard, got: {message}"
    );
    assert_eq!(
        pager.dirty_page_count(),
        dirty_before,
        "the guard must run before the parent is marked dirty, so a caught error \
         leaves no page queued for write-back by a balance that never happened"
    );
}
