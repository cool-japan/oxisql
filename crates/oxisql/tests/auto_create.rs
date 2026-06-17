//! Integration tests for [`oxisql::connect_or_create`] against live
//! PostgreSQL and MySQL servers.
//!
//! These tests require a running database server and are therefore `#[ignore]`d
//! by default so they never run in ordinary CI (matching the pattern used by
//! the `oxisql-postgres` / `oxisql-mysql` backend crates).  To run them:
//!
//! ```bash
//! # PostgreSQL
//! docker run --rm -e POSTGRES_PASSWORD=test -p 5432:5432 postgres
//! cargo test -p oxisql --features postgres auto_create_pg -- --include-ignored
//!
//! # MySQL
//! docker run --rm -e MYSQL_ROOT_PASSWORD=test -p 3306:3306 mysql
//! cargo test -p oxisql --features mysql auto_create_mysql -- --include-ignored
//! ```
//!
//! The base connection URI may be overridden with the `OXISQL_TEST_PG_URI` /
//! `OXISQL_TEST_MYSQL_URI` environment variables (the trailing `/<dbname>` path
//! is replaced by the test).

#[cfg(feature = "postgres")]
mod pg {
    // Trait methods (`execute`/`query`) are called on the `Box<dyn Connection>`
    // returned by the facade; the trait object names the trait, so no `use
    // oxisql::Connection` import is required here.

    /// Build a `postgres://` URI whose target database is `db`, honouring the
    /// `OXISQL_TEST_PG_URI` override (default: local server, user `postgres`,
    /// password `test`).
    fn pg_uri(db: &str) -> String {
        let base = std::env::var("OXISQL_TEST_PG_URI")
            .unwrap_or_else(|_| "postgres://postgres:test@localhost:5432/postgres".to_string());
        // Replace the path component with our target database name.
        match base.split_once("://") {
            Some((scheme, rest)) => {
                let authority = rest.split('/').next().unwrap_or(rest);
                format!("{scheme}://{authority}/{db}")
            }
            None => format!("postgres://localhost:5432/{db}"),
        }
    }

    /// `connect_or_create` must create a missing PostgreSQL database, then
    /// return a working connection to it.
    #[tokio::test]
    #[ignore = "requires a live PostgreSQL server"]
    async fn auto_create_pg_creates_then_connects() {
        // A fresh, almost-certainly-absent database name.
        let db = format!(
            "oxisql_autocreate_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let uri = pg_uri(&db);

        // The database does not exist yet — a plain connect must fail.
        assert!(
            oxisql::connect(&uri).await.is_err(),
            "database {db} should not exist before creation"
        );

        // connect_or_create creates it and hands back a usable connection.
        let conn = oxisql::connect_or_create(&uri)
            .await
            .expect("connect_or_create should create the database and connect");
        conn.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)", &[])
            .await
            .expect("DDL on the freshly-created database");
        conn.execute("INSERT INTO t (id, name) VALUES (1, 'a')", &[])
            .await
            .expect("insert");
        let rows = conn
            .query("SELECT id, name FROM t", &[])
            .await
            .expect("select");
        assert_eq!(rows.len(), 1);

        // A second call must succeed too (database already exists path).
        let conn2 = oxisql::connect_or_create(&uri)
            .await
            .expect("second connect_or_create should connect to the existing database");
        let rows2 = conn2.query("SELECT id FROM t", &[]).await.expect("select");
        assert_eq!(rows2.len(), 1);

        // Clean up: drop the test database from the maintenance connection.
        drop(conn);
        drop(conn2);
        let maint = oxisql::connect("postgres://postgres:test@localhost:5432/postgres")
            .await
            .expect("maintenance connect for cleanup");
        let _ = maint
            .execute(&format!(r#"DROP DATABASE IF EXISTS "{db}""#), &[])
            .await;
    }
}

#[cfg(feature = "mysql")]
mod mysql {
    // Trait methods are called on the `Box<dyn Connection>` returned by the
    // facade; the trait object names the trait, so no `use oxisql::Connection`
    // import is required here.

    /// Build a `mysql://` URI whose target database is `db`, honouring the
    /// `OXISQL_TEST_MYSQL_URI` override (default: local server, user `root`,
    /// password `test`).
    fn mysql_uri(db: &str) -> String {
        let base = std::env::var("OXISQL_TEST_MYSQL_URI")
            .unwrap_or_else(|_| "mysql://root:test@127.0.0.1:3306/mysql".to_string());
        match base.split_once("://") {
            Some((scheme, rest)) => {
                let authority = rest.split('/').next().unwrap_or(rest);
                format!("{scheme}://{authority}/{db}")
            }
            None => format!("mysql://root:test@127.0.0.1:3306/{db}"),
        }
    }

    /// `connect_or_create` must create a missing MySQL database, then return a
    /// working connection to it.
    #[tokio::test]
    #[ignore = "requires a live MySQL server"]
    async fn auto_create_mysql_creates_then_connects() {
        let db = format!(
            "oxisql_autocreate_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let uri = mysql_uri(&db);

        // connect_or_create creates the database and returns a usable connection.
        let conn = oxisql::connect_or_create(&uri)
            .await
            .expect("connect_or_create should create the database and connect");
        conn.execute("CREATE TABLE t (id INT PRIMARY KEY, name VARCHAR(64))", &[])
            .await
            .expect("DDL on the freshly-created database");
        conn.execute("INSERT INTO t (id, name) VALUES (1, 'a')", &[])
            .await
            .expect("insert");
        let rows = conn
            .query("SELECT id, name FROM t", &[])
            .await
            .expect("select");
        assert_eq!(rows.len(), 1);

        // A second call must succeed too (database-already-exists path).
        let conn2 = oxisql::connect_or_create(&uri)
            .await
            .expect("second connect_or_create should connect to the existing database");
        let rows2 = conn2.query("SELECT id FROM t", &[]).await.expect("select");
        assert_eq!(rows2.len(), 1);

        // Clean up: drop the test database via the maintenance connection.
        drop(conn);
        drop(conn2);
        let maint = oxisql::connect("mysql://root:test@127.0.0.1:3306/mysql")
            .await
            .expect("maintenance connect for cleanup");
        let _ = maint
            .execute(&format!("DROP DATABASE IF EXISTS `{db}`"), &[])
            .await;
    }
}

// When neither backend feature is enabled this test crate still needs at least
// one item to compile cleanly; the per-backend modules above are feature-gated.
#[cfg(not(any(feature = "postgres", feature = "mysql")))]
#[test]
fn no_wire_backend_enabled() {
    // Nothing to test without a wire-protocol backend; this keeps the test
    // binary non-empty under embedded-only feature sets.
}
