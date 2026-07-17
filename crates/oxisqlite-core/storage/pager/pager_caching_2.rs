//! # `Pager` - caching Methods
//!
//! This module contains method implementations for `Pager`.
//!
//! 🤖 Generated with [SplitRS](https://github.com/cool-japan/splitrs)

#[cfg(not(feature = "omit_autovacuum"))]
use super::functions::ptrmap::*;
use crate::storage::page_cache::{CacheError, PageCacheKey};
use crate::storage::sqlite3_ondisk;
use crate::storage::wal::{CheckpointMode, CheckpointStatus};
use crate::storage::wal::{CheckpointResult, WalFsyncStatus};
use crate::Completion;
use crate::{LimboError, Result};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use tracing::trace;

use super::functions::allocate_page;
use super::type_aliases::PageRef;
use super::types::{
    CheckpointState, FlushState, PagerCacheflushResult, PagerCacheflushStatus, SavepointFrame,
    SynchronousMode,
};
// Only used by the ptrmap-page branch in `allocate_page`, compiled out
// entirely under `omit_autovacuum`.
#[cfg(not(feature = "omit_autovacuum"))]
use super::types::AutoVacuumMode;

use super::pager_type::Pager;

impl Pager {
    /// Flush dirty pages to disk.
    /// In the base case, it will write the dirty pages to the WAL and then fsync the WAL.
    /// If the WAL size is over the checkpoint threshold, it will checkpoint the WAL to
    /// the database file and then fsync the database file.
    pub fn cacheflush(&self) -> Result<PagerCacheflushStatus> {
        let mut checkpoint_result = CheckpointResult::default();
        loop {
            let state = self.flush_info.borrow().state;
            trace!("cacheflush {:?}", state);
            match state {
                FlushState::Start => {
                    let db_size = self.db_header.lock().database_size;
                    for page_id in self.dirty_pages.borrow().iter() {
                        let mut cache = self.page_cache.write();
                        let page_key = PageCacheKey::new(*page_id);
                        let page = cache.get(&page_key).expect("we somehow added a page to dirty list but we didn't mark it as dirty, causing cache to drop it.");
                        let page_type = page.get().contents.as_ref().unwrap().maybe_page_type();
                        trace!("cacheflush(page={}, page_type={:?}", page_id, page_type);
                        self.wal.borrow_mut().append_frame(
                            page.clone(),
                            db_size,
                            self.flush_info.borrow().in_flight_writes.clone(),
                        )?;
                        page.clear_dirty();
                    }
                    // This is okay assuming we use shared cache by default.
                    {
                        let mut cache = self.page_cache.write();
                        cache.clear().unwrap();
                    }
                    self.dirty_pages.borrow_mut().clear();
                    self.flush_info.borrow_mut().state = FlushState::WaitAppendFrames;
                    return Ok(PagerCacheflushStatus::IO);
                }
                FlushState::WaitAppendFrames => {
                    let in_flight = *self.flush_info.borrow().in_flight_writes.borrow();
                    if in_flight == 0 {
                        self.flush_info.borrow_mut().state = FlushState::SyncWal;
                    } else {
                        return Ok(PagerCacheflushStatus::IO);
                    }
                }
                FlushState::SyncWal => {
                    if self.synchronous_mode.get() != SynchronousMode::Off
                        && WalFsyncStatus::IO == self.wal.borrow_mut().sync()?
                    {
                        return Ok(PagerCacheflushStatus::IO);
                    }

                    if !self.wal.borrow().should_checkpoint() {
                        self.flush_info.borrow_mut().state = FlushState::Start;
                        return Ok(PagerCacheflushStatus::Done(
                            PagerCacheflushResult::WalWritten,
                        ));
                    }
                    self.flush_info.borrow_mut().state = FlushState::Checkpoint;
                }
                FlushState::Checkpoint => {
                    match self.checkpoint()? {
                        CheckpointStatus::Done(res) => {
                            checkpoint_result = res;
                            self.flush_info.borrow_mut().state = FlushState::SyncDbFile;
                        }
                        CheckpointStatus::IO => return Ok(PagerCacheflushStatus::IO),
                    };
                }
                FlushState::SyncDbFile => {
                    if self.synchronous_mode.get() == SynchronousMode::Off {
                        self.flush_info.borrow_mut().state = FlushState::Start;
                        break;
                    }
                    sqlite3_ondisk::begin_sync(self.db_file.clone(), self.syncing.clone())?;
                    self.flush_info.borrow_mut().state = FlushState::WaitSyncDbFile;
                }
                FlushState::WaitSyncDbFile => {
                    if *self.syncing.borrow() {
                        return Ok(PagerCacheflushStatus::IO);
                    } else {
                        self.flush_info.borrow_mut().state = FlushState::Start;
                        break;
                    }
                }
            }
        }
        Ok(PagerCacheflushStatus::Done(
            PagerCacheflushResult::Checkpointed(checkpoint_result),
        ))
    }

