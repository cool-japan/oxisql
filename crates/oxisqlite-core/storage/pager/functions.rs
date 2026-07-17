//! Auto-generated module
//!
//! 🤖 Generated with [SplitRS](https://github.com/cool-japan/splitrs)

use crate::storage::buffer_pool::BufferPool;
use crate::storage::page_cache::{DumbLruPageCache, PageCacheKey};
use crate::storage::sqlite3_ondisk::PageContent;
use crate::Buffer;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use super::type_aliases::PageRef;
use super::types::Page;

/// Rolls back a page cache slot reserved by [`Pager::read_page`] when the read
/// it was reserved for failed to even start (e.g. WAL/disk I/O submission
/// itself returned an error synchronously, before any completion callback
/// could run). Without this, such a failure would leave a permanently-locked
/// phantom page in the cache — since no completion callback will ever fire to
/// unlock it — silently breaking every future `read_page` call for that page.
pub(super) fn unreserve_cache_slot(
    page_cache: &mut DumbLruPageCache,
    key: PageCacheKey,
    page: &PageRef,
) {
    page.clear_locked();
    let _ = page_cache.delete(key);
}
pub fn allocate_page(page_id: usize, buffer_pool: &Rc<BufferPool>, offset: usize) -> PageRef {
    let page = Arc::new(Page::new(page_id));
    {
        let buffer = buffer_pool.get();
        let bp = buffer_pool.clone();
        let drop_fn = Rc::new(move |buf| {
            bp.put(buf);
        });
        let buffer = Arc::new(RefCell::new(Buffer::new(buffer, drop_fn)));
        page.set_loaded();
        page.get().contents = Some(PageContent::new(offset, buffer));
    }
    page
}

#[cfg(not(feature = "omit_autovacuum"))]
pub(super) mod ptrmap {
    use crate::{storage::sqlite3_ondisk::MIN_PAGE_SIZE, LimboError, Result};

    // Constants
    pub const PTRMAP_ENTRY_SIZE: usize = 5;
    /// Page 1 is the schema page which contains the database header.
    /// Page 2 is the first pointer map page if the database has any pointer map pages.
    pub const FIRST_PTRMAP_PAGE_NO: u32 = 2;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    #[repr(u8)]
    pub enum PtrmapType {
        RootPage = 1,
        FreePage = 2,
        Overflow1 = 3,
        Overflow2 = 4,
        BTreeNode = 5,
    }

    impl PtrmapType {
        pub fn from_u8(value: u8) -> Option<Self> {
            match value {
                1 => Some(PtrmapType::RootPage),
                2 => Some(PtrmapType::FreePage),
                3 => Some(PtrmapType::Overflow1),
                4 => Some(PtrmapType::Overflow2),
                5 => Some(PtrmapType::BTreeNode),
                _ => None,
            }
        }
    }

    #[derive(Debug, Clone, Copy)]
    pub struct PtrmapEntry {
        pub entry_type: PtrmapType,
        pub parent_page_no: u32,
    }

    impl PtrmapEntry {
        pub fn serialize(&self, buffer: &mut [u8]) -> Result<()> {
            if buffer.len() < PTRMAP_ENTRY_SIZE {
                return Err(LimboError::InternalError(format!(
                "Buffer too small to serialize ptrmap entry. Expected at least {} bytes, got {}",
                PTRMAP_ENTRY_SIZE,
                buffer.len()
            )));
            }
            buffer[0] = self.entry_type as u8;
            buffer[1..5].copy_from_slice(&self.parent_page_no.to_be_bytes());
            Ok(())
        }

        pub fn deserialize(buffer: &[u8]) -> Option<Self> {
            if buffer.len() < PTRMAP_ENTRY_SIZE {
                return None;
            }
            let entry_type_u8 = buffer[0];
            let parent_bytes_slice = buffer.get(1..5)?;
            let parent_page_no = u32::from_be_bytes(parent_bytes_slice.try_into().ok()?);
            PtrmapType::from_u8(entry_type_u8).map(|entry_type| PtrmapEntry {
                entry_type,
                parent_page_no,
            })
        }
    }

    /// Calculates how many database pages are mapped by a single pointer map page.
    /// This is based on the total page size, as ptrmap pages are filled with entries.
    pub fn entries_per_ptrmap_page(page_size: usize) -> usize {
        assert!(page_size >= MIN_PAGE_SIZE as usize);
        page_size / PTRMAP_ENTRY_SIZE
    }

