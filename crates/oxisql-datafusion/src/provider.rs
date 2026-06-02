//! [`OxiSqlTableProvider`]: a DataFusion `TableProvider` backed by a snapshot of
//! `oxisql_core::Row`s held in memory.
//!
//! This is the M4 "in-memory scan" path.  A live-streaming path (M5) will
//! drive a real `oxisql_core::Connection` at plan time and yield batches
//! incrementally.

use std::any::Any;
use std::fmt;
use std::sync::Arc;

use arrow::datatypes::SchemaRef;
use async_trait::async_trait;
use datafusion::catalog::Session;
use datafusion::common::stats::{ColumnStatistics, Precision};
use datafusion::common::Statistics;
use datafusion::datasource::memory::MemorySourceConfig;
use datafusion::datasource::{TableProvider, TableType};
use datafusion::error::Result as DFResult;
use datafusion::logical_expr::{BinaryExpr, Expr, Operator, TableProviderFilterPushDown};
use datafusion::physical_plan::ExecutionPlan;
use datafusion::scalar::ScalarValue;
use oxisql_core::{Row, Value};

use crate::error::OxiSqlFusionError;
use crate::types::rows_to_record_batch;

/// A DataFusion [`TableProvider`] that serves a fixed snapshot of
/// [`oxisql_core::Row`]s as a single Arrow `RecordBatch` partition.
///
/// The provider is cheaply cloneable (rows are stored behind an `Arc`).
/// For live database-backed scans, use the streaming provider introduced in
/// M5 (`OxiSqlStreamProvider`).
///
/// Filter pushdown is supported for simple binary comparisons (`=`, `<>`, `<`,
/// `<=`, `>`, `>=`) and `IS NULL` / `IS NOT NULL` predicates.  Filters are
/// applied in-process after materialization; `Inexact` is reported so DataFusion
/// still applies its own post-filter as a safety net.
///
/// Range-based partitioning is available via [`Self::with_range_partition`], which
/// sorts rows by a key column and splits them into contiguous ranges for
/// parallel DataFusion scans.
#[derive(Clone)]
pub struct OxiSqlTableProvider {
    schema: SchemaRef,
    rows: Arc<Vec<Row>>,
    /// Pre-computed partitions; empty means "treat all rows as one partition".
    partitions: Vec<Arc<Vec<Row>>>,
}

impl fmt::Debug for OxiSqlTableProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OxiSqlTableProvider")
            .field("schema", &self.schema)
            .field("row_count", &self.rows.len())
            .finish()
    }
}

impl OxiSqlTableProvider {
    /// Create a provider from a pre-collected row snapshot and an Arrow schema.
    ///
    /// The `schema` must be consistent with the layout of `rows`: field `i`
    /// should correspond to column position `i` in every row.
    pub fn from_rows(rows: Vec<Row>, schema: SchemaRef) -> Self {
        Self {
            schema,
            rows: Arc::new(rows),
            partitions: Vec::new(),
        }
    }

    /// Construct by executing `SELECT * FROM {table_name}` on `conn`.
    ///
    /// The `schema` must match the columns returned by the query.
    pub async fn from_connection(
        conn: &dyn oxisql_core::Connection,
        table_name: &str,
        schema: SchemaRef,
    ) -> Result<Self, OxiSqlFusionError> {
        let sql = format!("SELECT * FROM {table_name}");
        let rows = conn
            .query(&sql, &[])
            .await
            .map_err(|e| OxiSqlFusionError::OxiSql(e.to_string()))?;
        Ok(Self::from_rows(rows, schema))
    }

    /// Replace the current row snapshot by re-querying `conn`.
    ///
    /// After this call, any subsequent DataFusion scans will see the
    /// refreshed data.
    pub async fn refresh(
        &mut self,
        conn: &dyn oxisql_core::Connection,
        table_name: &str,
    ) -> Result<(), OxiSqlFusionError> {
        let sql = format!("SELECT * FROM {table_name}");
        let rows = conn
            .query(&sql, &[])
            .await
            .map_err(|e| OxiSqlFusionError::OxiSql(e.to_string()))?;
        self.rows = Arc::new(rows);
        Ok(())
    }

