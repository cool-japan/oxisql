//! Regression tests for the on-disk-corruption detection sites in
//! `page_ops.rs`'s `defragment_page()` and `compute_free_space()`.
//!
//! These functions used to `unimplemented!`/`todo!` on the exact conditions
//! that indicate a corrupted page (a cell pointer or freeblock pointer that
//! points outside the page's valid range) instead of returning
//! `Err(LimboError::Corrupt(_))` like every neighboring check in this file
//! (see the repeated `return_corrupt!` uses in `free_cell_range` /
//! `find_free_cell` a few lines above). Each test below hand-constructs a
//! blank page and corrupts specific header/pointer bytes to trigger exactly
//! one condition, then asserts a clean `Err` comes back instead of a panic.

use super::tests_2::get_page;
use super::*;

/// `defragment_page` must reject a cell whose recorded pointer sits past the
/// last valid in-page offset (`usable_space - 4`) instead of panicking via
/// the old `unimplemented!("corrupted page")`.
#[test]
fn test_defragment_page_rejects_cell_pointer_past_last_valid_offset() {
    let usable_space: u16 = 4096;
    let page = get_page(2);
    let page_ref = page.get();
    let contents = page_ref.get_contents();

    // One "cell" registered, but its pointer is corrupted to sit past
    // `last_cell` (usable_space - 4 = 4092).
    contents.write_u16(offset::BTREE_CELL_COUNT, 1);
    let (cell_ptr_array_start, _) = contents.cell_pointer_array_offset_and_size();
    let corrupt_pc: u16 = usable_space - 3; // 4093 > last_cell (4092)
    contents.write_u16_no_offset(cell_ptr_array_start, corrupt_pc);

    // Make the bytes at the corrupted location decode as a small,
    // otherwise well-formed cell (len_payload=1, rowid=5, 1 payload byte)
    // so no unrelated sanity check (e.g. debug_validate_cells!) fires
    // before defragment_page's own bounds check gets a chance to run.
    let mut fake_cell = Vec::new();
    write_varint_to_vec(1, &mut fake_cell);
    write_varint_to_vec(5, &mut fake_cell);
    fake_cell.push(0xAA);
    assert_eq!(fake_cell.len(), 3, "sanity: expected a 3-byte encoded cell");
    assert!(
        corrupt_pc as usize + fake_cell.len() <= usable_space as usize,
        "sanity: fake cell must fit within the page"
    );
    let buf = contents.as_ptr();
    buf[corrupt_pc as usize..corrupt_pc as usize + fake_cell.len()].copy_from_slice(&fake_cell);

    let result = defragment_page(contents, usable_space);
    assert!(
        matches!(result, Err(LimboError::Corrupt(_))),
        "expected Corrupt error for out-of-range cell pointer, got {:?}",
        result
    );
}

/// `defragment_page` must reject a page whose cells collectively overflow
/// the available content area (driving the compaction cursor `cbrk` below
/// `first_cell`) instead of panicking via the old `todo!("corrupt")`.
#[test]
fn test_defragment_page_rejects_cells_overflowing_content_area() {
    let usable_space: u16 = 4096;
    let page = get_page(2);
    let page_ref = page.get();
    let contents = page_ref.get_contents();

    // Two cells registered; individually each fits comfortably within the
    // page (so the per-cell `pc > last_cell` check, and the debug-only
    // per-cell validation pass, both stay happy), but their combined sizes
    // are engineered to exceed the space actually available for content,
    // which is what should trip the `cbrk < first_cell` corruption check
    // as defragment_page tries to compact them from the end of the page
    // backward.
    contents.write_u16(offset::BTREE_CELL_COUNT, 2);
    let (cell_ptr_array_start, _) = contents.cell_pointer_array_offset_and_size();
    // first_cell = header_size(8) + cell_ptr_array_size(2 cells * 2) = 12.

    let pc0: u16 = 20;
    let pc1: u16 = 30;
    contents.write_u16_no_offset(cell_ptr_array_start, pc0);
    contents.write_u16_no_offset(cell_ptr_array_start + 2, pc1);

    let mut cell0 = Vec::new();
    write_varint_to_vec(1997, &mut cell0);
    write_varint_to_vec(5, &mut cell0);
    let size0 = 1997 + cell0.len() as u16;

    let mut cell1 = Vec::new();
    write_varint_to_vec(2087, &mut cell1);
    write_varint_to_vec(7, &mut cell1);
    let size1 = 2087 + cell1.len() as u16;

    assert_eq!(size0, 2000);
    assert_eq!(size1, 2090);
    let final_cbrk = usable_space - (size0 + size1);
    assert!(
        final_cbrk < 12,
        "sanity: combined cell sizes must drive cbrk below first_cell (12), got {}",
        final_cbrk
    );

    let buf = contents.as_ptr();
    buf[pc0 as usize..pc0 as usize + cell0.len()].copy_from_slice(&cell0);
    buf[pc1 as usize..pc1 as usize + cell1.len()].copy_from_slice(&cell1);

    let result = defragment_page(contents, usable_space);
    assert!(
        matches!(result, Err(LimboError::Corrupt(_))),
        "expected Corrupt error for cells overflowing the content area, got {:?}",
        result
    );
}

