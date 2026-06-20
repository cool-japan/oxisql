// Copyright 2026 COOLJAPAN OU (Team KitaSan)
// SPDX-License-Identifier: Apache-2.0

use super::{
    pager::PageRef,
    sqlite3_ondisk::{
        write_varint_to_vec, IndexInteriorCell, IndexLeafCell, OverflowCell, DATABASE_HEADER_SIZE,
    },
};
use crate::{
    return_corrupt,
    types::{compare_immutable, CursorResult, ImmutableRecord, RefValue, SeekKey, SeekOp, Value},
    LimboError, Result,
};
use crate::{
    schema::Index,
    storage::{
        pager::{BtreePageAllocMode, Pager},
        sqlite3_ondisk::{
            read_u32, read_varint, BTreeCell, PageContent, PageType, TableInteriorCell,
            TableLeafCell,
        },
    },
    translate::{collate::CollationSeq, plan::IterationDirection},
    types::{IndexKeyInfo, IndexKeySortOrder, ParseRecordState},
    MvCursor,
};
#[cfg(debug_assertions)]
use std::collections::HashSet;
use std::{
    cell::{Cell, Ref, RefCell},
    cmp::{Ordering, Reverse},
    collections::BinaryHeap,
    fmt::Debug,
    pin::Pin,
    rc::Rc,
    sync::Arc,
};
use tracing::{instrument, Level};
/// The B-Tree page header is 12 bytes for interior pages and 8 bytes for leaf pages.
///
/// +--------+-----------------+-----------------+-----------------+--------+----- ..... ----+
/// | Page   | First Freeblock | Cell Count      | Cell Content    | Frag.  | Right-most     |
/// | Type   | Offset          |                 | Area Start      | Bytes  | pointer        |
/// +--------+-----------------+-----------------+-----------------+--------+----- ..... ----+
///     0        1        2        3        4        5        6        7        8       11
///
pub mod offset {
    /// Type of the B-Tree page (u8).
    pub const BTREE_PAGE_TYPE: usize = 0;
    /// A pointer to the first freeblock (u16).
    ///
    /// This field of the B-Tree page header is an offset to the first freeblock, or zero if
    /// there are no freeblocks on the page.  A freeblock is a structure used to identify
    /// unallocated space within a B-Tree page, organized as a chain.
    ///
    /// Please note that freeblocks do not mean the regular unallocated free space to the left
    /// of the cell content area pointer, but instead blocks of at least 4
    /// bytes WITHIN the cell content area that are not in use due to e.g.
    /// deletions.
    pub const BTREE_FIRST_FREEBLOCK: usize = 1;
    /// The number of cells in the page (u16).
    pub const BTREE_CELL_COUNT: usize = 3;
    /// A pointer to first byte of cell allocated content from top (u16).
    ///
    /// SQLite strives to place cells as far toward the end of the b-tree page as it can, in
    /// order to leave space for future growth of the cell pointer array. This means that the
    /// cell content area pointer moves leftward as cells are added to the page.
    pub const BTREE_CELL_CONTENT_AREA: usize = 5;
    /// The number of fragmented bytes (u8).
    ///
    /// Fragments are isolated groups of 1, 2, or 3 unused bytes within the cell content area.
    pub const BTREE_FRAGMENTED_BYTES_COUNT: usize = 7;
    /// The right-most pointer (saved separately from cells) (u32)
    pub const BTREE_RIGHTMOST_PTR: usize = 8;
}
/// Maximum depth of an SQLite B-Tree structure. Any B-Tree deeper than
/// this will be declared corrupt. This value is calculated based on a
/// maximum database size of 2^31 pages a minimum fanout of 2 for a
/// root-node and 3 for all other internal nodes.
///
/// If a tree that appears to be taller than this is encountered, it is
/// assumed that the database is corrupt.
pub const BTCURSOR_MAX_DEPTH: usize = 20;
/// Evaluate a `Result<CursorResult<T>>`, if IO return IO.
macro_rules! return_if_io {
    ($expr:expr) => {
        match $expr? {
            CursorResult::Ok(v) => v,
            CursorResult::IO => return Ok(CursorResult::IO),
        }
    };
}
/// Check if the page is unlocked, if not return IO.
macro_rules! return_if_locked {
    ($expr:expr) => {{
        if $expr.is_locked() {
            return Ok(CursorResult::IO);
        }
    }};
}
/// Validate cells in a page are in a valid state. Only in debug mode.
macro_rules! debug_validate_cells {
    ($page_contents:expr, $usable_space:expr) => {
        #[cfg(debug_assertions)]
        {
            debug_validate_cells_core($page_contents, $usable_space);
        }
    };
}
/// Check if the page is unlocked, if not return IO. If the page is not locked but not loaded, then try to load it.
macro_rules! return_if_locked_maybe_load {
    ($pager:expr, $btree_page:expr) => {{
        if $btree_page.get().is_locked() {
            return Ok(CursorResult::IO);
        }
        if !$btree_page.get().is_loaded() {
            let page = $pager.read_page($btree_page.get().get().id)?;
            $btree_page.page.replace(page);
            return Ok(CursorResult::IO);
        }
    }};
}
/// Wrapper around a page reference used in order to update the reference in case page was unloaded
/// and we need to update the reference.
pub struct BTreePageInner {
    pub page: RefCell<PageRef>,
}
pub type BTreePage = Arc<BTreePageInner>;
unsafe impl Send for BTreePageInner {}
unsafe impl Sync for BTreePageInner {}
/// State machine of destroy operations
/// Keep track of traversal so that it can be resumed when IO is encountered
#[derive(Debug, Clone)]
enum DestroyState {
    Start,
    LoadPage,
    ProcessPage,
    ClearOverflowPages { cell: BTreeCell },
    FreePage,
}
struct DestroyInfo {
    state: DestroyState,
}
#[derive(Debug, Clone)]
enum DeleteSavepoint {
    Rowid(i64),
    Payload(ImmutableRecord),
}
#[derive(Debug, Clone)]
enum DeleteState {
    Start,
    DeterminePostBalancingSeekKey,
    LoadPage {
        post_balancing_seek_key: Option<DeleteSavepoint>,
    },
    FindCell {
        post_balancing_seek_key: Option<DeleteSavepoint>,
    },
    ClearOverflowPages {
        cell_idx: usize,
        cell: BTreeCell,
        original_child_pointer: Option<u32>,
        post_balancing_seek_key: Option<DeleteSavepoint>,
    },
    InteriorNodeReplacement {
        cell_idx: usize,
        original_child_pointer: Option<u32>,
        post_balancing_seek_key: Option<DeleteSavepoint>,
    },
    CheckNeedsBalancing {
        rightmost_cell_was_dropped: bool,
        post_balancing_seek_key: Option<DeleteSavepoint>,
    },
    WaitForBalancingToComplete {
        target_key: DeleteSavepoint,
    },
    SeekAfterBalancing {
        target_key: DeleteSavepoint,
    },
}
#[derive(Clone)]
struct DeleteInfo {
    state: DeleteState,
    balance_write_info: Option<WriteInfo>,
}
/// State machine of a write operation.
/// May involve balancing due to overflow.
#[derive(Debug, Clone, Copy)]
enum WriteState {
    Start,
    BalanceStart,
    BalanceNonRoot,
    BalanceNonRootWaitLoadPages,
    Finish,
}
struct ReadPayloadOverflow {
    payload: Vec<u8>,
    next_page: u32,
    remaining_to_read: usize,
    page: BTreePage,
}
enum PayloadOverflowWithOffset {
    SkipOverflowPages {
        next_page: u32,
        pages_left_to_skip: u32,
        page_offset: u32,
        amount: u32,
        buffer_offset: usize,
        is_write: bool,
    },
    ProcessPage {
        next_page: u32,
        remaining_to_read: u32,
        page: BTreePage,
        current_offset: usize,
        buffer_offset: usize,
        is_write: bool,
    },
}
#[derive(Clone, Debug)]
pub enum BTreeKey<'a> {
    TableRowId((i64, Option<&'a ImmutableRecord>)),
    IndexKey(&'a ImmutableRecord),
}
impl BTreeKey<'_> {
    /// Create a new table rowid key from a rowid and an optional immutable record.
    /// The record is optional because it may not be available when the key is created.
    pub fn new_table_rowid(rowid: i64, record: Option<&ImmutableRecord>) -> BTreeKey<'_> {
        BTreeKey::TableRowId((rowid, record))
    }
    /// Create a new index key from an immutable record.
    pub fn new_index_key(record: &ImmutableRecord) -> BTreeKey<'_> {
        BTreeKey::IndexKey(record)
    }
    /// Get the record, if present. Index will always be present,
    fn get_record(&self) -> Option<&'_ ImmutableRecord> {
        match self {
            BTreeKey::TableRowId((_, record)) => *record,
            BTreeKey::IndexKey(record) => Some(record),
        }
    }
    /// Get the rowid, if present. Index will never be present.
    fn maybe_rowid(&self) -> Option<i64> {
        match self {
            BTreeKey::TableRowId((rowid, _)) => Some(*rowid),
            BTreeKey::IndexKey(_) => None,
        }
    }
    /// Assert that the key is an integer rowid and return it.
    fn to_rowid(&self) -> i64 {
        match self {
            BTreeKey::TableRowId((rowid, _)) => *rowid,
            BTreeKey::IndexKey(_) => panic!("BTreeKey::to_rowid called on IndexKey"),
        }
    }
    /// Assert that the key is an index key and return it.
    fn to_index_key_values(&self) -> &'_ Vec<RefValue> {
        match self {
            BTreeKey::TableRowId(_) => {
                panic!("BTreeKey::to_index_key called on TableRowId")
            }
            BTreeKey::IndexKey(key) => key.get_values(),
        }
    }
}
#[derive(Clone)]
struct BalanceInfo {
    /// Old pages being balanced. We can have maximum 3 pages being balanced at the same time.
    pages_to_balance: [Option<BTreePage>; 3],
    /// Bookkeeping of the rightmost pointer so the offset::BTREE_RIGHTMOST_PTR can be updated.
    rightmost_pointer: *mut u8,
    /// Divider cells of old pages. We can have maximum 2 divider cells because of 3 pages.
    divider_cells: [Option<Vec<u8>>; 2],
    /// Number of siblings being used to balance
    sibling_count: usize,
    /// First divider cell to remove that marks the first sibling
    first_divider_cell: usize,
}
#[derive(Clone)]
struct WriteInfo {
    /// State of the write operation state machine.
    state: WriteState,
    balance_info: RefCell<Option<BalanceInfo>>,
}
impl WriteInfo {
    fn new() -> WriteInfo {
        WriteInfo {
            state: WriteState::Start,
            balance_info: RefCell::new(None),
        }
    }
}
/// Holds the state machine for the operation that was in flight when the cursor
/// was suspended due to IO.
enum CursorState {
    None,
    ReadWritePayload(PayloadOverflowWithOffset),
    Write(WriteInfo),
    Destroy(DestroyInfo),
    Delete(DeleteInfo),
}
impl CursorState {
    fn write_info(&self) -> Option<&WriteInfo> {
        match self {
            CursorState::Write(x) => Some(x),
            _ => None,
        }
    }
    fn mut_write_info(&mut self) -> Option<&mut WriteInfo> {
        match self {
            CursorState::Write(x) => Some(x),
            _ => None,
        }
    }
    fn destroy_info(&self) -> Option<&DestroyInfo> {
        match self {
            CursorState::Destroy(x) => Some(x),
            _ => None,
        }
    }
    fn mut_destroy_info(&mut self) -> Option<&mut DestroyInfo> {
        match self {
            CursorState::Destroy(x) => Some(x),
            _ => None,
        }
    }
    fn delete_info(&self) -> Option<&DeleteInfo> {
        match self {
            CursorState::Delete(x) => Some(x),
            _ => None,
        }
    }
    fn mut_delete_info(&mut self) -> Option<&mut DeleteInfo> {
        match self {
            CursorState::Delete(x) => Some(x),
            _ => None,
        }
    }
}
impl Debug for CursorState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Delete(..) => write!(f, "Delete"),
            Self::Destroy(..) => write!(f, "Destroy"),
            Self::None => write!(f, "None"),
            Self::ReadWritePayload(..) => write!(f, "ReadWritePayload"),
            Self::Write(..) => write!(f, "Write"),
        }
    }
}
enum OverflowState {
    Start,
    ProcessPage { next_page: u32 },
    Done,
}
/// Holds a Record or RowId, so that these can be transformed into a SeekKey to restore
/// cursor position to its previous location.
pub enum CursorContext {
    TableRowId(i64),
    /// If we are in an index tree we can then reuse this field to save
    /// our cursor information
    IndexKeyRowId(ImmutableRecord),
}
/// In the future, we may expand these general validity states
#[derive(Debug, PartialEq, Eq)]
pub enum CursorValidState {
    /// Cursor is pointing a to an existing location/cell in the Btree
    Valid,
    /// Cursor may be pointing to a non-existent location/cell. This can happen after balancing operations
    RequireSeek,
}
#[derive(Debug)]
/// State used for seeking
pub enum CursorSeekState {
    Start,
    MovingBetweenPages {
        eq_seen: Cell<bool>,
    },
    InteriorPageBinarySearch {
        min_cell_idx: Cell<isize>,
        max_cell_idx: Cell<isize>,
        nearest_matching_cell: Cell<Option<usize>>,
        eq_seen: Cell<bool>,
    },
    FoundLeaf {
        eq_seen: Cell<bool>,
    },
    LeafPageBinarySearch {
        min_cell_idx: Cell<isize>,
        max_cell_idx: Cell<isize>,
        nearest_matching_cell: Cell<Option<usize>>,
        /// Indicates if we have seen an exact match during the downwards traversal of the btree.
        /// This is only needed in index seeks, in cases where we need to determine whether we call
        /// an additional next()/prev() to fetch a matching record from an interior node. We will not
        /// do that if both are true:
        /// 1. We have not seen an EQ during the traversal
        /// 2. We are looking for an exact match ([SeekOp::GE] or [SeekOp::LE] with eq_only: true)
        eq_seen: Cell<bool>,
        /// Indicates when we have not not found a value in leaf and now will look in the next/prev record.
        /// This value is only used for indexbtree
        moving_up_to_parent: Cell<bool>,
    },
}
#[derive(Debug)]
struct FindCellState(Option<isize>);
impl FindCellState {
    #[inline]
    fn set(&mut self, cell_idx: isize) {
        self.0 = Some(cell_idx);
    }
    #[inline]
    fn get_cell_idx(&mut self) -> isize {
        self.0.expect("get can only be called after a set")
    }
    #[inline]
    fn reset(&mut self) {
        self.0 = None;
    }
}
pub struct BTreeCursor {
    /// The multi-version cursor that is used to read and write to the database file.
    mv_cursor: Option<Rc<RefCell<MvCursor>>>,
    /// The pager that is used to read and write to the database file.
    pager: Rc<Pager>,
    /// Page id of the root page used to go back up fast.
    root_page: usize,
    /// Rowid and record are stored before being consumed.
    has_record: Cell<bool>,
    null_flag: bool,
    /// Index internal pages are consumed on the way up, so we store going upwards flag in case
    /// we just moved to a parent page and the parent page is an internal index page which requires
    /// to be consumed.
    going_upwards: bool,
    /// Information maintained across execution attempts when an operation yields due to I/O.
    state: CursorState,
    /// Information maintained while freeing overflow pages. Maintained separately from cursor state since
    /// any method could require freeing overflow pages
    overflow_state: Option<OverflowState>,
    /// Page stack used to traverse the btree.
    /// Each cursor has a stack because each cursor traverses the btree independently.
    stack: PageStack,
    /// Reusable immutable record, used to allow better allocation strategy.
    reusable_immutable_record: RefCell<Option<ImmutableRecord>>,
    /// Reusable immutable record, used to allow better allocation strategy.
    parse_record_state: RefCell<ParseRecordState>,
    pub index_key_info: Option<IndexKeyInfo>,
    /// Maintain count of the number of records in the btree. Used for the `Count` opcode
    count: usize,
    /// Stores the cursor context before rebalancing so that a seek can be done later
    context: Option<CursorContext>,
    /// Store whether the Cursor is in a valid state. Meaning if it is pointing to a valid cell index or not
    pub valid_state: CursorValidState,
    /// Colations for Index Btree constraint checks
    /// Contains the Collation Seq for the whole Index
    /// This Vec should be empty for Table Btree
    collations: Vec<CollationSeq>,
    seek_state: CursorSeekState,
    /// Separate state to read a record with overflow pages. This separation from `state` is necessary as
    /// we can be in a function that relies on `state`, but also needs to process overflow pages
    read_overflow_state: RefCell<Option<ReadPayloadOverflow>>,
    /// Contains the current cell_idx for `find_cell`
    find_cell_state: FindCellState,
    /// Resumable state for the ANALYZE walk (sqlite_stat1 computation).
    /// `None` unless an `index_stat` walk is in progress and has yielded for I/O.
    analyze_walk: Option<AnalyzeWalk>,
}
/// Phase of the resumable ANALYZE cursor walk used by [`BTreeCursor::index_stat`].
#[derive(Debug, Clone, Copy, PartialEq)]
enum AnalyzePhase {
    Init,
    Rewind,
    Read,
    Advance,
    Done,
}
/// Resumable accumulator state for computing an `sqlite_stat1` entry while
/// walking a table or index b-tree. Persisted on the cursor so the walk can
/// survive I/O yields.
#[derive(Debug)]
struct AnalyzeWalk {
    phase: AnalyzePhase,
    /// Total number of entries seen so far.
    n: i64,
    /// `distinct[i]` counts how many times the prefix of length `i + 1` changed.
    /// The number of distinct prefixes of length `i + 1` is therefore `distinct[i] + 1`.
    distinct: Vec<i64>,
    /// Owned copy of the previous row's leading key columns (for change detection).
    prev_key: Vec<Value>,
    /// Number of leading key columns to consider (0 for a plain table walk).
    num_cols: usize,
}
/// Return the index of the first key column (in `0..num_cols`) at which `prev`
/// and `cur` differ, or `num_cols` if the keys are equal over the whole prefix.
///
/// Column equality uses [`Value`]'s own `PartialEq`, which compares numeric
/// values across the integer/float boundary, treats `NULL` as equal to `NULL`
/// (so consecutive NULL keys collapse into one group, matching SQLite's
/// `sqlite_stat1` behaviour), and compares text/blob by their raw bytes.
fn first_change(prev: &[Value], cur: &[Value], num_cols: usize) -> usize {
    for (i, (p, c)) in prev.iter().zip(cur.iter()).take(num_cols).enumerate() {
        if p != c {
            return i;
        }
    }
    num_cols
}

// ---------------------------------------------------------------------------
// BTreeCursor implementation — split across three include files to keep each
// source file under the 2 000-line policy.
//
//   cursor_core.rs   — construction, read, seek, navigation basics (~1344 lines)
//   cursor_write.rs  — insert, balance_non_root, balance_root, delete (~1588 lines)
//   cursor_nav.rs    — public navigation + count/stat/context/analyze (~1263 lines)
//
// All three files contain separate `impl BTreeCursor { … }` blocks, which is
// fully valid Rust — a type may have any number of impl blocks across the same
// module.
// ---------------------------------------------------------------------------
include!("cursor_core.rs");
include!("cursor_write.rs");
include!("cursor_nav.rs");

// ---------------------------------------------------------------------------
// Standalone page-operation types and functions (IntegrityCheck*, PageStack,
// CellArray, BTreePageInner::get, edit_page, …) — split into page_ops.rs.
// ---------------------------------------------------------------------------
include!("page_ops.rs");

#[cfg(test)]
mod tests;