    pub fn wal_get_frame(
        &self,
        frame_no: u32,
        p_frame: *mut u8,
        frame_len: u32,
    ) -> Result<Arc<Completion>> {
        let wal = self.wal.borrow();
        return wal.read_frame_raw(
            frame_no.into(),
            self.buffer_pool.clone(),
            p_frame,
            frame_len,
        );
    }

    pub fn checkpoint(&self) -> Result<CheckpointStatus> {
        let mut checkpoint_result = CheckpointResult::default();
        loop {
            let state = *self.checkpoint_state.borrow();
            trace!("pager_checkpoint(state={:?})", state);
            match state {
                CheckpointState::Checkpoint => {
                    let in_flight = self.checkpoint_inflight.clone();
                    match self.wal.borrow_mut().checkpoint(
                        self,
                        in_flight,
                        CheckpointMode::Passive,
                    )? {
                        CheckpointStatus::IO => return Ok(CheckpointStatus::IO),
                        CheckpointStatus::Done(res) => {
                            checkpoint_result = res;
                            self.checkpoint_state.replace(CheckpointState::SyncDbFile);
                        }
                    };
                }
                CheckpointState::SyncDbFile => {
                    if self.synchronous_mode.get() == SynchronousMode::Off {
                        self.checkpoint_state
                            .replace(CheckpointState::CheckpointDone);
                    } else {
                        sqlite3_ondisk::begin_sync(self.db_file.clone(), self.syncing.clone())?;
                        self.checkpoint_state
                            .replace(CheckpointState::WaitSyncDbFile);
                    }
                }
                CheckpointState::WaitSyncDbFile => {
                    if *self.syncing.borrow() {
                        return Ok(CheckpointStatus::IO);
                    } else {
                        self.checkpoint_state
                            .replace(CheckpointState::CheckpointDone);
                    }
                }
                CheckpointState::CheckpointDone => {
                    return if *self.checkpoint_inflight.borrow() > 0 {
                        Ok(CheckpointStatus::IO)
                    } else {
                        self.checkpoint_state.replace(CheckpointState::Checkpoint);
                        Ok(CheckpointStatus::Done(checkpoint_result))
                    };
                }
            }
        }
    }

    /// Invalidates entire page cache by removing all dirty and clean pages. Usually used in case
    /// of a rollback or in case we want to invalidate page cache after starting a read transaction
    /// right after new writes happened which would invalidate current page cache.
    pub fn clear_page_cache(&self) {
        self.dirty_pages.borrow_mut().clear();
        self.page_cache.write().unset_dirty_all_pages();
        self.page_cache
            .write()
            .clear()
            .expect("Failed to clear page cache");
    }

