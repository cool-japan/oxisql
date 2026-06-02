//! [`OxiSqlContext`] — a DataFusion [`SessionContext`] wrapper pre-configured
//! for OxiSQL backends.
//!
//! Provides convenience methods for registering OxiSQL-backed tables and
//! executing SQL queries.

use std::sync::Arc;

use arrow::array::ArrayRef;
use arrow::datatypes::{DataType, Field, Schema, SchemaRef};
use datafusion::arrow::array::RecordBatch;
use datafusion::error::DataFusionError;
use datafusion::logical_expr::{create_udaf, create_udf, AccumulatorFactoryFunction, Volatility};
use datafusion::physical_plan::displayable;
use datafusion::prelude::{DataFrame, SessionContext};
use oxisql_core::Connection;

use crate::error::OxiSqlFusionError;
use crate::provider::OxiSqlTableProvider;
use crate::stream::OxiSqlStreamProvider;
use crate::types::value_to_arrow_type;

/// A DataFusion [`SessionContext`] wrapper with convenience methods for
/// registering OxiSQL-backed tables and executing SQL.
///
/// # Example
///
/// ```rust,no_run
/// # #[tokio::main]
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// // use oxisql_datafusion::OxiSqlContext;
/// // use std::sync::Arc;
/// // use arrow::datatypes::{DataType, Field, Schema};
/// //
/// // let conn = oxisql_embedded::EmbeddedConnection::open_memory()?;
/// // // ... create tables and insert data ...
/// // let conn_arc = Arc::new(conn) as Arc<dyn oxisql_core::Connection>;
/// //
/// // let schema = Arc::new(Schema::new(vec![
/// //     Field::new("id", DataType::Int64, true),
/// //     Field::new("name", DataType::Utf8, true),
/// // ]));
/// //
/// // let ctx = OxiSqlContext::new();
/// // ctx.register_table("users", conn_arc, schema)?;
/// // let batches = ctx.execute_sql("SELECT * FROM users WHERE id > 1").await?;
/// # Ok(())
/// # }
/// ```
pub struct OxiSqlContext {
    inner: SessionContext,
}

impl OxiSqlContext {
    /// Create a new `OxiSqlContext` with default DataFusion settings.
    pub fn new() -> Self {
        Self {
            inner: SessionContext::new(),
        }
    }

    /// Create an `OxiSqlContext` from an existing [`SessionContext`].
    pub fn from_session_context(ctx: SessionContext) -> Self {
        Self { inner: ctx }
    }

    /// Register a live OxiSQL connection as a DataFusion table.
    ///
    /// This creates an [`OxiSqlStreamProvider`] backed by `conn` and registers
    /// it under `name` in the default catalog.  SQL queries referencing `name`
    /// will be executed against `conn` at scan time.
    ///
    /// # Errors
    ///
    /// Returns [`OxiSqlFusionError`] if registration fails (e.g. a table with
    /// the same name already exists and `replace` is `false`).
    pub fn register_table(
        &self,
        name: &str,
        conn: Arc<dyn Connection>,
        schema: SchemaRef,
    ) -> Result<(), OxiSqlFusionError> {
        let provider = Arc::new(OxiSqlStreamProvider::new(conn, name, schema));
        self.inner
            .register_table(name, provider)
            .map(|_| ())
            .map_err(|e| OxiSqlFusionError::OxiSql(e.to_string()))
    }

    /// Register a pre-built snapshot of rows as a DataFusion table.
    ///
    /// Use this when you want to register a static snapshot (not a live connection).
    pub fn register_snapshot(
        &self,
        name: &str,
        rows: Vec<oxisql_core::Row>,
        schema: SchemaRef,
    ) -> Result<(), OxiSqlFusionError> {
        use crate::provider::OxiSqlTableProvider;
        let provider = Arc::new(OxiSqlTableProvider::from_rows(rows, schema));
        self.inner
            .register_table(name, provider)
            .map(|_| ())
            .map_err(|e| OxiSqlFusionError::OxiSql(e.to_string()))
    }

    /// Deregister a table by name.
    ///
    /// Returns `Ok(true)` if the table existed and was removed, `Ok(false)`
    /// if no table with that name was registered.
    pub fn deregister_table(&self, name: &str) -> Result<bool, OxiSqlFusionError> {
        self.inner
            .deregister_table(name)
            .map(|opt| opt.is_some())
            .map_err(|e| OxiSqlFusionError::OxiSql(e.to_string()))
    }

    /// Create a [`DataFrame`] from a SQL string.
    ///
    /// The SQL can reference any table previously registered via
    /// `register_table` or `register_snapshot`.
    ///
    /// # Errors
    ///
    /// Returns [`OxiSqlFusionError`] if the SQL cannot be parsed or planned.
    pub async fn sql(&self, sql: &str) -> Result<DataFrame, OxiSqlFusionError> {
        self.inner
            .sql(sql)
            .await
            .map_err(|e| OxiSqlFusionError::OxiSql(e.to_string()))
    }

