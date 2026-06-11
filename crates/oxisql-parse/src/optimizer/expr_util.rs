//! Shared expression utilities for optimizer passes.
//!
//! Provides:
//! - SQL fragment parsing helpers (`parse_predicate`, `parse_expr`)
//! - Expression rendering (`render`)
//! - Conjunct splitting and joining (`split_conjuncts`, `join_conjuncts`)
//! - Equi-join key extraction (`equi_key`)
//! - Column-reference collection (`collect_colrefs`)
//! - Canonical structural hashing (`canonical_hash`)
//! - Common-subexpression detection (`find_common_subexprs`)

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use sqlparser::ast::{BinaryOperator, Expr, SelectItem, SetExpr, Statement};
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;

// ── Types ─────────────────────────────────────────────────────────────────────

/// A qualified column reference extracted from an expression.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ColRef {
    /// The table qualifier, if the column was written as `table.column`.
    pub qualifier: Option<String>,
    /// The bare column name.
    pub name: String,
}

// ── Parsing helpers ───────────────────────────────────────────────────────────

/// Parse a SQL predicate string into an [`Expr`], returning `None` on failure.
///
/// The predicate is wrapped in `SELECT 1 WHERE <predicate>` so that the full
/// `sqlparser` WHERE-expression grammar is available.
pub fn parse_predicate(s: &str) -> Option<Expr> {
    let sql = format!("SELECT 1 WHERE {s}");
    let dialect = GenericDialect {};
    let stmts = Parser::parse_sql(&dialect, &sql).ok()?;
    if let Some(Statement::Query(q)) = stmts.into_iter().next() {
        if let SetExpr::Select(sel) = *q.body {
            return sel.selection;
        }
    }
    None
}

/// Parse a standalone SQL expression (not a WHERE predicate), returning `None`
/// on failure.
///
/// The expression is wrapped in `SELECT <expr>` so that the full projection
/// grammar is available, including function calls, casts, etc.
pub fn parse_expr(s: &str) -> Option<Expr> {
    let sql = format!("SELECT {s}");
    let dialect = GenericDialect {};
    let stmts = Parser::parse_sql(&dialect, &sql).ok()?;
    if let Some(Statement::Query(q)) = stmts.into_iter().next() {
        if let SetExpr::Select(sel) = *q.body {
            if let Some(SelectItem::UnnamedExpr(e)) = sel.projection.into_iter().next() {
                return Some(e);
            }
        }
    }
    None
}

// ── Rendering ────────────────────────────────────────────────────────────────

/// Render an [`Expr`] back to a SQL string via its `Display` implementation.
pub fn render(e: &Expr) -> String {
    e.to_string()
}

// ── Conjunct utilities ────────────────────────────────────────────────────────

/// Split a top-level AND expression into its conjuncts (leaves of the AND tree).
///
/// `a AND b AND c` → `[a, b, c]`.  Non-AND expressions are returned as a
/// single-element `Vec`.
pub fn split_conjuncts(e: Expr) -> Vec<Expr> {
    match e {
        Expr::BinaryOp {
            left,
            op: BinaryOperator::And,
            right,
        } => {
            let mut v = split_conjuncts(*left);
            v.extend(split_conjuncts(*right));
            v
        }
        other => vec![other],
    }
}

/// Join a list of expressions with AND.
///
/// Returns `None` when the list is empty, and `Some(expr)` when it has one
/// element, or `Some(a AND b AND … AND z)` for multiple elements.
pub fn join_conjuncts(exprs: Vec<Expr>) -> Option<Expr> {
    exprs.into_iter().reduce(|acc, e| Expr::BinaryOp {
        left: Box::new(acc),
        op: BinaryOperator::And,
        right: Box::new(e),
    })
}

// ── Equi-join key extraction ──────────────────────────────────────────────────

/// If `e` is an equi-join predicate `a.x = b.y` (or `x = y`), return the two
/// [`ColRef`]s.  Returns `None` for any other expression shape.
pub fn equi_key(e: &Expr) -> Option<(ColRef, ColRef)> {
    if let Expr::BinaryOp {
        left,
        op: BinaryOperator::Eq,
        right,
    } = e
    {
        let l = expr_to_colref(left)?;
        let r = expr_to_colref(right)?;
        Some((l, r))
    } else {
        None
    }
}

fn expr_to_colref(e: &Expr) -> Option<ColRef> {
    match e {
        Expr::Identifier(id) => Some(ColRef {
            qualifier: None,
            name: id.value.clone(),
        }),
        Expr::CompoundIdentifier(parts) if parts.len() == 2 => Some(ColRef {
            qualifier: Some(parts[0].value.clone()),
            name: parts[1].value.clone(),
        }),
        _ => None,
    }
}

// ── Column reference collection ───────────────────────────────────────────────

