//! Auto-generated module
//!
//! 🤖 Generated with [SplitRS](https://github.com/cool-japan/splitrs)

use crate::storage::sqlite3_ondisk::{PageContent, PageType};
use crate::storage::wal::CheckpointResult;
use std::cell::{RefCell, UnsafeCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::constants::{PAGE_DIRTY, PAGE_ERROR, PAGE_LOADED, PAGE_LOCKED, PAGE_UPTODATE};

#[derive(Clone, Copy, Debug)]
/// The state of the current pager cache flush.
pub(super) enum FlushState {
    /// Idle.
    Start,
    /// Waiting for all in-flight writes to the on-disk WAL to complete.
    WaitAppendFrames,
    /// Fsync the on-disk WAL.
    SyncWal,
    /// Checkpoint the WAL to the database file (if needed).
    Checkpoint,
    /// Fsync the database file.
    SyncDbFile,
    /// Waiting for the database file to be fsynced.
    WaitSyncDbFile,
}
/// A snapshot of WAL state captured at SAVEPOINT open time.
///
/// Used by ROLLBACK TO SAVEPOINT to restore the WAL (and thus the database)
/// to the exact state it was in when the savepoint was created.
#[derive(Debug, Clone)]
pub struct SavepointFrame {
    /// The savepoint name (compared case-insensitively per SQLite spec).
    pub name: String,
    /// `shared.max_frame` at the time the savepoint was opened.
    pub wal_max_frame: u64,
    /// `shared.last_checksum` at the time the savepoint was opened.
    pub wal_checksum: (u32, u32),
    /// True when this savepoint implicitly started the write transaction
    /// (i.e., it was opened in autocommit mode).  RELEASE of an owner
    /// savepoint commits the transaction.
    pub is_transaction_owner: bool,
}
#[derive(Debug, Clone)]
/// The status of the current cache flush.
/// A Done state means that the WAL was committed to disk and fsynced,
/// plus potentially checkpointed to the DB (and the DB then fsynced).
pub enum PagerCacheflushStatus {
    Done(PagerCacheflushResult),
    IO,
}
#[derive(Debug)]
pub struct CreateBTreeFlags(pub u8);
impl CreateBTreeFlags {
    pub const TABLE: u8 = 0b0001;
    pub const INDEX: u8 = 0b0010;
    pub fn new_table() -> Self {
        Self(CreateBTreeFlags::TABLE)
    }
    pub fn new_index() -> Self {
        Self(CreateBTreeFlags::INDEX)
    }
    pub fn is_table(&self) -> bool {
        (self.0 & CreateBTreeFlags::TABLE) != 0
    }
    pub fn is_index(&self) -> bool {
        (self.0 & CreateBTreeFlags::INDEX) != 0
    }
    pub fn get_flags(&self) -> u8 {
        self.0
    }
}
#[derive(Debug, Clone)]
pub enum PagerCacheflushResult {
    /// The WAL was written to disk and fsynced.
    WalWritten,
    /// The WAL was written, fsynced, and a checkpoint was performed.
    /// The database file was then also fsynced.
    Checkpointed(CheckpointResult),
}
/// Durability level set by `PRAGMA synchronous`.
///
/// Mirrors SQLite's synchronous settings. Controls when fsync is issued to the
/// WAL and the database file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SynchronousMode {
    /// `0` — never fsync. Fastest, least durable (data may be lost on OS crash).
    Off,
    /// `1` — fsync the WAL at checkpoint time only. The default.
    Normal,
    /// `2` — fsync the WAL on every commit AND fsync the DB file after checkpoint.
    Full,
    /// `3` — like FULL with extra fsync of the directory containing the rollback
    /// journal; for WAL mode this behaves like FULL for our purposes.
    Extra,
}
impl SynchronousMode {
    /// Numeric form used by `PRAGMA synchronous` (and SQLite's on-the-wire value).
    pub fn as_i64(self) -> i64 {
        match self {
            SynchronousMode::Off => 0,
            SynchronousMode::Normal => 1,
            SynchronousMode::Full => 2,
            SynchronousMode::Extra => 3,
        }
    }
}
/// This will keep track of the state of current cache flush in order to not repeat work
pub(super) struct FlushInfo {
    pub(super) state: FlushState,
    /// Number of writes taking place. When in_flight gets to 0 we can schedule a fsync.
    pub(super) in_flight_writes: Rc<RefCell<usize>>,
}
pub struct PageInner {
    pub flags: AtomicUsize,
    pub contents: Option<PageContent>,
    pub id: usize,
}
#[derive(Clone, Debug, Copy)]
pub(super) enum CheckpointState {
    Checkpoint,
    SyncDbFile,
    WaitSyncDbFile,
    CheckpointDone,
}
#[derive(Debug)]
pub struct Page {
    pub inner: UnsafeCell<PageInner>,
}
impl Page {
    pub fn new(id: usize) -> Self {
        Self {
            inner: UnsafeCell::new(PageInner {
                flags: AtomicUsize::new(0),
                contents: None,
                id,
            }),
        }
    }
    /// Shared, read-only view of the page's cache metadata + contents.
    ///
    /// Sound to call any number of times, nested or not, and to hold the
    /// result across other `get_ref`/flag-check/`get_contents` calls: shared
    /// references never invalidate each other. See [`Self::get`]'s doc
    /// comment for the (much more restrictive) rules that govern the
    /// exclusive accessor, which this deliberately avoids for every
    /// read-only use (which is most of them: flag checks, `id` reads, and
    /// reading through to the page's `PageContent`, whose own API is fully
    /// `&self`-based).
    pub fn get_ref(&self) -> &PageInner {
        // SAFETY: constructing a shared reference from `UnsafeCell::get` is
        // exactly the sanctioned pattern; the pointee is valid for as long
        // as `self` (and hence the `UnsafeCell`) is.
        unsafe { &*self.inner.get() }
    }

