//! # `InsertOperation` - Trait Implementations
//!
//! This module contains trait implementations for `InsertOperation`.
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
    ElementType, InsertOperation, JsonLocationKind, JsonTraversalResult, JsonbHeader,
    PathOperationMode,
};

impl PathOperation for InsertOperation {
    fn operation_mode(&self) -> PathOperationMode {
        self.mode
    }

    fn execute(&mut self, json: &mut Jsonb, mut stack: Vec<JsonTraversalResult>) -> Result<()> {
        if stack.is_empty() {
            bail_parse_error!("Nothing to operate on!")
        }
        let value = &self.value.data;
        let target = stack.pop().unwrap();

        // handle array
        if target.has_specific_index() {
            let array_value_idx = target.get_array_index().unwrap();
            let obj_value_idx = target.field_value_index;
            let (JsonbHeader(_, obj_value_size), obj_value_header_size) =
                json.read_header(obj_value_idx)?;
            let (JsonbHeader(_, array_value_size), array_value_header_size) =
                json.read_header(array_value_idx)?;

            let delta =
                value.len() as isize - (array_value_size + array_value_header_size) as isize;

            let end_pos = array_value_idx + array_value_size + array_value_header_size;
            json.data
                .splice(array_value_idx..end_pos, value.iter().copied());

            // update parent
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
                (new_h_delta - obj_value_header_size) as isize
            } else {
                0
            };

            json.update_parent_references(stack, target.delta + delta + h_delta)?;
        } else {
            let old_value_idx = target.field_value_index;
            let (JsonbHeader(_, old_value_size), old_value_header_size) =
                json.read_header(old_value_idx)?;
            let delta = value.len() as isize - (old_value_header_size + old_value_size) as isize;

            let end_pos = old_value_idx + old_value_header_size + old_value_size;

            json.data
                .splice(old_value_idx..end_pos, value.iter().copied());

            json.update_parent_references(stack, delta + target.delta)?;
        }

        Ok(())
    }
}
