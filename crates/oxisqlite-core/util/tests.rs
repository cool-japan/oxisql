//! Unit tests for util.rs, split out for size.

use super::*;
use limbo_sqlite3_parser::ast::{self, Expr, Id, Literal, Operator::*, Type};

#[test]
fn test_normalize_ident() {
    assert_eq!(normalize_ident("foo"), "foo");
    assert_eq!(normalize_ident("`foo`"), "foo");
    assert_eq!(normalize_ident("[foo]"), "foo");
    assert_eq!(normalize_ident("\"foo\""), "foo");
}

#[test]
fn test_anonymous_variable_comparison() {
    let expr1 = Expr::Variable("".to_string());
    let expr2 = Expr::Variable("".to_string());
    assert!(!exprs_are_equivalent(&expr1, &expr2));
}

#[test]
fn test_named_variable_comparison() {
    let expr1 = Expr::Variable("1".to_string());
    let expr2 = Expr::Variable("1".to_string());
    assert!(exprs_are_equivalent(&expr1, &expr2));

    let expr1 = Expr::Variable("1".to_string());
    let expr2 = Expr::Variable("2".to_string());
    assert!(!exprs_are_equivalent(&expr1, &expr2));
}

#[test]
fn test_basic_addition_exprs_are_equivalent() {
    let expr1 = Expr::Binary(
        Box::new(Expr::Literal(Literal::Numeric("826".to_string()))),
        Add,
        Box::new(Expr::Literal(Literal::Numeric("389".to_string()))),
    );
    let expr2 = Expr::Binary(
        Box::new(Expr::Literal(Literal::Numeric("389".to_string()))),
        Add,
        Box::new(Expr::Literal(Literal::Numeric("826".to_string()))),
    );
    assert!(exprs_are_equivalent(&expr1, &expr2));
}

#[test]
fn test_addition_expressions_equivalent_normalized() {
    let expr1 = Expr::Binary(
        Box::new(Expr::Literal(Literal::Numeric("123.0".to_string()))),
        Add,
        Box::new(Expr::Literal(Literal::Numeric("243".to_string()))),
    );
    let expr2 = Expr::Binary(
        Box::new(Expr::Literal(Literal::Numeric("243.0".to_string()))),
        Add,
        Box::new(Expr::Literal(Literal::Numeric("123".to_string()))),
    );
    assert!(exprs_are_equivalent(&expr1, &expr2));
}

#[test]
fn test_subtraction_expressions_not_equivalent() {
    let expr3 = Expr::Binary(
        Box::new(Expr::Literal(Literal::Numeric("364".to_string()))),
        Subtract,
        Box::new(Expr::Literal(Literal::Numeric("22.0".to_string()))),
    );
    let expr4 = Expr::Binary(
        Box::new(Expr::Literal(Literal::Numeric("22.0".to_string()))),
        Subtract,
        Box::new(Expr::Literal(Literal::Numeric("364".to_string()))),
    );
    assert!(!exprs_are_equivalent(&expr3, &expr4));
}

#[test]
fn test_subtraction_expressions_normalized() {
    let expr3 = Expr::Binary(
        Box::new(Expr::Literal(Literal::Numeric("66.0".to_string()))),
        Subtract,
        Box::new(Expr::Literal(Literal::Numeric("22".to_string()))),
    );
    let expr4 = Expr::Binary(
        Box::new(Expr::Literal(Literal::Numeric("66".to_string()))),
        Subtract,
        Box::new(Expr::Literal(Literal::Numeric("22.0".to_string()))),
    );
    assert!(exprs_are_equivalent(&expr3, &expr4));
}

#[test]
fn test_expressions_equivalent_case_insensitive_functioncalls() {
    let func1 = Expr::FunctionCall {
        name: Id("SUM".to_string()),
        distinctness: None,
        args: Some(vec![Expr::Id(Id("x".to_string()))]),
        order_by: None,
        filter_over: None,
    };
    let func2 = Expr::FunctionCall {
        name: Id("sum".to_string()),
        distinctness: None,
        args: Some(vec![Expr::Id(Id("x".to_string()))]),
        order_by: None,
        filter_over: None,
    };
    assert!(exprs_are_equivalent(&func1, &func2));

    let func3 = Expr::FunctionCall {
        name: Id("SUM".to_string()),
        distinctness: Some(ast::Distinctness::Distinct),
        args: Some(vec![Expr::Id(Id("x".to_string()))]),
        order_by: None,
        filter_over: None,
    };
    assert!(!exprs_are_equivalent(&func1, &func3));
}

