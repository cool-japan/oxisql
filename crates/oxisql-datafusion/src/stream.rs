//! Live-streaming DataFusion table provider backed by a real OxiSQL connection.
//!
//! Unlike [`OxiSqlTableProvider`], which materialises all rows at construction
//! time, [`OxiSqlStreamProvider`] queries the database at plan-execution time,
//! translating DataFusion filter and projection hints into SQL WHERE / SELECT
//! clauses so that the backend does the heavy lifting.
//!
//! [`OxiSqlTableProvider`]: crate::OxiSqlTableProvider

use std::any::Any;
use std::fmt;
use std::sync::Arc;

use arrow::datatypes::SchemaRef;
use async_trait::async_trait;
use datafusion::catalog::Session;
use datafusion::datasource::{TableProvider, TableType};
use datafusion::error::Result as DFResult;
use datafusion::execution::TaskContext;
use datafusion::logical_expr::{BinaryExpr, Expr, Operator, TableProviderFilterPushDown};
use datafusion::physical_expr::EquivalenceProperties;
use datafusion::physical_plan::execution_plan::{Boundedness, EmissionType};
use datafusion::physical_plan::stream::RecordBatchStreamAdapter;
use datafusion::physical_plan::{
    DisplayAs, DisplayFormatType, ExecutionPlan, Partitioning, PlanProperties,
    SendableRecordBatchStream,
};
use datafusion::scalar::ScalarValue;
use oxisql_core::Connection;

// ── Sort order ────────────────────────────────────────────────────────────────

/// Sort direction for an `ORDER BY` column pushed down to the SQL backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortOrder {
    /// Sort ascending (default SQL ordering).
    Asc,
    /// Sort descending.
    Desc,
}

impl fmt::Display for SortOrder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SortOrder::Asc => f.write_str("ASC"),
            SortOrder::Desc => f.write_str("DESC"),
        }
    }
}

/// A DataFusion [`TableProvider`] that executes a live SQL query at scan time,
/// pushing filters and projections down to the OxiSQL backend.
///
/// # Filter pushdown
///
/// Simple comparison predicates (`=`, `<>`, `<`, `<=`, `>`, `>=`, `AND`, `OR`,
/// `IS NULL`, `IS NOT NULL`, `NOT`) are converted to SQL WHERE clauses.
/// Complex expressions (functions, subqueries, arithmetic) are left to
/// DataFusion's post-scan filtering.
///
/// # Projection pushdown
///
/// When DataFusion requests specific columns, the provider generates
/// `SELECT col1, col2, ...` instead of `SELECT *`.
///
/// # Limit pushdown
///
/// When DataFusion specifies a limit, the provider appends `LIMIT N` to the
/// query, reducing the number of rows transferred.
///
/// # Sort pushdown
///
/// An optional `ORDER BY` clause can be injected into the generated SQL via
/// [`Self::with_sort`].  Each entry is a `(column_name, SortOrder)` pair.
pub struct OxiSqlStreamProvider {
    schema: SchemaRef,
    table_name: String,
    conn: Arc<dyn Connection>,
    /// Optional sort order to push to the backend as an `ORDER BY` clause.
    sort_order: Option<Vec<(String, SortOrder)>>,
}

impl OxiSqlStreamProvider {
    /// Construct from a connection, table name, and Arrow schema.
    ///
    /// The `schema` must match the column layout returned by the backend for
    /// `SELECT * FROM table_name`.  Column names in the schema are used
    /// verbatim as SQL column references.
    pub fn new(
        conn: Arc<dyn Connection>,
        table_name: impl Into<String>,
        schema: SchemaRef,
    ) -> Self {
        Self {
            schema,
            table_name: table_name.into(),
            conn,
            sort_order: None,
        }
    }