    /// Execute a SQL query and collect all result batches.
    ///
    /// Convenience wrapper around [`Self::sql`] + `collect()`.  For large result
    /// sets, use [`Self::sql`] and stream the batches manually.
    ///
    /// # Errors
    ///
    /// Returns [`OxiSqlFusionError`] if planning or execution fails.
    pub async fn execute_sql(&self, sql: &str) -> Result<Vec<RecordBatch>, OxiSqlFusionError> {
        let df = self.sql(sql).await?;
        df.collect()
            .await
            .map_err(|e| OxiSqlFusionError::OxiSql(e.to_string()))
    }

    /// Register a SQL view in this context.
    ///
    /// The view is defined by a SELECT statement; subsequent queries can
    /// reference it by `name`.  This calls `CREATE VIEW name AS sql` against
    /// the inner DataFusion session.
    ///
    /// # Errors
    ///
    /// Returns [`datafusion::error::DataFusionError`] if the view cannot be
    /// created (e.g. bad SQL, duplicate name).
    pub async fn register_view(
        &self,
        name: &str,
        sql: &str,
    ) -> Result<(), datafusion::error::DataFusionError> {
        self.inner
            .sql(&format!("CREATE VIEW {name} AS {sql}"))
            .await?;
        Ok(())
    }

    /// Return a reference to the inner [`SessionContext`] for advanced use.
    pub fn session_context(&self) -> &SessionContext {
        &self.inner
    }

    /// Consume this context and return the inner [`SessionContext`].
    pub fn into_session_context(self) -> SessionContext {
        self.inner
    }

    /// Register a user-defined scalar function (UDF).
    ///
    /// Wraps `func` — which operates on [`ArrayRef`] slices — in a DataFusion
    /// [`ScalarUDF`] and registers it under `name`.  The function receives one
    /// `ArrayRef` per parameter declared in `param_types` and must return an
    /// `ArrayRef` whose data-type matches `return_type`.
    ///
    /// [`ScalarUDF`]: datafusion::logical_expr::ScalarUDF
    ///
    /// # Errors
    ///
    /// Currently infallible (DataFusion's `register_udf` silently overwrites),
    /// but returns [`OxiSqlFusionError`] for future-compatibility.
    pub fn register_scalar_function(
        &self,
        name: &str,
        return_type: DataType,
        param_types: Vec<DataType>,
        func: impl Fn(&[ArrayRef]) -> Result<ArrayRef, DataFusionError> + Send + Sync + 'static,
    ) -> Result<(), OxiSqlFusionError> {
        use datafusion::logical_expr::ColumnarValue;
        let func = Arc::new(func);
        let implementation = Arc::new(
            move |args: &[ColumnarValue]| -> Result<ColumnarValue, DataFusionError> {
                // Convert ColumnarValue slice to ArrayRef slice.
                let arrays = ColumnarValue::values_to_arrays(args)?;
                let result = func(&arrays)?;
                Ok(ColumnarValue::Array(result))
            },
        );
        let udf = create_udf(
            name,
            param_types,
            return_type,
            Volatility::Immutable,
            implementation,
        );
        self.inner.register_udf(udf);
        Ok(())
    }

    /// Register a user-defined aggregate function (UDAF).
    ///
    /// Wraps the provided [`AccumulatorFactoryFunction`] in a DataFusion
    /// [`AggregateUDF`] and registers it under `name`.
    ///
    /// The `state_types` slice lists the Arrow [`DataType`]s of the intermediate
    /// accumulator state (used for partial aggregation in distributed plans).
    ///
    /// [`AggregateUDF`]: datafusion::logical_expr::AggregateUDF
    ///
    /// # Errors
    ///
    /// Currently infallible (DataFusion's `register_udaf` silently overwrites),
    /// but returns [`OxiSqlFusionError`] for future-compatibility.
    pub fn register_aggregate_function(
        &self,
        name: &str,
        input_types: Vec<DataType>,
        return_type: DataType,
        accumulator: AccumulatorFactoryFunction,
        state_types: Vec<DataType>,
    ) -> Result<(), OxiSqlFusionError> {
        let udaf = create_udaf(
            name,
            input_types,
            Arc::new(return_type),
            Volatility::Immutable,
            accumulator,
            Arc::new(state_types),
        );
        self.inner.register_udaf(udaf);
        Ok(())
    }

    /// Register a snapshot of an embedded table in this DataFusion context.
    ///
    /// This is a convenience wrapper around the free function
    /// [`register_embedded_table`] that uses the inner [`SessionContext`]
    /// already held by this `OxiSqlContext`.
    ///
    /// The `conn` may be any [`Connection`] implementation (not only
    /// `EmbeddedConnection`); the function simply issues `SELECT * FROM
    /// {table_name}` and materialises the rows into an [`OxiSqlTableProvider`]
    /// snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`OxiSqlFusionError`] if the query or registration fails.
    pub async fn register_embedded_table(
        &self,
        conn: &dyn Connection,
        table_name: &str,
    ) -> Result<(), OxiSqlFusionError> {
        register_embedded_table(&self.inner, conn, table_name).await
    }