#[test]
fn test_expressions_equivalent_identical_fn_with_distinct() {
    let sum = Expr::FunctionCall {
        name: Id("SUM".to_string()),
        distinctness: None,
        args: Some(vec![Expr::Id(Id("x".to_string()))]),
        order_by: None,
        filter_over: None,
    };
    let sum_distinct = Expr::FunctionCall {
        name: Id("SUM".to_string()),
        distinctness: Some(ast::Distinctness::Distinct),
        args: Some(vec![Expr::Id(Id("x".to_string()))]),
        order_by: None,
        filter_over: None,
    };
    assert!(!exprs_are_equivalent(&sum, &sum_distinct));
}

#[test]
fn test_expressions_equivalent_multiplication() {
    let expr1 = Expr::Binary(
        Box::new(Expr::Literal(Literal::Numeric("42.0".to_string()))),
        Multiply,
        Box::new(Expr::Literal(Literal::Numeric("38".to_string()))),
    );
    let expr2 = Expr::Binary(
        Box::new(Expr::Literal(Literal::Numeric("38.0".to_string()))),
        Multiply,
        Box::new(Expr::Literal(Literal::Numeric("42".to_string()))),
    );
    assert!(exprs_are_equivalent(&expr1, &expr2));
}

#[test]
fn test_expressions_both_parenthesized_equivalent() {
    let expr1 = Expr::Parenthesized(vec![Expr::Binary(
        Box::new(Expr::Literal(Literal::Numeric("683".to_string()))),
        Add,
        Box::new(Expr::Literal(Literal::Numeric("799.0".to_string()))),
    )]);
    let expr2 = Expr::Binary(
        Box::new(Expr::Literal(Literal::Numeric("799".to_string()))),
        Add,
        Box::new(Expr::Literal(Literal::Numeric("683".to_string()))),
    );
    assert!(exprs_are_equivalent(&expr1, &expr2));
}
#[test]
fn test_expressions_parenthesized_equivalent() {
    let expr7 = Expr::Parenthesized(vec![Expr::Binary(
        Box::new(Expr::Literal(Literal::Numeric("6".to_string()))),
        Add,
        Box::new(Expr::Literal(Literal::Numeric("7".to_string()))),
    )]);
    let expr8 = Expr::Binary(
        Box::new(Expr::Literal(Literal::Numeric("6".to_string()))),
        Add,
        Box::new(Expr::Literal(Literal::Numeric("7".to_string()))),
    );
    assert!(exprs_are_equivalent(&expr7, &expr8));
}

#[test]
fn test_like_expressions_equivalent() {
    let expr1 = Expr::Like {
        lhs: Box::new(Expr::Id(Id("name".to_string()))),
        not: false,
        op: ast::LikeOperator::Like,
        rhs: Box::new(Expr::Literal(Literal::String("%john%".to_string()))),
        escape: Some(Box::new(Expr::Literal(Literal::String("\\".to_string())))),
    };
    let expr2 = Expr::Like {
        lhs: Box::new(Expr::Id(Id("name".to_string()))),
        not: false,
        op: ast::LikeOperator::Like,
        rhs: Box::new(Expr::Literal(Literal::String("%john%".to_string()))),
        escape: Some(Box::new(Expr::Literal(Literal::String("\\".to_string())))),
    };
    assert!(exprs_are_equivalent(&expr1, &expr2));
}

#[test]
fn test_expressions_equivalent_like_escaped() {
    let expr1 = Expr::Like {
        lhs: Box::new(Expr::Id(Id("name".to_string()))),
        not: false,
        op: ast::LikeOperator::Like,
        rhs: Box::new(Expr::Literal(Literal::String("%john%".to_string()))),
        escape: Some(Box::new(Expr::Literal(Literal::String("\\".to_string())))),
    };
    let expr2 = Expr::Like {
        lhs: Box::new(Expr::Id(Id("name".to_string()))),
        not: false,
        op: ast::LikeOperator::Like,
        rhs: Box::new(Expr::Literal(Literal::String("%john%".to_string()))),
        escape: Some(Box::new(Expr::Literal(Literal::String("#".to_string())))),
    };
    assert!(!exprs_are_equivalent(&expr1, &expr2));
}
#[test]
fn test_expressions_equivalent_between() {
    let expr1 = Expr::Between {
        lhs: Box::new(Expr::Id(Id("age".to_string()))),
        not: false,
        start: Box::new(Expr::Literal(Literal::Numeric("18".to_string()))),
        end: Box::new(Expr::Literal(Literal::Numeric("65".to_string()))),
    };
    let expr2 = Expr::Between {
        lhs: Box::new(Expr::Id(Id("age".to_string()))),
        not: false,
        start: Box::new(Expr::Literal(Literal::Numeric("18".to_string()))),
        end: Box::new(Expr::Literal(Literal::Numeric("65".to_string()))),
    };
    assert!(exprs_are_equivalent(&expr1, &expr2));

    // differing BETWEEN bounds
    let expr3 = Expr::Between {
        lhs: Box::new(Expr::Id(Id("age".to_string()))),
        not: false,
        start: Box::new(Expr::Literal(Literal::Numeric("20".to_string()))),
        end: Box::new(Expr::Literal(Literal::Numeric("65".to_string()))),
    };
    assert!(!exprs_are_equivalent(&expr1, &expr3));
}
#[test]
fn test_cast_exprs_equivalent() {
    let cast1 = Expr::Cast {
        expr: Box::new(Expr::Literal(Literal::Numeric("123".to_string()))),
        type_name: Some(Type {
            name: "INTEGER".to_string(),
            size: None,
        }),
    };

    let cast2 = Expr::Cast {
        expr: Box::new(Expr::Literal(Literal::Numeric("123".to_string()))),
        type_name: Some(Type {
            name: "integer".to_string(),
            size: None,
        }),
    };
    assert!(exprs_are_equivalent(&cast1, &cast2));
}

