impl BTreeCursor {
    fn insert_into_page(&mut self, bkey: &BTreeKey) -> Result<CursorResult<()>> {
        let record = bkey.get_record().expect("expected record present on insert");
        if let CursorState::None = &self.state {
            self.state = CursorState::Write(WriteInfo::new());
        }
        let ret = loop {
            let write_state = {
                let write_info = self
                    .state
                    .mut_write_info()
                    .expect("can't insert while counting");
                write_info.state
            };
            match write_state {
                WriteState::Start => {
                    let page = self.stack.top();
                    return_if_locked_maybe_load!(self.pager, page);
                    let (cell_idx, page_type) = {
                        return_if_locked!(page.get());
                        let page = page.get();
                        page.set_dirty();
                        self.pager.add_dirty(page.get().id);
                        let page = page.get().contents.as_mut().unwrap();
                        assert!(
                            matches!(page.page_type(), PageType::TableLeaf |
                            PageType::IndexLeaf)
                        );
                        (return_if_io!(self.find_cell(page, bkey)), page.page_type())
                    };
                    self.stack.set_cell_index(cell_idx as i32);
                    tracing::debug!(cell_idx);
                    if cell_idx < page.get().get_contents().cell_count() {
                        let cell = page
                            .get()
                            .get_contents()
                            .cell_get(
                                cell_idx,
                                payload_overflow_threshold_max(
                                    page_type,
                                    self.usable_space() as u16,
                                ),
                                payload_overflow_threshold_min(
                                    page_type,
                                    self.usable_space() as u16,
                                ),
                                self.usable_space(),
                            )?;
                        match cell {
                            BTreeCell::TableLeafCell(tbl_leaf) => {
                                if tbl_leaf._rowid == bkey.to_rowid() {
                                    tracing::debug!(
                                        "found exact match with cell_idx={cell_idx}, overwriting"
                                    );
                                    self.overwrite_cell(page.clone(), cell_idx, record)?;
                                    self
                                        .state
                                        .mut_write_info()
                                        .expect("expected write info")
                                        .state = WriteState::Finish;
                                    continue;
                                }
                            }
                            BTreeCell::IndexLeafCell(..) => {
                                let cmp = compare_immutable(
                                    record.get_values(),
                                    self.get_immutable_record().as_ref().unwrap().get_values(),
                                    self.key_sort_order(),
                                    &self.collations,
                                );
                                if cmp == Ordering::Equal {
                                    tracing::debug!(
                                        "found exact match with cell_idx={cell_idx}, overwriting"
                                    );
                                    self.has_record.set(true);
                                    self.overwrite_cell(page.clone(), cell_idx, record)?;
                                    self
                                        .state
                                        .mut_write_info()
                                        .expect("expected write info")
                                        .state = WriteState::Finish;
                                    continue;
                                }
                            }
                            other => {
                                panic!(
                                    "unexpected cell type, expected TableLeaf or IndexLeaf, found: {:?}",
                                    other
                                )
                            }
                        }
                    }
                    let mut cell_payload: Vec<u8> = Vec::with_capacity(record.len() + 4);
                    fill_cell_payload(
                        page_type,
                        bkey.maybe_rowid(),
                        &mut cell_payload,
                        record,
                        self.usable_space() as u16,
                        self.pager.clone(),
                    );
                    let overflow = {
                        let page = page.get();
                        let contents = page.get().contents.as_mut().unwrap();
                        tracing::debug!(
                            name : "overflow", cell_count = contents.cell_count()
                        );
                        insert_into_cell(
                            contents,
                            cell_payload.as_slice(),
                            cell_idx,
                            self.usable_space() as u16,
                        )?;
                        contents.overflow_cells.len()
                    };
                    self.stack.set_cell_index(cell_idx as i32);
                    if overflow > 0 {
                        tracing::debug!(
                            page = page.get().get().id, cell_idx, "balance triggered:"
                        );
                        self.save_context(
                            match bkey {
                                BTreeKey::TableRowId(rowid) => {
                                    CursorContext::TableRowId(rowid.0)
                                }
                                BTreeKey::IndexKey(record) => {
                                    CursorContext::IndexKeyRowId((*record).clone())
                                }
                            },
                        );
                        let write_info = self
                            .state
                            .mut_write_info()
                            .expect("can't count while inserting");
                        write_info.state = WriteState::BalanceStart;
                    } else {
                        let write_info = self
                            .state
                            .mut_write_info()
                            .expect("can't count while inserting");
                        write_info.state = WriteState::Finish;
                    }
                }
                WriteState::BalanceStart
                | WriteState::BalanceNonRoot
                | WriteState::BalanceNonRootWaitLoadPages => {
                    return_if_io!(self.balance());
                }
                WriteState::Finish => {
                    break Ok(CursorResult::Ok(()));
                }
            };
        };
        if matches!(self.state.write_info().unwrap().state, WriteState::Finish) {
            return_if_io!(self.restore_context());
        }
        self.state = CursorState::None;
        ret
    }
    /// Balance a leaf page.
    /// Balancing is done when a page overflows.
    /// see e.g. https://en.wikipedia.org/wiki/B-tree
    ///
    /// This is a naive algorithm that doesn't try to distribute cells evenly by content.
    /// It will try to split the page in half by keys not by content.
    /// Sqlite tries to have a page at least 40% full.
    #[instrument(skip(self), level = Level::TRACE)]
    fn balance(&mut self) -> Result<CursorResult<()>> {
        assert!(
            matches!(self.state, CursorState::Write(_)),
            "Cursor must be in balancing state"
        );
        loop {
            let state = self.state.write_info().expect("must be balancing").state;
            match state {
                WriteState::BalanceStart => {
                    assert!(
                        self.state.write_info().unwrap().balance_info.borrow().is_none(),
                        "BalanceInfo should be empty on start"
                    );
                    let current_page = self.stack.top();
                    {
                        let current_page = current_page.get();
                        let page = current_page.get().contents.as_mut().unwrap();
                        let usable_space = self.usable_space();
                        let free_space = compute_free_space(page, usable_space as u16);
                        if page.overflow_cells.is_empty()
                            && (!self.stack.has_parent()
                                || free_space as usize * 3 <= usable_space * 2)
                        {
                            let write_info = self.state.mut_write_info().unwrap();
                            write_info.state = WriteState::Finish;
                            return Ok(CursorResult::Ok(()));
                        }
                    }
                    if !self.stack.has_parent() {
                        self.balance_root();
                    }
                    let write_info = self.state.mut_write_info().unwrap();
                    write_info.state = WriteState::BalanceNonRoot;
                    self.stack.pop();
                    return_if_io!(self.balance_non_root());
                }
                WriteState::BalanceNonRoot | WriteState::BalanceNonRootWaitLoadPages => {
                    return_if_io!(self.balance_non_root());
                }
                WriteState::Finish => return Ok(CursorResult::Ok(())),
                _ => panic!("unexpected state on balance {:?}", state),
            }
        }
    }
    /// Balance a non root page by trying to balance cells between a maximum of 3 siblings that should be neighboring the page that overflowed/underflowed.
    fn balance_non_root(&mut self) -> Result<CursorResult<()>> {
        assert!(
            matches!(self.state, CursorState::Write(_)),
            "Cursor must be in balancing state"
        );
        let state = self.state.write_info().expect("must be balancing").state;
        tracing::debug!("balance_non_root(state={:?})", state);
        let (next_write_state, result) = match state {
            WriteState::Start => todo!(),
            WriteState::BalanceStart => todo!(),
            WriteState::BalanceNonRoot => {
                let parent_page = self.stack.top();
                return_if_locked_maybe_load!(self.pager, parent_page);
                let parent_page = parent_page.get();
                if self.stack.current_cell_index() as usize
                    == parent_page.get_contents().cell_count() + 1
                {
                    self.stack.retreat();
                } else if self.stack.current_cell_index() == -1 {
                    self.stack.advance();
                }
                parent_page.set_dirty();
                self.pager.add_dirty(parent_page.get().id);
                let parent_contents = parent_page.get().contents.as_ref().unwrap();
                let page_to_balance_idx = self.stack.current_cell_index() as usize;
                tracing::debug!(
                    "balance_non_root(parent_id={} page_to_balance_idx={})", parent_page
                    .get().id, page_to_balance_idx
                );
                assert!(
                    matches!(parent_contents.page_type(), PageType::IndexInterior |
                    PageType::TableInterior)
                );
                let mut pages_to_balance: [Option<BTreePage>; 3] = [const { None }; 3];
                let number_of_cells_in_parent = parent_contents.cell_count()
                    + parent_contents.overflow_cells.len();
                assert!(
                    parent_contents.overflow_cells.is_empty(),
                    "balancing child page with overflowed parent not yet implemented"
                );
                assert!(
                    page_to_balance_idx <= parent_contents.cell_count(),
                    "page_to_balance_idx={} is out of bounds for parent cell count {}",
                    page_to_balance_idx, number_of_cells_in_parent
                );
                let (sibling_pointer, first_cell_divider) = match number_of_cells_in_parent {
                    n if n < 2 => (number_of_cells_in_parent, 0),
                    2 => (2, 0),
                    _ => {
                        let next_divider = if page_to_balance_idx == 0 {
                            0
                        } else if page_to_balance_idx == number_of_cells_in_parent {
                            number_of_cells_in_parent - 2
                        } else {
                            page_to_balance_idx - 1
                        };
                        (2, next_divider)
                    }
                };
                let sibling_count = sibling_pointer + 1;
                let last_sibling_is_right_pointer = sibling_pointer + first_cell_divider
                    - parent_contents.overflow_cells.len()
                    == parent_contents.cell_count();
                let right_pointer = if last_sibling_is_right_pointer {
                    parent_contents.rightmost_pointer_raw().unwrap()
                } else {
                    let (start_of_cell, _) = parent_contents
                        .cell_get_raw_region(
                            first_cell_divider + sibling_pointer,
                            payload_overflow_threshold_max(
                                parent_contents.page_type(),
                                self.usable_space() as u16,
                            ),
                            payload_overflow_threshold_min(
                                parent_contents.page_type(),
                                self.usable_space() as u16,
                            ),
                            self.usable_space(),
                        );
                    let buf = parent_contents.as_ptr().as_mut_ptr();
                    unsafe { buf.add(start_of_cell) }
                };
                let mut pgno: u32 = unsafe {
                    right_pointer.cast::<u32>().read().swap_bytes()
                };
                let current_sibling = sibling_pointer;
                for i in (0..=current_sibling).rev() {
                    let page = self.read_page(pgno as usize)?;
                    {
                        let sibling_page = page.get();
                        sibling_page.set_dirty();
                        self.pager.add_dirty(sibling_page.get().id);
                    }
                    #[cfg(debug_assertions)]
                    {
                        return_if_locked!(page.get());
                        debug_validate_cells!(
                            & page.get().get_contents(), self.usable_space() as u16
                        );
                    }
                    pages_to_balance[i].replace(page);
                    assert_eq!(
                        parent_contents.overflow_cells.len(), 0,
                        "overflow in parent is not yet implented while balancing it"
                    );
                    if i == 0 {
                        break;
                    }
                    let next_cell_divider = i + first_cell_divider - 1;
                    pgno = match parent_contents
                        .cell_get(
                            next_cell_divider,
                            payload_overflow_threshold_max(
                                parent_contents.page_type(),
                                self.usable_space() as u16,
                            ),
                            payload_overflow_threshold_min(
                                parent_contents.page_type(),
                                self.usable_space() as u16,
                            ),
                            self.usable_space(),
                        )?
                    {
                        BTreeCell::TableInteriorCell(table_interior_cell) => {
                            table_interior_cell._left_child_page
                        }
                        BTreeCell::IndexInteriorCell(index_interior_cell) => {
                            index_interior_cell.left_child_page
                        }
                        BTreeCell::TableLeafCell(..) | BTreeCell::IndexLeafCell(..) => {
                            unreachable!()
                        }
                    };
                }
                #[cfg(debug_assertions)]
                {
                    let page_type_of_siblings = pages_to_balance[0]
                        .as_ref()
                        .unwrap()
                        .get()
                        .get_contents()
                        .page_type();
                    for page in pages_to_balance.iter().take(sibling_count) {
                        return_if_locked_maybe_load!(self.pager, page.as_ref().unwrap());
                        let page = page.as_ref().unwrap().get();
                        let contents = page.get_contents();
                        debug_validate_cells!(& contents, self.usable_space() as u16);
                        assert_eq!(contents.page_type(), page_type_of_siblings);
                    }
                }
                self.state
                    .write_info()
                    .unwrap()
                    .balance_info
                    .replace(
                        Some(BalanceInfo {
                            pages_to_balance,
                            rightmost_pointer: right_pointer,
                            divider_cells: [const { None }; 2],
                            sibling_count,
                            first_divider_cell: first_cell_divider,
                        }),
                    );
                (WriteState::BalanceNonRootWaitLoadPages, Ok(CursorResult::IO))
            }
            WriteState::BalanceNonRootWaitLoadPages => {
                let write_info = self.state.write_info().unwrap();
                let mut balance_info = write_info.balance_info.borrow_mut();
                let balance_info = balance_info.as_mut().unwrap();
                for page in balance_info
                    .pages_to_balance
                    .iter()
                    .take(balance_info.sibling_count)
                {
                    let page = page.as_ref().unwrap();
                    return_if_locked_maybe_load!(self.pager, page);
                }
                let parent_page_btree = self.stack.top();
                let parent_page = parent_page_btree.get();
                let parent_contents = parent_page.get_contents();
                let parent_is_root = !self.stack.has_parent();
                assert!(
                    parent_contents.overflow_cells.is_empty(),
                    "overflow parent not yet implemented"
                );
                let mut max_cells = 0;
                let mut pages_to_balance_new: [Option<BTreePage>; 5] = [const {
                    None
                }; 5];
                for i in (0..balance_info.sibling_count).rev() {
                    let sibling_page = balance_info
                        .pages_to_balance[i]
                        .as_ref()
                        .unwrap();
                    let sibling_page = sibling_page.get();
                    assert!(sibling_page.is_loaded());
                    let sibling_contents = sibling_page.get_contents();
                    max_cells += sibling_contents.cell_count();
                    max_cells += sibling_contents.overflow_cells.len();
                    if i == balance_info.sibling_count - 1 {
                        continue;
                    }
                    let cell_idx = balance_info.first_divider_cell + i;
                    let (cell_start, cell_len) = parent_contents
                        .cell_get_raw_region(
                            cell_idx,
                            payload_overflow_threshold_max(
                                parent_contents.page_type(),
                                self.usable_space() as u16,
                            ),
                            payload_overflow_threshold_min(
                                parent_contents.page_type(),
                                self.usable_space() as u16,
                            ),
                            self.usable_space(),
                        );
                    let buf = parent_contents.as_ptr();
                    let cell_buf = &buf[cell_start..cell_start + cell_len];
                    max_cells += 1;
                    tracing::debug!(
                        "balance_non_root(drop_divider_cell, first_divider_cell={}, divider_cell={}, left_pointer={})",
                        balance_info.first_divider_cell, i, read_u32(cell_buf, 0)
                    );
                    balance_info.divider_cells[i].replace(cell_buf.to_vec());
                    tracing::trace!(
                        "dropping divider cell from parent cell_idx={} count={}",
                        cell_idx, parent_contents.cell_count()
                    );
                    drop_cell(parent_contents, cell_idx, self.usable_space() as u16)?;
                }
                let mut cell_array = CellArray {
                    cells: Vec::with_capacity(max_cells),
                    number_of_cells_per_page: [0; 5],
                };
                let cells_capacity_start = cell_array.cells.capacity();
                let mut total_cells_inserted = 0;
                let mut count_cells_in_old_pages: [u16; 5] = [0; 5];
                let page_type = balance_info
                    .pages_to_balance[0]
                    .as_ref()
                    .unwrap()
                    .get()
                    .get_contents()
                    .page_type();
                tracing::debug!("balance_non_root(page_type={:?})", page_type);
                let leaf_data = matches!(page_type, PageType::TableLeaf);
                let leaf = matches!(
                    page_type, PageType::TableLeaf | PageType::IndexLeaf
                );
                for (i, old_page) in balance_info
                    .pages_to_balance
                    .iter()
                    .take(balance_info.sibling_count)
                    .enumerate()
                {
                    let old_page = old_page.as_ref().unwrap().get();
                    let old_page_contents = old_page.get_contents();
                    debug_validate_cells!(
                        & old_page_contents, self.usable_space() as u16
                    );
                    for cell_idx in 0..old_page_contents.cell_count() {
                        let (cell_start, cell_len) = old_page_contents
                            .cell_get_raw_region(
                                cell_idx,
                                payload_overflow_threshold_max(
                                    old_page_contents.page_type(),
                                    self.usable_space() as u16,
                                ),
                                payload_overflow_threshold_min(
                                    old_page_contents.page_type(),
                                    self.usable_space() as u16,
                                ),
                                self.usable_space(),
                            );
                        let buf = old_page_contents.as_ptr();
                        let cell_buf = &mut buf[cell_start..cell_start + cell_len];
                        cell_array.cells.push(to_static_buf(cell_buf));
                    }
                    let offset = total_cells_inserted;
                    for overflow_cell in old_page_contents.overflow_cells.iter_mut() {
                        cell_array
                            .cells
                            .insert(
                                offset + overflow_cell.index,
                                to_static_buf(&mut Pin::as_mut(&mut overflow_cell.payload)),
                            );
                    }
                    count_cells_in_old_pages[i] = cell_array.cells.len() as u16;
                    let mut cells_inserted = old_page_contents.cell_count()
                        + old_page_contents.overflow_cells.len();
                    if i < balance_info.sibling_count - 1 && !leaf_data {
                        let mut divider_cell = balance_info
                            .divider_cells[i]
                            .as_mut()
                            .unwrap()
                            .as_mut_slice();
                        cells_inserted += 1;
                        if !leaf {
                            let right_pointer = old_page_contents
                                .rightmost_pointer()
                                .unwrap();
                            divider_cell[..4]
                                .copy_from_slice(&right_pointer.to_be_bytes());
                        } else {
                            assert!(divider_cell.len() >= 4);
                            divider_cell = &mut divider_cell[4..];
                        }
                        cell_array.cells.push(to_static_buf(divider_cell));
                    }
                    total_cells_inserted += cells_inserted;
                }
                assert_eq!(
                    cell_array.cells.capacity(), cells_capacity_start,
                    "calculation of max cells was wrong"
                );
                #[cfg(debug_assertions)]
                let mut cells_debug = Vec::new();
                #[cfg(debug_assertions)]
                {
                    for cell in &cell_array.cells {
                        cells_debug.push(cell.to_vec());
                        if leaf {
                            assert!(cell[0] != 0)
                        }
                    }
                }
                #[cfg(debug_assertions)]
                validate_cells_after_insertion(&cell_array, leaf_data);
                let mut new_page_sizes: [i64; 5] = [0; 5];
                let leaf_correction = if leaf { 4 } else { 0 };
                let usable_space = self.usable_space() - 12 + leaf_correction;
                for i in 0..balance_info.sibling_count {
                    cell_array.number_of_cells_per_page[i] = count_cells_in_old_pages[i];
                    let page = &balance_info.pages_to_balance[i].as_ref().unwrap();
                    let page = page.get();
                    let page_contents = page.get_contents();
                    let free_space = compute_free_space(
                        page_contents,
                        self.usable_space() as u16,
                    );
                    new_page_sizes[i] = usable_space as i64 - free_space as i64;
                    for overflow in &page_contents.overflow_cells {
                        new_page_sizes[i] += 2 + overflow.payload.len() as i64;
                    }
                    if !leaf && i < balance_info.sibling_count - 1 {
                        new_page_sizes[i]
                            += cell_array.cells[cell_array.cell_count(i)].len() as i64;
                    }
                }
                let mut sibling_count_new = balance_info.sibling_count;
                let mut i = 0;
                while i < sibling_count_new {
                    while new_page_sizes[i] > usable_space as i64 {
                        let needs_new_page = i + 1 >= sibling_count_new;
                        if needs_new_page {
                            sibling_count_new = i + 2;
                            assert!(
                                sibling_count_new <= 5,
                                "it is corrupt to require more than 5 pages to balance 3 siblings"
                            );
                            new_page_sizes[sibling_count_new - 1] = 0;
                            cell_array.number_of_cells_per_page[sibling_count_new - 1] = cell_array
                                .cells
                                .len() as u16;
                        }
                        let size_of_cell_to_remove_from_left = 2
                            + cell_array.cells[cell_array.cell_count(i) - 1].len()
                                as i64;
                        new_page_sizes[i] -= size_of_cell_to_remove_from_left;
                        let size_of_cell_to_move_right = if !leaf_data {
                            if cell_array.number_of_cells_per_page[i]
                                < cell_array.cells.len() as u16
                            {
                                2 + cell_array.cells[cell_array.cell_count(i)].len() as i64
                            } else {
                                0
                            }
                        } else {
                            size_of_cell_to_remove_from_left
                        };
                        new_page_sizes[i + 1] += size_of_cell_to_move_right as i64;
                        cell_array.number_of_cells_per_page[i] -= 1;
                    }
                    while cell_array.number_of_cells_per_page[i]
                        < cell_array.cells.len() as u16
                    {
                        let size_of_cell_to_remove_from_right = 2
                            + cell_array.cells[cell_array.cell_count(i)].len() as i64;
                        let can_take = new_page_sizes[i]
                            + size_of_cell_to_remove_from_right > usable_space as i64;
                        if can_take {
                            break;
                        }
                        new_page_sizes[i] += size_of_cell_to_remove_from_right;
                        cell_array.number_of_cells_per_page[i] += 1;
                        let size_of_cell_to_remove_from_right = if !leaf_data {
                            if cell_array.number_of_cells_per_page[i]
                                < cell_array.cells.len() as u16
                            {
                                2 + cell_array.cells[cell_array.cell_count(i)].len() as i64
                            } else {
                                0
                            }
                        } else {
                            size_of_cell_to_remove_from_right
                        };
                        new_page_sizes[i + 1] -= size_of_cell_to_remove_from_right;
                    }
                    let page_completes_all_cells = cell_array.number_of_cells_per_page[i]
                        >= cell_array.cells.len() as u16;
                    if page_completes_all_cells {
                        sibling_count_new = i + 1;
                        break;
                    }
                    i += 1;
                    if i >= sibling_count_new {
                        break;
                    }
                }
                tracing::debug!(
                    "balance_non_root(sibling_count={}, sibling_count_new={}, cells={})",
                    balance_info.sibling_count, sibling_count_new, cell_array.cells.len()
                );
                for i in (1..sibling_count_new).rev() {
                    let mut size_right_page = new_page_sizes[i];
                    let mut size_left_page = new_page_sizes[i - 1];
                    let mut cell_left = cell_array.number_of_cells_per_page[i - 1] - 1;
                    let mut cell_right = cell_left + 1 - leaf_data as u16;
                    loop {
                        let cell_left_size = cell_array.cell_size(cell_left as usize)
                            as i64;
                        let cell_right_size = cell_array.cell_size(cell_right as usize)
                            as i64;
                        let pointer_size = if i == sibling_count_new - 1 {
                            0
                        } else {
                            2
                        };
                        let would_not_improve_balance = size_right_page + cell_right_size
                            + 2 > size_left_page - (cell_left_size + pointer_size);
                        if size_right_page != 0 && would_not_improve_balance {
                            break;
                        }
                        size_left_page -= cell_left_size + 2;
                        size_right_page += cell_right_size + 2;
                        cell_array.number_of_cells_per_page[i - 1] = cell_left;
                        if cell_left == 0 {
                            break;
                        }
                        cell_left -= 1;
                        cell_right -= 1;
                    }
                    new_page_sizes[i] = size_right_page;
                    new_page_sizes[i - 1] = size_left_page;
                    assert!(
                        cell_array.number_of_cells_per_page[i - 1] > if i > 1 {
                        cell_array.number_of_cells_per_page[i - 2] } else { 0 }
                    );
                }
                for i in 0..sibling_count_new {
                    if i < balance_info.sibling_count {
                        let page = balance_info.pages_to_balance[i].as_ref().unwrap();
                        page.get().set_dirty();
                        pages_to_balance_new[i].replace(page.clone());
                    } else {
                        let page = self.allocate_page(page_type, 0);
                        pages_to_balance_new[i].replace(page);
                        count_cells_in_old_pages[i] = cell_array.cells.len() as u16;
                    }
                }
                {
                    let mut page_numbers: [usize; 5] = [0; 5];
                    for (i, page) in pages_to_balance_new
                        .iter()
                        .take(sibling_count_new)
                        .enumerate()
                    {
                        page_numbers[i] = page.as_ref().unwrap().get().get().id;
                    }
                    page_numbers.sort();
                    for (page, new_id) in pages_to_balance_new
                        .iter()
                        .take(sibling_count_new)
                        .rev()
                        .zip(page_numbers.iter().rev().take(sibling_count_new))
                    {
                        let page = page.as_ref().unwrap();
                        if *new_id != page.get().get().id {
                            page.get().get().id = *new_id;
                            self.pager
                                .update_dirty_loaded_page_in_cache(*new_id, page.get())?;
                        }
                    }
                    #[cfg(debug_assertions)]
                    {
                        tracing::debug!(
                            "balance_non_root(parent page_id={})", parent_page.get().id
                        );
                        for page in pages_to_balance_new.iter().take(sibling_count_new) {
                            tracing::debug!(
                                "balance_non_root(new_sibling page_id={})", page.as_ref()
                                .unwrap().get().get().id
                            );
                        }
                    }
                }
                #[cfg(debug_assertions)]
                let mut pages_pointed_to = HashSet::new();
                let right_page_id = pages_to_balance_new[sibling_count_new - 1]
                    .as_ref()
                    .unwrap()
                    .get()
                    .get()
                    .id as u32;
                let rightmost_pointer = balance_info.rightmost_pointer;
                let rightmost_pointer = unsafe {
                    std::slice::from_raw_parts_mut(rightmost_pointer, 4)
                };
                rightmost_pointer[0..4].copy_from_slice(&right_page_id.to_be_bytes());
                #[cfg(debug_assertions)] pages_pointed_to.insert(right_page_id);
                tracing::debug!(
                    "balance_non_root(rightmost_pointer_update, rightmost_pointer={})",
                    right_page_id
                );
                let is_leaf_page = matches!(
                    page_type, PageType::TableLeaf | PageType::IndexLeaf
                );
                if !is_leaf_page {
                    let last_page = balance_info
                        .pages_to_balance[balance_info.sibling_count - 1]
                        .as_ref()
                        .unwrap();
                    let right_pointer = last_page
                        .get()
                        .get_contents()
                        .rightmost_pointer()
                        .unwrap();
                    let new_last_page = pages_to_balance_new[sibling_count_new - 1]
                        .as_ref()
                        .unwrap();
                    new_last_page
                        .get()
                        .get_contents()
                        .write_u32(offset::BTREE_RIGHTMOST_PTR, right_pointer);
                }
                for (i, page) in pages_to_balance_new
                    .iter()
                    .enumerate()
                    .take(sibling_count_new - 1)
                {
                    let page = page.as_ref().unwrap();
                    let divider_cell_idx = cell_array.cell_count(i);
                    let mut divider_cell = &mut cell_array.cells[divider_cell_idx];
                    let mut new_divider_cell = Vec::new();
                    if !is_leaf_page {
                        let previous_pointer_divider = read_u32(&divider_cell, 0);
                        page.get()
                            .get_contents()
                            .write_u32(
                                offset::BTREE_RIGHTMOST_PTR,
                                previous_pointer_divider,
                            );
                        new_divider_cell
                            .extend_from_slice(
                                &(page.get().get().id as u32).to_be_bytes(),
                            );
                        new_divider_cell.extend_from_slice(&divider_cell[4..]);
                    } else if leaf_data {
                        divider_cell = &mut cell_array.cells[divider_cell_idx - 1];
                        let (_, n_bytes_payload) = read_varint(divider_cell)?;
                        let (rowid, _) = read_varint(&divider_cell[n_bytes_payload..])?;
                        new_divider_cell
                            .extend_from_slice(
                                &(page.get().get().id as u32).to_be_bytes(),
                            );
                        write_varint_to_vec(rowid as u64, &mut new_divider_cell);
                    } else {
                        new_divider_cell
                            .extend_from_slice(
                                &(page.get().get().id as u32).to_be_bytes(),
                            );
                        new_divider_cell.extend_from_slice(divider_cell);
                    }
                    let left_pointer = read_u32(&new_divider_cell[..4], 0);
                    assert!(left_pointer != parent_page.get().id as u32);
                    #[cfg(debug_assertions)] pages_pointed_to.insert(left_pointer);
                    tracing::debug!(
                        "balance_non_root(insert_divider_cell, first_divider_cell={}, divider_cell={}, left_pointer={})",
                        balance_info.first_divider_cell, i, left_pointer
                    );
                    assert_eq!(left_pointer, page.get().get().id as u32);
                    assert!(
                        left_pointer <= self.pager.db_header.lock().database_size,
                        "invalid page number divider left pointer {} > database number of pages",
                        left_pointer,
                    );
                    insert_into_cell(
                            parent_contents,
                            &new_divider_cell,
                            balance_info.first_divider_cell + i,
                            self.usable_space() as u16,
                        )
                        .unwrap();
                    #[cfg(debug_assertions)]
                    self.validate_balance_non_root_divider_cell_insertion(
                        balance_info,
                        parent_contents,
                        i,
                        &page.get(),
                    );
                }
                tracing::debug!(
                    "balance_non_root(parent_overflow={})", parent_contents
                    .overflow_cells.len()
                );
                #[cfg(debug_assertions)]
                {
                    for page in pages_to_balance_new.iter().take(sibling_count_new) {
                        let page = page.as_ref().unwrap();
                        assert!(
                            pages_pointed_to.contains(& (page.get().get().id as u32)),
                            "page {} not pointed to by divider cell or rightmost pointer",
                            page.get().get().id
                        );
                    }
                }
                let mut done = [false; 5];
                for i in (1 - sibling_count_new as i64)..sibling_count_new as i64 {
                    let page_idx = i.unsigned_abs() as usize;
                    if done[page_idx] {
                        continue;
                    }
                    if i >= 0
                        || count_cells_in_old_pages[page_idx - 1]
                            >= cell_array.number_of_cells_per_page[page_idx - 1]
                    {
                        let (start_old_cells, start_new_cells, number_new_cells) = if page_idx
                            == 0
                        {
                            (0, 0, cell_array.cell_count(0))
                        } else {
                            let this_was_old_page = page_idx
                                < balance_info.sibling_count;
                            let start_old_cells = if this_was_old_page {
                                count_cells_in_old_pages[page_idx - 1] as usize
                                    + (!leaf_data) as usize
                            } else {
                                cell_array.cells.len()
                            };
                            let start_new_cells = cell_array.cell_count(page_idx - 1)
                                + (!leaf_data) as usize;
                            (
                                start_old_cells,
                                start_new_cells,
                                cell_array.cell_count(page_idx) - start_new_cells,
                            )
                        };
                        let page = pages_to_balance_new[page_idx].as_ref().unwrap();
                        let page = page.get();
                        tracing::debug!("pre_edit_page(page={})", page.get().id);
                        let page_contents = page.get_contents();
                        edit_page(
                            page_contents,
                            start_old_cells,
                            start_new_cells,
                            number_new_cells,
                            &cell_array,
                            self.usable_space() as u16,
                        )?;
                        debug_validate_cells!(page_contents, self.usable_space() as u16);
                        tracing::trace!(
                            "edit_page page={} cells={}", page.get().id, page_contents
                            .cell_count()
                        );
                        page_contents.overflow_cells.clear();
                        done[page_idx] = true;
                    }
                }
                let first_child_page = pages_to_balance_new[0].as_ref().unwrap();
                let first_child_page = first_child_page.get();
                let first_child_contents = first_child_page.get_contents();
                if parent_is_root && parent_contents.cell_count() == 0
                    && parent_contents.offset
                        <= compute_free_space(
                            first_child_contents,
                            self.usable_space() as u16,
                        ) as usize
                {
                    assert!(sibling_count_new == 1);
                    let parent_offset = if parent_page.get().id == 1 {
                        DATABASE_HEADER_SIZE
                    } else {
                        0
                    };
                    defragment_page(first_child_contents, self.usable_space() as u16);
                    let child_top = first_child_contents.cell_content_area() as usize;
                    let parent_buf = parent_contents.as_ptr();
                    let child_buf = first_child_contents.as_ptr();
                    let content_size = self.usable_space() - child_top;
                    parent_buf[child_top..child_top + content_size]
                        .copy_from_slice(
                            &child_buf[child_top..child_top + content_size],
                        );
                    let header_and_pointer_size = first_child_contents.header_size()
                        + first_child_contents.cell_pointer_array_size();
                    parent_buf[parent_offset..parent_offset + header_and_pointer_size]
                        .copy_from_slice(
                            &child_buf[first_child_contents
                                .offset..first_child_contents.offset
                                + header_and_pointer_size],
                        );
                    self.stack.set_cell_index(0);
                    sibling_count_new -= 1;
                    assert!(sibling_count_new < balance_info.sibling_count);
                }
                #[cfg(debug_assertions)]
                self.post_balance_non_root_validation(
                    &parent_page_btree,
                    balance_info,
                    parent_contents,
                    pages_to_balance_new,
                    page_type,
                    leaf_data,
                    cells_debug,
                    sibling_count_new,
                    rightmost_pointer,
                );
                for i in sibling_count_new..balance_info.sibling_count {
                    let page = balance_info.pages_to_balance[i].as_ref().unwrap();
                    self.pager.free_page(Some(page.get().clone()), page.get().get().id)?;
                }
                (WriteState::BalanceStart, Ok(CursorResult::Ok(())))
            }
            WriteState::Finish => todo!(),
        };
        if matches!(next_write_state, WriteState::BalanceStart) {
            let _ = self.state.mut_write_info().unwrap().balance_info.take();
        }
        let write_info = self.state.mut_write_info().unwrap();
        write_info.state = next_write_state;
        result
    }
    #[cfg(debug_assertions)]
    fn validate_balance_non_root_divider_cell_insertion(
        &self,
        balance_info: &mut BalanceInfo,
        parent_contents: &mut PageContent,
        i: usize,
        page: &std::sync::Arc<crate::Page>,
    ) {
        let left_pointer = if parent_contents.overflow_cells.len() == 0 {
            let (cell_start, cell_len) = parent_contents
                .cell_get_raw_region(
                    balance_info.first_divider_cell + i,
                    payload_overflow_threshold_max(
                        parent_contents.page_type(),
                        self.usable_space() as u16,
                    ),
                    payload_overflow_threshold_min(
                        parent_contents.page_type(),
                        self.usable_space() as u16,
                    ),
                    self.usable_space(),
                );
            tracing::debug!(
                "balance_non_root(cell_start={}, cell_len={})", cell_start, cell_len
            );
            let left_pointer = read_u32(
                &parent_contents.as_ptr()[cell_start..cell_start + cell_len],
                0,
            );
            left_pointer
        } else {
            let mut left_pointer = None;
            for cell in parent_contents.overflow_cells.iter() {
                if cell.index == balance_info.first_divider_cell + i {
                    left_pointer = Some(read_u32(&cell.payload, 0));
                }
            }
            left_pointer.expect("overflow cell with divider cell was not found")
        };
        assert_eq!(
            left_pointer, page.get().id as u32,
            "the cell we just inserted doesn't point to the correct page. points to {}, should point to {}",
            left_pointer, page.get().id as u32
        );
    }
    #[cfg(debug_assertions)]
    fn post_balance_non_root_validation(
        &self,
        parent_page: &BTreePage,
        balance_info: &mut BalanceInfo,
        parent_contents: &mut PageContent,
        pages_to_balance_new: [Option<BTreePage>; 5],
        page_type: PageType,
        leaf_data: bool,
        mut cells_debug: Vec<Vec<u8>>,
        sibling_count_new: usize,
        rightmost_pointer: &mut [u8],
    ) {
        let mut valid = true;
        let mut current_index_cell = 0;
        for cell_idx in 0..parent_contents.cell_count() {
            let cell = parent_contents
                .cell_get(
                    cell_idx,
                    payload_overflow_threshold_max(
                        parent_contents.page_type(),
                        self.usable_space() as u16,
                    ),
                    payload_overflow_threshold_min(
                        parent_contents.page_type(),
                        self.usable_space() as u16,
                    ),
                    self.usable_space(),
                )
                .unwrap();
            match cell {
                BTreeCell::TableInteriorCell(table_interior_cell) => {
                    let left_child_page = table_interior_cell._left_child_page;
                    if left_child_page == parent_page.get().get().id as u32 {
                        tracing::error!(
                            "balance_non_root(parent_divider_points_to_same_page, page_id={}, cell_left_child_page={})",
                            parent_page.get().get().id, left_child_page,
                        );
                        valid = false;
                    }
                }
                BTreeCell::IndexInteriorCell(index_interior_cell) => {
                    let left_child_page = index_interior_cell.left_child_page;
                    if left_child_page == parent_page.get().get().id as u32 {
                        tracing::error!(
                            "balance_non_root(parent_divider_points_to_same_page, page_id={}, cell_left_child_page={})",
                            parent_page.get().get().id, left_child_page,
                        );
                        valid = false;
                    }
                }
                _ => {}
            }
        }
        for (page_idx, page) in pages_to_balance_new
            .iter()
            .take(sibling_count_new)
            .enumerate()
        {
            let page = page.as_ref().unwrap();
            let page = page.get();
            let contents = page.get_contents();
            debug_validate_cells!(contents, self.usable_space() as u16);
            for cell_idx in 0..contents.cell_count() {
                let (cell_start, cell_len) = contents
                    .cell_get_raw_region(
                        cell_idx,
                        payload_overflow_threshold_max(
                            contents.page_type(),
                            self.usable_space() as u16,
                        ),
                        payload_overflow_threshold_min(
                            contents.page_type(),
                            self.usable_space() as u16,
                        ),
                        self.usable_space(),
                    );
                let buf = contents.as_ptr();
                let cell_buf = to_static_buf(
                    &mut buf[cell_start..cell_start + cell_len],
                );
                let cell_buf_in_array = &cells_debug[current_index_cell];
                if cell_buf != cell_buf_in_array {
                    tracing::error!(
                        "balance_non_root(cell_not_found_debug, page_id={}, cell_in_cell_array_idx={})",
                        page.get().id, current_index_cell,
                    );
                    valid = false;
                }
                let cell = crate::storage::sqlite3_ondisk::read_btree_cell(
                        cell_buf,
                        &page_type,
                        0,
                        payload_overflow_threshold_max(
                            parent_contents.page_type(),
                            self.usable_space() as u16,
                        ),
                        payload_overflow_threshold_min(
                            parent_contents.page_type(),
                            self.usable_space() as u16,
                        ),
                        self.usable_space(),
                    )
                    .unwrap();
                match &cell {
                    BTreeCell::TableInteriorCell(table_interior_cell) => {
                        let left_child_page = table_interior_cell._left_child_page;
                        if left_child_page == page.get().id as u32 {
                            tracing::error!(
                                "balance_non_root(child_page_points_same_page, page_id={}, cell_left_child_page={}, page_idx={})",
                                page.get().id, left_child_page, page_idx
                            );
                            valid = false;
                        }
                        if left_child_page == parent_page.get().get().id as u32 {
                            tracing::error!(
                                "balance_non_root(child_page_points_parent_of_child, page_id={}, cell_left_child_page={}, page_idx={})",
                                page.get().id, left_child_page, page_idx
                            );
                            valid = false;
                        }
                    }
                    BTreeCell::IndexInteriorCell(index_interior_cell) => {
                        let left_child_page = index_interior_cell.left_child_page;
                        if left_child_page == page.get().id as u32 {
                            tracing::error!(
                                "balance_non_root(child_page_points_same_page, page_id={}, cell_left_child_page={}, page_idx={})",
                                page.get().id, left_child_page, page_idx
                            );
                            valid = false;
                        }
                        if left_child_page == parent_page.get().get().id as u32 {
                            tracing::error!(
                                "balance_non_root(child_page_points_parent_of_child, page_id={}, cell_left_child_page={}, page_idx={})",
                                page.get().id, left_child_page, page_idx
                            );
                            valid = false;
                        }
                    }
                    _ => {}
                }
                current_index_cell += 1;
            }
            let parent_buf = parent_contents.as_ptr();
            let cell_divider_idx = balance_info.first_divider_cell + page_idx;
            if sibling_count_new == 0 {
                let rightmost = read_u32(rightmost_pointer, 0);
                debug_validate_cells!(parent_contents, self.usable_space() as u16);
                if !pages_to_balance_new[0].is_some() {
                    tracing::error!(
                        "balance_non_root(balance_shallower_incorrect_page, page_idx={})",
                        0
                    );
                    valid = false;
                }
                for i in 1..sibling_count_new {
                    if pages_to_balance_new[i].is_some() {
                        tracing::error!(
                            "balance_non_root(balance_shallower_incorrect_page, page_idx={})",
                            i
                        );
                        valid = false;
                    }
                }
                if current_index_cell != cells_debug.len()
                    || cells_debug.len() != contents.cell_count()
                    || contents.cell_count() != parent_contents.cell_count()
                {
                    tracing::error!(
                        "balance_non_root(balance_shallower_incorrect_cell_count, current_index_cell={}, cells_debug={}, cell_count={}, parent_cell_count={})",
                        current_index_cell, cells_debug.len(), contents.cell_count(),
                        parent_contents.cell_count()
                    );
                    valid = false;
                }
                if rightmost == page.get().id as u32
                    || rightmost == parent_page.get().get().id as u32
                {
                    tracing::error!(
                        "balance_non_root(balance_shallower_rightmost_pointer, page_id={}, parent_page_id={}, rightmost={})",
                        page.get().id, parent_page.get().get().id, rightmost,
                    );
                    valid = false;
                }
                if let Some(rm) = contents.rightmost_pointer() {
                    if rm != rightmost {
                        tracing::error!(
                            "balance_non_root(balance_shallower_rightmost_pointer, page_rightmost={}, rightmost={})",
                            rm, rightmost,
                        );
                        valid = false;
                    }
                }
                if let Some(rm) = parent_contents.rightmost_pointer() {
                    if rm != rightmost {
                        tracing::error!(
                            "balance_non_root(balance_shallower_rightmost_pointer, parent_rightmost={}, rightmost={})",
                            rm, rightmost,
                        );
                        valid = false;
                    }
                }
                if parent_contents.page_type() != page_type {
                    tracing::error!(
                        "balance_non_root(balance_shallower_parent_page_type, page_type={:?}, parent_page_type={:?})",
                        page_type, parent_contents.page_type()
                    );
                    valid = false;
                }
                for parent_cell_idx in 0..contents.cell_count() {
                    let (parent_cell_start, parent_cell_len) = parent_contents
                        .cell_get_raw_region(
                            parent_cell_idx,
                            payload_overflow_threshold_max(
                                parent_contents.page_type(),
                                self.usable_space() as u16,
                            ),
                            payload_overflow_threshold_min(
                                parent_contents.page_type(),
                                self.usable_space() as u16,
                            ),
                            self.usable_space(),
                        );
                    let (cell_start, cell_len) = contents
                        .cell_get_raw_region(
                            parent_cell_idx,
                            payload_overflow_threshold_max(
                                contents.page_type(),
                                self.usable_space() as u16,
                            ),
                            payload_overflow_threshold_min(
                                contents.page_type(),
                                self.usable_space() as u16,
                            ),
                            self.usable_space(),
                        );
                    let buf = contents.as_ptr();
                    let cell_buf = to_static_buf(
                        &mut buf[cell_start..cell_start + cell_len],
                    );
                    let parent_cell_buf = to_static_buf(
                        &mut parent_buf[parent_cell_start..parent_cell_start
                            + parent_cell_len],
                    );
                    let cell_buf_in_array = &cells_debug[parent_cell_idx];
                    if cell_buf != cell_buf_in_array || cell_buf != parent_cell_buf {
                        tracing::error!(
                            "balance_non_root(balance_shallower_cell_not_found_debug, page_id={}, cell_in_cell_array_idx={})",
                            page.get().id, parent_cell_idx,
                        );
                        valid = false;
                    }
                }
            } else if page_idx == sibling_count_new - 1 {
                if cell_divider_idx == parent_contents.cell_count() {
                    let rightmost = read_u32(rightmost_pointer, 0);
                    if rightmost != page.get().id as u32 {
                        tracing::error!(
                            "balance_non_root(cell_divider_right_pointer, should point to {}, but points to {})",
                            page.get().id, rightmost
                        );
                        valid = false;
                    }
                }
            } else {
                let mut was_overflow = false;
                for overflow_cell in &parent_contents.overflow_cells {
                    if overflow_cell.index == cell_divider_idx {
                        let left_pointer = read_u32(&overflow_cell.payload, 0);
                        if left_pointer != page.get().id as u32 {
                            tracing::error!(
                                "balance_non_root(cell_divider_left_pointer_overflow, should point to page_id={}, but points to {}, divider_cell={}, overflow_cells_parent={})",
                                page.get().id, left_pointer, page_idx, parent_contents
                                .overflow_cells.len()
                            );
                            valid = false;
                        }
                        was_overflow = true;
                        break;
                    }
                }
                if was_overflow {
                    if !leaf_data {
                        current_index_cell += 1;
                    }
                    continue;
                }
                let (cell_start, cell_len) = parent_contents
                    .cell_get_raw_region(
                        cell_divider_idx,
                        payload_overflow_threshold_max(
                            parent_contents.page_type(),
                            self.usable_space() as u16,
                        ),
                        payload_overflow_threshold_min(
                            parent_contents.page_type(),
                            self.usable_space() as u16,
                        ),
                        self.usable_space(),
                    );
                let cell_left_pointer = read_u32(
                    &parent_buf[cell_start..cell_start + cell_len],
                    0,
                );
                if cell_left_pointer != page.get().id as u32 {
                    tracing::error!(
                        "balance_non_root(cell_divider_left_pointer, should point to page_id={}, but points to {}, divider_cell={}, overflow_cells_parent={})",
                        page.get().id, cell_left_pointer, page_idx, parent_contents
                        .overflow_cells.len()
                    );
                    valid = false;
                }
                if leaf_data {
                    if page_idx >= balance_info.sibling_count - 1 {
                        continue;
                    }
                    let cell_buf: &'static mut [u8] = to_static_buf(
                        &mut cells_debug[current_index_cell - 1],
                    );
                    let cell = crate::storage::sqlite3_ondisk::read_btree_cell(
                            cell_buf,
                            &page_type,
                            0,
                            payload_overflow_threshold_max(
                                parent_contents.page_type(),
                                self.usable_space() as u16,
                            ),
                            payload_overflow_threshold_min(
                                parent_contents.page_type(),
                                self.usable_space() as u16,
                            ),
                            self.usable_space(),
                        )
                        .unwrap();
                    let parent_cell = parent_contents
                        .cell_get(
                            cell_divider_idx,
                            payload_overflow_threshold_max(
                                parent_contents.page_type(),
                                self.usable_space() as u16,
                            ),
                            payload_overflow_threshold_min(
                                parent_contents.page_type(),
                                self.usable_space() as u16,
                            ),
                            self.usable_space(),
                        )
                        .unwrap();
                    let rowid = match cell {
                        BTreeCell::TableLeafCell(table_leaf_cell) => {
                            table_leaf_cell._rowid
                        }
                        _ => unreachable!(),
                    };
                    let rowid_parent = match parent_cell {
                        BTreeCell::TableInteriorCell(table_interior_cell) => {
                            table_interior_cell._rowid
                        }
                        _ => unreachable!(),
                    };
                    if rowid_parent != rowid {
                        tracing::error!(
                            "balance_non_root(cell_divider_rowid, page_id={}, cell_divider_idx={}, rowid_parent={}, rowid={})",
                            page.get().id, cell_divider_idx, rowid_parent, rowid
                        );
                        valid = false;
                    }
                } else {
                    let mut was_overflow = false;
                    for overflow_cell in &parent_contents.overflow_cells {
                        if overflow_cell.index == cell_divider_idx {
                            let left_pointer = read_u32(&overflow_cell.payload, 0);
                            if left_pointer != page.get().id as u32 {
                                tracing::error!(
                                    "balance_non_root(cell_divider_divider_cell_overflow should point to page_id={}, but points to {}, divider_cell={}, overflow_cells_parent={})",
                                    page.get().id, left_pointer, page_idx, parent_contents
                                    .overflow_cells.len()
                                );
                                valid = false;
                            }
                            was_overflow = true;
                            break;
                        }
                    }
                    if was_overflow {
                        if !leaf_data {
                            current_index_cell += 1;
                        }
                        continue;
                    }
                    let (parent_cell_start, parent_cell_len) = parent_contents
                        .cell_get_raw_region(
                            cell_divider_idx,
                            payload_overflow_threshold_max(
                                parent_contents.page_type(),
                                self.usable_space() as u16,
                            ),
                            payload_overflow_threshold_min(
                                parent_contents.page_type(),
                                self.usable_space() as u16,
                            ),
                            self.usable_space(),
                        );
                    let cell_buf_in_array = &cells_debug[current_index_cell];
                    let left_pointer = read_u32(
                        &parent_buf[parent_cell_start..parent_cell_start
                            + parent_cell_len],
                        0,
                    );
                    if left_pointer != page.get().id as u32 {
                        tracing::error!(
                            "balance_non_root(divider_cell_left_pointer_interior should point to page_id={}, but points to {}, divider_cell={}, overflow_cells_parent={})",
                            page.get().id, left_pointer, page_idx, parent_contents
                            .overflow_cells.len()
                        );
                        valid = false;
                    }
                    match page_type {
                        PageType::TableInterior | PageType::IndexInterior => {
                            let parent_cell_buf = &parent_buf[parent_cell_start..parent_cell_start
                                + parent_cell_len];
                            if parent_cell_buf[4..] != cell_buf_in_array[4..] {
                                tracing::error!(
                                    "balance_non_root(cell_divider_cell, page_id={}, cell_divider_idx={})",
                                    page.get().id, cell_divider_idx,
                                );
                                valid = false;
                            }
                        }
                        PageType::IndexLeaf => {
                            let parent_cell_buf = &parent_buf[parent_cell_start..parent_cell_start
                                + parent_cell_len];
                            if parent_cell_buf[4..] != cell_buf_in_array[..] {
                                tracing::error!(
                                    "balance_non_root(cell_divider_cell_index_leaf, page_id={}, cell_divider_idx={})",
                                    page.get().id, cell_divider_idx,
                                );
                                valid = false;
                            }
                        }
                        _ => unreachable!(),
                    }
                    current_index_cell += 1;
                }
            }
        }
        assert!(valid, "corrupted database, cells were to balanced properly");
    }
    /// Balance the root page.
    /// This is done when the root page overflows, and we need to create a new root page.
    /// See e.g. https://en.wikipedia.org/wiki/B-tree
    fn balance_root(&mut self) {
        let is_page_1 = {
            let current_root = self.stack.top();
            current_root.get().get().id == 1
        };
        let offset = if is_page_1 { DATABASE_HEADER_SIZE } else { 0 };
        let root_btree = self.stack.top();
        let root = root_btree.get();
        let root_contents = root.get_contents();
        let child_btree = self
            .pager
            .do_allocate_page(root_contents.page_type(), 0, BtreePageAllocMode::Any);
        tracing::debug!(
            "balance_root(root={}, rightmost={}, page_type={:?})", root.get().id,
            child_btree.get().get().id, root.get_contents().page_type()
        );
        self.pager.add_dirty(root.get().id);
        self.pager.add_dirty(child_btree.get().get().id);
        let root_buf = root_contents.as_ptr();
        let child = child_btree.get();
        let child_contents = child.get_contents();
        let child_buf = child_contents.as_ptr();
        let (root_pointer_start, root_pointer_len) = root_contents
            .cell_pointer_array_offset_and_size();
        let (child_pointer_start, _) = child
            .get_contents()
            .cell_pointer_array_offset_and_size();
        let top = root_contents.cell_content_area() as usize;
        child_buf[child_pointer_start..child_pointer_start + root_pointer_len]
            .copy_from_slice(
                &root_buf[root_pointer_start..root_pointer_start + root_pointer_len],
            );
        child_buf[top..].copy_from_slice(&root_buf[top..]);
        child_buf[0..root_contents.header_size()]
            .copy_from_slice(&root_buf[offset..offset + root_contents.header_size()]);
        std::mem::swap(
            &mut child_contents.overflow_cells,
            &mut root_contents.overflow_cells,
        );
        root_contents.overflow_cells.clear();
        let new_root_page_type = match root_contents.page_type() {
            PageType::IndexLeaf => PageType::IndexInterior,
            PageType::TableLeaf => PageType::TableInterior,
            other => other,
        } as u8;
        root_contents.write_u8(offset::BTREE_PAGE_TYPE, new_root_page_type);
        root_contents.write_u32(offset::BTREE_RIGHTMOST_PTR, child.get().id as u32);
        root_contents
            .write_u16(offset::BTREE_CELL_CONTENT_AREA, self.usable_space() as u16);
        root_contents.write_u16(offset::BTREE_CELL_COUNT, 0);
        root_contents.write_u16(offset::BTREE_FIRST_FREEBLOCK, 0);
        root_contents.write_u8(offset::BTREE_FRAGMENTED_BYTES_COUNT, 0);
        root_contents.overflow_cells.clear();
        self.root_page = root.get().id;
        self.stack.clear();
        self.stack.push(root_btree.clone());
        self.stack.set_cell_index(0);
        self.stack.push(child_btree.clone());
    }
    fn usable_space(&self) -> usize {
        self.pager.usable_space()
    }
    /// Find the index of the cell in the page that contains the given rowid.
    fn find_cell(
        &mut self,
        page: &PageContent,
        key: &BTreeKey,
    ) -> Result<CursorResult<usize>> {
        if self.find_cell_state.0.is_none() {
            self.find_cell_state.set(0);
        }
        let cell_count = page.cell_count();
        while self.find_cell_state.get_cell_idx() < cell_count as isize {
            assert!(self.find_cell_state.get_cell_idx() >= 0);
            let cell_idx = self.find_cell_state.get_cell_idx() as usize;
            match page
                .cell_get(
                    cell_idx,
                    payload_overflow_threshold_max(
                        page.page_type(),
                        self.usable_space() as u16,
                    ),
                    payload_overflow_threshold_min(
                        page.page_type(),
                        self.usable_space() as u16,
                    ),
                    self.usable_space(),
                )
                .unwrap()
            {
                BTreeCell::TableLeafCell(cell) => {
                    if key.to_rowid() <= cell._rowid {
                        break;
                    }
                }
                BTreeCell::TableInteriorCell(cell) => {
                    if key.to_rowid() <= cell._rowid {
                        break;
                    }
                }
                BTreeCell::IndexInteriorCell(
                    IndexInteriorCell { payload, first_overflow_page, payload_size, .. },
                )
                | BTreeCell::IndexLeafCell(
                    IndexLeafCell { payload, first_overflow_page, payload_size },
                ) => {
                    return_if_io!(
                        self.read_record_w_possible_overflow(payload,
                        first_overflow_page, payload_size,)
                    );
                    let key_values = key.to_index_key_values();
                    let record = self.get_immutable_record();
                    let record = record.as_ref().unwrap();
                    let record_same_number_cols = &record
                        .get_values()[..key_values.len()];
                    let order = compare_immutable(
                        key_values,
                        record_same_number_cols,
                        self.key_sort_order(),
                        &self.collations,
                    );
                    match order {
                        Ordering::Less | Ordering::Equal => {
                            break;
                        }
                        Ordering::Greater => {}
                    }
                }
            }
            let cell_idx = self.find_cell_state.get_cell_idx();
            self.find_cell_state.set(cell_idx + 1);
        }
        let cell_idx = self.find_cell_state.get_cell_idx();
        assert!(cell_idx >= 0);
        let cell_idx = cell_idx as usize;
        assert!(cell_idx <= cell_count);
        self.find_cell_state.reset();
        Ok(CursorResult::Ok(cell_idx))
    }
}
