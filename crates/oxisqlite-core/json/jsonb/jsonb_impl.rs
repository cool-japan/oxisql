//! # `Jsonb` - from_str_group Methods
//!
//! This module contains method implementations for `Jsonb`.
//!
//! 🤖 Generated with [SplitRS](https://github.com/cool-japan/splitrs)

use super::super::path::{JsonPath, PathElement};
use crate::json::error::{Error as PError, Result as PResult};
use crate::{bail_parse_error, LimboError, Result};
use std::{
    borrow::Cow,
    collections::{HashMap, VecDeque},
    str::from_utf8_unchecked,
};

use super::functions::{compare, skip_whitespace, unescape_string, PathOperation};
use super::types::{
    ArrayPositionKind, DeleteOperation, ElementType, JsonLocationKind, JsonTraversalResult,
    JsonbHeader, PathOperationMode, ReplaceOperation, SegmentVariant, SetOperation,
};

use super::jsonb_type::Jsonb;

impl Jsonb {
    pub(super) fn from_str(input: &str) -> PResult<Self> {
        let mut result = Self::new(input.len(), None);
        let input = input.as_bytes();

        if input.is_empty() {
            return Err(PError::Message {
                msg: "Unexpected input after json".to_string(),
                location: None,
            });
        }

        // Parse the first complete JSON value
        let mut pos = 0;
        pos = result.deserialize_value(input, pos, 0)?;

        // Skip any trailing whitespace
        pos = skip_whitespace(input, pos);

        // Check for any non-whitespace characters after the JSON value
        if pos < input.len() {
            return Err(PError::Message {
                msg: "Unexpected input after json".to_string(),
                location: Some(pos),
            });
        }

        Ok(result)
    }

    pub fn from_raw_data(data: &[u8]) -> Self {
        Self::new(data.len(), Some(data))
    }

    pub fn data(self) -> Vec<u8> {
        self.data
    }

    pub fn element_type_at(&self, idx: usize) -> Result<ElementType> {
        let (JsonbHeader(element_type, _), _) = self.read_header(idx)?;
        Ok(element_type)
    }

    pub fn array_len(&self) -> Result<usize> {
        let (header, header_skip) = self.read_header(0)?;
        if header.0 != ElementType::ARRAY {
            return Ok(0);
        }

        let mut count = 0;
        let mut pos = header_skip;
        while pos < header_skip + header.1 {
            pos = self.skip_element(pos)?;
            count += 1;
        }

        Ok(count)
    }

    pub fn navigate_path(
        &mut self,
        path: &JsonPath,
        mode: PathOperationMode,
    ) -> Result<Vec<JsonTraversalResult>> {
        let mut path_iter = path.elements.iter().peekable();
        let mut pos = 0;
        let mut stack: Vec<JsonTraversalResult> = Vec::with_capacity(path.elements.len());

        while let Some(current) = path_iter.next() {
            let next_is_array = matches!(path_iter.peek(), Some(PathElement::ArrayLocator(_)))
                && !matches!(current, PathElement::ArrayLocator(_));

            let result = if next_is_array {
                let array_locator = path_iter.next().unwrap();

                self.navigate_to_segment(
                    SegmentVariant::KeyWithArrayIndex(current, array_locator),
                    pos,
                    mode,
                )?
            } else {
                self.navigate_to_segment(SegmentVariant::Single(current), pos, mode)?
            };

            pos = match &result.array_position_info {
                Some(ArrayPositionKind::SpecificIndex(idx)) => *idx,
                None => result.field_value_index,
            };

            stack.push(result);
        }

        Ok(stack)
    }

    pub fn operate_on_path<T>(&mut self, path: &JsonPath, operation: &mut T) -> Result<()>
    where
        T: PathOperation,
    {
        let mode = operation.operation_mode();

        let stack = self.navigate_path(path, mode)?;

        operation.execute(self, stack)?;

        Ok(())
    }

