#![allow(clippy::arc_with_non_send_sync)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::array;
use std::cell::UnsafeCell;
use std::collections::HashMap;
use strum::EnumString;
use tracing::{instrument, Level};

use std::fmt::Formatter;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::{
    cell::{Cell, RefCell},
    fmt,
    rc::Rc,
    sync::Arc,
};

use crate::fast_lock::SpinLock;
use crate::io::{File, SyncCompletion, IO};
use crate::result::LimboResult;
use crate::storage::sqlite3_ondisk::{
    begin_read_wal_frame, begin_write_wal_frame, finish_read_page, WAL_FRAME_HEADER_SIZE,
    WAL_HEADER_SIZE,
};
use crate::{Buffer, Result};
use crate::{Completion, Page};

use self::sqlite3_ondisk::{checksum_wal, PageContent, WAL_MAGIC_BE, WAL_MAGIC_LE};

use super::buffer_pool::BufferPool;
use super::pager::{PageRef, Pager};
use super::sqlite3_ondisk::{self, begin_write_btree_page, WalHeader};

pub const READMARK_NOT_USED: u32 = 0xffffffff;

pub const NO_LOCK: u32 = 0;
pub const SHARED_LOCK: u32 = 1;
pub const WRITE_LOCK: u32 = 2;

#[derive(Debug, Clone)]
pub struct CheckpointResult {
    /// number of frames in WAL
    pub num_wal_frames: u64,
    /// number of frames moved successfully from WAL to db file after checkpoint
    pub num_checkpointed_frames: u64,
    /// Page ids actually backfilled (WAL -> main db file) during this
    /// checkpoint pass. Lets a caller invalidate exactly these pages from its
    /// page cache instead of evicting everything (see
    /// `Pager::wal_checkpoint`/`Pager::wal_checkpoint_mode`).
    pub checkpointed_page_ids: Vec<u64>,
}

impl Default for CheckpointResult {
    fn default() -> Self {
        Self::new()
    }
}

impl CheckpointResult {
    pub fn new() -> Self {
        Self {
            num_wal_frames: 0,
            num_checkpointed_frames: 0,
            checkpointed_page_ids: Vec::new(),
        }
    }
}

#[derive(Debug, Copy, Clone, EnumString)]
#[strum(ascii_case_insensitive)]
pub enum CheckpointMode {
    /// Checkpoint as many frames as possible without waiting for any database readers or writers to finish, then sync the database file if all frames in the log were checkpointed.
    Passive,
    /// This mode blocks until there is no database writer and all readers are reading from the most recent database snapshot. It then checkpoints all frames in the log file and syncs the database file. This mode blocks new database writers while it is pending, but new database readers are allowed to continue unimpeded.
    Full,
    /// This mode works the same way as `Full` with the addition that after checkpointing the log file it blocks (calls the busy-handler callback) until all readers are reading from the database file only. This ensures that the next writer will restart the log file from the beginning. Like `Full`, this mode blocks new database writer attempts while it is pending, but does not impede readers.
    Restart,
    /// This mode works the same way as `Restart` with the addition that it also truncates the log file to zero bytes just prior to a successful return.
    Truncate,
}

#[derive(Debug, Default)]
pub struct LimboRwLock {
    lock: AtomicU32,
    nreads: AtomicU32,
    value: AtomicU32,
}

impl LimboRwLock {
    pub fn new() -> Self {
        Self {
            lock: AtomicU32::new(NO_LOCK),
            nreads: AtomicU32::new(0),
            value: AtomicU32::new(READMARK_NOT_USED),
        }
    }

    /// Shared lock. Returns true if it was successful, false if it couldn't lock it
    pub fn read(&mut self) -> bool {
        let lock = self.lock.load(Ordering::SeqCst);
        let ok = match lock {
            NO_LOCK => {
                let res = self.lock.compare_exchange(
                    lock,
                    SHARED_LOCK,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                );
                let ok = res.is_ok();
                if ok {
                    self.nreads.fetch_add(1, Ordering::SeqCst);
                }
                ok
            }
            SHARED_LOCK => {
                // There is this race condition where we could've unlocked after loading lock ==
                // SHARED_LOCK.
                self.nreads.fetch_add(1, Ordering::SeqCst);
                let lock_after_load = self.lock.load(Ordering::SeqCst);
                if lock_after_load != lock {
                    // try to lock it again
                    let res = self.lock.compare_exchange(
                        lock_after_load,
                        SHARED_LOCK,
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                    );
                    let ok = res.is_ok();
                    if ok {
                        // we were able to acquire it back
                        true
                    } else {
                        // we couldn't acquire it back, reduce number again
                        self.nreads.fetch_sub(1, Ordering::SeqCst);
                        false
                    }
                } else {
                    true
                }
            }
            WRITE_LOCK => false,
            _ => unreachable!(),
        };
        tracing::trace!("read_lock({})", ok);
        ok
    }

    /// Locks exclusively. Returns true if it was successful, false if it couldn't lock it
    pub fn write(&mut self) -> bool {
        let lock = self.lock.load(Ordering::SeqCst);
        let ok = match lock {
            NO_LOCK => {
                let res = self.lock.compare_exchange(
                    lock,
                    WRITE_LOCK,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                );
                res.is_ok()
            }
            SHARED_LOCK => {
                // no op
                false
            }
            WRITE_LOCK => false,
            _ => unreachable!(),
        };
        tracing::trace!("write_lock({})", ok);
        ok
    }

    /// Unlock the current held lock.
    pub fn unlock(&mut self) {
        let lock = self.lock.load(Ordering::SeqCst);
        tracing::trace!("unlock(lock={})", lock);
        match lock {
            NO_LOCK => {}
            SHARED_LOCK => {
                let prev = self.nreads.fetch_sub(1, Ordering::SeqCst);
                if prev == 1 {
                    let res = self.lock.compare_exchange(
                        lock,
                        NO_LOCK,
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                    );
                    assert!(res.is_ok());
                }
            }
            WRITE_LOCK => {
                let res =
                    self.lock
                        .compare_exchange(lock, NO_LOCK, Ordering::SeqCst, Ordering::SeqCst);
                assert!(res.is_ok());
            }
            _ => unreachable!(),
        }
    }
}

/// Write-ahead log (WAL).
pub trait Wal {
    /// Begin a read transaction.
    fn begin_read_tx(&mut self) -> Result<LimboResult>;

    /// Begin a write transaction.
    fn begin_write_tx(&mut self) -> Result<LimboResult>;

    /// End a read transaction.
    fn end_read_tx(&self) -> Result<LimboResult>;

    /// End a write transaction.
    fn end_write_tx(&self) -> Result<LimboResult>;

    /// Find the latest frame containing a page.
    fn find_frame(&self, page_id: u64) -> Result<Option<u64>>;

    /// Read a frame from the WAL.
    fn read_frame(&self, frame_id: u64, page: PageRef, buffer_pool: Rc<BufferPool>) -> Result<()>;

    /// Read a frame from the WAL.
    fn read_frame_raw(
        &self,
        frame_id: u64,
        buffer_pool: Rc<BufferPool>,
        frame: *mut u8,
        frame_len: u32,
    ) -> Result<Arc<Completion>>;

    /// Write a frame to the WAL.
    fn append_frame(
        &mut self,
        page: PageRef,
        db_size: u32,
        write_counter: Rc<RefCell<usize>>,
    ) -> Result<()>;

    fn should_checkpoint(&self) -> bool;
    fn checkpoint(
        &mut self,
        pager: &Pager,
        write_counter: Rc<RefCell<usize>>,
        mode: CheckpointMode,
    ) -> Result<CheckpointStatus>;
    fn sync(&mut self) -> Result<WalFsyncStatus>;
    fn get_max_frame_in_wal(&self) -> u64;
    fn get_max_frame(&self) -> u64;
    fn get_min_frame(&self) -> u64;

    /// Roll back the current write transaction.
    ///
    /// Restores `max_frame` and `last_checksum` to the values they held at the
    /// start of the write transaction, and removes any frame-cache entries that
    /// were written during that transaction.
    fn rollback(&self);

    /// Returns (current max_frame, current last_checksum) for savepoint capture.
    fn current_frame_state(&self) -> (u64, (u32, u32));

    /// Roll back WAL to a specific target frame (for ROLLBACK TO SAVEPOINT).
    ///
    /// Prunes frame_cache and pages_in_frames for all frames > target_frame,
    /// then restores max_frame and last_checksum to the savepoint snapshot.
    fn rollback_to_frame(&self, target_frame: u64, target_checksum: (u32, u32)) -> Result<()>;

