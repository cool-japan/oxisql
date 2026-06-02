//! Bridge from `oxisql_parse::LogicalPlan` to DataFusion `LogicalPlan`.
//!
//! # Design
//!
//! `oxisql_parse::LogicalPlan` stores predicate, projection, sort, and join
//! expressions as raw SQL fragment *strings* rather than structured `Expr`
//! trees.  Converting those strings back to DataFusion `Expr` values without
//! re-parsing the original SQL would require a full SQL parser, which DataFusion
//! already provides.
//!
//! Therefore this module offers two approaches:
//!
//! 1. **Structural conversion** (`to_datafusion_plan`) — handles the subset of
//!    plan variants that do not require expression re-parsing:
//!    - `Scan` → `SELECT * FROM <table>` round-trip through DataFusion's own
//!      planner (table must already be registered in `ctx`).
//!    - `Limit` → wraps the recursively-converted inner plan with
//!      `LogicalPlanBuilder::limit`.
//!    - `Empty` → DataFusion `EmptyRelation`.
//!    All other variants return [`OxiSqlFusionError::UnsupportedType`] and
//!    should be handled via the SQL round-trip approach instead.
//!
//! 2. **SQL round-trip** (`sql_to_datafusion_plan`) — take the original SQL
//!    string and let DataFusion plan it from scratch.  This is the most reliable
//!    path when the SQL source is available.
//!
//! # Example
//!
//! ```rust,no_run
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! use datafusion::prelude::SessionContext;
//! use oxisql_datafusion::plan_bridge::sql_to_datafusion_plan;
//!
//! let ctx = SessionContext::new();
//! // Tables used by the SQL must already be registered in `ctx`.
//! let _plan = sql_to_datafusion_plan("SELECT 1 AS n", &ctx).await?;
//! # Ok(())
//! # }
//! ```

use std::sync::Arc;

use datafusion::common::DFSchema;
use datafusion::logical_expr::{EmptyRelation, LogicalPlan as DfPlan, LogicalPlanBuilder};
use datafusion::prelude::SessionContext;
use oxisql_parse::LogicalPlan as OxiPlan;

use crate::error::OxiSqlFusionError;

/// Convert an `oxisql_parse::LogicalPlan` to a DataFusion `LogicalPlan`.
///
/// Only the following variants are converted structurally:
///
/// | OxiPlan variant | DataFusion equivalent |
/// |---|---|
/// | `Scan { table, alias, .. }` | `SELECT * FROM table [AS alias]` planned by DataFusion |
/// | `Limit { count, offset, input }` | `LogicalPlanBuilder::limit(skip, fetch)` |
/// | `Empty` | `EmptyRelation { produce_one_row: false }` |
///
/// All other variants (Filter, Project, Sort, Aggregate, Join, Values, SetOp,
/// Cte, Window, Subquery, Exists, InSubquery, CteRef) return
/// [`OxiSqlFusionError::UnsupportedType`].  For those cases use
/// [`sql_to_datafusion_plan`] with the original SQL string instead.
///
/// # Errors
///
/// - [`OxiSqlFusionError::DataFusion`] — DataFusion planning or catalog error.
/// - [`OxiSqlFusionError::UnsupportedType`] — plan variant not yet bridged.
///
/// # Panics
///
/// This function does not panic.
pub async fn to_datafusion_plan(
    plan: &OxiPlan,
    ctx: &SessionContext,
) -> Result<DfPlan, OxiSqlFusionError> {
    // Use Box::pin for the recursive async calls to avoid infinite-sized futures.
    match plan {
        OxiPlan::Scan { table, alias, .. } => {
            let sql = match alias {
                Some(a) => format!("SELECT * FROM \"{table}\" AS \"{a}\""),
                None => format!("SELECT * FROM \"{table}\""),
            };
            let df = ctx.sql(&sql).await.map_err(OxiSqlFusionError::DataFusion)?;
            Ok(df.logical_plan().clone())
        }

        OxiPlan::Limit {
            count,
            offset,
            input,
        } => {
            let inner = Box::pin(to_datafusion_plan(input, ctx)).await?;
            let skip = offset.unwrap_or(0) as usize;
            let fetch = count.map(|c| c as usize);
            LogicalPlanBuilder::from(inner)
                .limit(skip, fetch)
                .map_err(OxiSqlFusionError::DataFusion)?
                .build()
                .map_err(OxiSqlFusionError::DataFusion)
        }

        OxiPlan::Empty => {
            let empty_schema = Arc::new(DFSchema::empty());
            Ok(DfPlan::EmptyRelation(EmptyRelation {
                produce_one_row: false,
                schema: empty_schema,
            }))
        }

        other => Err(OxiSqlFusionError::UnsupportedType(format!(
            "plan variant '{}' is not directly convertible; use sql_to_datafusion_plan \
             with the original SQL string instead",
            plan_variant_name(other)
        ))),
    }
}

/// Simplified bridge: parse a raw SQL string through DataFusion and return the
/// resulting `LogicalPlan`.
///
/// This is the most reliable bridging approach because DataFusion's own SQL
/// planner resolves schemas, types, and expressions without any re-parsing
/// ambiguity.  Use [`to_datafusion_plan`] only when the original SQL is
/// unavailable.
///
/// All tables referenced by `sql` must already be registered in `ctx`.
///
/// # Errors
///
/// Returns [`OxiSqlFusionError::DataFusion`] if DataFusion cannot parse or plan
/// the SQL.
pub async fn sql_to_datafusion_plan(
    sql: &str,
    ctx: &SessionContext,
) -> Result<DfPlan, OxiSqlFusionError> {
    ctx.sql(sql)
        .await
        .map_err(OxiSqlFusionError::DataFusion)
        .map(|df| df.logical_plan().clone())
}

/// Return a short human-readable name for a plan variant (used in error messages).
fn plan_variant_name(plan: &OxiPlan) -> &'static str {
    match plan {
        OxiPlan::Scan { .. } => "Scan",
        OxiPlan::Filter { .. } => "Filter",
        OxiPlan::Project { .. } => "Project",
        OxiPlan::Join { .. } => "Join",
        OxiPlan::Aggregate { .. } => "Aggregate",
        OxiPlan::Sort { .. } => "Sort",
        OxiPlan::Limit { .. } => "Limit",
        OxiPlan::Values { .. } => "Values",
        OxiPlan::Empty => "Empty",
        OxiPlan::SetOp { .. } => "SetOp",
        OxiPlan::Cte { .. } => "Cte",
        OxiPlan::CteRef { .. } => "CteRef",
        OxiPlan::Window { .. } => "Window",
        OxiPlan::Subquery { .. } => "Subquery",
        OxiPlan::Exists { .. } => "Exists",
        OxiPlan::InSubquery { .. } => "InSubquery",
    }
}