    /// Evicts exactly the given page ids from the page cache, leaving every
    /// other cached page untouched.
    ///
    /// Used after a checkpoint, which only needs to invalidate the specific
    /// pages it just backfilled from the WAL into the main database file
    /// (see [`crate::storage::wal::CheckpointResult::checkpointed_page_ids`])
    /// rather than the entire cache — evicting everything on every checkpoint
    /// is a real perf cliff under sustained writes, since it forces every
    /// other still-valid cached page to be re-read from disk.
    ///
    /// A page that cannot be evicted right now (still locked by in-flight I/O,
    /// or dirty again because a concurrent write already modified it since the
    /// checkpoint read it) is silently left in the cache rather than treated
    /// as an error: the checkpoint itself already completed successfully, and
    /// skipping eviction of a page that isn't safe to drop yet is strictly
    /// more conservative (and correct) than the panic this used to risk via
    /// an unconditional `.clear()` + `.expect(...)`.
    fn evict_checkpointed_pages(&self, page_ids: &[u64]) {
        if page_ids.is_empty() {
            return;
        }
        let mut cache = self.page_cache.write();
        for page_id in page_ids {
            match cache.delete(PageCacheKey::new(*page_id as usize)) {
                Ok(()) => {}
                Err(CacheError::Locked | CacheError::Dirty | CacheError::ActiveRefs) => {
                    // Not safe to evict right now; leave it cached.
                }
                Err(e) => {
                    tracing::debug!(
                        "evict_checkpointed_pages: failed to evict page {}: {:?}",
                        page_id,
                        e
                    );
                }
            }
        }
    }

    /// Roll back the current write transaction.
    ///
    /// Clears all dirty pages from the page cache, resets in-flight flush and
    /// checkpoint state machines to their idle state, and delegates to the WAL
    /// to undo any frames appended during this transaction.
    pub fn rollback(&self) {
        // Invalidate every dirty (and clean) cached page so that subsequent
        // reads reload from the WAL / database file.
        self.clear_page_cache();

        // Reset the flush state machine so the next commit starts fresh.
        self.flush_info.borrow_mut().state = FlushState::Start;
        *self.flush_info.borrow().in_flight_writes.borrow_mut() = 0;

        // Reset the checkpoint state machine.
        self.checkpoint_state.replace(CheckpointState::Checkpoint);

        // Reset the sync flag.
        *self.syncing.borrow_mut() = false;

        // Roll back WAL frames appended during this transaction.
        self.wal.borrow().rollback();

        // Discard all savepoints: a full ROLLBACK invalidates them all.
        self.savepoints.borrow_mut().clear();
    }

    /// Flush all currently dirty pages to WAL frames without clearing the page
    /// cache.
    ///
    /// This is called at `SAVEPOINT` open time to materialise the pre-savepoint
    /// state as WAL frames.  After this call the pager's `dirty_pages` set is
    /// empty and every modified page lives in `frame_cache`.  Subsequent
    /// `ROLLBACK TO` can restore to this state by pruning frames written after
    /// the savepoint and then relying on WAL-based reads.
    ///
    /// Unlike `cacheflush` this method intentionally does **not** clear the
    /// page cache, so in-progress reads/writes continue to work from the
    /// existing in-memory pages.
    fn flush_dirty_pages_to_wal(&self) -> Result<()> {
        if self.dirty_pages.borrow().is_empty() {
            return Ok(());
        }
        let db_size = self.db_header.lock().database_size;
        // Collect page IDs first to avoid holding the dirty_pages borrow while
        // iterating and calling append_frame.
        let dirty_ids: Vec<usize> = self.dirty_pages.borrow().iter().copied().collect();
        for page_id in dirty_ids {
            let page_key = PageCacheKey::new(page_id);
            // Use the write-lock variant of get() which returns Option<Arc<Page>>.
            let mut cache = self.page_cache.write();
            let page = match cache.get(&page_key) {
                Some(p) => p,
                // Page was dirtied but evicted; skip defensively.
                None => continue,
            };
            self.wal.borrow_mut().append_frame(
                page.clone(),
                db_size,
                self.flush_info.borrow().in_flight_writes.clone(),
            )?;
        }
        // Clear the dirty-pages set — these pages are now in WAL frames.
        // The page cache is left intact so in-flight reads see the same data.
        self.dirty_pages.borrow_mut().clear();
        // Advance the reader-visible max frame so that, after a ROLLBACK TO
        // clears the page cache, subsequent reads can find these frames via
        // find_frame.
        let new_max = self.wal.borrow().current_frame_state().0;
        self.wal.borrow_mut().set_reader_max_frame(new_max);
        Ok(())
    }

