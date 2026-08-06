//! Quickstart: open a C-free, Pure-Rust SQLite-compatible database, create a
//! table, insert rows, and query them back.
//!
//! `oxisql-sqlite-compat` runs entirely on the in-tree `oxisqlite` engine (a
//! C-free fork of limbo) — no `libsqlite3`, no `rusqlite-sys`, no C compiler
//! involved at any point.
//!
//! Run with:
//! ```text
//! cargo run -p oxisql-sqlite-compat --example sqlite_quickstart
//! ```

use oxisql_core::Connection;
use oxisql_sqlite_compat::SqliteConnection;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // An in-memory database needs no filesystem path at all. For a
    // file-backed database, `SqliteConnection::open(path)` takes any path
    // (e.g. one built from `std::env::temp_dir()` for a scratch database).
    let conn = SqliteConnection::open_memory().await?;

    conn.execute(
        "CREATE TABLE pilots (id INTEGER PRIMARY KEY, name TEXT NOT NULL, callsign TEXT)",
        &[],
    )
    .await?;

    // OxiSQL uses Postgres-style `$1`/`$2` positional placeholders across
    // every backend (including this SQLite-compatible one) for a single,
    // unified parameter-binding convention.
    conn.execute(
        "INSERT INTO pilots (name, callsign) VALUES ($1, $2)",
        &[&"Maverick", &"Pete Mitchell"],
    )
    .await?;
    conn.execute(
        "INSERT INTO pilots (name, callsign) VALUES ($1, $2)",
        &[&"Goose", &"Nick Bradshaw"],
    )
    .await?;

    let rows = conn
        .query("SELECT id, name, callsign FROM pilots ORDER BY id", &[])
        .await?;

    println!("pilots:");
    for row in &rows {
        let id: i64 = row.try_get("id")?;
        let name: String = row.try_get("name")?;
        let callsign: String = row.try_get("callsign")?;
        println!("  #{id} {name} \"{callsign}\"");
    }

    assert_eq!(rows.len(), 2, "expected exactly the two inserted rows");
    Ok(())
}
