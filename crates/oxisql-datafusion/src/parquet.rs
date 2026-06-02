//! DataFusion [`TableProvider`] backed by an oxistore-columnar Parquet file.
//!
//! Each scan reads the Parquet file via the free functions exposed by
//! `oxistore_columnar` (`read_batches` / `read_batches_with_projection`).
//! When DataFusion requests a column projection, column indices are forwarded
//! to the reader for I/O-level column pruning.
//!
//! # Example
//!
//! ```rust,no_run
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! use oxisql_datafusion::ParquetTableProvider;
//! use oxisql_datafusion::OxiSqlContext;
//!
//! let ctx = OxiSqlContext::new();
//! ctx.register_parquet("events", "/data/events.parquet")?;
//! let batches = ctx.execute_sql("SELECT * FROM events LIMIT 10").await?;
//! # Ok(())
//! # }
//! ```

use std::any::Any;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow::datatypes::SchemaRef;
use async_trait::async_trait;
use datafusion::catalog::Session;
use datafusion::datasource::memory::MemorySourceConfig;
use datafusion::datasource::{TableProvider, TableType};
use datafusion::error::DataFusionError;
use datafusion::error::Result as DFResult;
use datafusion::logical_expr::Expr;
use datafusion::physical_plan::ExecutionPlan;

/// A DataFusion [`TableProvider`] that reads a Parquet file on every scan
/// using the `oxistore_columnar` reader.
///
/// Schema is inferred once at construction time by reading file metadata.
/// Column projection is pushed down to the Parquet reader so unneeded column
/// pages are not decoded.
#[derive(Debug, Clone)]
pub struct ParquetTableProvider {
    path: PathBuf,
    schema: SchemaRef,
}

impl ParquetTableProvider {
    /// Open a Parquet file and infer its Arrow schema from file metadata.
    ///
    /// # Errors
    ///
    /// Returns [`DataFusionError`] if the file does not exist, cannot be read,
    /// or does not contain valid Parquet metadata.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, DataFusionError> {
        let path = path.as_ref().to_path_buf();
        let meta = oxistore_columnar::read_metadata(&path)
            .map_err(|e| DataFusionError::External(Box::new(e)))?;
        Ok(Self {
            path,
            schema: meta.schema,
        })
    }

    /// Return the path of the underlying Parquet file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[async_trait]
impl TableProvider for ParquetTableProvider {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn schema(&self) -> SchemaRef {
        Arc::clone(&self.schema)
    }

    fn table_type(&self) -> TableType {
        TableType::Base
    }

    async fn scan(
        &self,
        _state: &dyn Session,
        projection: Option<&Vec<usize>>,
        _filters: &[Expr],
        _limit: Option<usize>,
    ) -> DFResult<Arc<dyn ExecutionPlan>> {
        let batches = match projection {
            Some(indices) if !indices.is_empty() => {
                oxistore_columnar::read_batches_with_projection(&self.path, indices)
                    .map_err(|e| DataFusionError::External(Box::new(e)))?
            }
            _ => oxistore_columnar::read_batches(&self.path)
                .map_err(|e| DataFusionError::External(Box::new(e)))?,
        };

        // Derive the projected schema from the first batch (if projection applied).
        let projected_schema = if let (Some(indices), Some(first)) = (projection, batches.first()) {
            if indices.is_empty() {
                Arc::clone(&self.schema)
            } else {
                first.schema()
            }
        } else {
            Arc::clone(&self.schema)
        };

        let partitions = vec![batches];
        // Pass `None` for the projection argument: the I/O layer
        // (`read_batches_with_projection`) already produced batches with only the
        // requested columns, so the schema passed here matches those batches
        // exactly.  Re-applying the original (file-relative) projection indices
        // against the already-narrowed schema would produce out-of-bounds errors.
        let exec = MemorySourceConfig::try_new_exec(&partitions, projected_schema, None)?;
        Ok(exec as Arc<dyn ExecutionPlan>)
    }
}
