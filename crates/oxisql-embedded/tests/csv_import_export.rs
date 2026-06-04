//! Integration tests for CSV import and export via [`EmbeddedConnection`].

use oxisql_core::Connection;
use oxisql_embedded::EmbeddedConnection;

/// Helper to open a fresh in-memory connection.
fn open() -> EmbeddedConnection {
    EmbeddedConnection::open_memory().expect("open_memory must succeed")
}

// ── import_csv ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn csv_import_simple_three_columns() {
    let conn = open();
    let csv = "id,name,age\r\n1,Alice,30\r\n2,Bob,25\r\n";
    let inserted = conn
        .import_csv("people", csv)
        .await
        .expect("import should succeed");
    assert_eq!(inserted, 2, "two data rows should be imported");

    let rows = conn
        .query("SELECT * FROM people", &[])
        .await
        .expect("SELECT should work after import");
    assert_eq!(rows.len(), 2);
}

#[tokio::test]
async fn csv_import_empty_fields_become_null() {
    let conn = open();
    let csv = "a,b,c\r\n1,,3\r\n";
    conn.import_csv("t", csv)
        .await
        .expect("import should succeed");

    let rows = conn
        .query("SELECT * FROM t", &[])
        .await
        .expect("SELECT must work");
    assert_eq!(rows.len(), 1);
    // The middle field should be NULL
    let b: Option<String> = rows[0].try_get("b").expect("b column must exist");
    assert!(b.is_none(), "empty CSV field should become NULL");
}

#[tokio::test]
async fn csv_import_column_names_sanitised() {
    let conn = open();
    // Header has spaces and hyphens
    let csv = "first name,last-name\r\nAlice,Smith\r\n";
    conn.import_csv("users", csv)
        .await
        .expect("import with messy headers should succeed");

    let rows = conn
        .query("SELECT first_name, last_name FROM users", &[])
        .await
        .expect("sanitised column names should be queryable");
    assert_eq!(rows.len(), 1);
    let first: String = rows[0]
        .try_get("first_name")
        .expect("first_name must exist");
    assert_eq!(first, "Alice");
}

#[tokio::test]
async fn csv_import_quoted_commas_in_values() {
    let conn = open();
    let csv = "city,country\r\n\"Portland, OR\",USA\r\n";
    conn.import_csv("places", csv)
        .await
        .expect("import with quoted values should succeed");

    let rows = conn
        .query("SELECT city FROM places", &[])
        .await
        .expect("SELECT must work");
    assert_eq!(rows.len(), 1);
    let city: String = rows[0].try_get("city").expect("city column must exist");
    assert_eq!(city, "Portland, OR");
}

#[tokio::test]
async fn csv_import_single_quotes_in_values_escaped() {
    let conn = open();
    // Value contains a single quote, which must be escaped in the generated SQL
    let csv = "msg\r\nit's fine\r\n";
    conn.import_csv("messages", csv)
        .await
        .expect("import with single-quote values should succeed");

    let rows = conn
        .query("SELECT msg FROM messages", &[])
        .await
        .expect("SELECT must work");
    assert_eq!(rows.len(), 1);
    let msg: String = rows[0].try_get("msg").expect("msg column must exist");
    assert_eq!(msg, "it's fine");
}

#[tokio::test]
async fn csv_import_lf_only_line_endings() {
    let conn = open();
    let csv = "x,y\n10,20\n30,40\n";
    let n = conn
        .import_csv("coords", csv)
        .await
        .expect("LF-only CSV should parse");
    assert_eq!(n, 2);
}

#[tokio::test]
async fn csv_import_empty_csv_returns_zero() {
    let conn = open();
    // Empty string — nothing to import
    let n = conn
        .import_csv("nothing", "")
        .await
        .expect("empty CSV should succeed");
    assert_eq!(n, 0);
}

#[tokio::test]
async fn csv_import_header_only_no_rows() {
    let conn = open();
    let csv = "col1,col2\r\n";
    let n = conn
        .import_csv("empty_table", csv)
        .await
        .expect("header-only CSV should create table with zero rows");
    assert_eq!(n, 0);
    // Table should exist (even though empty)
    let rows = conn
        .query("SELECT * FROM empty_table", &[])
        .await
        .expect("empty table should be queryable");
    assert_eq!(rows.len(), 0);
}