#[test]
fn test_ident_equivalency() {
    assert!(check_ident_equivalency("\"foo\"", "foo"));
    assert!(check_ident_equivalency("[foo]", "foo"));
    assert!(check_ident_equivalency("`FOO`", "foo"));
    assert!(check_ident_equivalency("\"foo\"", "`FOO`"));
    assert!(!check_ident_equivalency("\"foo\"", "[bar]"));
    assert!(!check_ident_equivalency("foo", "\"bar\""));
}

#[test]
fn test_simple_uri() {
    let uri = "file:/home/user/db.sqlite";
    let opts = parse_sqlite_uri(uri).unwrap();
    assert_eq!(opts.path, "/home/user/db.sqlite");
    assert_eq!(opts.authority, None);
}

#[test]
fn test_uri_with_authority() {
    let uri = "file://localhost/home/user/db.sqlite";
    let opts = parse_sqlite_uri(uri).unwrap();
    assert_eq!(opts.path, "/home/user/db.sqlite");
    assert_eq!(opts.authority, Some("localhost"));
}

#[test]
fn test_uri_with_invalid_authority() {
    let uri = "file://example.com/home/user/db.sqlite";
    let result = parse_sqlite_uri(uri);
    assert!(result.is_err());
}

#[test]
fn test_uri_with_query_params() {
    let uri = "file:/home/user/db.sqlite?vfs=unix&mode=ro&immutable=1";
    let opts = parse_sqlite_uri(uri).unwrap();
    assert_eq!(opts.path, "/home/user/db.sqlite");
    assert_eq!(opts.vfs, Some("unix".to_string()));
    assert_eq!(opts.mode, OpenMode::ReadOnly);
    assert_eq!(opts.immutable, true);
}

#[test]
fn test_uri_with_fragment() {
    let uri = "file:/home/user/db.sqlite#section1";
    let opts = parse_sqlite_uri(uri).unwrap();
    assert_eq!(opts.path, "/home/user/db.sqlite");
}

#[test]
fn test_uri_with_percent_encoding() {
    let uri = "file:/home/user/db%20with%20spaces.sqlite?vfs=unix";
    let opts = parse_sqlite_uri(uri).unwrap();
    assert_eq!(opts.path, "/home/user/db with spaces.sqlite");
    assert_eq!(opts.vfs, Some("unix".to_string()));
}

#[test]
fn test_uri_without_scheme() {
    let uri = "/home/user/db.sqlite";
    let result = parse_sqlite_uri(uri);
    assert!(result.is_ok());
    assert_eq!(result.unwrap().path, "/home/user/db.sqlite");
}

#[test]
fn test_uri_with_empty_query() {
    let uri = "file:/home/user/db.sqlite?";
    let opts = parse_sqlite_uri(uri).unwrap();
    assert_eq!(opts.path, "/home/user/db.sqlite");
    assert_eq!(opts.vfs, None);
}

#[test]
fn test_uri_with_partial_query() {
    let uri = "file:/home/user/db.sqlite?mode=rw";
    let opts = parse_sqlite_uri(uri).unwrap();
    assert_eq!(opts.path, "/home/user/db.sqlite");
    assert_eq!(opts.mode, OpenMode::ReadWrite);
    assert_eq!(opts.vfs, None);
}

#[test]
fn test_uri_windows_style_path() {
    let uri = "file:///C:/Users/test/db.sqlite";
    let opts = parse_sqlite_uri(uri).unwrap();
    assert_eq!(opts.path, "/C:/Users/test/db.sqlite");
}

#[test]
fn test_uri_with_only_query_params() {
    let uri = "file:?mode=memory&cache=shared";
    let opts = parse_sqlite_uri(uri).unwrap();
    assert_eq!(opts.path, "");
    assert_eq!(opts.mode, OpenMode::Memory);
    assert_eq!(opts.cache, CacheMode::Shared);
}

#[test]
fn test_uri_with_only_fragment() {
    let uri = "file:#fragment";
    let opts = parse_sqlite_uri(uri).unwrap();
    assert_eq!(opts.path, "");
}

