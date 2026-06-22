//! Auto-generated test module (consolidated from inline `#[cfg(test)] mod` blocks)

use crate::storage::sqlite3_ondisk::{TableInteriorCell, TableLeafCell};
use crate::{
    types::{CursorResult, ImmutableRecord, SeekKey, SeekOp},
    Result,
};

use super::*;

#[cfg(test)]
mod tests_2 {
    use super::*;
    use super::{btree_init_page, defragment_page, drop_cell, insert_into_cell};
    #[cfg(feature = "index_experimental")]
    use crate::storage::pager::CreateBTreeFlags;
    use crate::{
        fast_lock::SpinLock,
        io::{Buffer, Completion, MemoryIO, OpenFlags, IO},
        storage::{
            database::DatabaseFile,
            page_cache::DumbLruPageCache,
            sqlite3_ondisk::{self, DatabaseHeader},
        },
        types::Text,
        vdbe::Register,
        BufferPool, Connection, DatabaseStorage, StepResult, WalFile, WalFileShared,
        WriteCompletion,
    };
    use crate::{
        io::BufferData,
        storage::{
            btree::{
                compute_free_space, fill_cell_payload, payload_overflow_threshold_max,
                payload_overflow_threshold_min,
            },
            sqlite3_ondisk::{BTreeCell, PageContent, PageType},
        },
        types::Value,
        Database, Page, Pager, PlatformIO,
    };
    use rand::{rng as thread_rng, Rng};
    use rand_chacha::{rand_core::SeedableRng, ChaCha8Rng};
    use sorted_vec::SortedVec;
    use std::{
        cell::RefCell, collections::HashSet, mem::transmute, ops::Deref, panic, rc::Rc, sync::Arc,
    };
    use tempfile::TempDir;
    use test_log::test;
    #[allow(clippy::arc_with_non_send_sync)]
    fn get_page(id: usize) -> BTreePage {
        let page = Arc::new(Page::new(id));
        let drop_fn = Rc::new(|_| {});
        let inner = PageContent::new(
            0,
            Arc::new(RefCell::new(Buffer::new(
                BufferData::new(vec![0; 4096]),
                drop_fn,
            ))),
        );
        page.get().contents.replace(inner);
        let page = Arc::new(BTreePageInner {
            page: RefCell::new(page),
        });
        btree_init_page(&page, PageType::TableLeaf, 0, 4096);
        page
    }
    #[allow(clippy::arc_with_non_send_sync)]
    fn get_database() -> Arc<Database> {
        let mut path = TempDir::new().unwrap().keep();
        path.push("test.db");
        let io: Arc<dyn IO> = Arc::new(PlatformIO::new().unwrap());
        let db = Database::open_file(io.clone(), path.to_str().unwrap(), false).unwrap();
        {
            let conn = db.connect().unwrap();
            conn.pragma_update("journal_mode", "wal").unwrap();
        }
        db
    }
    fn ensure_cell(page: &mut PageContent, cell_idx: usize, payload: &Vec<u8>) {
        let cell = page.cell_get_raw_region(
            cell_idx,
            payload_overflow_threshold_max(page.page_type(), 4096),
            payload_overflow_threshold_min(page.page_type(), 4096),
            4096,
        );
        tracing::trace!("cell idx={} start={} len={}", cell_idx, cell.0, cell.1);
        let buf = &page.as_ptr()[cell.0..cell.0 + cell.1];
        assert_eq!(buf.len(), payload.len());
        assert_eq!(buf, payload);
    }
    fn add_record(
        id: usize,
        pos: usize,
        page: &mut PageContent,
        record: ImmutableRecord,
        conn: &Arc<Connection>,
    ) -> Vec<u8> {
        let mut payload: Vec<u8> = Vec::new();
        fill_cell_payload(
            page.page_type(),
            Some(id as i64),
            &mut payload,
            &record,
            4096,
            conn.pager.clone(),
        );
        insert_into_cell(page, &payload, pos, 4096).unwrap();
        payload
    }
    #[test]
    fn test_insert_cell() {
        let db = get_database();
        let conn = db.connect().unwrap();
        let page = get_page(2);
        let page = page.get();
        let page = page.get_contents();
        let header_size = 8;
        let record = ImmutableRecord::from_registers(&[Register::Value(Value::Integer(1))]);
        let payload = add_record(1, 0, page, record, &conn);
        assert_eq!(page.cell_count(), 1);
        let free = compute_free_space(page, 4096);
        assert_eq!(free, 4096 - payload.len() as u16 - 2 - header_size);
        let cell_idx = 0;
        ensure_cell(page, cell_idx, &payload);
    }
    struct Cell {
        pos: usize,
        payload: Vec<u8>,
    }
    #[test]
    fn test_drop_1() {
        let db = get_database();
        let conn = db.connect().unwrap();
        let page = get_page(2);
        let page = page.get();
        let page = page.get_contents();
        let header_size = 8;
        let mut total_size = 0;
        let mut cells = Vec::new();
        let usable_space = 4096;
        for i in 0..3 {
            let record =
                ImmutableRecord::from_registers(&[Register::Value(Value::Integer(i as i64))]);
            let payload = add_record(i, i, page, record, &conn);
            assert_eq!(page.cell_count(), i + 1);
            let free = compute_free_space(page, usable_space);
            total_size += payload.len() as u16 + 2;
            assert_eq!(free, 4096 - total_size - header_size);
            cells.push(Cell { pos: i, payload });
        }
        for (i, cell) in cells.iter().enumerate() {
            ensure_cell(page, i, &cell.payload);
        }
        cells.remove(1);
        drop_cell(page, 1, usable_space).unwrap();
        for (i, cell) in cells.iter().enumerate() {
            ensure_cell(page, i, &cell.payload);
        }
    }
    fn validate_btree(pager: Rc<Pager>, page_idx: usize) -> (usize, bool) {
        let cursor = BTreeCursor::new_table(None, pager.clone(), page_idx);
        let page = cursor.read_page(page_idx).unwrap();
        while page.get().is_locked() {
            pager.io.run_once().unwrap();
        }
        let page = page.get();
        page.set_dirty();
        let contents = page.get().contents.as_ref().unwrap();
        let page_type = contents.page_type();
        let mut previous_key = None;
        let mut valid = true;
        let mut depth = None;
        debug_validate_cells!(contents, pager.usable_space() as u16);
        let mut child_pages = Vec::new();
        for cell_idx in 0..contents.cell_count() {
            let cell = contents
                .cell_get(
                    cell_idx,
                    payload_overflow_threshold_max(page_type, 4096),
                    payload_overflow_threshold_min(page_type, 4096),
                    cursor.usable_space(),
                )
                .unwrap();
            let current_depth = match cell {
                BTreeCell::TableLeafCell(..) => 1,
                BTreeCell::TableInteriorCell(TableInteriorCell {
                    _left_child_page, ..
                }) => {
                    let child_page = cursor.read_page(_left_child_page as usize).unwrap();
                    while child_page.get().is_locked() {
                        pager.io.run_once().unwrap();
                    }
                    child_pages.push(child_page);
                    if _left_child_page == page.get().id as u32 {
                        valid = false;
                        tracing::error!(
                            "left child page is the same as parent {}",
                            _left_child_page
                        );
                        continue;
                    }
                    let (child_depth, child_valid) =
                        validate_btree(pager.clone(), _left_child_page as usize);
                    valid &= child_valid;
                    child_depth
                }
                _ => panic!("unsupported btree cell: {:?}", cell),
            };
            if current_depth >= 100 {
                tracing::error!("depth is too big");
                page.clear_dirty();
                return (100, false);
            }
            depth = Some(depth.unwrap_or(current_depth + 1));
            if depth != Some(current_depth + 1) {
                tracing::error!("depth is different for child of page {}", page_idx);
                valid = false;
            }
            match cell {
                BTreeCell::TableInteriorCell(TableInteriorCell { _rowid, .. })
                | BTreeCell::TableLeafCell(TableLeafCell { _rowid, .. }) => {
                    if previous_key.is_some() && previous_key.unwrap() >= _rowid {
                        tracing::error!(
                            "keys are in bad order: prev={:?}, current={}",
                            previous_key,
                            _rowid
                        );
                        valid = false;
                    }
                    previous_key = Some(_rowid);
                }
                _ => panic!("unsupported btree cell: {:?}", cell),
            }
        }
        if let Some(right) = contents.rightmost_pointer() {
            let (right_depth, right_valid) = validate_btree(pager.clone(), right as usize);
            valid &= right_valid;
            depth = Some(depth.unwrap_or(right_depth + 1));
            if depth != Some(right_depth + 1) {
                tracing::error!("depth is different for child of page {}", page_idx);
                valid = false;
            }
        }
        let first_page_type = child_pages.first().map(|p| {
            if !p.get().is_loaded() {
                let new_page = pager.read_page(p.get().get().id).unwrap();
                p.page.replace(new_page);
            }
            while p.get().is_locked() {
                pager.io.run_once().unwrap();
            }
            p.get().get_contents().page_type()
        });
        if let Some(child_type) = first_page_type {
            for page in child_pages.iter().skip(1) {
                if !page.get().is_loaded() {
                    let new_page = pager.read_page(page.get().get().id).unwrap();
                    page.page.replace(new_page);
                }
                while page.get().is_locked() {
                    pager.io.run_once().unwrap();
                }
                if page.get().get_contents().page_type() != child_type {
                    tracing::error!("child pages have different types");
                    valid = false;
                }
            }
        }
        if contents.rightmost_pointer().is_none() && contents.cell_count() == 0 {
            valid = false;
        }
        page.clear_dirty();
        (depth.unwrap(), valid)
    }
    fn format_btree(pager: Rc<Pager>, page_idx: usize, depth: usize) -> String {
        let cursor = BTreeCursor::new_table(None, pager.clone(), page_idx);
        let page = cursor.read_page(page_idx).unwrap();
        while page.get().is_locked() {
            pager.io.run_once().unwrap();
        }
        let page = page.get();
        page.set_dirty();
        let contents = page.get().contents.as_ref().unwrap();
        let page_type = contents.page_type();
        let mut current = Vec::new();
        let mut child = Vec::new();
        for cell_idx in 0..contents.cell_count() {
            let cell = contents
                .cell_get(
                    cell_idx,
                    payload_overflow_threshold_max(page_type, 4096),
                    payload_overflow_threshold_min(page_type, 4096),
                    cursor.usable_space(),
                )
                .unwrap();
            match cell {
                BTreeCell::TableInteriorCell(cell) => {
                    current.push(format!(
                        "node[rowid:{}, ptr(<=):{}]",
                        cell._rowid, cell._left_child_page
                    ));
                    child.push(format_btree(
                        pager.clone(),
                        cell._left_child_page as usize,
                        depth + 2,
                    ));
                }
                BTreeCell::TableLeafCell(cell) => {
                    current.push(format!(
                        "leaf[rowid:{}, len(payload):{}, overflow:{}]",
                        cell._rowid,
                        cell._payload.len(),
                        cell.first_overflow_page.is_some()
                    ));
                }
                _ => panic!("unsupported btree cell: {:?}", cell),
            }
        }
        if let Some(rightmost) = contents.rightmost_pointer() {
            child.push(format_btree(pager.clone(), rightmost as usize, depth + 2));
        }
        let current = format!(
            "{}-page:{}, ptr(right):{}\n{}+cells:{}",
            " ".repeat(depth),
            page_idx,
            contents.rightmost_pointer().unwrap_or(0),
            " ".repeat(depth),
            current.join(", ")
        );
        page.clear_dirty();
        if child.is_empty() {
            current
        } else {
            current + "\n" + &child.join("\n")
        }
    }
    fn empty_btree() -> (Rc<Pager>, usize) {
        let db_header = DatabaseHeader::default();
        let page_size = db_header.get_page_size();
        #[allow(clippy::arc_with_non_send_sync)]
        let io: Arc<dyn IO> = Arc::new(MemoryIO::new());
        let io_file = io.open_file("test.db", OpenFlags::Create, false).unwrap();
        let db_file = Arc::new(DatabaseFile::new(io_file));
        let buffer_pool = Rc::new(BufferPool::new(page_size as usize));
        let wal_shared = WalFileShared::open_shared(&io, "test.wal", page_size).unwrap();
        let wal_file = WalFile::new(io.clone(), page_size, wal_shared, buffer_pool.clone());
        let wal = Rc::new(RefCell::new(wal_file));
        let page_cache = Arc::new(parking_lot::RwLock::new(DumbLruPageCache::new(2000)));
        let pager = {
            let db_header = Arc::new(SpinLock::new(db_header.clone()));
            Pager::finish_open(db_header, db_file, wal, io, page_cache, buffer_pool).unwrap()
        };
        let pager = Rc::new(pager);
        let page1 = pager.allocate_page().unwrap();
        let page1 = Arc::new(BTreePageInner {
            page: RefCell::new(page1),
        });
        btree_init_page(&page1, PageType::TableLeaf, 0, 4096);
        (pager, page1.get().get().id)
    }
    #[test]
    #[ignore]
    pub fn btree_insert_fuzz_ex() {
        for sequence in [
            &[
                (777548915, 3364),
                (639157228, 3796),
                (709175417, 1214),
                (390824637, 210),
                (906124785, 1481),
                (197677875, 1305),
                (457946262, 3734),
                (956825466, 592),
                (835875722, 1334),
                (649214013, 1250),
                (531143011, 1788),
                (765057993, 2351),
                (510007766, 1349),
                (884516059, 822),
                (81604840, 2545),
            ]
            .as_slice(),
            &[
                (293471650, 2452),
                (163608869, 627),
                (544576229, 464),
                (705823748, 3441),
            ]
            .as_slice(),
            &[
                (987283511, 2924),
                (261851260, 1766),
                (343847101, 1657),
                (315844794, 572),
            ]
            .as_slice(),
            &[
                (987283511, 2924),
                (261851260, 1766),
                (343847101, 1657),
                (315844794, 572),
                (649272840, 1632),
                (723398505, 3140),
                (334416967, 3874),
            ]
            .as_slice(),
        ] {
            let (pager, root_page) = empty_btree();
            let mut cursor = BTreeCursor::new_table(None, pager.clone(), root_page);
            for (key, size) in sequence.iter() {
                run_until_done(
                    || {
                        let key = SeekKey::TableRowId(*key);
                        cursor.seek(key, SeekOp::GE { eq_only: true })
                    },
                    pager.deref(),
                )
                .unwrap();
                let value = ImmutableRecord::from_registers(&[Register::Value(Value::Blob(vec![
                        0;
                        *size
                    ]))]);
                tracing::info!("insert key:{}", key);
                run_until_done(
                    || cursor.insert(&BTreeKey::new_table_rowid(*key, Some(&value)), true),
                    pager.deref(),
                )
                .unwrap();
                tracing::info!(
                    "=========== btree ===========\n{}\n\n",
                    format_btree(pager.clone(), root_page, 0)
                );
            }
            for (key, _) in sequence.iter() {
                let seek_key = SeekKey::TableRowId(*key);
                assert!(
                    matches!(
                        cursor.seek(seek_key, SeekOp::GE { eq_only: true }).unwrap(),
                        CursorResult::Ok(true)
                    ),
                    "key {} is not found",
                    key
                );
            }
        }
    }
    fn rng_from_time_or_env() -> (ChaCha8Rng, u64) {
        let seed = std::env::var("SEED").map_or(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis(),
            |v| {
                v.parse()
                    .expect("Failed to parse SEED environment variable as u64")
            },
        );
        let rng = ChaCha8Rng::seed_from_u64(seed as u64);
        (rng, seed as u64)
    }
    fn btree_insert_fuzz_run(
        attempts: usize,
        inserts: usize,
        size: impl Fn(&mut ChaCha8Rng) -> usize,
    ) {
        const VALIDATE_INTERVAL: usize = 1000;
        let do_validate_btree = std::env::var("VALIDATE_BTREE")
            .map_or(false, |v| v.parse().expect("validate should be bool"));
        let (mut rng, seed) = rng_from_time_or_env();
        let mut seen = HashSet::new();
        tracing::info!("super seed: {}", seed);
        for _ in 0..attempts {
            let (pager, root_page) = empty_btree();
            let mut cursor = BTreeCursor::new_table(None, pager.clone(), root_page);
            let mut keys = SortedVec::new();
            tracing::info!("seed: {}", seed);
            for insert_id in 0..inserts {
                let do_validate = do_validate_btree || (insert_id % VALIDATE_INTERVAL == 0);
                pager.begin_read_tx().unwrap();
                pager.begin_write_tx().unwrap();
                let size = size(&mut rng);
                let key = {
                    let result;
                    loop {
                        let key = (rng.next_u64() % (1 << 30)) as i64;
                        if seen.contains(&key) {
                            continue;
                        } else {
                            seen.insert(key);
                        }
                        result = key;
                        break;
                    }
                    result
                };
                keys.push(key);
                tracing::info!(
                    "INSERT INTO t VALUES ({}, randomblob({})); -- {}",
                    key,
                    size,
                    insert_id
                );
                run_until_done(
                    || {
                        let key = SeekKey::TableRowId(key);
                        cursor.seek(key, SeekOp::GE { eq_only: true })
                    },
                    pager.deref(),
                )
                .unwrap();
                let value =
                    ImmutableRecord::from_registers(&[Register::Value(Value::Blob(vec![0; size]))]);
                let btree_before = if do_validate {
                    format_btree(pager.clone(), root_page, 0)
                } else {
                    "".to_string()
                };
                run_until_done(
                    || cursor.insert(&BTreeKey::new_table_rowid(key, Some(&value)), true),
                    pager.deref(),
                )
                .unwrap();
                loop {
                    match pager.end_tx().unwrap() {
                        crate::PagerCacheflushStatus::Done(_) => break,
                        crate::PagerCacheflushStatus::IO => {
                            pager.io.run_once().unwrap();
                        }
                    }
                }
                pager.begin_read_tx().unwrap();
                cursor.move_to_root();
                let mut valid = true;
                if do_validate {
                    cursor.move_to_root();
                    for key in keys.iter() {
                        tracing::trace!("seeking key: {}", key);
                        run_until_done(|| cursor.next(), pager.deref()).unwrap();
                        let cursor_rowid = run_until_done(|| cursor.rowid(), pager.deref())
                            .unwrap()
                            .unwrap();
                        if *key != cursor_rowid {
                            valid = false;
                            println!("key {} is not found, got {}", key, cursor_rowid);
                            break;
                        }
                    }
                }
                if do_validate
                    && (!valid || matches!(validate_btree(pager.clone(), root_page), (_, false)))
                {
                    let btree_after = format_btree(pager.clone(), root_page, 0);
                    println!("btree before:\n{}", btree_before);
                    println!("btree after:\n{}", btree_after);
                    panic!("invalid btree");
                }
                pager.end_read_tx().unwrap();
            }
            pager.begin_read_tx().unwrap();
            tracing::info!(
                "=========== btree ===========\n{}\n\n",
                format_btree(pager.clone(), root_page, 0)
            );
            if matches!(validate_btree(pager.clone(), root_page), (_, false)) {
                panic!("invalid btree");
            }
            cursor.move_to_root();
            for key in keys.iter() {
                tracing::trace!("seeking key: {}", key);
                run_until_done(|| cursor.next(), pager.deref()).unwrap();
                let cursor_rowid = run_until_done(|| cursor.rowid(), pager.deref())
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    *key, cursor_rowid,
                    "key {} is not found, got {}",
                    key, cursor_rowid
                );
            }
            pager.end_read_tx().unwrap();
        }
    }
    #[cfg(feature = "index_experimental")]
    fn btree_index_insert_fuzz_run(attempts: usize, inserts: usize) {
        let (mut rng, seed) = if std::env::var("SEED").is_ok() {
            let seed = std::env::var("SEED").unwrap();
            let seed = seed.parse::<u64>().unwrap();
            let rng = ChaCha8Rng::seed_from_u64(seed);
            (rng, seed)
        } else {
            rng_from_time_or_env()
        };
        let mut seen = HashSet::new();
        tracing::info!("super seed: {}", seed);
        for _ in 0..attempts {
            let (pager, _) = empty_btree();
            let index_root_page_result =
                pager.btree_create(&CreateBTreeFlags::new_index()).unwrap();
            let index_root_page = match index_root_page_result {
                crate::types::CursorResult::Ok(id) => id as usize,
                crate::types::CursorResult::IO => {
                    panic!("btree_create returned IO in test, unexpected")
                }
            };
            let mut cursor = BTreeCursor::new_table(None, pager.clone(), index_root_page);
            let mut keys = SortedVec::new();
            tracing::info!("seed: {}", seed);
            for i in 0..inserts {
                pager.begin_read_tx().unwrap();
                pager.begin_write_tx().unwrap();
                let key = {
                    let result;
                    loop {
                        let cols = (0..10)
                            .map(|_| (rng.next_u64() % (1 << 30)) as i64)
                            .collect::<Vec<_>>();
                        if seen.contains(&cols) {
                            continue;
                        } else {
                            seen.insert(cols.clone());
                        }
                        result = cols;
                        break;
                    }
                    result
                };
                tracing::info!("insert {}/{}: {:?}", i + 1, inserts, key);
                keys.push(key.clone());
                let value = ImmutableRecord::from_registers(
                    &key.iter()
                        .map(|col| Register::Value(Value::Integer(*col)))
                        .collect::<Vec<_>>(),
                );
                run_until_done(
                    || {
                        cursor.insert(
                            &BTreeKey::new_index_key(&value),
                            cursor.is_write_in_progress(),
                        )
                    },
                    pager.deref(),
                )
                .unwrap();
                cursor.move_to_root();
                loop {
                    match pager.end_tx().unwrap() {
                        crate::PagerCacheflushStatus::Done(_) => break,
                        crate::PagerCacheflushStatus::IO => {
                            pager.io.run_once().unwrap();
                        }
                    }
                }
            }
            pager.begin_read_tx().unwrap();
            cursor.move_to_root();
            for (i, key) in keys.iter().enumerate() {
                tracing::info!("seeking key {}/{}: {:?}", i + 1, keys.len(), key);
                let exists = run_until_done(
                    || {
                        cursor.seek(
                            SeekKey::IndexKey(&ImmutableRecord::from_registers(
                                &key.iter()
                                    .map(|col| Register::Value(Value::Integer(*col)))
                                    .collect::<Vec<_>>(),
                            )),
                            SeekOp::GE { eq_only: true },
                        )
                    },
                    pager.deref(),
                )
                .unwrap();
                assert!(exists, "key {:?} is not found", key);
            }
            cursor.move_to_root();
            let mut count = 0;
            while run_until_done(|| cursor.next(), pager.deref()).unwrap() {
                count += 1;
            }
            assert_eq!(
                count,
                keys.len(),
                "key count is not right, got {}, expected {}",
                count,
                keys.len()
            );
            cursor.move_to_root();
            let mut prev = None;
            for (i, key) in keys.iter().enumerate() {
                tracing::info!("iterating key {}/{}: {:?}", i + 1, keys.len(), key);
                run_until_done(|| cursor.next(), pager.deref()).unwrap();
                let record = run_until_done(|| cursor.record(), &pager).unwrap();
                let record = record.as_ref().unwrap();
                let cur = record.get_values().clone();
                if let Some(prev) = prev {
                    if prev >= cur {
                        println!("Seed: {}", seed);
                    }
                    assert!(
                        prev < cur,
                        "keys are not in ascending order: {:?} < {:?}",
                        prev,
                        cur
                    );
                }
                prev = Some(cur);
            }
            pager.end_read_tx().unwrap();
        }
    }
    #[test]
    pub fn test_drop_odd() {
        let db = get_database();
        let conn = db.connect().unwrap();
        let page = get_page(2);
        let page = page.get();
        let page = page.get_contents();
        let header_size = 8;
        let mut total_size = 0;
        let mut cells = Vec::new();
        let usable_space = 4096;
        let total_cells = 10;
        for i in 0..total_cells {
            let record =
                ImmutableRecord::from_registers(&[Register::Value(Value::Integer(i as i64))]);
            let payload = add_record(i, i, page, record, &conn);
            assert_eq!(page.cell_count(), i + 1);
            let free = compute_free_space(page, usable_space);
            total_size += payload.len() as u16 + 2;
            assert_eq!(free, 4096 - total_size - header_size);
            cells.push(Cell { pos: i, payload });
        }
        let mut removed = 0;
        let mut new_cells = Vec::new();
        for cell in cells {
            if cell.pos % 2 == 1 {
                drop_cell(page, cell.pos - removed, usable_space).unwrap();
                removed += 1;
            } else {
                new_cells.push(cell);
            }
        }
        let cells = new_cells;
        for (i, cell) in cells.iter().enumerate() {
            ensure_cell(page, i, &cell.payload);
        }
        for (i, cell) in cells.iter().enumerate() {
            ensure_cell(page, i, &cell.payload);
        }
    }
    #[test]
    pub fn btree_insert_fuzz_run_equal_size() {
        for size in 1..8 {
            tracing::info!("======= size:{} =======", size);
            btree_insert_fuzz_run(2, 1024, |_| size);
        }
    }
    #[test]
    #[cfg(feature = "index_experimental")]
    pub fn btree_index_insert_fuzz_run_equal_size() {
        btree_index_insert_fuzz_run(2, 1024);
    }
    #[test]
    pub fn btree_insert_fuzz_run_random() {
        btree_insert_fuzz_run(128, 16, |rng| (rng.next_u32() % 4096) as usize);
    }
    #[test]
    pub fn btree_insert_fuzz_run_small() {
        btree_insert_fuzz_run(1, 100, |rng| (rng.next_u32() % 128) as usize);
    }
    #[test]
    pub fn btree_insert_fuzz_run_big() {
        btree_insert_fuzz_run(64, 32, |rng| 3 * 1024 + (rng.next_u32() % 1024) as usize);
    }
    #[test]
    pub fn btree_insert_fuzz_run_overflow() {
        btree_insert_fuzz_run(64, 32, |rng| (rng.next_u32() % 32 * 1024) as usize);
    }
    #[test]
    #[ignore]
    pub fn fuzz_long_btree_insert_fuzz_run_equal_size() {
        for size in 1..8 {
            tracing::info!("======= size:{} =======", size);
            btree_insert_fuzz_run(2, 10_000, |_| size);
        }
    }
    #[test]
    #[ignore]
    #[cfg(feature = "index_experimental")]
    pub fn fuzz_long_btree_index_insert_fuzz_run_equal_size() {
        btree_index_insert_fuzz_run(2, 10_000);
    }
    #[test]
    #[ignore]
    pub fn fuzz_long_btree_insert_fuzz_run_random() {
        btree_insert_fuzz_run(2, 10_000, |rng| (rng.next_u32() % 4096) as usize);
    }
    #[test]
    #[ignore]
    pub fn fuzz_long_btree_insert_fuzz_run_small() {
        btree_insert_fuzz_run(2, 10_000, |rng| (rng.next_u32() % 128) as usize);
    }
    #[test]
    #[ignore]
    pub fn fuzz_long_btree_insert_fuzz_run_big() {
        btree_insert_fuzz_run(2, 10_000, |rng| 3 * 1024 + (rng.next_u32() % 1024) as usize);
    }
    #[test]
    #[ignore]
    pub fn fuzz_long_btree_insert_fuzz_run_overflow() {
        btree_insert_fuzz_run(2, 5_000, |rng| (rng.next_u32() % 32 * 1024) as usize);
    }
    #[allow(clippy::arc_with_non_send_sync)]
    fn setup_test_env(database_size: u32) -> (Rc<Pager>, Arc<SpinLock<DatabaseHeader>>) {
        let page_size = 512;
        let mut db_header = DatabaseHeader::default();
        db_header.update_page_size(page_size);
        db_header.database_size = database_size;
        let db_header = Arc::new(SpinLock::new(db_header));
        let buffer_pool = Rc::new(BufferPool::new(10));
        for _ in 0..10 {
            let vec = vec![0; page_size as usize];
            buffer_pool.put(Pin::new(vec));
        }
        let io: Arc<dyn IO> = Arc::new(MemoryIO::new());
        let db_file = Arc::new(DatabaseFile::new(
            io.open_file("test.db", OpenFlags::Create, false).unwrap(),
        ));
        let drop_fn = Rc::new(|_buf| {});
        let buf = Arc::new(RefCell::new(Buffer::allocate(page_size as usize, drop_fn)));
        {
            let mut buf_mut = buf.borrow_mut();
            let buf_slice = buf_mut.as_mut_slice();
            sqlite3_ondisk::write_header_to_buf(buf_slice, &db_header.lock());
        }
        let write_complete = Box::new(|_| {});
        let c = Completion::Write(WriteCompletion::new(write_complete));
        db_file.write_page(1, buf.clone(), Arc::new(c)).unwrap();
        let wal_shared = WalFileShared::open_shared(&io, "test.wal", page_size).unwrap();
        let wal = Rc::new(RefCell::new(WalFile::new(
            io.clone(),
            page_size,
            wal_shared,
            buffer_pool.clone(),
        )));
        let pager = Rc::new(
            Pager::finish_open(
                db_header.clone(),
                db_file,
                wal,
                io,
                Arc::new(parking_lot::RwLock::new(DumbLruPageCache::new(10))),
                buffer_pool,
            )
            .unwrap(),
        );
        pager.io.run_once().unwrap();
        (pager, db_header)
    }
    #[test]
    #[ignore]
    pub fn test_clear_overflow_pages() -> Result<()> {
        let (pager, db_header) = setup_test_env(5);
        let mut cursor = BTreeCursor::new_table(None, pager.clone(), 1);
        let max_local = payload_overflow_threshold_max(PageType::TableLeaf, 4096);
        let usable_size = cursor.usable_space();
        let large_payload = vec![b'A'; max_local + usable_size];
        let mut current_page = 2u32;
        while current_page <= 4 {
            let drop_fn = Rc::new(|_buf| {});
            #[allow(clippy::arc_with_non_send_sync)]
            let buf = Arc::new(RefCell::new(Buffer::allocate(
                db_header.lock().get_page_size() as usize,
                drop_fn,
            )));
            let write_complete = Box::new(|_| {});
            let c = Completion::Write(WriteCompletion::new(write_complete));
            #[allow(clippy::arc_with_non_send_sync)]
            pager
                .db_file
                .write_page(current_page as usize, buf.clone(), Arc::new(c))?;
            pager.io.run_once()?;
            let page = cursor.read_page(current_page as usize)?;
            while page.get().is_locked() {
                cursor.pager.io.run_once()?;
            }
            {
                let page = page.get();
                let contents = page.get_contents();
                let next_page = if current_page < 4 {
                    current_page + 1
                } else {
                    0
                };
                contents.write_u32(0, next_page);
                let buf = contents.as_ptr();
                buf[4..].fill(b'A');
            }
            current_page += 1;
        }
        pager.io.run_once()?;
        let leaf_cell = BTreeCell::TableLeafCell(TableLeafCell {
            _rowid: 1,
            _payload: unsafe { transmute::<&[u8], &'static [u8]>(large_payload.as_slice()) },
            first_overflow_page: Some(2),
            payload_size: large_payload.len() as u64,
        });
        let initial_freelist_pages = db_header.lock().freelist_pages;
        let clear_result = cursor.clear_overflow_pages(&leaf_cell)?;
        match clear_result {
            CursorResult::Ok(_) => {
                assert_eq!(
                    db_header.lock().freelist_pages,
                    initial_freelist_pages + 3,
                    "Expected 3 pages to be added to freelist"
                );
                let trunk_page_id = db_header.lock().freelist_trunk_page;
                if trunk_page_id > 0 {
                    let trunk_page = cursor.read_page(trunk_page_id as usize)?;
                    if let Some(contents) = trunk_page.get().get().contents.as_ref() {
                        let n_leaf = contents.read_u32(4);
                        assert!(n_leaf > 0, "Trunk page should have leaf entries");
                        for i in 0..n_leaf {
                            let leaf_page_id = contents.read_u32(8 + (i as usize * 4));
                            assert!(
                                (2..=4).contains(&leaf_page_id),
                                "Leaf page ID {} should be in range 2-4",
                                leaf_page_id
                            );
                        }
                    }
                }
            }
            CursorResult::IO => {
                cursor.pager.io.run_once()?;
            }
        }
        Ok(())
    }
    #[test]
    pub fn test_clear_overflow_pages_no_overflow() -> Result<()> {
        let (pager, db_header) = setup_test_env(5);
        let mut cursor = BTreeCursor::new_table(None, pager.clone(), 1);
        let small_payload = vec![b'A'; 10];
        let leaf_cell = BTreeCell::TableLeafCell(TableLeafCell {
            _rowid: 1,
            _payload: unsafe { transmute::<&[u8], &'static [u8]>(small_payload.as_slice()) },
            first_overflow_page: None,
            payload_size: small_payload.len() as u64,
        });
        let initial_freelist_pages = db_header.lock().freelist_pages;
        let clear_result = cursor.clear_overflow_pages(&leaf_cell)?;
        match clear_result {
            CursorResult::Ok(_) => {
                assert_eq!(
                    db_header.lock().freelist_pages,
                    initial_freelist_pages,
                    "Freelist should not change when no overflow pages exist"
                );
                assert_eq!(
                    db_header.lock().freelist_trunk_page,
                    0,
                    "No trunk page should be created when no overflow pages exist"
                );
            }
            CursorResult::IO => {
                cursor.pager.io.run_once()?;
            }
        }
        Ok(())
    }
    #[test]
    fn test_btree_destroy() -> Result<()> {
        let initial_size = 3;
        let (pager, db_header) = setup_test_env(initial_size);
        let mut cursor = BTreeCursor::new_table(None, pager.clone(), 2);
        assert_eq!(
            db_header.lock().database_size,
            initial_size,
            "Database should initially have 3 pages"
        );
        let root_page = cursor.read_page(2)?;
        {
            btree_init_page(&root_page, PageType::TableInterior, 0, 512);
        }
        let page3 = cursor.allocate_page(PageType::TableLeaf, 0);
        let page4 = cursor.allocate_page(PageType::TableLeaf, 0);
        {
            let root_page = root_page.get();
            let contents = root_page.get().contents.as_mut().unwrap();
            contents.write_u32(offset::BTREE_RIGHTMOST_PTR, page4.get().get().id as u32);
            let cell_content = vec![
                (page3.get().get().id >> 24) as u8,
                (page3.get().get().id >> 16) as u8,
                (page3.get().get().id >> 8) as u8,
                page3.get().get().id as u8,
                100,
            ];
            insert_into_cell(contents, &cell_content, 0, 512)?;
        }
        for page in [&page3, &page4] {
            let page = page.get();
            let contents = page.get().contents.as_mut().unwrap();
            let record_bytes = vec![5, page.get().id as u8, b'h', b'e', b'l', b'l', b'o'];
            insert_into_cell(contents, &record_bytes, 0, 512)?;
        }
        assert_eq!(
            db_header.lock().database_size,
            5,
            "Database should have 4 pages total"
        );
        let initial_free_pages = db_header.lock().freelist_pages;
        assert_eq!(initial_free_pages, 0, "should start with no free pages");
        run_until_done(|| cursor.btree_destroy(), pager.deref())?;
        let pages_freed = db_header.lock().freelist_pages - initial_free_pages;
        assert_eq!(pages_freed, 3, "should free 3 pages (root + 2 leaves)");
        Ok(())
    }
    #[test]
    pub fn test_defragment() {
        let db = get_database();
        let conn = db.connect().unwrap();
        let page = get_page(2);
        let page = page.get();
        let page = page.get_contents();
        let header_size = 8;
        let mut total_size = 0;
        let mut cells = Vec::new();
        let usable_space = 4096;
        for i in 0..3 {
            let record =
                ImmutableRecord::from_registers(&[Register::Value(Value::Integer(i as i64))]);
            let payload = add_record(i, i, page, record, &conn);
            assert_eq!(page.cell_count(), i + 1);
            let free = compute_free_space(page, usable_space);
            total_size += payload.len() as u16 + 2;
            assert_eq!(free, 4096 - total_size - header_size);
            cells.push(Cell { pos: i, payload });
        }
        for (i, cell) in cells.iter().enumerate() {
            ensure_cell(page, i, &cell.payload);
        }
        cells.remove(1);
        drop_cell(page, 1, usable_space).unwrap();
        for (i, cell) in cells.iter().enumerate() {
            ensure_cell(page, i, &cell.payload);
        }
        defragment_page(page, usable_space);
        for (i, cell) in cells.iter().enumerate() {
            ensure_cell(page, i, &cell.payload);
        }
    }
    #[test]
    pub fn test_drop_odd_with_defragment() {
        let db = get_database();
        let conn = db.connect().unwrap();
        let page = get_page(2);
        let page = page.get();
        let page = page.get_contents();
        let header_size = 8;
        let mut total_size = 0;
        let mut cells = Vec::new();
        let usable_space = 4096;
        let total_cells = 10;
        for i in 0..total_cells {
            let record =
                ImmutableRecord::from_registers(&[Register::Value(Value::Integer(i as i64))]);
            let payload = add_record(i, i, page, record, &conn);
            assert_eq!(page.cell_count(), i + 1);
            let free = compute_free_space(page, usable_space);
            total_size += payload.len() as u16 + 2;
            assert_eq!(free, 4096 - total_size - header_size);
            cells.push(Cell { pos: i, payload });
        }
        let mut removed = 0;
        let mut new_cells = Vec::new();
        for cell in cells {
            if cell.pos % 2 == 1 {
                drop_cell(page, cell.pos - removed, usable_space).unwrap();
                removed += 1;
            } else {
                new_cells.push(cell);
            }
        }
        let cells = new_cells;
        for (i, cell) in cells.iter().enumerate() {
            ensure_cell(page, i, &cell.payload);
        }
        defragment_page(page, usable_space);
        for (i, cell) in cells.iter().enumerate() {
            ensure_cell(page, i, &cell.payload);
        }
    }
    #[test]
    pub fn test_fuzz_drop_defragment_insert() {
        let db = get_database();
        let conn = db.connect().unwrap();
        let page = get_page(2);
        let page = page.get();
        let page = page.get_contents();
        let header_size = 8;
        let mut total_size = 0;
        let mut cells = Vec::new();
        let usable_space = 4096;
        let mut i = 100000;
        let seed = thread_rng().gen();
        tracing::info!("seed {}", seed);
        let mut rng = ChaCha8Rng::seed_from_u64(seed);
        while i > 0 {
            i -= 1;
            match rng.next_u64() % 4 {
                0 => {
                    let cell_idx = rng.next_u64() as usize % (page.cell_count() + 1);
                    let free = compute_free_space(page, usable_space);
                    let record = ImmutableRecord::from_registers(&[Register::Value(
                        Value::Integer(i as i64),
                    )]);
                    let mut payload: Vec<u8> = Vec::new();
                    fill_cell_payload(
                        page.page_type(),
                        Some(i as i64),
                        &mut payload,
                        &record,
                        4096,
                        conn.pager.clone(),
                    );
                    if (free as usize) < payload.len() + 2 {
                        continue;
                    }
                    insert_into_cell(page, &payload, cell_idx, 4096).unwrap();
                    assert!(page.overflow_cells.is_empty());
                    total_size += payload.len() as u16 + 2;
                    cells.insert(cell_idx, Cell { pos: i, payload });
                }
                1 => {
                    if page.cell_count() == 0 {
                        continue;
                    }
                    let cell_idx = rng.next_u64() as usize % page.cell_count();
                    let (_, len) = page.cell_get_raw_region(
                        cell_idx,
                        payload_overflow_threshold_max(page.page_type(), 4096),
                        payload_overflow_threshold_min(page.page_type(), 4096),
                        usable_space as usize,
                    );
                    drop_cell(page, cell_idx, usable_space).unwrap();
                    total_size -= len as u16 + 2;
                    cells.remove(cell_idx);
                }
                2 => {
                    defragment_page(page, usable_space);
                }
                3 => {
                    for (i, cell) in cells.iter().enumerate() {
                        ensure_cell(page, i, &cell.payload);
                    }
                    assert_eq!(page.cell_count(), cells.len());
                }
                _ => unreachable!(),
            }
            let free = compute_free_space(page, usable_space);
            assert_eq!(free, 4096 - total_size - header_size);
        }
    }
    #[test]
    pub fn test_fuzz_drop_defragment_insert_issue_1085() {
        let db = get_database();
        let conn = db.connect().unwrap();
        let page = get_page(2);
        let page = page.get();
        let page = page.get_contents();
        let header_size = 8;
        let mut total_size = 0;
        let mut cells = Vec::new();
        let usable_space = 4096;
        let mut i = 1000;
        for seed in [15292777653676891381, 9261043168681395159] {
            tracing::info!("seed {}", seed);
            let mut rng = ChaCha8Rng::seed_from_u64(seed);
            while i > 0 {
                i -= 1;
                match rng.next_u64() % 3 {
                    0 => {
                        let cell_idx = rng.next_u64() as usize % (page.cell_count() + 1);
                        let free = compute_free_space(page, usable_space);
                        let record = ImmutableRecord::from_registers(&[Register::Value(
                            Value::Integer(i as i64),
                        )]);
                        let mut payload: Vec<u8> = Vec::new();
                        fill_cell_payload(
                            page.page_type(),
                            Some(i),
                            &mut payload,
                            &record,
                            4096,
                            conn.pager.clone(),
                        );
                        if (free as usize) < payload.len() - 2 {
                            continue;
                        }
                        insert_into_cell(page, &payload, cell_idx, 4096).unwrap();
                        assert!(page.overflow_cells.is_empty());
                        total_size += payload.len() as u16 + 2;
                        cells.push(Cell {
                            pos: i as usize,
                            payload,
                        });
                    }
                    1 => {
                        if page.cell_count() == 0 {
                            continue;
                        }
                        let cell_idx = rng.next_u64() as usize % page.cell_count();
                        let (_, len) = page.cell_get_raw_region(
                            cell_idx,
                            payload_overflow_threshold_max(page.page_type(), 4096),
                            payload_overflow_threshold_min(page.page_type(), 4096),
                            usable_space as usize,
                        );
                        drop_cell(page, cell_idx, usable_space).unwrap();
                        total_size -= len as u16 + 2;
                        cells.remove(cell_idx);
                    }
                    2 => {
                        defragment_page(page, usable_space);
                    }
                    _ => unreachable!(),
                }
                let free = compute_free_space(page, usable_space);
                assert_eq!(free, 4096 - total_size - header_size);
            }
        }
    }
    #[test]
    pub fn test_drop_page_in_balancing_issue_1203() {
        let db = get_database();
        let conn = db.connect().unwrap();
        let queries = vec![
            "CREATE TABLE lustrous_petit (awesome_nomous TEXT,ambitious_amargi TEXT,fantastic_daniels BLOB,stupendous_highleyman TEXT,relaxed_crane TEXT,elegant_bromma INTEGER,proficient_castro BLOB,ambitious_liman TEXT,responsible_lusbert BLOB);",
            "INSERT INTO lustrous_petit VALUES ('funny_sarambi', 'hardworking_naoumov', X'666561726C6573735F68696C6C', 'elegant_iafd', 'rousing_flag', 681399778772406122, X'706572736F6E61626C655F676F6477696E6772696D6D', 'insightful_anonymous', X'706F77657266756C5F726F636861'), ('personable_holmes', 'diligent_pera', X'686F6E6573745F64696D656E73696F6E', 'energetic_raskin', 'gleaming_federasyon', -2778469859573362611, X'656666696369656E745F6769617A', 'sensible_skirda', X'66616E7461737469635F6B656174696E67'), ('inquisitive_baedan', 'brave_sphinx', X'67656E65726F75735F6D6F6E7473656E79', 'inquisitive_syndicate', 'amiable_room', 6954857961525890638, X'7374756E6E696E675F6E6965747A73636865', 'glowing_coordinator', X'64617A7A6C696E675F7365766572696E65'), ('upbeat_foxtale', 'engaging_aktimon', X'63726561746976655F6875746368696E6773', 'ample_locura', 'creative_barrett', 6413352509911171593, X'6772697070696E675F6D696E7969', 'competitive_parissi', X'72656D61726B61626C655F77696E7374616E6C6579');",
            "INSERT INTO lustrous_petit VALUES ('ambitious_berry', 'devoted_marshall', X'696E7175697369746976655F6C6172657661', 'flexible_pramen', 'outstanding_stauch', 6936508362673228293, X'6C6F76696E675F6261756572', 'charming_anonymous', X'68617264776F726B696E675F616E6E6973'), ('enchanting_cohen', 'engaging_rubel', X'686F6E6573745F70726F766F63617A696F6E65', 'humorous_robin', 'imaginative_shuzo', 4762266264295288131, X'726F7573696E675F6261796572', 'vivid_bolling', X'6F7267616E697A65645F7275696E73'), ('affectionate_resistance', 'gripping_rustamova', X'6B696E645F6C61726B696E', 'bright_boulanger', 'upbeat_ashirov', -1726815435854320541, X'61646570745F66646361', 'dazzling_tashjian', X'68617264776F726B696E675F6D6F72656C'), ('zestful_ewald', 'favorable_lewis', X'73747570656E646F75735F7368616C6966', 'bright_combustion', 'blithesome_harding', 8408539013935554176, X'62726176655F737079726F706F756C6F75', 'hilarious_finnegan', X'676976696E675F6F7267616E697A696E67'), ('blithesome_picqueray', 'sincere_william', X'636F75726167656F75735F6D69746368656C6C', 'rousing_atan', 'mirthful_katie', -429232313453215091, X'6C6F76656C795F776174616E616265', 'stupendous_mcmillan', X'666F63757365645F6B61666568'), ('incredible_kid', 'friendly_yvetot', X'706572666563745F617A697A', 'helpful_manhattan', 'shining_horrox', -4318061095860308846, X'616D626974696F75735F726F7765', 'twinkling_anarkiya', X'696D6167696E61746976655F73756D6E6572');",
            "INSERT INTO lustrous_petit VALUES ('sleek_graeber', 'approachable_ghazzawi', X'62726176655F6865776974747768697465', 'adaptable_zimmer', 'polite_cohn', -5464225138957223865, X'68756D6F726F75735F736E72', 'adaptable_igualada', X'6C6F76656C795F7A686F75'), ('imaginative_rautiainen', 'magnificent_ellul', X'73706C656E6469645F726F6361', 'responsible_brown', 'upbeat_uruguaya', -1185340834321792223, X'616D706C655F6D6470', 'philosophical_kelly', X'676976696E675F6461676865726D6172676F7369616E'), ('blithesome_darkness', 'creative_newell', X'6C757374726F75735F61706174726973', 'engaging_kids', 'charming_wark', -1752453819873942466, X'76697669645F6162657273', 'independent_barricadas', X'676C697374656E696E675F64686F6E6474'), ('productive_chardronnet', 'optimistic_karnage', X'64696C6967656E745F666F72657374', 'engaging_beggar', 'sensible_wolke', 784341549042407442, X'656E676167696E675F6265726B6F7769637A', 'blithesome_zuzenko', X'6E6963655F70726F766F63617A696F6E65');",
            "INSERT INTO lustrous_petit VALUES ('shining_sagris', 'considerate_mother', X'6F70656E5F6D696E6465645F72696F74', 'polite_laufer', 'patient_mink', 2240393952789100851, X'636F75726167656F75735F6D636D696C6C616E', 'glowing_robertson', X'68656C7066756C5F73796D6F6E6473'), ('dazzling_glug', 'stupendous_poznan', X'706572736F6E61626C655F6672616E6B73', 'open_minded_ruins', 'qualified_manes', 2937238916206423261, X'696E736967687466756C5F68616B69656C', 'passionate_borl', X'616D6961626C655F6B7570656E647561'), ('wondrous_parry', 'knowledgeable_giovanni', X'6D6F76696E675F77696E6E', 'shimmering_aberlin', 'affectionate_calhoun', 702116954493913499, X'7265736F7572636566756C5F62726F6D6D61', 'propitious_mezzagarcia', X'746563686E6F6C6F676963616C5F6E6973686974616E69');",
            "INSERT INTO lustrous_petit VALUES ('kind_room', 'hilarious_crow', X'6F70656E5F6D696E6465645F6B6F74616E7969', 'hardworking_petit', 'adaptable_zarrow', 2491343172109894986, X'70726F647563746976655F646563616C6F677565', 'willing_sindikalis', X'62726561746874616B696E675F6A6F7264616E');",
            "INSERT INTO lustrous_petit VALUES ('confident_etrebilal', 'agreeable_shifu', X'726F6D616E7469635F7363687765697A6572', 'loving_debs', 'gripping_spooner', -3136910055229112693, X'677265676172696F75735F736B726F7A6974736B79', 'ample_ontiveros', X'7175616C69666965645F726F6D616E69656E6B6F'), ('competitive_call', 'technological_egoumenides', X'6469706C6F6D617469635F6D6F6E616768616E', 'willing_stew', 'frank_neal', -5973720171570031332, X'6C6F76696E675F6465737461', 'dazzling_gambone', X'70726F647563746976655F6D656E64656C676C6565736F6E'), ('favorable_delesalle', 'sensible_atterbury', X'666169746866756C5F64617861', 'bountiful_aldred', 'marvelous_malgraith', 5330463874397264493, X'706572666563745F7765726265', 'lustrous_anti', X'6C6F79616C5F626F6F6B6368696E'), ('stellar_corlu', 'loyal_espana', X'6D6F76696E675F7A6167', 'efficient_nelson', 'qualified_shepard', 1015518116803600464, X'737061726B6C696E675F76616E6469766572', 'loving_scoffer', X'686F6E6573745F756C72696368'), ('adaptable_taylor', 'shining_yasushi', X'696D6167696E61746976655F776974746967', 'alluring_blackmore', 'zestful_coeurderoy', -7094136731216188999, X'696D6167696E61746976655F757A63617465677569', 'gleaming_hernandez', X'6672616E6B5F646F6D696E69636B'), ('competitive_luis', 'stellar_fredericks', X'616772656561626C655F6D696368656C', 'optimistic_navarro', 'funny_hamilton', 4003895682491323194, X'6F70656E5F6D696E6465645F62656C6D6173', 'incredible_thorndycraft', X'656C6567616E745F746F6C6B69656E'), ('remarkable_parsons', 'sparkling_ulrich', X'737061726B6C696E675F6D6172696E636561', 'technological_leighlais', 'warmhearted_konok', -5789111414354869563, X'676976696E675F68657272696E67', 'adept_dabtara', X'667269656E646C795F72617070');",
            "INSERT INTO lustrous_petit VALUES ('hardworking_norberg', 'approachable_winter', X'62726176655F68617474696E6768', 'imaginative_james', 'open_minded_capital', -5950508516718821688, X'6C757374726F75735F72616E7473', 'warmhearted_limanov', X'696E736967687466756C5F646F637472696E65'), ('generous_shatz', 'generous_finley', X'726176697368696E675F6B757A6E6574736F76', 'stunning_arrigoni', 'favorable_volcano', -8442328990977069526, X'6D6972746866756C5F616C7467656C64', 'thoughtful_zurbrugg', X'6D6972746866756C5F6D6F6E726F65'), ('frank_kerr', 'splendid_swain', X'70617373696F6E6174655F6D6470', 'flexible_dubey', 'sensible_tj', 6352949260574274181, X'656666696369656E745F6B656D736B79', 'vibrant_ege', X'736C65656B5F6272696768746F6E'), ('organized_neal', 'glistening_sugar', X'656E676167696E675F6A6F72616D', 'romantic_krieger', 'qualified_corr', -4774868512022958085, X'706572666563745F6B6F7A6172656B', 'bountiful_zaikowska', X'74686F7567687466756C5F6C6F6767616E73'), ('excellent_lydiettcarrion', 'diligent_denslow', X'666162756C6F75735F6D616E68617474616E', 'confident_tomar', 'glistening_ligt', -1134906665439009896, X'7175616C69666965645F6F6E6B656E', 'remarkable_anarkiya', X'6C6F79616C5F696E64616261'), ('passionate_melis', 'loyal_xsilent', X'68617264776F726B696E675F73637564', 'lustrous_barnes', 'nice_sugako', -4097897163377829983, X'726F6D616E7469635F6461686572', 'bright_imrie', X'73656E7369626C655F6D61726B'), ('giving_mlb', 'breathtaking_fourier', X'736C65656B5F616E61726368697374', 'glittering_malet', 'brilliant_crew', 8791228049111405793, X'626F756E746966756C5F626576656E736565', 'lovely_swords', X'70726F706974696F75735F696E656469746173'), ('honest_wright', 'qualified_rabble', X'736C65656B5F6D6172656368616C', 'shimmering_marius', 'blithesome_mckelvie', -1330737263592370654, X'6F70656E5F6D696E6465645F736D616C6C', 'energetic_gorman', X'70726F706974696F75735F6B6F74616E7969');",
            "DELETE FROM lustrous_petit WHERE (ambitious_liman > 'adept_dabtaqu');",
            "INSERT INTO lustrous_petit VALUES ('technological_dewey', 'fabulous_st', X'6F7074696D69737469635F73687562', 'considerate_levy', 'adaptable_kernis', 4195134012457716562, X'61646570745F736F6C6964617269646164', 'vibrant_crump', X'6C6F79616C5F72796E6572'), ('super_marjan', 'awesome_gethin', X'736C65656B5F6F737465727765696C', 'diplomatic_loidl', 'qualified_bokani', -2822676417968234733, X'6272696768745F64756E6C6170', 'creative_en', X'6D6972746866756C5F656C6F6666'), ('philosophical_malet', 'unique_garcia', X'76697669645F6E6F7262657267', 'spellbinding_fire', 'faithful_barringtonbush', -7293711848773657758, X'6272696C6C69616E745F6F6B65656665', 'gripping_guillon', X'706572736F6E61626C655F6D61726C696E7370696B65'), ('thoughtful_morefus', 'lustrous_rodriguez', X'636F6E666964656E745F67726F73736D616E726F73686368696E', 'devoted_jackson', 'propitious_karnage', -7802999054396485709, X'63617061626C655F64', 'enchanting_orwell', X'7477696E6B6C696E675F64616C616B6F676C6F75'), ('alluring_guillon', 'brilliant_pinotnoir', X'706572736F6E61626C655F6A6165636B6C65', 'open_minded_azeez', 'courageous_romania', 2126962403055072268, X'746563686E6F6C6F676963616C5F6962616E657A', 'open_minded_rosa', X'6C757374726F75735F6575726F7065'), ('courageous_kolokotronis', 'inquisitive_gahman', X'677265676172696F75735F626172726574', 'ambitious_shakur', 'fantastic_apatris', -1232732971861520864, X'737061726B6C696E675F7761746368', 'captivating_clover', X'636F6E666964656E745F736574686E65737363617374726F'), ('charming_sullivan', 'focused_congress', X'7368696D6D6572696E675F636C7562', 'wondrous_skrbina', 'giving_mendanlioglu', -6837337053772308333, X'636861726D696E675F73616C696E6173', 'rousing_hedva', X'6469706C6F6D617469635F7061796E');",
        ];
        for query in queries {
            let mut stmt = conn.query(query).unwrap().unwrap();
            loop {
                let row = stmt.step().expect("step");
                match row {
                    StepResult::Done => {
                        break;
                    }
                    _ => {
                        tracing::debug!("row {:?}", row);
                    }
                }
            }
        }
    }
    #[test]
    pub fn test_drop_page_in_balancing_issue_1203_2() {
        let db = get_database();
        let conn = db.connect().unwrap();
        let queries = vec![
            "CREATE TABLE super_becky (engrossing_berger BLOB,plucky_chai BLOB,mirthful_asbo REAL,bountiful_jon REAL,competitive_petit REAL,engrossing_rexroth REAL);",
            "INSERT INTO super_becky VALUES (X'636861726D696E675F6261796572', X'70726F647563746976655F70617269737369', 6847793643.408741, 7330361375.924953, -6586051582.891455, -6921021872.711397), (X'657863656C6C656E745F6F7267616E697A696E67', X'6C757374726F75735F73696E64696B616C6973', 9905774996.48619, 570325205.2246342, 5852346465.53047, 728566012.1968269), (X'7570626561745F73656174746C65', X'62726176655F6661756E', -2202725836.424899, 5424554426.388281, 2625872085.917082, -6657362503.808359), (X'676C6F77696E675F6D617877656C6C', X'7761726D686561727465645F726F77616E', -9610936969.793116, 4886606277.093559, -3414536174.7928505, 6898267795.317778), (X'64796E616D69635F616D616E', X'7374656C6C61725F7374657073', 3918935692.153696, 151068445.947237, 4582065669.356403, -3312668220.4789667), (X'64696C6967656E745F64757272757469', X'7175616C69666965645F6D726163686E696B', 5527271629.262201, 6068855126.044355, 289904657.13490677, 2975774820.0877323), (X'6469706C6F6D617469635F726F76657363696F', X'616C6C7572696E675F626F7474696369', 9844748192.66119, -6180276383.305578, -4137330511.025565, -478754566.79494476), (X'776F6E64726F75735F6173686572', X'6465766F7465645F6176657273696F6E', 2310211470.114773, -6129166761.628184, -2865371645.3145514, 7542428654.8645935), (X'617070726F61636861626C655F6B686F6C61', X'6C757374726F75735F6C696E6E656C6C', -4993113161.458349, 7356727284.362968, -3228937035.568404, -1779334005.5067253);",
            "INSERT INTO super_becky VALUES (X'74686F7567687466756C5F726576696577', X'617765736F6D655F63726F73736579', 9401977997.012783, 8428201961.643898, 2822821303.052643, 4555601220.718847), (X'73706563746163756C61725F6B686179617469', X'616772656561626C655F61646F6E696465', 7414547022.041355, 365016845.73330307, 50682963.055828094, -9258802584.962656), (X'6C6F79616C5F656D6572736F6E', X'676C6F77696E675F626174616C6F', -5522070106.765736, 2712536599.6384163, 6631385631.869345, 1242757880.7583427), (X'68617264776F726B696E675F6F6B656C6C79', X'666162756C6F75735F66696C697373', 6682622809.9778805, 4233900041.917185, 9017477903.795563, -756846353.6034946), (X'68617264776F726B696E675F626C61756D616368656E', X'616666656374696F6E6174655F6B6F736D616E', -1146438175.3174362, -7545123696.438596, -6799494012.403366, 5646913977.971333), (X'66616E7461737469635F726F77616E', X'74686F7567687466756C5F7465727269746F72696573', -4414529784.916277, -6209371635.279242, 4491104121.288605, 2590223842.117277);",
            "INSERT INTO super_becky VALUES (X'676C697374656E696E675F706F72746572', X'696E7175697369746976655F656D', 2986144164.3676434, 3495899172.5935287, -849280584.9386635, 6869709150.2699375), (X'696D6167696E61746976655F6D65726C696E6F', X'676C6F77696E675F616B74696D6F6E', 8733490615.829357, 6782649864.719433, 6926744218.74107, 1532081022.4379768), (X'6E6963655F726F73736574', X'626C69746865736F6D655F66696C697373', -839304300.0706863, 6155504968.705227, -2951592321.950267, -6254186334.572437), (X'636F6E666964656E745F6C69626574', X'676C696D6D6572696E675F6B6F74616E7969', -5344675223.37533, -8703794729.211002, 3987472096.020382, -7678989974.961197), (X'696D6167696E61746976655F6B61726162756C7574', X'64796E616D69635F6D6367697272', 2028227065.6995697, -7435689525.030833, 7011220815.569796, 5526665697.213846), (X'696E7175697369746976655F636C61726B', X'616666656374696F6E6174655F636C6561766572', 3016598350.546356, -3686782925.383732, 9671422351.958004, 9099319829.078941), (X'63617061626C655F746174616E6B61', X'696E6372656469626C655F6F746F6E6F6D61', 6339989259.432795, -8888997534.102034, 6855868409.475763, -2565348887.290493), (X'676F7267656F75735F6265726E657269', X'65647563617465645F6F6D6F77616C69', 6992467657.527826, -3538089391.748543, -7103111660.146708, 4019283237.3740463), (X'616772656561626C655F63756C74757265', X'73706563746163756C61725F657370616E61', 189387871.06959534, 6211851191.361202, 1786455196.9768047, 7966404387.318119);",
            "INSERT INTO super_becky VALUES (X'7068696C6F736F70686963616C5F6C656967686C616973', X'666162756C6F75735F73656D696E61746F7265', 8688321500.141502, -7855144036.024546, -5234949709.573349, -9937638367.366447), (X'617070726F61636861626C655F726F677565', X'676C65616D696E675F6D7574696E79', -5351540099.744092, -3614025150.9013805, -2327775310.276925, 2223379997.077526), (X'676C696D6D6572696E675F63617263686961', X'696D6167696E61746976655F61737379616E6E', 4104832554.8371887, -5531434716.627781, 1652773397.4099865, 3884980522.1830273);",
            "DELETE FROM super_becky WHERE (plucky_chai != X'7761726D686561727465645F6877616E67' AND mirthful_asbo != 9537234687.183533 AND bountiful_jon = -3538089391.748543);",
            "INSERT INTO super_becky VALUES (X'706C75636B795F6D617263616E74656C', X'696D6167696E61746976655F73696D73', 9535651632.375484, 92270815.0720501, 1299048084.6248207, 6460855331.572151), (X'726F6D616E7469635F706F746C61746368', X'68756D6F726F75735F63686165686F', 9345375719.265533, 7825332230.247925, -7133157299.39028, -6939677879.6597), (X'656666696369656E745F6261676E696E69', X'63726561746976655F67726168616D', -2615470560.1954746, 6790849074.977201, -8081732985.448849, -8133707792.312794), (X'677265676172696F75735F73637564', X'7368696E696E675F67726F7570', -7996394978.2610035, -9734939565.228964, 1108439333.8481388, -5420483517.169478), (X'6C696B61626C655F6B616E6176616C6368796B', X'636F75726167656F75735F7761726669656C64', -1959869609.656724, 4176668769.239971, -8423220404.063669, 9987687878.685959), (X'657863656C6C656E745F68696C6473646F74746572', X'676C6974746572696E675F7472616D7564616E61', -5220160777.908238, 3892402687.8826714, 9803857762.617172, -1065043714.0265541), (X'6D61676E69666963656E745F717565657273', X'73757065725F717565657273', -700932053.2006226, -4706306995.253335, -5286045811.046467, 1954345265.5250092), (X'676976696E675F6275636B65726D616E6E', X'667269656E646C795F70697A7A6F6C61746F', -2186859620.9089565, -6098492099.446075, -7456845586.405931, 8796967674.444252);",
            "DELETE FROM super_becky WHERE TRUE;",
            "INSERT INTO super_becky VALUES (X'6F7074696D69737469635F6368616E69616C', X'656E657267657469635F6E65677261', 1683345860.4208698, 4163199322.9289455, -4192968616.7868404, -7253371206.571701), (X'616C6C7572696E675F686176656C', X'7477696E6B6C696E675F626965627579636B', -9947019174.287437, 5975899640.893995, 3844707723.8570194, -9699970750.513876), (X'6F7074696D69737469635F7A686F75', X'616D626974696F75735F636F6E6772657373', 4143738484.1081524, -2138255286.170598, 9960750454.03466, 5840575852.80299), (X'73706563746163756C61725F6A6F6E67', X'73656E7369626C655F616269646F72', -1767611042.9716015, -7684260477.580351, 4570634429.188147, -9222640121.140202), (X'706F6C6974655F6B657272', X'696E736967687466756C5F63686F646F726B6F6666', -635016769.5123329, -4359901288.494518, -7531565119.905825, -1180410948.6572971), (X'666C657869626C655F636F6D756E69656C6C6F', X'6E6963655F6172636F73', 8708423014.802425, -6276712625.559328, -771680766.2485523, 8639486874.113342);",
            "DELETE FROM super_becky WHERE (mirthful_asbo < 9730384310.536528 AND plucky_chai < X'6E6963655F61726370B2');",
            "DELETE FROM super_becky WHERE (mirthful_asbo > 6248699554.426553 AND bountiful_jon > 4124481472.333034);",
            "INSERT INTO super_becky VALUES (X'676C696D6D6572696E675F77656C7368', X'64696C6967656E745F636F7262696E', 8217054003.369003, 8745594518.77864, 1928172803.2261295, -8375115534.050233), (X'616772656561626C655F6463', X'6C6F76696E675F666F72656D616E', -5483889804.871533, -8264576639.127487, 4770567289.404846, -3409172927.2573576), (X'6D617276656C6F75735F6173696D616B6F706F756C6F73', X'746563686E6F6C6F676963616C5F6A61637175696572', 2694858779.206814, -1703227425.3442516, -4504989231.263319, -3097265869.5230227), (X'73747570656E646F75735F64757075697364657269', X'68696C6172696F75735F6D75697268656164', 568174708.66469, -4878260547.265669, -9579691520.956625, 73507727.8100338), (X'626C69746865736F6D655F626C6F6B', X'61646570745F6C65696572', 7772117077.916897, 4590608571.321514, -881713470.657032, -9158405774.647465);",
            "INSERT INTO super_becky VALUES (X'6772697070696E675F6573736578', X'67656E65726F75735F636875726368696C6C', -4180431825.598956, 7277443000.677654, 2499796052.7878246, -2858339306.235305), (X'756E697175655F6D6172656368616C', X'62726561746874616B696E675F636875726368696C6C', 1401354536.7625294, -611427440.2796707, -4621650430.463729, 1531473111.7482872), (X'657863656C6C656E745F66696E6C6579', X'666169746866756C5F62726F636B', -4020697828.0073624, -2833530733.19637, -7766170050.654022, 8661820959.434689);",
            "INSERT INTO super_becky VALUES (X'756E697175655F6C617061797265', X'6C6F76696E675F7374617465', 7063237787.258968, -5425712581.365798, -7750509440.0141945, -7570954710.892544), (X'62726561746874616B696E675F6E65616C', X'636F75726167656F75735F61727269676F6E69', 289862394.2028198, 9690362375.014446, -4712463267.033899, 2474917855.0973473), (X'7477696E6B6C696E675F7368616B7572', X'636F75726167656F75735F636F6D6D6974746565', 5449035403.229155, -2159678989.597906, 3625606019.1150894, -3752010405.4475393);",
            "INSERT INTO super_becky VALUES (X'70617373696F6E6174655F73686970776179', X'686F6E6573745F7363687765697A6572', 4193384746.165228, -2232151704.896323, 8615245520.962444, -9789090953.995636);",
            "INSERT INTO super_becky VALUES (X'6C696B61626C655F69', X'6661766F7261626C655F6D626168', 6581403690.769894, 3260059398.9544716, -407118859.046051, -3155853965.2700634), (X'73696E636572655F6F72', X'616772656561626C655F617070656C6261756D', 9402938544.308651, -7595112171.758331, -7005316716.211025, -8368210960.419411);",
            "INSERT INTO super_becky VALUES (X'6D617276656C6F75735F6B61736864616E', X'6E6963655F636F7272', -5976459640.85817, -3177550476.2092276, 2073318650.736992, -1363247319.9978447);",
            "INSERT INTO super_becky VALUES (X'73706C656E6469645F6C616D656E646F6C61', X'677265676172696F75735F766F6E6E65677574', 6898259773.050102, 8973519699.707073, -25070632.280548096, -1845922497.9676847), (X'617765736F6D655F7365766572', X'656E657267657469635F706F746C61746368', -8750678407.717808, 5130907533.668898, -6778425327.111566, 3718982135.202587);",
            "INSERT INTO super_becky VALUES (X'70726F706974696F75735F6D616C617465737461', X'657863656C6C656E745F65766572657474', -8846855772.62094, -6168969732.697067, -8796372709.125793, 9983557891.544613), (X'73696E636572655F6C6177', X'696E7175697369746976655F73616E647374726F6D', -6366985697.975358, 3838628702.6652164, 3680621713.3371124, -786796486.8049564), (X'706F6C6974655F676C6561736F6E', X'706C75636B795F677579616E61', -3987946379.104308, -2119148244.413993, -1448660343.6888638, -1264195510.1611118), (X'676C6974746572696E675F6C6975', X'70657273697374656E745F6F6C6976696572', 6741779968.943846, -3239809989.227495, -1026074003.5506897, 4654600514.871752);",
            "DELETE FROM super_becky WHERE (engrossing_berger < X'6566651A3C70278D4E200657551D8071A1' AND competitive_petit > 1236742147.9451914);",
            "INSERT INTO super_becky VALUES (X'6661766F7261626C655F726569746D616E', X'64657465726D696E65645F726974746572', -7412553243.829927, -7572665195.290464, 7879603411.222157, 3706943306.5691853), (X'70657273697374656E745F6E6F6C616E', X'676C6974746572696E675F73686570617264', 7028261282.277422, -2064164782.3494844, -5244048504.507779, -2399526243.005843), (X'6B6E6F776C6564676561626C655F70617474656E', X'70726F66696369656E745F726F7365627261756768', 3713056763.583538, 3919834206.566164, -6306779387.430006, -9939464323.995546), (X'616461707461626C655F7172757A', X'696E7175697369746976655F68617261776179', 6519349690.299835, -9977624623.820414, 7500579325.440605, -8118341251.362242);",
            "INSERT INTO super_becky VALUES (X'636F6E73696465726174655F756E696F6E', X'6E6963655F6573736578', -1497385534.8720198, 9957688503.242973, 9191804202.566128, -179015615.7117195), (X'666169746866756C5F626F776C656773', X'6361707469766174696E675F6D6367697272', 893707300.1576138, 3381656294.246702, 6884723724.381908, 6248331214.701559), (X'6B6E6F776C6564676561626C655F70656E6E61', X'6B696E645F616A697468', -3335162603.6574974, 1812878172.8505402, 5115606679.658335, -5690100280.808182), (X'617765736F6D655F77696E7374616E6C6579', X'70726F706974696F75735F6361726173736F', -7395576292.503981, 4956546102.029215, -1468521769.7486448, -2968223925.60355), (X'636F75726167656F75735F77617266617265', X'74686F7567687466756C5F7361707068697265', 7052982930.566017, -9806098174.104418, -6910398936.377775, -4041963031.766964), (X'657863656C6C656E745F6B62', X'626C69746865736F6D655F666F75747A6F706F756C6F73', 6142173202.994768, 5193126957.544125, -7522202722.983735, -1659088056.594862), (X'7374756E6E696E675F6E6576616461', X'626F756E746966756C5F627572746F6E', -3822097036.7628613, -3458840259.240303, 2544472236.86788, 6928890176.466003);",
            "INSERT INTO super_becky VALUES (X'706572736F6E61626C655F646D69747269', X'776F6E64726F75735F6133796F', 2651932559.0077076, 811299402.3174248, -8271909238.671928, 6761098864.189909);",
            "INSERT INTO super_becky VALUES (X'726F7573696E675F6B6C6166657461', X'64617A7A6C696E675F6B6E617070', 9370628891.439335, -5923332007.253168, -2763161830.5880013, -9156194881.875952), (X'656666696369656E745F6C6576656C6C6572', X'616C6C7572696E675F706561636F7474', 3102641409.8314342, 2838360181.628153, 2466271662.169607, 1015942181.844162), (X'6469706C6F6D617469635F7065726B696E73', X'726F7573696E675F6172616269', -1551071129.022499, -8079487600.186886, 7832984580.070087, -6785993247.895652), (X'626F756E746966756C5F6D656D62657273', X'706F77657266756C5F70617269737369', 9226031830.72445, 7012021503.536997, -2297349030.108919, -2738320055.4710903), (X'676F7267656F75735F616E6172636F7469636F', X'68656C7066756C5F7765696C616E64', -8394163480.676959, -2978605095.699134, -6439355448.021704, 9137308022.281273), (X'616666656374696F6E6174655F70726F6C65696E666F', X'706C75636B795F73616E7A', 3546758708.3524914, -1870964264.9353771, 338752565.3643894, -3908023657.299715), (X'66756E6E795F706F70756C61697265', X'6F75747374616E64696E675F626576696E67746F6E', -1533858145.408224, 6164225076.710373, 8419445987.622173, 584555253.6852646), (X'76697669645F6D7474', X'7368696D6D6572696E675F70616F6E65737361', 5512251366.193035, -8680583180.123213, -4445968638.153208, -3274009935.4229546);",
            "INSERT INTO super_becky VALUES (X'7068696C6F736F70686963616C5F686F7264', X'657863656C6C656E745F67757373656C7370726F757473', -816909447.0240917, -3614686681.8786583, 7701617524.26067, -4541962047.183721), (X'616D6961626C655F69676E6174696576', X'6D61676E69666963656E745F70726F76696E6369616C69', -1318532883.847702, -4918966075.976474, -7601723171.33518, -3515747704.3847466), (X'70726F66696369656E745F32303137', X'66756E6E795F6E77', -1264540201.518032, 8227396547.578808, 6245093925.183641, -8368355328.110817);",
            "INSERT INTO super_becky VALUES (X'77696C6C696E675F6E6F6B6B65', X'726F6D616E7469635F677579616E61', 6618610796.3707695, -3814565359.1524105, 1663106272.4565296, -4175107840.768817), (X'72656C617865645F7061766C6F76', X'64657465726D696E65645F63686F646F726B6F6666', -3350029338.034504, -3520837855.4619064, 3375167499.631817, -8866806483.714607), (X'616D706C655F67696464696E6773', X'667269656E646C795F6A6F686E', 1458864959.9942684, 1344208968.0486107, 9335156635.91314, -6180643697.918882), (X'72656C617865645F6C65726F79', X'636F75726167656F75735F6E6F72646772656E', -5164986537.499656, 8820065797.720875, 6146530425.891005, 6949241471.958189), (X'666F63757365645F656D6D61', X'696D6167696E61746976655F6C6F6E67', -9587619060.80035, 6128068142.184402, 6765196076.956905, 800226302.7983418);",
            "INSERT INTO super_becky VALUES (X'616D626974696F75735F736F6E67', X'706572666563745F6761686D616E', 4989979180.706432, -9374266591.537058, 314459621.2820797, -3200029490.9553604), (X'666561726C6573735F626C6174', X'676C697374656E696E675F616374696F6E', -8512203612.903147, -7625581186.013805, -9711122307.234787, -301590929.32751083), (X'617765736F6D655F6669646573', X'666169746866756C5F63756E6E696E6768616D', -1428228887.9205084, 7669883854.400173, 5604446195.905277, -1509311057.9653416), (X'68756D6F726F75735F77697468647261776E', X'62726561746874616B696E675F7472617562656C', -7292778713.676636, -6728132503.529593, 2805341768.7252483, 330416975.2300949);",
            "INSERT INTO super_becky VALUES (X'677265676172696F75735F696873616E', X'7374656C6C61725F686172746D616E', 8819210651.1988, 5298459883.813452, 7293544377.958424, 460475869.72971725), (X'696E736967687466756C5F62657765726E69747A', X'676C65616D696E675F64656E736C6F77', -6911957282.193239, 1754196756.2193146, -6316860403.693853, -3094020672.236368), (X'6D6972746866756C5F616D6265727261656B656C6C79', X'68756D6F726F75735F6772617665', 1785574023.0269203, -372056983.82761574, 4133719439.9538956, 9374053482.066044), (X'76697669645F736169747461', X'7761726D686561727465645F696E656469746173', 2787071361.6099434, 9663839418.553448, -5934098589.901047, -9774745509.608858), (X'61646570745F6F6375727279', X'6C696B61626C655F726569746D616E', -3098540915.1310825, 5460848322.672174, -6012867197.519758, 6769770087.661135), (X'696E646570656E64656E745F6F', X'656C6567616E745F726F6F726461', 1462542860.3143978, 3360904654.2464733, 5458876201.665213, -5522844849.529962), (X'72656D61726B61626C655F626F6B616E69', X'6F70656E5F6D696E6465645F686F72726F78', 7589481760.867031, 7970075121.546291, 7513467575.5213585, 9663061478.289227), (X'636F6E666964656E745F6C616479', X'70617373696F6E6174655F736B726F7A6974736B79', 8266917234.53915, -7172933478.625412, 309854059.94031143, -8309837814.497616);",
            "DELETE FROM super_becky WHERE (competitive_petit != 8725256604.165474 OR engrossing_rexroth > -3607424615.7839313 OR plucky_chai < X'726F7573696E675F6216E20375');",
            "INSERT INTO super_becky VALUES (X'7368696E696E675F736F6C69646169726573', X'666561726C6573735F63617264616E', -170727879.20838165, 2744601113.384678, 5676912434.941502, 6757573601.657997), (X'636F75726167656F75735F706C616E636865', X'696E646570656E64656E745F636172736F6E', -6271723086.761938, -180566679.7470188, -1285774632.134449, 1359665735.7842407), (X'677265676172696F75735F7374616D61746F76', X'7374756E6E696E675F77696C64726F6F7473', -6210238866.953484, 2492683045.8287067, -9688894361.68205, 5420275482.048567), (X'696E646570656E64656E745F6F7267616E697A6572', X'676C6974746572696E675F736F72656C', 9291163783.3073, -6843003475.769236, -1320245894.772686, -5023483808.044955), (X'676C6F77696E675F6E65736963', X'676C65616D696E675F746F726D6579', 829526382.8027191, 9365690945.1316, 4761505764.826195, -4149154965.0024815), (X'616C6C7572696E675F646F637472696E65', X'6E6963655F636C6561766572', 3896644979.981762, -288600448.8016701, 9462856570.130062, -909633752.5993862);",
        ];
        for query in queries {
            let mut stmt = conn.query(query).unwrap().unwrap();
            loop {
                let row = stmt.step().expect("step");
                match row {
                    StepResult::Done => {
                        break;
                    }
                    _ => {
                        tracing::debug!("row {:?}", row);
                    }
                }
            }
        }
    }
    #[test]
    pub fn test_free_space() {
        let db = get_database();
        let conn = db.connect().unwrap();
        let page = get_page(2);
        let page = page.get();
        let page = page.get_contents();
        let header_size = 8;
        let usable_space = 4096;
        let record = ImmutableRecord::from_registers(&[Register::Value(Value::Integer(0))]);
        let payload = add_record(0, 0, page, record, &conn);
        let free = compute_free_space(page, usable_space);
        assert_eq!(free, 4096 - payload.len() as u16 - 2 - header_size);
    }
    #[test]
    pub fn test_defragment_1() {
        let db = get_database();
        let conn = db.connect().unwrap();
        let page = get_page(2);
        let page = page.get();
        let page = page.get_contents();
        let usable_space = 4096;
        let record = ImmutableRecord::from_registers(&[Register::Value(Value::Integer(0))]);
        let payload = add_record(0, 0, page, record, &conn);
        assert_eq!(page.cell_count(), 1);
        defragment_page(page, usable_space);
        assert_eq!(page.cell_count(), 1);
        let (start, len) = page.cell_get_raw_region(
            0,
            payload_overflow_threshold_max(page.page_type(), 4096),
            payload_overflow_threshold_min(page.page_type(), 4096),
            usable_space as usize,
        );
        let buf = page.as_ptr();
        assert_eq!(&payload, &buf[start..start + len]);
    }
    #[test]
    pub fn test_insert_drop_insert() {
        let db = get_database();
        let conn = db.connect().unwrap();
        let page = get_page(2);
        let page = page.get();
        let page = page.get_contents();
        let usable_space = 4096;
        let record = ImmutableRecord::from_registers(&[
            Register::Value(Value::Integer(0)),
            Register::Value(Value::Text(Text::new("aaaaaaaa"))),
        ]);
        let _ = add_record(0, 0, page, record, &conn);
        assert_eq!(page.cell_count(), 1);
        drop_cell(page, 0, usable_space).unwrap();
        assert_eq!(page.cell_count(), 0);
        let record = ImmutableRecord::from_registers(&[Register::Value(Value::Integer(0))]);
        let payload = add_record(0, 0, page, record, &conn);
        assert_eq!(page.cell_count(), 1);
        let (start, len) = page.cell_get_raw_region(
            0,
            payload_overflow_threshold_max(page.page_type(), 4096),
            payload_overflow_threshold_min(page.page_type(), 4096),
            usable_space as usize,
        );
        let buf = page.as_ptr();
        assert_eq!(&payload, &buf[start..start + len]);
    }
    #[test]
    pub fn test_insert_drop_insert_multiple() {
        let db = get_database();
        let conn = db.connect().unwrap();
        let page = get_page(2);
        let page = page.get();
        let page = page.get_contents();
        let usable_space = 4096;
        let record = ImmutableRecord::from_registers(&[
            Register::Value(Value::Integer(0)),
            Register::Value(Value::Text(Text::new("aaaaaaaa"))),
        ]);
        let _ = add_record(0, 0, page, record, &conn);
        for _ in 0..100 {
            assert_eq!(page.cell_count(), 1);
            drop_cell(page, 0, usable_space).unwrap();
            assert_eq!(page.cell_count(), 0);
            let record = ImmutableRecord::from_registers(&[Register::Value(Value::Integer(0))]);
            let payload = add_record(0, 0, page, record, &conn);
            assert_eq!(page.cell_count(), 1);
            let (start, len) = page.cell_get_raw_region(
                0,
                payload_overflow_threshold_max(page.page_type(), 4096),
                payload_overflow_threshold_min(page.page_type(), 4096),
                usable_space as usize,
            );
            let buf = page.as_ptr();
            assert_eq!(&payload, &buf[start..start + len]);
        }
    }
    #[test]
    pub fn test_drop_a_few_insert() {
        let db = get_database();
        let conn = db.connect().unwrap();
        let page = get_page(2);
        let page = page.get();
        let page = page.get_contents();
        let usable_space = 4096;
        let record = ImmutableRecord::from_registers(&[Register::Value(Value::Integer(0))]);
        let payload = add_record(0, 0, page, record, &conn);
        let record = ImmutableRecord::from_registers(&[Register::Value(Value::Integer(1))]);
        let _ = add_record(1, 1, page, record, &conn);
        let record = ImmutableRecord::from_registers(&[Register::Value(Value::Integer(2))]);
        let _ = add_record(2, 2, page, record, &conn);
        drop_cell(page, 1, usable_space).unwrap();
        drop_cell(page, 1, usable_space).unwrap();
        ensure_cell(page, 0, &payload);
    }
    #[test]
    pub fn test_fuzz_victim_1() {
        let db = get_database();
        let conn = db.connect().unwrap();
        let page = get_page(2);
        let page = page.get();
        let page = page.get_contents();
        let usable_space = 4096;
        let record = ImmutableRecord::from_registers(&[Register::Value(Value::Integer(0))]);
        let _ = add_record(0, 0, page, record, &conn);
        let record = ImmutableRecord::from_registers(&[Register::Value(Value::Integer(0))]);
        let _ = add_record(0, 0, page, record, &conn);
        drop_cell(page, 0, usable_space).unwrap();
        defragment_page(page, usable_space);
        let record = ImmutableRecord::from_registers(&[Register::Value(Value::Integer(0))]);
        let _ = add_record(0, 1, page, record, &conn);
        drop_cell(page, 0, usable_space).unwrap();
        let record = ImmutableRecord::from_registers(&[Register::Value(Value::Integer(0))]);
        let _ = add_record(0, 1, page, record, &conn);
    }
    #[test]
    pub fn test_fuzz_victim_2() {
        let db = get_database();
        let conn = db.connect().unwrap();
        let page = get_page(2);
        let usable_space = 4096;
        let insert = |pos, page| {
            let record = ImmutableRecord::from_registers(&[Register::Value(Value::Integer(0))]);
            let _ = add_record(0, pos, page, record, &conn);
        };
        let drop = |pos, page| {
            drop_cell(page, pos, usable_space).unwrap();
        };
        let defragment = |page| {
            defragment_page(page, usable_space);
        };
        let page = page.get();
        defragment(page.get_contents());
        defragment(page.get_contents());
        insert(0, page.get_contents());
        drop(0, page.get_contents());
        insert(0, page.get_contents());
        drop(0, page.get_contents());
        insert(0, page.get_contents());
        defragment(page.get_contents());
        defragment(page.get_contents());
        drop(0, page.get_contents());
        defragment(page.get_contents());
        insert(0, page.get_contents());
        drop(0, page.get_contents());
        insert(0, page.get_contents());
        insert(1, page.get_contents());
        insert(1, page.get_contents());
        insert(0, page.get_contents());
        drop(3, page.get_contents());
        drop(2, page.get_contents());
        compute_free_space(page.get_contents(), usable_space);
    }
    #[test]
    pub fn test_fuzz_victim_3() {
        let db = get_database();
        let conn = db.connect().unwrap();
        let page = get_page(2);
        let usable_space = 4096;
        let insert = |pos, page| {
            let record = ImmutableRecord::from_registers(&[Register::Value(Value::Integer(0))]);
            let _ = add_record(0, pos, page, record, &conn);
        };
        let drop = |pos, page| {
            drop_cell(page, pos, usable_space).unwrap();
        };
        let defragment = |page| {
            defragment_page(page, usable_space);
        };
        let record = ImmutableRecord::from_registers(&[Register::Value(Value::Integer(0))]);
        let mut payload: Vec<u8> = Vec::new();
        fill_cell_payload(
            page.get().get_contents().page_type(),
            Some(0),
            &mut payload,
            &record,
            4096,
            conn.pager.clone(),
        );
        let page = page.get();
        insert(0, page.get_contents());
        defragment(page.get_contents());
        insert(0, page.get_contents());
        defragment(page.get_contents());
        insert(0, page.get_contents());
        drop(2, page.get_contents());
        drop(0, page.get_contents());
        let free = compute_free_space(page.get_contents(), usable_space);
        let total_size = payload.len() + 2;
        assert_eq!(
            free,
            usable_space - page.get_contents().header_size() as u16 - total_size as u16
        );
        dbg!(free);
    }
    #[test]
    pub fn btree_insert_sequential() {
        let (pager, root_page) = empty_btree();
        let mut keys = Vec::new();
        for i in 0..10000 {
            let mut cursor = BTreeCursor::new_table(None, pager.clone(), root_page);
            tracing::info!("INSERT INTO t VALUES ({});", i,);
            let value = ImmutableRecord::from_registers(&[Register::Value(Value::Integer(i))]);
            tracing::trace!("before insert {}", i);
            run_until_done(
                || {
                    let key = SeekKey::TableRowId(i);
                    cursor.seek(key, SeekOp::GE { eq_only: true })
                },
                pager.deref(),
            )
            .unwrap();
            run_until_done(
                || cursor.insert(&BTreeKey::new_table_rowid(i, Some(&value)), true),
                pager.deref(),
            )
            .unwrap();
            keys.push(i);
        }
        if matches!(validate_btree(pager.clone(), root_page), (_, false)) {
            panic!("invalid btree");
        }
        tracing::trace!(
            "=========== btree ===========\n{}\n\n",
            format_btree(pager.clone(), root_page, 0)
        );
        for key in keys.iter() {
            let mut cursor = BTreeCursor::new_table(None, pager.clone(), root_page);
            let key = Value::Integer(*key);
            let exists = run_until_done(|| cursor.exists(&key), pager.deref()).unwrap();
            assert!(exists, "key not found {}", key);
        }
    }
    #[test]
    pub fn test_big_payload_compute_free() {
        let db = get_database();
        let conn = db.connect().unwrap();
        let page = get_page(2);
        let usable_space = 4096;
        let record =
            ImmutableRecord::from_registers(&[Register::Value(Value::Blob(vec![0; 3600]))]);
        let mut payload: Vec<u8> = Vec::new();
        fill_cell_payload(
            page.get().get_contents().page_type(),
            Some(0),
            &mut payload,
            &record,
            4096,
            conn.pager.clone(),
        );
        insert_into_cell(page.get().get_contents(), &payload, 0, 4096).unwrap();
        let free = compute_free_space(page.get().get_contents(), usable_space);
        let total_size = payload.len() + 2;
        assert_eq!(
            free,
            usable_space - page.get().get_contents().header_size() as u16 - total_size as u16
        );
        dbg!(free);
    }
    #[test]
    pub fn test_delete_balancing() {
        let (pager, root_page) = empty_btree();
        for i in 1..=10000 {
            let mut cursor = BTreeCursor::new_table(None, pager.clone(), root_page);
            let value = ImmutableRecord::from_registers(&[Register::Value(Value::Text(
                Text::new("hello world"),
            ))]);
            run_until_done(
                || {
                    let key = SeekKey::TableRowId(i);
                    cursor.seek(key, SeekOp::GE { eq_only: true })
                },
                pager.deref(),
            )
            .unwrap();
            run_until_done(
                || cursor.insert(&BTreeKey::new_table_rowid(i, Some(&value)), true),
                pager.deref(),
            )
            .unwrap();
        }
        match validate_btree(pager.clone(), root_page) {
            (_, false) => panic!("Invalid B-tree after insertion"),
            _ => {}
        }
        for i in 500..=3500 {
            let mut cursor = BTreeCursor::new_table(None, pager.clone(), root_page);
            let seek_key = SeekKey::TableRowId(i);
            let found = run_until_done(
                || cursor.seek(seek_key.clone(), SeekOp::GE { eq_only: true }),
                pager.deref(),
            )
            .unwrap();
            if found {
                run_until_done(|| cursor.delete(), pager.deref()).unwrap();
            }
        }
        for i in 1..=10000 {
            if i >= 500 && i <= 3500 {
                continue;
            }
            let mut cursor = BTreeCursor::new_table(None, pager.clone(), root_page);
            let key = Value::Integer(i);
            let exists = run_until_done(|| cursor.exists(&key), pager.deref()).unwrap();
            assert!(exists, "Key {} should exist but doesn't", i);
        }
        for i in 500..=3500 {
            let mut cursor = BTreeCursor::new_table(None, pager.clone(), root_page);
            let key = Value::Integer(i);
            let exists = run_until_done(|| cursor.exists(&key), pager.deref()).unwrap();
            assert!(!exists, "Deleted key {} still exists", i);
        }
    }
    #[test]
    pub fn test_overflow_cells() {
        let iterations = 10_usize;
        let mut huge_texts = Vec::new();
        for i in 0..iterations {
            let mut huge_text = String::new();
            for _j in 0..8192 {
                huge_text.push((b'A' + i as u8) as char);
            }
            huge_texts.push(huge_text);
        }
        let (pager, root_page) = empty_btree();
        for i in 0..iterations {
            let mut cursor = BTreeCursor::new_table(None, pager.clone(), root_page);
            tracing::info!("INSERT INTO t VALUES ({});", i,);
            let value = ImmutableRecord::from_registers(&[Register::Value(Value::Text(Text {
                value: huge_texts[i].as_bytes().to_vec(),
                subtype: crate::types::TextSubtype::Text,
            }))]);
            tracing::trace!("before insert {}", i);
            tracing::debug!(
                "=========== btree before ===========\n{}\n\n",
                format_btree(pager.clone(), root_page, 0)
            );
            run_until_done(
                || {
                    let key = SeekKey::TableRowId(i as i64);
                    cursor.seek(key, SeekOp::GE { eq_only: true })
                },
                pager.deref(),
            )
            .unwrap();
            run_until_done(
                || cursor.insert(&BTreeKey::new_table_rowid(i as i64, Some(&value)), true),
                pager.deref(),
            )
            .unwrap();
            tracing::debug!(
                "=========== btree after ===========\n{}\n\n",
                format_btree(pager.clone(), root_page, 0)
            );
        }
        let mut cursor = BTreeCursor::new_table(None, pager.clone(), root_page);
        cursor.move_to_root();
        for i in 0..iterations {
            let has_next = run_until_done(|| cursor.next(), pager.deref()).unwrap();
            if !has_next {
                panic!("expected Some(rowid) but got {:?}", cursor.has_record.get());
            }
            let rowid = run_until_done(|| cursor.rowid(), pager.deref())
                .unwrap()
                .unwrap();
            assert_eq!(rowid, i as i64, "got!=expected");
        }
    }
    #[test]
    pub fn test_read_write_payload_with_offset() {
        let (pager, root_page) = empty_btree();
        let mut cursor = BTreeCursor::new(None, pager.clone(), root_page, vec![]);
        let offset = 2;
        let initial_text = "hello world";
        let initial_blob = initial_text.as_bytes().to_vec();
        let value =
            ImmutableRecord::from_registers(&[Register::Value(Value::Blob(initial_blob.clone()))]);
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
        let mut read_buffer = Vec::new();
        run_until_done(
            || {
                cursor.read_write_payload_with_offset(
                    offset,
                    &mut read_buffer,
                    initial_blob.len() as u32,
                    false,
                )
            },
            pager.deref(),
        )
        .unwrap();
        assert_eq!(
            std::str::from_utf8(&read_buffer).unwrap(),
            initial_text,
            "Read data doesn't match expected data"
        );
        let mut modified_hello = "olleh".as_bytes().to_vec();
        run_until_done(
            || cursor.read_write_payload_with_offset(offset, &mut modified_hello, 5, true),
            pager.deref(),
        )
        .unwrap();
        let mut verification_buffer = Vec::new();
        run_until_done(
            || {
                cursor.read_write_payload_with_offset(
                    offset,
                    &mut verification_buffer,
                    initial_blob.len() as u32,
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
    fn run_until_done<T>(
        mut action: impl FnMut() -> Result<CursorResult<T>>,
        pager: &Pager,
    ) -> Result<T> {
        loop {
            match action()? {
                CursorResult::Ok(res) => {
                    return Ok(res);
                }
                CursorResult::IO => pager.io.run_once().unwrap(),
            }
        }
    }
    #[test]
    fn test_free_array() {
        let (mut rng, seed) = rng_from_time_or_env();
        tracing::info!("seed={}", seed);
        const ITERATIONS: usize = 10000;
        for _ in 0..ITERATIONS {
            let mut cell_array = CellArray {
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
            while compute_free_space(page.get_contents(), pager.usable_space() as u16) >= size + 10
            {
                insert_cell(i, size, page.get_contents(), pager.clone(), page_type);
                i += 1;
                size = (rng.next_u64() % 1024) as u16;
            }
            let contents = page.get_contents();
            for cell_idx in 0..contents.cell_count() {
                let buf = contents.as_ptr();
                let (start, len) = contents.cell_get_raw_region(
                    cell_idx,
                    payload_overflow_threshold_max(contents.page_type(), 4096),
                    payload_overflow_threshold_min(contents.page_type(), 4096),
                    pager.usable_space(),
                );
                cell_array
                    .cells
                    .push(to_static_buf(&mut buf[start..start + len]));
                cells_cloned.push(buf[start..start + len].to_vec());
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
                let (start, len) = contents.cell_get_raw_region(
                    cell_idx,
                    payload_overflow_threshold_max(contents.page_type(), 4096),
                    payload_overflow_threshold_min(contents.page_type(), 4096),
                    pager.usable_space(),
                );
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
        let record = ImmutableRecord::from_registers(&[Register::Value(Value::Blob(vec![
            0;
            size as usize
        ]))]);
        fill_cell_payload(
            page_type,
            Some(i as i64),
            &mut payload,
            &record,
            pager.usable_space() as u16,
            pager.clone(),
        );
        insert_into_cell(contents, &payload, i as usize, pager.usable_space() as u16).unwrap();
    }
}
