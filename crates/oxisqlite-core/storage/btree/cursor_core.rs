impl BTreeCursor {
    pub fn new(
        mv_cursor: Option<Rc<RefCell<MvCursor>>>,
        pager: Rc<Pager>,
        root_page: usize,
        collations: Vec<CollationSeq>,
    ) -> Self {
        Self {
            mv_cursor,
            pager,
            root_page,
            has_record: Cell::new(false),
            null_flag: false,
            going_upwards: false,
            state: CursorState::None,
            overflow_state: None,
            stack: PageStack {
                current_page: Cell::new(-1),
                cell_indices: RefCell::new([0; BTCURSOR_MAX_DEPTH + 1]),
                stack: RefCell::new([const { None }; BTCURSOR_MAX_DEPTH + 1]),
            },
            reusable_immutable_record: RefCell::new(None),
            index_key_info: None,
            count: 0,
            context: None,
            valid_state: CursorValidState::Valid,
            collations,
            seek_state: CursorSeekState::Start,
            read_overflow_state: RefCell::new(None),
            find_cell_state: FindCellState(None),
            parse_record_state: RefCell::new(ParseRecordState::Init),
            analyze_walk: None,
        }
    }
    pub fn new_table(
        mv_cursor: Option<Rc<RefCell<MvCursor>>>,
        pager: Rc<Pager>,
        root_page: usize,
    ) -> Self {
        Self::new(mv_cursor, pager, root_page, Vec::new())
    }
    pub fn new_index(
        mv_cursor: Option<Rc<RefCell<MvCursor>>>,
        pager: Rc<Pager>,
        root_page: usize,
        index: &Index,
        collations: Vec<CollationSeq>,
    ) -> Self {
        let mut cursor = Self::new(mv_cursor, pager, root_page, collations);
        cursor.index_key_info = Some(IndexKeyInfo::new_from_index(index));
        cursor
    }
    pub fn key_sort_order(&self) -> IndexKeySortOrder {
        match &self.index_key_info {
            Some(index_key_info) => index_key_info.sort_order,
            None => IndexKeySortOrder::default(),
        }
    }
    pub fn has_rowid(&self) -> bool {
        match &self.index_key_info {
            Some(index_key_info) => index_key_info.has_rowid,
            None => true,
        }
    }
    pub fn get_index_rowid_from_record(&self) -> Option<i64> {
        if !self.has_rowid() {
            return None;
        }
        let rowid = match self.get_immutable_record().as_ref().unwrap().last_value() {
            Some(RefValue::Integer(rowid)) => *rowid as i64,
            _ => {
                unreachable!(
                    "index where has_rowid() is true should have an integer rowid as the last value"
                )
            }
        };
        Some(rowid)
    }
    /// Check if the table is empty.
    /// This is done by checking if the root page has no cells.
    fn is_empty_table(&self) -> Result<CursorResult<bool>> {
        if let Some(mv_cursor) = &self.mv_cursor {
            let mv_cursor = mv_cursor.borrow();
            return Ok(CursorResult::Ok(mv_cursor.is_empty()));
        }
        let page = self.pager.read_page(self.root_page)?;
        return_if_locked!(page);
        let cell_count = page.get().contents.as_ref().unwrap().cell_count();
        Ok(CursorResult::Ok(cell_count == 0))
    }
    /// Move the cursor to the previous record and return it.
    /// Used in backwards iteration.
    #[instrument(skip(self), level = Level::TRACE, name = "prev")]
    fn get_prev_record(&mut self) -> Result<CursorResult<bool>> {
        loop {
            let page = self.stack.top();
            return_if_locked_maybe_load!(self.pager, page);
            let page = page.get();
            let contents = page.get().contents.as_ref().unwrap();
            // Validate the (untrusted) page-type byte before interpreting the
            // page; it may have been reached via a corrupt child pointer.
            contents.validate_btree_page_type()?;
            let cell_count = contents.cell_count();
            let cell_idx = self.stack.current_cell_index();
            if self.stack.current_cell_index() == i32::MAX && !self.going_upwards {
                let rightmost_pointer = contents.rightmost_pointer();
                if let Some(rightmost_pointer) = rightmost_pointer {
                    self.stack
                        .push_backwards(self.read_page(rightmost_pointer as usize)?);
                    continue;
                }
            }
            if cell_idx >= cell_count as i32 {
                self.stack.set_cell_index(cell_count as i32 - 1);
            } else if !self.stack.current_cell_index_less_than_min() {
                let is_index = page.is_index();
                let should_visit_internal_node = is_index && self.going_upwards;
                let page_type = contents.page_type();
                if should_visit_internal_node {
                    self.going_upwards = false;
                    return Ok(CursorResult::Ok(true));
                } else if matches!(
                    page_type, PageType::IndexLeaf | PageType::TableLeaf |
                    PageType::TableInterior
                ) {
                    self.stack.retreat();
                }
            }
            if self.stack.current_cell_index_less_than_min() {
                loop {
                    if self.stack.current_cell_index() >= 0 {
                        break;
                    }
                    if self.stack.has_parent() {
                        self.going_upwards = true;
                        self.stack.pop();
                    } else {
                        return Ok(CursorResult::Ok(false));
                    }
                }
                continue;
            }
            let cell_idx = self.stack.current_cell_index() as usize;
            let cell = contents
                .cell_get(
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
                )?;
            match cell {
                BTreeCell::TableInteriorCell(
                    TableInteriorCell { _left_child_page, _rowid },
                ) => {
                    let mem_page = self.read_page(_left_child_page as usize)?;
                    self.stack.push_backwards(mem_page);
                    continue;
                }
                BTreeCell::TableLeafCell(TableLeafCell { .. }) => {
                    return Ok(CursorResult::Ok(true));
                }
                BTreeCell::IndexInteriorCell(
                    IndexInteriorCell { left_child_page, .. },
                ) => {
                    if !self.going_upwards {
                        let mem_page = self.read_page(left_child_page as usize)?;
                        self.stack.retreat();
                        self.stack.push_backwards(mem_page);
                        continue;
                    }
                    self.going_upwards = false;
                    return Ok(CursorResult::Ok(true));
                }
                BTreeCell::IndexLeafCell(IndexLeafCell { .. }) => {
                    return Ok(CursorResult::Ok(true));
                }
            }
        }
    }
    /// Reads the record of a cell that has overflow pages. This is a state machine that requires to be called until completion so everything
    /// that calls this function should be reentrant.
    #[instrument(skip_all, level = Level::TRACE)]
    fn process_overflow_read(
        &self,
        payload: &'static [u8],
        start_next_page: u32,
        payload_size: u64,
    ) -> Result<CursorResult<()>> {
        if self.read_overflow_state.borrow().is_none() {
            let page = self.read_page(start_next_page as usize)?;
            *self.read_overflow_state.borrow_mut() = Some(ReadPayloadOverflow {
                payload: payload.to_vec(),
                next_page: start_next_page,
                remaining_to_read: payload_size as usize - payload.len(),
                page,
            });
            return Ok(CursorResult::IO);
        }
        let mut read_overflow_state = self.read_overflow_state.borrow_mut();
        let ReadPayloadOverflow {
            payload,
            next_page,
            remaining_to_read,
            page: page_btree,
        } = read_overflow_state.as_mut().unwrap();
        if page_btree.get().is_locked() {
            return Ok(CursorResult::IO);
        }
        tracing::debug!(next_page, remaining_to_read, "reading overflow page");
        let page = page_btree.get();
        let contents = page.get_contents();
        let next = contents.read_u32_no_offset(0);
        let buf = contents.as_ptr();
        let usable_space = self.pager.usable_space();
        let to_read = (*remaining_to_read).min(usable_space - 4);
        payload.extend_from_slice(&buf[4..4 + to_read]);
        *remaining_to_read -= to_read;
        if *remaining_to_read != 0 && next != 0 {
            let new_page = self
                .pager
                .read_page(next as usize)
                .map(|page| {
                    Arc::new(BTreePageInner {
                        page: RefCell::new(page),
                    })
                })?;
            *page_btree = new_page;
            *next_page = next;
            return Ok(CursorResult::IO);
        }
        assert!(
            * remaining_to_read == 0 && next == 0,
            "we can't have more pages to read while also have read everything"
        );
        let mut payload_swap = Vec::new();
        std::mem::swap(payload, &mut payload_swap);
        let mut reuse_immutable = self.get_immutable_record_or_create();
        crate::storage::sqlite3_ondisk::read_record(
            &payload_swap,
            reuse_immutable.as_mut().unwrap(),
        )?;
        let _ = read_overflow_state.take();
        Ok(CursorResult::Ok(()))
    }
    /// Calculates how much of a cell's payload should be stored locally vs in overflow pages
    ///
    /// Parameters:
    /// - payload_len: Total length of the payload data
    /// - page_type: Type of the B-tree page (affects local storage thresholds)
    ///
    /// Returns:
    /// - A tuple of (n_local, payload_len) where:
    ///   - n_local: Amount of payload to store locally on the page
    ///   - payload_len: Total payload length (unchanged from input)
    pub fn parse_cell_info(
        &self,
        payload_len: usize,
        page_type: PageType,
        usable_size: usize,
    ) -> Result<(usize, usize)> {
        let max_local = payload_overflow_threshold_max(page_type, usable_size as u16);
        let min_local = payload_overflow_threshold_min(page_type, usable_size as u16);
        let n_local = if payload_len <= max_local {
            payload_len
        } else {
            let surplus = min_local
                + (payload_len - min_local) % (self.usable_space() - 4);
            if surplus <= max_local { surplus } else { min_local }
        };
        Ok((n_local, payload_len))
    }
    /// This function is used to read/write into the payload of a cell that
    /// cursor is pointing to.
    /// Parameters:
    /// - offset: offset in the payload to start reading/writing
    /// - buffer: buffer to read/write into
    /// - amount: amount of bytes to read/write
    /// - is_write: true if writing, false if reading
    ///
    /// If the cell has overflow pages, it will skip till the overflow page which
    /// is at the offset given.
    pub fn read_write_payload_with_offset(
        &mut self,
        mut offset: u32,
        buffer: &mut Vec<u8>,
        mut amount: u32,
        is_write: bool,
    ) -> Result<CursorResult<()>> {
        if let CursorState::ReadWritePayload(
            PayloadOverflowWithOffset::SkipOverflowPages { .. },
        )
        | CursorState::ReadWritePayload(PayloadOverflowWithOffset::ProcessPage { .. }) = &self
            .state
        {
            return self
                .continue_payload_overflow_with_offset(buffer, self.usable_space());
        }
        let page_btree = self.stack.top();
        return_if_locked_maybe_load!(self.pager, page_btree);
        let page = page_btree.get();
        let contents = page.get().contents.as_ref().unwrap();
        let cell_idx = self.stack.current_cell_index() as usize - 1;
        if cell_idx >= contents.cell_count() {
            return Err(LimboError::Corrupt("Invalid cell index".into()));
        }
        let usable_size = self.usable_space();
        let cell = contents
            .cell_get(
                cell_idx,
                payload_overflow_threshold_max(contents.page_type(), usable_size as u16),
                payload_overflow_threshold_min(contents.page_type(), usable_size as u16),
                usable_size,
            )?;
        let (payload, payload_size, first_overflow_page) = match cell {
            BTreeCell::TableLeafCell(cell) => {
                (cell._payload, cell.payload_size, cell.first_overflow_page)
            }
            BTreeCell::IndexLeafCell(cell) => {
                (cell.payload, cell.payload_size, cell.first_overflow_page)
            }
            BTreeCell::IndexInteriorCell(cell) => {
                (cell.payload, cell.payload_size, cell.first_overflow_page)
            }
            BTreeCell::TableInteriorCell(_) => {
                return Err(
                    LimboError::Corrupt(
                        "Cannot access payload of table interior cell".into(),
                    ),
                );
            }
        };
        // `payload_size` comes from the (untrusted) cell header; a corrupt value
        // smaller than the requested range would make the copy below read out of
        // bounds. Reject it with a typed error instead of panicking.
        if offset + amount > payload_size as u32 {
            return Err(LimboError::Corrupt(
                "payload read range exceeds cell payload size".into(),
            ));
        }
        let (local_size, _) = self
            .parse_cell_info(payload_size as usize, contents.page_type(), usable_size)?;
        let mut bytes_processed: u32 = 0;
        if offset < local_size as u32 {
            let mut local_amount: u32 = amount;
            if local_amount + offset > local_size as u32 {
                local_amount = local_size as u32 - offset;
            }
            if is_write {
                self.write_payload_to_page(
                    offset,
                    local_amount,
                    payload,
                    buffer,
                    page_btree.clone(),
                );
            } else {
                self.read_payload_from_page(offset, local_amount, payload, buffer);
            }
            offset = 0;
            amount -= local_amount;
            bytes_processed += local_amount;
        } else {
            offset -= local_size as u32;
        }
        if amount > 0 {
            if first_overflow_page.is_none() {
                return Err(
                    LimboError::Corrupt("Expected overflow page but none found".into()),
                );
            }
            let overflow_size = usable_size - 4;
            let pages_to_skip = offset / overflow_size as u32;
            let page_offset = offset % overflow_size as u32;
            self.state = CursorState::ReadWritePayload(PayloadOverflowWithOffset::SkipOverflowPages {
                next_page: first_overflow_page.unwrap(),
                pages_left_to_skip: pages_to_skip,
                page_offset: page_offset,
                amount: amount,
                buffer_offset: bytes_processed as usize,
                is_write,
            });
            return Ok(CursorResult::IO);
        }
        Ok(CursorResult::Ok(()))
    }
    pub fn continue_payload_overflow_with_offset(
        &mut self,
        buffer: &mut Vec<u8>,
        usable_space: usize,
    ) -> Result<CursorResult<()>> {
        loop {
            let mut state = std::mem::replace(&mut self.state, CursorState::None);
            match &mut state {
                CursorState::ReadWritePayload(
                    PayloadOverflowWithOffset::SkipOverflowPages {
                        next_page,
                        pages_left_to_skip,
                        page_offset,
                        amount,
                        buffer_offset,
                        is_write,
                    },
                ) => {
                    if *pages_left_to_skip == 0 {
                        let page = self.read_page(*next_page as usize)?;
                        return_if_locked_maybe_load!(self.pager, page);
                        self.state = CursorState::ReadWritePayload(PayloadOverflowWithOffset::ProcessPage {
                            next_page: *next_page,
                            remaining_to_read: *amount,
                            page: page,
                            current_offset: *page_offset as usize,
                            buffer_offset: *buffer_offset,
                            is_write: *is_write,
                        });
                        continue;
                    }
                    let page = self.read_page(*next_page as usize)?;
                    return_if_locked_maybe_load!(self.pager, page);
                    let page = page.get();
                    let contents = page.get_contents();
                    let next = contents.read_u32_no_offset(0);
                    if next == 0 {
                        return Err(
                            LimboError::Corrupt("Overflow chain ends prematurely".into()),
                        );
                    }
                    *next_page = next;
                    *pages_left_to_skip -= 1;
                    self.state = CursorState::ReadWritePayload(PayloadOverflowWithOffset::SkipOverflowPages {
                        next_page: next,
                        pages_left_to_skip: *pages_left_to_skip,
                        page_offset: *page_offset,
                        amount: *amount,
                        buffer_offset: *buffer_offset,
                        is_write: *is_write,
                    });
                    return Ok(CursorResult::IO);
                }
                CursorState::ReadWritePayload(
                    PayloadOverflowWithOffset::ProcessPage {
                        next_page,
                        remaining_to_read,
                        page: page_btree,
                        current_offset,
                        buffer_offset,
                        is_write,
                    },
                ) => {
                    if page_btree.get().is_locked() {
                        self.state = CursorState::ReadWritePayload(PayloadOverflowWithOffset::ProcessPage {
                            next_page: *next_page,
                            remaining_to_read: *remaining_to_read,
                            page: page_btree.clone(),
                            current_offset: *current_offset,
                            buffer_offset: *buffer_offset,
                            is_write: *is_write,
                        });
                        return Ok(CursorResult::IO);
                    }
                    let page = page_btree.get();
                    let contents = page.get_contents();
                    let overflow_size = usable_space - 4;
                    let page_offset = *current_offset;
                    let bytes_to_process = std::cmp::min(
                        *remaining_to_read,
                        overflow_size as u32 - page_offset as u32,
                    );
                    let payload_offset = 4 + page_offset;
                    let page_payload = contents.as_ptr();
                    if *is_write {
                        self.write_payload_to_page(
                            payload_offset as u32,
                            bytes_to_process,
                            page_payload,
                            buffer,
                            page_btree.clone(),
                        );
                    } else {
                        self.read_payload_from_page(
                            payload_offset as u32,
                            bytes_to_process,
                            page_payload,
                            buffer,
                        );
                    }
                    *remaining_to_read -= bytes_to_process;
                    *buffer_offset += bytes_to_process as usize;
                    if *remaining_to_read == 0 {
                        self.state = CursorState::None;
                        return Ok(CursorResult::Ok(()));
                    }
                    let next = contents.read_u32_no_offset(0);
                    if next == 0 {
                        return Err(
                            LimboError::Corrupt("Overflow chain ends prematurely".into()),
                        );
                    }
                    *next_page = next;
                    *current_offset = 0;
                    *page_btree = self.read_page(next as usize)?;
                    return Ok(CursorResult::IO);
                }
                _ => {
                    return Err(
                        LimboError::InternalError(
                            "Invalid state for continue_payload_overflow_with_offset"
                                .into(),
                        ),
                    );
                }
            }
        }
    }
    fn read_payload_from_page(
        &self,
        payload_offset: u32,
        num_bytes: u32,
        payload: &[u8],
        buffer: &mut Vec<u8>,
    ) {
        buffer
            .extend_from_slice(
                &payload[payload_offset as usize..(payload_offset + num_bytes) as usize],
            );
    }
    /// This function write from a buffer into a page.
    /// SAFETY: This function uses unsafe in the write path to write to the page payload directly.
    /// - Make sure the page is pointing to valid data ie the page is not evicted from the page-cache.
    fn write_payload_to_page(
        &mut self,
        payload_offset: u32,
        num_bytes: u32,
        payload: &[u8],
        buffer: &mut Vec<u8>,
        page: BTreePage,
    ) {
        page.get().set_dirty();
        self.pager.add_dirty(page.get().get().id);
        let payload_mut = unsafe {
            std::slice::from_raw_parts_mut(payload.as_ptr() as *mut u8, payload.len())
        };
        payload_mut[payload_offset
                as usize..payload_offset as usize + num_bytes as usize]
            .copy_from_slice(&buffer[..num_bytes as usize]);
    }
    /// Move the cursor to the next record and return it.
    /// Used in forwards iteration, which is the default.
    #[instrument(skip(self), level = Level::TRACE, name = "next")]
    fn get_next_record(&mut self) -> Result<CursorResult<bool>> {
        if let Some(mv_cursor) = &self.mv_cursor {
            let mut mv_cursor = mv_cursor.borrow_mut();
            let rowid = mv_cursor.current_row_id();
            match rowid {
                Some(_rowid) => {
                    mv_cursor.forward();
                    return Ok(CursorResult::Ok(true));
                }
                None => return Ok(CursorResult::Ok(false)),
            }
        }
        loop {
            let mem_page_rc = self.stack.top();
            return_if_locked_maybe_load!(self.pager, mem_page_rc);
            let mem_page = mem_page_rc.get();
            let contents = mem_page.get_contents();
            // Validate the (untrusted) page-type byte before interpreting the
            // page; it may have been reached via a corrupt child pointer.
            contents.validate_btree_page_type()?;
            let cell_count = contents.cell_count();
            tracing::debug!(
                id = mem_page_rc.get().get_ref().id, cell = self.stack.current_cell_index(),
                cell_count, "current_before_advance",
            );
            let is_index = mem_page_rc.get().is_index();
            let should_skip_advance = is_index && self.going_upwards
                && self.stack.current_cell_index() >= 0
                && self.stack.current_cell_index() < cell_count as i32;
            if should_skip_advance {
                tracing::debug!(
                    going_upwards = self.going_upwards, page = mem_page_rc.get().get_ref()
                    .id, cell_idx = self.stack.current_cell_index(), "skipping advance",
                );
                self.going_upwards = false;
                return Ok(CursorResult::Ok(true));
            }
            self.stack.advance();
            let cell_idx = self.stack.current_cell_index() as usize;
            tracing::debug!(id = mem_page_rc.get().get_ref().id, cell = cell_idx, "current");
            if cell_idx == cell_count {
                let has_parent = self.stack.has_parent();
                match contents.rightmost_pointer() {
                    Some(right_most_pointer) => {
                        self.stack.advance();
                        let mem_page = self.read_page(right_most_pointer as usize)?;
                        self.stack.push(mem_page);
                        continue;
                    }
                    None => {
                        if has_parent {
                            tracing::trace!("moving simple upwards");
                            self.going_upwards = true;
                            self.stack.pop();
                            continue;
                        } else {
                            return Ok(CursorResult::Ok(false));
                        }
                    }
                }
            }
            if cell_idx > contents.cell_count() {
                let has_parent = self.stack.current() > 0;
                if has_parent {
                    tracing::debug!("moving upwards");
                    self.going_upwards = true;
                    self.stack.pop();
                    continue;
                } else {
                    return Ok(CursorResult::Ok(false));
                }
            }
            if cell_idx >= contents.cell_count() {
                return Err(LimboError::Corrupt(
                    "cell index out of bounds during traversal".into(),
                ));
            }
            let cell = contents
                .cell_get(
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
                )?;
            match &cell {
                BTreeCell::TableInteriorCell(
                    TableInteriorCell { _left_child_page, _rowid },
                ) => {
                    let mem_page = self.read_page(*_left_child_page as usize)?;
                    self.stack.push(mem_page);
                    continue;
                }
                BTreeCell::TableLeafCell(TableLeafCell { .. }) => {
                    return Ok(CursorResult::Ok(true));
                }
                BTreeCell::IndexInteriorCell(
                    IndexInteriorCell { left_child_page, .. },
                ) => {
                    if self.going_upwards {
                        self.going_upwards = false;
                        return Ok(CursorResult::Ok(true));
                    } else {
                        let mem_page = self.read_page(*left_child_page as usize)?;
                        self.stack.push(mem_page);
                        continue;
                    }
                }
                BTreeCell::IndexLeafCell(IndexLeafCell { .. }) => {
                    return Ok(CursorResult::Ok(true));
                }
            }
        }
    }
    /// Move the cursor to the record that matches the seek key and seek operation.
    /// This may be used to seek to a specific record in a point query (e.g. SELECT * FROM table WHERE col = 10)
    /// or e.g. find the first record greater than the seek key in a range query (e.g. SELECT * FROM table WHERE col > 10).
    /// We don't include the rowid in the comparison and that's why the last value from the record is not included.
    fn do_seek(&mut self, key: SeekKey<'_>, op: SeekOp) -> Result<CursorResult<bool>> {
        let ret = return_if_io!(
            match key { SeekKey::TableRowId(rowid) => { self.tablebtree_seek(rowid, op) }
            SeekKey::IndexKey(index_key) => { self.indexbtree_seek(index_key, op) } }
        );
        self.valid_state = CursorValidState::Valid;
        Ok(CursorResult::Ok(ret))
    }
    /// Move the cursor to the root page of the btree.
    #[instrument(skip_all, level = Level::TRACE)]
    fn move_to_root(&mut self) {
        self.seek_state = CursorSeekState::Start;
        self.going_upwards = false;
        tracing::trace!(root_page = self.root_page);
        let mem_page = self.read_page(self.root_page).unwrap();
        self.stack.clear();
        self.stack.push(mem_page);
    }
    /// Move the cursor to the rightmost record in the btree.
    #[instrument(skip(self), level = Level::TRACE)]
    fn move_to_rightmost(&mut self) -> Result<CursorResult<bool>> {
        self.move_to_root();
        loop {
            let mem_page = self.stack.top();
            let page_idx = mem_page.get().get().id;
            let page = self.read_page(page_idx)?;
            return_if_locked_maybe_load!(self.pager, page);
            let page = page.get();
            let contents = page.get().contents.as_ref().unwrap();
            if contents.is_leaf() {
                if contents.cell_count() > 0 {
                    self.stack.set_cell_index(contents.cell_count() as i32 - 1);
                    return Ok(CursorResult::Ok(true));
                }
                return Ok(CursorResult::Ok(false));
            }
            match contents.rightmost_pointer() {
                Some(right_most_pointer) => {
                    self.stack.set_cell_index(contents.cell_count() as i32 + 1);
                    let mem_page = self.read_page(right_most_pointer as usize)?;
                    self.stack.push(mem_page);
                    continue;
                }
                None => {
                    unreachable!("interior page should have a rightmost pointer");
                }
            }
        }
    }
    /// Specialized version of move_to() for table btrees.
    #[instrument(skip(self), level = Level::TRACE)]
    fn tablebtree_move_to(
        &mut self,
        rowid: i64,
        seek_op: SeekOp,
    ) -> Result<CursorResult<()>> {
        'outer: loop {
            let page = self.stack.top();
            return_if_locked_maybe_load!(self.pager, page);
            let page = page.get();
            let contents = page.get().contents.as_ref().unwrap();
            // This page was reached by following an (untrusted) child/rightmost
            // pointer; validate its page-type byte so a corrupt pointer to a
            // non-b-tree page yields a Corrupt error rather than panicking in the
            // infallible `is_leaf()`/`page_type()` below.
            contents.validate_btree_page_type()?;
            if contents.is_leaf() {
                self.seek_state = CursorSeekState::FoundLeaf {
                    eq_seen: Cell::new(false),
                };
                return Ok(CursorResult::Ok(()));
            }
            let cell_count = contents.cell_count();
            if matches!(
                self.seek_state, CursorSeekState::Start |
                CursorSeekState::MovingBetweenPages { .. }
            ) {
                let eq_seen = match &self.seek_state {
                    CursorSeekState::MovingBetweenPages { eq_seen } => eq_seen.get(),
                    _ => false,
                };
                let min_cell_idx = Cell::new(0);
                let max_cell_idx = Cell::new(cell_count as isize - 1);
                let nearest_matching_cell = Cell::new(None);
                self.seek_state = CursorSeekState::InteriorPageBinarySearch {
                    min_cell_idx,
                    max_cell_idx,
                    nearest_matching_cell,
                    eq_seen: Cell::new(eq_seen),
                };
            }
            let CursorSeekState::InteriorPageBinarySearch {
                min_cell_idx,
                max_cell_idx,
                nearest_matching_cell,
                eq_seen,
                ..
            } = &self.seek_state else {
                unreachable!("we must be in an interior binary search state");
            };
            loop {
                let min = min_cell_idx.get();
                let max = max_cell_idx.get();
                if min > max {
                    if let Some(nearest_matching_cell) = nearest_matching_cell.get() {
                        let left_child_page = contents
                            .cell_table_interior_read_left_child_page(
                                nearest_matching_cell as usize,
                            )?;
                        self.stack.set_cell_index(nearest_matching_cell as i32);
                        let mem_page = self.read_page(left_child_page as usize)?;
                        self.stack.push(mem_page);
                        self.seek_state = CursorSeekState::MovingBetweenPages {
                            eq_seen: Cell::new(eq_seen.get()),
                        };
                        continue 'outer;
                    }
                    self.stack.set_cell_index(cell_count as i32 + 1);
                    match contents.rightmost_pointer() {
                        Some(right_most_pointer) => {
                            let mem_page = self.read_page(right_most_pointer as usize)?;
                            self.stack.push(mem_page);
                            self.seek_state = CursorSeekState::MovingBetweenPages {
                                eq_seen: Cell::new(eq_seen.get()),
                            };
                            continue 'outer;
                        }
                        None => {
                            unreachable!(
                                "we shall not go back up! The only way is down the slope"
                            );
                        }
                    }
                }
                let cur_cell_idx = (min + max) >> 1;
                let cell_rowid = contents
                    .cell_table_interior_read_rowid(cur_cell_idx as usize)?;
                let is_on_left = match seek_op {
                    SeekOp::GT => cell_rowid > rowid,
                    SeekOp::GE { .. } => cell_rowid >= rowid,
                    SeekOp::LE { .. } => cell_rowid >= rowid,
                    SeekOp::LT => cell_rowid + 1 >= rowid,
                };
                if is_on_left {
                    nearest_matching_cell.set(Some(cur_cell_idx as usize));
                    max_cell_idx.set(cur_cell_idx - 1);
                } else {
                    min_cell_idx.set(cur_cell_idx + 1);
                }
            }
        }
    }
    /// Specialized version of move_to() for index btrees.
    #[instrument(skip(self, index_key), level = Level::TRACE)]
    fn indexbtree_move_to(
        &mut self,
        index_key: &ImmutableRecord,
        cmp: SeekOp,
    ) -> Result<CursorResult<()>> {
        let iter_dir = cmp.iteration_direction();
        'outer: loop {
            let page = self.stack.top();
            return_if_locked_maybe_load!(self.pager, page);
            let page = page.get();
            let contents = page.get().contents.as_ref().unwrap();
            // Validate the (untrusted) page-type byte before interpreting the
            // page: a corrupt child pointer may reference a non-b-tree page.
            contents.validate_btree_page_type()?;
            if contents.is_leaf() {
                let eq_seen = match &self.seek_state {
                    CursorSeekState::MovingBetweenPages { eq_seen } => eq_seen.get(),
                    _ => false,
                };
                self.seek_state = CursorSeekState::FoundLeaf {
                    eq_seen: Cell::new(eq_seen),
                };
                return Ok(CursorResult::Ok(()));
            }
            if matches!(
                self.seek_state, CursorSeekState::Start |
                CursorSeekState::MovingBetweenPages { .. }
            ) {
                let eq_seen = match &self.seek_state {
                    CursorSeekState::MovingBetweenPages { eq_seen } => eq_seen.get(),
                    _ => false,
                };
                let cell_count = contents.cell_count();
                let min_cell_idx = Cell::new(0);
                let max_cell_idx = Cell::new(cell_count as isize - 1);
                let nearest_matching_cell = Cell::new(None);
                self.seek_state = CursorSeekState::InteriorPageBinarySearch {
                    min_cell_idx,
                    max_cell_idx,
                    nearest_matching_cell,
                    eq_seen: Cell::new(eq_seen),
                };
            }
            let CursorSeekState::InteriorPageBinarySearch {
                min_cell_idx,
                max_cell_idx,
                nearest_matching_cell,
                eq_seen,
            } = &self.seek_state else {
                unreachable!(
                    "we must be in an interior binary search state, got {:?}", self
                    .seek_state
                );
            };
            loop {
                let min = min_cell_idx.get();
                let max = max_cell_idx.get();
                if min > max {
                    let Some(leftmost_matching_cell) = nearest_matching_cell.get() else {
                        self.stack.set_cell_index(contents.cell_count() as i32 + 1);
                        match contents.rightmost_pointer() {
                            Some(right_most_pointer) => {
                                let mem_page = self.read_page(right_most_pointer as usize)?;
                                self.stack.push(mem_page);
                                self.seek_state = CursorSeekState::MovingBetweenPages {
                                    eq_seen: Cell::new(eq_seen.get()),
                                };
                                continue 'outer;
                            }
                            None => {
                                unreachable!(
                                    "we shall not go back up! The only way is down the slope"
                                );
                            }
                        }
                    };
                    let matching_cell = contents
                        .cell_get(
                            leftmost_matching_cell,
                            payload_overflow_threshold_max(
                                contents.page_type(),
                                self.usable_space() as u16,
                            ),
                            payload_overflow_threshold_min(
                                contents.page_type(),
                                self.usable_space() as u16,
                            ),
                            self.usable_space(),
                        )?;
                    self.stack.set_cell_index(leftmost_matching_cell as i32);
                    if iter_dir == IterationDirection::Backwards {
                        self.stack.retreat();
                    }
                    let BTreeCell::IndexInteriorCell(
                        IndexInteriorCell { left_child_page, .. },
                    ) = &matching_cell else {
                        unreachable!("unexpected cell type: {:?}", matching_cell);
                    };
                    let mem_page = self.read_page(*left_child_page as usize)?;
                    self.stack.push(mem_page);
                    self.seek_state = CursorSeekState::MovingBetweenPages {
                        eq_seen: Cell::new(eq_seen.get()),
                    };
                    continue 'outer;
                }
                let cur_cell_idx = (min + max) >> 1;
                self.stack.set_cell_index(cur_cell_idx as i32);
                let cell = contents
                    .cell_get(
                        cur_cell_idx as usize,
                        payload_overflow_threshold_max(
                            contents.page_type(),
                            self.usable_space() as u16,
                        ),
                        payload_overflow_threshold_min(
                            contents.page_type(),
                            self.usable_space() as u16,
                        ),
                        self.usable_space(),
                    )?;
                let BTreeCell::IndexInteriorCell(
                    IndexInteriorCell { payload, payload_size, first_overflow_page, .. },
                ) = &cell else {
                    unreachable!("unexpected cell type: {:?}", cell);
                };
                if let Some(next_page) = first_overflow_page {
                    return_if_io!(
                        self.process_overflow_read(payload, * next_page, * payload_size)
                    )
                } else {
                    crate::storage::sqlite3_ondisk::read_record(
                        payload,
                        self.get_immutable_record_or_create().as_mut().unwrap(),
                    )?
                };
                let (target_leaf_page_is_in_left_subtree, is_eq) = {
                    let record = self.get_immutable_record();
                    let record = record.as_ref().unwrap();
                    let record_slice_equal_number_of_cols = &record
                        .get_values()
                        .as_slice()[..index_key.get_values().len()];
                    let interior_cell_vs_index_key = compare_immutable(
                        record_slice_equal_number_of_cols,
                        index_key.get_values(),
                        self.key_sort_order(),
                        &self.collations,
                    );
                    (
                        match cmp {
                            SeekOp::GT => interior_cell_vs_index_key.is_gt(),
                            SeekOp::GE { .. } => interior_cell_vs_index_key.is_ge(),
                            SeekOp::LE { .. } => interior_cell_vs_index_key.is_gt(),
                            SeekOp::LT => interior_cell_vs_index_key.is_ge(),
                        },
                        interior_cell_vs_index_key.is_eq(),
                    )
                };
                if is_eq {
                    eq_seen.set(true);
                }
                if target_leaf_page_is_in_left_subtree {
                    nearest_matching_cell.set(Some(cur_cell_idx as usize));
                    max_cell_idx.set(cur_cell_idx - 1);
                } else {
                    min_cell_idx.set(cur_cell_idx + 1);
                }
            }
        }
    }
    /// Specialized version of do_seek() for table btrees that uses binary search instead
    /// of iterating cells in order.
    #[instrument(skip_all, level = Level::TRACE)]
    fn tablebtree_seek(
        &mut self,
        rowid: i64,
        seek_op: SeekOp,
    ) -> Result<CursorResult<bool>> {
        assert!(self.mv_cursor.is_none());
        let iter_dir = seek_op.iteration_direction();
        if matches!(
            self.seek_state, CursorSeekState::Start { .. } |
            CursorSeekState::MovingBetweenPages { .. } |
            CursorSeekState::InteriorPageBinarySearch { .. }
        ) {
            return_if_io!(self.move_to(SeekKey::TableRowId(rowid), seek_op));
            let page = self.stack.top();
            return_if_locked_maybe_load!(self.pager, page);
            let page = page.get();
            let contents = page.get().contents.as_ref().unwrap();
            // The page was reached by following (untrusted) child pointers; a
            // corrupt interior pointer could land us on a non-leaf or invalid
            // page. Validate the page type instead of asserting.
            contents.validate_btree_page_type()?;
            if !contents.is_leaf() {
                return Err(LimboError::Corrupt(
                    "tablebtree_seek() reached a non-leaf page".into(),
                ));
            }
            let cell_count = contents.cell_count();
            if cell_count == 0 {
                self.stack.set_cell_index(0);
                return Ok(CursorResult::Ok(false));
            }
            let min_cell_idx = Cell::new(0);
            let max_cell_idx = Cell::new(cell_count as isize - 1);
            let nearest_matching_cell = Cell::new(None);
            self.seek_state = CursorSeekState::LeafPageBinarySearch {
                min_cell_idx,
                max_cell_idx,
                nearest_matching_cell,
                moving_up_to_parent: Cell::new(false),
                eq_seen: Cell::new(false),
            };
        }
        let CursorSeekState::LeafPageBinarySearch {
            min_cell_idx,
            max_cell_idx,
            nearest_matching_cell,
            ..
        } = &self.seek_state else {
            unreachable!("we must be in a leaf binary search state");
        };
        let page = self.stack.top();
        return_if_locked_maybe_load!(self.pager, page);
        let page = page.get();
        let contents = page.get().contents.as_ref().unwrap();
        loop {
            let min = min_cell_idx.get();
            let max = max_cell_idx.get();
            if min > max {
                if let Some(nearest_matching_cell) = nearest_matching_cell.get() {
                    self.stack.set_cell_index(nearest_matching_cell as i32);
                    return Ok(CursorResult::Ok(true));
                } else {
                    return Ok(CursorResult::Ok(false));
                };
            }
            let cur_cell_idx = (min + max) >> 1;
            let cell_rowid = contents.cell_table_leaf_read_rowid(cur_cell_idx as usize)?;
            let cmp = cell_rowid.cmp(&rowid);
            let found = match seek_op {
                SeekOp::GT => cmp.is_gt(),
                SeekOp::GE { eq_only: true } => cmp.is_eq(),
                SeekOp::GE { eq_only: false } => cmp.is_ge(),
                SeekOp::LE { eq_only: true } => cmp.is_eq(),
                SeekOp::LE { eq_only: false } => cmp.is_le(),
                SeekOp::LT => cmp.is_lt(),
            };
            if found && seek_op.eq_only() {
                self.stack.set_cell_index(cur_cell_idx as i32);
                return Ok(CursorResult::Ok(true));
            }
            if found {
                nearest_matching_cell.set(Some(cur_cell_idx as usize));
                match iter_dir {
                    IterationDirection::Forwards => {
                        max_cell_idx.set(cur_cell_idx - 1);
                    }
                    IterationDirection::Backwards => {
                        min_cell_idx.set(cur_cell_idx + 1);
                    }
                }
            } else {
                if cmp.is_gt() {
                    max_cell_idx.set(cur_cell_idx - 1);
                } else if cmp.is_lt() {
                    min_cell_idx.set(cur_cell_idx + 1);
                } else {
                    match iter_dir {
                        IterationDirection::Forwards => {
                            min_cell_idx.set(cur_cell_idx + 1);
                        }
                        IterationDirection::Backwards => {
                            max_cell_idx.set(cur_cell_idx - 1);
                        }
                    }
                }
            }
        }
    }
    #[instrument(skip_all, level = Level::TRACE)]
    fn indexbtree_seek(
        &mut self,
        key: &ImmutableRecord,
        seek_op: SeekOp,
    ) -> Result<CursorResult<bool>> {
        if matches!(
            self.seek_state, CursorSeekState::Start { .. } |
            CursorSeekState::MovingBetweenPages { .. } |
            CursorSeekState::InteriorPageBinarySearch { .. }
        ) {
            return_if_io!(self.move_to(SeekKey::IndexKey(key), seek_op));
            let CursorSeekState::FoundLeaf { eq_seen } = &self.seek_state else {
                unreachable!(
                    "We must still be in FoundLeaf state after move_to, got: {:?}", self
                    .seek_state
                );
            };
            let eq_seen = eq_seen.get();
            let page = self.stack.top();
            return_if_locked_maybe_load!(self.pager, page);
            let page = page.get();
            let contents = page.get().contents.as_ref().unwrap();
            let cell_count = contents.cell_count();
            if cell_count == 0 {
                self.stack.set_cell_index(0);
                match seek_op.iteration_direction() {
                    IterationDirection::Forwards => {
                        return self.next();
                    }
                    IterationDirection::Backwards => {
                        return self.prev();
                    }
                }
            }
            let min = Cell::new(0);
            let max = Cell::new(cell_count as isize - 1);
            let nearest_matching_cell = Cell::new(None);
            self.seek_state = CursorSeekState::LeafPageBinarySearch {
                min_cell_idx: min,
                max_cell_idx: max,
                nearest_matching_cell,
                moving_up_to_parent: Cell::new(false),
                eq_seen: Cell::new(eq_seen),
            };
        }
        let CursorSeekState::LeafPageBinarySearch {
            min_cell_idx,
            max_cell_idx,
            nearest_matching_cell,
            eq_seen,
            moving_up_to_parent,
        } = &self.seek_state else {
            unreachable!(
                "we must be in a leaf binary search state, got: {:?}", self.seek_state
            );
        };
        if moving_up_to_parent.get() {
            let page = self.stack.top();
            return_if_locked_maybe_load!(self.pager, page);
            let page = page.get();
            let contents = page.get().contents.as_ref().unwrap();
            let cur_cell_idx = self.stack.current_cell_index() as usize;
            let cell = contents
                .cell_get(
                    cur_cell_idx,
                    payload_overflow_threshold_max(
                        contents.page_type(),
                        self.usable_space() as u16,
                    ),
                    payload_overflow_threshold_min(
                        contents.page_type(),
                        self.usable_space() as u16,
                    ),
                    self.usable_space(),
                )?;
            let BTreeCell::IndexInteriorCell(
                IndexInteriorCell { payload, first_overflow_page, payload_size, .. },
            ) = &cell else {
                unreachable!("unexpected cell type: {:?}", cell);
            };
            if let Some(next_page) = first_overflow_page {
                return_if_io!(
                    self.process_overflow_read(payload, * next_page, * payload_size)
                )
            } else {
                crate::storage::sqlite3_ondisk::read_record(
                    payload,
                    self.get_immutable_record_or_create().as_mut().unwrap(),
                )?
            };
            let (_, found) = self.compare_with_current_record(key, seek_op);
            moving_up_to_parent.set(false);
            return Ok(CursorResult::Ok(found));
        }
        let page = self.stack.top();
        return_if_locked_maybe_load!(self.pager, page);
        let page = page.get();
        let contents = page.get().contents.as_ref().unwrap();
        let cell_count = contents.cell_count();
        let iter_dir = seek_op.iteration_direction();
        loop {
            let min = min_cell_idx.get();
            let max = max_cell_idx.get();
            if min > max {
                if let Some(nearest_matching_cell) = nearest_matching_cell.get() {
                    self.stack.set_cell_index(nearest_matching_cell as i32);
                    return Ok(CursorResult::Ok(true));
                } else {
                    if seek_op.eq_only() && !eq_seen.get() {
                        return Ok(CursorResult::Ok(false));
                    }
                    match iter_dir {
                        IterationDirection::Forwards => {
                            if !moving_up_to_parent.get() {
                                moving_up_to_parent.set(true);
                                self.stack.set_cell_index(cell_count as i32);
                            }
                            let next_res = return_if_io!(self.next());
                            if !next_res {
                                return Ok(CursorResult::Ok(false));
                            }
                            return Ok(CursorResult::IO);
                        }
                        IterationDirection::Backwards => {
                            if !moving_up_to_parent.get() {
                                moving_up_to_parent.set(true);
                                self.stack.set_cell_index(-1);
                            }
                            let prev_res = return_if_io!(self.prev());
                            if !prev_res {
                                return Ok(CursorResult::Ok(false));
                            }
                            return Ok(CursorResult::IO);
                        }
                    }
                };
            }
            let cur_cell_idx = (min + max) >> 1;
            self.stack.set_cell_index(cur_cell_idx as i32);
            let cell = contents
                .cell_get(
                    cur_cell_idx as usize,
                    payload_overflow_threshold_max(
                        contents.page_type(),
                        self.usable_space() as u16,
                    ),
                    payload_overflow_threshold_min(
                        contents.page_type(),
                        self.usable_space() as u16,
                    ),
                    self.usable_space(),
                )?;
            let BTreeCell::IndexLeafCell(
                IndexLeafCell { payload, first_overflow_page, payload_size },
            ) = &cell else {
                unreachable!("unexpected cell type: {:?}", cell);
            };
            if let Some(next_page) = first_overflow_page {
                return_if_io!(
                    self.process_overflow_read(payload, * next_page, * payload_size)
                )
            } else {
                crate::storage::sqlite3_ondisk::read_record(
                    payload,
                    self.get_immutable_record_or_create().as_mut().unwrap(),
                )?
            };
            let (cmp, found) = self.compare_with_current_record(key, seek_op);
            if found {
                nearest_matching_cell.set(Some(cur_cell_idx as usize));
                match iter_dir {
                    IterationDirection::Forwards => {
                        max_cell_idx.set(cur_cell_idx - 1);
                    }
                    IterationDirection::Backwards => {
                        min_cell_idx.set(cur_cell_idx + 1);
                    }
                }
            } else {
                if cmp.is_gt() {
                    max_cell_idx.set(cur_cell_idx - 1);
                } else if cmp.is_lt() {
                    min_cell_idx.set(cur_cell_idx + 1);
                } else {
                    match iter_dir {
                        IterationDirection::Forwards => {
                            min_cell_idx.set(cur_cell_idx + 1);
                        }
                        IterationDirection::Backwards => {
                            max_cell_idx.set(cur_cell_idx - 1);
                        }
                    }
                }
            }
        }
    }
    fn compare_with_current_record(
        &self,
        key: &ImmutableRecord,
        seek_op: SeekOp,
    ) -> (Ordering, bool) {
        let cmp = {
            let record = self.get_immutable_record();
            let record = record.as_ref().unwrap();
            tracing::debug!(? record);
            let record_slice_equal_number_of_cols = &record
                .get_values()
                .as_slice()[..key.get_values().len()];
            compare_immutable(
                record_slice_equal_number_of_cols,
                key.get_values(),
                self.key_sort_order(),
                &self.collations,
            )
        };
        let found = match seek_op {
            SeekOp::GT => cmp.is_gt(),
            SeekOp::GE { eq_only: true } => cmp.is_eq(),
            SeekOp::GE { eq_only: false } => cmp.is_ge(),
            SeekOp::LE { eq_only: true } => cmp.is_eq(),
            SeekOp::LE { eq_only: false } => cmp.is_le(),
            SeekOp::LT => cmp.is_lt(),
        };
        (cmp, found)
    }
    fn read_record_w_possible_overflow(
        &mut self,
        payload: &'static [u8],
        next_page: Option<u32>,
        payload_size: u64,
    ) -> Result<CursorResult<()>> {
        if let Some(next_page) = next_page {
            self.process_overflow_read(payload, next_page, payload_size)
        } else {
            crate::storage::sqlite3_ondisk::read_record(
                payload,
                self.get_immutable_record_or_create().as_mut().unwrap(),
            )?;
            Ok(CursorResult::Ok(()))
        }
    }
    #[instrument(skip_all, level = Level::TRACE)]
    pub fn move_to(
        &mut self,
        key: SeekKey<'_>,
        cmp: SeekOp,
    ) -> Result<CursorResult<()>> {
        assert!(self.mv_cursor.is_none());
        tracing::trace!(? key, ? cmp);
        if matches!(
            self.seek_state, CursorSeekState::LeafPageBinarySearch { .. } |
            CursorSeekState::FoundLeaf { .. }
        ) {
            self.seek_state = CursorSeekState::Start;
        }
        if matches!(self.seek_state, CursorSeekState::Start) {
            self.move_to_root();
        }
        let ret = match key {
            SeekKey::TableRowId(rowid_key) => self.tablebtree_move_to(rowid_key, cmp),
            SeekKey::IndexKey(index_key) => self.indexbtree_move_to(index_key, cmp),
        };
        return_if_io!(ret);
        Ok(CursorResult::Ok(()))
    }
}