    /// Return the number of rows in the current snapshot.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Return `true` if the snapshot contains no rows.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Partition rows by a range key column value for parallel DataFusion scans.
    ///
    /// Rows are sorted by `key_column`'s value (using [`Value`]'s [`PartialOrd`])
    /// and then split into `n_partitions` contiguous chunks.  Each chunk becomes
    /// a separate DataFusion partition, enabling parallel execution and ensuring
    /// that range queries like `WHERE id BETWEEN 1000 AND 2000` scan the fewest
    /// partitions.
    ///
    /// If `n_partitions` is 0 or `key_column` is not found in the schema, the
    /// provider is returned unchanged (no partitioning applied).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let provider = OxiSqlTableProvider::from_rows(rows, schema)
    ///     .with_range_partition("id", 4);
    /// ```
    #[must_use]
    pub fn with_range_partition(mut self, key_column: &str, n_partitions: usize) -> Self {
        if n_partitions == 0 {
            return self;
        }
        // Resolve the column index from the schema.
        let col_idx = match self.schema.index_of(key_column) {
            Ok(idx) => idx,
            Err(_) => return self,
        };

        // Obtain an owned Vec to sort.
        let mut sorted: Vec<Row> = (*self.rows).clone();
        sorted.sort_by(|a, b| {
            let va = a.get_by_index(col_idx);
            let vb = b.get_by_index(col_idx);
            match (va, vb) {
                (Some(l), Some(r)) => l.partial_cmp(r).unwrap_or(std::cmp::Ordering::Equal),
                (None, Some(_)) => std::cmp::Ordering::Less,
                (Some(_), None) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            }
        });

        // Split into n_partitions contiguous chunks.
        let total = sorted.len();
        let chunk_size = total.div_ceil(n_partitions.max(1));
        let parts: Vec<Arc<Vec<Row>>> = sorted
            .chunks(if chunk_size == 0 { 1 } else { chunk_size })
            .map(|chunk| Arc::new(chunk.to_vec()))
            .collect();

        self.rows = Arc::new(sorted);
        self.partitions = parts;
        self
    }
}

// ── Filter helpers ────────────────────────────────────────────────────────────

/// Return `true` if `expr` is a filter we can evaluate in-process against a
/// [`Row`].  We accept simple binary comparisons where at least one side is a
/// column reference and the other is a scalar literal, plus `IS NULL` / `IS NOT
/// NULL` tests.
///
/// Returning `true` here means [`supports_filters_pushdown`] will report
/// `Inexact`, signalling DataFusion that the provider *attempts* the filter but
/// DataFusion must still apply it post-scan as a safety net.
fn is_simple_filter(expr: &Expr) -> bool {
    match expr {
        Expr::BinaryExpr(BinaryExpr { left, op, right }) => {
            matches!(
                op,
                Operator::Eq
                    | Operator::NotEq
                    | Operator::Lt
                    | Operator::LtEq
                    | Operator::Gt
                    | Operator::GtEq
            ) && is_col_or_literal(left)
                && is_col_or_literal(right)
        }
        Expr::IsNull(inner) | Expr::IsNotNull(inner) => is_col_or_literal(inner),
        _ => false,
    }
}

/// Return `true` when `expr` is a column reference or a scalar literal.
fn is_col_or_literal(expr: &Expr) -> bool {
    matches!(expr, Expr::Column(_) | Expr::Literal(_, _))
}