    /// Update the reader-visible max frame boundary.
    ///
    /// `WalFile::max_frame` controls which frames `find_frame` will return.
    /// At `SAVEPOINT` open time we eagerly flush dirty pages to WAL and must
    /// raise this boundary so that post-rollback reads see the newly written
    /// frames.  At `ROLLBACK TO` time we restore it to the savepoint snapshot
    /// so reads see exactly the savepoint state.
    fn set_reader_max_frame(&mut self, frame: u64);
}

/// A dummy WAL implementation that does nothing.
/// This is used for ephemeral indexes where a WAL is not really
/// needed, and is preferable to passing an Option<dyn Wal> around
/// everywhere.
pub struct DummyWAL;

impl Wal for DummyWAL {
    fn begin_read_tx(&mut self) -> Result<LimboResult> {
        Ok(LimboResult::Ok)
    }

    fn end_read_tx(&self) -> Result<LimboResult> {
        Ok(LimboResult::Ok)
    }

    fn begin_write_tx(&mut self) -> Result<LimboResult> {
        Ok(LimboResult::Ok)
    }

    fn end_write_tx(&self) -> Result<LimboResult> {
        Ok(LimboResult::Ok)
    }

    fn find_frame(&self, _page_id: u64) -> Result<Option<u64>> {
        Ok(None)
    }

    fn read_frame(
        &self,
        _frame_id: u64,
        _page: crate::PageRef,
        _buffer_pool: Rc<BufferPool>,
    ) -> Result<()> {
        Ok(())
    }

    fn read_frame_raw(
        &self,
        _frame_id: u64,
        _buffer_pool: Rc<BufferPool>,
        _frame: *mut u8,
        _frame_len: u32,
    ) -> Result<Arc<Completion>> {
        // `DummyWAL` backs ephemeral/in-memory indexes that never have a real
        // on-disk WAL, so there is no frame data to read raw bytes for. This
        // method is only reachable via `Pager::wal_get_frame` (the
        // `sqlite3_wal_frame`-style API meant for real, file-backed WALs), so
        // a descriptive error is the correct outcome here, not a real
        // implementation.
        Err(crate::error::LimboError::InternalError(
            "DummyWAL::read_frame_raw: ephemeral in-memory WAL has no frames to read".to_string(),
        ))
    }

    fn append_frame(
        &mut self,
        _page: crate::PageRef,
        _db_size: u32,
        _write_counter: Rc<RefCell<usize>>,
    ) -> Result<()> {
        Ok(())
    }

    fn should_checkpoint(&self) -> bool {
        false
    }

    fn checkpoint(
        &mut self,
        _pager: &Pager,
        _write_counter: Rc<RefCell<usize>>,
        _mode: crate::CheckpointMode,
    ) -> Result<crate::CheckpointStatus> {
        Ok(crate::CheckpointStatus::Done(
            crate::CheckpointResult::default(),
        ))
    }

    fn sync(&mut self) -> Result<crate::storage::wal::WalFsyncStatus> {
        Ok(crate::storage::wal::WalFsyncStatus::Done)
    }

    fn get_max_frame_in_wal(&self) -> u64 {
        0
    }

    fn get_max_frame(&self) -> u64 {
        0
    }

    fn get_min_frame(&self) -> u64 {
        0
    }

    fn rollback(&self) {
        // DummyWAL has no persistent state to roll back.
    }

    fn current_frame_state(&self) -> (u64, (u32, u32)) {
        (0, (0, 0))
    }

    fn rollback_to_frame(&self, _target_frame: u64, _target_checksum: (u32, u32)) -> Result<()> {
        Ok(())
    }

    fn set_reader_max_frame(&mut self, _frame: u64) {
        // DummyWAL has no reader max frame state.
    }
}

// Syncing requires a state machine because we need to schedule a sync and then wait until it is
// finished. If we don't wait there will be undefined behaviour that no one wants to debug.
#[derive(Copy, Clone, Debug)]
enum SyncState {
    NotSyncing,
    Syncing,
}

#[derive(Debug, Copy, Clone)]
pub enum CheckpointState {
    Start,
    ReadFrame,
    WaitReadFrame,
    WritePage,
    WaitWritePage,
    Done,
}

#[derive(Debug, Copy, Clone, PartialEq)]
pub enum WalFsyncStatus {
    Done,
    IO,
}

#[derive(Debug, Clone)]
pub enum CheckpointStatus {
    Done(CheckpointResult),
    IO,
}

// Checkpointing is a state machine that has multiple steps. Since there are multiple steps we save
// in flight information of the checkpoint in OngoingCheckpoint. page is just a helper Page to do
// page operations like reading a frame to a page, and writing a page to disk. This page should not
// be placed back in pager page cache or anything, it's just a helper.
// min_frame and max_frame is the range of frames that can be safely transferred from WAL to db
// file.
// current_page is a helper to iterate through all the pages that might have a frame in the safe
// range. This is inefficient for now.
struct OngoingCheckpoint {
    page: PageRef,
    state: CheckpointState,
    min_frame: u64,
    max_frame: u64,
    current_page: u64,
    /// Page ids actually selected for backfill (`WritePage`'d) so far during
    /// the checkpoint pass currently in progress. Reset in `CheckpointState::Start`,
    /// appended to in `CheckpointState::ReadFrame`, and drained into
    /// `CheckpointResult::checkpointed_page_ids` in `CheckpointState::Done`.
    touched_pages: Vec<u64>,
}

impl fmt::Debug for OngoingCheckpoint {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("OngoingCheckpoint")
            .field("state", &self.state)
            .field("min_frame", &self.min_frame)
            .field("max_frame", &self.max_frame)
            .field("current_page", &self.current_page)
            .field("touched_pages_count", &self.touched_pages.len())
            .finish()
    }
}

#[allow(dead_code)]
pub struct WalFile {
    io: Arc<dyn IO>,
    buffer_pool: Rc<BufferPool>,

    syncing: Rc<Cell<bool>>,
    sync_state: Cell<SyncState>,
    page_size: u32,

    shared: Arc<UnsafeCell<WalFileShared>>,
    ongoing_checkpoint: OngoingCheckpoint,
    checkpoint_threshold: usize,
    // min and max frames for this connection
    /// This is the index to the read_lock in WalFileShared that we are holding. This lock contains
    /// the max frame for this connection.
    max_frame_read_lock_index: usize,
    /// Max frame allowed to lookup range=(minframe..max_frame)
    max_frame: u64,
    /// Start of range to look for frames range=(minframe..max_frame)
    min_frame: u64,
    /// Snapshot of shared `max_frame` taken at the start of the current write
    /// transaction.  Used by `rollback()` to undo any appended frames.
    txn_start_max_frame: Cell<u64>,
    /// Snapshot of `shared.last_checksum` taken at the start of the current
    /// write transaction.  Used by `rollback()` to restore the cumulative
    /// checksum.
    txn_start_last_checksum: Cell<(u32, u32)>,
}

impl fmt::Debug for WalFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WalFile")
            .field("syncing", &self.syncing.get())
            .field("sync_state", &self.sync_state)
            .field("page_size", &self.page_size)
            .field("shared", &self.shared)
            .field("ongoing_checkpoint", &self.ongoing_checkpoint)
            .field("checkpoint_threshold", &self.checkpoint_threshold)
            .field("max_frame_read_lock_index", &self.max_frame_read_lock_index)
            .field("max_frame", &self.max_frame)
            .field("min_frame", &self.min_frame)
            .field("txn_start_max_frame", &self.txn_start_max_frame)
            .field("txn_start_last_checksum", &self.txn_start_last_checksum)
            // Excluding io, buffer_pool
            .finish()
    }
}

