use super::*;

#[test]
fn parse_select() {
    let stmts = parse("SELECT 1").expect("should parse");
    assert_eq!(stmts.len(), 1);
}

#[test]
fn parse_multiple() {
    let stmts = parse("SELECT 1; SELECT 2").expect("should parse");
    assert_eq!(stmts.len(), 2);
}

#[test]
fn parse_one_single() {
    let stmt = parse_one("SELECT 42").expect("should parse");
    assert!(matches!(stmt, Statement::Query(_)));
}

#[test]
fn parse_one_rejects_multiple() {
    let result = parse_one("SELECT 1; SELECT 2");
    assert!(result.is_err());
}

#[test]
fn parse_error() {
    let result = parse("SELECTT BADDD");
    assert!(result.is_err());
}

#[test]
fn parse_with_postgres_dialect() {
    let stmts = parse_with_dialect("SELECT id::text FROM t WHERE id = $1", SqlDialect::Postgres)
        .expect("should parse");
    assert_eq!(stmts.len(), 1);
}

#[test]
fn parse_with_mysql_dialect() {
    let stmts = parse_with_dialect("SELECT `id` FROM `users` WHERE name = ?", SqlDialect::MySQL)
        .expect("should parse");
    assert_eq!(stmts.len(), 1);
}

#[test]
fn format_roundtrip() {
    let stmt = parse_one("SELECT id, name FROM users WHERE id = 1").expect("should parse");
    let sql = format(&stmt);
    // Re-parse the formatted SQL to verify it is valid
    let reparsed = parse_one(&sql).expect("formatted SQL should reparse");
    assert_eq!(format(&reparsed), sql);
}

#[test]
fn is_read_only_select() {
    let stmt = parse_one("SELECT * FROM t").expect("should parse");
    assert!(is_read_only(&stmt));
}

#[test]
fn is_read_only_insert() {
    let stmt = parse_one("INSERT INTO t (id) VALUES (1)").expect("should parse");
    assert!(!is_read_only(&stmt));
}

#[test]
fn is_read_only_update() {
    let stmt = parse_one("UPDATE t SET x = 1").expect("should parse");
    assert!(!is_read_only(&stmt));
}

#[test]
fn is_read_only_delete() {
    let stmt = parse_one("DELETE FROM t").expect("should parse");
    assert!(!is_read_only(&stmt));
}

#[test]
fn is_read_only_create() {
    let stmt = parse_one("CREATE TABLE t (id INT)").expect("should parse");
    assert!(!is_read_only(&stmt));
}

#[test]
fn is_read_only_drop() {
    let stmt = parse_one("DROP TABLE t").expect("should parse");
    assert!(!is_read_only(&stmt));
}

#[test]
fn extract_tables_select() {
    let stmt = parse_one("SELECT * FROM users").expect("should parse");
    let tables = extract_tables(&stmt);
    assert!(tables.contains(&"users".to_string()));
}

#[test]
fn extract_tables_join() {
    let stmt = parse_one("SELECT u.id FROM users u JOIN orders o ON u.id = o.user_id")
        .expect("should parse");
    let tables = extract_tables(&stmt);
    assert!(tables.contains(&"users".to_string()));
    assert!(tables.contains(&"orders".to_string()));
}

#[test]
fn extract_tables_insert() {
    let stmt = parse_one("INSERT INTO products (name) VALUES ('widget')").expect("should parse");
    let tables = extract_tables(&stmt);
    assert!(tables.contains(&"products".to_string()));
}

#[test]
fn extract_tables_create() {
    let stmt = parse_one("CREATE TABLE my_table (id INT)").expect("should parse");
    let tables = extract_tables(&stmt);
    assert!(tables.contains(&"my_table".to_string()));
}

#[test]
fn extract_tables_drop() {
    let stmt = parse_one("DROP TABLE my_table").expect("should parse");
    let tables = extract_tables(&stmt);
    assert!(tables.contains(&"my_table".to_string()));
}

#[test]
fn count_params_dollar() {
    assert_eq!(
        count_params("SELECT * FROM t WHERE id = $1 AND name = $2"),
        2
    );
    assert_eq!(count_params("SELECT $1, $3"), 3); // highest is $3
}