    /// Attach an `ORDER BY` clause to the SQL generated at scan time.
    ///
    /// Each entry is a `(column_name, SortOrder)` pair.  The resulting SQL
    /// fragment is appended between the WHERE clause and any LIMIT clause.
    ///
    /// This is a "sort-into-SQL" approach: the backend database performs the
    /// ordering, reducing the work DataFusion must do on the result set.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use oxisql_datafusion::stream::{OxiSqlStreamProvider, SortOrder};
    ///
    /// let provider = OxiSqlStreamProvider::new(conn, "users", schema)
    ///     .with_sort(vec![
    ///         ("score".into(), SortOrder::Desc),
    ///         ("id".into(), SortOrder::Asc),
    ///     ]);
    /// ```
    #[must_use]
    pub fn with_sort(mut self, order: Vec<(String, SortOrder)>) -> Self {
        self.sort_order = Some(order);
        self
    }

    /// Return the configured sort order, if any.
    ///
    /// Returns `None` if no sort has been attached via [`Self::with_sort`].
    pub fn sort_order(&self) -> Option<&[(String, SortOrder)]> {
        self.sort_order.as_deref()
    }

    /// Construct from a live [`oxisql_mysql::MyConnection`] for streaming MySQL
    /// query results through DataFusion.
    ///
    /// The connection is wrapped in an `Arc` and used as the underlying
    /// [`Connection`] trait object.  All filter, projection, limit, and sort
    /// pushdown features of [`OxiSqlStreamProvider`] are available on the
    /// resulting provider.
    ///
    /// # Feature flag
    ///
    /// Only available when the `mysql` Cargo feature of `oxisql-datafusion` is
    /// enabled.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use std::sync::Arc;
    /// use arrow::datatypes::{DataType, Field, Schema};
    /// use oxisql_mysql::{MyConnection, TlsMode};
    /// use oxisql_datafusion::stream::OxiSqlStreamProvider;
    ///
    /// let conn = MyConnection::connect(
    ///     "mysql://root:secret@localhost:3306/mydb",
    ///     TlsMode::Disabled,
    /// ).await?;
    ///
    /// let schema = Arc::new(Schema::new(vec![
    ///     Field::new("id",   DataType::Int64, true),
    ///     Field::new("name", DataType::Utf8,  true),
    /// ]));
    ///
    /// let provider = OxiSqlStreamProvider::from_mysql(conn, "users", schema);
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(feature = "mysql")]
    pub fn from_mysql(
        conn: oxisql_mysql::MyConnection,
        table_name: impl Into<String>,
        schema: SchemaRef,
    ) -> Self {
        Self::new(Arc::new(conn) as Arc<dyn Connection>, table_name, schema)
    }

    /// Construct from a live [`oxisql_postgres::PgConnection`] for streaming
    /// Postgres query results through DataFusion.
    ///
    /// The connection is wrapped in an `Arc` and used as the underlying
    /// [`Connection`] trait object.  All filter, projection, limit, and sort
    /// pushdown features of [`OxiSqlStreamProvider`] are available on the
    /// resulting provider.
    ///
    /// # Feature flag
    ///
    /// Only available when the `postgres` Cargo feature of `oxisql-datafusion`
    /// is enabled.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use std::sync::Arc;
    /// use arrow::datatypes::{DataType, Field, Schema};
    /// use oxisql_postgres::{PgConnection, TlsMode};
    /// use oxisql_datafusion::stream::OxiSqlStreamProvider;
    ///
    /// let conn = PgConnection::connect(
    ///     "host=localhost user=postgres password=secret dbname=mydb",
    ///     TlsMode::Disabled,
    /// ).await?;
    ///
    /// let schema = Arc::new(Schema::new(vec![
    ///     Field::new("id",   DataType::Int64, true),
    ///     Field::new("name", DataType::Utf8,  true),
    /// ]));
    ///
    /// let provider = OxiSqlStreamProvider::from_postgres(conn, "users", schema);
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(feature = "postgres")]
    pub fn from_postgres(
        conn: oxisql_postgres::PgConnection,
        table_name: impl Into<String>,
        schema: SchemaRef,
    ) -> Self {
        Self::new(Arc::new(conn) as Arc<dyn Connection>, table_name, schema)
    }

    /// Construct from a live [`oxisql_sqlite_compat::SqliteConnection`] for
    /// streaming SQLite query results through DataFusion.
    ///
    /// The connection is wrapped in an `Arc` and used as the underlying
    /// [`Connection`] trait object.  All filter, projection, limit, and sort
    /// pushdown features of [`OxiSqlStreamProvider`] are available on the
    /// resulting provider.
    ///
    /// Both in-memory databases (created with
    /// [`SqliteConnection::open_memory`]) and file-backed databases are
    /// supported.
    ///
    /// # Feature flag
    ///
    /// Only available when the `sqlite` Cargo feature of `oxisql-datafusion`
    /// is enabled.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use std::sync::Arc;
    /// use arrow::datatypes::{DataType, Field, Schema};
    /// use oxisql_sqlite_compat::SqliteConnection;
    /// use oxisql_datafusion::stream::OxiSqlStreamProvider;
    ///
    /// let conn = SqliteConnection::open_memory().await?;
    ///
    /// let schema = Arc::new(Schema::new(vec![
    ///     Field::new("id",   DataType::Int64, true),
    ///     Field::new("name", DataType::Utf8,  true),
    /// ]));
    ///
    /// let provider = OxiSqlStreamProvider::from_sqlite(conn, "users", schema);
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// [`SqliteConnection::open_memory`]: oxisql_sqlite_compat::SqliteConnection::open_memory
    #[cfg(feature = "sqlite")]
    pub fn from_sqlite(
        conn: oxisql_sqlite_compat::SqliteConnection,
        table_name: impl Into<String>,
        schema: SchemaRef,
    ) -> Self {
        Self::new(Arc::new(conn) as Arc<dyn Connection>, table_name, schema)
    }

    /// Return the SQL SELECT column list for the given projection.
    fn project_clause(&self, projection: Option<&Vec<usize>>) -> String {
        match projection {
            None => "*".to_string(),
            Some(indices) => indices
                .iter()
                .map(|&i| self.schema.field(i).name().as_str())
                .collect::<Vec<_>>()
                .join(", "),
        }
    }

    /// Return the projected Arrow schema for the given column indices.
    fn projected_schema(&self, projection: Option<&Vec<usize>>) -> SchemaRef {
        match projection {
            None => Arc::clone(&self.schema),
            Some(indices) => {
                let fields: Vec<_> = indices
                    .iter()
                    .map(|&i| self.schema.field(i).clone())
                    .collect();
                Arc::new(arrow::datatypes::Schema::new(fields))
            }
        }
    }
}