/// Collect all column references reachable from `e`.
///
/// Both bare identifiers (`col`) and qualified identifiers (`tbl.col`) are
/// included.  Aggregate arguments, subquery expressions, and other non-column
/// leaves are skipped.
pub fn collect_colrefs(e: &Expr) -> Vec<ColRef> {
    let mut refs = Vec::new();
    collect_colrefs_inner(e, &mut refs);
    refs
}

fn collect_colrefs_inner(e: &Expr, out: &mut Vec<ColRef>) {
    match e {
        Expr::Identifier(id) => out.push(ColRef {
            qualifier: None,
            name: id.value.clone(),
        }),
        Expr::CompoundIdentifier(parts) if parts.len() == 2 => out.push(ColRef {
            qualifier: Some(parts[0].value.clone()),
            name: parts[1].value.clone(),
        }),
        Expr::BinaryOp { left, right, .. } => {
            collect_colrefs_inner(left, out);
            collect_colrefs_inner(right, out);
        }
        Expr::UnaryOp { expr, .. } => collect_colrefs_inner(expr, out),
        Expr::IsNull(e) | Expr::IsNotNull(e) => collect_colrefs_inner(e, out),
        Expr::Between {
            expr, low, high, ..
        } => {
            collect_colrefs_inner(expr, out);
            collect_colrefs_inner(low, out);
            collect_colrefs_inner(high, out);
        }
        Expr::InList { expr, list, .. } => {
            collect_colrefs_inner(expr, out);
            for item in list {
                collect_colrefs_inner(item, out);
            }
        }
        Expr::Like { expr, pattern, .. } | Expr::ILike { expr, pattern, .. } => {
            collect_colrefs_inner(expr, out);
            collect_colrefs_inner(pattern, out);
        }
        _ => {}
    }
}

// ── Canonical hashing ─────────────────────────────────────────────────────────

/// Compute a canonical (order-normalised) structural hash of an [`Expr`].
///
/// Commutative binary operators (`AND`, `OR`, `+`, `*`) sort their children
/// by hash before combining, so `a AND b` and `b AND a` produce the same hash.
/// This is used to identify identical subexpressions regardless of operand order.
pub fn canonical_hash(e: &Expr) -> u64 {
    let mut h = DefaultHasher::new();
    hash_expr(e, &mut h);
    h.finish()
}

fn hash_expr(e: &Expr, h: &mut DefaultHasher) {
    use std::mem::discriminant;
    discriminant(e).hash(h);
    match e {
        Expr::Identifier(id) => id.value.hash(h),
        Expr::CompoundIdentifier(parts) => {
            for p in parts {
                p.value.hash(h);
            }
        }
        Expr::Value(v) => v.to_string().hash(h),
        Expr::BinaryOp { left, op, right } => {
            let is_commutative = matches!(
                op,
                BinaryOperator::And
                    | BinaryOperator::Or
                    | BinaryOperator::Plus
                    | BinaryOperator::Multiply
            );
            op.to_string().hash(h);
            if is_commutative {
                let lh = {
                    let mut tmp = DefaultHasher::new();
                    hash_expr(left, &mut tmp);
                    tmp.finish()
                };
                let rh = {
                    let mut tmp = DefaultHasher::new();
                    hash_expr(right, &mut tmp);
                    tmp.finish()
                };
                let (a, b) = if lh <= rh { (lh, rh) } else { (rh, lh) };
                a.hash(h);
                b.hash(h);
            } else {
                hash_expr(left, h);
                hash_expr(right, h);
            }
        }
        Expr::UnaryOp { op, expr } => {
            op.to_string().hash(h);
            hash_expr(expr, h);
        }
        other => other.to_string().hash(h),
    }
}

// ── Common subexpression detection ───────────────────────────────────────────

/// Find common (repeated) subexpressions in `e` by canonical hash.
///
/// Returns one entry per unique subexpression appearing more than once:
/// `(canonical_hash, rendered_sql_string, occurrence_count)`.
///
/// Trivial leaves (identifiers, column references, literal values) are not
/// reported — only structurally non-trivial subexpressions are candidates.
pub fn find_common_subexprs(e: &Expr) -> Vec<(u64, String, usize)> {
    use std::collections::HashMap;
    let mut counts: HashMap<u64, (String, usize)> = HashMap::new();
    count_subexprs(e, &mut counts);
    counts
        .into_iter()
        .filter(|(_, (_, cnt))| *cnt > 1)
        .map(|(hash, (s, cnt))| (hash, s, cnt))
        .collect()
}