    /// Calculates the cycle length of pointer map pages
    /// The cycle length is the number of database pages that are mapped by a single pointer map page.
    pub fn ptrmap_page_cycle_length(page_size: usize) -> usize {
        assert!(page_size >= MIN_PAGE_SIZE as usize);
        (page_size / PTRMAP_ENTRY_SIZE) + 1
    }

    /// Determines if a given page number `db_page_no` (1-indexed) is a pointer map page in a database with autovacuum enabled
    pub fn is_ptrmap_page(db_page_no: u32, page_size: usize) -> bool {
        //  The first page cannot be a ptrmap page because its for the schema
        if db_page_no == 1 {
            return false;
        }
        if db_page_no == FIRST_PTRMAP_PAGE_NO {
            return true;
        }
        return get_ptrmap_page_no_for_db_page(db_page_no, page_size) == db_page_no;
    }

    /// Calculates which pointer map page (1-indexed) contains the entry for `db_page_no_to_query` (1-indexed).
    /// `db_page_no_to_query` is the page whose ptrmap entry we are interested in.
    pub fn get_ptrmap_page_no_for_db_page(db_page_no_to_query: u32, page_size: usize) -> u32 {
        let group_size = ptrmap_page_cycle_length(page_size) as u32;
        if group_size == 0 {
            panic!("Page size too small, a ptrmap page cannot map any db pages.");
        }

        let effective_page_index = db_page_no_to_query - FIRST_PTRMAP_PAGE_NO;
        let group_idx = effective_page_index / group_size;

        (group_idx * group_size) + FIRST_PTRMAP_PAGE_NO
    }

    /// Calculates the byte offset of the entry for `db_page_no_to_query` (1-indexed)
    /// within its pointer map page (`ptrmap_page_no`, 1-indexed).
    pub fn get_ptrmap_offset_in_page(
        db_page_no_to_query: u32,
        ptrmap_page_no: u32,
        page_size: usize,
    ) -> Result<usize> {
        // The data pages mapped by `ptrmap_page_no` are:
        // `ptrmap_page_no + 1`, `ptrmap_page_no + 2`, ..., up to `ptrmap_page_no + n_data_pages_per_group`.
        // `db_page_no_to_query` must be one of these.
        // The 0-indexed position of `db_page_no_to_query` within this sequence of data pages is:
        // `db_page_no_to_query - (ptrmap_page_no + 1)`.

        let n_data_pages_per_group = entries_per_ptrmap_page(page_size);
        let first_data_page_mapped = ptrmap_page_no + 1;
        let last_data_page_mapped = ptrmap_page_no + n_data_pages_per_group as u32;

        if db_page_no_to_query < first_data_page_mapped
            || db_page_no_to_query > last_data_page_mapped
        {
            return Err(LimboError::InternalError(format!(
                "Page {} is not mapped by the data page range [{}, {}] of ptrmap page {}",
                db_page_no_to_query, first_data_page_mapped, last_data_page_mapped, ptrmap_page_no
            )));
        }
        if is_ptrmap_page(db_page_no_to_query, page_size) {
            return Err(LimboError::InternalError(format!(
                "Page {} is a pointer map page and should not have an entry calculated this way.",
                db_page_no_to_query
            )));
        }

        let entry_index_on_page = (db_page_no_to_query - first_data_page_mapped) as usize;
        Ok(entry_index_on_page * PTRMAP_ENTRY_SIZE)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use parking_lot::RwLock;

    use crate::storage::page_cache::{DumbLruPageCache, PageCacheKey};

    use super::Page;

    #[test]
    fn test_shared_cache() {
        // ensure cache can be shared between threads
        let cache = Arc::new(RwLock::new(DumbLruPageCache::new(10)));

        let thread = {
            let cache = cache.clone();
            std::thread::spawn(move || {
                let mut cache = cache.write();
                let page_key = PageCacheKey::new(1);
                cache.insert(page_key, Arc::new(Page::new(1))).unwrap();
            })
        };
        let _ = thread.join();
        let mut cache = cache.write();
        let page_key = PageCacheKey::new(1);
        let page = cache.get(&page_key);
        assert_eq!(page.unwrap().get().id, 1);
    }
}

#[cfg(test)]
#[cfg(not(feature = "omit_autovacuum"))]
mod ptrmap_tests {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;