    pub(super) fn update_parent_references(
        &mut self,
        stack: Vec<JsonTraversalResult>,
        delta: isize,
    ) -> Result<()> {
        let mut delta = delta;
        let mut is_prev_arr = false;
        for parent in stack.iter().rev() {
            let (JsonbHeader(el_type, el_size), el_header_len) =
                self.read_header(parent.field_value_index)?;

            if el_type == ElementType::ARRAY && !is_prev_arr {
                is_prev_arr = true;
                let arr_element_idx = parent.get_array_index().unwrap();
                let (JsonbHeader(arr_el_type, arr_el_size), arr_el_header_len) =
                    self.read_header(arr_element_idx)?;

                let new_arr_el_header_len = self.write_element_header(
                    arr_element_idx,
                    arr_el_type,
                    (arr_el_size as isize + delta) as usize,
                    true,
                )?;

                delta += (new_arr_el_header_len - arr_el_header_len) as isize;
            } else {
                is_prev_arr = false;
            }
            let new_size = el_size as isize + delta;
            let new_header_size = self.write_element_header(
                parent.field_value_index,
                el_type,
                new_size as usize,
                true,
            )?;

            let header_diff = new_header_size as isize - el_header_len as isize;

            delta += parent.delta;
            delta += header_diff;
        }

        Ok(())
    }