fn count_subexprs(e: &Expr, counts: &mut std::collections::HashMap<u64, (String, usize)>) {
    // Only non-trivial subexpressions are candidates for CSE.
    let is_nontrivial = !matches!(
        e,
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) | Expr::Value(_)
    );
    if is_nontrivial {
        let h = canonical_hash(e);
        let entry = counts.entry(h).or_insert_with(|| (render(e), 0));
        entry.1 += 1;
    }
    // Recurse into children.
    match e {
        Expr::BinaryOp { left, right, .. } => {
            count_subexprs(left, counts);
            count_subexprs(right, counts);
        }
        Expr::UnaryOp { expr, .. } => count_subexprs(expr, counts),
        Expr::Between {
            expr, low, high, ..
        } => {
            count_subexprs(expr, counts);
            count_subexprs(low, counts);
            count_subexprs(high, counts);
        }
        Expr::InList { expr, list, .. } => {
            count_subexprs(expr, counts);
            for item in list {
                count_subexprs(item, counts);
            }
        }
        _ => {}
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_predicate_simple() {
        let e = parse_predicate("x > 1");
        assert!(e.is_some(), "should parse simple predicate");
    }

    #[test]
    fn test_parse_predicate_and() {
        let e = parse_predicate("x > 1 AND y < 10");
        assert!(e.is_some(), "should parse AND predicate");
    }

    #[test]
    fn test_parse_expr_column() {
        let e = parse_expr("a.b");
        assert!(e.is_some(), "should parse qualified identifier");
    }

    #[test]
    fn test_split_conjuncts_single() {
        let e = parse_predicate("x > 1").expect("parse");
        let parts = split_conjuncts(e);
        assert_eq!(parts.len(), 1);
    }

    #[test]
    fn test_split_conjuncts_and() {
        let e = parse_predicate("x > 1 AND y < 10 AND z = 5").expect("parse");
        let parts = split_conjuncts(e);
        assert_eq!(parts.len(), 3, "AND should yield 3 conjuncts");
    }

    #[test]
    fn test_join_conjuncts_empty_is_none() {
        assert!(join_conjuncts(vec![]).is_none());
    }

    #[test]
    fn test_join_conjuncts_single() {
        let e = parse_predicate("x > 1").expect("parse");
        let joined = join_conjuncts(vec![e.clone()]);
        assert!(joined.is_some());
    }

    #[test]
    fn test_equi_key_qualified() {
        let e = parse_predicate("a.id = b.id").expect("parse");
        let key = equi_key(&e);
        assert!(key.is_some(), "equi_key should find a.id = b.id");
        let (l, r) = key.expect("key");
        assert_eq!(l.qualifier.as_deref(), Some("a"));
        assert_eq!(l.name, "id");
        assert_eq!(r.qualifier.as_deref(), Some("b"));
    }

    #[test]
    fn test_equi_key_non_eq_returns_none() {
        let e = parse_predicate("x > 1").expect("parse");
        assert!(equi_key(&e).is_none(), "non-equality should return None");
    }

    #[test]
    fn test_collect_colrefs() {
        let e = parse_predicate("a.x = b.y AND z > 1").expect("parse");
        let refs = collect_colrefs(&e);
        assert!(refs.len() >= 2, "should collect at least a.x and b.y");
    }

    #[test]
    fn test_canonical_hash_commutative() {
        let e1 = parse_predicate("x = 1 AND y = 2").expect("parse");
        let e2 = parse_predicate("y = 2 AND x = 1").expect("parse");
        assert_eq!(
            canonical_hash(&e1),
            canonical_hash(&e2),
            "commutative AND should produce same hash"
        );
    }

    #[test]
    fn test_canonical_hash_non_commutative() {
        let e1 = parse_predicate("x > 1").expect("parse e1");
        let e2 = parse_predicate("1 > x").expect("parse e2");
        assert_ne!(
            canonical_hash(&e1),
            canonical_hash(&e2),
            "non-commutative > should produce different hashes"
        );
    }

    #[test]
    fn test_find_common_subexprs_no_repeat() {
        let e = parse_predicate("x > 1 AND y < 2").expect("parse");
        let common = find_common_subexprs(&e);
        assert!(
            common.is_empty(),
            "no repeated subexpressions in x>1 AND y<2"
        );
    }

    #[test]
    fn test_find_common_subexprs_with_repeat() {
        // (x + 1) > 0 AND (x + 1) < 10 — the subexpr `x + 1` appears twice.
        let e = parse_predicate("(x + 1) > 0 AND (x + 1) < 10").expect("parse");
        let common = find_common_subexprs(&e);
        assert!(!common.is_empty(), "should detect repeated subexpr `x + 1`");
    }

    #[test]
    fn test_render_roundtrip() {
        let e = parse_predicate("a.id = b.id").expect("parse");
        let s = render(&e);
        assert!(!s.is_empty(), "render should produce non-empty string");
        // Should round-trip (re-parseable)
        assert!(
            parse_predicate(&s).is_some(),
            "rendered string should be re-parseable"
        );
    }
}