impl fmt::Debug for OxiSqlStreamProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OxiSqlStreamProvider")
            .field("table_name", &self.table_name)
            .field("schema", &self.schema)
            .finish()
    }
}

#[async_trait]
impl TableProvider for OxiSqlStreamProvider {
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
        limit: Option<usize>,
    ) -> DFResult<Arc<dyn ExecutionPlan>> {
        let col_clause = self.project_clause(projection);
        let mut sql = format!("SELECT {} FROM {}", col_clause, self.table_name);

        // Build WHERE clause from pushed-down filters.
        let where_parts: Vec<String> = filters.iter().filter_map(expr_to_sql).collect();
        if !where_parts.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&where_parts.join(" AND "));
        }

        // Append ORDER BY clause if sort pushdown has been configured.
        if let Some(ref order) = self.sort_order {
            if !order.is_empty() {
                sql.push_str(" ORDER BY ");
                let order_clause = order
                    .iter()
                    .map(|(col, dir)| format!("{col} {dir}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                sql.push_str(&order_clause);
            }
        }

        // Append LIMIT.
        if let Some(n) = limit {
            sql.push_str(&format!(" LIMIT {n}"));
        }

        let output_schema = self.projected_schema(projection);
        let exec = OxiSqlExecPlan::new(Arc::clone(&self.conn), sql, Arc::clone(&output_schema));
        Ok(Arc::new(exec) as Arc<dyn ExecutionPlan>)
    }

    fn supports_filters_pushdown(
        &self,
        filters: &[&Expr],
    ) -> DFResult<Vec<TableProviderFilterPushDown>> {
        Ok(filters
            .iter()
            .map(|f| {
                if can_push_filter(f) {
                    TableProviderFilterPushDown::Exact
                } else {
                    TableProviderFilterPushDown::Unsupported
                }
            })
            .collect())
    }
}

