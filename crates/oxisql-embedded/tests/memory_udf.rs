use oxisql_core::{OxiSqlError, Value};
use oxisql_embedded::EmbeddedConnection;

// ── UDF registry tests ────────────────────────────────────────────────────────

#[test]
fn test_udf_registry_empty_default() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory should not fail");
    assert_eq!(
        conn.udf_count().expect("udf_count should not fail"),
        0,
        "fresh connection must have empty UDF registry"
    );
}

#[test]
fn test_register_and_call_udf() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory should not fail");

    // Register a 'double' scalar: double(x) = x * 2
    conn.register_udf("double", |args| match args.first() {
        Some(Value::I64(n)) => Value::I64(n * 2),
        _ => Value::Null,
    })
    .expect("register_udf must succeed");

    let result = conn
        .call_udf("double", vec![Value::I64(21)])
        .expect("call_udf must succeed for registered function");

    assert_eq!(result, Value::I64(42), "double(21) must equal 42");
}

#[test]
fn test_udf_unknown_name() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory should not fail");
    let result = conn.call_udf("nonexistent_udf", vec![]);
    assert!(
        result.is_err(),
        "call_udf on unregistered name must return Err"
    );
    assert!(
        matches!(result.unwrap_err(), OxiSqlError::Parse(_)),
        "unknown UDF must return OxiSqlError::Parse"
    );
}

#[test]
fn test_udf_register_overwrite() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory should not fail");

    conn.register_udf("double", |args| match args.first() {
        Some(Value::I64(n)) => Value::I64(n * 2),
        _ => Value::Null,
    })
    .expect("first register");

    // Overwrite with triple
    conn.register_udf("double", |args| match args.first() {
        Some(Value::I64(n)) => Value::I64(n * 3),
        _ => Value::Null,
    })
    .expect("second register (overwrite)");

    assert_eq!(
        conn.udf_count().unwrap(),
        1,
        "overwrite must not add a second entry"
    );

    let result = conn
        .call_udf("double", vec![Value::I64(7)])
        .expect("call after overwrite");
    assert_eq!(
        result,
        Value::I64(21),
        "after overwrite, double(7) should equal 21 (triple)"
    );
}

#[test]
fn test_udf_shared_across_clones() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory should not fail");
    conn.register_udf("negate", |args| match args.first() {
        Some(Value::I64(n)) => Value::I64(-n),
        _ => Value::Null,
    })
    .expect("register_udf");

    // Clone inherits the same Arc<RwLock<UdfRegistry>>
    let conn2 = conn.clone();
    let result = conn2
        .call_udf("negate", vec![Value::I64(5)])
        .expect("call_udf on cloned connection");
    assert_eq!(result, Value::I64(-5));
}

/// Verify that concurrent calls to `call_udf` do not deadlock.
///
/// `UdfRegistry` is protected by `Arc<std::sync::RwLock<UdfRegistry>>`.
/// `call_udf` acquires only a *read* lock, so multiple concurrent callers
/// can proceed simultaneously without waiting for each other.
#[tokio::test]
async fn test_udf_concurrent_reads() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory should not fail");
    conn.register_udf("square", |args| match args.first() {
        Some(Value::I64(n)) => Value::I64(n * n),
        _ => Value::Null,
    })
    .expect("register_udf square");

    // Multiple concurrent reads should not deadlock.
    let c1 = conn.clone();
    let c2 = conn.clone();
    let (r1, r2) = tokio::join!(
        async move { c1.call_udf("square", vec![Value::I64(5)]) },
        async move { c2.call_udf("square", vec![Value::I64(6)]) }
    );
    assert_eq!(r1.expect("call_udf c1 must succeed"), Value::I64(25));
    assert_eq!(r2.expect("call_udf c2 must succeed"), Value::I64(36));
}

#[test]
fn test_udf_multiple_functions() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory should not fail");

    conn.register_udf("add_one", |args| match args.first() {
        Some(Value::I64(n)) => Value::I64(n + 1),
        _ => Value::Null,
    })
    .expect("register add_one");

    conn.register_udf("to_text", |args| match args.first() {
        Some(Value::I64(n)) => Value::Text(n.to_string()),
        _ => Value::Null,
    })
    .expect("register to_text");

    assert_eq!(conn.udf_count().unwrap(), 2);

    let r1 = conn.call_udf("add_one", vec![Value::I64(99)]).unwrap();
    assert_eq!(r1, Value::I64(100));

    let r2 = conn.call_udf("to_text", vec![Value::I64(42)]).unwrap();
    assert_eq!(r2, Value::Text("42".into()));
}

// ── Savepoint tests ───────────────────────────────────────────────────────────