    /// Open a new named savepoint by capturing the current WAL frame state.
    ///
    /// All currently dirty pages are first flushed to WAL so that the
    /// pre-savepoint state is recorded as actual WAL frames.  `ROLLBACK TO`
    /// can then restore to this point by pruning frames written after the
    /// savepoint and clearing the page cache.
    ///
    /// `is_txn_owner` should be `true` when this savepoint implicitly started
    /// the surrounding write transaction (i.e., opened in autocommit mode).
    pub fn open_savepoint(&self, name: String, is_txn_owner: bool) -> Result<()> {
        // Materialise the current state as WAL frames before snapshotting.
        self.flush_dirty_pages_to_wal()?;
        let (max_frame, checksum) = self.wal.borrow().current_frame_state();
        let frame = SavepointFrame {
            name,
            wal_max_frame: max_frame,
            wal_checksum: checksum,
            is_transaction_owner: is_txn_owner,
        };
        self.savepoints.borrow_mut().push(frame);
        Ok(())
    }

    /// Roll back to a previously opened savepoint (ROLLBACK TO SAVEPOINT).
    ///
    /// Truncates the WAL to the savepoint's frame watermark, clears the page
    /// cache, and resets the flush state machine — exactly as a full rollback
    /// does, but scoped to the savepoint boundary.  The savepoint itself
    /// remains valid so subsequent writes can continue within the transaction.
    ///
    /// Nested savepoints opened after the target are discarded.
    pub fn rollback_to_savepoint(&self, name: &str) -> Result<()> {
        // Find the innermost savepoint with this name.
        let idx = {
            let sps = self.savepoints.borrow();
            sps.iter()
                .rposition(|sp| sp.name.eq_ignore_ascii_case(name))
                .ok_or_else(|| LimboError::ParseError(format!("no such savepoint: {name}")))?
        };
        let sp = {
            let sps = self.savepoints.borrow();
            sps[idx].clone()
        };
        // All savepoints opened after the target are invalid after rollback.
        self.savepoints.borrow_mut().truncate(idx + 1);
        // Clear page cache (and dirty_pages) so reads see only the WAL state.
        self.clear_page_cache();
        // Prune WAL frames appended after the savepoint.
        self.wal
            .borrow()
            .rollback_to_frame(sp.wal_max_frame, sp.wal_checksum)?;
        // Restore the reader-visible max frame to the savepoint boundary so
        // that find_frame returns exactly the savepoint-era frames.
        self.wal.borrow_mut().set_reader_max_frame(sp.wal_max_frame);
        // Reset flush state machine.  Do NOT reset in_flight_writes: writes
        // from the savepoint flush may still be in-flight for file-backed DBs
        // and will drain naturally through the IO loop during COMMIT.
        self.flush_info.borrow_mut().state = FlushState::Start;
        self.checkpoint_state.replace(CheckpointState::Checkpoint);
        *self.syncing.borrow_mut() = false;
        Ok(())
    }

    /// Release a savepoint (RELEASE SAVEPOINT).
    ///
    /// Removes the named savepoint and all savepoints opened after it from the
    /// stack.  Returns `true` when the released savepoint was the implicit
    /// transaction owner — the caller must then commit the transaction.
    pub fn release_savepoint(&self, name: &str) -> Result<bool> {
        // Find the innermost savepoint with this name.
        let idx = {
            let sps = self.savepoints.borrow();
            sps.iter()
                .rposition(|sp| sp.name.eq_ignore_ascii_case(name))
                .ok_or_else(|| LimboError::ParseError(format!("no such savepoint: {name}")))?
        };
        let is_owner = self.savepoints.borrow()[idx].is_transaction_owner;
        // Release the target savepoint and all nested ones opened after it.
        self.savepoints.borrow_mut().truncate(idx);
        Ok(is_owner)
    }