// ── Expression translation ────────────────────────────────────────────────────

/// Check whether `expr` can be fully translated to SQL and pushed to the backend.
///
/// Returns `true` only for expressions that [`expr_to_sql`] can translate
/// completely.  The two functions must stay in sync: every pattern that returns
/// `false` here must also return `None` from [`expr_to_sql`].
pub fn can_push_filter(expr: &Expr) -> bool {
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
                    | Operator::And
                    | Operator::Or
            ) && can_push_atom(left)
                && can_push_atom(right)
        }
        Expr::IsNull(inner) | Expr::IsNotNull(inner) => can_push_atom(inner),
        Expr::Not(inner) => can_push_filter(inner),
        _ => false,
    }
}

/// Check whether a leaf expression (column or literal) can appear inside a
/// pushed-down filter.
fn can_push_atom(expr: &Expr) -> bool {
    match expr {
        Expr::Column(_) => true,
        Expr::Literal(_, _) => true,
        other => can_push_filter(other),
    }
}

/// Translate a DataFusion expression to a SQL WHERE-clause fragment.
///
/// Returns `None` for unsupported expressions.
/// [`can_push_filter`] can be used to check translatability without executing
/// the translation.
pub fn expr_to_sql(expr: &Expr) -> Option<String> {
    match expr {
        Expr::BinaryExpr(BinaryExpr { left, op, right }) => {
            let l = atom_to_sql(left)?;
            let r = atom_to_sql(right)?;
            let op_str = match op {
                Operator::Eq => "=",
                Operator::NotEq => "<>",
                Operator::Lt => "<",
                Operator::LtEq => "<=",
                Operator::Gt => ">",
                Operator::GtEq => ">=",
                Operator::And => "AND",
                Operator::Or => "OR",
                _ => return None,
            };
            Some(format!("({l} {op_str} {r})"))
        }
        Expr::IsNull(inner) => Some(format!("({} IS NULL)", atom_to_sql(inner)?)),
        Expr::IsNotNull(inner) => Some(format!("({} IS NOT NULL)", atom_to_sql(inner)?)),
        Expr::Not(inner) => Some(format!("(NOT {})", expr_to_sql(inner)?)),
        _ => None,
    }
}

/// Translate a leaf expression (column or literal) or a nested filter
/// expression to SQL.
fn atom_to_sql(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Column(col) => Some(col.name.clone()),
        Expr::Literal(scalar, _metadata) => scalar_to_sql(scalar),
        other => expr_to_sql(other),
    }
}