#[test]
fn test_uri_with_invalid_scheme() {
    let uri = "http:/home/user/db.sqlite";
    let result = parse_sqlite_uri(uri);
    assert!(result.is_ok());
    assert_eq!(result.unwrap().path, "http:/home/user/db.sqlite");
}

#[test]
fn test_uri_with_multiple_query_params() {
    let uri = "file:/home/user/db.sqlite?vfs=unix&mode=rw&cache=private&immutable=0";
    let opts = parse_sqlite_uri(uri).unwrap();
    assert_eq!(opts.path, "/home/user/db.sqlite");
    assert_eq!(opts.vfs, Some("unix".to_string()));
    assert_eq!(opts.mode, OpenMode::ReadWrite);
    assert_eq!(opts.cache, CacheMode::Private);
    assert_eq!(opts.immutable, false);
}

#[test]
fn test_uri_with_unknown_query_param() {
    let uri = "file:/home/user/db.sqlite?unknown=param";
    let opts = parse_sqlite_uri(uri).unwrap();
    assert_eq!(opts.path, "/home/user/db.sqlite");
    assert_eq!(opts.vfs, None);
}

#[test]
fn test_uri_with_multiple_equal_signs() {
    let uri = "file:/home/user/db.sqlite?vfs=unix=custom";
    let opts = parse_sqlite_uri(uri).unwrap();
    assert_eq!(opts.path, "/home/user/db.sqlite");
    assert_eq!(opts.vfs, Some("unix=custom".to_string()));
}

#[test]
fn test_uri_with_trailing_slash() {
    let uri = "file:/home/user/db.sqlite/";
    let opts = parse_sqlite_uri(uri).unwrap();
    assert_eq!(opts.path, "/home/user/db.sqlite/");
}

#[test]
fn test_uri_with_encoded_characters_in_query() {
    let uri = "file:/home/user/db.sqlite?vfs=unix%20mode";
    let opts = parse_sqlite_uri(uri).unwrap();
    assert_eq!(opts.path, "/home/user/db.sqlite");
    assert_eq!(opts.vfs, Some("unix mode".to_string()));
}

#[test]
fn test_uri_windows_network_path() {
    let uri = "file://server/share/db.sqlite";
    let result = parse_sqlite_uri(uri);
    assert!(result.is_err()); // non-localhost authority should fail
}

#[test]
fn test_uri_windows_drive_letter_with_slash() {
    let uri = "file:///C:/database.sqlite";
    let opts = parse_sqlite_uri(uri).unwrap();
    assert_eq!(opts.path, "/C:/database.sqlite");
}

#[test]
fn test_localhost_with_double_slash_and_no_path() {
    let uri = "file://localhost";
    let opts = parse_sqlite_uri(uri).unwrap();
    assert_eq!(opts.path, "");
    assert_eq!(opts.authority, Some("localhost"));
}

#[test]
fn test_uri_windows_drive_letter_without_slash() {
    let uri = "file:///C:/database.sqlite";
    let opts = parse_sqlite_uri(uri).unwrap();
    assert_eq!(opts.path, "/C:/database.sqlite");
}

#[test]
fn test_improper_mode() {
    // any other mode but ro, rwc, rw, memory should fail per sqlite

    let uri = "file:data.db?mode=readonly";
    let res = parse_sqlite_uri(uri);
    assert!(res.is_err());
    // including empty
    let uri = "file:/home/user/db.sqlite?vfs=&mode=";
    let res = parse_sqlite_uri(uri);
    assert!(res.is_err());
}

// Some examples from https://www.sqlite.org/c3ref/open.html#urifilenameexamples
#[test]
fn test_simple_file_current_dir() {
    let uri = "file:data.db";
    let opts = parse_sqlite_uri(uri).unwrap();
    assert_eq!(opts.path, "data.db");
    assert_eq!(opts.authority, None);
    assert_eq!(opts.vfs, None);
    assert_eq!(opts.mode, OpenMode::ReadWriteCreate);
}

#[test]
fn test_simple_file_three_slash() {
    let uri = "file:///home/data/data.db";
    let opts = parse_sqlite_uri(uri).unwrap();
    assert_eq!(opts.path, "/home/data/data.db");
    assert_eq!(opts.authority, None);
    assert_eq!(opts.vfs, None);
    assert_eq!(opts.mode, OpenMode::ReadWriteCreate);
}

#[test]
fn test_simple_file_two_slash_localhost() {
    let uri = "file://localhost/home/fred/data.db";
    let opts = parse_sqlite_uri(uri).unwrap();
    assert_eq!(opts.path, "/home/fred/data.db");
    assert_eq!(opts.authority, Some("localhost"));
    assert_eq!(opts.vfs, None);
}

#[test]
fn test_windows_double_invalid() {
    let uri = "file://C:/home/fred/data.db?mode=ro";
    let opts = parse_sqlite_uri(uri);
    assert!(opts.is_err());
}