#[tokio::test]
async fn test_savepoint_no_op() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory should not fail");
    // Savepoints are no-ops on in-memory storage — verify no panic/error.
    let r = conn.savepoint("sp1").await;
    assert!(
        r.is_ok() || matches!(r, Err(OxiSqlError::UnsupportedUri(_))),
        "savepoint must return Ok or UnsupportedUri, got: {r:?}"
    );
}

#[tokio::test]
async fn test_rollback_to_savepoint_no_op() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory should not fail");
    // Create savepoint first, then roll back — both are no-ops.
    conn.savepoint("sp1").await.ok();
    let r = conn.rollback_to_savepoint("sp1").await;
    assert!(
        r.is_ok() || matches!(r, Err(OxiSqlError::UnsupportedUri(_))),
        "rollback_to_savepoint must return Ok or UnsupportedUri, got: {r:?}"
    );
}

#[tokio::test]
async fn test_release_savepoint_no_op() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory should not fail");
    conn.savepoint("sp1").await.ok();
    let r = conn.release_savepoint("sp1").await;
    assert!(
        r.is_ok() || matches!(r, Err(OxiSqlError::UnsupportedUri(_))),
        "release_savepoint must return Ok or UnsupportedUri, got: {r:?}"
    );
}

// ── Aggregate UDF tests ───────────────────────────────────────────────────────

#[tokio::test]
async fn test_aggregate_sum() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory should not fail");
    conn.register_aggregate(
        "sum_int",
        || Value::I64(0),
        |acc, v| match (acc, v) {
            (Value::I64(a), Value::I64(b)) => Value::I64(a + b),
            (a, _) => a,
        },
        |acc| acc,
    )
    .expect("register_aggregate should succeed");

    let result = conn
        .apply_aggregate("sum_int", vec![Value::I64(1), Value::I64(2), Value::I64(3)])
        .expect("apply_aggregate should succeed");
    assert_eq!(result, Value::I64(6));
}

#[tokio::test]
async fn test_aggregate_empty_values() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory should not fail");
    // Aggregate over empty input should return the init value via finalize.
    conn.register_aggregate(
        "sum_empty",
        || Value::I64(0),
        |acc, v| match (acc, v) {
            (Value::I64(a), Value::I64(b)) => Value::I64(a + b),
            (a, _) => a,
        },
        |acc| acc,
    )
    .expect("register_aggregate should succeed");

    let result = conn
        .apply_aggregate("sum_empty", vec![])
        .expect("apply_aggregate over empty slice should succeed");
    assert_eq!(result, Value::I64(0));
}

#[tokio::test]
async fn test_aggregate_unknown_name() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory should not fail");
    let r = conn.apply_aggregate("nonexistent_agg", vec![]);
    assert!(r.is_err(), "unknown aggregate must return Err");
    assert!(
        matches!(r.unwrap_err(), OxiSqlError::Parse(_)),
        "unknown aggregate must return OxiSqlError::Parse"
    );
}

#[tokio::test]
async fn test_aggregate_overwrite() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory should not fail");

    // Register a "count" aggregate initially.
    conn.register_aggregate(
        "my_agg",
        || Value::I64(0),
        |acc, _v| match acc {
            Value::I64(n) => Value::I64(n + 1),
            a => a,
        },
        |acc| acc,
    )
    .expect("first register_aggregate");

    // Overwrite with a "sum" aggregate.
    conn.register_aggregate(
        "my_agg",
        || Value::I64(0),
        |acc, v| match (acc, v) {
            (Value::I64(a), Value::I64(b)) => Value::I64(a + b),
            (a, _) => a,
        },
        |acc| acc,
    )
    .expect("second register_aggregate (overwrite)");

    let result = conn
        .apply_aggregate("my_agg", vec![Value::I64(10), Value::I64(20)])
        .expect("apply_aggregate after overwrite");
    // After overwrite to sum: 10 + 20 = 30
    assert_eq!(result, Value::I64(30));
}

#[tokio::test]
async fn test_aggregate_shared_across_clones() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory should not fail");
    conn.register_aggregate(
        "max_int",
        || Value::I64(i64::MIN),
        |acc, v| match (acc, v) {
            (Value::I64(a), Value::I64(b)) => Value::I64(a.max(b)),
            (a, _) => a,
        },
        |acc| acc,
    )
    .expect("register_aggregate");

    let conn2 = conn.clone();
    let result = conn2
        .apply_aggregate("max_int", vec![Value::I64(5), Value::I64(3), Value::I64(9)])
        .expect("apply_aggregate on cloned connection");
    assert_eq!(result, Value::I64(9));
}
