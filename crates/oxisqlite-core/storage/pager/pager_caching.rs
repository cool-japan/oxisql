//! # `Pager` - caching Methods
//!
//! This module contains method implementations for `Pager`.
//!
//! 🤖 Generated with [SplitRS](https://github.com/cool-japan/splitrs)

use crate::fast_lock::SpinLock;
use crate::result::LimboResult;
use crate::storage::btree::BTreePageInner;
use crate::storage::buffer_pool::BufferPool;
use crate::storage::database::DatabaseStorage;
use crate::storage::sqlite3_ondisk::{
    self, DatabaseHeader, PageType, DATABASE_HEADER_PAGE_ID, DATABASE_HEADER_SIZE,
};
// Only used by `ptrmap_get`, which is compiled out entirely under
// `omit_autovacuum`.
use crate::storage::btree::BTreePage;
use crate::storage::page_cache::{CacheError, CacheResizeResult, DumbLruPageCache, PageCacheKey};
#[cfg(not(feature = "omit_autovacuum"))]
use crate::storage::sqlite3_ondisk::PageContent;
use crate::storage::wal::Wal;
use crate::types::CursorResult;
use crate::{LimboError, Result};
use parking_lot::RwLock;
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;
#[cfg(not(feature = "omit_autovacuum"))]
use {super::functions::ptrmap::*, crate::io::Buffer as IoBuffer};

use super::functions::unreserve_cache_slot;
use super::type_aliases::PageRef;
use super::types::{
    AutoVacuumMode, BtreePageAllocMode, CheckpointState, CreateBTreeFlags, FlushInfo, FlushState,
    Page, PagerCacheflushStatus, SynchronousMode,
};

use super::pager_type::Pager;

impl Pager {
    /// Begins opening a database by reading the database header.
    pub fn begin_open(db_file: Arc<dyn DatabaseStorage>) -> Result<Arc<SpinLock<DatabaseHeader>>> {
        sqlite3_ondisk::begin_read_database_header(db_file)
    }

    /// Completes opening a database by initializing the Pager with the database header.
    pub fn finish_open(
        db_header_ref: Arc<SpinLock<DatabaseHeader>>,
        db_file: Arc<dyn DatabaseStorage>,
        wal: Rc<RefCell<dyn Wal>>,
        io: Arc<dyn crate::io::IO>,
        page_cache: Arc<RwLock<DumbLruPageCache>>,
        buffer_pool: Rc<BufferPool>,
    ) -> Result<Self> {
        Ok(Self {
            db_file,
            wal,
            page_cache,
            io,
            dirty_pages: Rc::new(RefCell::new(HashSet::new())),
            db_header: db_header_ref.clone(),
            flush_info: RefCell::new(FlushInfo {
                state: FlushState::Start,
                in_flight_writes: Rc::new(RefCell::new(0)),
            }),
            syncing: Rc::new(RefCell::new(false)),
            checkpoint_state: RefCell::new(CheckpointState::Checkpoint),
            checkpoint_inflight: Rc::new(RefCell::new(0)),
            buffer_pool,
            auto_vacuum_mode: RefCell::new(AutoVacuumMode::None),
            synchronous_mode: Cell::new(SynchronousMode::Normal),
            savepoints: RefCell::new(Vec::new()),
        })
    }

    pub fn get_auto_vacuum_mode(&self) -> AutoVacuumMode {
        *self.auto_vacuum_mode.borrow()
    }

    pub fn set_auto_vacuum_mode(&self, mode: AutoVacuumMode) {
        *self.auto_vacuum_mode.borrow_mut() = mode;
    }

    pub fn get_synchronous_mode(&self) -> SynchronousMode {
        self.synchronous_mode.get()
    }

    pub fn set_synchronous_mode(&self, mode: SynchronousMode) {
        self.synchronous_mode.set(mode);
    }