#[test]
fn test_simple_file_two_slash() {
    let uri = "file:///C:/Documents%20and%20Settings/fred/Desktop/data.db";
    let opts = parse_sqlite_uri(uri).unwrap();
    assert_eq!(opts.path, "/C:/Documents and Settings/fred/Desktop/data.db");
    assert_eq!(opts.vfs, None);
}

#[test]
fn test_decode_percent_basic() {
    assert_eq!(decode_percent("hello%20world"), "hello world");
    assert_eq!(decode_percent("file%3Adata.db"), "file:data.db");
    assert_eq!(decode_percent("path%2Fto%2Ffile"), "path/to/file");
}

#[test]
fn test_decode_percent_edge_cases() {
    assert_eq!(decode_percent(""), "");
    assert_eq!(decode_percent("plain_text"), "plain_text");
    assert_eq!(
        decode_percent("%2Fhome%2Fuser%2Fdb.sqlite"),
        "/home/user/db.sqlite"
    );
    // multiple percent-encoded characters in sequence
    assert_eq!(decode_percent("%41%42%43"), "ABC");
    assert_eq!(decode_percent("%61%62%63"), "abc");
}

#[test]
fn test_decode_percent_invalid_sequences() {
    // invalid percent encoding (single % without two hex digits)
    assert_eq!(decode_percent("hello%"), "hello%");
    // only one hex digit after %
    assert_eq!(decode_percent("file%2"), "file%2");
    // invalid hex digits (not 0-9, A-F, a-f)
    assert_eq!(decode_percent("file%2X.db"), "file%2X.db");

    // Incomplete sequence at the end, leave untouched
    assert_eq!(decode_percent("path%2Fto%2"), "path/to%2");
}

#[test]
fn test_decode_percent_mixed_valid_invalid() {
    assert_eq!(decode_percent("hello%20world%"), "hello world%");
    assert_eq!(decode_percent("%2Fpath%2Xto%2Ffile"), "/path%2Xto/file");
    assert_eq!(decode_percent("file%3Adata.db%2"), "file:data.db%2");
}

#[test]
fn test_decode_percent_special_characters() {
    assert_eq!(
        decode_percent("%21%40%23%24%25%5E%26%2A%28%29"),
        "!@#$%^&*()"
    );
    assert_eq!(decode_percent("%5B%5D%7B%7D%7C%5C%3A"), "[]{}|\\:");
}

#[test]
fn test_decode_percent_unmodified_valid_text() {
    // ensure already valid text remains unchanged
    assert_eq!(
        decode_percent("C:/Users/Example/Database.sqlite"),
        "C:/Users/Example/Database.sqlite"
    );
    assert_eq!(
        decode_percent("/home/user/db.sqlite"),
        "/home/user/db.sqlite"
    );
}

#[test]
fn test_text_to_integer() {
    assert_eq!(cast_text_to_integer("1"), Value::Integer(1),);
    assert_eq!(cast_text_to_integer("-1"), Value::Integer(-1),);
    assert_eq!(
        cast_text_to_integer("1823400-00000"),
        Value::Integer(1823400),
    );
    assert_eq!(cast_text_to_integer("-10000000"), Value::Integer(-10000000),);
    assert_eq!(cast_text_to_integer("123xxx"), Value::Integer(123),);
    assert_eq!(
        cast_text_to_integer("9223372036854775807"),
        Value::Integer(i64::MAX),
    );
    assert_eq!(
        cast_text_to_integer("9223372036854775808"),
        Value::Integer(0),
    );
    assert_eq!(
        cast_text_to_integer("-9223372036854775808"),
        Value::Integer(i64::MIN),
    );
    assert_eq!(
        cast_text_to_integer("-9223372036854775809"),
        Value::Integer(0),
    );
    assert_eq!(cast_text_to_integer("-"), Value::Integer(0),);
}

