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
use datafusion::datasource::{TableProvider, TableType};
use datafusion::error::Result as DFResult;
use datafusion::logical_expr::{
    Between, BinaryExpr, Expr, Like, Operator, TableProviderFilterPushDown,
};
use datafusion::physical_plan::ExecutionPlan;
use datafusion::scalar::ScalarValue;
use datafusion_datasource::memory::MemorySourceConfig;
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

    /// Return the number of explicit partitions stored, or `1` when no
    /// partitioning has been applied (all rows treated as one partition).
    pub fn partition_count(&self) -> usize {
        if self.partitions.is_empty() {
            1
        } else {
            self.partitions.len()
        }
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

    /// Automatically partition the snapshot data for parallel execution.
    ///
    /// Chooses the partition key (first column) and number of partitions based
    /// on the row count.  The formula is
    /// `max(1, min(n_parallel, row_count / target_batch_size))`.
    ///
    /// - `n_parallel`: maximum number of partitions (typically the number of CPU
    ///   threads available to the DataFusion session).
    /// - `target_batch_size`: target rows per partition.  A value of `8192` is a
    ///   reasonable default for most workloads.
    ///
    /// Uses range partitioning on the first schema column when
    /// `row_count > target_batch_size` and `n_parallel > 1`, leaving the data
    /// as a single partition otherwise.
    ///
    /// Returns the provider unchanged (no-op) when the schema has no columns or
    /// when partitioning would not help (too few rows or `n_parallel <= 1`).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let provider = OxiSqlTableProvider::from_rows(rows, schema)
    ///     .with_auto_partition(num_cpus::get(), 8192);
    /// ```
    #[must_use]
    pub fn with_auto_partition(self, n_parallel: usize, target_batch_size: usize) -> Self {
        let total = self.rows.len();
        if total <= target_batch_size || n_parallel <= 1 {
            return self;
        }
        let n = (total / target_batch_size).min(n_parallel).max(1);
        let first_col = self.schema.fields().first().map(|f| f.name().clone());
        match first_col {
            Some(col) => self.with_range_partition(&col, n),
            None => self,
        }
    }

    /// Partition rows into `n` buckets by hashing the value of `key_column`.
    ///
    /// Each row is placed into bucket `hash(row[key_column]) % n`.  Buckets
    /// become separate DataFusion partitions, enabling parallel execution.
    /// Unlike [`Self::with_range_partition`], rows within each bucket have no
    /// guaranteed ordering.
    ///
    /// Returns an error if `n == 0` or `key_column` is not found in the schema.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let provider = OxiSqlTableProvider::from_rows(rows, schema)
    ///     .with_hash_partition("user_id", 8)?;
    /// ```
    pub fn with_hash_partition(
        mut self,
        key_column: &str,
        n: usize,
    ) -> Result<Self, OxiSqlFusionError> {
        if n == 0 {
            return Err(OxiSqlFusionError::OxiSql(
                "with_hash_partition: n must be greater than 0".into(),
            ));
        }
        let col_idx = self.schema.index_of(key_column).map_err(|_| {
            OxiSqlFusionError::OxiSql(format!(
                "with_hash_partition: column '{key_column}' not found in schema"
            ))
        })?;

        let mut buckets: Vec<Vec<Row>> = (0..n).map(|_| Vec::new()).collect();
        for row in self.rows.as_ref() {
            let val = row.get_by_index(col_idx);
            let h = hash_value(val.unwrap_or(&Value::Null));
            let bucket = (h % (n as u64)) as usize;
            buckets[bucket].push(row.clone());
        }

        self.partitions = buckets.into_iter().map(Arc::new).collect();
        Ok(self)
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
        Expr::BinaryExpr(BinaryExpr { left, op, right }) => match op {
            Operator::Eq
            | Operator::NotEq
            | Operator::Lt
            | Operator::LtEq
            | Operator::Gt
            | Operator::GtEq => is_col_or_literal(left) && is_col_or_literal(right),
            Operator::And | Operator::Or => is_simple_filter(left) && is_simple_filter(right),
            _ => false,
        },
        Expr::IsNull(inner) | Expr::IsNotNull(inner) => is_col_or_literal(inner),
        Expr::Not(inner) => is_simple_filter(inner),
        // `col IN (lit, lit, …)` / `col NOT IN (…)`
        Expr::InList(inlist) => {
            is_col_or_literal(&inlist.expr) && inlist.list.iter().all(is_col_or_literal)
        }
        // `col BETWEEN low AND high`
        Expr::Between(Between {
            expr, low, high, ..
        }) => is_col_or_literal(expr) && is_col_or_literal(low) && is_col_or_literal(high),
        // `col LIKE 'pat'` / `col ILIKE 'pat'` and negations
        Expr::Like(Like { expr, pattern, .. }) => {
            is_col_or_literal(expr) && is_col_or_literal(pattern)
        }
        _ => false,
    }
}