    /// Retrieves the pointer map entry for a given database page.
    /// `target_page_num` (1-indexed) is the page whose entry is sought.
    /// Returns `Ok(None)` if the page is not supposed to have a ptrmap entry (e.g. header, or a ptrmap page itself).
    #[cfg(not(feature = "omit_autovacuum"))]
    pub fn ptrmap_get(&self, target_page_num: u32) -> Result<CursorResult<Option<PtrmapEntry>>> {
        tracing::trace!("ptrmap_get(page_idx = {})", target_page_num);
        let configured_page_size = self.db_header.lock().get_page_size() as usize;

        if target_page_num < FIRST_PTRMAP_PAGE_NO
            || is_ptrmap_page(target_page_num, configured_page_size)
        {
            return Ok(CursorResult::Ok(None));
        }

        let ptrmap_pg_no = get_ptrmap_page_no_for_db_page(target_page_num, configured_page_size);
        let offset_in_ptrmap_page =
            get_ptrmap_offset_in_page(target_page_num, ptrmap_pg_no, configured_page_size)?;
        tracing::trace!(
            "ptrmap_get(page_idx = {}) = ptrmap_pg_no = {}",
            target_page_num,
            ptrmap_pg_no
        );

        let ptrmap_page = self.read_page(ptrmap_pg_no as usize)?;
        if ptrmap_page.is_locked() {
            return Ok(CursorResult::IO);
        }
        if !ptrmap_page.is_loaded() {
            return Ok(CursorResult::IO);
        }
        let ptrmap_page_inner = ptrmap_page.get();

        let page_content: &PageContent = match ptrmap_page_inner.contents.as_ref() {
            Some(content) => content,
            None => {
                return Err(LimboError::InternalError(format!(
                    "Ptrmap page {} content not loaded",
                    ptrmap_pg_no
                )))
            }
        };

        let page_buffer_guard: std::cell::Ref<IoBuffer> = page_content.buffer.borrow();
        let full_buffer_slice: &[u8] = page_buffer_guard.as_slice();

        // Ptrmap pages are not page 1, so their internal offset within their buffer should be 0.
        // The actual page data starts at page_content.offset within the full_buffer_slice.
        if ptrmap_pg_no != 1 && page_content.offset != 0 {
            return Err(LimboError::Corrupt(format!(
                "Ptrmap page {} has unexpected internal offset {}",
                ptrmap_pg_no, page_content.offset
            )));
        }
        let ptrmap_page_data_slice: &[u8] = &full_buffer_slice[page_content.offset..];
        let actual_data_length = ptrmap_page_data_slice.len();

        // Check if the calculated offset for the entry is within the bounds of the actual page data length.
        if offset_in_ptrmap_page + PTRMAP_ENTRY_SIZE > actual_data_length {
            return Err(LimboError::InternalError(format!(
                "Ptrmap offset {} + entry size {} out of bounds for page {} (actual data len {})",
                offset_in_ptrmap_page, PTRMAP_ENTRY_SIZE, ptrmap_pg_no, actual_data_length
            )));
        }

        let entry_slice = &ptrmap_page_data_slice
            [offset_in_ptrmap_page..offset_in_ptrmap_page + PTRMAP_ENTRY_SIZE];
        match PtrmapEntry::deserialize(entry_slice) {
            Some(entry) => Ok(CursorResult::Ok(Some(entry))),
            None => Err(LimboError::Corrupt(format!(
                "Failed to deserialize ptrmap entry for page {} from ptrmap page {}",
                target_page_num, ptrmap_pg_no
            ))),
        }
    }