#[test]
fn test_text_to_real() {
    assert_eq!(cast_text_to_real("1"), Value::Float(1.0));
    assert_eq!(cast_text_to_real("-1"), Value::Float(-1.0));
    assert_eq!(cast_text_to_real("1.0"), Value::Float(1.0));
    assert_eq!(cast_text_to_real("-1.0"), Value::Float(-1.0));
    assert_eq!(cast_text_to_real("1e10"), Value::Float(1e10));
    assert_eq!(cast_text_to_real("-1e10"), Value::Float(-1e10));
    assert_eq!(cast_text_to_real("1e-10"), Value::Float(1e-10));
    assert_eq!(cast_text_to_real("-1e-10"), Value::Float(-1e-10));
    assert_eq!(cast_text_to_real("1.123e10"), Value::Float(1.123e10));
    assert_eq!(cast_text_to_real("-1.123e10"), Value::Float(-1.123e10));
    assert_eq!(cast_text_to_real("1.123e-10"), Value::Float(1.123e-10));
    assert_eq!(cast_text_to_real("-1.123-e-10"), Value::Float(-1.123));
    assert_eq!(cast_text_to_real("1-282584294928"), Value::Float(1.0));
    assert_eq!(
        cast_text_to_real("1.7976931348623157e309"),
        Value::Float(f64::INFINITY),
    );
    assert_eq!(
        cast_text_to_real("-1.7976931348623157e308"),
        Value::Float(f64::MIN),
    );
    assert_eq!(
        cast_text_to_real("-1.7976931348623157e309"),
        Value::Float(f64::NEG_INFINITY),
    );
    assert_eq!(cast_text_to_real("1E"), Value::Float(1.0));
    assert_eq!(cast_text_to_real("1EE"), Value::Float(1.0));
    assert_eq!(cast_text_to_real("-1E"), Value::Float(-1.0));
    assert_eq!(cast_text_to_real("1."), Value::Float(1.0));
    assert_eq!(cast_text_to_real("-1."), Value::Float(-1.0));
    assert_eq!(cast_text_to_real("1.23E"), Value::Float(1.23));
    assert_eq!(cast_text_to_real(".1.23E-"), Value::Float(0.1));
    assert_eq!(cast_text_to_real("0"), Value::Float(0.0));
    assert_eq!(cast_text_to_real("-0"), Value::Float(0.0));
    assert_eq!(cast_text_to_real("-0"), Value::Float(0.0));
    assert_eq!(cast_text_to_real("-0.0"), Value::Float(0.0));
    assert_eq!(cast_text_to_real("0.0"), Value::Float(0.0));
    assert_eq!(cast_text_to_real("-"), Value::Float(0.0));
}

#[test]
fn test_text_to_numeric() {
    assert_eq!(cast_text_to_numeric("1"), Value::Integer(1));
    assert_eq!(cast_text_to_numeric("-1"), Value::Integer(-1));
    assert_eq!(
        cast_text_to_numeric("1823400-00000"),
        Value::Integer(1823400)
    );
    assert_eq!(cast_text_to_numeric("-10000000"), Value::Integer(-10000000));
    assert_eq!(cast_text_to_numeric("123xxx"), Value::Integer(123));
    assert_eq!(
        cast_text_to_numeric("9223372036854775807"),
        Value::Integer(i64::MAX)
    );
    assert_eq!(
        cast_text_to_numeric("9223372036854775808"),
        Value::Float(9.22337203685478e18)
    ); // Exceeds i64, becomes float
    assert_eq!(
        cast_text_to_numeric("-9223372036854775808"),
        Value::Integer(i64::MIN)
    );
    assert_eq!(
        cast_text_to_numeric("-9223372036854775809"),
        Value::Float(-9.22337203685478e18)
    ); // Exceeds i64, becomes float

    assert_eq!(cast_text_to_numeric("1.0"), Value::Float(1.0));
    assert_eq!(cast_text_to_numeric("-1.0"), Value::Float(-1.0));
    assert_eq!(cast_text_to_numeric("1e10"), Value::Float(1e10));
    assert_eq!(cast_text_to_numeric("-1e10"), Value::Float(-1e10));
    assert_eq!(cast_text_to_numeric("1e-10"), Value::Float(1e-10));
    assert_eq!(cast_text_to_numeric("-1e-10"), Value::Float(-1e-10));
    assert_eq!(cast_text_to_numeric("1.123e10"), Value::Float(1.123e10));
    assert_eq!(cast_text_to_numeric("-1.123e10"), Value::Float(-1.123e10));
    assert_eq!(cast_text_to_numeric("1.123e-10"), Value::Float(1.123e-10));
    assert_eq!(cast_text_to_numeric("-1.123-e-10"), Value::Float(-1.123));
    assert_eq!(cast_text_to_numeric("1-282584294928"), Value::Integer(1));
    assert_eq!(cast_text_to_numeric("xxx"), Value::Integer(0));
    assert_eq!(
        cast_text_to_numeric("1.7976931348623157e309"),
        Value::Float(f64::INFINITY)
    );
    assert_eq!(
        cast_text_to_numeric("-1.7976931348623157e308"),
        Value::Float(f64::MIN)
    );
    assert_eq!(
        cast_text_to_numeric("-1.7976931348623157e309"),
        Value::Float(f64::NEG_INFINITY)
    );

    assert_eq!(cast_text_to_numeric("1E"), Value::Float(1.0));
    assert_eq!(cast_text_to_numeric("1EE"), Value::Float(1.0));
    assert_eq!(cast_text_to_numeric("-1E"), Value::Float(-1.0));
    assert_eq!(cast_text_to_numeric("1."), Value::Float(1.0));
    assert_eq!(cast_text_to_numeric("-1."), Value::Float(-1.0));
    assert_eq!(cast_text_to_numeric("1.23E"), Value::Float(1.23));
    assert_eq!(cast_text_to_numeric("1.23E-"), Value::Float(1.23));

    assert_eq!(cast_text_to_numeric("0"), Value::Integer(0));
    assert_eq!(cast_text_to_numeric("-0"), Value::Integer(0));
    assert_eq!(cast_text_to_numeric("-0.0"), Value::Float(0.0));
    assert_eq!(cast_text_to_numeric("0.0"), Value::Float(0.0));
    assert_eq!(cast_text_to_numeric("-"), Value::Integer(0));
    assert_eq!(cast_text_to_numeric("-e"), Value::Integer(0));
    assert_eq!(cast_text_to_numeric("-E"), Value::Integer(0));
}

