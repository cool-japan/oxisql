//! Regression tests for overflow-page payload read/write and for
//! `CellArray`/`page_free_array` (the owned-snapshot cell removal path used
//! by `balance_non_root`).
//!
//! Split out of `tests.rs` (rather than appended into `tests_2`) to keep
//! that module's line count under the 2000-line policy ceiling; mirrors the
//! `mvcc_count`/`page_corruption` siblings, which reuse `tests_2`'s
//! `pub(super)` test helpers (`get_page`/`empty_btree`) instead of
//! duplicating setup. This file additionally reuses `rng_from_time_or_env`
//! and `run_until_done`.

use super::tests_2::{empty_btree, rng_from_time_or_env, run_until_done};
use super::*;
use crate::vdbe::Register;
use rand::Rng;
use std::ops::Deref;

#[test]
pub fn test_read_write_payload_with_overflow_page() {
    let (pager, root_page) = empty_btree();
    let mut cursor = BTreeCursor::new(None, pager.clone(), root_page, vec![]);
    let mut large_blob = vec![b'A'; 40960 - 11];
    let hello_world = b"hello world";
    large_blob.extend_from_slice(hello_world);
    let value =
        ImmutableRecord::from_registers(&[Register::Value(Value::Blob(large_blob.clone()))]);
    run_until_done(
        || {
            let key = SeekKey::TableRowId(1);
            cursor.seek(key, SeekOp::GE { eq_only: true })
        },
        pager.deref(),
    )
    .unwrap();
    run_until_done(
        || cursor.insert(&BTreeKey::new_table_rowid(1, Some(&value)), true),
        pager.deref(),
    )
    .unwrap();
    cursor
        .stack
        .set_cell_index(cursor.stack.current_cell_index() + 1);
    let offset_to_hello_world = 4 + (large_blob.len() - 11) as u32;
    let mut read_buffer = Vec::new();
    run_until_done(
        || {
            cursor.read_write_payload_with_offset(
                offset_to_hello_world,
                &mut read_buffer,
                11,
                false,
            )
        },
        pager.deref(),
    )
    .unwrap();
    assert_eq!(
        std::str::from_utf8(&read_buffer).unwrap(),
        "hello world",
        "Failed to read 'hello world' from overflow page"
    );
    let mut modified_hello = "olleh".as_bytes().to_vec();
    run_until_done(
        || {
            cursor.read_write_payload_with_offset(
                offset_to_hello_world,
                &mut modified_hello,
                5,
                true,
            )
        },
        pager.deref(),
    )
    .unwrap();
    let mut verification_buffer = Vec::new();
    run_until_done(
        || {
            cursor.read_write_payload_with_offset(
                offset_to_hello_world,
                &mut verification_buffer,
                hello_world.len() as u32,
                false,
            )
        },
        pager.deref(),
    )
    .unwrap();
    assert_eq!(
        std::str::from_utf8(&verification_buffer).unwrap(),
        "olleh world",
        "Modified data doesn't match expected result"
    );
}

#[test]
fn test_free_array() {
    let (mut rng, seed) = rng_from_time_or_env();
    tracing::info!("seed={}", seed);
    const ITERATIONS: usize = 10000;
    for _ in 0..ITERATIONS {
        let mut cell_array = CellArray {
            bufs: Vec::new(),
            cells: Vec::new(),
            number_of_cells_per_page: [0; 5],
        };
        let mut cells_cloned = Vec::new();
        let (pager, _) = empty_btree();
        let page_type = PageType::TableLeaf;
        let page = pager.allocate_page().unwrap();
        let page = Arc::new(BTreePageInner {
            page: RefCell::new(page),
        });
        btree_init_page(&page, page_type, 0, pager.usable_space() as u16);
        let page = page.get();
        let mut size = (rng.next_u64() % 100) as u16;
        let mut i = 0;
        while compute_free_space(page.get_contents_mut(), pager.usable_space() as u16).unwrap()
            >= size + 10
        {
            insert_cell(i, size, page.get_contents_mut(), pager.clone(), page_type);
            i += 1;
            size = (rng.next_u64() % 1024) as u16;
        }
        let contents = page.get_contents_mut();
        // Owned snapshot of the page, mirroring balance_non_root: each cell
        // is a `CellRef` into this snapshot, tagged with its origin slot so
        // page_free_array can reclaim the correct byte range.
        let page_offset = contents.offset;
        let snapshot_idx = cell_array.bufs.len() as u32;
        cell_array.bufs.push(Box::from(contents.as_slice()));
        for cell_idx in 0..contents.cell_count() {
            let (start, len) = contents
                .cell_get_raw_region(
                    cell_idx,
                    payload_overflow_threshold_max(contents.page_type(), 4096),
                    payload_overflow_threshold_min(contents.page_type(), 4096),
                    pager.usable_space(),
                )
                .unwrap();
            cell_array.cells.push(CellRef {
                buf: snapshot_idx,
                start: start as u32,
                len: len as u32,
                origin: Some(CellOrigin {
                    page_slot: 0,
                    page_offset: (start - page_offset) as u16,
                }),
            });
            cells_cloned.push(contents.as_slice()[start..start + len].to_vec());
        }
        debug_validate_cells!(contents, pager.usable_space() as u16);
        let cells_before_free = contents.cell_count();
        let size = rng.next_u64() as usize % cells_before_free;
        let prefix = rng.next_u64() % 2 == 0;
        let start = if prefix {
            0
        } else {
            contents.cell_count() - size
        };
        let removed = page_free_array(
            contents,
            0,
            start,
            size as usize,
            &cell_array,
            pager.usable_space() as u16,
        )
        .unwrap();
        if prefix {
            shift_cells_left(contents, cells_before_free, removed);
        }
        assert_eq!(removed, size);
        assert_eq!(contents.cell_count(), cells_before_free - size);
        #[cfg(debug_assertions)]
        debug_validate_cells_core(contents, pager.usable_space() as u16);
        let mut cell_idx_cloned = if prefix { size } else { 0 };
        for cell_idx in 0..contents.cell_count() {
            let buf = contents.as_ptr();
            let (start, len) = contents
                .cell_get_raw_region(
                    cell_idx,
                    payload_overflow_threshold_max(contents.page_type(), 4096),
                    payload_overflow_threshold_min(contents.page_type(), 4096),
                    pager.usable_space(),
                )
                .unwrap();
            let cell_in_page = &buf[start..start + len];
            let cell_in_array = &cells_cloned[cell_idx_cloned];
            assert_eq!(cell_in_page, cell_in_array);
            cell_idx_cloned += 1;
        }
    }
}

fn insert_cell(
    i: u64,
    size: u16,
    contents: &mut PageContent,
    pager: Rc<Pager>,
    page_type: PageType,
) {
    let mut payload = Vec::new();
    let record =
        ImmutableRecord::from_registers(&[Register::Value(Value::Blob(vec![0; size as usize]))]);
    fill_cell_payload(
        page_type,
        Some(i as i64),
        &mut payload,
        &record,
        pager.usable_space() as u16,
        pager.clone(),
    )
    .unwrap();
    insert_into_cell(contents, &payload, i as usize, pager.usable_space() as u16).unwrap();
}