/// Evaluate `expr` against `row` in the context of `schema`.
///
/// This is a best-effort, over-inclusive evaluator.  For any construct that
/// cannot be evaluated (unsupported operator, type mismatch, missing column),
/// the function returns `true` so the row is kept and DataFusion's post-scan
/// filter removes it.
fn eval_filter_on_row(expr: &Expr, row: &Row, schema: &arrow::datatypes::Schema) -> bool {
    match expr {
        Expr::BinaryExpr(BinaryExpr { left, op, right }) => {
            // Extract (column_index, scalar) pairs.
            // We handle `col OP literal` and `literal OP col` forms.
            let (col_idx, scalar, flip) = if let (Expr::Column(col), Expr::Literal(sv, _)) =
                (left.as_ref(), right.as_ref())
            {
                match schema.index_of(col.name.as_str()) {
                    Ok(idx) => (idx, sv, false),
                    Err(_) => return true,
                }
            } else if let (Expr::Literal(sv, _), Expr::Column(col)) =
                (left.as_ref(), right.as_ref())
            {
                match schema.index_of(col.name.as_str()) {
                    Ok(idx) => (idx, sv, true),
                    Err(_) => return true,
                }
            } else {
                return true; // complex expression — keep row
            };

            let row_val = match row.get_by_index(col_idx) {
                Some(v) => v,
                None => return true,
            };

            let ord = compare_value_scalar(row_val, scalar);
            match ord {
                None => true, // incomparable types — keep row
                Some(o) => {
                    // When `flip`, the literal is on the left so invert the
                    // ordering direction before applying the operator.
                    let effective = if flip { o.reverse() } else { o };
                    match op {
                        Operator::Eq => effective == std::cmp::Ordering::Equal,
                        Operator::NotEq => effective != std::cmp::Ordering::Equal,
                        Operator::Lt => effective == std::cmp::Ordering::Less,
                        Operator::LtEq => effective != std::cmp::Ordering::Greater,
                        Operator::Gt => effective == std::cmp::Ordering::Greater,
                        Operator::GtEq => effective != std::cmp::Ordering::Less,
                        _ => true,
                    }
                }
            }
        }
        Expr::IsNull(inner) => {
            if let Expr::Column(col) = inner.as_ref() {
                match schema.index_of(col.name.as_str()) {
                    Ok(idx) => matches!(row.get_by_index(idx), Some(Value::Null) | None),
                    Err(_) => true,
                }
            } else {
                true
            }
        }
        Expr::IsNotNull(inner) => {
            if let Expr::Column(col) = inner.as_ref() {
                match schema.index_of(col.name.as_str()) {
                    Ok(idx) => !matches!(row.get_by_index(idx), Some(Value::Null) | None),
                    Err(_) => true,
                }
            } else {
                true
            }
        }
        _ => true, // unsupported — keep row (over-inclusive)
    }
}

/// Compare a [`Value`] from a row against a DataFusion [`ScalarValue`].
///
/// Returns `None` when the types are incomparable so the caller can decide to
/// keep the row (safe over-inclusion).
fn compare_value_scalar(val: &Value, scalar: &ScalarValue) -> Option<std::cmp::Ordering> {
    match (val, scalar) {
        // Integer comparisons.
        (Value::I64(v), ScalarValue::Int64(Some(s))) => v.partial_cmp(s),
        (Value::I64(v), ScalarValue::Int32(Some(s))) => v.partial_cmp(&i64::from(*s)),
        (Value::I64(v), ScalarValue::Int16(Some(s))) => v.partial_cmp(&i64::from(*s)),
        (Value::I64(v), ScalarValue::Int8(Some(s))) => v.partial_cmp(&i64::from(*s)),
        // Float comparisons (including integer coerced to float).
        (Value::F64(v), ScalarValue::Float64(Some(s))) => v.partial_cmp(s),
        (Value::F64(v), ScalarValue::Float32(Some(s))) => v.partial_cmp(&f64::from(*s)),
        (Value::I64(v), ScalarValue::Float64(Some(s))) => (*v as f64).partial_cmp(s),
        (Value::I64(v), ScalarValue::Float32(Some(s))) => (*v as f64).partial_cmp(&f64::from(*s)),
        // Text comparisons.
        (Value::Text(v), ScalarValue::Utf8(Some(s)))
        | (Value::Text(v), ScalarValue::LargeUtf8(Some(s))) => v.as_str().partial_cmp(s.as_str()),
        // Boolean comparisons.
        (Value::Bool(v), ScalarValue::Boolean(Some(s))) => v.partial_cmp(s),
        // NULL scalar — only EQ / IS NULL can match.
        (Value::Null, ScalarValue::Null)
        | (Value::Null, ScalarValue::Int64(None))
        | (Value::Null, ScalarValue::Int32(None))
        | (Value::Null, ScalarValue::Int16(None))
        | (Value::Null, ScalarValue::Int8(None))
        | (Value::Null, ScalarValue::Float64(None))
        | (Value::Null, ScalarValue::Float32(None))
        | (Value::Null, ScalarValue::Boolean(None))
        | (Value::Null, ScalarValue::Utf8(None))
        | (Value::Null, ScalarValue::LargeUtf8(None)) => Some(std::cmp::Ordering::Equal),
        _ => None,
    }
}