    /// Writes or updates the pointer map entry for a given database page.
    /// `db_page_no_to_update` (1-indexed) is the page whose entry is to be set.
    /// `entry_type` and `parent_page_no` define the new entry.
    #[cfg(not(feature = "omit_autovacuum"))]
    pub fn ptrmap_put(
        &self,
        db_page_no_to_update: u32,
        entry_type: PtrmapType,
        parent_page_no: u32,
    ) -> Result<CursorResult<()>> {
        tracing::trace!(
            "ptrmap_put(page_idx = {}, entry_type = {:?}, parent_page_no = {})",
            db_page_no_to_update,
            entry_type,
            parent_page_no
        );

        let page_size = self.db_header.lock().get_page_size() as usize;

        if db_page_no_to_update < FIRST_PTRMAP_PAGE_NO
            || is_ptrmap_page(db_page_no_to_update, page_size)
        {
            return Err(LimboError::InternalError(format!(
                "Cannot set ptrmap entry for page {}: it's a header/ptrmap page or invalid.",
                db_page_no_to_update
            )));
        }

        let ptrmap_pg_no = get_ptrmap_page_no_for_db_page(db_page_no_to_update, page_size);
        let offset_in_ptrmap_page =
            get_ptrmap_offset_in_page(db_page_no_to_update, ptrmap_pg_no, page_size)?;
        tracing::trace!(
            "ptrmap_put(page_idx = {}, entry_type = {:?}, parent_page_no = {}) = ptrmap_pg_no = {}, offset_in_ptrmap_page = {}",
            db_page_no_to_update,
            entry_type,
            parent_page_no,
            ptrmap_pg_no,
            offset_in_ptrmap_page
        );

        let ptrmap_page = self.read_page(ptrmap_pg_no as usize)?;
        if ptrmap_page.is_locked() {
            return Ok(CursorResult::IO);
        }
        if !ptrmap_page.is_loaded() {
            return Ok(CursorResult::IO);
        }
        let ptrmap_page_inner = ptrmap_page.get();

        let page_content = match ptrmap_page_inner.contents.as_ref() {
            Some(content) => content,
            None => {
                return Err(LimboError::InternalError(format!(
                    "Ptrmap page {} content not loaded",
                    ptrmap_pg_no
                )))
            }
        };

        let mut page_buffer_guard = page_content.buffer.borrow_mut();
        let full_buffer_slice = page_buffer_guard.as_mut_slice();

        if offset_in_ptrmap_page + PTRMAP_ENTRY_SIZE > full_buffer_slice.len() {
            return Err(LimboError::InternalError(format!(
                "Ptrmap offset {} + entry size {} out of bounds for page {} (actual data len {})",
                offset_in_ptrmap_page,
                PTRMAP_ENTRY_SIZE,
                ptrmap_pg_no,
                full_buffer_slice.len()
            )));
        }

        let entry = PtrmapEntry {
            entry_type,
            parent_page_no,
        };
        entry.serialize(
            &mut full_buffer_slice
                [offset_in_ptrmap_page..offset_in_ptrmap_page + PTRMAP_ENTRY_SIZE],
        )?;

        ptrmap_page.set_dirty();
        self.add_dirty(ptrmap_pg_no as usize);
        Ok(CursorResult::Ok(()))
    }