    use super::ptrmap::*;
    use crate::fast_lock::SpinLock;
    use crate::io::{MemoryIO, OpenFlags, IO};
    use crate::storage::buffer_pool::BufferPool;
    use crate::storage::database::{DatabaseFile, DatabaseStorage};
    use crate::storage::page_cache::DumbLruPageCache;
    use crate::storage::pager::{AutoVacuumMode, CreateBTreeFlags, Pager};
    use crate::storage::sqlite3_ondisk::DatabaseHeader;
    use crate::storage::sqlite3_ondisk::MIN_PAGE_SIZE;
    use crate::storage::wal::{WalFile, WalFileShared};
    use crate::types::CursorResult;
    use parking_lot::RwLock;

    // Helper to create a Pager for testing
    fn test_pager_setup(page_size: u32, initial_db_pages: u32) -> Pager {
        let io: Arc<dyn IO> = Arc::new(MemoryIO::new());
        let db_file_raw = io.open_file("test.db", OpenFlags::Create, true).unwrap();
        let db_storage: Arc<dyn DatabaseStorage> = Arc::new(DatabaseFile::new(db_file_raw));

        //  Initialize a minimal header in autovacuum mode
        let mut header_data = DatabaseHeader::default();
        header_data.update_page_size(page_size);
        let db_header_arc = Arc::new(SpinLock::new(header_data));
        db_header_arc.lock().vacuum_mode_largest_root_page = 1;

        //  Construct interfaces for the pager
        let buffer_pool = Rc::new(BufferPool::new(page_size as usize));
        let page_cache = Arc::new(RwLock::new(DumbLruPageCache::new(
            (initial_db_pages + 10) as usize,
        )));

        let wal = Rc::new(RefCell::new(WalFile::new(
            io.clone(),
            page_size,
            WalFileShared::open_shared(&io, "test.db-wal", page_size).unwrap(),
            buffer_pool.clone(),
        )));

        let pager = Pager::finish_open(db_header_arc, db_storage, wal, io, page_cache, buffer_pool)
            .unwrap();
        pager.set_auto_vacuum_mode(AutoVacuumMode::Full);

        //  Allocate all the pages as btree root pages
        for _ in 0..initial_db_pages {
            match pager.btree_create(&CreateBTreeFlags::new_table()) {
                Ok(CursorResult::Ok(_root_page_id)) => (),
                Ok(CursorResult::IO) => {
                    panic!("test_pager_setup: btree_create returned CursorResult::IO unexpectedly");
                }
                Err(e) => {
                    panic!("test_pager_setup: btree_create failed: {:?}", e);
                }
            }
        }

        return pager;
    }

    #[test]
    fn test_ptrmap_page_allocation() {
        let page_size = 4096;
        let initial_db_pages = 10;
        let pager = test_pager_setup(page_size, initial_db_pages);

        // Page 5 should be mapped by ptrmap page 2.
        let db_page_to_update: u32 = 5;
        let expected_ptrmap_pg_no =
            get_ptrmap_page_no_for_db_page(db_page_to_update, page_size as usize);
        assert_eq!(expected_ptrmap_pg_no, FIRST_PTRMAP_PAGE_NO);

        //  Ensure the pointer map page ref is created and loadable via the pager
        let ptrmap_page_ref = pager.read_page(expected_ptrmap_pg_no as usize);
        assert!(ptrmap_page_ref.is_ok());

        //  Ensure that the database header size is correctly reflected
        assert_eq!(pager.db_header.lock().database_size, initial_db_pages + 2); // (1+1) -> (header + ptrmap)

        //  Read the entry from the ptrmap page and verify it
        let entry = pager.ptrmap_get(db_page_to_update).unwrap();
        assert!(matches!(entry, CursorResult::Ok(Some(_))));
        let CursorResult::Ok(Some(entry)) = entry else {
            panic!("entry is not Some");
        };
        assert_eq!(entry.entry_type, PtrmapType::RootPage);
        assert_eq!(entry.parent_page_no, 0);
    }

