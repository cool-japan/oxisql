//! # `SearchOperation` - Trait Implementations
//!
//! This module contains trait implementations for `SearchOperation`.
//!
//! ## Implemented Traits
//!
//! - `PathOperation`
//!
//! 🤖 Generated with [SplitRS](https://github.com/cool-japan/splitrs)

use crate::Result;

use super::functions::PathOperation;
use super::jsonb_type::Jsonb;
use super::types::{JsonTraversalResult, JsonbHeader, PathOperationMode, SearchOperation};

impl PathOperation for SearchOperation {
    fn operation_mode(&self) -> PathOperationMode {
        self.mode
    }

    fn execute(&mut self, json: &mut Jsonb, mut stack: Vec<JsonTraversalResult>) -> Result<()> {
        let target = stack.pop().unwrap();
        let idx = if let Some(idx) = target.get_array_index() {
            idx
        } else {
            target.field_value_index
        };
        let (JsonbHeader(_, size), header_size) = json.read_header(idx)?;
        self.value
            .data
            .extend_from_slice(&json.data[idx..idx + header_size + size]);

        Ok(())
    }
}
