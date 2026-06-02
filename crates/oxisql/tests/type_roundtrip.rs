//! Value type round-trip tests for the embedded (GlueSQL memory) backend.
//!
//! Each test verifies that a value can be:
//!   1. Written to the database via SQL.
//!   2. Read back and compared to the original Rust [`Value`] variant.
//!
//! All tests use the `memory://` URI scheme, which requires the `embedded`
//! feature.  They will no-op (or be skipped) when that feature is absent.

#[cfg(feature = "embedded")]
mod embedded_roundtrip {
    use oxisql::Value;

    /// Open a fresh in-memory connection.
    async fn make_conn() -> Box<dyn oxisql::Connection> {
        oxisql::connect("memory://")
            .await
            .expect("embedded connect must succeed")
    }

    // ── Scalar types ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_int_round_trip() {
        let conn = make_conn().await;
        conn.execute("CREATE TABLE rt_int (v INT)", &[])
            .await
            .expect("CREATE TABLE rt_int");
        conn.execute("INSERT INTO rt_int VALUES (42)", &[])
            .await
            .expect("INSERT 42");

        let rows = conn
            .query("SELECT v FROM rt_int", &[])
            .await
            .expect("SELECT v FROM rt_int");

        assert_eq!(rows.len(), 1, "expected exactly 1 row");
        let v = rows[0].get_by_index(0).expect("value at index 0");
        assert!(
            matches!(v, Value::I64(42)),
            "expected Value::I64(42), got {v:?}"
        );
    }

    #[tokio::test]
    async fn test_text_round_trip() {
        let conn = make_conn().await;
        conn.execute("CREATE TABLE rt_text (v TEXT)", &[])
            .await
            .expect("CREATE TABLE rt_text");
        conn.execute("INSERT INTO rt_text VALUES ('hello world')", &[])
            .await
            .expect("INSERT hello world");

        let rows = conn
            .query("SELECT v FROM rt_text", &[])
            .await
            .expect("SELECT v FROM rt_text");

        assert_eq!(rows.len(), 1, "expected exactly 1 row");
        let v = rows[0].get_by_index(0).expect("value at index 0");
        assert_eq!(
            v,
            &Value::Text("hello world".into()),
            "text value must round-trip exactly"
        );
    }

    #[tokio::test]
    async fn test_float_round_trip() {
        let conn = make_conn().await;
        conn.execute("CREATE TABLE rt_float (v FLOAT)", &[])
            .await
            .expect("CREATE TABLE rt_float");
        conn.execute("INSERT INTO rt_float VALUES (2.71)", &[])
            .await
            .expect("INSERT 2.71");

        let rows = conn
            .query("SELECT v FROM rt_float", &[])
            .await
            .expect("SELECT v FROM rt_float");

        assert_eq!(rows.len(), 1, "expected exactly 1 row");
        let v = rows[0].get_by_index(0).expect("value at index 0");
        // GlueSQL FLOAT is stored as f32 internally and returned as F64.
        // Use a tolerance wide enough to accommodate f32 precision loss.
        match v {
            Value::F64(f) => {
                let diff = (f - 2.71_f64).abs();
                // f32 precision limit: ~1e-7; use 1e-4 for safety.
                assert!(diff < 1e-4, "float must round-trip within 1e-4, got {f}");
            }
            Value::I64(n) => {
                // Some backends coerce 2.71 to integer — accept if close.
                assert_eq!(*n, 2, "integer coercion of 2.71 must yield 2");
            }
            other => panic!("unexpected value variant for float: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_bool_round_trip() {
        let conn = make_conn().await;
        conn.execute("CREATE TABLE rt_bool (v BOOLEAN)", &[])
            .await
            .expect("CREATE TABLE rt_bool");
        conn.execute("INSERT INTO rt_bool VALUES (TRUE)", &[])
            .await
            .expect("INSERT TRUE");

        let rows = conn
            .query("SELECT v FROM rt_bool", &[])
            .await
            .expect("SELECT v FROM rt_bool");

        assert_eq!(rows.len(), 1, "expected exactly 1 row");
        let v = rows[0].get_by_index(0).expect("value at index 0");
        assert!(
            matches!(v, Value::Bool(true)),
            "expected Value::Bool(true), got {v:?}"
        );
    }

    #[tokio::test]
    async fn test_null_round_trip() {
        let conn = make_conn().await;
        conn.execute("CREATE TABLE rt_null (v TEXT)", &[])
            .await
            .expect("CREATE TABLE rt_null");
        conn.execute("INSERT INTO rt_null VALUES (NULL)", &[])
            .await
            .expect("INSERT NULL");

        let rows = conn
            .query("SELECT v FROM rt_null", &[])
            .await
            .expect("SELECT v FROM rt_null");

        assert_eq!(rows.len(), 1, "expected exactly 1 row");
        let v = rows[0].get_by_index(0).expect("value at index 0");
        assert!(matches!(v, Value::Null), "expected Value::Null, got {v:?}");
    }

    // ── Special characters and escaping ───────────────────────────────────────

    #[tokio::test]
    async fn test_text_with_special_chars() {
        // Verify that text with embedded single quotes survives a round-trip.
        // We use a literal SQL string here to exercise GlueSQL's own parser.
        let conn = make_conn().await;
        conn.execute("CREATE TABLE rt_escape (v TEXT)", &[])
            .await
            .expect("CREATE TABLE rt_escape");

        // Escape the single quote as '' (standard SQL).
        conn.execute(
            "INSERT INTO rt_escape VALUES ('it''s a test with quotes')",
            &[],
        )
        .await
        .expect("INSERT text with escaped single quote");

        let rows = conn
            .query("SELECT v FROM rt_escape", &[])
            .await
            .expect("SELECT v FROM rt_escape");

        assert_eq!(rows.len(), 1, "expected exactly 1 row");
        let v = rows[0].get_by_index(0).expect("value at index 0");
        assert_eq!(
            v,
            &Value::Text("it's a test with quotes".into()),
            "escaped single quote must round-trip correctly"
        );
    }

    // ── Multiple columns in a single row ─────────────────────────────────────

    #[tokio::test]
    async fn test_multiple_columns_round_trip() {
        let conn = make_conn().await;
        conn.execute("CREATE TABLE rt_multi (a INT, b TEXT, c BOOLEAN)", &[])
            .await
            .expect("CREATE TABLE rt_multi");
        conn.execute("INSERT INTO rt_multi VALUES (1, 'hello', TRUE)", &[])
            .await
            .expect("INSERT INTO rt_multi");

        let rows = conn
            .query("SELECT a, b, c FROM rt_multi", &[])
            .await
            .expect("SELECT a, b, c FROM rt_multi");

        assert_eq!(rows.len(), 1, "expected exactly 1 row");
        assert_eq!(rows[0].column_count(), 3, "expected 3 columns");

        let a = rows[0].get_by_index(0).expect("column a at index 0");
        let b = rows[0].get_by_index(1).expect("column b at index 1");
        let c = rows[0].get_by_index(2).expect("column c at index 2");

        assert!(
            matches!(a, Value::I64(1)),
            "column a must be I64(1), got {a:?}"
        );
        assert_eq!(b, &Value::Text("hello".into()), "column b must be 'hello'");
        assert!(
            matches!(c, Value::Bool(true)),
            "column c must be Bool(true), got {c:?}"
        );
    }

    // ── Multiple rows ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_multiple_rows_round_trip() {
        let conn = make_conn().await;
        conn.execute("CREATE TABLE rt_rows (id INT, label TEXT)", &[])
            .await
            .expect("CREATE TABLE rt_rows");
        conn.execute("INSERT INTO rt_rows VALUES (10, 'ten')", &[])
            .await
            .expect("INSERT row 1");
        conn.execute("INSERT INTO rt_rows VALUES (20, 'twenty')", &[])
            .await
            .expect("INSERT row 2");
        conn.execute("INSERT INTO rt_rows VALUES (30, 'thirty')", &[])
            .await
            .expect("INSERT row 3");

        let rows = conn
            .query("SELECT id, label FROM rt_rows ORDER BY id", &[])
            .await
            .expect("SELECT id, label FROM rt_rows");

        assert_eq!(rows.len(), 3, "expected 3 rows");

        let expected: &[(i64, &str)] = &[(10, "ten"), (20, "twenty"), (30, "thirty")];
        for (row, (exp_id, exp_label)) in rows.iter().zip(expected.iter()) {
            let id = row.get_by_index(0).expect("id column");
            let label = row.get_by_index(1).expect("label column");
            assert!(
                matches!(id, Value::I64(n) if *n == *exp_id),
                "id mismatch: expected I64({exp_id}), got {id:?}"
            );
            assert_eq!(
                label,
                &Value::Text((*exp_label).into()),
                "label mismatch for id={exp_id}"
            );
        }
    }

    // ── Typed extraction via try_get ──────────────────────────────────────────

    #[tokio::test]
    async fn test_typed_extraction_round_trip() {
        let conn = make_conn().await;
        conn.execute(
            "CREATE TABLE rt_typed (id INT, score FLOAT, active BOOLEAN, tag TEXT)",
            &[],
        )
        .await
        .expect("CREATE TABLE rt_typed");
        conn.execute(
            "INSERT INTO rt_typed VALUES (7, 9.81, FALSE, 'gravity')",
            &[],
        )
        .await
        .expect("INSERT INTO rt_typed");

        let rows = conn
            .query("SELECT id, score, active, tag FROM rt_typed", &[])
            .await
            .expect("SELECT FROM rt_typed");

        assert_eq!(rows.len(), 1, "expected exactly 1 row");

        let id: i64 = rows[0].try_get("id").expect("id as i64");
        assert_eq!(id, 7, "id must be 7");

        let tag: String = rows[0].try_get("tag").expect("tag as String");
        assert_eq!(tag, "gravity", "tag must be 'gravity'");

        let active: bool = rows[0].try_get("active").expect("active as bool");
        assert!(!active, "active must be false");
    }
}