// TODO(pere): lock only important parts + pin WalFileShared
/// WalFileShared is the part of a WAL that will be shared between threads. A wal has information
/// that needs to be communicated between threads so this struct does the job.
#[allow(dead_code)]
pub struct WalFileShared {
    pub wal_header: Arc<SpinLock<WalHeader>>,
    pub min_frame: AtomicU64,
    pub max_frame: AtomicU64,
    pub nbackfills: AtomicU64,
    // Frame cache maps a Page to all the frames it has stored in WAL in ascending order.
    // This is to easily find the frame it must checkpoint each connection if a checkpoint is
    // necessary.
    // One difference between SQLite and limbo is that we will never support multi process, meaning
    // we don't need WAL's index file. So we can do stuff like this without shared memory.
    // TODO: this will need refactoring because this is incredible memory inefficient.
    pub frame_cache: Arc<SpinLock<HashMap<u64, Vec<u64>>>>,
    // Another memory inefficient array made to just keep track of pages that are in frame_cache.
    pub pages_in_frames: Arc<SpinLock<Vec<u64>>>,
    pub last_checksum: (u32, u32), // Check of last frame in WAL, this is a cumulative checksum over all frames in the WAL
    pub file: Arc<dyn File>,
    /// read_locks is a list of read locks that can coexist with the max_frame number stored in
    /// value. There is a limited amount because and unbounded amount of connections could be
    /// fatal. Therefore, for now we copy how SQLite behaves with limited amounts of read max
    /// frames that is equal to 5
    pub read_locks: [LimboRwLock; 5],
    /// There is only one write allowed in WAL mode. This lock takes care of ensuring there is only
    /// one used.
    pub write_lock: LimboRwLock,
    pub loaded: AtomicBool,
}

impl fmt::Debug for WalFileShared {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WalFileShared")
            .field("wal_header", &self.wal_header)
            .field("min_frame", &self.min_frame)
            .field("max_frame", &self.max_frame)
            .field("nbackfills", &self.nbackfills)
            .field("frame_cache", &self.frame_cache)
            .field("pages_in_frames", &self.pages_in_frames)
            .field("last_checksum", &self.last_checksum)
            // Excluding `file`, `read_locks`, and `write_lock`
            .finish()
    }
}

impl Wal for WalFile {
    /// Begin a read transaction.
    fn begin_read_tx(&mut self) -> Result<LimboResult> {
        let max_frame_in_wal = self.get_shared().max_frame.load(Ordering::SeqCst);

        let mut max_read_mark = 0;
        let mut max_read_mark_index = -1;
        // Find the largest mark we can find, ignore frames that are impossible to be in range and
        // that are not set
        for (index, lock) in self.get_shared().read_locks.iter().enumerate() {
            let this_mark = lock.value.load(Ordering::SeqCst);
            if this_mark > max_read_mark && this_mark <= max_frame_in_wal as u32 {
                max_read_mark = this_mark;
                max_read_mark_index = index as i64;
            }
        }

        // If we didn't find any mark or we can update, let's update them
        if (max_read_mark as u64) < max_frame_in_wal || max_read_mark_index == -1 {
            for (index, lock) in self.get_shared().read_locks.iter_mut().enumerate() {
                let busy = !lock.write();
                if !busy {
                    // If this was busy then it must mean >1 threads tried to set this read lock
                    lock.value.store(max_frame_in_wal as u32, Ordering::SeqCst);
                    max_read_mark = max_frame_in_wal as u32;
                    max_read_mark_index = index as i64;
                    lock.unlock();
                    break;
                }
            }
        }

        if max_read_mark_index == -1 {
            return Ok(LimboResult::Busy);
        }

        let shared = self.get_shared();
        {
            let lock = &mut shared.read_locks[max_read_mark_index as usize];
            let busy = !lock.read();
            if busy {
                return Ok(LimboResult::Busy);
            }
        }
        self.min_frame = shared.nbackfills.load(Ordering::SeqCst) + 1;
        self.max_frame_read_lock_index = max_read_mark_index as usize;
        self.max_frame = max_read_mark as u64;
        tracing::debug!(
            "begin_read_tx(min_frame={}, max_frame={}, lock={}, max_frame_in_wal={})",
            self.min_frame,
            self.max_frame,
            self.max_frame_read_lock_index,
            max_frame_in_wal
        );
        Ok(LimboResult::Ok)
    }

    /// End a read transaction.
    #[inline(always)]
    fn end_read_tx(&self) -> Result<LimboResult> {
        tracing::debug!("end_read_tx");
        let read_lock = &mut self.get_shared().read_locks[self.max_frame_read_lock_index];
        read_lock.unlock();
        Ok(LimboResult::Ok)
    }

    /// Begin a write transaction
    fn begin_write_tx(&mut self) -> Result<LimboResult> {
        let busy = !self.get_shared().write_lock.write();
        tracing::debug!("begin_write_transaction(busy={})", busy);
        if busy {
            return Ok(LimboResult::Busy);
        }
        // Record the WAL state at the start of this write transaction so that
        // rollback() can restore it precisely.
        let shared = self.get_shared();
        self.txn_start_max_frame
            .set(shared.max_frame.load(Ordering::SeqCst));
        self.txn_start_last_checksum.set(shared.last_checksum);
        Ok(LimboResult::Ok)
    }

    /// End a write transaction
    fn end_write_tx(&self) -> Result<LimboResult> {
        tracing::debug!("end_write_txn");
        self.get_shared().write_lock.unlock();
        Ok(LimboResult::Ok)
    }

    /// Find the latest frame containing a page.
    fn find_frame(&self, page_id: u64) -> Result<Option<u64>> {
        let shared = self.get_shared();
        let frames = shared.frame_cache.lock();
        let frames = frames.get(&page_id);
        if frames.is_none() {
            return Ok(None);
        }
        let frames = frames.expect("frames must be Some after is_none() check");
        for frame in frames.iter().rev() {
            if *frame <= self.max_frame {
                return Ok(Some(*frame));
            }
        }
        Ok(None)
    }

    /// Read a frame from the WAL.
    fn read_frame(&self, frame_id: u64, page: PageRef, buffer_pool: Rc<BufferPool>) -> Result<()> {
        tracing::debug!("read_frame({})", frame_id);
        let offset = self.frame_offset(frame_id);
        page.set_locked();
        let frame = page.clone();
        let complete = Box::new(move |buf: Arc<RefCell<Buffer>>| {
            let frame = frame.clone();
            finish_read_page(page.get().id, buf, frame)
                .expect("finish_read_page failed in WAL read completion");
        });
        begin_read_wal_frame(
            &self.get_shared().file,
            offset + WAL_FRAME_HEADER_SIZE,
            buffer_pool,
            complete,
        )?;
        Ok(())
    }

    fn read_frame_raw(
        &self,
        frame_id: u64,
        buffer_pool: Rc<BufferPool>,
        frame: *mut u8,
        frame_len: u32,
    ) -> Result<Arc<Completion>> {
        tracing::debug!("read_frame({})", frame_id);
        let offset = self.frame_offset(frame_id);
        let complete = Box::new(move |buf: Arc<RefCell<Buffer>>| {
            let buf = buf.borrow();
            let buf_ptr = buf.as_ptr();
            unsafe {
                std::ptr::copy_nonoverlapping(buf_ptr, frame, frame_len as usize);
            }
        });
        let c = begin_read_wal_frame(
            &self.get_shared().file,
            offset + WAL_FRAME_HEADER_SIZE,
            buffer_pool,
            complete,
        )?;
        Ok(c)
    }

    /// Write a frame to the WAL.
    fn append_frame(
        &mut self,
        page: PageRef,
        db_size: u32,
        write_counter: Rc<RefCell<usize>>,
    ) -> Result<()> {
        let page_id = page.get().id;
        let shared = self.get_shared();
        let max_frame = shared.max_frame.load(Ordering::SeqCst);
        let frame_id = if max_frame == 0 { 1 } else { max_frame + 1 };
        let offset = self.frame_offset(frame_id);
        tracing::debug!(
            "append_frame(frame={}, offset={}, page_id={})",
            frame_id,
            offset,
            page_id
        );
        let header = shared.wal_header.clone();
        let header = header.lock();
        let checksums = shared.last_checksum;
        let checksums = begin_write_wal_frame(
            &shared.file,
            offset,
            &page,
            self.page_size as u16,
            db_size,
            write_counter,
            &header,
            checksums,
        )?;
        shared.last_checksum = checksums;
        shared.max_frame.store(frame_id, Ordering::SeqCst);
        {
            let mut frame_cache = shared.frame_cache.lock();
            let frames = frame_cache.get_mut(&(page_id as u64));
            match frames {
                Some(frames) => frames.push(frame_id),
                None => {
                    frame_cache.insert(page_id as u64, vec![frame_id]);
                    shared.pages_in_frames.lock().push(page_id as u64);
                }
            }
        }
        Ok(())
    }

    fn should_checkpoint(&self) -> bool {
        let shared = self.get_shared();
        let frame_id = shared.max_frame.load(Ordering::SeqCst) as usize;
        frame_id >= self.checkpoint_threshold
    }

