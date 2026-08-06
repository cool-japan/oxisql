//! Quickstart: connect to MySQL over the Pure-Rust `mysql_async` driver (no
//! `libmysqlclient`), create a table, insert a parameterized row, and query
//! it back.
//!
//! Requires a running MySQL server. Point `OXISQL_MYSQL_URL` at it, e.g.:
//! ```text
//! # docker run --rm -e MYSQL_ALLOW_EMPTY_PASSWORD=yes -p 3306:3306 mysql:8
//! export OXISQL_MYSQL_URL="mysql://root@localhost:3306/test"
//! cargo run -p oxisql-mysql --example mysql_quickstart
//! ```
//! Without the server this builds cleanly but fails at connect time — this
//! example exists to demonstrate the API surface, not to be run in CI.

use oxisql_core::Connection;
use oxisql_mysql::{MyConnection, TlsMode};

const DEFAULT_URL: &str = "mysql://root@localhost:3306/test";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::var("OXISQL_MYSQL_URL").unwrap_or_else(|_| DEFAULT_URL.to_string());

    // `TlsMode::Disabled` is a plain-text connection, suitable for a
    // trusted/local network. For encrypted connections, pass
    // `TlsMode::Rustls(config)` built from OxiTLS's rustcrypto provider.
    let conn = MyConnection::connect(&url, TlsMode::Disabled).await?;

    conn.execute(
        "CREATE TEMPORARY TABLE oxisql_quickstart_items (id INT PRIMARY KEY, name TEXT NOT NULL)",
        &[],
    )
    .await?;

    // `mysql_async` (and MySQL's own wire protocol) uses `?` positional
    // placeholders, unlike the `$1`/`$2` style used by the Postgres and
    // embedded backends.
    conn.execute(
        "INSERT INTO oxisql_quickstart_items (id, name) VALUES (?, ?)",
        &[&1i64, &"widget"],
    )
    .await?;
    conn.execute(
        "INSERT INTO oxisql_quickstart_items (id, name) VALUES (?, ?)",
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
