//! Auto-generated module
//!
//! 🤖 Generated with [SplitRS](https://github.com/cool-japan/splitrs)

use super::functions::{
    make_character_type_ok_table, make_character_type_table, make_whitespace_table,
};

pub(super) const SIZE_MARKER_8BIT: u8 = 12;

pub(super) const SIZE_MARKER_16BIT: u8 = 13;

pub(super) const SIZE_MARKER_32BIT: u8 = 14;

pub(super) const MAX_JSON_DEPTH: usize = 1000;

pub(super) const INFINITY_CHAR_COUNT: u8 = 5;

pub(super) static WS_TABLE: [u8; 256] = make_whitespace_table();

pub(super) static CHARACTER_TYPE: [u8; 256] = make_character_type_table();

pub(super) static CHARACTER_TYPE_OK: [u8; 256] = make_character_type_ok_table();