/// Return `true` when `expr` is a column reference or a scalar literal.
fn is_col_or_literal(expr: &Expr) -> bool {
    matches!(expr, Expr::Column(_) | Expr::Literal(_, _))
}

/// Convert a DataFusion [`ScalarValue`] to an OxiSQL [`Value`].
///
/// Returns `None` for scalar types that have no straightforward OxiSQL mapping
/// (e.g. Decimal128, Date32, complex types).  Typed NULLs of any integer /
/// float / bool / text type are mapped to [`Value::Null`].
fn scalar_to_value(scalar: &ScalarValue) -> Option<Value> {
    match scalar {
        ScalarValue::Int8(Some(v)) => Some(Value::I64(i64::from(*v))),
        ScalarValue::Int16(Some(v)) => Some(Value::I64(i64::from(*v))),
        ScalarValue::Int32(Some(v)) => Some(Value::I64(i64::from(*v))),
        ScalarValue::Int64(Some(v)) => Some(Value::I64(*v)),
        ScalarValue::Float32(Some(v)) => Some(Value::F64(f64::from(*v))),
        ScalarValue::Float64(Some(v)) => Some(Value::F64(*v)),
        ScalarValue::Boolean(Some(v)) => Some(Value::Bool(*v)),
        ScalarValue::Utf8(Some(s)) | ScalarValue::LargeUtf8(Some(s)) => {
            Some(Value::Text(s.clone()))
        }
        // Any typed NULL → Value::Null.
        ScalarValue::Null
        | ScalarValue::Int8(None)
        | ScalarValue::Int16(None)
        | ScalarValue::Int32(None)
        | ScalarValue::Int64(None)
        | ScalarValue::Float32(None)
        | ScalarValue::Float64(None)
        | ScalarValue::Boolean(None)
        | ScalarValue::Utf8(None)
        | ScalarValue::LargeUtf8(None) => Some(Value::Null),
        _ => None,
    }
}

/// Evaluate a leaf [`Expr`] (column reference or literal) against `row` to
/// produce an OxiSQL [`Value`].  Returns `None` for compound or unsupported
/// expressions.
fn eval_expr_to_value(expr: &Expr, row: &Row, schema: &arrow::datatypes::Schema) -> Option<Value> {
    match expr {
        Expr::Column(col) => {
            let idx = schema.index_of(col.name.as_str()).ok()?;
            row.get_by_index(idx).cloned()
        }
        Expr::Literal(sv, _) => scalar_to_value(sv),
        _ => None,
    }
}

/// SQL LIKE pattern matching (`%` = any substring, `_` = any single char).
///
/// When `case_insensitive` is `true`, both sides are lower-cased before
/// matching (implements ILIKE semantics).
fn sql_like_match(text: &str, pattern: &str, case_insensitive: bool) -> bool {
    let (t, p) = if case_insensitive {
        (text.to_lowercase(), pattern.to_lowercase())
    } else {
        (text.to_owned(), pattern.to_owned())
    };
    let text_chars: Vec<char> = t.chars().collect();
    let pat_chars: Vec<char> = p.chars().collect();
    like_match(&text_chars, &pat_chars)
}