    #[test]
    fn test_is_ptrmap_page_logic() {
        let page_size = MIN_PAGE_SIZE as usize;
        let n_data_pages = entries_per_ptrmap_page(page_size);
        assert_eq!(n_data_pages, 102); //   512/5 = 102

        assert!(!is_ptrmap_page(1, page_size)); // Header
        assert!(is_ptrmap_page(2, page_size)); // P0
        assert!(!is_ptrmap_page(3, page_size)); // D0_1
        assert!(!is_ptrmap_page(4, page_size)); // D0_2
        assert!(!is_ptrmap_page(5, page_size)); // D0_3
        assert!(is_ptrmap_page(105, page_size)); // P1
        assert!(!is_ptrmap_page(106, page_size)); // D1_1
        assert!(!is_ptrmap_page(107, page_size)); // D1_2
        assert!(!is_ptrmap_page(108, page_size)); // D1_3
        assert!(is_ptrmap_page(208, page_size)); // P2
    }

    #[test]
    fn test_get_ptrmap_page_no() {
        let page_size = MIN_PAGE_SIZE as usize; // Maps 103 data pages

        // Test pages mapped by P0 (page 2)
        assert_eq!(get_ptrmap_page_no_for_db_page(3, page_size), 2); // D(3) -> P0(2)
        assert_eq!(get_ptrmap_page_no_for_db_page(4, page_size), 2); // D(4) -> P0(2)
        assert_eq!(get_ptrmap_page_no_for_db_page(5, page_size), 2); // D(5) -> P0(2)
        assert_eq!(get_ptrmap_page_no_for_db_page(104, page_size), 2); // D(104) -> P0(2)

        assert_eq!(get_ptrmap_page_no_for_db_page(105, page_size), 105); // Page 105 is a pointer map page.

        // Test pages mapped by P1 (page 6)
        assert_eq!(get_ptrmap_page_no_for_db_page(106, page_size), 105); // D(106) -> P1(105)
        assert_eq!(get_ptrmap_page_no_for_db_page(107, page_size), 105); // D(107) -> P1(105)
        assert_eq!(get_ptrmap_page_no_for_db_page(108, page_size), 105); // D(108) -> P1(105)

        assert_eq!(get_ptrmap_page_no_for_db_page(208, page_size), 208); // Page 208 is a pointer map page.
    }

    #[test]
    fn test_get_ptrmap_offset() {
        let page_size = MIN_PAGE_SIZE as usize; //  Maps 103 data pages

        assert_eq!(get_ptrmap_offset_in_page(3, 2, page_size).unwrap(), 0);
        assert_eq!(
            get_ptrmap_offset_in_page(4, 2, page_size).unwrap(),
            1 * PTRMAP_ENTRY_SIZE
        );
        assert_eq!(
            get_ptrmap_offset_in_page(5, 2, page_size).unwrap(),
            2 * PTRMAP_ENTRY_SIZE
        );

        //  P1 (page 105) maps D(106)...D(207)
        // D(106) is index 0 on P1. Offset 0.
        // D(107) is index 1 on P1. Offset 5.
        // D(108) is index 2 on P1. Offset 10.
        assert_eq!(get_ptrmap_offset_in_page(106, 105, page_size).unwrap(), 0);
        assert_eq!(
            get_ptrmap_offset_in_page(107, 105, page_size).unwrap(),
            1 * PTRMAP_ENTRY_SIZE
        );
        assert_eq!(
            get_ptrmap_offset_in_page(108, 105, page_size).unwrap(),
            2 * PTRMAP_ENTRY_SIZE
        );
    }
}

/// Tests for the "no room in page cache" fix (wave-ac-7-pager): a full page
/// cache must surface as a clean `Err(LimboError::CacheFull)` from every
/// allocation path, without ever bumping `header.database_size` on that
/// error path, and a checkpoint must evict only the pages it actually
/// backfilled rather than the entire cache.
#[cfg(test)]
mod cache_full_tests {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;

    use parking_lot::RwLock;

    use crate::fast_lock::SpinLock;
    use crate::io::{MemoryIO, OpenFlags, IO};
    use crate::storage::buffer_pool::BufferPool;
    use crate::storage::database::{DatabaseFile, DatabaseStorage};
    use crate::storage::page_cache::{DumbLruPageCache, PageCacheKey};
    use crate::storage::pager::{BtreePageAllocMode, Pager};
    use crate::storage::sqlite3_ondisk::{DatabaseHeader, PageType, DATABASE_HEADER_PAGE_ID};
    use crate::storage::wal::{WalFile, WalFileShared};
    use crate::LimboError;