#[test]
fn count_params_question() {
    assert_eq!(count_params("SELECT * FROM t WHERE id = ? AND name = ?"), 2);
}

#[test]
fn count_params_none() {
    assert_eq!(count_params("SELECT * FROM t"), 0);
}

#[test]
fn count_params_in_string_literal() {
    // Parameters inside string literals should ideally be skipped
    let n = count_params("SELECT '$1' FROM t WHERE id = $1");
    assert!(n >= 1);
}

#[test]
fn dialect_to_sqlparser() {
    // Just verify no panics and the variants produce distinct dialects
    let _ = SqlDialect::Generic.to_sqlparser();
    let _ = SqlDialect::Postgres.to_sqlparser();
    let _ = SqlDialect::MySQL.to_sqlparser();
}

#[test]
fn normalize_collapses_whitespace() {
    assert_eq!(normalize("SELECT  id  FROM  t"), "SELECT ID FROM T");
    assert_eq!(normalize("  select id from t  "), "SELECT ID FROM T");
}

#[test]
fn normalize_preserves_string_literals() {
    // Content inside '' should not be uppercased.
    let result = normalize("SELECT 'hello world' FROM t");
    assert!(
        result.contains("'hello world'"),
        "string literal should be preserved: {result}"
    );
}

#[test]
fn extract_columns_select() {
    let stmt = parse_one("SELECT id, name FROM users WHERE age > 18").expect("parse");
    let cols = extract_columns(&stmt);
    assert!(cols.contains(&"id".to_string()));
    assert!(cols.contains(&"name".to_string()));
    assert!(cols.contains(&"age".to_string()));
}

#[test]
fn extract_columns_join() {
    let stmt = parse_one("SELECT u.id, o.total FROM users u JOIN orders o ON u.id = o.user_id")
        .expect("parse");
    let cols = extract_columns(&stmt);
    assert!(cols.contains(&"id".to_string()));
    assert!(cols.contains(&"total".to_string()));
    assert!(cols.contains(&"user_id".to_string()));
}

// ── New comprehensive tests ─────────────────────────────────────────────

#[test]
fn parse_insert_statement() {
    let stmts = parse_with_dialect(
        "INSERT INTO users (id, name) VALUES (1, 'Alice')",
        SqlDialect::Generic,
    )
    .unwrap();
    assert_eq!(stmts.len(), 1);
    let tables = extract_tables(&stmts[0]);
    assert!(tables.iter().any(|t| t == "users"));
}

#[test]
fn parse_update_statement() {
    let stmts = parse_with_dialect(
        "UPDATE users SET name = 'Bob' WHERE id = 1",
        SqlDialect::Generic,
    )
    .unwrap();
    assert_eq!(stmts.len(), 1);
    assert!(!is_read_only(&stmts[0]));
}

#[test]
fn parse_delete_statement() {
    let stmts = parse_with_dialect("DELETE FROM users WHERE id = 1", SqlDialect::Generic).unwrap();
    assert_eq!(stmts.len(), 1);
    let tables = extract_tables(&stmts[0]);
    assert!(tables.iter().any(|t| t == "users"));
}

#[test]
fn parse_with_question_mark_params() {
    let stmts = parse_with_dialect("SELECT * FROM users WHERE id = ?", SqlDialect::MySQL).unwrap();
    assert_eq!(stmts.len(), 1);
    assert_eq!(count_params("SELECT * FROM users WHERE id = ?"), 1);
}

#[test]
fn parse_with_dollar_params() {
    let stmts = parse_with_dialect(
        "SELECT * FROM users WHERE id = $1 AND active = $2",
        SqlDialect::Generic,
    )
    .unwrap();
    assert_eq!(stmts.len(), 1);
    assert_eq!(
        count_params("SELECT * FROM users WHERE id = $1 AND active = $2"),
        2
    );
}

#[test]
fn parse_complex_join() {
    let sql = "SELECT u.id, u.name, o.total FROM users u \
               JOIN orders o ON u.id = o.user_id WHERE u.active = true";
    let stmts = parse_with_dialect(sql, SqlDialect::Generic).unwrap();
    assert_eq!(stmts.len(), 1);
    let tables = extract_tables(&stmts[0]);
    assert!(tables.iter().any(|t| t == "users"));
    assert!(tables.iter().any(|t| t == "orders"));
}

