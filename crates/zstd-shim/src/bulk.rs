//! Block ("bulk") compress/decompress — source-compatible with `zstd::bulk`,
//! backed by `oxiarc-zstd`.

use std::io;
use std::marker::PhantomData;

/// Source-compatible with `zstd::bulk::Compressor`.
pub struct Compressor<'a> {
    level: i32,
    _marker: PhantomData<&'a ()>,
}

impl Compressor<'static> {
    pub fn new(level: i32) -> io::Result<Self> {
        Ok(Self {
            level,
            _marker: PhantomData,
        })
    }
}

impl<'a> Compressor<'a> {
    pub fn compress(&mut self, data: &[u8]) -> io::Result<Vec<u8>> {
        // zstd treats level 0 as "use default"; oxiarc wants 1..=22.
        let level = if self.level <= 0 {
            crate::DEFAULT_COMPRESSION_LEVEL
        } else {
            self.level
        };
        oxiarc_zstd::compress_with_level(data, level)
            .map_err(io::Error::other)
    }
}

/// Source-compatible with `zstd::bulk::Decompressor`.
pub struct Decompressor<'a> {
    _marker: PhantomData<&'a ()>,
}

impl Decompressor<'static> {
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            _marker: PhantomData,
        })
    }
}

impl<'a> Decompressor<'a> {
    pub fn decompress(&mut self, data: &[u8], _capacity: usize) -> io::Result<Vec<u8>> {
        oxiarc_zstd::decode_all(data).map_err(io::Error::other)
    }
}

#[cfg(test)]
mod tests {
    use super::{Compressor, Decompressor};
    use crate::DEFAULT_COMPRESSION_LEVEL;

    #[test]
    fn round_trip_via_oxiarc() {
        let input = b"COOLJAPAN pure-rust zstd shim round-trip payload 0123456789".repeat(16);
        let mut comp = Compressor::new(DEFAULT_COMPRESSION_LEVEL).expect("new compressor");
        let packed = comp.compress(&input).expect("compress");
        assert!(!packed.is_empty(), "compressed output must be non-empty");
        let mut dec = Decompressor::new().expect("new decompressor");
        let out = dec
            .decompress(&packed, input.len())
            .expect("decompress");
        assert_eq!(out, input, "round-trip must reproduce input");
    }
}