// ── export_table_to_csv ───────────────────────────────────────────────────────

#[tokio::test]
async fn csv_export_basic_round_trip() {
    let conn = open();
    conn.execute("CREATE TABLE items (id INT, label TEXT)", &[])
        .await
        .expect("CREATE TABLE");
    conn.execute("INSERT INTO items VALUES (1, 'Apple')", &[])
        .await
        .expect("INSERT 1");
    conn.execute("INSERT INTO items VALUES (2, 'Banana')", &[])
        .await
        .expect("INSERT 2");

    let csv = conn
        .export_table_to_csv("items")
        .await
        .expect("export should succeed");

    // Should contain header + 2 data rows
    let lines: Vec<&str> = csv.split("\r\n").filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 3, "header + 2 rows = 3 lines");
    assert!(lines[0].contains("id") || lines[0].contains("label"));
    assert!(csv.contains("Apple"));
    assert!(csv.contains("Banana"));
}

#[tokio::test]
async fn csv_export_empty_table_returns_header_only() {
    let conn = open();
    conn.execute("CREATE TABLE empty_test (id INT, label TEXT)", &[])
        .await
        .expect("CREATE TABLE");

    let csv = conn
        .export_table_to_csv("empty_test")
        .await
        .expect("export empty table should succeed");
    // No rows => header-only CSV (column names from schema introspection)
    let lines: Vec<&str> = csv.split("\r\n").filter(|l| !l.is_empty()).collect();
    // Should have exactly 1 line: the header row
    assert_eq!(
        lines.len(),
        1,
        "empty table should produce header row only; got: {csv}"
    );
    assert!(
        lines[0].contains("id") || lines[0].contains("label"),
        "header should contain column names; got: {}",
        lines[0]
    );
}

#[tokio::test]
async fn csv_export_values_with_commas_are_quoted() {
    let conn = open();
    conn.execute("CREATE TABLE cities (name TEXT)", &[])
        .await
        .expect("CREATE TABLE");
    conn.execute("INSERT INTO cities VALUES ('Portland, OR')", &[])
        .await
        .expect("INSERT");

    let csv = conn
        .export_table_to_csv("cities")
        .await
        .expect("export should succeed");

    // The comma in the value should be inside quotes
    assert!(
        csv.contains("\"Portland, OR\""),
        "value with comma must be quoted in CSV; got: {csv}"
    );
}

#[tokio::test]
async fn csv_export_null_values_become_empty_field() {
    let conn = open();
    conn.execute("CREATE TABLE t (a TEXT, b TEXT)", &[])
        .await
        .expect("CREATE TABLE");
    // Insert NULL for b
    conn.execute("INSERT INTO t VALUES ('hello', NULL)", &[])
        .await
        .expect("INSERT with NULL");

    let csv = conn
        .export_table_to_csv("t")
        .await
        .expect("export should succeed");

    // Data row should be "hello,"  (empty field for NULL)
    let data_lines: Vec<&str> = csv
        .split("\r\n")
        .filter(|l| !l.is_empty())
        .skip(1) // skip header
        .collect();
    assert_eq!(data_lines.len(), 1);
    // The field after the comma should be empty (NULL → empty string)
    let parts: Vec<&str> = data_lines[0].splitn(2, ',').collect();
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[1], "", "NULL must export as empty field");
}

// ── full round-trip (import then export) ─────────────────────────────────────

#[tokio::test]
async fn csv_full_round_trip_import_export() {
    let conn = open();
    let original_csv = "country,capital\r\nJapan,Tokyo\r\nFrance,Paris\r\nGermany,Berlin\r\n";

    let n = conn
        .import_csv("capitals", original_csv)
        .await
        .expect("import should succeed");
    assert_eq!(n, 3);

    let exported = conn
        .export_table_to_csv("capitals")
        .await
        .expect("export should succeed");

    // Re-import into a second table and verify row counts match
    conn.import_csv("capitals2", &exported)
        .await
        .expect("re-import from exported CSV should succeed");

    let rows1 = conn
        .query("SELECT * FROM capitals", &[])
        .await
        .expect("SELECT capitals");
    let rows2 = conn
        .query("SELECT * FROM capitals2", &[])
        .await
        .expect("SELECT capitals2");

    assert_eq!(
        rows1.len(),
        rows2.len(),
        "re-imported table must have same row count"
    );
}
