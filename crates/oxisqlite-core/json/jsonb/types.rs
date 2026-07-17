//! Auto-generated module
//!
//! 🤖 Generated with [SplitRS](https://github.com/cool-japan/splitrs)

use super::super::path::PathElement;
use crate::{bail_parse_error, Result};
use std::borrow::Cow;

use super::constants::{SIZE_MARKER_16BIT, SIZE_MARKER_32BIT, SIZE_MARKER_8BIT};
use super::jsonb_type::Jsonb;
use super::type_aliases::PayloadSize;

pub struct SetOperation {
    pub(super) value: Jsonb,
    pub(super) mode: PathOperationMode,
}
impl SetOperation {
    pub fn new(value: Jsonb) -> Self {
        Self {
            value,
            mode: PathOperationMode::Upsert,
        }
    }
}
#[derive(Debug, Clone, PartialEq)]
pub enum JsonLocationKind {
    ObjectProperty(usize),
    DocumentRoot,
    ArrayEntry,
}
#[derive(Debug, Clone, Copy)]
pub struct JsonbHeader(pub(super) ElementType, pub(super) PayloadSize);
impl JsonbHeader {
    pub(super) fn new(element_type: ElementType, payload_size: PayloadSize) -> Self {
        Self(element_type, payload_size)
    }
    pub fn make_null() -> Self {
        Self(ElementType::NULL, 0)
    }
    pub fn make_obj() -> Self {
        Self(ElementType::OBJECT, 0)
    }
    pub(super) fn from_slice(cursor: usize, slice: &[u8]) -> Result<(Self, usize)> {
        match slice.get(cursor) {
            Some(header_byte) => {
                // Extract first 4 bits (values 0-15)
                let element_type = header_byte & 15;
                if element_type > 12 {
                    bail_parse_error!("Invalid element type: {}", element_type);
                }
                // Get the last 4 bits for header_size
                let header_size = header_byte >> 4;
                let offset: usize;
                let total_size = match header_size {
                    size if size <= 11 => {
                        offset = 1;
                        size as usize
                    }
                    12 => match slice.get(cursor + 1) {
                        Some(value) => {
                            offset = 2;
                            *value as usize
                        }
                        None => bail_parse_error!("Failed to read 1-byte size"),
                    },
                    13 => match Self::get_size_bytes(slice, cursor + 1, 2) {
                        Ok(bytes) => {
                            offset = 3;
                            u16::from_be_bytes([bytes[0], bytes[1]]) as usize
                        }
                        Err(e) => return Err(e),
                    },
                    14 => match Self::get_size_bytes(slice, cursor + 1, 4) {
                        Ok(bytes) => {
                            offset = 5;
                            u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize
                        }
                        Err(e) => return Err(e),
                    },
                    _ => unreachable!(),
                };
                Ok((Self(element_type.try_into()?, total_size), offset))
            }
            None => bail_parse_error!("Failed to read header byte"),
        }
    }
    pub fn into_bytes(self) -> HeaderFormat {
        let (element_type, payload_size) = (self.0, self.1);
        match payload_size {
            // Small payload (fits in 4 bits)
            size if size <= 11 => {
                HeaderFormat::Inline([(element_type as u8) | ((size as u8) << 4)])
            }
            // Medium payload (fits in 1 byte)
            size if size <= 0xFF => {
                HeaderFormat::OneByte([(element_type as u8) | (SIZE_MARKER_8BIT << 4), size as u8])
            }
            // Large payload (fits in 2 bytes)
            size if size <= 0xFFFF => {
                let size_bytes = (size as u16).to_be_bytes();
                HeaderFormat::TwoBytes([
                    (element_type as u8) | (SIZE_MARKER_16BIT << 4),
                    size_bytes[0],
                    size_bytes[1],
                ])
            }
            // Extra large payload (fits in 4 bytes)
            size if size <= 0xFFFFFFFF => {
                let size_bytes = (size as u32).to_be_bytes();
                HeaderFormat::FourBytes([
                    (element_type as u8) | (SIZE_MARKER_32BIT << 4),
                    size_bytes[0],
                    size_bytes[1],
                    size_bytes[2],
                    size_bytes[3],
                ])
            }
            // Payload too large
            _ => panic!("Payload size too large for encoding"),
        }
    }
    pub(super) fn get_size_bytes(slice: &[u8], start: usize, count: usize) -> Result<&[u8]> {
        match slice.get(start..start + count) {
            Some(bytes) => Ok(bytes),
            None => bail_parse_error!("Failed to read header size"),
        }
    }
}
#[derive(Debug, Clone)]
pub enum ArrayPositionKind {
    SpecificIndex(usize),
}
pub(crate) enum HeaderFormat {
    Inline([u8; 1]),    // Small payloads embedded directly in the header
    OneByte([u8; 2]),   // Medium payloads with 1-byte size field
    TwoBytes([u8; 3]),  // Large payloads with 2-byte size field
    FourBytes([u8; 5]), // Extra large payloads with 4-byte size field
}
impl HeaderFormat {
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Inline(bytes) => bytes,
            Self::OneByte(bytes) => bytes,
            Self::TwoBytes(bytes) => bytes,
            Self::FourBytes(bytes) => bytes,
        }
    }
}
pub struct SearchOperation {
    pub(super) value: Jsonb,
    pub(super) mode: PathOperationMode,
}
impl SearchOperation {
    pub fn new(capacity: usize) -> Self {
        Self {
            mode: PathOperationMode::ReplaceExisting,
            value: Jsonb::new(capacity, None),
        }
    }
    pub fn result(self) -> Jsonb {
        self.value
    }
    /// Borrows the bytes accumulated so far without consuming `self`, so a single
    /// `SearchOperation` can be read out and then reused via [`Self::clear`] instead of being
    /// reallocated -- e.g. once per path element of a multi-path `json_extract`/`jsonb_extract`
    /// call, instead of allocating a fresh `SearchOperation` (and its backing buffer) per path.
    pub fn data(&self) -> &[u8] {
        &self.value.data
    }
    /// Clears the accumulated result while retaining the underlying buffer's capacity, so
    /// this `SearchOperation` can be run again for another path via [`PathOperation::execute`]
    /// as if it were freshly constructed.
    pub fn clear(&mut self) {
        self.value.data.clear();
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathOperationMode {
    /// Only replace values if the complete path already exists
    ReplaceExisting,
    /// Only insert values if the path doesn't exist yet
    InsertNew,
    /// Either replace existing values or create new ones as needed
    Upsert,
}
impl PathOperationMode {
    /// Returns true if this mode allows replacing existing values
    pub fn allows_replace(&self) -> bool {
        matches!(self, Self::ReplaceExisting | Self::Upsert)
    }
    /// Returns true if this mode allows creating new paths
    pub fn allows_insert(&self) -> bool {
        matches!(self, Self::InsertNew | Self::Upsert)
    }
}
pub struct InsertOperation {
    pub(super) value: Jsonb,
    pub(super) mode: PathOperationMode,
}
impl InsertOperation {
    pub fn new(value: Jsonb) -> Self {
        Self {
            value,
            mode: PathOperationMode::InsertNew,
        }
    }
}
#[derive(Debug, Clone)]
pub enum SegmentVariant<'a> {
    Single(&'a PathElement<'a>),
    KeyWithArrayIndex(&'a PathElement<'a>, &'a PathElement<'a>),
}
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum ElementType {
    NULL = 0,
    TRUE = 1,
    FALSE = 2,
    INT = 3,
    INT5 = 4,
    FLOAT = 5,
    FLOAT5 = 6,
    TEXT = 7,
    TEXTJ = 8,
    TEXT5 = 9,
    TEXTRAW = 10,
    ARRAY = 11,
    OBJECT = 12,
    RESERVED1 = 13,
    RESERVED2 = 14,
    RESERVED3 = 15,
}
impl ElementType {
    pub fn is_valid_key(&self) -> bool {
        matches!(self, Self::TEXT | Self::TEXT5 | Self::TEXTJ | Self::TEXTRAW)
    }
}
pub struct DeleteOperation {
    pub(super) mode: PathOperationMode,
}
impl DeleteOperation {
    pub fn new() -> Self {
        Self {
            mode: PathOperationMode::ReplaceExisting,
        }
    }
}
pub struct ReplaceOperation {
    pub(super) value: Jsonb,
    pub(super) mode: PathOperationMode,
}
impl ReplaceOperation {
    pub fn new(value: Jsonb) -> Self {
        Self {
            value,
            mode: PathOperationMode::ReplaceExisting,
        }
    }
}
pub enum JsonIndentation<'a> {
    Indentation(Cow<'a, str>),
    None,
}
impl<'a> JsonIndentation<'a> {
    pub fn is_pretty(&self) -> bool {
        match self {
            Self::Indentation(_) => true,
            Self::None => false,
        }
    }
}
#[derive(Debug, Clone)]
pub struct JsonTraversalResult {
    pub(super) field_key_index: JsonLocationKind,
    pub field_value_index: usize,
    pub(super) delta: isize,
    pub(super) array_position_info: Option<ArrayPositionKind>,
}
impl JsonTraversalResult {
    pub fn new(field_value_index: usize, field_key_index: JsonLocationKind, delta: isize) -> Self {
        Self {
            field_value_index,
            delta,
            field_key_index,
            array_position_info: None,
        }
    }
    pub fn with_array_index(
        field_value_index: usize,
        field_key_index: JsonLocationKind,
        delta: isize,
        index: usize,
    ) -> Self {
        Self {
            field_value_index,
            field_key_index,
            delta,
            array_position_info: Some(ArrayPositionKind::SpecificIndex(index)),
        }
    }
    pub fn has_specific_index(&self) -> bool {
        matches!(
            self.array_position_info,
            Some(ArrayPositionKind::SpecificIndex(_))
        )
    }
    pub fn get_array_index(&self) -> Option<usize> {
        match self.array_position_info {
            Some(ArrayPositionKind::SpecificIndex(idx)) => Some(idx),
            _ => None,
        }
    }
}