    /// This method is used to allocate a new root page for a btree, both for tables and indexes
    pub fn btree_create(&self, flags: &CreateBTreeFlags) -> Result<CursorResult<u32>> {
        let page_type = match flags {
            _ if flags.is_table() => PageType::TableLeaf,
            _ if flags.is_index() => PageType::IndexLeaf,
            _ => unreachable!("Invalid flags state"),
        };
        #[cfg(feature = "omit_autovacuum")]
        {
            let page = self.do_allocate_page(page_type, 0, BtreePageAllocMode::Any)?;
            let page_id = page.get().get().id;
            return Ok(CursorResult::Ok(page_id as u32));
        }

        //  If autovacuum is enabled, we need to allocate a new page number that is greater than the largest root page number
        #[cfg(not(feature = "omit_autovacuum"))]
        {
            let auto_vacuum_mode = self.auto_vacuum_mode.borrow();
            match *auto_vacuum_mode {
                AutoVacuumMode::None => {
                    let page = self.do_allocate_page(page_type, 0, BtreePageAllocMode::Any)?;
                    let page_id = page.get().get().id;
                    return Ok(CursorResult::Ok(page_id as u32));
                }
                AutoVacuumMode::Full => {
                    let mut root_page_num = self.db_header.lock().vacuum_mode_largest_root_page;
                    assert!(root_page_num > 0); //  Largest root page number cannot be 0 because that is set to 1 when creating the database with autovacuum enabled
                    root_page_num += 1;
                    assert!(root_page_num >= FIRST_PTRMAP_PAGE_NO); //  can never be less than 2 because we have already incremented

                    while is_ptrmap_page(
                        root_page_num,
                        self.db_header.lock().get_page_size() as usize,
                    ) {
                        root_page_num += 1;
                    }
                    assert!(root_page_num >= 3); //  the very first root page is page 3

                    //  root_page_num here is the desired (canonical) root page: real SQLite
                    //  requires root pages to be the lowest-numbered pages in the file so any
                    //  page beyond the largest known root can always be relocated to make room
                    //  for a new one (see `relocatePage` in SQLite's btree.c).
                    let page = self.do_allocate_page(
                        page_type,
                        0,
                        BtreePageAllocMode::Exact(root_page_num),
                    )?;
                    let allocated_page_id = page.get().get().id as u32;
                    if allocated_page_id != root_page_num {
                        //  KNOWN LIMITATION: `do_allocate_page` ignores `BtreePageAllocMode`
                        //  entirely and always allocates the next sequential page, so this is
                        //  the common case as soon as any data has been written (not just a
                        //  rare fragmentation edge case). Real SQLite would relocate whatever
                        //  page currently occupies `root_page_num` to `allocated_page_id` and
                        //  fix up the single pointer that referenced it (found via that page's
                        //  ptrmap entry), freeing the canonical slot for this new root.
                        //
                        //  We deliberately do NOT attempt that here: this crate's ptrmap
                        //  bookkeeping is only ever populated for ROOT pages (the `ptrmap_put`
                        //  call below is the only call site outside tests) — ordinary b-tree
                        //  interior/leaf pages and overflow pages never get a ptrmap entry
                        //  written when `do_allocate_page`/`allocate_overflow_page` create them
                        //  during inserts/splits. That means `ptrmap_get(root_page_num)` cannot
                        //  be trusted to say what currently occupies that slot or what points to
                        //  it — freshly allocated pages are not zero-initialized, so a "read" of
                        //  a never-written ptrmap entry can just as easily return
                        //  `LimboError::Corrupt` (undecodable byte) as it can a plausible-looking
                        //  but *wrong* entry. Relocating content on the strength of that data
                        //  would risk rewriting an unrelated page's pointer and silently
                        //  corrupting a different table. Until ptrmap entries are maintained for
                        //  every page (not just roots), we keep this fallback: use whatever page
                        //  the allocator actually returned as the root. The "roots are
                        //  compact/lowest-numbered" auto-vacuum invariant is therefore not
                        //  maintained, but every table/index remains fully readable and
                        //  writable, and no incremental/full VACUUM implementation exists yet to
                        //  depend on that invariant.
                    }
                    let final_root_page_id = allocated_page_id;

                    let ptrmap_result =
                        self.ptrmap_put(final_root_page_id, PtrmapType::RootPage, 0)?;
                    if matches!(ptrmap_result, CursorResult::IO) {
                        return Ok(CursorResult::IO);
                    }

                    // Track the high-water mark of root pages actually used (regardless of
                    // whether we landed on the canonical slot) so the next CREATE TABLE/INDEX
                    // computes a slot that doesn't collide with this one.
                    {
                        let mut header_guard = self.db_header.lock();
                        if final_root_page_id > header_guard.vacuum_mode_largest_root_page {
                            header_guard.vacuum_mode_largest_root_page = final_root_page_id;
                            self.write_database_header(&header_guard)?;
                        }
                    }

                    Ok(CursorResult::Ok(final_root_page_id))
                }
                AutoVacuumMode::Incremental => {
                    unimplemented!()
                }
            }
        }
    }

