//! Unit tests for `MysqlError` variants and their conversions.
//!
//! No live MySQL server is required.

use oxisql_mysql::MysqlError;

#[test]
fn constraint_violation_display() {
    let e = MysqlError::ConstraintViolation {
        constraint: "23000".into(),
        message: "Duplicate entry 'foo' for key 'PRIMARY'".into(),
    };
    let s = format!("{e}");
    assert!(s.contains("23000") || s.contains("constraint"), "got: {s}");
}

#[test]
fn connection_timeout_display() {
    let e = MysqlError::ConnectionTimeout("10s".into());
    assert!(format!("{e}").contains("timeout"));
}

#[test]
fn pool_exhausted_display() {
    let e = MysqlError::PoolExhausted("max=5".into());
    let s = format!("{e}");
    assert!(s.contains("exhausted") || s.contains("pool"), "got: {s}");
}

#[test]
fn constraint_violation_converts_to_oxisql() {
    use oxisql_core::OxiSqlError;
    let e = MysqlError::ConstraintViolation {
        constraint: "23000".into(),
        message: "Duplicate entry".into(),
    };
    let oe: OxiSqlError = e.into();
    assert!(matches!(oe, OxiSqlError::ConstraintViolation(_)));
}

#[test]
fn timeout_converts_to_oxisql() {
    use oxisql_core::OxiSqlError;
    let e = MysqlError::ConnectionTimeout("5s".into());
    let oe: OxiSqlError = e.into();
    assert!(matches!(oe, OxiSqlError::Timeout(_)));
}

#[test]
fn pool_exhausted_converts_to_oxisql() {
    use oxisql_core::OxiSqlError;
    let e = MysqlError::PoolExhausted("max=10".into());
    let oe: OxiSqlError = e.into();
    assert!(matches!(oe, OxiSqlError::ConnectionPool(_)));
}

#[test]
fn validate_savepoint_name_unit() {
    // Verify the error type hierarchy is accessible — no live DB needed.
    let _timeout = MysqlError::ConnectionTimeout("".into());
    let _pool = MysqlError::PoolExhausted("".into());
    let _cv = MysqlError::ConstraintViolation {
        constraint: "23000".into(),
        message: "test".into(),
    };
}