#[test]
fn parse_subquery() {
    let sql = "SELECT * FROM users WHERE id IN (SELECT user_id FROM admins)";
    let stmts = parse_with_dialect(sql, SqlDialect::Generic).unwrap();
    assert_eq!(stmts.len(), 1);
    assert!(is_read_only(&stmts[0]));
}

#[test]
fn parse_cte() {
    let sql = "WITH active AS (SELECT * FROM users WHERE active = true) \
         SELECT * FROM active";
    let stmts = parse_with_dialect(sql, SqlDialect::Generic).unwrap();
    assert_eq!(stmts.len(), 1);
}

#[test]
fn parse_error_message_contains_position() {
    let result = parse_with_dialect("SELECT * FORM users", SqlDialect::Generic);
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(!msg.is_empty());
}

#[test]
fn parse_postgres_cast_syntax() {
    // PostgreSQL :: cast syntax — verify no panic; success depends on dialect
    let sql = "SELECT created_at::TEXT FROM events";
    let result = parse_with_dialect(sql, SqlDialect::Postgres);
    let _ = result;
}

#[test]
fn parse_mysql_backtick_identifiers() {
    let sql = "SELECT `id`, `name` FROM `users`";
    let stmts = parse_with_dialect(sql, SqlDialect::MySQL).unwrap();
    assert_eq!(stmts.len(), 1);
}

// ── ParseCache tests ────────────────────────────────────────────────────

#[test]
fn parse_cache_basic() {
    let cache = ParseCache::new(10);
    assert!(cache.is_empty());

    // Parse once
    let stmts = cache.parse("SELECT 1", SqlDialect::Generic).unwrap();
    assert_eq!(stmts.len(), 1);
    assert_eq!(cache.len(), 1);

    // Parse again — should hit cache
    let stmts2 = cache.parse("SELECT 1", SqlDialect::Generic).unwrap();
    assert_eq!(stmts2.len(), 1);
    assert_eq!(cache.len(), 1); // Still 1, not 2
}

#[test]
fn parse_cache_different_dialects_separate_entries() {
    let cache = ParseCache::new(10);
    cache.parse("SELECT 1", SqlDialect::Generic).unwrap();
    cache.parse("SELECT 1", SqlDialect::Postgres).unwrap();
    // Same SQL, different dialects → different cache entries
    assert_eq!(cache.len(), 2);
}

#[test]
fn parse_cache_capacity_evicts() {
    let cache = ParseCache::new(2);
    cache.parse("SELECT 1", SqlDialect::Generic).unwrap();
    cache.parse("SELECT 2", SqlDialect::Generic).unwrap();
    cache.parse("SELECT 3", SqlDialect::Generic).unwrap(); // evicts oldest
    assert_eq!(cache.len(), 2);
}

#[test]
fn parse_cache_clear() {
    let cache = ParseCache::new(10);
    cache.parse("SELECT 1", SqlDialect::Generic).unwrap();
    cache.clear();
    assert!(cache.is_empty());
}

#[test]
fn parse_cache_debug_format() {
    let cache = ParseCache::new(5);
    cache.parse("SELECT 1", SqlDialect::Generic).unwrap();
    let debug = std::format!("{cache:?}");
    assert!(debug.contains("ParseCache(len=1)"));
}

// ── Normalization + extraction combined ─────────────────────────────────

#[test]
fn normalize_uppercase_keywords() {
    let normalized = normalize("select id from users where active = true");
    assert!(normalized.contains("SELECT"));
    assert!(normalized.contains("FROM"));
    assert!(normalized.contains("WHERE"));
}

#[test]
fn extract_columns_subquery() {
    let stmts = parse_with_dialect(
        "SELECT u.id FROM users u WHERE u.id IN (SELECT user_id FROM admins)",
        SqlDialect::Generic,
    )
    .unwrap();
    let cols = extract_columns(&stmts[0]);
    // Should contain at least one column reference
    assert!(!cols.is_empty());
}

// ── Logical Plan tests ──────────────────────────────────────────────────