    #[instrument(skip_all, level = Level::TRACE)]
    fn checkpoint(
        &mut self,
        pager: &Pager,
        write_counter: Rc<RefCell<usize>>,
        mode: CheckpointMode,
    ) -> Result<CheckpointStatus> {
        'checkpoint_loop: loop {
            let state = self.ongoing_checkpoint.state;
            tracing::debug!(?state);
            match state {
                CheckpointState::Start => {
                    // TODO(pere): check what frames are safe to checkpoint between many readers!
                    self.ongoing_checkpoint.min_frame = self.min_frame;
                    let shared = self.get_shared();
                    let mut max_safe_frame = shared.max_frame.load(Ordering::SeqCst);
                    for (read_lock_idx, read_lock) in shared.read_locks.iter_mut().enumerate() {
                        let this_mark = read_lock.value.load(Ordering::SeqCst);
                        if this_mark < max_safe_frame as u32 {
                            let busy = !read_lock.write();
                            if !busy {
                                let new_mark = if read_lock_idx == 0 {
                                    max_safe_frame as u32
                                } else {
                                    READMARK_NOT_USED
                                };
                                read_lock.value.store(new_mark, Ordering::SeqCst);
                                read_lock.unlock();
                            } else {
                                max_safe_frame = this_mark as u64;
                            }
                        }
                    }
                    self.ongoing_checkpoint.max_frame = max_safe_frame;
                    self.ongoing_checkpoint.current_page = 0;
                    self.ongoing_checkpoint.touched_pages.clear();
                    self.ongoing_checkpoint.state = CheckpointState::ReadFrame;
                    tracing::trace!(
                        "checkpoint_start(min_frame={}, max_frame={})",
                        self.ongoing_checkpoint.max_frame,
                        self.ongoing_checkpoint.min_frame
                    );
                }
                CheckpointState::ReadFrame => {
                    let shared = self.get_shared();
                    let min_frame = self.ongoing_checkpoint.min_frame;
                    let max_frame = self.ongoing_checkpoint.max_frame;
                    let pages_in_frames = shared.pages_in_frames.clone();
                    let pages_in_frames = pages_in_frames.lock();

                    let frame_cache = shared.frame_cache.clone();
                    let frame_cache = frame_cache.lock();
                    assert!(self.ongoing_checkpoint.current_page as usize <= pages_in_frames.len());
                    if self.ongoing_checkpoint.current_page as usize == pages_in_frames.len() {
                        self.ongoing_checkpoint.state = CheckpointState::Done;
                        continue 'checkpoint_loop;
                    }
                    let page = pages_in_frames[self.ongoing_checkpoint.current_page as usize];
                    let frames = frame_cache
                        .get(&page)
                        .expect("page must be in frame cache if it's in list");

                    for frame in frames.iter().rev() {
                        if *frame >= min_frame && *frame <= max_frame {
                            tracing::debug!(
                                "checkpoint page(state={:?}, page={}, frame={})",
                                state,
                                page,
                                *frame
                            );
                            self.ongoing_checkpoint.page.get().id = page as usize;
                            self.ongoing_checkpoint.touched_pages.push(page);

                            self.read_frame(
                                *frame,
                                self.ongoing_checkpoint.page.clone(),
                                self.buffer_pool.clone(),
                            )?;
                            self.ongoing_checkpoint.state = CheckpointState::WaitReadFrame;
                            continue 'checkpoint_loop;
                        }
                    }
                    self.ongoing_checkpoint.current_page += 1;
                }
                CheckpointState::WaitReadFrame => {
                    if self.ongoing_checkpoint.page.is_locked() {
                        return Ok(CheckpointStatus::IO);
                    } else {
                        self.ongoing_checkpoint.state = CheckpointState::WritePage;
                    }
                }
                CheckpointState::WritePage => {
                    self.ongoing_checkpoint.page.set_dirty();
                    begin_write_btree_page(
                        pager,
                        &self.ongoing_checkpoint.page,
                        write_counter.clone(),
                    )?;
                    self.ongoing_checkpoint.state = CheckpointState::WaitWritePage;
                }
                CheckpointState::WaitWritePage => {
                    if *write_counter.borrow() > 0 {
                        return Ok(CheckpointStatus::IO);
                    }
                    let shared = self.get_shared();
                    if (self.ongoing_checkpoint.current_page as usize)
                        < shared.pages_in_frames.lock().len()
                    {
                        self.ongoing_checkpoint.current_page += 1;
                        self.ongoing_checkpoint.state = CheckpointState::ReadFrame;
                    } else {
                        self.ongoing_checkpoint.state = CheckpointState::Done;
                    }
                }
                CheckpointState::Done => {
                    if *write_counter.borrow() > 0 {
                        return Ok(CheckpointStatus::IO);
                    }
                    // Taken before `self.get_shared()` below: `get_shared`'s
                    // return value keeps an (elided) immutable borrow of
                    // `self` alive for the rest of this arm, which would
                    // otherwise conflict with this mutable borrow of a
                    // different field of `self`.
                    let checkpointed_page_ids =
                        std::mem::take(&mut self.ongoing_checkpoint.touched_pages);
                    let shared = self.get_shared();

                    // Record two num pages fields to return as checkpoint result to caller.
                    // Ref: pnLog, pnCkpt on https://www.sqlite.org/c3ref/wal_checkpoint_v2.html
                    let checkpoint_result = CheckpointResult {
                        num_wal_frames: shared.max_frame.load(Ordering::SeqCst),
                        num_checkpointed_frames: self.ongoing_checkpoint.max_frame,
                        checkpointed_page_ids,
                    };
                    let everything_backfilled = shared.max_frame.load(Ordering::SeqCst)
                        == self.ongoing_checkpoint.max_frame;
                    if everything_backfilled {
                        // Everything currently in the WAL has been safely
                        // copied into the main db file, so it is safe to start
                        // a brand new WAL "generation" here -- regardless of
                        // checkpoint mode. This drops the now fully-redundant
                        // frame bookkeeping and evolves checkpoint_seq/salts
                        // via `reset_wal_file`, matching the `WalHeader` field
                        // doc comments and mirroring how SQLite restarts the
                        // physical log once it has been fully backfilled.
                        // Only `Truncate` additionally shrinks the on-disk
                        // file to zero bytes.
                        shared.frame_cache.lock().clear();
                        shared.pages_in_frames.lock().clear();
                        shared.max_frame.store(0, Ordering::SeqCst);
                        shared.nbackfills.store(0, Ordering::SeqCst);
                        self.reset_wal_file(matches!(mode, CheckpointMode::Truncate))?;
                    } else {
                        // Not everything could be backfilled this pass (e.g.
                        // an active reader is still pinned to an older
                        // snapshot). Advance the backfill watermark to
                        // whatever we *did* manage to checkpoint just now...
                        shared
                            .nbackfills
                            .store(self.ongoing_checkpoint.max_frame, Ordering::SeqCst);
                        // ...and opportunistically discard frame_cache /
                        // pages_in_frames entries that are now durably in the
                        // main db file, so these structures don't grow
                        // without bound for the lifetime of a long-running
                        // WAL connection (see `trim_backfilled_frames`).
                        self.trim_backfilled_frames(self.ongoing_checkpoint.max_frame);
                    }
                    self.ongoing_checkpoint.state = CheckpointState::Start;
                    return Ok(CheckpointStatus::Done(checkpoint_result));
                }
            }
        }
    }

    #[instrument(skip_all, level = Level::DEBUG)]
    fn sync(&mut self) -> Result<WalFsyncStatus> {
        match self.sync_state.get() {
            SyncState::NotSyncing => {
                tracing::debug!("wal_sync");
                let syncing = self.syncing.clone();
                self.syncing.set(true);
                let completion = Completion::Sync(SyncCompletion {
                    complete: Box::new(move |_| {
                        tracing::debug!("wal_sync finish");
                        syncing.set(false);
                    }),
                    is_completed: Cell::new(false),
                });
                let shared = self.get_shared();
                shared.file.sync(Arc::new(completion))?;
                self.sync_state.set(SyncState::Syncing);
                Ok(WalFsyncStatus::IO)
            }
            SyncState::Syncing => {
                if self.syncing.get() {
                    tracing::debug!("wal_sync is already syncing");
                    Ok(WalFsyncStatus::IO)
                } else {
                    self.sync_state.set(SyncState::NotSyncing);
                    Ok(WalFsyncStatus::Done)
                }
            }
        }
    }

    fn get_max_frame_in_wal(&self) -> u64 {
        self.get_shared().max_frame.load(Ordering::SeqCst)
    }

    fn get_max_frame(&self) -> u64 {
        self.max_frame
    }

    fn get_min_frame(&self) -> u64 {
        self.min_frame
    }

    /// Roll back the current write transaction.
    ///
    /// Restores `shared.max_frame` and `shared.last_checksum` to the snapshot
    /// captured in `begin_write_tx`, then removes from the frame cache every
    /// frame that was appended during this transaction.
    fn rollback(&self) {
        let start_frame = self.txn_start_max_frame.get();
        let start_checksum = self.txn_start_last_checksum.get();
        self.rollback_to_frame(start_frame, start_checksum)
            .expect("wal rollback_to_frame failed unexpectedly");

        tracing::debug!(
            "wal_rollback(start_frame={}, restored_max_frame={})",
            start_frame,
            self.get_shared().max_frame.load(Ordering::SeqCst),
        );
    }

    fn current_frame_state(&self) -> (u64, (u32, u32)) {
        let shared = self.get_shared();
        let max_frame = shared.max_frame.load(Ordering::SeqCst);
        let checksum = shared.last_checksum;
        (max_frame, checksum)
    }

    fn rollback_to_frame(&self, target_frame: u64, target_checksum: (u32, u32)) -> Result<()> {
        let shared = self.get_shared();
        {
            let mut frame_cache = shared.frame_cache.lock();
            let mut pages_in_frames = shared.pages_in_frames.lock();
            // Remove frames > target_frame from each page's list; drop empty entries.
            frame_cache.retain(|_page_id, frames| {
                frames.retain(|&frame| frame <= target_frame);
                !frames.is_empty()
            });
            // Rebuild pages_in_frames to match the pruned frame_cache.
            pages_in_frames.retain(|page_id| frame_cache.contains_key(page_id));
        }
        // Restore watermark and cumulative checksum.
        shared.max_frame.store(target_frame, Ordering::SeqCst);
        shared.last_checksum = target_checksum;
        tracing::debug!(
            "wal_rollback_to_frame(target_frame={}, restored_max_frame={})",
            target_frame,
            shared.max_frame.load(Ordering::SeqCst),
        );
        Ok(())
    }

    fn set_reader_max_frame(&mut self, frame: u64) {
        self.max_frame = frame;
    }
}

