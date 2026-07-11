// UPSTREAM: vendored Limbo fork — allow upstream style
//! Extension API for the C-free **oxisqlite** engine: write new SQL
//! functionality in pure Rust, no C, in the spirit of `sqlite3` extensions.
//!
//! Provides the `scalar`, `AggregateDerive`, and `VTabModuleDerive` macros
//! (scalar/aggregate functions, virtual tables), an optional `vfs`-gated VFS
//! interface, and the `register_extension!` macro that plugs it all in.
#![allow(
    rustdoc::bare_urls,
    rustdoc::invalid_html_tags,
    rustdoc::invalid_rust_codeblocks
)]
#![allow(clippy::cast_slice_from_raw_parts)]

mod functions;
mod types;
#[cfg(feature = "vfs")]
mod vfs_modules;
mod vtabs;
pub use functions::{
    AggCtx, AggFunc, FinalizeFunction, InitAggFunction, ScalarFunction, StepFunction,
};
use functions::{RegisterAggFn, RegisterScalarFn};
#[cfg(feature = "vfs")]
pub use limbo_macros::VfsDerive;
pub use limbo_macros::{register_extension, scalar, AggregateDerive, VTabModuleDerive};
use std::os::raw::c_void;
pub use types::{ResultCode, StepResult, Value, ValueType};
#[cfg(feature = "vfs")]
pub use vfs_modules::{RegisterVfsFn, VfsExtension, VfsFile, VfsFileImpl, VfsImpl, VfsInterface};
use vtabs::RegisterModuleFn;
pub use vtabs::{
    Conn, Connection, ConstraintInfo, ConstraintOp, ConstraintUsage, ExtIndexInfo, IndexInfo,
    OrderByInfo, Statement, Stmt, VTabCreateResult, VTabCursor, VTabKind, VTabModule,
    VTabModuleImpl, VTable,
};

pub type ExtResult<T> = std::result::Result<T, ResultCode>;

pub type ExtensionEntryPoint = unsafe extern "C" fn(api: *const ExtensionApi) -> ResultCode;

#[repr(C)]
pub struct ExtensionApi {
    pub ctx: *mut c_void,
    pub register_scalar_function: RegisterScalarFn,
    pub register_aggregate_function: RegisterAggFn,
    pub register_vtab_module: RegisterModuleFn,
    #[cfg(feature = "vfs")]
    pub vfs_interface: VfsInterface,
}

unsafe impl Send for ExtensionApi {}
unsafe impl Send for ExtensionApiRef {}

#[repr(C)]
pub struct ExtensionApiRef {
    pub api: *const ExtensionApi,
}