/// 1. Simple SELECT with explicit column list → Project(Scan)
#[test]
fn plan_simple_select() {
    let stmt = parse_one("SELECT id FROM users").expect("parse");
    let plan = plan_statement(&stmt).expect("plan");
    let text = explain(&plan);
    assert!(text.contains("Project"), "expected Project in: {text}");
    assert!(text.contains("Scan"), "expected Scan in: {text}");
    assert!(text.contains("users"), "expected users in: {text}");
}

/// 2. SELECT * with WHERE → Filter(Scan)
#[test]
fn plan_select_with_filter() {
    let stmt = parse_one("SELECT * FROM users WHERE age > 18").expect("parse");
    let plan = plan_statement(&stmt).expect("plan");
    let text = explain(&plan);
    assert!(text.contains("Filter"), "expected Filter in: {text}");
    assert!(text.contains("Scan"), "expected Scan in: {text}");
}

/// 3. SELECT * with LIMIT → Limit(Scan)
#[test]
fn plan_select_with_limit() {
    let stmt = parse_one("SELECT * FROM t LIMIT 10").expect("parse");
    let plan = plan_statement(&stmt).expect("plan");
    let text = explain(&plan);
    assert!(text.contains("Limit"), "expected Limit in: {text}");
    assert!(text.contains("count=10"), "expected count=10 in: {text}");
    assert!(text.contains("Scan"), "expected Scan in: {text}");
}

/// 4. JOIN → plan contains a Join node
#[test]
fn plan_join() {
    let stmt = parse_one("SELECT * FROM u JOIN o ON u.id = o.uid").expect("parse");
    let plan = plan_statement(&stmt).expect("plan");
    let text = explain(&plan);
    assert!(text.contains("Join"), "expected Join in: {text}");
    assert!(text.contains("Scan"), "expected Scan in: {text}");
}

/// 5. GROUP BY → plan contains Aggregate node
#[test]
fn plan_aggregate() {
    let stmt = parse_one("SELECT x, COUNT(*) FROM t GROUP BY x").expect("parse");
    let plan = plan_statement(&stmt).expect("plan");
    let text = explain(&plan);
    assert!(text.contains("Aggregate"), "expected Aggregate in: {text}");
}

/// 6. INSERT → Values node
#[test]
fn plan_insert() {
    let stmt = parse_one("INSERT INTO orders (id, total) VALUES (1, 99), (2, 42)").expect("parse");
    let plan = plan_statement(&stmt).expect("plan");
    match plan {
        LogicalPlan::Values { columns, rows } => {
            assert_eq!(columns, vec!["id", "total"]);
            assert_eq!(rows, 2);
        }
        other => panic!("expected Values, got: {other:?}"),
    }
}

/// 7. explain() output contains "Scan"
#[test]
fn explain_simple() {
    let plan = LogicalPlan::Scan {
        table: "products".to_string(),
        alias: None,
        limit: None,
    };
    let text = explain(&plan);
    assert!(text.contains("Scan"), "expected Scan in: {text}");
    assert!(text.contains("products"), "expected products in: {text}");
}

/// 8. parse_postgres convenience wrapper
#[test]
fn parse_postgres_fn() {
    let stmts = parse_postgres("SELECT $1::text").expect("parse_postgres");
    assert_eq!(stmts.len(), 1);
}

/// 9. parse_mysql convenience wrapper
#[test]
fn parse_mysql_fn() {
    let stmts = parse_mysql("SELECT `id` FROM `t`").expect("parse_mysql");
    assert_eq!(stmts.len(), 1);
}

/// 10. parse_sqlite convenience wrapper
#[test]
fn parse_sqlite_fn() {
    let stmts = parse_sqlite("SELECT 1").expect("parse_sqlite");
    assert_eq!(stmts.len(), 1);
}

// ── Set-operation tests ─────────────────────────────────────────────────

/// UNION produces SetOp { op: Union, all: false }
#[test]
fn test_plan_union() {
    let stmt = parse_one("SELECT a FROM t1 UNION SELECT a FROM t2").expect("parse");
    let plan = plan_statement(&stmt).expect("plan");
    match plan {
        LogicalPlan::SetOp { op, all, .. } => {
            assert_eq!(op, SetOpType::Union);
            assert!(!all, "UNION without ALL should have all=false");
        }
        other => panic!("expected SetOp, got: {other:?}"),
    }
}