impl WalFile {
    pub fn new(
        io: Arc<dyn IO>,
        page_size: u32,
        shared: Arc<UnsafeCell<WalFileShared>>,
        buffer_pool: Rc<BufferPool>,
    ) -> Self {
        let checkpoint_page = Arc::new(Page::new(0));
        let buffer = buffer_pool.get();
        {
            let buffer_pool = buffer_pool.clone();
            let drop_fn = Rc::new(move |buf| {
                buffer_pool.put(buf);
            });
            checkpoint_page.get().contents = Some(PageContent::new(
                0,
                Arc::new(RefCell::new(Buffer::new(buffer, drop_fn))),
            ));
        }
        Self {
            io,
            shared,
            ongoing_checkpoint: OngoingCheckpoint {
                page: checkpoint_page,
                state: CheckpointState::Start,
                min_frame: 0,
                max_frame: 0,
                current_page: 0,
                touched_pages: Vec::new(),
            },
            checkpoint_threshold: 1000,
            page_size,
            buffer_pool,
            syncing: Rc::new(Cell::new(false)),
            sync_state: Cell::new(SyncState::NotSyncing),
            max_frame: 0,
            min_frame: 0,
            max_frame_read_lock_index: 0,
            txn_start_max_frame: Cell::new(0),
            txn_start_last_checksum: Cell::new((0, 0)),
        }
    }

    fn frame_offset(&self, frame_id: u64) -> usize {
        assert!(frame_id > 0, "Frame ID must be 1-based");
        let page_size = self.page_size;
        let page_offset = (frame_id - 1) * (page_size + WAL_FRAME_HEADER_SIZE as u32) as u64;
        let offset = WAL_HEADER_SIZE as u64 + page_offset;
        offset as usize
    }

    #[allow(clippy::mut_from_ref)]
    fn get_shared(&self) -> &mut WalFileShared {
        unsafe {
            self.shared
                .get()
                .as_mut()
                .expect("WalFileShared pointer must be non-null")
        }
    }

    /// Reset the on-disk WAL after a checkpoint that fully backfilled the log
    /// into the main database file.
    ///
    /// This is called for every [`CheckpointMode`] once a checkpoint achieves
    /// a full backfill (see the `everything_backfilled` branch in
    /// [`WalFile::checkpoint`]), not just `Restart`/`Truncate`: any checkpoint
    /// that catches the WAL up completely starts a brand new WAL "generation",
    /// exactly like SQLite restarting its log once nothing is left to
    /// checkpoint.
    ///
    /// Evolves the 32-byte WAL header so any leftover frames still physically
    /// on disk (we don't necessarily truncate the file -- see below) are
    /// rejected by a future recovery via the salt-mismatch check:
    /// `checkpoint_seq` is incremented, `salt_1` is incremented (not
    /// re-randomized), and `salt_2` gets a fresh random value. This matches
    /// the doc comments on the `WalHeader` fields and mirrors SQLite's own
    /// convention for distinguishing WAL generations during crash recovery.
    /// The in-memory cumulative checksum is reset to match. When `truncate` is
    /// true, the WAL file is also physically shrunk to zero bytes and a fresh
    /// empty 32-byte header is rewritten, so the on-disk `-wal` carries no
    /// frames (max_frame 0) and a byte-level reader sees a self-contained
    /// database file.
    ///
    /// Correctness note: it is only safe to bump the salt here because the
    /// caller only invokes this once every frame currently in the WAL has
    /// already been durably copied into the main database file (i.e.
    /// `shared.max_frame` has just been reset to 0 and `frame_cache`/
    /// `pages_in_frames` have just been cleared by the caller). Bumping the
    /// salt while older, not-yet-backfilled frames were still relied upon
    /// would strand them: they'd carry the OLD salt while the header now
    /// advertises the NEW one, so a crash-recovery pass would reject them at
    /// the first mismatch and silently lose data that was never checkpointed.
    fn reset_wal_file(&self, truncate: bool) -> Result<()> {
        let shared = self.get_shared();
        // Evolve the header for the new WAL generation and recompute the
        // checksum over the first 24 bytes.
        {
            let mut header = shared.wal_header.lock();
            header.checkpoint_seq = header.checkpoint_seq.wrapping_add(1);
            header.salt_1 = header.salt_1.wrapping_add(1);
            header.salt_2 = self.io.generate_random_number() as u32;
            let native = cfg!(target_endian = "big");
            let checksums = checksum_wal(
                &header.as_bytes()[..WAL_HEADER_SIZE - 2 * 4],
                &header,
                (0, 0),
                native,
            );
            header.checksum_1 = checksums.0;
            header.checksum_2 = checksums.1;
            shared.last_checksum = checksums;
        }
        if truncate {
            // Physically shrink the WAL file to 0 bytes, awaiting completion.
            let truncated = Rc::new(Cell::new(false));
            let truncated_cb = truncated.clone();
            let completion = Completion::Sync(SyncCompletion {
                complete: Box::new(move |_| {
                    truncated_cb.set(true);
                }),
                is_completed: Cell::new(false),
            });
            shared.file.truncate(0, Arc::new(completion))?;
            let mut attempts = 0;
            while !truncated.get() {
                self.io.run_once()?;
                attempts += 1;
                if attempts >= 1000 {
                    return Err(crate::error::LimboError::InternalError(
                        "failed to truncate WAL file".to_string(),
                    ));
                }
            }
        }
        // Rewrite a valid, empty 32-byte header (fresh salt already set above) so
        // subsequent in-process appends start after a well-formed header. After a
        // physical truncate this makes the file exactly 32 bytes (zero frames);
        // for Restart it rewrites the header in place.
        {
            let header = shared.wal_header.lock();
            sqlite3_ondisk::begin_write_wal_header(&shared.file, &header)?;
        }
        Ok(())
    }

