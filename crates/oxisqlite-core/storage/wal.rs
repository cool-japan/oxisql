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

#[derive(Debug, Copy, Clone)]
pub struct CheckpointResult {
    /// number of frames in WAL
    pub num_wal_frames: u64,
    /// number of frames moved successfully from WAL to db file after checkpoint
    pub num_checkpointed_frames: u64,
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
        todo!();
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

#[derive(Debug, Copy, Clone)]
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
}

impl fmt::Debug for OngoingCheckpoint {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("OngoingCheckpoint")
            .field("state", &self.state)
            .field("min_frame", &self.min_frame)
            .field("max_frame", &self.max_frame)
            .field("current_page", &self.current_page)
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
                    let shared = self.get_shared();

                    // Record two num pages fields to return as checkpoint result to caller.
                    // Ref: pnLog, pnCkpt on https://www.sqlite.org/c3ref/wal_checkpoint_v2.html
                    let checkpoint_result = CheckpointResult {
                        num_wal_frames: shared.max_frame.load(Ordering::SeqCst),
                        num_checkpointed_frames: self.ongoing_checkpoint.max_frame,
                    };
                    let everything_backfilled = shared.max_frame.load(Ordering::SeqCst)
                        == self.ongoing_checkpoint.max_frame;
                    if everything_backfilled {
                        if !matches!(mode, CheckpointMode::Passive) {
                            // Everything was backfilled into the db file, so it is
                            // safe to reset the WAL bookkeeping.
                            shared.frame_cache.lock().clear();
                            shared.pages_in_frames.lock().clear();
                            shared.max_frame.store(0, Ordering::SeqCst);
                            shared.nbackfills.store(0, Ordering::SeqCst);
                            // For Restart/Truncate, physically reset the on-disk WAL:
                            // rewrite a fresh header (new salt) so leftover frames are
                            // rejected by future recovery, and for Truncate shrink the
                            // file to zero bytes first.
                            if matches!(mode, CheckpointMode::Restart | CheckpointMode::Truncate) {
                                self.reset_wal_file(matches!(mode, CheckpointMode::Truncate))?;
                            }
                        }
                    } else {
                        shared
                            .nbackfills
                            .store(self.ongoing_checkpoint.max_frame, Ordering::SeqCst);
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

    /// Reset the on-disk WAL after a Restart/Truncate checkpoint.
    ///
    /// Rewrites the 32-byte WAL header with a freshly generated salt (so any
    /// leftover frames still on disk are rejected by a future recovery via the
    /// salt-mismatch check) and resets the in-memory cumulative checksum. When
    /// `truncate` is true, the WAL file is also physically shrunk to zero bytes
    /// and a fresh empty 32-byte header is rewritten, so the on-disk `-wal`
    /// carries no frames (max_frame 0) and a byte-level reader sees a
    /// self-contained database file.
    fn reset_wal_file(&self, truncate: bool) -> Result<()> {
        let shared = self.get_shared();
        // Generate a new salt so any leftover frames become unrecoverable, and
        // recompute the header checksum over the first 24 bytes.
        {
            let mut header = shared.wal_header.lock();
            header.salt_1 = self.io.generate_random_number() as u32;
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