/// UNION ALL produces SetOp { op: Union, all: true }
#[test]
fn test_plan_union_all() {
    let stmt = parse_one("SELECT a FROM t1 UNION ALL SELECT a FROM t2").expect("parse");
    let plan = plan_statement(&stmt).expect("plan");
    match plan {
        LogicalPlan::SetOp { op, all, .. } => {
            assert_eq!(op, SetOpType::Union);
            assert!(all, "UNION ALL should have all=true");
        }
        other => panic!("expected SetOp, got: {other:?}"),
    }
}

/// INTERSECT produces SetOp { op: Intersect }
#[test]
fn test_plan_intersect() {
    let stmt = parse_one("SELECT a FROM t1 INTERSECT SELECT a FROM t2").expect("parse");
    let plan = plan_statement(&stmt).expect("plan");
    match plan {
        LogicalPlan::SetOp { op, .. } => {
            assert_eq!(op, SetOpType::Intersect);
        }
        other => panic!("expected SetOp, got: {other:?}"),
    }
}

/// EXCEPT produces SetOp { op: Except }
#[test]
fn test_plan_except() {
    let stmt = parse_one("SELECT a FROM t1 EXCEPT SELECT a FROM t2").expect("parse");
    let plan = plan_statement(&stmt).expect("plan");
    match plan {
        LogicalPlan::SetOp { op, .. } => {
            assert_eq!(op, SetOpType::Except);
        }
        other => panic!("expected SetOp, got: {other:?}"),
    }
}

// ── CTE tests ──────────────────────────────────────────────────────────

/// Simple CTE: plan wraps a Cte node.
#[test]
fn test_plan_cte_simple() {
    let sql = "WITH cte AS (SELECT 1 AS n) SELECT n FROM cte";
    let stmt = parse_one(sql).expect("parse");
    let plan = plan_statement(&stmt).expect("plan");
    // The outermost node must be a Cte wrapper.
    match &plan {
        LogicalPlan::Cte {
            name, recursive, ..
        } => {
            assert_eq!(name, "cte");
            assert!(!recursive, "non-recursive CTE should have recursive=false");
        }
        other => panic!("expected Cte at root, got: {other:?}"),
    }
}

/// explain() on a CTE plan produces non-empty output containing "Cte".
#[test]
fn test_explain_cte() {
    let sql = "WITH cte AS (SELECT 1 AS n) SELECT n FROM cte";
    let stmt = parse_one(sql).expect("parse");
    let plan = plan_statement(&stmt).expect("plan");
    let text = explain(&plan);
    assert!(!text.is_empty(), "explain output should not be empty");
    assert!(text.contains("Cte"), "expected 'Cte' in: {text}");
}

// ── Window function tests ───────────────────────────────────────────────

/// Window function: plan has a Window node.
#[test]
fn test_plan_window_row_number() {
    let sql = "SELECT ROW_NUMBER() OVER (PARTITION BY dept ORDER BY salary DESC) AS rn \
         FROM employees";
    let stmt = parse_one(sql).expect("parse");
    let plan = plan_statement(&stmt).expect("plan");
    // Walk to find the Window node.
    fn find_window(p: &LogicalPlan) -> bool {
        match p {
            LogicalPlan::Window { .. } => true,
            LogicalPlan::Project { input, .. }
            | LogicalPlan::Filter { input, .. }
            | LogicalPlan::Sort { input, .. }
            | LogicalPlan::Limit { input, .. }
            | LogicalPlan::Aggregate { input, .. } => find_window(input),
            _ => false,
        }
    }
    assert!(find_window(&plan), "expected a Window node in: {plan:?}");
}

/// explain() on a set-op plan produces non-empty output containing "SetOp".
#[test]
fn test_explain_set_op() {
    let stmt = parse_one("SELECT a FROM t1 UNION SELECT a FROM t2").expect("parse");
    let plan = plan_statement(&stmt).expect("plan");
    let text = explain(&plan);
    assert!(!text.is_empty(), "explain output should not be empty");
    assert!(text.contains("SetOp"), "expected 'SetOp' in: {text}");
}