fn like_match(text: &[char], pattern: &[char]) -> bool {
    match (text, pattern) {
        // Pattern exhausted: match only if text is also exhausted.
        (_, []) => text.is_empty(),
        // `%` matches zero or more characters.
        (_, ['%', rest @ ..]) => {
            for i in 0..=text.len() {
                if like_match(&text[i..], rest) {
                    return true;
                }
            }
            false
        }
        // No text left but pattern still has non-`%` characters.
        ([], _) => false,
        // `_` matches exactly one character.
        ([_, tr @ ..], ['_', pr @ ..]) => like_match(tr, pr),
        // Literal character match.
        ([tc, tr @ ..], [pc, pr @ ..]) => tc == pc && like_match(tr, pr),
    }
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
            // Short-circuit boolean connectives first.
            match op {
                Operator::And => {
                    return eval_filter_on_row(left, row, schema)
                        && eval_filter_on_row(right, row, schema);
                }
                Operator::Or => {
                    return eval_filter_on_row(left, row, schema)
                        || eval_filter_on_row(right, row, schema);
                }
                _ => {}
            }

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
        Expr::Not(inner) => !eval_filter_on_row(inner, row, schema),
        // `col IN (a, b, c)` / `col NOT IN (…)`
        Expr::InList(inlist) => match eval_expr_to_value(&inlist.expr, row, schema) {
            None => true,              // can't evaluate — keep row
            Some(Value::Null) => true, // NULL IN (...) = NULL (unknown) — keep row
            Some(v) => {
                let in_list = inlist.list.iter().any(|item| {
                    if let Some(item_val) = eval_expr_to_value(item, row, schema) {
                        v.partial_cmp(&item_val) == Some(std::cmp::Ordering::Equal)
                    } else {
                        false
                    }
                });
                if inlist.negated {
                    !in_list
                } else {
                    in_list
                }
            }
        },
        // `col BETWEEN low AND high`
        Expr::Between(Between {
            expr,
            low,
            high,
            negated,
        }) => {
            let val = eval_expr_to_value(expr, row, schema);
            let lo = eval_expr_to_value(low, row, schema);
            let hi = eval_expr_to_value(high, row, schema);
            match (val, lo, hi) {
                (Some(v), Some(l), Some(h)) => {
                    let above_low = v
                        .partial_cmp(&l)
                        .map(|o| o != std::cmp::Ordering::Less)
                        .unwrap_or(true);
                    let below_high = v
                        .partial_cmp(&h)
                        .map(|o| o != std::cmp::Ordering::Greater)
                        .unwrap_or(true);
                    let in_range = above_low && below_high;
                    if *negated {
                        !in_range
                    } else {
                        in_range
                    }
                }
                _ => true, // evaluation failure — keep row
            }
        }
        // `col LIKE 'pat'` / `col ILIKE 'pat'` and negations
        Expr::Like(Like {
            expr,
            pattern,
            negated,
            case_insensitive,
            ..
        }) => {
            let text_val = eval_expr_to_value(expr, row, schema);
            let pattern_val = eval_expr_to_value(pattern, row, schema);
            match (text_val, pattern_val) {
                (Some(Value::Text(text)), Some(Value::Text(pat))) => {
                    let matched = sql_like_match(&text, &pat, *case_insensitive);
                    if *negated {
                        !matched
                    } else {
                        matched
                    }
                }
                _ => true, // evaluation failure — keep row
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

/// Hash an OxiSQL [`Value`] to a `u64` suitable for bucket assignment.
///
/// `Value::F64` NaN is canonicalised to `u64::MAX` so all NaN values land in
/// the same bucket.  `Value::Null` always hashes to `0`.
fn hash_value(val: &Value) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    match val {
        Value::I64(i) => i.hash(&mut h),
        Value::F64(f) => {
            let bits = if f.is_nan() { u64::MAX } else { f.to_bits() };
            bits.hash(&mut h);
        }
        Value::Text(s) => s.hash(&mut h),
        Value::Blob(b) => b.hash(&mut h),
        Value::Bool(b) => b.hash(&mut h),
        Value::Null => 0u64.hash(&mut h),
        Value::Timestamp(t) => t.hash(&mut h),
        Value::Date(d) => d.hash(&mut h),
        Value::Time(t) => t.hash(&mut h),
        Value::Uuid(u) => u.hash(&mut h),
        Value::Json(s) | Value::Decimal(s) => s.hash(&mut h),
        Value::Array(arr) => {
            for v in arr {
                hash_value(v).hash(&mut h);
            }
        }
        Value::TypedArray { values, .. } => {
            for v in values {
                hash_value(v).hash(&mut h);
            }
        }
    }
    h.finish()
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