#[test]
fn test_parse_numeric_str_valid_integer() {
    assert_eq!(parse_numeric_str("123"), Ok((ValueType::Integer, "123")));
    assert_eq!(parse_numeric_str("-456"), Ok((ValueType::Integer, "-456")));
    assert_eq!(
        parse_numeric_str("000789"),
        Ok((ValueType::Integer, "000789"))
    );
}

#[test]
fn test_parse_numeric_str_valid_float() {
    assert_eq!(
        parse_numeric_str("123.456"),
        Ok((ValueType::Float, "123.456"))
    );
    assert_eq!(
        parse_numeric_str("-0.789"),
        Ok((ValueType::Float, "-0.789"))
    );
    assert_eq!(parse_numeric_str("1e10"), Ok((ValueType::Float, "1e10")));
    assert_eq!(
        parse_numeric_str("-1.23e-4"),
        Ok((ValueType::Float, "-1.23e-4"))
    );
    assert_eq!(
        parse_numeric_str("1.23E+4"),
        Ok((ValueType::Float, "1.23E+4"))
    );
    assert_eq!(parse_numeric_str("1.2.3"), Ok((ValueType::Float, "1.2")))
}

#[test]
fn test_parse_numeric_str_edge_cases() {
    assert_eq!(parse_numeric_str("1e"), Ok((ValueType::Float, "1")));
    assert_eq!(parse_numeric_str("1e-"), Ok((ValueType::Float, "1")));
    assert_eq!(parse_numeric_str("1e+"), Ok((ValueType::Float, "1")));
    assert_eq!(parse_numeric_str("-1e"), Ok((ValueType::Float, "-1")));
    assert_eq!(parse_numeric_str("-1e-"), Ok((ValueType::Float, "-1")));
}

#[test]
fn test_parse_numeric_str_invalid() {
    assert_eq!(parse_numeric_str(""), Err(()));
    assert_eq!(parse_numeric_str("abc"), Err(()));
    assert_eq!(parse_numeric_str("-"), Err(()));
    assert_eq!(parse_numeric_str("e10"), Err(()));
    assert_eq!(parse_numeric_str(".e10"), Err(()));
}

#[test]
fn test_parse_numeric_str_with_whitespace() {
    assert_eq!(parse_numeric_str("   123"), Ok((ValueType::Integer, "123")));
    assert_eq!(
        parse_numeric_str("  -456.78  "),
        Ok((ValueType::Float, "-456.78"))
    );
    assert_eq!(
        parse_numeric_str("  1.23e4  "),
        Ok((ValueType::Float, "1.23e4"))
    );
}

#[test]
fn test_parse_numeric_str_leading_zeros() {
    assert_eq!(
        parse_numeric_str("000123"),
        Ok((ValueType::Integer, "000123"))
    );
    assert_eq!(
        parse_numeric_str("000.456"),
        Ok((ValueType::Float, "000.456"))
    );
    assert_eq!(
        parse_numeric_str("0001e3"),
        Ok((ValueType::Float, "0001e3"))
    );
}

#[test]
fn test_parse_numeric_str_trailing_characters() {
    assert_eq!(parse_numeric_str("123abc"), Ok((ValueType::Integer, "123")));
    assert_eq!(
        parse_numeric_str("456.78xyz"),
        Ok((ValueType::Float, "456.78"))
    );
    assert_eq!(
        parse_numeric_str("1.23e4extra"),
        Ok((ValueType::Float, "1.23e4"))
    );
}

#[test]
fn test_module_name_basic() {
    let sql = "CREATE VIRTUAL TABLE x USING y;";
    assert_eq!(module_name_from_sql(sql).unwrap(), "y");
}

#[test]
fn test_module_name_with_args() {
    let sql = "CREATE VIRTUAL TABLE x USING modname('a', 'b');";
    assert_eq!(module_name_from_sql(sql).unwrap(), "modname");
}

#[test]
fn test_module_name_missing_using() {
    let sql = "CREATE VIRTUAL TABLE x (a, b);";
    assert!(module_name_from_sql(sql).is_err());
}

