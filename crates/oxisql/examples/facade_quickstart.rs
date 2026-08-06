//! Quickstart: the unified `oxisql` facade dispatches on the connection URI
//! scheme, so application code stays backend-agnostic. This example uses
//! the in-memory embedded backend (`memory://`), which needs no server and
//! no filesystem access.
//!
//! Swap the URI (and the corresponding Cargo feature) to target a different
//! backend without touching any other line: `postgres://...` (`postgres`
//! feature), `mysql://...` (`mysql` feature), or `sqlite://...` /
//! `sqlite::memory:` (`sqlite` feature) — see [`oxisql::connect`].
//!
//! Run with:
//! ```text
//! cargo run -p oxisql --example facade_quickstart --features embedded
//! ```

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let conn = oxisql::connect("memory://").await?;

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