    /// Allocate a new overflow page.
    /// This is done when a cell overflows and new space is needed.
    ///
    /// # Errors
    /// Propagates [`LimboError::CacheFull`] from [`Self::allocate_page`] when the
    /// page cache has no room for the new page. The caller must not have made
    /// any on-disk-visible changes yet that depend on this call succeeding.
    pub fn allocate_overflow_page(&self) -> Result<PageRef> {
        let page = self.allocate_page()?;
        tracing::debug!("Pager::allocate_overflow_page(id={})", page.get().id);

        // setup overflow page
        let contents = page.get().contents.as_mut().unwrap();
        let buf = contents.as_ptr();
        buf.fill(0);

        Ok(page)
    }

    /// Allocate a new page to the btree via the pager.
    /// This marks the page as dirty and writes the page header.
    ///
    /// # Errors
    /// Propagates [`LimboError::CacheFull`] from [`Self::allocate_page`] when the
    /// page cache has no room for the new page.
    pub fn do_allocate_page(
        &self,
        page_type: PageType,
        offset: usize,
        _alloc_mode: BtreePageAllocMode,
    ) -> Result<BTreePage> {
        let page = self.allocate_page()?;
        let page = Arc::new(BTreePageInner {
            page: RefCell::new(page),
        });
        crate::btree_init_page(&page, page_type, offset, self.usable_space() as u16);
        tracing::debug!(
            "do_allocate_page(id={}, page_type={:?})",
            page.get().get().id,
            page.get().get_contents().page_type()
        );
        Ok(page)
    }

    /// The "usable size" of a database page is the page size specified by the 2-byte integer at offset 16
    /// in the header, minus the "reserved" space size recorded in the 1-byte integer at offset 20 in the header.
    /// The usable size of a page might be an odd number. However, the usable size is not allowed to be less than 480.
    /// In other words, if the page size is 512, then the reserved space size cannot exceed 32.
    pub fn usable_space(&self) -> usize {
        let db_header = self.db_header.lock();
        (db_header.get_page_size() - db_header.reserved_space as u32) as usize
    }

    #[inline(always)]
    pub fn begin_read_tx(&self) -> Result<LimboResult> {
        self.wal.borrow_mut().begin_read_tx()
    }

    #[inline(always)]
    pub fn begin_write_tx(&self) -> Result<LimboResult> {
        self.wal.borrow_mut().begin_write_tx()
    }

    pub fn end_tx(&self) -> Result<PagerCacheflushStatus> {
        let cacheflush_status = self.cacheflush()?;
        return match cacheflush_status {
            PagerCacheflushStatus::IO => Ok(PagerCacheflushStatus::IO),
            PagerCacheflushStatus::Done(_) => {
                self.wal.borrow().end_write_tx()?;
                self.wal.borrow().end_read_tx()?;
                // Clear any remaining savepoints after the transaction commits.
                self.savepoints.borrow_mut().clear();
                Ok(cacheflush_status)
            }
        };
    }

    pub fn end_read_tx(&self) -> Result<()> {
        self.wal.borrow().end_read_tx()?;
        Ok(())
    }

