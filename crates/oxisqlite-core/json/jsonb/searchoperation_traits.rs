//! # `SearchOperation` - Trait Implementations
//!
//! This module contains trait implementations for `SearchOperation`.
//!
//! ## Implemented Traits
//!
//! - `PathOperation`
//!
//! 🤖 Generated with [SplitRS](https://github.com/cool-japan/splitrs)

use crate::{bail_parse_error, Result};

use super::functions::PathOperation;
use super::jsonb_type::Jsonb;
use super::types::{JsonTraversalResult, JsonbHeader, PathOperationMode, SearchOperation};

impl PathOperation for SearchOperation {
    fn operation_mode(&self) -> PathOperationMode {
        self.mode
    }

    fn execute(&mut self, json: &mut Jsonb, mut stack: Vec<JsonTraversalResult>) -> Result<()> {
        // An empty traversal stack means the path resolver produced no target;
        // popping it used to `unwrap()`.
        let Some(target) = stack.pop() else {
            bail_parse_error!("malformed JSON")
        };
        let idx = if let Some(idx) = target.get_array_index() {
            idx
        } else {
            target.field_value_index
        };
        let (JsonbHeader(_, size), header_size) = json.read_header(idx)?;
        // `size` comes verbatim from the element header of an attacker-supplied
        // JSONB blob (up to `u32::MAX`) and `read_header` does not check it
        // against the buffer length, so this slice used to panic with
        // "range end index N out of range" on e.g.
        // `SELECT json_extract(X'1bc7ff', '$[0]')`.
        let Some(element) = idx
            .checked_add(header_size)
            .and_then(|end| end.checked_add(size))
            .and_then(|end| json.data.get(idx..end))
        else {
            bail_parse_error!("malformed JSON")
        };
        self.value.data.extend_from_slice(element);

        Ok(())
    }
}