    fn navigate_to_segment(
        &mut self,
        segment: SegmentVariant,
        mut pos: usize,
        mode: PathOperationMode,
    ) -> Result<JsonTraversalResult> {
        let (JsonbHeader(element_type, element_size), header_size) = self.read_header(pos)?;

        match segment {
            SegmentVariant::Single(PathElement::Root()) => {
                return Ok(JsonTraversalResult::new(
                    pos,
                    JsonLocationKind::DocumentRoot,
                    0,
                ));
            }
            SegmentVariant::Single(PathElement::ArrayLocator(idx)) => {
                let (JsonbHeader(root_type, root_size), root_header_size) =
                    self.read_header(pos)?;

                if root_type == ElementType::ARRAY {
                    let end_pos = pos + root_header_size + root_size;

                    match idx {
                        Some(idx) if *idx >= 0 => {
                            let mut count = 0;
                            let mut arr_pos = pos + root_header_size;

                            while arr_pos < end_pos && count != *idx as usize {
                                arr_pos = self.skip_element(arr_pos)?;
                                count += 1;
                            }

                            if mode.allows_insert() && arr_pos == end_pos {
                                let placeholder =
                                    JsonbHeader::new(ElementType::OBJECT, 0).into_bytes();
                                let placeholder_bytes = placeholder.as_bytes();

                                self.data
                                    .splice(arr_pos..arr_pos, placeholder_bytes.iter().copied());

                                return Ok(JsonTraversalResult::with_array_index(
                                    pos + root_header_size,
                                    JsonLocationKind::ArrayEntry,
                                    placeholder_bytes.len() as isize,
                                    arr_pos,
                                ));
                            }

                            if arr_pos != end_pos && mode.allows_replace() {
                                return Ok(JsonTraversalResult::with_array_index(
                                    pos,
                                    JsonLocationKind::ArrayEntry,
                                    0,
                                    arr_pos,
                                ));
                            }

                            bail_parse_error!("Not found!");
                        }
                        Some(idx) if *idx < 0 => {
                            let mut idx_map: HashMap<i32, usize> = HashMap::with_capacity(100);
                            let mut element_idx = 0;
                            let mut arr_pos = pos + root_header_size;

                            while arr_pos < end_pos {
                                idx_map.insert(element_idx, arr_pos);
                                arr_pos = self.skip_element(arr_pos)?;
                                element_idx += 1;
                            }

                            let real_idx = element_idx + idx;

                            if let Some(index) = idx_map.get(&real_idx) {
                                return Ok(JsonTraversalResult::with_array_index(
                                    pos,
                                    JsonLocationKind::ArrayEntry,
                                    0,
                                    *index,
                                ));
                            } else {
                                bail_parse_error!("Element with negative index not found")
                            }
                        }
                        _ => unreachable!(),
                    }
                } else {
                    if root_type == ElementType::OBJECT
                        && root_size == 0
                        && (*idx == Some(0) || *idx == None)
                        && mode.allows_insert()
                    {
                        let array = JsonbHeader::new(ElementType::ARRAY, 0).into_bytes();
                        let array_bytes = array.as_bytes();
                        let placeholder = JsonbHeader::new(ElementType::OBJECT, 0).into_bytes();
                        let placeholder_bytes = placeholder.as_bytes();
                        self.data.splice(
                            pos..pos + root_header_size,
                            array_bytes
                                .iter()
                                .copied()
                                .chain(placeholder_bytes.iter().copied()),
                        );

                        return Ok(JsonTraversalResult::with_array_index(
                            pos,
                            JsonLocationKind::ArrayEntry,
                            placeholder_bytes.len() as isize,
                            pos + array_bytes.len(),
                        ));
                    };
                    bail_parse_error!("Root is not an array");
                }
            }
            SegmentVariant::Single(PathElement::Key(path_key, is_raw)) => {
                if element_type == ElementType::OBJECT {
                    let end_pos = pos + element_size + header_size;

                    pos += header_size;

                    while pos < end_pos {
                        let (JsonbHeader(key_type, key_len), key_header_len) =
                            self.read_header(pos)?;

                        if !key_type.is_valid_key() {
                            bail_parse_error!("Key should be string");
                        }

                        let key_start = pos + key_header_len;
                        let json_key = unsafe {
                            from_utf8_unchecked(&self.data[key_start..key_start + key_len])
                        };

                        if compare((json_key, key_type), (path_key, *is_raw)) {
                            if mode.allows_replace() {
                                let value_pos = pos + key_header_len + key_len;
                                let key_pos = pos;

                                return Ok(JsonTraversalResult::new(
                                    value_pos,
                                    JsonLocationKind::ObjectProperty(key_pos),
                                    0,
                                ));
                            } else {
                                bail_parse_error!("Cant replace")
                            }
                        } else {
                            pos += key_header_len + key_len;
                            pos = self.skip_element(pos)?;
                        }
                    }

                    if mode.allows_insert() {
                        let key_type = if *is_raw {
                            ElementType::TEXTRAW
                        } else {
                            ElementType::TEXT
                        };

                        let key_header = JsonbHeader::new(key_type, path_key.len()).into_bytes();
                        let key_header_bytes = key_header.as_bytes();
                        let key_bytes = path_key.as_bytes();
                        let value_header = JsonbHeader::new(ElementType::OBJECT, 0).into_bytes();
                        let value_header_bytes = value_header.as_bytes();

                        self.data.splice(
                            pos..pos,
                            key_header_bytes
                                .iter()
                                .copied()
                                .chain(key_bytes.iter().copied())
                                .chain(value_header_bytes.iter().copied()),
                        );

                        let key_idx = pos;
                        let value_idx = pos + key_header_bytes.len() + key_bytes.len();
                        let delta =
                            key_header_bytes.len() + key_bytes.len() + value_header_bytes.len();

                        return Ok(JsonTraversalResult::new(
                            value_idx,
                            JsonLocationKind::ObjectProperty(key_idx),
                            delta as isize,
                        ));
                    } else {
                        bail_parse_error!("Mode does not allow insert cannot create new key!")
                    }
                } else {
                    bail_parse_error!("Looks like this is noop");
                }
            }
            SegmentVariant::KeyWithArrayIndex(
                PathElement::Root(),
                PathElement::ArrayLocator(idx),
            ) => {
                let (JsonbHeader(root_type, root_size), root_header_size) =
                    self.read_header(pos)?;

                if root_type == ElementType::ARRAY {
                    let end_pos = pos + root_header_size + root_size;

                    match idx {
                        Some(idx) if *idx >= 0 => {
                            let mut count = 0;
                            let mut arr_pos = pos + root_header_size;

                            while arr_pos < end_pos && count != *idx as usize {
                                arr_pos = self.skip_element(arr_pos)?;
                                count += 1;
                            }

                            if mode.allows_insert() && arr_pos == end_pos && count == *idx as usize
                            {
                                let placeholder =
                                    JsonbHeader::new(ElementType::OBJECT, 0).into_bytes();
                                let placeholder_bytes = placeholder.as_bytes();

                                self.data
                                    .splice(arr_pos..arr_pos, placeholder_bytes.iter().copied());

                                return Ok(JsonTraversalResult::with_array_index(
                                    pos,
                                    JsonLocationKind::DocumentRoot,
                                    placeholder_bytes.len() as isize,
                                    arr_pos,
                                ));
                            }

                            if arr_pos != end_pos && mode.allows_replace() {
                                return Ok(JsonTraversalResult::with_array_index(
                                    pos,
                                    JsonLocationKind::DocumentRoot,
                                    0,
                                    arr_pos,
                                ));
                            }

                            bail_parse_error!("Not found!");
                        }
                        Some(idx) if *idx < 0 => {
                            let mut idx_map: HashMap<i32, usize> = HashMap::with_capacity(100);
                            let mut element_idx = 0;
                            let mut arr_pos = pos + root_header_size;

                            while arr_pos < end_pos {
                                idx_map.insert(element_idx, arr_pos);
                                arr_pos = self.skip_element(arr_pos)?;
                                element_idx += 1;
                            }

                            let real_idx = element_idx + idx;

                            if let Some(index) = idx_map.get(&real_idx) {
                                return Ok(JsonTraversalResult::with_array_index(
                                    pos,
                                    JsonLocationKind::DocumentRoot,
                                    0,
                                    *index,
                                ));
                            } else {
                                bail_parse_error!("Element with negative index not found")
                            }
                        }
                        _ => unreachable!(),
                    }
                } else {
                    bail_parse_error!("Root is not an array");
                }
            }
            SegmentVariant::KeyWithArrayIndex(
                PathElement::Key(path_key, is_raw),
                PathElement::ArrayLocator(idx),
            ) => {
                if element_type != ElementType::OBJECT {
                    bail_parse_error!("Not an object");
                }

                let end_pos = pos + header_size + element_size;

                let mut current_pos = pos + header_size;

                while current_pos < end_pos {
                    let (JsonbHeader(key_type, key_size), key_header_size) =
                        self.read_header(current_pos)?;

                    if !key_type.is_valid_key() {
                        bail_parse_error!("Key should be string")
                    }

                    let obj_key = unsafe {
                        from_utf8_unchecked(
                            &self.data[current_pos + key_header_size
                                ..current_pos + key_header_size + key_size],
                        )
                    };

                    if compare((obj_key, key_type), (path_key, *is_raw)) {
                        break;
                    } else {
                        current_pos =
                            self.skip_element(current_pos + key_size + key_header_size)?;
                    }
                }

                if current_pos == end_pos && mode.allows_insert() {
                    if idx.is_some() && idx.unwrap() != 0 {
                        bail_parse_error!("cant create new arr with idx");
                    }

                    let key_header_type = if *is_raw {
                        ElementType::TEXTRAW
                    } else {
                        ElementType::TEXT
                    };

                    let key_header = JsonbHeader::new(key_header_type, path_key.len()).into_bytes();
                    let key_header_bytes = key_header.as_bytes();
                    let key_bytes = path_key.as_bytes();
                    let array_header = JsonbHeader::new(ElementType::ARRAY, 1).into_bytes();
                    let array_header_bytes = array_header.as_bytes();
                    let array_value_header = JsonbHeader::new(ElementType::OBJECT, 0).into_bytes();
                    let array_value_header_bytes = array_value_header.as_bytes();

                    let delta = key_header_bytes.len()
                        + key_bytes.len()
                        + array_header_bytes.len()
                        + array_value_header_bytes.len();

                    self.data.splice(
                        current_pos..current_pos,
                        key_header_bytes
                            .iter()
                            .copied()
                            .chain(key_bytes.iter().copied())
                            .chain(array_header_bytes.iter().copied())
                            .chain(array_value_header_bytes.iter().copied()),
                    );

                    let key_idx = current_pos;
                    let value_idx = current_pos + key_header_bytes.len() + key_bytes.len();
                    let array_idx = value_idx + array_header_bytes.len();

                    return Ok(JsonTraversalResult::with_array_index(
                        value_idx,
                        JsonLocationKind::ObjectProperty(key_idx),
                        delta as isize,
                        array_idx,
                    ));
                }

                if current_pos != end_pos && mode.allows_replace() {
                    let key_idx = current_pos;

                    current_pos = self.skip_element(current_pos)?;
                    let value_idx = current_pos;

                    let (JsonbHeader(value_type, value_size), value_header_size) =
                        self.read_header(value_idx)?;

                    if value_type != ElementType::ARRAY {
                        bail_parse_error!("Should be array")
                    }

                    let end_pos = current_pos + value_header_size + value_size;

                    match idx {
                        Some(idx) if *idx >= 0 => {
                            let mut count = 0;
                            let mut arr_pos = value_idx + value_header_size;

                            while arr_pos < end_pos && count != *idx as usize {
                                arr_pos = self.skip_element(arr_pos)?;
                                count += 1;
                            }

                            if mode.allows_insert() && arr_pos == end_pos && count == *idx as usize
                            {
                                let placeholder =
                                    JsonbHeader::new(ElementType::OBJECT, 0).into_bytes();
                                let placeholder_bytes = placeholder.as_bytes();

                                self.data
                                    .splice(arr_pos..arr_pos, placeholder_bytes.iter().copied());
                                self.write_element_header(
                                    value_idx,
                                    ElementType::ARRAY,
                                    value_size + placeholder_bytes.len(),
                                    true,
                                )?;
                                return Ok(JsonTraversalResult::with_array_index(
                                    value_idx,
                                    JsonLocationKind::ObjectProperty(key_idx),
                                    placeholder_bytes.len() as isize,
                                    arr_pos,
                                ));
                            }

                            if arr_pos != end_pos && mode.allows_replace() {
                                return Ok(JsonTraversalResult::with_array_index(
                                    value_idx,
                                    JsonLocationKind::ObjectProperty(key_idx),
                                    0,
                                    arr_pos,
                                ));
                            }

                            bail_parse_error!("Not found!");
                        }
                        Some(idx) if *idx < 0 => {
                            let mut idx_map: HashMap<i32, usize> = HashMap::with_capacity(100);
                            let mut element_idx = 0;
                            let mut arr_pos = value_idx + value_header_size;

                            while arr_pos < end_pos {
                                idx_map.insert(element_idx, arr_pos);
                                arr_pos = self.skip_element(arr_pos)?;
                                element_idx += 1;
                            }

                            let real_idx = element_idx + idx;

                            if let Some(index) = idx_map.get(&real_idx) {
                                return Ok(JsonTraversalResult::with_array_index(
                                    value_idx,
                                    JsonLocationKind::ObjectProperty(key_idx),
                                    0,
                                    *index,
                                ));
                            } else {
                                bail_parse_error!(
                                    "ERROR: Element at negative index {} not found",
                                    idx
                                );
                            }
                        }
                        Some(_) => unreachable!(),
                        None => {
                            if mode.allows_insert() {
                                let placeholder =
                                    JsonbHeader::new(ElementType::OBJECT, 0).into_bytes();
                                let placeholder_bytes = placeholder.as_bytes();
                                let insertion_point = value_idx + value_size + value_header_size;

                                self.data.insert(insertion_point, placeholder_bytes[0]);
                            } else {
                                bail_parse_error!("Cant insert")
                            }
                        }
                    }
                }
            }
            _ => {
                unreachable!()
            }
        };

        Err(LimboError::ParseError("Not found".to_string()))
    }

