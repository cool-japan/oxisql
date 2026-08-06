//! Quickstart: connect to PostgreSQL over the Pure-Rust `tokio-postgres`
//! driver (no `libpq`), create a table, insert a parameterized row, and
//! query it back.
//!
//! Requires a running PostgreSQL server. Point `OXISQL_PG_CONN_STR` at it,
//! e.g.:
//! ```text
//! export OXISQL_PG_CONN_STR="host=localhost port=5432 user=postgres dbname=postgres"
//! cargo run -p oxisql-postgres --example postgres_quickstart
//! ```
//! Without the server this builds cleanly but fails at connect time — this
//! example exists to demonstrate the API surface, not to be run in CI.

use oxisql_core::Connection;
use oxisql_postgres::{PgConnection, TlsMode};

const DEFAULT_CONN_STR: &str = "host=localhost port=5432 user=postgres";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let conn_str =
        std::env::var("OXISQL_PG_CONN_STR").unwrap_or_else(|_| DEFAULT_CONN_STR.to_string());

    // `TlsMode::Disabled` is a plain-text connection, suitable for a
    // trusted/local network. For encrypted connections, build a
    // `rustls::ClientConfig` from OxiTLS and pass `TlsMode::Rustls(config)`.
    let conn = PgConnection::connect(&conn_str, TlsMode::Disabled).await?;

    conn.execute(
        "CREATE TEMP TABLE oxisql_quickstart_items (id INT PRIMARY KEY, name TEXT NOT NULL)",
        &[],
    )
    .await?;

    conn.execute(
        "INSERT INTO oxisql_quickstart_items (id, name) VALUES ($1, $2)",
        &[&1i64, &"widget"],
    )
    .await?;
    conn.execute(
        "INSERT INTO oxisql_quickstart_items (id, name) VALUES ($1, $2)",
        &[&2i64, &"gadget"],
    )
    .await?;

    let rows = conn
        .query(
            "SELECT id, name FROM oxisql_quickstart_items ORDER BY id",
            &[],
        )
        .await?;

    println!("items:");
    for row in &rows {
        let id: i64 = row.try_get("id")?;
        let name: String = row.try_get("name")?;
        println!("  #{id} {name}");
    }

    assert_eq!(rows.len(), 2, "expected exactly the two inserted rows");
    Ok(())
}
