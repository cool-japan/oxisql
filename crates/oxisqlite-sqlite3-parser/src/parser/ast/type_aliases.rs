//! Auto-generated module
//!
//! 🤖 Generated with [SplitRS](https://github.com/cool-japan/splitrs)

use super::types_10::TableReferenceId;
use super::types_9::Expr;

/// Backward-compatible alias for [`TableReferenceId`].
///
/// `TableReferenceId` is the canonical name for this type; this alias exists
/// only so that other crates in the workspace do not need to be updated in
/// lockstep with this rename.
pub type TableInternalId = TableReferenceId;

/// `PRAGMA` value
// https://sqlite.org/syntax/pragma-value.html
pub type PragmaValue = Expr; // TODO
