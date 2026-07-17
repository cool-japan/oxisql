impl BTreeCursor {
    pub fn seek_end(&mut self) -> Result<CursorResult<()>> {
        assert!(self.mv_cursor.is_none());
        self.move_to_root();
        loop {
            let mem_page = self.stack.top();
            let page_id = mem_page.get().get().id;
            let page = self.read_page(page_id)?;
            return_if_locked_maybe_load!(self.pager, page);
            let page = page.get();
            let contents = page.get().contents.as_ref().unwrap();
            if contents.is_leaf() {
                self.stack.set_cell_index(contents.cell_count() as i32);
                return Ok(CursorResult::Ok(()));
            }
            match contents.rightmost_pointer() {
                Some(right_most_pointer) => {
                    self.stack.set_cell_index(contents.cell_count() as i32 + 1);
                    let child = self.read_page(right_most_pointer as usize)?;
                    self.stack.push(child);
                }
                None => unreachable!("interior page must have rightmost pointer"),
            }
        }
    }
    pub fn seek_to_last(&mut self) -> Result<CursorResult<()>> {
        let has_record = return_if_io!(self.move_to_rightmost());
        self.invalidate_record();
        self.has_record.replace(has_record);
        if !has_record {
            let is_empty = return_if_io!(self.is_empty_table());
            assert!(is_empty);
            return Ok(CursorResult::Ok(()));
        }
        Ok(CursorResult::Ok(()))
    }
    pub fn is_empty(&self) -> bool {
        !self.has_record.get()
    }
    pub fn root_page(&self) -> usize {
        self.root_page
    }
    pub fn rewind(&mut self) -> Result<CursorResult<()>> {
        if self.mv_cursor.is_some() {
            let cursor_has_record = return_if_io!(self.get_next_record());
            self.invalidate_record();
            self.has_record.replace(cursor_has_record);
        } else {
            self.move_to_root();
            let cursor_has_record = return_if_io!(self.get_next_record());
            self.invalidate_record();
            self.has_record.replace(cursor_has_record);
        }
        Ok(CursorResult::Ok(()))
    }
    pub fn last(&mut self) -> Result<CursorResult<()>> {
        assert!(self.mv_cursor.is_none());
        let cursor_has_record = return_if_io!(self.move_to_rightmost());
        self.has_record.replace(cursor_has_record);
        self.invalidate_record();
        Ok(CursorResult::Ok(()))
    }
    pub fn next(&mut self) -> Result<CursorResult<bool>> {
        return_if_io!(self.restore_context());
        let cursor_has_record = return_if_io!(self.get_next_record());
        self.has_record.replace(cursor_has_record);
        self.invalidate_record();
        Ok(CursorResult::Ok(cursor_has_record))
    }
    fn invalidate_record(&mut self) {
        self.get_immutable_record_or_create().as_mut().unwrap().invalidate();
    }
    pub fn prev(&mut self) -> Result<CursorResult<bool>> {
        assert!(self.mv_cursor.is_none());
        return_if_io!(self.restore_context());
        let cursor_has_record = return_if_io!(self.get_prev_record());
        self.has_record.replace(cursor_has_record);
        self.invalidate_record();
        Ok(CursorResult::Ok(cursor_has_record))
    }
    #[instrument(skip(self), level = Level::TRACE)]
    pub fn rowid(&mut self) -> Result<CursorResult<Option<i64>>> {
        if let Some(mv_cursor) = &self.mv_cursor {
            let mv_cursor = mv_cursor.borrow();
            return Ok(
                CursorResult::Ok(mv_cursor.current_row_id().map(|rowid| rowid.row_id)),
            );
        }
        if self.has_record.get() {
            let page = self.stack.top();
            return_if_locked_maybe_load!(self.pager, page);
            let _ = return_if_io!(self.record());
            let page_type = page.get().get_contents().page_type();
            let page = page.get();
            let contents = page.get_contents();
            let cell_idx = self.stack.current_cell_index();
            let cell = contents
                .cell_get(
                    cell_idx as usize,
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
            if page_type.is_table() {
                let BTreeCell::TableLeafCell(TableLeafCell { _rowid, _payload, .. }) = cell
                else {
                    unreachable!("unexpected page_type");
                };
                Ok(CursorResult::Ok(Some(_rowid)))
            } else {
                Ok(CursorResult::Ok(self.get_index_rowid_from_record()))
            }
        } else {
            Ok(CursorResult::Ok(None))
        }
    }
    #[instrument(skip(self), level = Level::TRACE)]
    pub fn seek(&mut self, key: SeekKey<'_>, op: SeekOp) -> Result<CursorResult<bool>> {
        assert!(self.mv_cursor.is_none());
        tracing::trace!("");
        self.set_null_flag(false);
        let cursor_has_record = return_if_io!(self.do_seek(key, op));
        self.invalidate_record();
        self.seek_state = CursorSeekState::Start;
        self.valid_state = CursorValidState::Valid;
        self.has_record.replace(cursor_has_record);
        Ok(CursorResult::Ok(cursor_has_record))
    }
    /// Return a reference to the record the cursor is currently pointing to.
    /// If record was not parsed yet, then we have to parse it and in case of I/O we yield control
    /// back.
    #[instrument(skip(self), level = Level::TRACE)]
    pub fn record(&self) -> Result<CursorResult<Option<Ref<ImmutableRecord>>>> {
        if !self.has_record.get() {
            return Ok(CursorResult::Ok(None));
        }
        let invalidated = self
            .reusable_immutable_record
            .borrow()
            .as_ref()
            .map_or(true, |record| record.is_invalidated());
        if !invalidated {
            *self.parse_record_state.borrow_mut() = ParseRecordState::Init;
            let record_ref = Ref::filter_map(
                    self.reusable_immutable_record.borrow(),
                    |opt| opt.as_ref(),
                )
                .unwrap();
            return Ok(CursorResult::Ok(Some(record_ref)));
        }
        if *self.parse_record_state.borrow() == ParseRecordState::Init {
            *self.parse_record_state.borrow_mut() = ParseRecordState::Parsing {
                payload: Vec::new(),
            };
        }
        let page = self.stack.top();
        return_if_locked_maybe_load!(self.pager, page);
        let page = page.get();
        let contents = page.get_contents();
        let cell_idx = self.stack.current_cell_index();
        let cell = contents
            .cell_get(
                cell_idx as usize,
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
        let (payload, payload_size, first_overflow_page) = match cell {
            BTreeCell::TableLeafCell(
                TableLeafCell { _rowid, _payload, payload_size, first_overflow_page },
            ) => (_payload, payload_size, first_overflow_page),
            BTreeCell::IndexInteriorCell(
                IndexInteriorCell {
                    left_child_page: _,
                    payload,
                    payload_size,
                    first_overflow_page,
                },
            ) => (payload, payload_size, first_overflow_page),
            BTreeCell::IndexLeafCell(
                IndexLeafCell { payload, first_overflow_page, payload_size },
            ) => (payload, payload_size, first_overflow_page),
            _ => unreachable!("unexpected page_type"),
        };
        if let Some(next_page) = first_overflow_page {
            return_if_io!(self.process_overflow_read(payload, next_page, payload_size))
        } else {
            crate::storage::sqlite3_ondisk::read_record(
                payload,
                self.get_immutable_record_or_create().as_mut().unwrap(),
            )?
        };
        *self.parse_record_state.borrow_mut() = ParseRecordState::Init;
        let record_ref = Ref::filter_map(
                self.reusable_immutable_record.borrow(),
                |opt| opt.as_ref(),
            )
            .unwrap();
        Ok(CursorResult::Ok(Some(record_ref)))
    }
    #[instrument(skip(self), level = Level::TRACE)]
    pub fn insert(
        &mut self,
        key: &BTreeKey,
        mut moved_before: bool,
    ) -> Result<CursorResult<()>> {
        tracing::debug!(
            valid_state = ? self.valid_state, cursor_state = ? self.state,
            is_write_in_progress = self.is_write_in_progress()
        );
        match &self.mv_cursor {
            Some(mv_cursor) => {
                match key.maybe_rowid() {
                    Some(rowid) => {
                        let row_id = crate::mvcc::database::RowID::new(
                            self.table_id() as u64,
                            rowid,
                        );
                        let record_buf = key
                            .get_record()
                            .unwrap()
                            .get_payload()
                            .to_vec();
                        let row = crate::mvcc::database::Row::new(row_id, record_buf);
                        mv_cursor.borrow_mut().insert(row).unwrap();
                    }
                    None => todo!("Support mvcc inserts with index btrees"),
                }
            }
            None => {
                if self.valid_state != CursorValidState::Valid
                    && !self.is_write_in_progress()
                {
                    moved_before = false;
                }
                if !moved_before {
                    match key {
                        BTreeKey::IndexKey(_) => {
                            return_if_io!(
                                self.move_to(SeekKey::IndexKey(key.get_record().unwrap()),
                                SeekOp::GE { eq_only : true })
                            )
                        }
                        BTreeKey::TableRowId(_) => {
                            return_if_io!(
                                self.move_to(SeekKey::TableRowId(key.to_rowid()), SeekOp::GE
                                { eq_only : true })
                            )
                        }
                    };
                    self.context.take();
                    self.valid_state = CursorValidState::Valid;
                    self.seek_state = CursorSeekState::Start;
                    tracing::debug!(
                        "seeked to the right place, page is now {:?}", self.stack.top()
                        .get().get().id
                    );
                }
                return_if_io!(self.insert_into_page(key));
                if key.maybe_rowid().is_some() {
                    self.has_record.replace(true);
                }
            }
        };
        Ok(CursorResult::Ok(()))
    }
    /// Delete state machine flow:
    /// 1. Start -> check if the rowid to be delete is present in the page or not. If not we early return
    /// 2. DeterminePostBalancingSeekKey -> determine the key to seek to after balancing.
    /// 3. LoadPage -> load the page.
    /// 4. FindCell -> find the cell to be deleted in the page.
    /// 5. ClearOverflowPages -> Clear the overflow pages if there are any before dropping the cell, then if we are in a leaf page we just drop the cell in place.
    /// if we are in interior page, we need to rotate keys in order to replace current cell (InteriorNodeReplacement).
    /// 6. InteriorNodeReplacement -> we copy the left subtree leaf node into the deleted interior node's place.
    /// 7. WaitForBalancingToComplete -> perform balancing
    /// 8. SeekAfterBalancing -> adjust the cursor to a node that is closer to the deleted value. go to Finish
    /// 9. Finish -> Delete operation is done. Return CursorResult(Ok())
    #[instrument(skip(self), level = Level::TRACE)]
    pub fn delete(&mut self) -> Result<CursorResult<()>> {
        assert!(self.mv_cursor.is_none());
        if let CursorState::None = &self.state {
            self.state = CursorState::Delete(DeleteInfo {
                state: DeleteState::Start,
                balance_write_info: None,
            });
        }
        loop {
            let delete_state = {
                let delete_info = self
                    .state
                    .delete_info()
                    .expect("cannot get delete info");
                delete_info.state.clone()
            };
            tracing::debug!(? delete_state);
            match delete_state {
                DeleteState::Start => {
                    let page = self.stack.top();
                    page.get().set_dirty();
                    self.pager.add_dirty(page.get().get().id);
                    if matches!(
                        page.get().get_contents().page_type(), PageType::TableLeaf |
                        PageType::TableInterior
                    ) {
                        let _target_rowid = match return_if_io!(self.rowid()) {
                            Some(rowid) => rowid,
                            _ => {
                                self.state = CursorState::None;
                                return Ok(CursorResult::Ok(()));
                            }
                        };
                    } else {
                        if self.reusable_immutable_record.borrow().is_none() {
                            self.state = CursorState::None;
                            return Ok(CursorResult::Ok(()));
                        }
                    }
                    let delete_info = self.state.mut_delete_info().unwrap();
                    delete_info.state = DeleteState::DeterminePostBalancingSeekKey;
                }
                DeleteState::DeterminePostBalancingSeekKey => {
                    let page = self.stack.top();
                    return_if_locked_maybe_load!(self.pager, page);
                    let target_key = if page.get().is_index() {
                        let record = match return_if_io!(self.record()) {
                            Some(record) => record.clone(),
                            None => unreachable!("there should've been a record"),
                        };
                        DeleteSavepoint::Payload(record)
                    } else {
                        let Some(rowid) = return_if_io!(self.rowid()) else {
                            panic!("cursor should be pointing to a record with a rowid");
                        };
                        DeleteSavepoint::Rowid(rowid)
                    };
                    let delete_info = self.state.mut_delete_info().unwrap();
                    delete_info.state = DeleteState::LoadPage {
                        post_balancing_seek_key: Some(target_key),
                    };
                }
                DeleteState::LoadPage { post_balancing_seek_key } => {
                    let page = self.stack.top();
                    return_if_locked_maybe_load!(self.pager, page);
                    let delete_info = self.state.mut_delete_info().unwrap();
                    delete_info.state = DeleteState::FindCell {
                        post_balancing_seek_key,
                    };
                }
                DeleteState::FindCell { post_balancing_seek_key } => {
                    let page = self.stack.top();
                    let cell_idx = self.stack.current_cell_index() as usize;
                    let page = page.get();
                    let contents = page.get().contents.as_ref().unwrap();
                    if cell_idx >= contents.cell_count() {
                        return_corrupt!(
                            format!("Corrupted page: cell index {} is out of bounds for page with {} cells",
                            cell_idx, contents.cell_count())
                        );
                    }
                    tracing::debug!(
                        "DeleteState::FindCell: page_id: {}, cell_idx: {}", page.get()
                        .id, cell_idx
                    );
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
                    let original_child_pointer = match &cell {
                        BTreeCell::TableInteriorCell(interior) => {
                            Some(interior._left_child_page)
                        }
                        BTreeCell::IndexInteriorCell(interior) => {
                            Some(interior.left_child_page)
                        }
                        _ => None,
                    };
                    let delete_info = self.state.mut_delete_info().unwrap();
                    delete_info.state = DeleteState::ClearOverflowPages {
                        cell_idx,
                        cell,
                        original_child_pointer,
                        post_balancing_seek_key,
                    };
                }
                DeleteState::ClearOverflowPages {
                    cell_idx,
                    cell,
                    original_child_pointer,
                    post_balancing_seek_key,
                } => {
                    return_if_io!(self.clear_overflow_pages(& cell));
                    let page = self.stack.top();
                    let page = page.get();
                    let contents = page.get_contents();
                    let is_last_cell = cell_idx
                        == contents.cell_count().saturating_sub(1);
                    let delete_info = self.state.mut_delete_info().unwrap();
                    if !contents.is_leaf() {
                        delete_info.state = DeleteState::InteriorNodeReplacement {
                            cell_idx,
                            original_child_pointer,
                            post_balancing_seek_key,
                        };
                    } else {
                        let contents = page.get().contents.as_mut().unwrap();
                        drop_cell(contents, cell_idx, self.usable_space() as u16)?;
                        let delete_info = self.state.mut_delete_info().unwrap();
                        delete_info.state = DeleteState::CheckNeedsBalancing {
                            rightmost_cell_was_dropped: is_last_cell,
                            post_balancing_seek_key,
                        };
                    }
                }
                DeleteState::InteriorNodeReplacement {
                    cell_idx,
                    original_child_pointer,
                    post_balancing_seek_key,
                } => {
                    return_if_io!(self.prev());
                    let (cell_payload, leaf_cell_idx) = {
                        let leaf_page_ref = self.stack.top();
                        let leaf_page = leaf_page_ref.get();
                        let leaf_contents = leaf_page.get().contents.as_ref().unwrap();
                        assert!(leaf_contents.is_leaf());
                        assert!(leaf_contents.cell_count() > 0);
                        let leaf_cell_idx = leaf_contents.cell_count() - 1;
                        let last_cell_on_child_page = leaf_contents
                            .cell_get(
                                leaf_cell_idx,
                                payload_overflow_threshold_max(
                                    leaf_contents.page_type(),
                                    self.usable_space() as u16,
                                ),
                                payload_overflow_threshold_min(
                                    leaf_contents.page_type(),
                                    self.usable_space() as u16,
                                ),
                                self.usable_space(),
                            )?;
                        let mut cell_payload: Vec<u8> = Vec::new();
                        let child_pointer = original_child_pointer
                            .expect("there should be a pointer");
                        match last_cell_on_child_page {
                            BTreeCell::TableLeafCell(leaf_cell) => {
                                cell_payload
                                    .extend_from_slice(&child_pointer.to_be_bytes());
                                write_varint_to_vec(
                                    leaf_cell._rowid as u64,
                                    &mut cell_payload,
                                );
                            }
                            BTreeCell::IndexLeafCell(leaf_cell) => {
                                cell_payload
                                    .extend_from_slice(&child_pointer.to_be_bytes());
                                write_varint_to_vec(
                                    leaf_cell.payload_size,
                                    &mut cell_payload,
                                );
                                cell_payload.extend_from_slice(leaf_cell.payload);
                                if let Some(first_overflow_page) = leaf_cell
                                    .first_overflow_page
                                {
                                    cell_payload
                                        .extend_from_slice(&first_overflow_page.to_be_bytes());
                                }
                            }
                            _ => unreachable!("Expected table leaf cell"),
                        }
                        (cell_payload, leaf_cell_idx)
                    };
                    let parent_page = self.stack.parent_page().unwrap();
                    let leaf_page = self.stack.top();
                    parent_page.get().set_dirty();
                    self.pager.add_dirty(parent_page.get().get().id);
                    leaf_page.get().set_dirty();
                    self.pager.add_dirty(leaf_page.get().get().id);
                    {
                        let parent_page_ref = parent_page.get();
                        let parent_contents = parent_page_ref
                            .get()
                            .contents
                            .as_mut()
                            .unwrap();
                        drop_cell(
                            parent_contents,
                            cell_idx,
                            self.usable_space() as u16,
                        )?;
                        insert_into_cell(
                            parent_contents,
                            &cell_payload,
                            cell_idx,
                            self.usable_space() as u16,
                        )?;
                    }
                    {
                        let leaf_page_ref = leaf_page.get();
                        let leaf_contents = leaf_page_ref
                            .get()
                            .contents
                            .as_mut()
                            .unwrap();
                        drop_cell(
                            leaf_contents,
                            leaf_cell_idx,
                            self.usable_space() as u16,
                        )?;
                    }
                    let delete_info = self.state.mut_delete_info().unwrap();
                    delete_info.state = DeleteState::CheckNeedsBalancing {
                        rightmost_cell_was_dropped: false,
                        post_balancing_seek_key,
                    };
                }
                DeleteState::CheckNeedsBalancing {
                    rightmost_cell_was_dropped,
                    post_balancing_seek_key,
                } => {
                    let page = self.stack.top();
                    return_if_locked_maybe_load!(self.pager, page);
                    let page = page.get();
                    let contents = page.get().contents.as_ref().unwrap();
                    let free_space = compute_free_space(
                        contents,
                        self.usable_space() as u16,
                    )?;
                    let needs_balancing = self.stack.has_parent()
                        && free_space as usize * 3 > self.usable_space() * 2;
                    if rightmost_cell_was_dropped {
                        self.stack.retreat();
                    }
                    if needs_balancing {
                        let delete_info = self.state.mut_delete_info().unwrap();
                        if delete_info.balance_write_info.is_none() {
                            let mut write_info = WriteInfo::new();
                            write_info.state = WriteState::BalanceStart;
                            delete_info.balance_write_info = Some(write_info);
                        }
                        delete_info.state = DeleteState::WaitForBalancingToComplete {
                            target_key: post_balancing_seek_key.unwrap(),
                        };
                    } else {
                        self.stack.retreat();
                        self.state = CursorState::None;
                        return Ok(CursorResult::Ok(()));
                    }
                }
                DeleteState::WaitForBalancingToComplete { target_key } => {
                    let delete_info = self.state.mut_delete_info().unwrap();
                    let write_info = delete_info.balance_write_info.take().unwrap();
                    self.state = CursorState::Write(write_info);
                    match self.balance()? {
                        CursorResult::Ok(()) => {
                            let write_info = match &self.state {
                                CursorState::Write(wi) => wi.clone(),
                                _ => unreachable!("Balance operation changed cursor state"),
                            };
                            self.state = CursorState::Delete(DeleteInfo {
                                state: DeleteState::SeekAfterBalancing {
                                    target_key,
                                },
                                balance_write_info: Some(write_info),
                            });
                        }
                        CursorResult::IO => {
                            let write_info = match &self.state {
                                CursorState::Write(wi) => wi.clone(),
                                _ => unreachable!("Balance operation changed cursor state"),
                            };
                            self.state = CursorState::Delete(DeleteInfo {
                                state: DeleteState::WaitForBalancingToComplete {
                                    target_key,
                                },
                                balance_write_info: Some(write_info),
                            });
                            return Ok(CursorResult::IO);
                        }
                    }
                }
                DeleteState::SeekAfterBalancing { target_key } => {
                    let key = match &target_key {
                        DeleteSavepoint::Rowid(rowid) => SeekKey::TableRowId(*rowid),
                        DeleteSavepoint::Payload(immutable_record) => {
                            SeekKey::IndexKey(immutable_record)
                        }
                    };
                    return_if_io!(self.seek(key, SeekOp::LT));
                    self.state = CursorState::None;
                    return Ok(CursorResult::Ok(()));
                }
            }
        }
    }
    /// In outer joins, whenever the right-side table has no matching row, the query must still return a row
    /// for each left-side row. In order to achieve this, we set the null flag on the right-side table cursor
    /// so that it returns NULL for all columns until cleared.
    #[inline(always)]
    pub fn set_null_flag(&mut self, flag: bool) {
        self.null_flag = flag;
    }
    #[inline(always)]
    pub fn get_null_flag(&self) -> bool {
        self.null_flag
    }
    /// Search for a key in an Index Btree. Looking up indexes that need to be unique, we cannot compare the rowid
    pub fn key_exists_in_index(
        &mut self,
        key: &ImmutableRecord,
    ) -> Result<CursorResult<bool>> {
        return_if_io!(self.seek(SeekKey::IndexKey(key), SeekOp::GE { eq_only : true }));
        let record_opt = return_if_io!(self.record());
        match record_opt.as_ref() {
            Some(record) => {
                let existing_key = &record
                    .get_values()[..record.count().saturating_sub(1)];
                let inserted_key_vals = &key.get_values();
                if existing_key.len() != inserted_key_vals.len() {
                    return Ok(CursorResult::Ok(false));
                }
                Ok(
                    CursorResult::Ok(
                        existing_key
                            .iter()
                            .zip(inserted_key_vals.iter())
                            .all(|(a, b)| a == b),
                    ),
                )
            }
            None => Ok(CursorResult::Ok(false)),
        }
    }
    pub fn exists(&mut self, key: &Value) -> Result<CursorResult<bool>> {
        assert!(self.mv_cursor.is_none());
        let int_key = match key {
            Value::Integer(i) => i,
            _ => unreachable!("btree tables are indexed by integers!"),
        };
        let has_record = return_if_io!(
            self.seek(SeekKey::TableRowId(* int_key), SeekOp::GE { eq_only : true })
        );
        self.has_record.set(has_record);
        self.invalidate_record();
        Ok(CursorResult::Ok(has_record))
    }
    /// Clear the overflow pages linked to a specific page provided by the leaf cell
    /// Uses a state machine to keep track of it's operations so that traversal can be
    /// resumed from last point after IO interruption
    fn clear_overflow_pages(&mut self, cell: &BTreeCell) -> Result<CursorResult<()>> {
        loop {
            let state = self.overflow_state.take().unwrap_or(OverflowState::Start);
            match state {
                OverflowState::Start => {
                    let first_overflow_page = match cell {
                        BTreeCell::TableLeafCell(leaf_cell) => {
                            leaf_cell.first_overflow_page
                        }
                        BTreeCell::IndexLeafCell(leaf_cell) => {
                            leaf_cell.first_overflow_page
                        }
                        BTreeCell::IndexInteriorCell(interior_cell) => {
                            interior_cell.first_overflow_page
                        }
                        BTreeCell::TableInteriorCell(_) => {
                            return Ok(CursorResult::Ok(()));
                        }
                    };
                    if let Some(page) = first_overflow_page {
                        self.overflow_state = Some(OverflowState::ProcessPage {
                            next_page: page,
                        });
                        continue;
                    } else {
                        self.overflow_state = Some(OverflowState::Done);
                    }
                }
                OverflowState::ProcessPage { next_page } => {
                    if next_page < 2
                        || next_page as usize
                            > self.pager.db_header.lock().database_size as usize
                    {
                        self.overflow_state = None;
                        return Err(
                            LimboError::Corrupt("Invalid overflow page number".into()),
                        );
                    }
                    let page = self.read_page(next_page as usize)?;
                    return_if_locked_maybe_load!(self.pager, page);
                    let page = page.get();
                    let contents = page.get().contents.as_ref().unwrap();
                    let next = contents.read_u32(0);
                    self.pager.free_page(Some(page), next_page as usize)?;
                    if next != 0 {
                        self.overflow_state = Some(OverflowState::ProcessPage {
                            next_page: next,
                        });
                    } else {
                        self.overflow_state = Some(OverflowState::Done);
                    }
                }
                OverflowState::Done => {
                    self.overflow_state = None;
                    return Ok(CursorResult::Ok(()));
                }
            };
        }
    }
    /// Destroys a B-tree by freeing all its pages in an iterative depth-first order.
    /// This ensures child pages are freed before their parents
    /// Uses a state machine to keep track of the operation to ensure IO doesn't cause repeated traversals
    ///
    /// # Example
    /// For a B-tree with this structure (where 4' is an overflow page):
    /// ```text
    ///            1 (root)
    ///           /        \
    ///          2          3
    ///        /   \      /   \
    /// 4' <- 4     5    6     7
    /// ```
    ///
    /// The destruction order would be: [4',4,5,2,6,7,3,1]
    #[instrument(skip(self), level = Level::TRACE)]
    pub fn btree_destroy(&mut self) -> Result<CursorResult<Option<usize>>> {
        if let CursorState::None = &self.state {
            self.move_to_root();
            self.state = CursorState::Destroy(DestroyInfo {
                state: DestroyState::Start,
            });
        }
        loop {
            let destroy_state = {
                let destroy_info = self
                    .state
                    .destroy_info()
                    .expect("unable to get a mut reference to destroy state in cursor");
                destroy_info.state.clone()
            };
            match destroy_state {
                DestroyState::Start => {
                    let destroy_info = self
                        .state
                        .mut_destroy_info()
                        .expect(
                            "unable to get a mut reference to destroy state in cursor",
                        );
                    destroy_info.state = DestroyState::LoadPage;
                }
                DestroyState::LoadPage => {
                    let page = self.stack.top();
                    return_if_locked_maybe_load!(self.pager, page);
                    let destroy_info = self
                        .state
                        .mut_destroy_info()
                        .expect(
                            "unable to get a mut reference to destroy state in cursor",
                        );
                    destroy_info.state = DestroyState::ProcessPage;
                }
                DestroyState::ProcessPage => {
                    let page = self.stack.top();
                    self.stack.advance();
                    assert!(page.get().is_loaded());
                    let page = page.get();
                    let contents = page.get().contents.as_ref().unwrap();
                    let cell_idx = self.stack.current_cell_index();
                    if cell_idx >= contents.cell_count() as i32 {
                        match (contents.is_leaf(), cell_idx) {
                            (true, n) if n >= contents.cell_count() as i32 => {
                                let destroy_info = self
                                    .state
                                    .mut_destroy_info()
                                    .expect(
                                        "unable to get a mut reference to destroy state in cursor",
                                    );
                                destroy_info.state = DestroyState::FreePage;
                                continue;
                            }
                            (false, n) if n == contents.cell_count() as i32 => {
                                if let Some(rightmost) = contents.rightmost_pointer() {
                                    let rightmost_page = self.read_page(rightmost as usize)?;
                                    self.stack.push(rightmost_page);
                                    let destroy_info = self
                                        .state
                                        .mut_destroy_info()
                                        .expect(
                                            "unable to get a mut reference to destroy state in cursor",
                                        );
                                    destroy_info.state = DestroyState::LoadPage;
                                } else {
                                    let destroy_info = self
                                        .state
                                        .mut_destroy_info()
                                        .expect(
                                            "unable to get a mut reference to destroy state in cursor",
                                        );
                                    destroy_info.state = DestroyState::FreePage;
                                }
                                continue;
                            }
                            (false, n) if n > contents.cell_count() as i32 => {
                                let destroy_info = self
                                    .state
                                    .mut_destroy_info()
                                    .expect(
                                        "unable to get a mut reference to destroy state in cursor",
                                    );
                                destroy_info.state = DestroyState::FreePage;
                                continue;
                            }
                            _ => unreachable!("Invalid cell idx state"),
                        }
                    }
                    let cell = contents
                        .cell_get(
                            cell_idx as usize,
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
                    match contents.is_leaf() {
                        true => {
                            let destroy_info = self
                                .state
                                .mut_destroy_info()
                                .expect(
                                    "unable to get a mut reference to destroy state in cursor",
                                );
                            destroy_info.state = DestroyState::ClearOverflowPages {
                                cell,
                            };
                            continue;
                        }
                        false => {
                            match &cell {
                                BTreeCell::IndexInteriorCell(_) => {
                                    let destroy_info = self
                                        .state
                                        .mut_destroy_info()
                                        .expect(
                                            "unable to get a mut reference to destroy state in cursor",
                                        );
                                    destroy_info.state = DestroyState::ClearOverflowPages {
                                        cell,
                                    };
                                    continue;
                                }
                                _ => {
                                    let child_page_id = match &cell {
                                        BTreeCell::TableInteriorCell(cell) => cell._left_child_page,
                                        BTreeCell::IndexInteriorCell(cell) => cell.left_child_page,
                                        _ => panic!("expected interior cell"),
                                    };
                                    let child_page = self.read_page(child_page_id as usize)?;
                                    self.stack.push(child_page);
                                    let destroy_info = self
                                        .state
                                        .mut_destroy_info()
                                        .expect(
                                            "unable to get a mut reference to destroy state in cursor",
                                        );
                                    destroy_info.state = DestroyState::LoadPage;
                                    continue;
                                }
                            }
                        }
                    }
                }
                DestroyState::ClearOverflowPages { cell } => {
                    match self.clear_overflow_pages(&cell)? {
                        CursorResult::Ok(_) => {
                            match cell {
                                BTreeCell::IndexInteriorCell(index_int_cell) => {
                                    let child_page = self
                                        .read_page(index_int_cell.left_child_page as usize)?;
                                    self.stack.push(child_page);
                                    let destroy_info = self
                                        .state
                                        .mut_destroy_info()
                                        .expect(
                                            "unable to get a mut reference to destroy state in cursor",
                                        );
                                    destroy_info.state = DestroyState::LoadPage;
                                    continue;
                                }
                                BTreeCell::TableLeafCell(_)
                                | BTreeCell::IndexLeafCell(_) => {
                                    let destroy_info = self
                                        .state
                                        .mut_destroy_info()
                                        .expect(
                                            "unable to get a mut reference to destroy state in cursor",
                                        );
                                    destroy_info.state = DestroyState::LoadPage;
                                }
                                _ => panic!("unexpected cell type"),
                            }
                        }
                        CursorResult::IO => return Ok(CursorResult::IO),
                    }
                }
                DestroyState::FreePage => {
                    let page = self.stack.top();
                    let page_id = page.get().get().id;
                    self.pager.free_page(Some(page.get()), page_id)?;
                    if self.stack.has_parent() {
                        self.stack.pop();
                        let destroy_info = self
                            .state
                            .mut_destroy_info()
                            .expect(
                                "unable to get a mut reference to destroy state in cursor",
                            );
                        destroy_info.state = DestroyState::ProcessPage;
                    } else {
                        self.state = CursorState::None;
                        return Ok(CursorResult::Ok(None));
                    }
                }
            }
        }
    }
    pub fn table_id(&self) -> usize {
        self.root_page
    }
    pub fn overwrite_cell(
        &mut self,
        page_ref: BTreePage,
        cell_idx: usize,
        record: &ImmutableRecord,
    ) -> Result<CursorResult<()>> {
        let page_type = page_ref.get().get().contents.as_ref().unwrap().page_type();
        let mut new_payload = Vec::with_capacity(record.len());
        let rowid = return_if_io!(self.rowid());
        fill_cell_payload(
            page_type,
            rowid,
            &mut new_payload,
            record,
            self.usable_space() as u16,
            self.pager.clone(),
        )?;
        let (old_offset, old_local_size) = {
            let page_ref = page_ref.get();
            let page = page_ref.get().contents.as_ref().unwrap();
            page.cell_get_raw_region(
                cell_idx,
                payload_overflow_threshold_max(page_type, self.usable_space() as u16),
                payload_overflow_threshold_min(page_type, self.usable_space() as u16),
                self.usable_space(),
            )?
        };
        if new_payload.len() == old_local_size {
            self.overwrite_content(page_ref.clone(), old_offset, &new_payload)?;
            Ok(CursorResult::Ok(()))
        } else {
            drop_cell(
                page_ref.get().get_contents_mut(),
                cell_idx,
                self.usable_space() as u16,
            )?;
            insert_into_cell(
                page_ref.get().get_contents_mut(),
                &new_payload,
                cell_idx,
                self.usable_space() as u16,
            )?;
            Ok(CursorResult::Ok(()))
        }
    }
    pub fn overwrite_content(
        &mut self,
        page_ref: BTreePage,
        dest_offset: usize,
        new_payload: &[u8],
    ) -> Result<CursorResult<()>> {
        return_if_locked!(page_ref.get());
        let page_ref = page_ref.get();
        let buf = page_ref.get().contents.as_mut().unwrap().as_ptr();
        buf[dest_offset..dest_offset + new_payload.len()].copy_from_slice(&new_payload);
        Ok(CursorResult::Ok(()))
    }
    fn get_immutable_record_or_create(
        &self,
    ) -> std::cell::RefMut<'_, Option<ImmutableRecord>> {
        if self.reusable_immutable_record.borrow().is_none() {
            let record = ImmutableRecord::new(4096, 10);
            self.reusable_immutable_record.replace(Some(record));
        }
        self.reusable_immutable_record.borrow_mut()
    }
    fn get_immutable_record(&self) -> std::cell::RefMut<'_, Option<ImmutableRecord>> {
        self.reusable_immutable_record.borrow_mut()
    }
    pub fn is_write_in_progress(&self) -> bool {
        match self.state {
            CursorState::Write(_) => true,
            _ => false,
        }
    }
    /// Count the number of entries in the b-tree
    ///
    /// Only supposed to be used in the context of a simple Count Select Statement
    #[instrument(skip(self), level = Level::TRACE)]
    pub fn count(&mut self) -> Result<CursorResult<usize>> {
        if self.count == 0 {
            self.move_to_root();
        }
        if let Some(mv_cursor) = &self.mv_cursor {
            // The MVCC scan cursor eagerly collects the full set of row ids for
            // this table in its constructor (see `ScanCursor::new` /
            // `scan_row_ids_for_table`), so the count is simply its length.
            // Like the on-disk-btree path below, this does not filter by MVCC
            // visibility (`is_visible_to`) -- neither path in this cursor does,
            // so this keeps the same correctness ceiling as the rest of the
            // MVCC read path rather than introducing a new, inconsistent
            // semantic (visibility-aware counting would undercount compared to
            // every other MVCC read here).
            return Ok(CursorResult::Ok(mv_cursor.borrow().row_ids.len()));
        }
        let mut mem_page_rc;
        let mut mem_page;
        let mut contents;
        loop {
            mem_page_rc = self.stack.top();
            return_if_locked_maybe_load!(self.pager, mem_page_rc);
            mem_page = mem_page_rc.get();
            contents = mem_page.get().contents.as_ref().unwrap();
            if !matches!(contents.page_type(), PageType::TableInterior) {
                self.count += contents.cell_count();
            }
            self.stack.advance();
            let cell_idx = self.stack.current_cell_index() as usize;
            if contents.is_leaf() || cell_idx > contents.cell_count() {
                loop {
                    if !self.stack.has_parent() {
                        self.move_to_root();
                        return Ok(CursorResult::Ok(self.count));
                    }
                    self.stack.pop();
                    mem_page_rc = self.stack.top();
                    return_if_locked_maybe_load!(self.pager, mem_page_rc);
                    mem_page = mem_page_rc.get();
                    contents = mem_page.get().contents.as_ref().unwrap();
                    let cell_idx = self.stack.current_cell_index() as usize;
                    if cell_idx <= contents.cell_count() {
                        break;
                    }
                }
            }
            let cell_idx = self.stack.current_cell_index() as usize;
            assert!(cell_idx <= contents.cell_count(),);
            assert!(! contents.is_leaf());
            if cell_idx == contents.cell_count() {
                let right_most_pointer = contents.rightmost_pointer().unwrap();
                self.stack.advance();
                let mem_page = self.read_page(right_most_pointer as usize)?;
                self.stack.push(mem_page);
            } else {
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
                        TableInteriorCell { _left_child_page: left_child_page, .. },
                    )
                    | BTreeCell::IndexInteriorCell(
                        IndexInteriorCell { left_child_page, .. },
                    ) => {
                        self.stack.advance();
                        let mem_page = self.read_page(left_child_page as usize)?;
                        self.stack.push(mem_page);
                    }
                    _ => unreachable!(),
                }
            }
        }
    }
    /// Walk every entry of the b-tree to compute the data needed for an
    /// `sqlite_stat1` row: the total number of entries `N` and, for an index
    /// over `num_cols` leading key columns, a `distinct` vector where
    /// `distinct[i]` is the number of times the prefix of length `i + 1`
    /// changed (so the number of distinct prefixes is `distinct[i] + 1`).
    ///
    /// `num_cols == 0` denotes a plain table walk: only `N` is computed and the
    /// returned `distinct` vector is empty.
    ///
    /// The walk is resumable: if any underlying I/O yields, the accumulated
    /// state is stashed on the cursor and `CursorResult::IO` is returned so the
    /// VDBE can re-enter this method until it completes.
    pub fn index_stat(
        &mut self,
        num_cols: usize,
    ) -> Result<CursorResult<(i64, Vec<i64>)>> {
        if self.mv_cursor.is_some() {
            return Err(
                LimboError::InternalError(
                    "ANALYZE not supported on MVCC cursors yet".to_string(),
                ),
            );
        }
        let mut walk = self
            .analyze_walk
            .take()
            .unwrap_or_else(|| AnalyzeWalk {
                phase: AnalyzePhase::Init,
                n: 0,
                distinct: vec![0i64; num_cols],
                prev_key: Vec::new(),
                num_cols,
            });
        loop {
            match walk.phase {
                AnalyzePhase::Init => {
                    walk.phase = AnalyzePhase::Rewind;
                }
                AnalyzePhase::Rewind => {
                    match self.rewind()? {
                        CursorResult::IO => {
                            self.analyze_walk = Some(walk);
                            return Ok(CursorResult::IO);
                        }
                        CursorResult::Ok(()) => {
                            walk.phase = AnalyzePhase::Read;
                        }
                    }
                }
                AnalyzePhase::Read => {
                    if self.is_empty() {
                        walk.phase = AnalyzePhase::Done;
                        continue;
                    }
                    if walk.num_cols > 0 {
                        let num_cols = walk.num_cols;
                        let key_result: CursorResult<Option<Vec<Value>>> = match self
                            .record()?
                        {
                            CursorResult::IO => CursorResult::IO,
                            CursorResult::Ok(opt) => {
                                CursorResult::Ok(
                                    opt
                                        .map(|record| {
                                            record
                                                .get_values()
                                                .iter()
                                                .take(num_cols)
                                                .map(|rv| rv.to_owned())
                                                .collect()
                                        }),
                                )
                            }
                        };
                        let cur_key = match key_result {
                            CursorResult::IO => {
                                self.analyze_walk = Some(walk);
                                return Ok(CursorResult::IO);
                            }
                            CursorResult::Ok(None) => {
                                walk.phase = AnalyzePhase::Done;
                                continue;
                            }
                            CursorResult::Ok(Some(key)) => key,
                        };
                        if walk.n > 0 {
                            let change_pos = first_change(
                                &walk.prev_key,
                                &cur_key,
                                walk.num_cols,
                            );
                            for dist in walk.distinct.iter_mut().skip(change_pos) {
                                *dist += 1;
                            }
                        }
                        walk.prev_key = cur_key;
                    }
                    walk.n += 1;
                    walk.phase = AnalyzePhase::Advance;
                }
                AnalyzePhase::Advance => {
                    match self.next()? {
                        CursorResult::IO => {
                            self.analyze_walk = Some(walk);
                            return Ok(CursorResult::IO);
                        }
                        CursorResult::Ok(has_record) => {
                            walk.phase = if has_record {
                                AnalyzePhase::Read
                            } else {
                                AnalyzePhase::Done
                            };
                        }
                    }
                }
                AnalyzePhase::Done => {
                    self.analyze_walk = None;
                    return Ok(CursorResult::Ok((walk.n, walk.distinct)));
                }
            }
        }
    }
    pub fn save_context(&mut self, cursor_context: CursorContext) {
        self.valid_state = CursorValidState::RequireSeek;
        self.context = Some(cursor_context);
    }
    /// If context is defined, restore it and set it None on success
    fn restore_context(&mut self) -> Result<CursorResult<()>> {
        if self.context.is_none()
            || !matches!(self.valid_state, CursorValidState::RequireSeek)
        {
            return Ok(CursorResult::Ok(()));
        }
        let ctx = self.context.take().unwrap();
        let seek_key = match ctx {
            CursorContext::TableRowId(rowid) => SeekKey::TableRowId(rowid),
            CursorContext::IndexKeyRowId(ref record) => SeekKey::IndexKey(record),
        };
        let res = self.seek(seek_key, SeekOp::GE { eq_only: true })?;
        match res {
            CursorResult::Ok(_) => {
                self.valid_state = CursorValidState::Valid;
                Ok(CursorResult::Ok(()))
            }
            CursorResult::IO => {
                self.context = Some(ctx);
                Ok(CursorResult::IO)
            }
        }
    }
    pub fn collations(&self) -> &[CollationSeq] {
        &self.collations
    }
    pub fn read_page(&self, page_idx: usize) -> Result<BTreePage> {
        btree_read_page(&self.pager, page_idx)
    }
    pub fn allocate_page(&self, page_type: PageType, offset: usize) -> Result<BTreePage> {
        self.pager.do_allocate_page(page_type, offset, BtreePageAllocMode::Any)
    }
}