/// `compute_free_space` must reject a page whose first-freeblock pointer
/// sits before the cell content area instead of panicking via the old
/// `todo!("corrupted page")`.
#[test]
fn test_compute_free_space_rejects_freeblock_before_content_area() {
    let usable_space: u16 = 4096;
    let page = get_page(2);
    let page_ref = page.get();
    let contents = page_ref.get_contents();

    contents.write_u16(offset::BTREE_CELL_CONTENT_AREA, 100);
    contents.write_u16(offset::BTREE_FIRST_FREEBLOCK, 50); // 50 < 100 -> corrupt

    let result = compute_free_space(contents, usable_space);
    assert!(
        matches!(result, Err(LimboError::Corrupt(_))),
        "expected Corrupt error for freeblock before content area, got {:?}",
        result
    );
}

/// Deviation-note coverage: the sibling `assert!(next == 0, "corrupted page:
/// freeblocks list not in ascending order")` a few lines below the
/// `compute_free_space` fix above was, for consistency, also converted from
/// a live (non-debug) panic into a `return_corrupt!` -- it is the same
/// "corrupted freeblock chain" class of defect. This test drives that
/// specific path: a freeblock chain that terminates (per its own
/// `next <= cur_freeblock_ptr + size + 3` break condition) on a non-zero
/// `next`, i.e. a malformed/non-ascending list rather than a clean
/// zero-terminated one.
#[test]
fn test_compute_free_space_rejects_non_ascending_freeblock_chain() {
    let usable_space: u16 = 4096;
    let page = get_page(2);
    let page_ref = page.get();
    let contents = page_ref.get_contents();

    contents.write_u16(offset::BTREE_CELL_CONTENT_AREA, 100);
    contents.write_u16(offset::BTREE_FIRST_FREEBLOCK, 200); // >= content area, passes the other check
                                                            // Freeblock header at byte 200: next=150, size=10. `next` (150) is
                                                            // neither 0 nor past `cur_freeblock_ptr + size + 3` (213), so the scan
                                                            // loop breaks immediately with a non-zero `next` -- a corrupt chain.
    contents.write_u16_no_offset(200, 150);
    contents.write_u16_no_offset(202, 10);

    let result = compute_free_space(contents, usable_space);
    assert!(
        matches!(result, Err(LimboError::Corrupt(_))),
        "expected Corrupt error for non-ascending freeblock chain, got {:?}",
        result
    );
}

/// `cell_get_raw_region` must reject a 16-bit cell pointer that points past
/// the end of the page buffer instead of slicing out of bounds / unwrapping a
/// `read_varint` failure.
#[test]
fn test_cell_get_raw_region_rejects_out_of_range_cell_pointer() {
    let usable_space: u16 = 4096;
    let page = get_page(2);
    let page_ref = page.get();
    let contents = page_ref.get_contents();

    // One cell registered, but its pointer sits past the 4096-byte buffer.
    contents.write_u16(offset::BTREE_CELL_COUNT, 1);
    let (cell_ptr_array_start, _) = contents.cell_pointer_array_offset_and_size();
    contents.write_u16_no_offset(cell_ptr_array_start, 5000);

    let result = contents.cell_get_raw_region(
        0,
        payload_overflow_threshold_max(contents.page_type(), usable_space),
        payload_overflow_threshold_min(contents.page_type(), usable_space),
        usable_space as usize,
    );
    assert!(
        matches!(result, Err(LimboError::Corrupt(_))),
        "expected Corrupt error for out-of-range cell pointer, got {:?}",
        result
    );
}

