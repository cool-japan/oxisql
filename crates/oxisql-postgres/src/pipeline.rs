//! [`PgPipeline`] — batched multi-query execution for PostgreSQL.
//!
//! # Background
//!
//! PostgreSQL's wire protocol supports pipelining: multiple queries are sent
//! back-to-back before reading any responses, which keeps both sides of the
//! connection busy and reduces round-trip latency.
//!
//! `tokio-postgres` (0.7.x) achieves this *implicitly*: when multiple query
//! futures are polled concurrently (e.g. with `futures::future::try_join_all`),
//! they pipeline their requests over the same TCP connection.  There is no
//! explicit `pipeline()` API in 0.7.x.
//!
//! # Design and limitations
//!
//! Because `PgConnection` wraps `tokio_postgres::Client` behind an
//! `Arc<tokio::sync::Mutex<Client>>`, only one logical operation can hold the
//! lock at a time.  True parallel in-flight pipelining is therefore not
//! achievable within a single `PgPipeline` instance — each prepared-statement
//! compilation and each query execution happens sequentially under the lock.
//!
//! What `PgPipeline` *does* provide is a convenient batched-dispatch API:
//! callers queue all their queries up front, call [`PgPipeline::finish`] once,
//! and receive all results in one go.  The internal execution is sequential
//! but fully pipelined at the PostgreSQL wire level where possible (all
//! prepares are issued first; then all executions are issued in order).
//!
//! If true concurrent pipelining is required, use a connection pool with
//! multiple connections and issue queries on separate handles simultaneously.
//!
//! # Example
//!
//! ```rust,no_run
//! # #[tokio::main]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use oxisql_postgres::{PgConnection, TlsMode};
//!
//! let conn = PgConnection::connect(
//!     "host=localhost user=postgres dbname=testdb",
//!     TlsMode::Disabled,
//! ).await?;
//!
//! let mut pipeline = conn.pipeline();
//! pipeline.add_execute("INSERT INTO t (n) VALUES (1)", &[]);
//! pipeline.add_execute("INSERT INTO t (n) VALUES (2)", &[]);
//! pipeline.add_query("SELECT n FROM t ORDER BY n", &[]);
//!
//! let result = pipeline.finish().await?;
//! assert_eq!(result.executes.len(), 2);
//! assert_eq!(result.queries.len(), 1);
//! # Ok(())
//! # }
//! ```

use std::sync::Arc;

use futures::future::try_join_all;
use tokio::sync::Mutex;
use tokio_postgres::Statement;

use oxisql_core::{Row, ToSqlValue};

use crate::error::PgError;
use crate::types::{pg_row_to_row, value_to_param, OwnedParam};

// ── internal query kind ───────────────────────────────────────────────────────

/// Internal representation of a queued pipeline entry.
enum PipelineQuery {
    /// A write (DML/DDL) query — returns rows-affected count.
    Execute {
        sql: String,
        params: Vec<OwnedParam>,
    },
    /// A read query — returns a row set.
    Query {
        sql: String,
        params: Vec<OwnedParam>,
    },
}

// ── PipelineResult ────────────────────────────────────────────────────────────

/// Collected results from a completed [`PgPipeline`].
///
/// `executes` and `queries` are ordered to match the sequence in which
/// [`PgPipeline::add_execute`] and [`PgPipeline::add_query`] were called.
pub struct PipelineResult {
    /// Rows-affected count for each [`PgPipeline::add_execute`] call, in order.
    pub executes: Vec<u64>,
    /// Row sets for each [`PgPipeline::add_query`] call, in order.
    pub queries: Vec<Vec<Row>>,
}

// ── PgPipeline ────────────────────────────────────────────────────────────────

/// A batched-dispatch pipeline for PostgreSQL.
///
/// Create via `PgConnection::pipeline`, queue queries with
/// [`add_execute`][Self::add_execute] and [`add_query`][Self::add_query],
/// then send all at once with [`finish`][Self::finish].
///
/// See the module documentation for design limitations.
pub struct PgPipeline {
    client: Arc<Mutex<tokio_postgres::Client>>,
    queries: Vec<PipelineQuery>,
}

impl PgPipeline {
    /// Create a new pipeline backed by `client`.
    ///
    /// This is called by [`PgConnection::pipeline`] and is not meant to be
    /// constructed directly by callers.
    pub(crate) fn new(client: Arc<Mutex<tokio_postgres::Client>>) -> Self {
        Self {
            client,
            queries: Vec::new(),
        }
    }

