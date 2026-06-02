#![forbid(unsafe_code)]
#![warn(missing_docs)]

//! `oxisql-datafusion` — Apache DataFusion `TableProvider` over an OxiSQL
//! [`Connection`](oxisql_core::Connection).
//!
//! This crate exposes oxisql-backed tables to Apache DataFusion so that OLAP
//! SQL queries can be planned and executed against oxisql data using the full
//! DataFusion query engine.
//!
//! # Quick start
//!
//! ```rust
//! use std::sync::Arc;
//! use arrow::datatypes::{DataType, Field, Schema};
//! use oxisql_core::{Row, Value};
//! use oxisql_datafusion::OxiSqlTableProvider;
//!
//! let schema = Arc::new(Schema::new(vec![
//!     Field::new("id",    DataType::Int64,   false),
//!     Field::new("name",  DataType::Utf8,    false),
//!     Field::new("score", DataType::Float64, false),
//! ]));
//!
//! let rows = vec![
//!     Row::new(
//!         vec!["id".into(), "name".into(), "score".into()],
//!         vec![Value::I64(1), Value::Text("Alice".into()), Value::F64(95.5)],
//!     ),
//! ];
//!
//! let provider = OxiSqlTableProvider::from_rows(rows, schema);
//! ```

pub mod context;
pub mod error;
pub mod provider;
pub mod stream;
pub mod types;

#[cfg(feature = "columnar")]
pub mod parquet;

pub use context::{register_embedded_table, register_oxisql_table, OxiSqlContext};
pub use error::OxiSqlFusionError;
pub use provider::OxiSqlTableProvider;
pub use stream::{OxiSqlStreamProvider, SortOrder};

#[cfg(feature = "columnar")]
pub use parquet::ParquetTableProvider;

/// Bridge utilities for converting `oxisql_parse::LogicalPlan` to DataFusion
/// `LogicalPlan`.  Enabled by the `parse` feature flag.
#[cfg(feature = "parse")]
pub mod plan_bridge;

#[cfg(feature = "parse")]
pub use plan_bridge::{sql_to_datafusion_plan, to_datafusion_plan};