    pub fn checkpoint_shutdown(&self) -> Result<()> {
        let mut attempts = 0;
        {
            let mut wal = self.wal.borrow_mut();
            // fsync the wal synchronously before beginning checkpoint (unless OFF)
            if self.synchronous_mode.get() != SynchronousMode::Off {
                while let Ok(WalFsyncStatus::IO) = wal.sync() {
                    if attempts >= 10 {
                        return Err(LimboError::InternalError(
                            "Failed to fsync WAL before final checkpoint, fd likely closed".into(),
                        ));
                    }
                    self.io.run_once()?;
                    attempts += 1;
                }
            }
        }
        // Truncate the WAL on clean close so the .db is self-contained; if a
        // Truncate checkpoint cannot complete (e.g. other active readers), fall
        // back to a Passive checkpoint.
        let result = match self.wal_checkpoint_mode(CheckpointMode::Truncate) {
            Ok(_) => Ok(()),
            Err(_) => {
                let _ = self.wal_checkpoint_mode(CheckpointMode::Passive);
                Ok(())
            }
        };

        // Belt-and-suspenders final cleanup: proactively clear the page
        // cache on every clean shutdown, not only as a side effect of
        // `cacheflush()`'s dirty-page-flush cycle (which never runs for a
        // read-only connection, or one whose pages are all already clean).
        // `DumbLruPageCache` now has a `Drop` impl that unconditionally
        // frees every resident entry, so this call is not required to avoid
        // a leak -- but clearing here releases cached pages as soon as a
        // connection cleanly closes rather than only when the last `Arc`
        // reference to its page cache is finally dropped.
        //
        // Ownership note: each `Connection` owns a *private* page cache --
        // `Database::connect` constructs a fresh
        // `Arc::new(RwLock::new(DumbLruPageCache::default()))` per
        // connection and moves it straight into that connection's `Pager`
        // (`Database::_shared_page_cache` is dead code reserved for a
        // shared-cache mode that isn't wired up anywhere yet), so this
        // `Arc` is never cloned out to another `Connection`/`Pager` today.
        // `strong_count() == 1` is therefore always true in practice, but
        // the check is kept as a defensive guard: if shared-cache mode is
        // ever implemented without revisiting this call site, clearing
        // would otherwise evict pages a sibling connection still depends
        // on, and this would become a silent no-op instead.
        if Arc::strong_count(&self.page_cache) == 1 {
            if let Err(e) = self.page_cache.write().clear() {
                // Not fatal: a locked/dirty page just stays cached until the
                // final `Drop` frees it. This must never panic or propagate
                // out of what is ultimately a `Connection::drop` path.
                tracing::debug!(
                    "checkpoint_shutdown: page cache not fully clear at shutdown ({e:?}); \
                     remaining entries will be freed when the cache is dropped"
                );
            }
        }

        result
    }

    pub fn wal_checkpoint(&self) -> CheckpointResult {
        let checkpoint_result: CheckpointResult;
        loop {
            match self.wal.borrow_mut().checkpoint(
                self,
                Rc::new(RefCell::new(0)),
                CheckpointMode::Passive,
            ) {
                Ok(CheckpointStatus::IO) => {
                    let _ = self.io.run_once();
                }
                Ok(CheckpointStatus::Done(res)) => {
                    checkpoint_result = res;
                    break;
                }
                Err(err) => panic!("error while clearing cache {}", err),
            }
        }
        self.evict_checkpointed_pages(&checkpoint_result.checkpointed_page_ids);
        checkpoint_result
    }

    /// Run a blocking checkpoint in the given mode (used by close()/Drop and by
    /// `PRAGMA wal_checkpoint(<MODE>)` at the pager level). Unlike
    /// [`Self::wal_checkpoint`], this never panics and returns a `Result`.
    pub fn wal_checkpoint_mode(&self, mode: CheckpointMode) -> Result<CheckpointResult> {
        let checkpoint_result;
        loop {
            match self
                .wal
                .borrow_mut()
                .checkpoint(self, Rc::new(RefCell::new(0)), mode)?
            {
                CheckpointStatus::IO => {
                    self.io.run_once()?;
                }
                CheckpointStatus::Done(res) => {
                    checkpoint_result = res;
                    break;
                }
            }
        }
        self.evict_checkpointed_pages(&checkpoint_result.checkpointed_page_ids);
        Ok(checkpoint_result)
    }

