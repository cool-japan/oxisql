//! Error type for the `oxisql-datafusion` crate.

/// Errors that can occur while bridging OxiSQL rows to DataFusion.
#[derive(Debug, thiserror::Error)]
pub enum OxiSqlFusionError {
    /// An error originating from the OxiSQL layer.
    #[error("oxisql error: {0}")]
    OxiSql(String),

    /// An Arrow error encountered while building a `RecordBatch`.
    #[error("arrow error: {0}")]
    Arrow(#[from] arrow::error::ArrowError),

    /// A DataFusion error propagated from the execution layer.
    #[error("datafusion error: {0}")]
    DataFusion(#[from] datafusion::error::DataFusionError),

    /// Schema mismatch: row column count does not match the schema.
    #[error("schema mismatch: expected {expected} columns, got {got}")]
    SchemaMismatch {
        /// Number of fields in the Arrow schema.
        expected: usize,
        /// Number of values in the OxiSQL row.
        got: usize,
    },

    /// An Arrow [`DataType`] that has no corresponding OxiSQL `Value` mapping.
    ///
    /// [`DataType`]: arrow::datatypes::DataType
    #[error("unsupported Arrow type: {0}")]
    UnsupportedType(String),
}