    /// Build a `Pager` backed by a fresh in-memory file, with its page cache
    /// capped at `cache_capacity` pages, for deterministically exercising
    /// `CacheError::Full` behavior.
    fn test_pager_with_small_cache(cache_capacity: usize) -> Pager {
        let page_size: u32 = 4096;
        let io: Arc<dyn IO> = Arc::new(MemoryIO::new());
        let db_file_raw = io
            .open_file("test_cache_full.db", OpenFlags::Create, true)
            .unwrap();
        let db_storage: Arc<dyn DatabaseStorage> = Arc::new(DatabaseFile::new(db_file_raw));

        let mut header_data = DatabaseHeader::default();
        header_data.update_page_size(page_size);
        let db_header_arc = Arc::new(SpinLock::new(header_data));

        let buffer_pool = Rc::new(BufferPool::new(page_size as usize));
        let page_cache = Arc::new(RwLock::new(DumbLruPageCache::new(cache_capacity)));

        let wal = Rc::new(RefCell::new(WalFile::new(
            io.clone(),
            page_size,
            WalFileShared::open_shared(&io, "test_cache_full.db-wal", page_size).unwrap(),
            buffer_pool.clone(),
        )));

        Pager::finish_open(db_header_arc, db_storage, wal, io, page_cache, buffer_pool).unwrap()
    }

    #[test]
    fn test_allocate_page_cache_full_is_clean_error() {
        // Capacity 2 is exactly enough for one successful `allocate_page()`
        // call: the freshly allocated page itself, plus page 1 (the header),
        // which `allocate_page` caches (and dirties) as a side effect of
        // persisting the new `database_size` via `write_database_header`.
        let pager = test_pager_with_small_cache(2);

        let first = pager
            .allocate_page()
            .expect("first allocation must succeed: cache has exactly enough room");
        assert_eq!(first.get().id, 2);

        let database_size_before = pager.db_header.lock().database_size;
        assert_eq!(database_size_before, 2);

        // The cache is now full (page 1 + page 2, both dirty and therefore
        // unevictable). A plain page allocation must fail cleanly rather than
        // panic, and must not bump `database_size`.
        let err = pager
            .allocate_page()
            .expect_err("cache is full: allocate_page must return Err, not panic");
        assert!(
            matches!(err, LimboError::CacheFull),
            "expected LimboError::CacheFull, got {err:?}"
        );
        assert_eq!(
            pager.db_header.lock().database_size,
            database_size_before,
            "database_size must not be bumped when allocate_page hits a full cache"
        );

        // Likewise for the overflow-page allocation path used when a cell's
        // payload doesn't fit locally (large TEXT/BLOB values).
        let err = pager
            .allocate_overflow_page()
            .expect_err("cache is full: allocate_overflow_page must return Err, not panic");
        assert!(
            matches!(err, LimboError::CacheFull),
            "expected LimboError::CacheFull, got {err:?}"
        );
        assert_eq!(
            pager.db_header.lock().database_size,
            database_size_before,
            "database_size must not be bumped when allocate_overflow_page hits a full cache"
        );

        // And the btree-level allocator both of the above (and `btree_create`)
        // are built on. `BTreePage` (`Arc<BTreePageInner>`) isn't `Debug`, so
        // match directly instead of using `expect_err`.
        let err = match pager.do_allocate_page(PageType::TableLeaf, 0, BtreePageAllocMode::Any) {
            Err(e) => e,
            Ok(_) => panic!("cache is full: do_allocate_page must return Err, not panic"),
        };
        assert!(
            matches!(err, LimboError::CacheFull),
            "expected LimboError::CacheFull, got {err:?}"
        );
        assert_eq!(
            pager.db_header.lock().database_size,
            database_size_before,
            "database_size must not be bumped when do_allocate_page hits a full cache"
        );
    }