    /// Reads a page from the database.
    pub fn read_page(&self, page_idx: usize) -> Result<PageRef, LimboError> {
        tracing::trace!("read_page(page_idx = {})", page_idx);
        let mut page_cache = self.page_cache.write();
        let page_key = PageCacheKey::new(page_idx);
        if let Some(page) = page_cache.get(&page_key) {
            tracing::trace!("read_page(page_idx = {}) = cached", page_idx);
            return Ok(page.clone());
        }
        let page = Arc::new(Page::new(page_idx));
        page.set_locked();

        // Reserve the cache slot BEFORE performing any WAL/disk I/O, so a full
        // cache fails fast here instead of after a (potentially expensive) read
        // whose result we'd then have nowhere to put.
        match page_cache.insert(page_key.clone(), page.clone()) {
            Ok(_) => {}
            Err(CacheError::Full) => return Err(LimboError::CacheFull),
            Err(CacheError::KeyExists) => {
                unreachable!("Page should not exist in cache after get() miss")
            }
            Err(e) => {
                return Err(LimboError::InternalError(format!(
                    "Failed to insert page into cache: {:?}",
                    e
                )))
            }
        }

        match self.wal.borrow().find_frame(page_idx as u64) {
            Ok(Some(frame_id)) => {
                if let Err(e) =
                    self.wal
                        .borrow()
                        .read_frame(frame_id, page.clone(), self.buffer_pool.clone())
                {
                    unreserve_cache_slot(&mut page_cache, page_key, &page);
                    return Err(e);
                }
                page.set_uptodate();
                return Ok(page);
            }
            Ok(None) => {}
            Err(e) => {
                unreserve_cache_slot(&mut page_cache, page_key, &page);
                return Err(e);
            }
        }

        if let Err(e) = sqlite3_ondisk::begin_read_page(
            self.db_file.clone(),
            self.buffer_pool.clone(),
            page.clone(),
            page_idx,
        ) {
            unreserve_cache_slot(&mut page_cache, page_key, &page);
            return Err(e);
        }
        Ok(page)
    }

    /// Writes the database header.
    ///
    /// This is a blocking convenience wrapper: it drives
    /// `write_database_header_step` (private single-step helper) to
    /// completion, running I/O as needed. Mirrors the same loop-until-done
    /// pattern already used by
    /// [`Self::wal_checkpoint_mode`] for other one-shot, run-to-completion
    /// pager operations.
    pub fn write_database_header(&self, header: &DatabaseHeader) -> Result<()> {
        loop {
            match self.write_database_header_step(header)? {
                CursorResult::Ok(()) => return Ok(()),
                CursorResult::IO => self.io.run_once()?,
            }
        }
    }

    /// Single-step, non-blocking form of [`Self::write_database_header`].
    ///
    /// Returns `CursorResult::IO` (instead of internally busy-looping on
    /// `self.io.run_once()`, as this used to do) when the header page is
    /// still locked by an in-flight read, mirroring the same
    /// check-once-and-yield pattern already used by [`Self::ptrmap_get`] /
    /// [`Self::ptrmap_put`] in this file. Safe to call again after driving IO
    /// forward: re-entry just re-checks `read_page`'s (idempotent) result.
    fn write_database_header_step(&self, header: &DatabaseHeader) -> Result<CursorResult<()>> {
        let header_page = self.read_page(DATABASE_HEADER_PAGE_ID)?;
        if header_page.is_locked() {
            return Ok(CursorResult::IO);
        }
        header_page.set_dirty();
        self.add_dirty(DATABASE_HEADER_PAGE_ID);

        let contents = header_page.get().contents.as_ref().ok_or_else(|| {
            LimboError::InternalError(
                "page 1 contents missing while writing database header".to_string(),
            )
        })?;
        contents.write_database_header(header);

        Ok(CursorResult::Ok(()))
    }