#[test]
fn test_module_name_no_semicolon() {
    let sql = "CREATE VIRTUAL TABLE x USING limbo(a, b)";
    assert_eq!(module_name_from_sql(sql).unwrap(), "limbo");
}

#[test]
fn test_module_name_no_semicolon_or_args() {
    let sql = "CREATE VIRTUAL TABLE x USING limbo";
    assert_eq!(module_name_from_sql(sql).unwrap(), "limbo");
}

#[test]
fn test_module_args_none() {
    let sql = "CREATE VIRTUAL TABLE x USING modname;";
    let args = module_args_from_sql(sql).unwrap();
    assert_eq!(args.len(), 0);
}

#[test]
fn test_module_args_basic() {
    let sql = "CREATE VIRTUAL TABLE x USING modname('arg1', 'arg2');";
    let args = module_args_from_sql(sql).unwrap();
    assert_eq!(args.len(), 2);
    assert_eq!("arg1", args[0].to_text().unwrap());
    assert_eq!("arg2", args[1].to_text().unwrap());
    for arg in args {
        unsafe { arg.__free_internal_type() }
    }
}

#[test]
fn test_module_args_with_escaped_quote() {
    let sql = "CREATE VIRTUAL TABLE x USING modname('a''b', 'c');";
    let args = module_args_from_sql(sql).unwrap();
    assert_eq!(args.len(), 2);
    assert_eq!(args[0].to_text().unwrap(), "a'b");
    assert_eq!(args[1].to_text().unwrap(), "c");
    for arg in args {
        unsafe { arg.__free_internal_type() }
    }
}

#[test]
fn test_module_args_unterminated_string() {
    let sql = "CREATE VIRTUAL TABLE x USING modname('arg1, 'arg2');";
    assert!(module_args_from_sql(sql).is_err());
}

#[test]
fn test_module_args_extra_garbage_after_quote() {
    let sql = "CREATE VIRTUAL TABLE x USING modname('arg1'x);";
    assert!(module_args_from_sql(sql).is_err());
}

#[test]
fn test_module_args_trailing_comma() {
    let sql = "CREATE VIRTUAL TABLE x USING modname('arg1',);";
    let args = module_args_from_sql(sql).unwrap();
    assert_eq!(args.len(), 1);
    assert_eq!("arg1", args[0].to_text().unwrap());
    for arg in args {
        unsafe { arg.__free_internal_type() }
    }
}

#[test]
fn test_parse_numeric_literal_hex() {
    assert_eq!(
        parse_numeric_literal("0x1234").unwrap(),
        Value::Integer(4660)
    );
    assert_eq!(
        parse_numeric_literal("0xFFFFFFFF").unwrap(),
        Value::Integer(4294967295)
    );
    assert_eq!(
        parse_numeric_literal("0x7FFFFFFF").unwrap(),
        Value::Integer(2147483647)
    );
    assert_eq!(
        parse_numeric_literal("0x7FFFFFFFFFFFFFFF").unwrap(),
        Value::Integer(9223372036854775807)
    );
    assert_eq!(
        parse_numeric_literal("0xFFFFFFFFFFFFFFFF").unwrap(),
        Value::Integer(-1)
    );
    assert_eq!(
        parse_numeric_literal("0x8000000000000000").unwrap(),
        Value::Integer(-9223372036854775808)
    );

    assert_eq!(
        parse_numeric_literal("-0x1234").unwrap(),
        Value::Integer(-4660)
    );
    // too big hex
    assert!(parse_numeric_literal("-0x8000000000000000").is_err());
}

#[test]
fn test_parse_numeric_literal_integer() {
    assert_eq!(parse_numeric_literal("123").unwrap(), Value::Integer(123));
    assert_eq!(
        parse_numeric_literal("9_223_372_036_854_775_807").unwrap(),
        Value::Integer(9223372036854775807)
    );
}

#[test]
fn test_parse_numeric_literal_float() {
    assert_eq!(
        parse_numeric_literal("123.456").unwrap(),
        Value::Float(123.456)
    );
    assert_eq!(parse_numeric_literal(".123").unwrap(), Value::Float(0.123));
    assert_eq!(
        parse_numeric_literal("1.23e10").unwrap(),
        Value::Float(1.23e10)
    );
    assert_eq!(parse_numeric_literal("1e-10").unwrap(), Value::Float(1e-10));
    assert_eq!(
        parse_numeric_literal("1.23E+10").unwrap(),
        Value::Float(1.23e10)
    );
    assert_eq!(parse_numeric_literal("1.1_1").unwrap(), Value::Float(1.11));

    // > i64::MAX, convert to float
    assert_eq!(
        parse_numeric_literal("9223372036854775808").unwrap(),
        Value::Float(9.223372036854775808e+18)
    );
    // < i64::MIN, convert to float
    assert_eq!(
        parse_numeric_literal("-9223372036854775809").unwrap(),
        Value::Float(-9.223372036854775809e+18)
    );
}