    /// Discard `frame_cache`/`pages_in_frames` entries for frames that are now
    /// durably present in the main database file, i.e. frames at or below
    /// `backfilled_up_to`.
    ///
    /// This is called after a checkpoint pass that could *not* fully backfill
    /// the WAL (some frames above `backfilled_up_to` are still needed), so
    /// unlike the full reset in [`WalFile::checkpoint`]'s `everything_backfilled`
    /// branch, this only prunes the *safe* prefix rather than clearing
    /// everything. Without this, `frame_cache`/`pages_in_frames` would grow
    /// without bound for the lifetime of any long-running WAL-mode connection
    /// that has at least one reader briefly outliving a checkpoint (every
    /// `Passive` checkpoint would otherwise only ever append, never reclaim).
    ///
    /// Safety argument: `backfilled_up_to` is always
    /// `self.ongoing_checkpoint.max_frame`, the `max_safe_frame` a checkpoint
    /// pass computes for itself in `CheckpointState::Start`. That computation
    /// already walks every slot in `shared.read_locks` and clamps the value
    /// down to the pinned snapshot of any *currently active* reader, so it can
    /// never exceed an active reader's `max_frame`. Consequently, for every
    /// currently (or future, since new readers only ever adopt a mark
    /// `>= backfilled_up_to`, see `begin_read_tx`) active reader:
    ///   * a frame above `backfilled_up_to` that the reader still needs
    ///     remains untouched in `frame_cache` (we only remove frames
    ///     `<= backfilled_up_to`), so `find_frame` keeps finding it directly, or
    ///   * the reader needs "the latest frame `<= backfilled_up_to`" for a
    ///     page, which is *exactly* the version this checkpoint pass just
    ///     copied into the main db file (see `CheckpointState::ReadFrame`'s
    ///     `frame >= min_frame && frame <= max_frame` selection) -- so once
    ///     `find_frame` returns `None` for the trimmed entries, falling back
    ///     to reading the main db file yields identical content.
    ///
    /// Frames above `backfilled_up_to` are left completely untouched: they
    /// have not been copied to the db file yet and may still be needed by the
    /// writer or by any reader.
    fn trim_backfilled_frames(&self, backfilled_up_to: u64) {
        if backfilled_up_to == 0 {
            // Nothing has been backfilled yet -- frame ids are 1-based, so
            // there is nothing safe to trim.
            return;
        }
        let shared = self.get_shared();
        let mut frame_cache = shared.frame_cache.lock();
        let mut pages_in_frames = shared.pages_in_frames.lock();
        frame_cache.retain(|_page_id, frames| {
            frames.retain(|&frame| frame > backfilled_up_to);
            !frames.is_empty()
        });
        pages_in_frames.retain(|page_id| frame_cache.contains_key(page_id));
    }
}

impl WalFileShared {
    pub fn open_shared(
        io: &Arc<dyn IO>,
        path: &str,
        page_size: u32,
    ) -> Result<Arc<UnsafeCell<WalFileShared>>> {
        Self::open_shared_inner(io, path, page_size, false)
    }

