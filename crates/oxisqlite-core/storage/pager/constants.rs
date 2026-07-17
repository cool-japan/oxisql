//! Auto-generated module
//!
//! 🤖 Generated with [SplitRS](https://github.com/cool-japan/splitrs)

/// Page is up-to-date.
pub(super) const PAGE_UPTODATE: usize = 0b001;
/// Page is locked for I/O to prevent concurrent access.
pub(super) const PAGE_LOCKED: usize = 0b010;
/// Page had an I/O error.
pub(super) const PAGE_ERROR: usize = 0b100;
/// Page is dirty. Flush needed.
pub(super) const PAGE_DIRTY: usize = 0b1000;
/// Page's contents are loaded in memory.
pub(super) const PAGE_LOADED: usize = 0b10000;
