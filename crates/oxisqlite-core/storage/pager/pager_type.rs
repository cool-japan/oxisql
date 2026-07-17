//! Auto-generated module
//!
//! 🤖 Generated with [SplitRS](https://github.com/cool-japan/splitrs)

use crate::fast_lock::SpinLock;
use crate::storage::buffer_pool::BufferPool;
use crate::storage::database::DatabaseStorage;
use crate::storage::page_cache::DumbLruPageCache;
use crate::storage::sqlite3_ondisk::DatabaseHeader;
use crate::storage::wal::Wal;
use parking_lot::RwLock;
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use super::types::{AutoVacuumMode, CheckpointState, FlushInfo, SavepointFrame, SynchronousMode};

/// The pager interface implements the persistence layer by providing access
/// to pages of the database file, including caching, concurrency control, and
/// transaction management.
pub struct Pager {
    /// Source of the database pages.
    pub db_file: Arc<dyn DatabaseStorage>,
    /// The write-ahead log (WAL) for the database.
    pub(super) wal: Rc<RefCell<dyn Wal>>,
    /// A page cache for the database.
    pub(super) page_cache: Arc<RwLock<DumbLruPageCache>>,
    /// Buffer pool for temporary data storage.
    pub(super) buffer_pool: Rc<BufferPool>,
    /// I/O interface for input/output operations.
    pub io: Arc<dyn crate::io::IO>,
    pub(super) dirty_pages: Rc<RefCell<HashSet<usize>>>,
    pub db_header: Arc<SpinLock<DatabaseHeader>>,
    pub(super) flush_info: RefCell<FlushInfo>,
    pub(super) checkpoint_state: RefCell<CheckpointState>,
    pub(super) checkpoint_inflight: Rc<RefCell<usize>>,
    pub(super) syncing: Rc<RefCell<bool>>,
    pub(super) auto_vacuum_mode: RefCell<AutoVacuumMode>,
    /// Durability level controlled by `PRAGMA synchronous`. Defaults to NORMAL.
    pub(super) synchronous_mode: Cell<SynchronousMode>,
    /// Stack of active savepoints for SAVEPOINT / ROLLBACK TO / RELEASE.
    pub savepoints: RefCell<Vec<SavepointFrame>>,
}