    /// Open the shared WAL state for `path`.
    ///
    /// When `db_freshly_created` is true, the accompanying main database file
    /// did not exist (or was empty) and was just bootstrapped by
    /// [`crate::maybe_init_database_file`]. Any `-wal` file present on disk is
    /// therefore an orphan left behind by a previous database incarnation
    /// (e.g. the main `.db` was deleted while its `-wal` survived). Replaying
    /// such a WAL would resurrect stale committed pages on top of the fresh
    /// database, corrupting it (the classic symptom is row counts that grow by
    /// the previous content on every reopen). In that case we discard the
    /// orphaned WAL and start a fresh one instead of recovering from it.
    pub fn open_shared_inner(
        io: &Arc<dyn IO>,
        path: &str,
        page_size: u32,
        db_freshly_created: bool,
    ) -> Result<Arc<UnsafeCell<WalFileShared>>> {
        let file = io.open_file(path, crate::io::OpenFlags::Create, false)?;
        let orphaned_wal = db_freshly_created && file.size()? > 0;
        if orphaned_wal {
            tracing::warn!(
                "discarding orphaned WAL at {:?}: main database file was freshly created, \
                 so this WAL belongs to a previous database incarnation and will not be replayed",
                path
            );
        }
        // Recover from an existing WAL only when it is NOT orphaned. For an
        // orphaned WAL we fall through to the fresh-header branch below, which
        // overwrites the 32-byte WAL header with a brand-new random salt. That
        // both (a) starts this session at max_frame = 0 (so none of the stale
        // frames are visible now) and (b) guarantees every leftover frame still
        // on disk carries the OLD salt, so a future `read_entire_wal_dumb`
        // recovery rejects them at the first salt check and never replays them.
        let header = if !orphaned_wal && file.size()? > 0 {
            let (wal_file_shared, parse_error) = sqlite3_ondisk::read_entire_wal_dumb(&file)?;
            let mut max_loops = 100_000;
            while !unsafe { &*wal_file_shared.get() }
                .loaded
                .load(Ordering::SeqCst)
            {
                io.run_once()?;
                max_loops -= 1;
                if max_loops == 0 {
                    return Err(crate::error::LimboError::InternalError(
                        "WAL file not loaded after 100000 IO iterations".to_string(),
                    ));
                }
            }
            // If parsing the WAL detected corruption, surface it now.
            if let Some(err) = parse_error.lock().take() {
                return Err(err);
            }
            return Ok(wal_file_shared);
        } else {
            let magic = if cfg!(target_endian = "big") {
                WAL_MAGIC_BE
            } else {
                WAL_MAGIC_LE
            };
            let mut wal_header = WalHeader {
                magic,
                file_format: 3007000,
                page_size,
                checkpoint_seq: 0, // TODO implement sequence number
                salt_1: io.generate_random_number() as u32,
                salt_2: io.generate_random_number() as u32,
                checksum_1: 0,
                checksum_2: 0,
            };
            let native = cfg!(target_endian = "big"); // if target_endian is
                                                      // already big then we don't care but if isn't, header hasn't yet been
                                                      // encoded to big endian, therefore we want to swap bytes to compute this
                                                      // checksum.
            let checksums = (0, 0);
            let checksums = checksum_wal(
                &wal_header.as_bytes()[..WAL_HEADER_SIZE - 2 * 4], // first 24 bytes
                &wal_header,
                checksums,
                native, // this is false because we haven't encoded the wal header yet
            );
            wal_header.checksum_1 = checksums.0;
            wal_header.checksum_2 = checksums.1;
            sqlite3_ondisk::begin_write_wal_header(&file, &wal_header)?;
            Arc::new(SpinLock::new(wal_header))
        };
        let checksum = {
            let checksum = header.lock();
            (checksum.checksum_1, checksum.checksum_2)
        };
        let shared = WalFileShared {
            wal_header: header,
            min_frame: AtomicU64::new(0),
            max_frame: AtomicU64::new(0),
            nbackfills: AtomicU64::new(0),
            frame_cache: Arc::new(SpinLock::new(HashMap::new())),
            last_checksum: checksum,
            file,
            pages_in_frames: Arc::new(SpinLock::new(Vec::new())),
            read_locks: array::from_fn(|_| LimboRwLock {
                lock: AtomicU32::new(NO_LOCK),
                nreads: AtomicU32::new(0),
                value: AtomicU32::new(READMARK_NOT_USED),
            }),
            write_lock: LimboRwLock {
                lock: AtomicU32::new(NO_LOCK),
                nreads: AtomicU32::new(0),
                value: AtomicU32::new(READMARK_NOT_USED),
            },
            loaded: AtomicBool::new(true),
        };
        Ok(Arc::new(UnsafeCell::new(shared)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::{MemoryIO, OpenFlags};
    use crate::storage::database::{DatabaseFile, DatabaseStorage};
    use crate::storage::page_cache::DumbLruPageCache;
    use crate::storage::pager::{Pager, PagerCacheflushStatus};
    use crate::storage::sqlite3_ondisk::DatabaseHeader;
    use parking_lot::RwLock;

    const TEST_PAGE_SIZE: u32 = 4096;

    /// Shared plumbing for a single in-memory database: an `IO`, the main
    /// `.db` file storage, the WAL's cross-connection shared state, and the
    /// database header. Calling [`TestDb::connect`] multiple times simulates
    /// multiple independent connections to the *same* database, the way
    /// `Database::connect()` would -- each gets its own `WalFile` view (own
    /// `min_frame`/`max_frame`/read-lock slot) and its own page cache, but
    /// all share the same underlying WAL bookkeeping and main db file.
    struct TestDb {
        io: Arc<dyn IO>,
        db_storage: Arc<dyn DatabaseStorage>,
        wal_shared: Arc<UnsafeCell<WalFileShared>>,
        db_header: Arc<SpinLock<DatabaseHeader>>,
    }

    impl TestDb {
        fn new(page_size: u32) -> Self {
            let io: Arc<dyn IO> = Arc::new(MemoryIO::new());
            let db_file_raw = io
                .open_file("wal_test.db", OpenFlags::Create, false)
                .expect("open_file should succeed against MemoryIO");
            let db_storage: Arc<dyn DatabaseStorage> = Arc::new(DatabaseFile::new(db_file_raw));
            let wal_shared = WalFileShared::open_shared(&io, "wal_test.db-wal", page_size)
                .expect("open_shared should succeed for a fresh in-memory WAL");
            let mut header_data = DatabaseHeader::default();
            header_data.update_page_size(page_size);
            let db_header = Arc::new(SpinLock::new(header_data));
            Self {
                io,
                db_storage,
                wal_shared,
                db_header,
            }
        }

        /// Open a new independent "connection" to the same underlying
        /// database: a fresh `WalFile` view (its own read-lock slot / frame
        /// bounds once it begins a transaction) and a fresh, empty page
        /// cache, so a `read_page` call is guaranteed to consult the WAL /
        /// main db file rather than an already-cached copy.
        fn connect(&self, page_size: u32) -> Pager {
            let buffer_pool = Rc::new(BufferPool::new(page_size as usize));
            let page_cache = Arc::new(RwLock::new(DumbLruPageCache::new(1000)));
            let wal = Rc::new(RefCell::new(WalFile::new(
                self.io.clone(),
                page_size,
                self.wal_shared.clone(),
                buffer_pool.clone(),
            )));
            Pager::finish_open(
                self.db_header.clone(),
                self.db_storage.clone(),
                wal,
                self.io.clone(),
                page_cache,
                buffer_pool,
            )
            .expect("finish_open should succeed")
        }

        fn shared(&self) -> &WalFileShared {
            unsafe { &*self.wal_shared.get() }
        }
    }

    /// Begin a fresh read+write transaction on `pager`, run `f`, then drive
    /// `Pager::end_tx` to completion so every page `f` dirtied is appended to
    /// the WAL as a frame. Mirrors the begin_read_tx -> ... -> end_tx sequence
    /// the VDBE drives around every statement (see
    /// `vdbe/execute/txn_schema.rs`).
    fn with_write_txn(pager: &Pager, f: impl FnOnce(&Pager)) {
        assert!(matches!(
            pager.begin_read_tx().expect("begin_read_tx"),
            LimboResult::Ok
        ));
        assert!(matches!(
            pager.begin_write_tx().expect("begin_write_tx"),
            LimboResult::Ok
        ));
        f(pager);
        while let PagerCacheflushStatus::IO = pager.end_tx().expect("end_tx") {
            pager.io.run_once().expect("run_once");
        }
    }

    /// Allocate a brand new page, stamp `marker` into its first byte, and
    /// return its page id. Must be called inside `with_write_txn` (the page
    /// must be flushed for the marker to become a WAL frame).
    fn allocate_marked_page(pager: &Pager, marker: u8) -> usize {
        let page = pager.allocate_page().expect("allocate_page");
        let id = page.get().id;
        page.get().contents.as_ref().expect("contents").as_ptr()[0] = marker;
        id
    }

    /// Re-read an existing page, stamp `marker` into its first byte, and mark
    /// it dirty again. Must be called inside `with_write_txn`.
    fn rewrite_marked_page(pager: &Pager, page_id: usize, marker: u8) {
        let page = pager.read_page(page_id).expect("read_page");
        let mut spins = 0;
        while page.is_locked() {
            pager.io.run_once().expect("run_once");
            spins += 1;
            assert!(spins < 10_000, "page {page_id} stuck locked");
        }
        page.get().contents.as_ref().expect("contents").as_ptr()[0] = marker;
        page.set_dirty();
        pager.add_dirty(page_id);
    }

    /// Read a page's first byte through `pager`, driving any pending I/O
    /// first. `pager` must not already have `page_id` loaded in its own page
    /// cache from an earlier call, or this would just return the stale cached
    /// copy instead of exercising `find_frame` / the main db file fallback.
    fn read_marker(pager: &Pager, page_id: usize) -> u8 {
        let page = pager.read_page(page_id).expect("read_page");
        let mut spins = 0;
        while page.is_locked() {
            pager.io.run_once().expect("run_once");
            spins += 1;
            assert!(spins < 10_000, "page {page_id} stuck locked");
        }
        page.get().contents.as_ref().expect("contents").as_ptr()[0]
    }

    /// Drive a blocking checkpoint of `mode` to completion.
    fn checkpoint(pager: &Pager, mode: CheckpointMode) -> CheckpointResult {
        pager
            .wal_checkpoint_mode(mode)
            .expect("checkpoint should succeed")
    }

    #[test]
    fn dummy_wal_read_frame_raw_returns_err_instead_of_todo() {
        let dummy = DummyWAL;
        let buffer_pool = Rc::new(BufferPool::new(TEST_PAGE_SIZE as usize));
        let mut buf = [0u8; 16];
        let result = dummy.read_frame_raw(1, buffer_pool, buf.as_mut_ptr(), buf.len() as u32);
        assert!(
            result.is_err(),
            "DummyWAL::read_frame_raw must return an Err instead of panicking via todo!()"
        );
    }

    /// Directly exercises `WalFile::trim_backfilled_frames`'s boundary
    /// semantics without going through a full checkpoint: frames at or below
    /// the boundary are removed, frames above it are untouched, and a page
    /// whose every frame was trimmed is dropped from `pages_in_frames` too.
    #[test]
    fn trim_backfilled_frames_boundary_is_inclusive() {
        let io: Arc<dyn IO> = Arc::new(MemoryIO::new());
        let buffer_pool = Rc::new(BufferPool::new(TEST_PAGE_SIZE as usize));
        let shared = WalFileShared::open_shared(&io, "trim_test.db-wal", TEST_PAGE_SIZE)
            .expect("open_shared");
        let wal_file = WalFile::new(io.clone(), TEST_PAGE_SIZE, shared.clone(), buffer_pool);

        {
            let s = unsafe { &*shared.get() };
            let mut frame_cache = s.frame_cache.lock();
            frame_cache.insert(1, vec![1, 3, 5]); // page 1 seen at frames 1, 3, 5
            frame_cache.insert(2, vec![2, 4]); // page 2 seen at frames 2, 4
            frame_cache.insert(3, vec![10]); // page 3 only ever seen at frame 10
            drop(frame_cache);
            s.pages_in_frames.lock().extend_from_slice(&[1, 2, 3]);
        }

        // Trim everything at or below frame 4: page 1 keeps only frame 5;
        // page 2 has nothing left above the boundary and must be dropped
        // entirely (both from frame_cache and pages_in_frames); page 3's
        // frame 10 is untouched.
        wal_file.trim_backfilled_frames(4);

        let s = unsafe { &*shared.get() };
        {
            let frame_cache = s.frame_cache.lock();
            assert_eq!(frame_cache.get(&1), Some(&vec![5]));
            assert_eq!(
                frame_cache.get(&2),
                None,
                "page 2 had no frames left above the boundary, its key must be removed"
            );
            assert_eq!(frame_cache.get(&3), Some(&vec![10]));
        }
        {
            let mut pages_in_frames = s.pages_in_frames.lock().clone();
            pages_in_frames.sort_unstable();
            assert_eq!(
                pages_in_frames,
                vec![1, 3],
                "page 2 must be removed from pages_in_frames once its frame_cache entry is gone"
            );
        }

        // A boundary of 0 (nothing backfilled yet) must be a strict no-op.
        wal_file.trim_backfilled_frames(0);
        let frame_cache = s.frame_cache.lock();
        assert_eq!(frame_cache.get(&1), Some(&vec![5]));
        assert_eq!(frame_cache.get(&3), Some(&vec![10]));
    }

    /// checkpoint_seq/salt_1/salt_2 must evolve on *every* checkpoint mode
    /// that fully backfills the WAL, not just Restart/Truncate -- matching
    /// the `WalHeader` field doc comments.
    #[test]
    fn checkpoint_evolves_checkpoint_seq_and_salts_for_every_mode() {
        let db = TestDb::new(TEST_PAGE_SIZE);
        let p1 = db.connect(TEST_PAGE_SIZE);

        let (mut prev_seq, mut prev_salt1, mut prev_salt2) = {
            let header = db.shared().wal_header.lock();
            (header.checkpoint_seq, header.salt_1, header.salt_2)
        };

        let modes = [
            CheckpointMode::Passive,
            CheckpointMode::Passive,
            CheckpointMode::Restart,
            CheckpointMode::Passive,
            CheckpointMode::Truncate,
            CheckpointMode::Passive,
        ];
        for (i, mode) in modes.into_iter().enumerate() {
            with_write_txn(&p1, |pager| {
                let _ = allocate_marked_page(pager, i as u8);
            });
            checkpoint(&p1, mode);

            let (seq, salt1, salt2) = {
                let header = db.shared().wal_header.lock();
                (header.checkpoint_seq, header.salt_1, header.salt_2)
            };
            assert_eq!(
                seq,
                prev_seq.wrapping_add(1),
                "checkpoint_seq must increase on checkpoint #{i} (mode {mode:?})"
            );
            assert_eq!(
                salt1,
                prev_salt1.wrapping_add(1),
                "salt_1 must be incremented (not re-randomized) on checkpoint #{i} (mode {mode:?})"
            );
            assert_ne!(
                salt2, prev_salt2,
                "salt_2 must take a fresh random value on checkpoint #{i} (mode {mode:?})"
            );

            prev_seq = seq;
            prev_salt1 = salt1;
            prev_salt2 = salt2;
        }
    }

    /// The real bug fix: with no active readers, every `Passive` checkpoint
    /// fully backfills the WAL, so `frame_cache`/`pages_in_frames` must be
    /// reclaimed after *every single cycle* -- i.e. bounded at (in fact,
    /// exactly) zero -- rather than growing linearly with the number of
    /// write+checkpoint cycles that have run over the connection's lifetime.
    #[test]
    fn passive_checkpoint_reclaims_frame_cache_with_no_active_readers() {
        let db = TestDb::new(TEST_PAGE_SIZE);
        let p1 = db.connect(TEST_PAGE_SIZE);

        const CYCLES: usize = 300;
        for i in 0..CYCLES {
            with_write_txn(&p1, |pager| {
                let _ = allocate_marked_page(pager, (i % 256) as u8);
            });
            checkpoint(&p1, CheckpointMode::Passive);

            let (frame_cache_len, pages_len) = {
                let shared = db.shared();
                (
                    shared.frame_cache.lock().len(),
                    shared.pages_in_frames.lock().len(),
                )
            };
            assert_eq!(
                frame_cache_len, 0,
                "frame_cache must be empty after cycle {i} (no active readers pinned an old snapshot)"
            );
            assert_eq!(
                pages_len, 0,
                "pages_in_frames must be empty after cycle {i} (no active readers pinned an old snapshot)"
            );
        }
    }

    /// The safety property the bounded-growth fix depends on: a reader
    /// pinned to an old snapshot must keep seeing correct content across many
    /// intervening write+Passive-checkpoint cycles (which trim frame_cache
    /// down to that reader's own safe boundary), and once the reader
    /// releases its snapshot, the next checkpoint must reclaim everything --
    /// proving growth was bounded by the reader's lifetime, not unbounded.
    #[test]
    fn concurrent_reader_snapshot_survives_checkpoints_and_frame_cache_is_reclaimed_after_release()
    {
        let db = TestDb::new(TEST_PAGE_SIZE);
        let p1 = db.connect(TEST_PAGE_SIZE);

        let mut hot_page_id = 0usize;
        with_write_txn(&p1, |pager| {
            hot_page_id = allocate_marked_page(pager, 0xAA);
        });

        // A second connection begins a read transaction now, pinning its
        // snapshot to include the write above but nothing that follows.
        let p2 = db.connect(TEST_PAGE_SIZE);
        assert!(matches!(
            p2.begin_read_tx().expect("begin_read_tx"),
            LimboResult::Ok
        ));

        const CYCLES: usize = 150;
        for i in 0..CYCLES {
            with_write_txn(&p1, |pager| {
                rewrite_marked_page(pager, hot_page_id, (i % 256) as u8);
            });
            checkpoint(&p1, CheckpointMode::Passive);
        }

        // p2 is still pinned to the old snapshot: it must still see the
        // ORIGINAL marker, not any of the CYCLES subsequent rewrites, even
        // though many Passive checkpoints ran while it was active and
        // trimmed frame_cache down to the safe boundary in the meantime.
        assert_eq!(
            read_marker(&p2, hot_page_id),
            0xAA,
            "a reader pinned to an old snapshot must be unaffected by later writes/checkpoints"
        );

        // Release the reader and checkpoint once more: nothing pins an old
        // snapshot anymore, so this checkpoint must fully reclaim
        // frame_cache/pages_in_frames.
        p2.end_read_tx().expect("end_read_tx");
        checkpoint(&p1, CheckpointMode::Passive);

        let (frame_cache_len, pages_len) = {
            let shared = db.shared();
            (
                shared.frame_cache.lock().len(),
                shared.pages_in_frames.lock().len(),
            )
        };
        assert_eq!(
            frame_cache_len, 0,
            "frame_cache must be reclaimed once the blocking reader is gone"
        );
        assert_eq!(
            pages_len, 0,
            "pages_in_frames must be reclaimed once the blocking reader is gone"
        );
    }

    /// Stress-tests the multi-reader `max_safe_frame` computation in
    /// `CheckpointState::Start` (the ":741" logic): three readers pinned at
    /// three different snapshots must each keep seeing exactly the content
    /// that was current when they began, independent of each other and of
    /// the many write+checkpoint cycles that run after all three are active.
    #[test]
    fn multiple_readers_at_different_snapshots_each_see_correct_content() {
        let db = TestDb::new(TEST_PAGE_SIZE);
        let p1 = db.connect(TEST_PAGE_SIZE);

        let mut hot_page_id = 0usize;
        with_write_txn(&p1, |pager| {
            hot_page_id = allocate_marked_page(pager, 0);
        });

        // Reader A pins the snapshot right after the initial write (marker 0).
        let reader_a = db.connect(TEST_PAGE_SIZE);
        assert!(matches!(
            reader_a.begin_read_tx().expect("begin_read_tx"),
            LimboResult::Ok
        ));

        with_write_txn(&p1, |pager| {
            rewrite_marked_page(pager, hot_page_id, 1);
        });
        checkpoint(&p1, CheckpointMode::Passive);

        // Reader B pins the snapshot after marker 1 was written+checkpointed.
        let reader_b = db.connect(TEST_PAGE_SIZE);
        assert!(matches!(
            reader_b.begin_read_tx().expect("begin_read_tx"),
            LimboResult::Ok
        ));

        with_write_txn(&p1, |pager| {
            rewrite_marked_page(pager, hot_page_id, 2);
        });
        checkpoint(&p1, CheckpointMode::Passive);

        // Reader C pins the snapshot after marker 2.
        let reader_c = db.connect(TEST_PAGE_SIZE);
        assert!(matches!(
            reader_c.begin_read_tx().expect("begin_read_tx"),
            LimboResult::Ok
        ));

        // Churn through many more writes + Passive checkpoints while all
        // three readers stay pinned to their own snapshots.
        const CYCLES: usize = 100;
        for i in 0..CYCLES {
            with_write_txn(&p1, |pager| {
                rewrite_marked_page(pager, hot_page_id, (3 + i % 250) as u8);
            });
            checkpoint(&p1, CheckpointMode::Passive);
        }

        // Each reader must still see EXACTLY the marker that was current
        // when it began its read transaction, regardless of everything that
        // happened afterwards (and the trimming those checkpoints performed).
        assert_eq!(read_marker(&reader_a, hot_page_id), 0, "reader_a snapshot");
        assert_eq!(read_marker(&reader_b, hot_page_id), 1, "reader_b snapshot");
        assert_eq!(read_marker(&reader_c, hot_page_id), 2, "reader_c snapshot");

        // Release all readers and checkpoint once more: memory must be
        // reclaimed now that nothing pins an old snapshot.
        reader_a.end_read_tx().expect("end_read_tx");
        reader_b.end_read_tx().expect("end_read_tx");
        reader_c.end_read_tx().expect("end_read_tx");
        checkpoint(&p1, CheckpointMode::Passive);

        let (frame_cache_len, pages_len) = {
            let shared = db.shared();
            (
                shared.frame_cache.lock().len(),
                shared.pages_in_frames.lock().len(),
            )
        };
        assert_eq!(
            frame_cache_len, 0,
            "frame_cache must be reclaimed once all readers are gone"
        );
        assert_eq!(
            pages_len, 0,
            "pages_in_frames must be reclaimed once all readers are gone"
        );
    }
}
