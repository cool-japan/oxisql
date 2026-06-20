#[derive(Debug, thiserror::Error)]
pub enum IntegrityCheckError {
    #[error(
        "Cell {cell_idx} in page {page_id} is out of range. cell_range={cell_start}..{cell_end}, content_area={content_area}, usable_space={usable_space}"
    )]
    CellOutOfRange {
        cell_idx: usize,
        page_id: usize,
        cell_start: usize,
        cell_end: usize,
        content_area: usize,
        usable_space: usize,
    },
    #[error(
        "Cell {cell_idx} in page {page_id} extends out of page. cell_range={cell_start}..{cell_end}, content_area={content_area}, usable_space={usable_space}"
    )]
    CellOverflowsPage {
        cell_idx: usize,
        page_id: usize,
        cell_start: usize,
        cell_end: usize,
        content_area: usize,
        usable_space: usize,
    },
    #[error(
        "Page {page_id} cell {cell_idx} has rowid={rowid} in wrong order. Parent cell has parent_rowid={max_intkey} and next_rowid={next_rowid}"
    )]
    CellRowidOutOfRange {
        page_id: usize,
        cell_idx: usize,
        rowid: i64,
        max_intkey: i64,
        next_rowid: i64,
    },
    #[error(
        "Page {page_id} is at different depth from another leaf page this_page_depth={this_page_depth}, other_page_depth={other_page_depth} "
    )]
    LeafDepthMismatch {
        page_id: usize,
        this_page_depth: usize,
        other_page_depth: usize,
    },
    #[error(
        "Page {page_id} detected freeblock that extends page start={start} end={end}"
    )]
    FreeBlockOutOfRange { page_id: usize, start: usize, end: usize },
    #[error(
        "Page {page_id} cell overlap detected at position={start} with previous_end={prev_end}. content_area={content_area}, is_free_block={is_free_block}"
    )]
    CellOverlap {
        page_id: usize,
        start: usize,
        prev_end: usize,
        content_area: usize,
        is_free_block: bool,
    },
    #[error("Page {page_id} unexpected fragmentation got={got}, expected={expected}")]
    UnexpectedFragmentation { page_id: usize, got: usize, expected: usize },
}
#[derive(Clone)]
struct IntegrityCheckPageEntry {
    page_idx: usize,
    level: usize,
    max_intkey: i64,
}
pub struct IntegrityCheckState {
    pub current_page: usize,
    page_stack: Vec<IntegrityCheckPageEntry>,
    first_leaf_level: Option<usize>,
}
impl IntegrityCheckState {
    pub fn new(page_idx: usize) -> Self {
        Self {
            current_page: page_idx,
            page_stack: vec![
                IntegrityCheckPageEntry { page_idx, level : 0, max_intkey : i64::MAX, }
            ],
            first_leaf_level: None,
        }
    }
}
impl std::fmt::Debug for IntegrityCheckState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IntegrityCheckState")
            .field("current_page", &self.current_page)
            .field("first_leaf_level", &self.first_leaf_level)
            .finish()
    }
}
/// Perform integrity check on a whole table/index. We check for:
/// 1. Correct order of keys in case of rowids.
/// 2. There are no overlap between cells.
/// 3. Cells do not scape outside expected range.
/// 4. Depth of leaf pages are equal.
/// 5. Overflow pages are correct (TODO)
///
/// In order to keep this reentrant, we keep a stack of pages we need to check. Ideally, like in
/// SQLlite, we would have implemented a recursive solution which would make it easier to check the
/// depth.
pub fn integrity_check(
    state: &mut IntegrityCheckState,
    errors: &mut Vec<IntegrityCheckError>,
    pager: &Rc<Pager>,
) -> Result<CursorResult<()>> {
    let Some(IntegrityCheckPageEntry { page_idx, level, max_intkey }) = state
        .page_stack
        .last()
        .cloned() else {
        return Ok(CursorResult::Ok(()));
    };
    let page = btree_read_page(pager, page_idx)?;
    return_if_locked_maybe_load!(pager, page);
    state.page_stack.pop();
    let page = page.get();
    let contents = page.get_contents();
    let usable_space = pager.usable_space() as u16;
    let mut coverage_checker = CoverageChecker::new(page.get().id);
    let mut next_rowid = max_intkey;
    for cell_idx in (0..contents.cell_count()).rev() {
        let (cell_start, cell_length) = contents
            .cell_get_raw_region(
                cell_idx,
                payload_overflow_threshold_max(contents.page_type(), usable_space),
                payload_overflow_threshold_min(contents.page_type(), usable_space),
                usable_space as usize,
            );
        if cell_start < contents.cell_content_area() as usize
            || cell_start > usable_space as usize - 4
        {
            errors
                .push(IntegrityCheckError::CellOutOfRange {
                    cell_idx,
                    page_id: page.get().id,
                    cell_start,
                    cell_end: cell_start + cell_length,
                    content_area: contents.cell_content_area() as usize,
                    usable_space: usable_space as usize,
                });
        }
        if cell_start + cell_length > usable_space as usize {
            errors
                .push(IntegrityCheckError::CellOverflowsPage {
                    cell_idx,
                    page_id: page.get().id,
                    cell_start,
                    cell_end: cell_start + cell_length,
                    content_area: contents.cell_content_area() as usize,
                    usable_space: usable_space as usize,
                });
        }
        coverage_checker.add_cell(cell_start, cell_start + cell_length);
        let cell = contents
            .cell_get(
                cell_idx,
                payload_overflow_threshold_max(contents.page_type(), usable_space),
                payload_overflow_threshold_min(contents.page_type(), usable_space),
                usable_space as usize,
            )?;
        match cell {
            BTreeCell::TableInteriorCell(table_interior_cell) => {
                state
                    .page_stack
                    .push(IntegrityCheckPageEntry {
                        page_idx: table_interior_cell._left_child_page as usize,
                        level: level + 1,
                        max_intkey: table_interior_cell._rowid,
                    });
                let rowid = table_interior_cell._rowid;
                if rowid > max_intkey || rowid > next_rowid {
                    errors
                        .push(IntegrityCheckError::CellRowidOutOfRange {
                            page_id: page.get().id,
                            cell_idx,
                            rowid,
                            max_intkey,
                            next_rowid,
                        });
                }
                next_rowid = rowid;
            }
            BTreeCell::TableLeafCell(table_leaf_cell) => {
                if let Some(expected_leaf_level) = state.first_leaf_level {
                    if expected_leaf_level != level {
                        errors
                            .push(IntegrityCheckError::LeafDepthMismatch {
                                page_id: page.get().id,
                                this_page_depth: level,
                                other_page_depth: expected_leaf_level,
                            });
                    }
                } else {
                    state.first_leaf_level = Some(level);
                }
                let rowid = table_leaf_cell._rowid;
                if rowid > max_intkey || rowid > next_rowid {
                    errors
                        .push(IntegrityCheckError::CellRowidOutOfRange {
                            page_id: page.get().id,
                            cell_idx,
                            rowid,
                            max_intkey,
                            next_rowid,
                        });
                }
                next_rowid = rowid;
            }
            BTreeCell::IndexInteriorCell(index_interior_cell) => {
                state
                    .page_stack
                    .push(IntegrityCheckPageEntry {
                        page_idx: index_interior_cell.left_child_page as usize,
                        level: level + 1,
                        max_intkey,
                    });
            }
            BTreeCell::IndexLeafCell(_) => {
                if let Some(expected_leaf_level) = state.first_leaf_level {
                    if expected_leaf_level != level {
                        errors
                            .push(IntegrityCheckError::LeafDepthMismatch {
                                page_id: page.get().id,
                                this_page_depth: level,
                                other_page_depth: expected_leaf_level,
                            });
                    }
                } else {
                    state.first_leaf_level = Some(level);
                }
            }
        }
    }
    let first_freeblock = contents.first_freeblock();
    if first_freeblock > 0 {
        let mut pc = first_freeblock;
        while pc > 0 {
            let next = contents.read_u16_no_offset(pc as usize);
            let size = contents.read_u16_no_offset(pc as usize + 2) as usize;
            if pc > usable_space - 4 {
                errors
                    .push(IntegrityCheckError::FreeBlockOutOfRange {
                        page_id: page.get().id,
                        start: pc as usize,
                        end: pc as usize + size,
                    });
                break;
            }
            coverage_checker.add_free_block(pc as usize, pc as usize + size);
            pc = next;
        }
    }
    coverage_checker
        .analyze(
            usable_space,
            contents.cell_content_area() as usize,
            errors,
            contents.num_frag_free_bytes() as usize,
        );
    Ok(CursorResult::IO)
}
pub fn btree_read_page(pager: &Rc<Pager>, page_idx: usize) -> Result<BTreePage> {
    pager
        .read_page(page_idx)
        .map(|page| {
            Arc::new(BTreePageInner {
                page: RefCell::new(page),
            })
        })
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct IntegrityCheckCellRange {
    start: usize,
    end: usize,
    is_free_block: bool,
}
impl Ord for IntegrityCheckCellRange {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.start.cmp(&other.start)
    }
}
impl PartialOrd for IntegrityCheckCellRange {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
#[cfg(debug_assertions)]
fn validate_cells_after_insertion(cell_array: &CellArray, leaf_data: bool) {
    for cell in &cell_array.cells {
        assert!(cell.len() >= 4);
        if leaf_data {
            assert!(cell[0] != 0, "payload is {:?}", cell);
        }
    }
}
pub struct CoverageChecker {
    /// Min-heap ordered by cell start
    heap: BinaryHeap<Reverse<IntegrityCheckCellRange>>,
    page_idx: usize,
}
impl CoverageChecker {
    pub fn new(page_idx: usize) -> Self {
        Self {
            heap: BinaryHeap::new(),
            page_idx,
        }
    }
    fn add_range(&mut self, cell_start: usize, cell_end: usize, is_free_block: bool) {
        self.heap
            .push(
                Reverse(IntegrityCheckCellRange {
                    start: cell_start,
                    end: cell_end,
                    is_free_block,
                }),
            );
    }
    pub fn add_cell(&mut self, cell_start: usize, cell_end: usize) {
        self.add_range(cell_start, cell_end, false);
    }
    pub fn add_free_block(&mut self, cell_start: usize, cell_end: usize) {
        self.add_range(cell_start, cell_end, true);
    }
    pub fn analyze(
        &mut self,
        usable_space: u16,
        content_area: usize,
        errors: &mut Vec<IntegrityCheckError>,
        expected_fragmentation: usize,
    ) {
        let mut fragmentation = 0;
        let mut prev_end = content_area;
        while let Some(cell) = self.heap.pop() {
            let start = cell.0.start;
            if prev_end > start {
                errors
                    .push(IntegrityCheckError::CellOverlap {
                        page_id: self.page_idx,
                        start,
                        prev_end,
                        content_area,
                        is_free_block: cell.0.is_free_block,
                    });
                break;
            } else {
                fragmentation += start - prev_end;
                prev_end = cell.0.end;
            }
        }
        fragmentation += usable_space as usize - prev_end;
        if fragmentation != expected_fragmentation {
            errors
                .push(IntegrityCheckError::UnexpectedFragmentation {
                    page_id: self.page_idx,
                    got: fragmentation,
                    expected: expected_fragmentation,
                });
        }
    }
}
/// Stack of pages representing the tree traversal order.
/// current_page represents the current page being used in the tree and current_page - 1 would be
/// the parent. Using current_page + 1 or higher is undefined behaviour.
struct PageStack {
    /// Pointer to the current page being consumed
    current_page: Cell<i32>,
    /// List of pages in the stack. Root page will be in index 0
    stack: RefCell<[Option<BTreePage>; BTCURSOR_MAX_DEPTH + 1]>,
    /// List of cell indices in the stack.
    /// cell_indices[current_page] is the current cell index being consumed. Similarly
    /// cell_indices[current_page-1] is the cell index of the parent of the current page
    /// that we save in case of going back up.
    /// There are two points that need special attention:
    ///  If cell_indices[current_page] = -1, it indicates that the current iteration has reached the start of the current_page
    ///  If cell_indices[current_page] = `cell_count`, it means that the current iteration has reached the end of the current_page
    cell_indices: RefCell<[i32; BTCURSOR_MAX_DEPTH + 1]>,
}
impl PageStack {
    fn increment_current(&self) {
        self.current_page.set(self.current_page.get() + 1);
    }
    fn decrement_current(&self) {
        assert!(self.current_page.get() > 0);
        self.current_page.set(self.current_page.get() - 1);
    }
    /// Push a new page onto the stack.
    /// This effectively means traversing to a child page.
    #[instrument(skip_all, level = Level::TRACE, name = "pagestack::push")]
    fn _push(&self, page: BTreePage, starting_cell_idx: i32) {
        tracing::trace!(
            current = self.current_page.get(), new_page_id = page.get().get().id,
        );
        self.increment_current();
        let current = self.current_page.get();
        assert!(
            current < BTCURSOR_MAX_DEPTH as i32,
            "corrupted database, stack is bigger than expected"
        );
        assert!(current >= 0);
        self.stack.borrow_mut()[current as usize] = Some(page);
        self.cell_indices.borrow_mut()[current as usize] = starting_cell_idx;
    }
    fn push(&self, page: BTreePage) {
        self._push(page, -1);
    }
    fn push_backwards(&self, page: BTreePage) {
        self._push(page, i32::MAX);
    }
    /// Pop a page off the stack.
    /// This effectively means traversing back up to a parent page.
    #[instrument(skip_all, level = Level::TRACE, name = "pagestack::pop")]
    fn pop(&self) {
        let current = self.current_page.get();
        assert!(current >= 0);
        tracing::trace!(current);
        self.cell_indices.borrow_mut()[current as usize] = 0;
        self.stack.borrow_mut()[current as usize] = None;
        self.decrement_current();
    }
    /// Get the top page on the stack.
    /// This is the page that is currently being traversed.
    #[instrument(skip(self), level = Level::TRACE, name = "pagestack::top")]
    fn top(&self) -> BTreePage {
        let page = self.stack.borrow()[self.current()].as_ref().unwrap().clone();
        tracing::trace!(current = self.current(), page_id = page.get().get().id);
        page
    }
    /// Current page pointer being used
    fn current(&self) -> usize {
        let current = self.current_page.get() as usize;
        assert!(self.current_page.get() >= 0);
        current
    }
    /// Cell index of the current page
    fn current_cell_index(&self) -> i32 {
        let current = self.current();
        self.cell_indices.borrow()[current]
    }
    /// Check if the current cell index is less than 0.
    /// This means we have been iterating backwards and have reached the start of the page.
    fn current_cell_index_less_than_min(&self) -> bool {
        let cell_idx = self.current_cell_index();
        cell_idx < 0
    }
    /// Advance the current cell index of the current page to the next cell.
    /// We usually advance after going traversing a new page
    #[instrument(skip(self), level = Level::TRACE, name = "pagestack::advance")]
    fn advance(&self) {
        let current = self.current();
        tracing::trace!(
            curr_cell_index = self.cell_indices.borrow() [current], cell_indices = ? self
            .cell_indices,
        );
        self.cell_indices.borrow_mut()[current] += 1;
    }
    #[instrument(skip(self), level = Level::TRACE, name = "pagestack::retreat")]
    fn retreat(&self) {
        let current = self.current();
        tracing::trace!(
            curr_cell_index = self.cell_indices.borrow() [current], cell_indices = ? self
            .cell_indices,
        );
        self.cell_indices.borrow_mut()[current] -= 1;
    }
    fn set_cell_index(&self, idx: i32) {
        let current = self.current();
        self.cell_indices.borrow_mut()[current] = idx;
    }
    fn has_parent(&self) -> bool {
        self.current_page.get() > 0
    }
    fn clear(&self) {
        self.current_page.set(-1);
    }
    pub fn parent_page(&self) -> Option<BTreePage> {
        if self.current_page.get() > 0 {
            Some(self.stack.borrow()[self.current() - 1].as_ref().unwrap().clone())
        } else {
            None
        }
    }
}
/// Used for redistributing cells during a balance operation.
struct CellArray {
    cells: Vec<&'static mut [u8]>,
    number_of_cells_per_page: [u16; 5],
}
impl CellArray {
    pub fn cell_size(&self, cell_idx: usize) -> u16 {
        self.cells[cell_idx].len() as u16
    }
    pub fn cell_count(&self, page_idx: usize) -> usize {
        self.number_of_cells_per_page[page_idx] as usize
    }
}
impl BTreePageInner {
    pub fn get(&self) -> PageRef {
        self.page.borrow().clone()
    }
}
/// Try to find a free block available and allocate it if found
fn find_free_cell(
    page_ref: &PageContent,
    usable_space: u16,
    amount: usize,
) -> Result<usize> {
    let mut prev_pc = page_ref.offset + offset::BTREE_FIRST_FREEBLOCK;
    let mut pc = page_ref.first_freeblock() as usize;
    let maxpc = usable_space as usize - amount;
    while pc <= maxpc {
        if pc + 4 > usable_space as usize {
            return_corrupt!("Free block header extends beyond page");
        }
        let next = page_ref.read_u16_no_offset(pc);
        let size = page_ref.read_u16_no_offset(pc + 2);
        if amount <= size as usize {
            let new_size = size as usize - amount;
            if new_size < 4 {
                if page_ref.num_frag_free_bytes() > 57 {
                    return Ok(0);
                }
                page_ref.write_u16_no_offset(prev_pc, next);
                let frag = page_ref.num_frag_free_bytes() + new_size as u8;
                page_ref.write_u8(offset::BTREE_FRAGMENTED_BYTES_COUNT, frag);
                return Ok(pc);
            } else if new_size + pc > maxpc {
                return_corrupt!("Free block extends beyond page end");
            } else {
                page_ref.write_u16_no_offset(pc + 2, new_size as u16);
                return Ok(pc + new_size);
            }
        }
        prev_pc = pc;
        pc = next as usize;
        if pc <= prev_pc {
            if pc != 0 {
                return_corrupt!("Free list not in ascending order");
            }
            return Ok(0);
        }
    }
    if pc > maxpc + amount - 4 {
        return_corrupt!("Free block chain extends beyond page end");
    }
    Ok(0)
}
pub fn btree_init_page(
    page: &BTreePage,
    page_type: PageType,
    offset: usize,
    usable_space: u16,
) {
    let contents = page.get();
    tracing::debug!("btree_init_page(id={}, offset={})", contents.get().id, offset);
    let contents = contents.get().contents.as_mut().unwrap();
    contents.offset = offset;
    let id = page_type as u8;
    contents.write_u8(offset::BTREE_PAGE_TYPE, id);
    contents.write_u16(offset::BTREE_FIRST_FREEBLOCK, 0);
    contents.write_u16(offset::BTREE_CELL_COUNT, 0);
    contents.write_u16(offset::BTREE_CELL_CONTENT_AREA, usable_space);
    contents.write_u8(offset::BTREE_FRAGMENTED_BYTES_COUNT, 0);
    contents.write_u32(offset::BTREE_RIGHTMOST_PTR, 0);
}
fn to_static_buf(buf: &mut [u8]) -> &'static mut [u8] {
    unsafe { std::mem::transmute::<&mut [u8], &'static mut [u8]>(buf) }
}
fn edit_page(
    page: &mut PageContent,
    start_old_cells: usize,
    start_new_cells: usize,
    number_new_cells: usize,
    cell_array: &CellArray,
    usable_space: u16,
) -> Result<()> {
    tracing::debug!(
        "edit_page start_old_cells={} start_new_cells={} number_new_cells={} cell_array={}",
        start_old_cells, start_new_cells, number_new_cells, cell_array.cells.len()
    );
    let end_old_cells = start_old_cells + page.cell_count() + page.overflow_cells.len();
    let end_new_cells = start_new_cells + number_new_cells;
    let mut count_cells = page.cell_count();
    if start_old_cells < start_new_cells {
        debug_validate_cells!(page, usable_space);
        let number_to_shift = page_free_array(
            page,
            start_old_cells,
            start_new_cells - start_old_cells,
            cell_array,
            usable_space,
        )?;
        shift_cells_left(page, count_cells, number_to_shift);
        count_cells -= number_to_shift;
        debug_validate_cells!(page, usable_space);
    }
    if end_new_cells < end_old_cells {
        debug_validate_cells!(page, usable_space);
        let number_tail_removed = page_free_array(
            page,
            end_new_cells,
            end_old_cells - end_new_cells,
            cell_array,
            usable_space,
        )?;
        assert!(count_cells >= number_tail_removed);
        count_cells -= number_tail_removed;
        debug_validate_cells!(page, usable_space);
    }
    defragment_page(page, usable_space);
    if start_new_cells < start_old_cells {
        let count = number_new_cells.min(start_old_cells - start_new_cells);
        page_insert_array(page, start_new_cells, count, cell_array, 0, usable_space)?;
        count_cells += count;
    }
    debug_validate_cells!(page, usable_space);
    for i in 0..page.overflow_cells.len() {
        let overflow_cell = &page.overflow_cells[i];
        if start_old_cells + overflow_cell.index >= start_new_cells {
            let cell_idx = start_old_cells + overflow_cell.index - start_new_cells;
            if cell_idx < number_new_cells {
                count_cells += 1;
                page_insert_array(
                    page,
                    start_new_cells + cell_idx,
                    1,
                    cell_array,
                    cell_idx,
                    usable_space,
                )?;
            }
        }
    }
    debug_validate_cells!(page, usable_space);
    page_insert_array(
        page,
        start_new_cells + count_cells,
        number_new_cells - count_cells,
        cell_array,
        count_cells,
        usable_space,
    )?;
    debug_validate_cells!(page, usable_space);
    page.write_u16(offset::BTREE_CELL_COUNT, number_new_cells as u16);
    Ok(())
}
/// Shifts the cell pointers in the B-tree page to the left by a specified number of positions.
///
/// # Parameters
/// - `page`: A mutable reference to the `PageContent` representing the B-tree page.
/// - `count_cells`: The total number of cells currently in the page.
/// - `number_to_shift`: The number of cell pointers to shift to the left.
///
/// # Behavior
/// This function modifies the cell pointer array within the page by copying memory regions.
/// It shifts the pointers starting from `number_to_shift` to the beginning of the array,
/// effectively removing the first `number_to_shift` pointers.
fn shift_cells_left(page: &mut PageContent, count_cells: usize, number_to_shift: usize) {
    let buf = page.as_ptr();
    let (start, _) = page.cell_pointer_array_offset_and_size();
    buf.copy_within(start + (number_to_shift * 2)..start + (count_cells * 2), start);
}
fn page_free_array(
    page: &mut PageContent,
    first: usize,
    count: usize,
    cell_array: &CellArray,
    usable_space: u16,
) -> Result<usize> {
    tracing::debug!("page_free_array {}..{}", first, first + count);
    let buf = &mut page.as_ptr()[page.offset..usable_space as usize];
    let buf_range = buf.as_ptr_range();
    let mut number_of_cells_removed = 0;
    let mut number_of_cells_buffered = 0;
    let mut buffered_cells_offsets: [u16; 10] = [0; 10];
    let mut buffered_cells_ends: [u16; 10] = [0; 10];
    for i in first..first + count {
        let cell = &cell_array.cells[i];
        let cell_pointer = cell.as_ptr_range();
        if cell_pointer.start >= buf_range.start && cell_pointer.start < buf_range.end {
            assert!(
                cell_pointer.end >= buf_range.start && cell_pointer.end <= buf_range.end,
                "whole cell should be inside the page"
            );
            let offset = (cell_pointer.start as usize - buf_range.start as usize) as u16;
            let len = (cell_pointer.end as usize - cell_pointer.start as usize) as u16;
            assert!(len > 0, "cell size should be greater than 0");
            let end = offset + len;
            let mut j = 0;
            while j < number_of_cells_buffered {
                if buffered_cells_offsets[j] == end {
                    buffered_cells_offsets[j] = offset;
                    break;
                } else if buffered_cells_ends[j] == offset {
                    buffered_cells_ends[j] = end;
                    break;
                }
                j += 1;
            }
            if j >= number_of_cells_buffered {
                if number_of_cells_buffered >= buffered_cells_offsets.len() {
                    for j in 0..number_of_cells_buffered {
                        free_cell_range(
                            page,
                            buffered_cells_offsets[j],
                            buffered_cells_ends[j] - buffered_cells_offsets[j],
                            usable_space,
                        )?;
                    }
                    number_of_cells_buffered = 0;
                }
                buffered_cells_offsets[number_of_cells_buffered] = offset;
                buffered_cells_ends[number_of_cells_buffered] = end;
                number_of_cells_buffered += 1;
            }
            number_of_cells_removed += 1;
        }
    }
    for j in 0..number_of_cells_buffered {
        free_cell_range(
            page,
            buffered_cells_offsets[j],
            buffered_cells_ends[j] - buffered_cells_offsets[j],
            usable_space,
        )?;
    }
    page.write_u16(
        offset::BTREE_CELL_COUNT,
        page.cell_count() as u16 - number_of_cells_removed as u16,
    );
    Ok(number_of_cells_removed)
}
fn page_insert_array(
    page: &mut PageContent,
    first: usize,
    count: usize,
    cell_array: &CellArray,
    mut start_insert: usize,
    usable_space: u16,
) -> Result<()> {
    tracing::debug!(
        "page_insert_array(cell_array.cells={}..{}, cell_count={}, page_type={:?})",
        first, first + count, page.cell_count(), page.page_type()
    );
    for i in first..first + count {
        insert_into_cell(page, cell_array.cells[i], start_insert, usable_space)?;
        start_insert += 1;
    }
    debug_validate_cells!(page, usable_space);
    Ok(())
}
/// Free the range of bytes that a cell occupies.
/// This function also updates the freeblock list in the page.
/// Freeblocks are used to keep track of free space in the page,
/// and are organized as a linked list.
fn free_cell_range(
    page: &mut PageContent,
    mut offset: u16,
    len: u16,
    usable_space: u16,
) -> Result<()> {
    if len < 4 {
        return_corrupt!("Minimum cell size is 4");
    }
    if offset > usable_space.saturating_sub(4) {
        return_corrupt!("Start offset beyond usable space");
    }
    let mut size = len;
    let mut end = offset + len;
    let mut pointer_to_pc = page.offset as u16 + 1;
    let pc = if page.first_freeblock() == 0 {
        0
    } else {
        let first_block = page.first_freeblock();
        let mut pc = first_block;
        while pc < offset {
            if pc <= pointer_to_pc {
                if pc == 0 {
                    break;
                }
                return_corrupt!("free cell range free block not in ascending order");
            }
            let next = page.read_u16_no_offset(pc as usize);
            pointer_to_pc = pc;
            pc = next;
        }
        if pc > usable_space - 4 {
            return_corrupt!("Free block beyond usable space");
        }
        let mut removed_fragmentation = 0;
        if pc > 0 && offset + len + 3 >= pc {
            removed_fragmentation = (pc - end) as u8;
            if end > pc {
                return_corrupt!("Invalid block overlap");
            }
            end = pc + page.read_u16_no_offset(pc as usize + 2);
            if end > usable_space {
                return_corrupt!("Coalesced block extends beyond page");
            }
            size = end - offset;
            pc = page.read_u16_no_offset(pc as usize);
        }
        if pointer_to_pc > page.offset as u16 + 1 {
            let prev_end = pointer_to_pc
                + page.read_u16_no_offset(pointer_to_pc as usize + 2);
            if prev_end + 3 >= offset {
                if prev_end > offset {
                    return_corrupt!("Invalid previous block overlap");
                }
                removed_fragmentation += (offset - prev_end) as u8;
                size = end - pointer_to_pc;
                offset = pointer_to_pc;
            }
        }
        if removed_fragmentation > page.num_frag_free_bytes() {
            return_corrupt!(
                format!("Invalid fragmentation count. Had {} and removed {}", page
                .num_frag_free_bytes(), removed_fragmentation)
            );
        }
        let frag = page.num_frag_free_bytes() - removed_fragmentation;
        page.write_u8(offset::BTREE_FRAGMENTED_BYTES_COUNT, frag);
        pc
    };
    if offset <= page.cell_content_area() {
        if offset < page.cell_content_area() {
            return_corrupt!("Free block before content area");
        }
        if pointer_to_pc != page.offset as u16 + offset::BTREE_FIRST_FREEBLOCK as u16 {
            return_corrupt!("Invalid content area merge");
        }
        page.write_u16(offset::BTREE_FIRST_FREEBLOCK, pc);
        page.write_u16(offset::BTREE_CELL_CONTENT_AREA, end);
    } else {
        page.write_u16_no_offset(pointer_to_pc as usize, offset);
        page.write_u16_no_offset(offset as usize, pc);
        page.write_u16_no_offset(offset as usize + 2, size);
    }
    Ok(())
}
/// Defragment a page. This means packing all the cells to the end of the page.
fn defragment_page(page: &PageContent, usable_space: u16) {
    debug_validate_cells!(page, usable_space);
    tracing::debug!("defragment_page");
    let cloned_page = page.clone();
    let mut cbrk = usable_space;
    let last_cell = usable_space - 4;
    let first_cell = cloned_page.unallocated_region_start() as u16;
    if cloned_page.cell_count() > 0 {
        let read_buf = cloned_page.as_ptr();
        let write_buf = page.as_ptr();
        for i in 0..cloned_page.cell_count() {
            let (cell_offset, _) = page.cell_pointer_array_offset_and_size();
            let cell_idx = cell_offset + (i * 2);
            let pc = cloned_page.read_u16_no_offset(cell_idx);
            if pc > last_cell {
                unimplemented!("corrupted page");
            }
            assert!(pc <= last_cell);
            let (_, size) = cloned_page
                .cell_get_raw_region(
                    i,
                    payload_overflow_threshold_max(page.page_type(), usable_space),
                    payload_overflow_threshold_min(page.page_type(), usable_space),
                    usable_space as usize,
                );
            let size = size as u16;
            cbrk -= size;
            if cbrk < first_cell || pc + size > usable_space {
                todo!("corrupt");
            }
            assert!(cbrk + size <= usable_space && cbrk >= first_cell);
            page.write_u16_no_offset(cell_idx, cbrk);
            write_buf[cbrk as usize..cbrk as usize + size as usize]
                .copy_from_slice(&read_buf[pc as usize..pc as usize + size as usize]);
        }
    }
    assert!(cbrk >= first_cell);
    page.write_u16(offset::BTREE_CELL_CONTENT_AREA, cbrk);
    page.write_u16(offset::BTREE_FIRST_FREEBLOCK, 0);
    page.write_u8(offset::BTREE_FRAGMENTED_BYTES_COUNT, 0);
    debug_validate_cells!(page, usable_space);
}
#[cfg(debug_assertions)]
/// Only enabled in debug mode, where we ensure that all cells are valid.
fn debug_validate_cells_core(page: &PageContent, usable_space: u16) {
    for i in 0..page.cell_count() {
        let (offset, size) = page
            .cell_get_raw_region(
                i,
                payload_overflow_threshold_max(page.page_type(), usable_space),
                payload_overflow_threshold_min(page.page_type(), usable_space),
                usable_space as usize,
            );
        let buf = &page.as_ptr()[offset..offset + size];
        assert!(
            size >= 2,
            "cell size should be at least 2 bytes idx={}, cell={:?}, offset={}", i, buf,
            offset
        );
        if page.is_leaf() {
            assert!(page.as_ptr() [offset] != 0);
        }
        assert!(
            offset + size <= usable_space as usize, "cell spans out of usable space"
        );
    }
}
/// Insert a record into a cell.
/// If the cell overflows, an overflow cell is created.
/// insert_into_cell() is called from insert_into_page(),
/// and the overflow cell count is used to determine if the page overflows,
/// i.e. whether we need to balance the btree after the insert.
fn insert_into_cell(
    page: &mut PageContent,
    payload: &[u8],
    cell_idx: usize,
    usable_space: u16,
) -> Result<()> {
    assert!(
        cell_idx <= page.cell_count() + page.overflow_cells.len(),
        "attempting to add cell to an incorrect place cell_idx={} cell_count={}",
        cell_idx, page.cell_count()
    );
    let free = compute_free_space(page, usable_space);
    const CELL_POINTER_SIZE_BYTES: usize = 2;
    let enough_space = payload.len() + CELL_POINTER_SIZE_BYTES <= free as usize;
    if !enough_space {
        page.overflow_cells
            .push(OverflowCell {
                index: cell_idx,
                payload: Pin::new(Vec::from(payload)),
            });
        return Ok(());
    }
    let new_cell_data_pointer = allocate_cell_space(
        page,
        payload.len() as u16,
        usable_space,
    )?;
    tracing::debug!(
        "insert_into_cell(idx={}, pc={}, size={})", cell_idx, new_cell_data_pointer,
        payload.len()
    );
    assert!(new_cell_data_pointer + payload.len() as u16 <= usable_space);
    let buf = page.as_ptr();
    buf[new_cell_data_pointer as usize..new_cell_data_pointer as usize + payload.len()]
        .copy_from_slice(payload);
    let (cell_pointer_array_start, _) = page.cell_pointer_array_offset_and_size();
    let cell_pointer_cur_idx = cell_pointer_array_start
        + (CELL_POINTER_SIZE_BYTES * cell_idx);
    let n_cells_forward = page.cell_count() - cell_idx;
    let n_bytes_forward = CELL_POINTER_SIZE_BYTES * n_cells_forward;
    if n_bytes_forward > 0 {
        buf.copy_within(
            cell_pointer_cur_idx..cell_pointer_cur_idx + n_bytes_forward,
            cell_pointer_cur_idx + CELL_POINTER_SIZE_BYTES,
        );
    }
    page.write_u16_no_offset(cell_pointer_cur_idx, new_cell_data_pointer);
    let new_n_cells = (page.cell_count() + 1) as u16;
    page.write_u16(offset::BTREE_CELL_COUNT, new_n_cells);
    debug_validate_cells!(page, usable_space);
    Ok(())
}
/// Free blocks can be zero, meaning the "real free space" that can be used to allocate is expected to be between first cell byte
/// and end of cell pointer area.
#[allow(unused_assignments)]
fn compute_free_space(page: &PageContent, usable_space: u16) -> u16 {
    let usable_space = usable_space as usize;
    let mut cell_content_area_start = page.cell_content_area();
    if cell_content_area_start == 0 {
        cell_content_area_start = u16::MAX;
    }
    let pointer_size = if matches!(
        page.page_type(), PageType::TableLeaf | PageType::IndexLeaf
    ) {
        0
    } else {
        4
    };
    let first_cell = page.offset + 8 + pointer_size + (2 * page.cell_count());
    let mut free_space_bytes = cell_content_area_start as usize
        + page.num_frag_free_bytes() as usize;
    let mut cur_freeblock_ptr = page.first_freeblock() as usize;
    if cur_freeblock_ptr > 0 {
        if cur_freeblock_ptr < cell_content_area_start as usize {
            todo!("corrupted page");
        }
        let mut next = 0;
        let mut size = 0;
        loop {
            next = page.read_u16_no_offset(cur_freeblock_ptr) as usize;
            size = page.read_u16_no_offset(cur_freeblock_ptr + 2) as usize;
            free_space_bytes += size;
            if next <= cur_freeblock_ptr + size + 3 {
                break;
            }
            cur_freeblock_ptr = next;
        }
        assert!(next == 0, "corrupted page: freeblocks list not in ascending order");
        assert!(
            cur_freeblock_ptr + size <= usable_space,
            "corrupted page: last freeblock extends last page end"
        );
    }
    assert!(
        free_space_bytes <= usable_space,
        "corrupted page: free space is greater than usable space"
    );
    free_space_bytes as u16 - first_cell as u16
}
/// Allocate space for a cell on a page.
fn allocate_cell_space(
    page_ref: &PageContent,
    amount: u16,
    usable_space: u16,
) -> Result<u16> {
    let amount = amount as usize;
    let (cell_offset, _) = page_ref.cell_pointer_array_offset_and_size();
    let gap = cell_offset + 2 * page_ref.cell_count();
    let mut top = page_ref.cell_content_area() as usize;
    if page_ref.first_freeblock() != 0 && gap + 2 <= top {
        let pc = find_free_cell(page_ref, usable_space, amount)?;
        if pc != 0 {
            return Ok(pc as u16);
        }
    }
    if gap + 2 + amount > top {
        defragment_page(page_ref, usable_space);
        top = page_ref.read_u16(offset::BTREE_CELL_CONTENT_AREA) as usize;
    }
    top -= amount;
    page_ref.write_u16(offset::BTREE_CELL_CONTENT_AREA, top as u16);
    assert!(top + amount <= usable_space as usize);
    Ok(top as u16)
}
/// Fill in the cell payload with the record.
/// If the record is too large to fit in the cell, it will spill onto overflow pages.
fn fill_cell_payload(
    page_type: PageType,
    int_key: Option<i64>,
    cell_payload: &mut Vec<u8>,
    record: &ImmutableRecord,
    usable_space: u16,
    pager: Rc<Pager>,
) {
    assert!(matches!(page_type, PageType::TableLeaf | PageType::IndexLeaf));
    let record_buf = record.get_payload().to_vec();
    if matches!(page_type, PageType::TableLeaf) {
        let int_key = int_key.unwrap();
        write_varint_to_vec(record_buf.len() as u64, cell_payload);
        write_varint_to_vec(int_key as u64, cell_payload);
    } else {
        write_varint_to_vec(record_buf.len() as u64, cell_payload);
    }
    let payload_overflow_threshold_max = payload_overflow_threshold_max(
        page_type,
        usable_space,
    );
    tracing::debug!(
        "fill_cell_payload(record_size={}, payload_overflow_threshold_max={})",
        record_buf.len(), payload_overflow_threshold_max
    );
    if record_buf.len() <= payload_overflow_threshold_max {
        cell_payload.extend_from_slice(record_buf.as_slice());
        return;
    }
    let payload_overflow_threshold_min = payload_overflow_threshold_min(
        page_type,
        usable_space,
    );
    let mut space_left = payload_overflow_threshold_min
        + (record_buf.len() - payload_overflow_threshold_min)
            % (usable_space as usize - 4);
    if space_left > payload_overflow_threshold_max {
        space_left = payload_overflow_threshold_min;
    }
    let cell_size = space_left + cell_payload.len() + 4;
    let mut to_copy_buffer = record_buf.as_slice();
    let prev_size = cell_payload.len();
    cell_payload.resize(prev_size + space_left + 4, 0);
    let mut pointer = unsafe { cell_payload.as_mut_ptr().add(prev_size) };
    let mut pointer_to_next = unsafe {
        cell_payload.as_mut_ptr().add(prev_size + space_left)
    };
    let mut overflow_pages = Vec::new();
    loop {
        let to_copy = space_left.min(to_copy_buffer.len());
        unsafe { std::ptr::copy(to_copy_buffer.as_ptr(), pointer, to_copy) };
        let left = to_copy_buffer.len() - to_copy;
        if left == 0 {
            break;
        }
        let overflow_page = pager.allocate_overflow_page();
        overflow_pages.push(overflow_page.clone());
        {
            let id = overflow_page.get().id as u32;
            let contents = overflow_page.get().contents.as_mut().unwrap();
            let buf = contents.as_ptr();
            let as_bytes = id.to_be_bytes();
            unsafe { std::ptr::copy(as_bytes.as_ptr(), pointer_to_next, 4) };
            pointer = unsafe { buf.as_mut_ptr().add(4) };
            pointer_to_next = buf.as_mut_ptr();
            space_left = usable_space as usize - 4;
        }
        to_copy_buffer = &to_copy_buffer[to_copy..];
    }
    assert_eq!(cell_size, cell_payload.len());
}
/// Returns the maximum payload size (X) that can be stored directly on a b-tree page without spilling to overflow pages.
///
/// For table leaf pages: X = usable_size - 35
/// For index pages: X = ((usable_size - 12) * 64/255) - 23
///
/// The usable size is the total page size less the reserved space at the end of each page.
/// These thresholds are designed to:
/// - Give a minimum fanout of 4 for index b-trees
/// - Ensure enough payload is on the b-tree page that the record header can usually be accessed
///   without consulting an overflow page
fn payload_overflow_threshold_max(page_type: PageType, usable_space: u16) -> usize {
    match page_type {
        PageType::IndexInterior | PageType::IndexLeaf => {
            ((usable_space as usize - 12) * 64 / 255) - 23
        }
        PageType::TableInterior | PageType::TableLeaf => usable_space as usize - 35,
    }
}
/// Returns the minimum payload size (M) that must be stored on the b-tree page before spilling to overflow pages is allowed.
///
/// For all page types: M = ((usable_size - 12) * 32/255) - 23
///
/// When payload size P exceeds max_local():
/// - If K = M + ((P-M) % (usable_size-4)) <= max_local(): store K bytes on page
/// - Otherwise: store M bytes on page
///
/// The remaining bytes are stored on overflow pages in both cases.
fn payload_overflow_threshold_min(_page_type: PageType, usable_space: u16) -> usize {
    ((usable_space as usize - 12) * 32 / 255) - 23
}
/// Drop a cell from a page.
/// This is done by freeing the range of bytes that the cell occupies.
fn drop_cell(page: &mut PageContent, cell_idx: usize, usable_space: u16) -> Result<()> {
    let (cell_start, cell_len) = page
        .cell_get_raw_region(
            cell_idx,
            payload_overflow_threshold_max(page.page_type(), usable_space),
            payload_overflow_threshold_min(page.page_type(), usable_space),
            usable_space as usize,
        );
    free_cell_range(page, cell_start as u16, cell_len as u16, usable_space)?;
    if page.cell_count() > 1 {
        shift_pointers_left(page, cell_idx);
    } else {
        page.write_u16(offset::BTREE_CELL_CONTENT_AREA, usable_space);
        page.write_u16(offset::BTREE_FIRST_FREEBLOCK, 0);
        page.write_u8(offset::BTREE_FRAGMENTED_BYTES_COUNT, 0);
    }
    page.write_u16(offset::BTREE_CELL_COUNT, page.cell_count() as u16 - 1);
    debug_validate_cells!(page, usable_space);
    Ok(())
}
/// Shift pointers to the left once starting from a cell position
/// This is useful when we remove a cell and we want to move left the cells from the right to fill
/// the empty space that's not needed
fn shift_pointers_left(page: &mut PageContent, cell_idx: usize) {
    assert!(page.cell_count() > 0);
    let buf = page.as_ptr();
    let (start, _) = page.cell_pointer_array_offset_and_size();
    let start = start + (cell_idx * 2) + 2;
    let right_cells = page.cell_count() - cell_idx - 1;
    let amount_to_shift = right_cells * 2;
    buf.copy_within(start..start + amount_to_shift, start - 2);
}