    /// Refresh the shared in-memory database header from the write-ahead log.
    ///
    /// The header occupies the first 100 bytes of page 1. At open time the
    /// header is read straight from the main database file (see
    /// [`Pager::begin_open`] / `sqlite3_ondisk::begin_read_database_header`),
    /// which deliberately bypasses the WAL. Every *other* page is read through
    /// [`Pager::read_page`], which consults the WAL via
    /// [`crate::storage::wal::Wal::find_frame`]. That asymmetry means a page-1
    /// change that has been committed to the WAL but not yet checkpointed back
    /// into the main file (the normal state after a non-checkpointing close)
    /// is invisible to the header — so cookies written via `PRAGMA
    /// application_id` / `PRAGMA user_version` reset to their pre-write value on
    /// reopen even though they are durably recorded in the WAL.
    ///
    /// This routine closes that gap the way SQLite itself resolves the header:
    /// if the WAL holds a frame for page 1, page 1 is read through the
    /// WAL-aware pager path and the shared header is re-decoded from that
    /// frame. When the WAL has no page-1 frame the header read from the main
    /// file at open time is already authoritative and the call is a no-op.
    ///
    /// It is intended to be invoked once, immediately after the shared WAL has
    /// been opened/recovered, before any connection observes the header.
    pub fn refresh_header_from_wal(&self) -> Result<()> {
        // A read transaction publishes the recovered `max_frame` to this
        // connection's WAL view so `find_frame` can see frames written by a
        // previous process/connection. Without it `find_frame` would observe a
        // zero read mark and miss the page-1 frame entirely.
        if let LimboResult::Busy = self.begin_read_tx()? {
            // Someone else holds the relevant lock; the header from the main
            // file remains a valid (if possibly stale) view and a later
            // transaction will resolve page 1 through the WAL as usual.
            return Ok(());
        }

        // Drive the single-step form to completion. This is a blocking,
        // run-to-completion convenience wrapper (like `write_database_header`
        // and `wal_checkpoint_mode`); it is only invoked once at WAL
        // open/recovery time, before any connection observes the header.
        let result = loop {
            match self.refresh_header_from_wal_inner() {
                Ok(CursorResult::Ok(())) => break Ok(()),
                Ok(CursorResult::IO) => match self.io.run_once() {
                    Ok(()) => {}
                    Err(e) => break Err(e),
                },
                Err(e) => break Err(e),
            }
        };

        // Always release the read transaction, even if the refresh failed.
        self.end_read_tx()?;
        result
    }

    /// Single-step, non-blocking form of the WAL-header-refresh logic used by
    /// [`Self::refresh_header_from_wal`].
    ///
    /// Returns `CursorResult::IO` (instead of internally busy-looping on
    /// `self.io.run_once()`, as this used to do) when page 1 is still locked
    /// by an in-flight read, mirroring the same check-once-and-yield pattern
    /// used by [`Self::ptrmap_get`] / [`Self::ptrmap_put`] /
    /// [`Self::write_database_header_step`] elsewhere in this file. Safe to
    /// call again after driving IO forward.
    fn refresh_header_from_wal_inner(&self) -> Result<CursorResult<()>> {
        let has_wal_frame = self
            .wal
            .borrow()
            .find_frame(DATABASE_HEADER_PAGE_ID as u64)?
            .is_some();
        if !has_wal_frame {
            // No newer page-1 frame in the WAL: the header read from the main
            // database file at open time is authoritative.
            return Ok(CursorResult::Ok(()));
        }

        // Resolve page 1 through the WAL-aware read path.
        let header_page = self.read_page(DATABASE_HEADER_PAGE_ID)?;
        if header_page.is_locked() {
            return Ok(CursorResult::IO);
        }

        let page = header_page.get();
        let contents = page.contents.as_ref().ok_or_else(|| {
            LimboError::InternalError(
                "page 1 contents missing while refreshing database header from WAL".to_string(),
            )
        })?;
        let buf = contents.as_ptr();
        sqlite3_ondisk::parse_database_header_into(&buf[..DATABASE_HEADER_SIZE], &self.db_header)?;

        // The page now lives in this throwaway pager's cache with a frame that
        // belongs to the recovered WAL view. Drop it so the first real
        // connection re-reads page 1 under its own read transaction rather than
        // reusing a cache entry populated outside a proper transaction scope.
        self.clear_page_cache();
        Ok(CursorResult::Ok(()))
    }

    /// Changes the size of the page cache.
    pub fn change_page_cache_size(&self, capacity: usize) -> Result<CacheResizeResult> {
        let mut page_cache = self.page_cache.write();
        Ok(page_cache.resize(capacity))
    }

    pub fn add_dirty(&self, page_id: usize) {
        // TODO: check duplicates?
        let mut dirty_pages = RefCell::borrow_mut(&self.dirty_pages);
        dirty_pages.insert(page_id);
    }

    pub fn wal_frame_count(&self) -> Result<u64> {
        Ok(self.wal.borrow().get_max_frame_in_wal())
    }
}