// ── TableProvider impl ────────────────────────────────────────────────────────

#[async_trait]
impl TableProvider for OxiSqlTableProvider {
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
        filters: &[Expr],
        _limit: Option<usize>,
    ) -> DFResult<Arc<dyn ExecutionPlan>> {
        // Decide which row slices to materialise into RecordBatches.
        // When explicit partitions are stored, use them; otherwise treat the
        // full snapshot as a single partition.
        let source_partitions: Vec<&[Row]> = if self.partitions.is_empty() {
            vec![self.rows.as_slice()]
        } else {
            self.partitions.iter().map(|p| p.as_slice()).collect()
        };

        // Apply simple in-memory filter pushdown before building RecordBatches.
        let schema_ref = Arc::clone(&self.schema);
        let df_err =
            |e: OxiSqlFusionError| datafusion::error::DataFusionError::External(Box::new(e));

        let mut partitions: Vec<Vec<arrow::record_batch::RecordBatch>> =
            Vec::with_capacity(source_partitions.len());

        for slice in source_partitions {
            // Collect indices of rows that pass all pushed-down filters.
            let kept: Vec<Row> = if filters.is_empty() {
                slice.to_vec()
            } else {
                slice
                    .iter()
                    .filter(|row| {
                        filters
                            .iter()
                            .all(|f| eval_filter_on_row(f, row, &schema_ref))
                    })
                    .cloned()
                    .collect()
            };

            let batch = rows_to_record_batch(kept, Arc::clone(&schema_ref)).map_err(df_err)?;
            partitions.push(vec![batch]);
        }

        let exec = MemorySourceConfig::try_new_exec(
            &partitions,
            Arc::clone(&self.schema),
            projection.cloned(),
        )?;
        Ok(exec as Arc<dyn ExecutionPlan>)
    }

    fn supports_filters_pushdown(
        &self,
        filters: &[&Expr],
    ) -> DFResult<Vec<TableProviderFilterPushDown>> {
        Ok(filters
            .iter()
            .map(|f| {
                if is_simple_filter(f) {
                    TableProviderFilterPushDown::Inexact
                } else {
                    TableProviderFilterPushDown::Unsupported
                }
            })
            .collect())
    }

    fn statistics(&self) -> Option<Statistics> {
        let n_cols = self.schema.fields().len();
        let col_stats: Vec<ColumnStatistics> = (0..n_cols)
            .map(|_| ColumnStatistics::new_unknown())
            .collect();
        let mut stats = Statistics::default()
            .with_num_rows(Precision::Exact(self.rows.len()))
            .with_total_byte_size(Precision::Absent);
        stats.column_statistics = col_stats;
        Some(stats)
    }
}

impl fmt::Display for OxiSqlTableProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "OxiSqlTableProvider(rows={}, cols={})",
            self.rows.len(),
            self.schema.fields().len()
        )
    }
}