    #[test]
    fn test_checkpoint_evicts_only_touched_pages() {
        // Ample capacity: no incidental eviction pressure from the cache
        // itself, so any eviction we observe is attributable to the
        // checkpoint's own logic.
        let pager = test_pager_with_small_cache(50);

        // A handful of ordinary pages that WILL be checkpointed: allocate
        // them, then commit them as WAL frames directly via the WAL's own API
        // (bypassing `Pager::cacheflush`, whose unconditional page-cache
        // clear in `FlushState::Start` is a separate, pre-existing,
        // out-of-scope behavior that would otherwise make it impossible to
        // isolate `wal_checkpoint`'s own eviction logic in this test).
        //
        // Page 1 (the header) is deliberately tracked separately from these:
        // every `allocate_page()` call -- including the "untouched" one below
        // -- persists the new `database_size` via `write_database_header`,
        // which unconditionally marks page 1 dirty again each time. So by the
        // time the checkpoint runs, page 1 has legitimately been re-dirtied
        // since it was committed here, and the eviction logic is correct to
        // leave it cached rather than evict it (see the second assertion
        // block below) -- this isn't a gap in the fix, it's the same
        // "don't evict a page that's dirty again" safety property that
        // protects `untouched_page`.
        let mut ordinary_ids: Vec<u64> = Vec::new();
        for _ in 0..5 {
            let page = pager.allocate_page().unwrap();
            ordinary_ids.push(page.get().id as u64);
        }
        let mut committed_ids = ordinary_ids.clone();
        committed_ids.push(DATABASE_HEADER_PAGE_ID as u64);
        {
            // Computed after the allocations above so the recorded db_size
            // matches reality, mirroring how `cacheflush` does it.
            let db_size = pager.db_header.lock().database_size;
            for &id in &committed_ids {
                let key = PageCacheKey::new(id as usize);
                let page = pager
                    .page_cache
                    .write()
                    .get(&key)
                    .expect("page must be cached after allocate_page");
                pager
                    .wal
                    .borrow_mut()
                    .append_frame(page.clone(), db_size, Rc::new(RefCell::new(0)))
                    .unwrap();
                page.clear_dirty();
            }
        }

        // A page that is cached but carries NO WAL frame at all: allocated
        // after the pages above were committed, and never appended. It stays
        // dirty, exactly like a genuinely pending write that just hasn't been
        // flushed yet. (As noted above, allocating it also re-dirties page 1
        // as a side effect of persisting the new `database_size`.)
        let untouched_page = pager.allocate_page().unwrap();
        let untouched_id = untouched_page.get().id;
        assert!(pager
            .page_cache
            .write()
            .contains_key(&PageCacheKey::new(untouched_id)));

        let result = pager.wal_checkpoint();

        // The checkpoint must report exactly the pages we actually gave it
        // frames for.
        for &id in &committed_ids {
            assert!(
                result.checkpointed_page_ids.contains(&id),
                "expected page {id} to be reported as checkpointed"
            );
        }
        assert!(
            !result
                .checkpointed_page_ids
                .contains(&(untouched_id as u64)),
            "the untouched page carries no WAL frame and must not be reported as checkpointed"
        );

        // The ordinary pages that WERE checkpointed (and are still clean --
        // nothing re-dirtied them afterward) should actually have been
        // evicted, proving the eviction isn't a no-op.
        for &id in &ordinary_ids {
            assert!(
                !pager
                    .page_cache
                    .write()
                    .contains_key(&PageCacheKey::new(id as usize)),
                "page {id} was checkpointed and clean, and should have been evicted"
            );
        }

        // The key regression check: a page the checkpoint never touched must
        // still be cached afterward. Before this fix, `wal_checkpoint`
        // unconditionally cleared the ENTIRE page cache on every call (and
        // would have panicked outright here, since `untouched_page` is still
        // dirty).
        assert!(
            pager
                .page_cache
                .write()
                .contains_key(&PageCacheKey::new(untouched_id)),
            "checkpoint must not evict pages it did not itself just back-fill"
        );

        // A softer companion regression check: page 1 was checkpointed but
        // has since been dirtied again (by the `untouched_page` allocation).
        // It must be left alone rather than evicted -- the same safety
        // property, on a page that genuinely was part of this checkpoint.
        assert!(
            pager
                .page_cache
                .write()
                .contains_key(&PageCacheKey::new(DATABASE_HEADER_PAGE_ID)),
            "a checkpointed page that became dirty again afterward must not be evicted"
        );
    }
}
