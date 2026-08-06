//! Quickstart: an in-memory embedded SQL database (GlueSQL-backed), with a
//! parameterized insert and a query.
//!
//! No server, no filesystem, no feature flags required — `EmbeddedConnection`
//! always has an in-memory backend available. Enable the `sled-storage`,
//! `fjall-storage`, or `redb-storage` features (see `Cargo.toml`) for
//! persistent, file-backed variants.
//!
//! Run with:
//! ```text
//! cargo run -p oxisql-embedded --example embedded_quickstart
//! ```

use oxisql_core::Connection;
use oxisql_embedded::EmbeddedConnection;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let conn = EmbeddedConnection::open_memory()?;

    conn.execute("CREATE TABLE items (id INT, name TEXT)", &[])
        .await?;

    // OxiSQL uses Postgres-style `$1`/`$2` positional placeholders across
    // every backend for a single, unified parameter-binding convention.
    conn.execute("INSERT INTO items VALUES ($1, $2)", &[&1i64, &"widget"])
        .await?;
    conn.execute("INSERT INTO items VALUES ($1, $2)", &[&2i64, &"gadget"])
        .await?;

    let rows = conn
        .query("SELECT id, name FROM items ORDER BY id", &[])
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