    pub fn free_page(&self, page: Option<PageRef>, page_id: usize) -> Result<()> {
        tracing::trace!("free_page(page_id={})", page_id);
        const TRUNK_PAGE_HEADER_SIZE: usize = 8;
        const LEAF_ENTRY_SIZE: usize = 4;
        const RESERVED_SLOTS: usize = 2;

        const TRUNK_PAGE_NEXT_PAGE_OFFSET: usize = 0; // Offset to next trunk page pointer
        const TRUNK_PAGE_LEAF_COUNT_OFFSET: usize = 4; // Offset to leaf count

        if page_id < 2 || page_id > self.db_header.lock().database_size as usize {
            return Err(LimboError::Corrupt(format!(
                "Invalid page number {} for free operation",
                page_id
            )));
        }

        let page = match page {
            Some(page) => {
                assert_eq!(page.get().id, page_id, "Page id mismatch");
                page
            }
            None => self.read_page(page_id)?,
        };

        self.db_header.lock().freelist_pages += 1;

        let trunk_page_id = self.db_header.lock().freelist_trunk_page;

        if trunk_page_id != 0 {
            // Add as leaf to current trunk
            let trunk_page = self.read_page(trunk_page_id as usize)?;
            let trunk_page_contents = trunk_page.get().contents.as_ref().unwrap();
            let number_of_leaf_pages = trunk_page_contents.read_u32(TRUNK_PAGE_LEAF_COUNT_OFFSET);

            // Reserve 2 slots for the trunk page header which is 8 bytes or 2*LEAF_ENTRY_SIZE
            let max_free_list_entries = (self.usable_size() / LEAF_ENTRY_SIZE) - RESERVED_SLOTS;

            if number_of_leaf_pages < max_free_list_entries as u32 {
                trunk_page.set_dirty();
                self.add_dirty(trunk_page_id as usize);

                trunk_page_contents
                    .write_u32(TRUNK_PAGE_LEAF_COUNT_OFFSET, number_of_leaf_pages + 1);
                trunk_page_contents.write_u32(
                    TRUNK_PAGE_HEADER_SIZE + (number_of_leaf_pages as usize * LEAF_ENTRY_SIZE),
                    page_id as u32,
                );
                page.clear_uptodate();
                page.clear_loaded();

                return Ok(());
            }
        }

        // If we get here, need to make this page a new trunk
        page.set_dirty();
        self.add_dirty(page_id);

        let contents = page.get().contents.as_mut().unwrap();
        // Point to previous trunk
        contents.write_u32(TRUNK_PAGE_NEXT_PAGE_OFFSET, trunk_page_id);
        // Zero leaf count
        contents.write_u32(TRUNK_PAGE_LEAF_COUNT_OFFSET, 0);
        // Update page 1 to point to new trunk
        self.db_header.lock().freelist_trunk_page = page_id as u32;
        // Clear flags
        page.clear_uptodate();
        page.clear_loaded();
        Ok(())
    }

