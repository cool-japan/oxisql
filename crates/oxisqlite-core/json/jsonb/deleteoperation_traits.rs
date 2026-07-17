//! # `DeleteOperation` - Trait Implementations
//!
//! This module contains trait implementations for `DeleteOperation`.
//!
//! ## Implemented Traits
//!
//! - `PathOperation`
//!
//! 🤖 Generated with [SplitRS](https://github.com/cool-japan/splitrs)

use crate::{bail_parse_error, Result};

use super::functions::PathOperation;
use super::jsonb_type::Jsonb;
use super::types::{
    DeleteOperation, ElementType, JsonLocationKind, JsonTraversalResult, JsonbHeader,
    PathOperationMode,
};

impl PathOperation for DeleteOperation {
    fn operation_mode(&self) -> PathOperationMode {
        self.mode
    }

    fn execute(&mut self, json: &mut Jsonb, mut stack: Vec<JsonTraversalResult>) -> Result<()> {
        if stack.is_empty() {
            bail_parse_error!("Nothing to operate on!")
        }

        let target = stack.pop().unwrap();

        // handle array
        if target.has_specific_index() {
            let array_value_idx = target.get_array_index().unwrap();

            let obj_value_idx = target.field_value_index;
            let (JsonbHeader(_, obj_value_size), obj_value_header_size) =
                json.read_header(obj_value_idx)?;
            let (JsonbHeader(_, array_value_size), array_value_header_size) =
                json.read_header(array_value_idx)?;
            let delta = 0 - (array_value_size + array_value_header_size) as isize;

            let end_pos = array_value_idx + array_value_size + array_value_header_size;
            json.data.drain(array_value_idx..end_pos);

            let h_delta = if matches!(
                target.field_key_index,
                JsonLocationKind::ObjectProperty(_) | JsonLocationKind::DocumentRoot
            ) {
                let new_h_delta = json.write_element_header(
                    obj_value_idx,
                    ElementType::ARRAY,
                    (obj_value_size as isize + delta) as usize,
                    true,
                )?;
                new_h_delta as isize - obj_value_header_size as isize
            } else {
                0
            };
            json.update_parent_references(stack, target.delta + delta + h_delta)?;
        } else if let JsonLocationKind::ObjectProperty(key_idx) = target.field_key_index {
            let value_idx = target.field_value_index;
            let (JsonbHeader(_, value_size), value_header_size) = json.read_header(value_idx)?;
            let (JsonbHeader(_, key_size), key_header_size) = json.read_header(key_idx)?;
            let delta = 0 - (value_header_size + value_size + key_size + key_header_size) as isize;

            let end_pos = key_idx + value_header_size + value_size + key_size + key_header_size;
            json.data.drain(key_idx..end_pos);

            json.update_parent_references(stack, delta + target.delta)?;
        } else {
            let nul = JsonbHeader::make_null().into_bytes();
            let nul_bytes = nul.as_bytes();
            json.data.clear();
            json.data.extend_from_slice(nul_bytes);
        }

        Ok(())
    }
}