    /// Exclusive view of the page's cache metadata + contents, fabricated
    /// from `&self`.
    ///
    /// # Why this is sound (and what's still required of callers)
    ///
    /// Same shape, and same caveat, as `PageContent::as_ptr` (private,
    /// crate-internal type):
    /// this is not enforced sound by the type system, only by the dynamic
    /// access pattern actually upholding Rust's aliasing rules. Every call
    /// site that needs to *mutate* `PageInner` itself (as opposed to reading
    /// it, or reading/writing *through* the `PageContent` it holds, which
    /// has its own `&self`-based API and doesn't need this at all) MUST
    /// gather anything else it needs first, call `get` last, perform its
    /// mutation in one uninterrupted stretch with no other `self`/`page`
    /// call (through *any* accessor, shared or exclusive) in between, then
    /// drop the reference. A fresh call to `get` (or even `get_ref`) while an
    /// older `get` result is still going to be used again invalidates that
    /// older result.
    #[allow(clippy::mut_from_ref)]
    pub fn get(&self) -> &mut PageInner {
        // SAFETY: see above; the caller upholds the access discipline.
        unsafe { &mut *self.inner.get() }
    }
    /// Shared, read-only access to this page's `PageContent`. Prefer this
    /// over [`Self::get_contents_mut`] for every call site that only reads
    /// through it (i.e. almost all of them, since `PageContent`'s own API --
    /// `read_*`/`write_*`/`cell_*`/... -- is fully `&self`-based); only use
    /// the exclusive version when actually mutating `PageContent`'s own
    /// struct fields directly (e.g. `overflow_cells`).
    pub fn get_contents(&self) -> &PageContent {
        self.get_ref().contents.as_ref().unwrap()
    }
    /// Exclusive access to this page's `PageContent`, for call sites that
    /// mutate its struct fields directly (e.g. `overflow_cells.push(..)`).
    /// Subject to the same "gather everything else first, use last, no
    /// other page access in between" discipline as [`Self::get`], which this
    /// is built on.
    #[allow(clippy::mut_from_ref)]
    pub fn get_contents_mut(&self) -> &mut PageContent {
        self.get().contents.as_mut().unwrap()
    }
    pub fn is_uptodate(&self) -> bool {
        self.get_ref().flags.load(Ordering::SeqCst) & PAGE_UPTODATE != 0
    }
    pub fn set_uptodate(&self) {
        self.get_ref()
            .flags
            .fetch_or(PAGE_UPTODATE, Ordering::SeqCst);
    }
    pub fn clear_uptodate(&self) {
        self.get_ref()
            .flags
            .fetch_and(!PAGE_UPTODATE, Ordering::SeqCst);
    }
    pub fn is_locked(&self) -> bool {
        self.get_ref().flags.load(Ordering::SeqCst) & PAGE_LOCKED != 0
    }
    pub fn set_locked(&self) {
        self.get_ref().flags.fetch_or(PAGE_LOCKED, Ordering::SeqCst);
    }
    pub fn clear_locked(&self) {
        self.get_ref()
            .flags
            .fetch_and(!PAGE_LOCKED, Ordering::SeqCst);
    }
    pub fn is_error(&self) -> bool {
        self.get_ref().flags.load(Ordering::SeqCst) & PAGE_ERROR != 0
    }
    pub fn set_error(&self) {
        self.get_ref().flags.fetch_or(PAGE_ERROR, Ordering::SeqCst);
    }
    pub fn clear_error(&self) {
        self.get_ref()
            .flags
            .fetch_and(!PAGE_ERROR, Ordering::SeqCst);
    }
    pub fn is_dirty(&self) -> bool {
        self.get_ref().flags.load(Ordering::SeqCst) & PAGE_DIRTY != 0
    }
    pub fn set_dirty(&self) {
        tracing::debug!("set_dirty(page={})", self.get_ref().id);
        self.get_ref().flags.fetch_or(PAGE_DIRTY, Ordering::SeqCst);
    }
    pub fn clear_dirty(&self) {
        tracing::debug!("clear_dirty(page={})", self.get_ref().id);
        self.get_ref()
            .flags
            .fetch_and(!PAGE_DIRTY, Ordering::SeqCst);
    }
    pub fn is_loaded(&self) -> bool {
        self.get_ref().flags.load(Ordering::SeqCst) & PAGE_LOADED != 0
    }
    pub fn set_loaded(&self) {
        self.get_ref().flags.fetch_or(PAGE_LOADED, Ordering::SeqCst);
    }
    pub fn clear_loaded(&self) {
        tracing::debug!("clear loaded {}", self.get_ref().id);
        self.get_ref()
            .flags
            .fetch_and(!PAGE_LOADED, Ordering::SeqCst);
    }
    pub fn is_index(&self) -> bool {
        match self.get_contents().page_type() {
            PageType::IndexLeaf | PageType::IndexInterior => true,
            PageType::TableLeaf | PageType::TableInterior => false,
        }
    }
}
/// The mode of allocating a btree page.
pub enum BtreePageAllocMode {
    /// Allocate any btree page
    Any,
    /// Allocate a specific page number, typically used for root page allocation
    Exact(u32),
    /// Allocate a page number less than or equal to the parameter
    Le(u32),
}
/// Track the state of the auto-vacuum mode.
#[derive(Clone, Copy, Debug)]
pub enum AutoVacuumMode {
    None,
    Full,
    Incremental,
}