    /// Queue a write (DML/DDL) statement to be executed when [`finish`][Self::finish] is called.
    ///
    /// `params` may be empty for parameter-free SQL.
    pub fn add_execute(&mut self, sql: &str, params: &[&dyn ToSqlValue]) {
        let owned: Vec<OwnedParam> = params
            .iter()
            .map(|p| value_to_param(&p.to_value()))
            .collect();
        self.queries.push(PipelineQuery::Execute {
            sql: sql.to_string(),
            params: owned,
        });
    }

    /// Queue a read (`SELECT`) statement to be executed when [`finish`][Self::finish] is called.
    ///
    /// `params` may be empty for parameter-free SQL.
    pub fn add_query(&mut self, sql: &str, params: &[&dyn ToSqlValue]) {
        let owned: Vec<OwnedParam> = params
            .iter()
            .map(|p| value_to_param(&p.to_value()))
            .collect();
        self.queries.push(PipelineQuery::Query {
            sql: sql.to_string(),
            params: owned,
        });
    }

    /// Send all queued queries in a single pass and return collected results.
    ///
    /// Internally this does two sequential passes under the connection lock:
    ///
    /// 1. **Prepare pass** — compile every distinct SQL string once.  All
    ///    `PREPARE` messages are flushed to the server before any `EXECUTE`
    ///    messages, allowing the server to start planning while the client
    ///    sends the remaining requests (wire-level pipelining).
    ///
    /// 2. **Execute pass** — run each query in enqueue order, collecting
    ///    rows-affected counts and row sets respectively.
    ///
    /// Returns a [`PipelineResult`] whose `executes` and `queries` vecs are
    /// in the same order as the corresponding `add_*` calls.
    ///
    /// # Errors
    ///
    /// Returns [`PgError`] on any preparation, execution, or type-conversion
    /// failure.  On error, results collected up to that point are discarded.
    pub async fn finish(self) -> Result<PipelineResult, PgError> {
        if self.queries.is_empty() {
            return Ok(PipelineResult {
                executes: Vec::new(),
                queries: Vec::new(),
            });
        }

        let client = self.client.lock().await;

        // --- Pass 1: prepare all statements concurrently using implicit
        //             tokio-postgres pipelining (futures polled in parallel
        //             via try_join_all).  The Mutex guard is held for the
        //             whole prepare pass; tokio-postgres internally pipelines
        //             all Prepare messages over the single TCP connection.
        let prepare_futs: Vec<_> = self
            .queries
            .iter()
            .map(|q| {
                let sql = match q {
                    PipelineQuery::Execute { sql, .. } => sql.as_str(),
                    PipelineQuery::Query { sql, .. } => sql.as_str(),
                };
                client.prepare(sql)
            })
            .collect();

        let stmts: Vec<Statement> = try_join_all(prepare_futs).await.map_err(PgError::from)?;

        // --- Pass 2: execute in enqueue order, accumulating results.
        let mut executes: Vec<u64> = Vec::new();
        let mut queries: Vec<Vec<Row>> = Vec::new();

        for (q, stmt) in self.queries.into_iter().zip(stmts) {
            match q {
                PipelineQuery::Execute { params, .. } => {
                    let refs = owned_refs(&params);
                    let n = client
                        .execute(&stmt, refs.as_slice())
                        .await
                        .map_err(PgError::from)?;
                    executes.push(n);
                }
                PipelineQuery::Query { params, .. } => {
                    let refs = owned_refs(&params);
                    let pg_rows = client
                        .query(&stmt, refs.as_slice())
                        .await
                        .map_err(PgError::from)?;
                    let rows: Vec<Row> = pg_rows.into_iter().map(pg_row_to_row).collect::<Result<
                        Vec<_>,
                        PgError,
                    >>(
                    )?;
                    queries.push(rows);
                }
            }
        }

        Ok(PipelineResult { executes, queries })
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Build a `&[&(dyn ToSql + Sync)]` reference slice from owned params.
fn owned_refs(owned: &[OwnedParam]) -> Vec<&(dyn tokio_postgres::types::ToSql + Sync)> {
    owned.iter().map(|p| p as _).collect()
}