/// Render a [`ScalarValue`] as a SQL literal fragment.
///
/// Returns `None` for types that have no safe, round-trippable SQL literal
/// representation (e.g. `Decimal128`, `Date64`, `Time32`, complex types).
fn scalar_to_sql(scalar: &ScalarValue) -> Option<String> {
    match scalar {
        ScalarValue::Int8(Some(v)) => Some(v.to_string()),
        ScalarValue::Int16(Some(v)) => Some(v.to_string()),
        ScalarValue::Int32(Some(v)) => Some(v.to_string()),
        ScalarValue::Int64(Some(v)) => Some(v.to_string()),
        ScalarValue::Float32(Some(v)) => Some(v.to_string()),
        ScalarValue::Float64(Some(v)) => Some(v.to_string()),
        ScalarValue::Boolean(Some(v)) => Some(if *v { "TRUE" } else { "FALSE" }.to_string()),
        ScalarValue::Utf8(Some(s)) | ScalarValue::LargeUtf8(Some(s)) => {
            // SQL-escape single quotes to prevent injection (connection is trusted).
            Some(format!("'{}'", s.replace('\'', "''")))
        }
        ScalarValue::Null => Some("NULL".to_string()),
        // Typed NULLs (e.g. Int8(None)) are also SQL NULL.
        ScalarValue::Int8(None)
        | ScalarValue::Int16(None)
        | ScalarValue::Int32(None)
        | ScalarValue::Int64(None)
        | ScalarValue::Float32(None)
        | ScalarValue::Float64(None)
        | ScalarValue::Boolean(None)
        | ScalarValue::Utf8(None)
        | ScalarValue::LargeUtf8(None) => Some("NULL".to_string()),
        _ => None,
    }
}

// ── OxiSqlExecPlan ────────────────────────────────────────────────────────────

/// A DataFusion [`ExecutionPlan`] that executes a SQL query lazily against an
/// OxiSQL connection at physical-plan execution time.
///
/// Unlike `MemorySourceConfig`, this defers the SQL query until DataFusion
/// calls `execute()`, keeping `scan()` cheap and allocation-free at plan time.
struct OxiSqlExecPlan {
    schema: SchemaRef,
    sql: String,
    conn: Arc<dyn Connection>,
    cache: Arc<PlanProperties>,
}

impl fmt::Debug for OxiSqlExecPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OxiSqlExecPlan")
            .field("sql", &self.sql)
            .field("schema", &self.schema)
            .finish()
    }
}

impl OxiSqlExecPlan {
    fn new(conn: Arc<dyn Connection>, sql: String, schema: SchemaRef) -> Self {
        let eq = EquivalenceProperties::new(Arc::clone(&schema));
        let properties = PlanProperties::new(
            eq,
            Partitioning::UnknownPartitioning(1),
            EmissionType::Incremental,
            Boundedness::Bounded,
        );
        Self {
            schema,
            sql,
            conn,
            cache: Arc::new(properties),
        }
    }
}

impl DisplayAs for OxiSqlExecPlan {
    fn fmt_as(&self, _t: DisplayFormatType, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "OxiSqlExecPlan sql={:?}", self.sql)
    }
}

impl ExecutionPlan for OxiSqlExecPlan {
    fn name(&self) -> &'static str {
        "OxiSqlExecPlan"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn properties(&self) -> &Arc<PlanProperties> {
        &self.cache
    }

    fn children(&self) -> Vec<&Arc<dyn ExecutionPlan>> {
        vec![]
    }

    fn with_new_children(
        self: Arc<Self>,
        children: Vec<Arc<dyn ExecutionPlan>>,
    ) -> datafusion::error::Result<Arc<dyn ExecutionPlan>> {
        if children.is_empty() {
            Ok(self)
        } else {
            Err(datafusion::error::DataFusionError::Internal(
                "OxiSqlExecPlan has no children".into(),
            ))
        }
    }

    fn execute(
        &self,
        _partition: usize,
        _context: Arc<TaskContext>,
    ) -> datafusion::error::Result<SendableRecordBatchStream> {
        let sql = self.sql.clone();
        let conn = Arc::clone(&self.conn);
        let schema = Arc::clone(&self.schema);

        let stream = futures::stream::once(async move {
            let rows = conn
                .query(&sql, &[])
                .await
                .map_err(|e| datafusion::error::DataFusionError::External(Box::new(e)))?;
            crate::types::rows_to_record_batch(rows, schema)
                .map_err(|e| datafusion::error::DataFusionError::External(Box::new(e)))
        });

        Ok(Box::pin(RecordBatchStreamAdapter::new(
            Arc::clone(&self.schema),
            stream,
        )))
    }
}