/// The table-leaf rowid fast path must reject a corrupt cell pointer that
/// points past the page instead of slicing out of bounds.
#[test]
fn test_cell_table_leaf_read_rowid_rejects_out_of_range_pointer() {
    let page = get_page(2); // TableLeaf, 4096-byte buffer
    let page_ref = page.get();
    let contents = page_ref.get_contents();

    // Leaf cell-pointer array starts at header byte 8; write a bogus offset.
    contents.write_u16(8, 5000);

    let result = contents.cell_table_leaf_read_rowid(0);
    assert!(
        matches!(result, Err(LimboError::Corrupt(_))),
        "expected Corrupt error for out-of-range leaf cell pointer, got {:?}",
        result
    );
}

/// The table-interior rowid / left-child fast paths must reject a corrupt cell
/// pointer that points past the page instead of indexing out of bounds.
#[test]
fn test_cell_table_interior_fast_paths_reject_out_of_range_pointer() {
    let page = get_page(2);
    let page_ref = page.get();
    let contents = page_ref.get_contents();

    // Turn this into a table-interior page and register one corrupt cell.
    contents.write_u8(0, PageType::TableInterior as u8);
    // Interior cell-pointer array starts at header byte 12.
    contents.write_u16(12, 5000);

    let rowid = contents.cell_table_interior_read_rowid(0);
    assert!(
        matches!(rowid, Err(LimboError::Corrupt(_))),
        "expected Corrupt error for out-of-range interior rowid pointer, got {:?}",
        rowid
    );
    let left_child = contents.cell_table_interior_read_left_child_page(0);
    assert!(
        matches!(left_child, Err(LimboError::Corrupt(_))),
        "expected Corrupt error for out-of-range interior left-child pointer, got {:?}",
        left_child
    );
}

/// `cell_get` must reject a page whose type byte decodes to an invalid
/// discriminant (reached, e.g., by following a corrupt child/rightmost pointer
/// to a non-b-tree page) instead of panicking in the infallible `page_type()`.
#[test]
fn test_cell_get_rejects_invalid_page_type() {
    let usable_space: u16 = 4096;
    let page = get_page(2); // valid TableLeaf
    let page_ref = page.get();
    let contents = page_ref.get_contents();

    // Compute the overflow thresholds while the page type is still valid, then
    // corrupt byte 0 to an invalid page-type discriminant (0xFF) and register a
    // single (in-bounds) cell so we get past the idx bounds check.
    let max = payload_overflow_threshold_max(contents.page_type(), usable_space);
    let min = payload_overflow_threshold_min(contents.page_type(), usable_space);
    contents.write_u16(offset::BTREE_CELL_COUNT, 1);
    contents.write_u8(0, 0xFF);

    let result = contents.cell_get(0, max, min, usable_space as usize);
    assert!(
        matches!(result, Err(LimboError::Corrupt(_))),
        "expected Corrupt error for invalid page-type byte, got {:?}",
        result
    );
}

/// `cell_get` must reject a cell index at/beyond the page's cell count (which
/// can happen when the untrusted cell count is corrupt) with a typed error
/// rather than the old `assert!(idx < ncells)` panic.
#[test]
fn test_cell_get_rejects_idx_out_of_bounds() {
    let usable_space: u16 = 4096;
    let page = get_page(2);
    let page_ref = page.get();
    let contents = page_ref.get_contents();

    // Freshly initialized page has zero cells; asking for cell 0 is out of
    // bounds and must come back as Corrupt instead of panicking.
    let max = payload_overflow_threshold_max(contents.page_type(), usable_space);
    let min = payload_overflow_threshold_min(contents.page_type(), usable_space);
    assert_eq!(contents.cell_count(), 0, "sanity: expected an empty page");

    let result = contents.cell_get(0, max, min, usable_space as usize);
    assert!(
        matches!(result, Err(LimboError::Corrupt(_))),
        "expected Corrupt error for out-of-bounds cell index, got {:?}",
        result
    );
}

/// `free_cell_range` must reject an overlapping freeblock (the freed range's
/// end exceeds the next freeblock pointer `pc`) via its own bounds check
/// *before* computing `(pc - end) as u8`, which would otherwise underflow and
/// panic under debug overflow checks.
#[test]
fn test_free_cell_range_rejects_overlapping_freeblock() {
    let usable_space: u16 = 4096;
    let page = get_page(2);
    let page_ref = page.get();
    let contents = page_ref.get_contents_mut();

    // First freeblock at offset 110, which lies *inside* the range we are
    // about to free ([100, 120)), so `end (120) > pc (110)`.
    contents.write_u16(offset::BTREE_FIRST_FREEBLOCK, 110);

    let result = free_cell_range(contents, 100, 20, usable_space);
    assert!(
        matches!(result, Err(LimboError::Corrupt(_))),
        "expected Corrupt error for an overlapping freeblock, got {:?}",
        result
    );
}