    /// Return a textual representation of the DataFusion logical and physical
    /// plans for `sql`.
    ///
    /// The returned string contains two sections separated by a blank line:
    ///
    /// ```text
    /// == Logical Plan ==
    /// <indent-formatted logical plan>
    ///
    /// == Physical Plan ==
    /// <indent-formatted physical plan>
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`OxiSqlFusionError`] if the SQL cannot be parsed, planned, or
    /// compiled to a physical plan.
    pub async fn explain_plan(&self, sql: &str) -> Result<String, OxiSqlFusionError> {
        let df = self.sql(sql).await?;
        let logical = format!("{}", df.logical_plan().display_indent());
        let physical_plan = df
            .create_physical_plan()
            .await
            .map_err(OxiSqlFusionError::DataFusion)?;
        let physical = displayable(physical_plan.as_ref()).indent(true).to_string();
        Ok(format!(
            "== Logical Plan ==\n{logical}\n\n== Physical Plan ==\n{physical}"
        ))
    }
}

impl Default for OxiSqlContext {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "columnar")]
impl OxiSqlContext {
    /// Register a Parquet file as a DataFusion table.
    ///
    /// The Arrow schema is inferred from the file's Parquet metadata.
    /// Subsequent SQL queries can reference the table by `name`.
    ///
    /// # Errors
    ///
    /// Returns [`OxiSqlFusionError`] if the file cannot be opened, its
    /// metadata is invalid, or a table with the same name already exists.
    pub fn register_parquet(
        &self,
        name: &str,
        path: impl AsRef<std::path::Path>,
    ) -> Result<(), OxiSqlFusionError> {
        let provider = crate::parquet::ParquetTableProvider::open(path)?;
        self.inner
            .register_table(name, Arc::new(provider))
            .map(|_| ())
            .map_err(OxiSqlFusionError::DataFusion)
    }
}

/// Register an OxiSQL table in a DataFusion [`SessionContext`].
///
/// Equivalent to constructing an [`OxiSqlStreamProvider`] and registering it
/// directly.  Use this when you already have a `SessionContext` and don't
/// want to wrap it in an [`OxiSqlContext`].
pub fn register_oxisql_table(
    ctx: &SessionContext,
    name: &str,
    conn: Arc<dyn Connection>,
    schema: SchemaRef,
) -> Result<(), OxiSqlFusionError> {
    let provider = Arc::new(OxiSqlStreamProvider::new(conn, name, schema));
    ctx.register_table(name, provider)
        .map(|_| ())
        .map_err(|e| OxiSqlFusionError::OxiSql(e.to_string()))
}

/// Infer an Arrow [`Schema`] from the first row of a result set.
///
/// Column names are taken from the row's label list; Arrow types are derived
/// via [`value_to_arrow_type`], falling back to [`DataType::Utf8`] for
/// [`Value::Null`] (unknown type).  All fields are declared nullable so that
/// subsequent rows with `NULL` values can be represented without error.
///
/// Returns `None` if the row has no columns.
fn infer_schema_from_first_row(row: &oxisql_core::Row) -> Option<SchemaRef> {
    let labels = row.columns();
    if labels.is_empty() {
        return None;
    }
    let fields: Vec<Field> = labels
        .iter()
        .enumerate()
        .map(|(idx, name)| {
            let dtype = row
                .get_by_index(idx)
                .and_then(value_to_arrow_type)
                .unwrap_or(DataType::Utf8);
            Field::new(name.as_str(), dtype, true)
        })
        .collect();
    Some(Arc::new(Schema::new(fields)))
}

/// Register all rows from a live [`Connection`] table in a DataFusion
/// [`SessionContext`] as a snapshot [`OxiSqlTableProvider`].
///
/// Executes `SELECT * FROM {table_name}` on `conn`, infers the Arrow schema
/// from the first row's column names and value types, then registers an
/// [`OxiSqlTableProvider`] snapshot under `table_name`.
///
/// # Empty tables
///
/// When the query returns no rows the table cannot be introspected for a schema
/// and registration is skipped (the function returns `Ok(())` without touching
/// the DataFusion catalog).
///
/// # Errors
///
/// Returns [`OxiSqlFusionError::OxiSql`] when the query fails.
/// Returns [`OxiSqlFusionError::DataFusion`] when table registration fails
/// (e.g. a table with the same name is already registered).
pub async fn register_embedded_table(
    ctx: &SessionContext,
    conn: &dyn Connection,
    table_name: &str,
) -> Result<(), OxiSqlFusionError> {
    let rows = conn
        .query(&format!("SELECT * FROM {table_name}"), &[])
        .await
        .map_err(|e| OxiSqlFusionError::OxiSql(e.to_string()))?;

    if rows.is_empty() {
        // Cannot infer schema from an empty result — skip registration.
        return Ok(());
    }

    let schema = infer_schema_from_first_row(&rows[0]).ok_or_else(|| {
        OxiSqlFusionError::OxiSql(format!(
            "table '{table_name}' returned rows with no columns"
        ))
    })?;

    let provider = OxiSqlTableProvider::from_rows(rows, schema);
    ctx.register_table(table_name, Arc::new(provider))
        .map(|_| ())
        .map_err(OxiSqlFusionError::DataFusion)
}