    fn skip_element(&self, mut pos: usize) -> Result<usize> {
        let (header, skip_header) = self.read_header(pos)?;
        pos += skip_header + header.1;
        Ok(pos)
    }

    // Primitive implementation could be optimized.
    pub fn patch(&mut self, patch: &Jsonb) -> Result<()> {
        let (patch_header, _) = patch.read_header(0)?;

        if patch_header.0 != ElementType::OBJECT {
            self.data.clear();
            self.data.extend_from_slice(&patch.data);
            return Ok(());
        }

        let result = self;

        let mut work_stack = VecDeque::with_capacity(10);
        work_stack.push_back((
            JsonPath {
                elements: vec![PathElement::Root()],
            },
            0,
        ));

        while let Some((path, patch_cursor)) = work_stack.pop_front() {
            let (patch_obj_header, patch_obj_header_size) = patch.read_header(patch_cursor)?;

            if patch_obj_header.0 != ElementType::OBJECT {
                continue;
            }

            let patch_end = patch_cursor + patch_obj_header_size + patch_obj_header.1;
            let mut patch_key_cursor = patch_cursor + patch_obj_header_size;

            let mut key_values = Vec::new();

            while patch_key_cursor < patch_end {
                let (key_header, key_header_size) = patch.read_header(patch_key_cursor)?;
                if !key_header.0.is_valid_key() {
                    return Err(LimboError::ParseError("Invalid key type".to_string()));
                }

                let key_start = patch_key_cursor + key_header_size;
                let key_text = unsafe {
                    from_utf8_unchecked(&patch.data[key_start..key_start + key_header.1])
                };

                // Read the value
                let value_cursor = key_start + key_header.1;
                let (value_header, value_header_size) = patch.read_header(value_cursor)?;
                let key_text = if matches!(
                    key_header.0,
                    ElementType::TEXT5 | ElementType::TEXTJ | ElementType::TEXTRAW
                ) {
                    Cow::Owned(unescape_string(key_text))
                } else {
                    Cow::Borrowed(key_text)
                };

                key_values.push((
                    key_text,
                    value_header.0,
                    value_cursor,
                    value_header_size,
                    value_header.1,
                ));

                patch_key_cursor = value_cursor + value_header_size + value_header.1;
            }

            for (key_text, value_type, value_cursor, value_header_size, value_size) in key_values {
                // Create a path to this key
                let mut key_path = path.clone();

                key_path.elements.push(PathElement::Key(key_text, false));

                match value_type {
                    ElementType::NULL => {
                        let mut op = DeleteOperation::new();

                        let _ = result.operate_on_path(&key_path, &mut op);
                    }
                    ElementType::OBJECT => {
                        let value_data = &patch.data
                            [value_cursor..value_cursor + value_header_size + value_size];

                        let target_path_result =
                            result.navigate_path(&key_path, PathOperationMode::ReplaceExisting);

                        if target_path_result.is_ok() {
                            let target_stack = target_path_result.unwrap();
                            let target_value_idx = target_stack.last().unwrap().field_value_index;
                            let (target_header, _) = result.read_header(target_value_idx)?;

                            if target_header.0 == ElementType::OBJECT {
                                work_stack.push_back((key_path, value_cursor));
                            } else {
                                let patch_obj = Jsonb::new(value_data.len(), Some(value_data));
                                let mut op = ReplaceOperation::new(patch_obj);
                                result.operate_on_path(&key_path, &mut op)?;
                                let _ = result.operate_on_path(&key_path, &mut op);

                                work_stack.push_back((key_path, value_cursor));
                            }
                        } else {
                            let empty_obj = Jsonb::new(
                                1,
                                Some(JsonbHeader::make_obj().into_bytes().as_bytes()),
                            );
                            let mut op = SetOperation::new(empty_obj);
                            let _ = result.operate_on_path(&key_path, &mut op);

                            work_stack.push_back((key_path, value_cursor));
                        }
                    }
                    _ => {
                        let value_data = &patch.data
                            [value_cursor..value_cursor + value_header_size + value_size];
                        let patch_value = Jsonb::new(value_data.len(), Some(value_data));

                        let mut op = SetOperation::new(patch_value);

                        let _ = result.operate_on_path(&key_path, &mut op);
                    }
                }
            }
        }

        Ok(())
    }
}