    ///
    /// # Errors
    /// Returns [`LimboError::CacheFull`] when the page cache has no room for
    /// the new page(s). On that path `header.database_size` (and anything
    /// derived from it, like a newly-allocated ptrmap page's cache entry) is
    /// left exactly as it was before the call — the size is only committed
    /// and persisted once every cache insert this call needs has succeeded.
    #[allow(clippy::readonly_write_lock)]
    pub fn allocate_page(&self) -> Result<PageRef> {
        let header = &self.db_header;
        let mut header = header.lock();

        // Compute the candidate new database size(s) WITHOUT touching the
        // live, shared header yet. `header` is a lock guard over the actual
        // `DatabaseHeader` the rest of the pager reads, so mutating
        // `header.database_size` here directly (as this used to do) would
        // leave that bump visible even if a later step below fails with
        // `CacheError::Full`. Instead, every fallible step operates on this
        // local candidate, and `header.database_size` is only ever written
        // once all of them have succeeded.
        //
        // Only mutated in the ptrmap-page branch below, which is compiled out
        // entirely under the `omit_autovacuum` feature.
        #[cfg_attr(feature = "omit_autovacuum", allow(unused_mut))]
        let mut candidate_size = header.database_size + 1;

        #[cfg(not(feature = "omit_autovacuum"))]
        let mut ptrmap_page: Option<PageRef> = None;

        #[cfg(not(feature = "omit_autovacuum"))]
        {
            //  If the following conditions are met, allocate a pointer map page, add to cache and increment the database size
            //  - autovacuum is enabled
            //  - the last page is a pointer map page
            if matches!(*self.auto_vacuum_mode.borrow(), AutoVacuumMode::Full)
                && is_ptrmap_page(candidate_size, header.get_page_size() as usize)
            {
                let page = allocate_page(candidate_size as usize, &self.buffer_pool, 0);
                let page_key = PageCacheKey::new(page.get().id);
                {
                    let mut cache = self.page_cache.write();
                    match cache.insert(page_key, page.clone()) {
                        Ok(_) => {}
                        Err(CacheError::Full) => return Err(LimboError::CacheFull),
                        Err(_) => {
                            return Err(LimboError::InternalError(
                                "Unknown error inserting page to cache".into(),
                            ))
                        }
                    }
                }
                page.set_dirty();
                self.add_dirty(page.get().id);
                candidate_size += 1;
                ptrmap_page = Some(page);
            }
        }

        // Reserve the page-cache slot for the page actually being requested
        // BEFORE mutating or persisting `header.database_size`, so a full
        // cache fails cleanly here and leaves the header untouched.
        let page = allocate_page(candidate_size as usize, &self.buffer_pool, 0);
        let page_key = PageCacheKey::new(page.get().id);
        {
            let mut cache = self.page_cache.write();
            if let Err(err) = cache.insert(page_key, page.clone()) {
                // Roll back the ptrmap-page reservation (if any) so we don't
                // leave a phantom dirty page in the cache while reporting
                // failure, keeping `header.database_size` untouched on this
                // error path.
                #[cfg(not(feature = "omit_autovacuum"))]
                if let Some(ptrmap_page) = ptrmap_page {
                    let ptrmap_page_id = ptrmap_page.get().id;
                    ptrmap_page.clear_dirty();
                    self.dirty_pages.borrow_mut().remove(&ptrmap_page_id);
                    let _ = cache.delete(PageCacheKey::new(ptrmap_page_id));
                }
                return Err(match err {
                    CacheError::Full => LimboError::CacheFull,
                    _ => LimboError::InternalError("Unknown error inserting page to cache".into()),
                });
            }
        }
        page.set_dirty();
        self.add_dirty(page.get().id);

        // Both cache inserts (if two were needed) have now succeeded: commit
        // the new database size and persist the header.
        header.database_size = candidate_size;
        self.write_database_header(&header)?;

        Ok(page)
    }

    pub fn update_dirty_loaded_page_in_cache(
        &self,
        id: usize,
        page: PageRef,
    ) -> Result<(), LimboError> {
        let mut cache = self.page_cache.write();
        let page_key = PageCacheKey::new(id);

        // FIXME: use specific page key for writer instead of max frame, this will make readers not conflict
        assert!(page.is_dirty());
        cache
            .insert_ignore_existing(page_key, page.clone())
            .map_err(|e| {
                LimboError::InternalError(format!(
                    "Failed to insert loaded page {} into cache: {:?}",
                    id, e
                ))
            })?;
        page.set_loaded();
        Ok(())
    }

    pub fn usable_size(&self) -> usize {
        let db_header = self.db_header.lock();
        (db_header.get_page_size() - db_header.reserved_space as u32) as usize
    }
}
