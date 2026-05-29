//! Pure extraction of `related.<F>.<rest…>` cross-entity-read paths (#95).
//!
//! A `@require` rule may reference a single related entity through the reserved
//! root identifier `related`. The path `related.approval.state` parses as
//! `Select { operand: Select { operand: Ident("related"), field: "approval" },
//! field: "state" }`. This module provides a pure AST walker,
//! [`related_paths`], that finds every such path anywhere in an expression so
//! the DSL apply-time validator (#95 part B) and the runtime prefetch resolver
//! (#95 part C) can decide what to load and what to reject.
//!
//! The walker is purely syntactic: it does NOT load anything and does NOT touch
//! `CelValue`. The actual dereference happens outside the engine
//! ("prefetch-and-bind", mirroring the `now` binding), preserving engine purity.

use crate::ast::{Comprehension, Expr};

/// The reserved root identifier that introduces a cross-entity read.
pub const RELATED_ROOT: &str = "related";

/// One `related.<relation>.<trailing…>` path found in a rule expression.
///
/// `related.approval.state` yields `relation = "approval"`, `trailing =
/// ["state"]`. A deeper path `related.approval.owner.name` yields `relation =
/// "approval"`, `trailing = ["owner", "name"]` — the runtime resolver uses the
/// trailing length to detect a multi-hop traversal across a second relation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelatedPath {
    /// The relation field name immediately after `related` (the field `F` that
    /// must be a `Relation{One}` on the schema being written).
    pub relation: String,
    /// The trailing selects after the relation field, in source order. For
    /// `related.F.col` this is `["col"]`; an empty vec means the path was bare
    /// `related.F` with no column select.
    pub trailing: Vec<String>,
}

/// Extract every `related.<F>.<…>` path from `expr`, in pre-order.
///
/// Pure and total: walks the entire AST (binary/unary/ternary operands, call
/// targets and arguments, list/map entries, struct fields, index operands, and
/// comprehension sub-expressions) so a `related.*` reference is found wherever
/// it appears. A bare `related` identifier with no field select contributes no
/// path (there is no relation field to resolve).
pub fn related_paths(expr: &Expr) -> Vec<RelatedPath> {
    let mut out = Vec::new();
    walk(expr, &mut out);
    out
}

/// Recursively collect related paths from `expr` into `out`.
fn walk(expr: &Expr, out: &mut Vec<RelatedPath>) {
    // First, see if THIS node is the top of a `related.…` select chain. We only
    // record at the outermost select so `related.a.b.c` is one path, not three.
    if let Some(path) = as_related_path(expr) {
        out.push(path);
        // The chain's only sub-expression worth re-walking is the `related`
        // root itself (an Ident) and any index expressions embedded in it,
        // which `collect_chain` already rejects by bailing. Nothing further to
        // descend into for a pure dotted chain.
        return;
    }

    match expr {
        Expr::Literal(_) | Expr::Ident(_) => {}
        Expr::Select { operand, .. } => walk(operand, out),
        Expr::Index { operand, index, .. } => {
            walk(operand, out);
            walk(index, out);
        }
        Expr::Call { target, args, .. } => {
            if let Some(t) = target {
                walk(t, out);
            }
            for arg in args {
                walk(arg, out);
            }
        }
        Expr::List(items) => {
            for item in items {
                walk(&item.value, out);
            }
        }
        Expr::Map(entries) => {
            for entry in entries {
                walk(&entry.key, out);
                walk(&entry.value, out);
            }
        }
        Expr::Struct { fields, .. } => {
            for (_, value) in fields {
                walk(value, out);
            }
        }
        Expr::Unary { operand, .. } => walk(operand, out),
        Expr::Binary { lhs, rhs, .. } => {
            walk(lhs, out);
            walk(rhs, out);
        }
        Expr::Ternary { cond, then, els } => {
            walk(cond, out);
            walk(then, out);
            walk(els, out);
        }
        Expr::Comprehension(c) => walk_comprehension(c, out),
    }
}

/// Walk every sub-expression of a comprehension so a `related.*` reference
/// inside an iteration macro (`xs.exists(x, related.f.g == x)`) is still found.
fn walk_comprehension(c: &Comprehension, out: &mut Vec<RelatedPath>) {
    walk(&c.iter_range, out);
    walk(&c.accu_init, out);
    walk(&c.loop_condition, out);
    walk(&c.loop_step, out);
    walk(&c.result, out);
}

/// If `expr` is a pure dotted select chain rooted at `related` (i.e.
/// `related.F` or `related.F.g.h…`), return the [`RelatedPath`]. Returns `None`
/// for anything else, including a bare `related` ident or a chain whose root is
/// reached through an index/call rather than plain field selects.
fn as_related_path(expr: &Expr) -> Option<RelatedPath> {
    // Only a Select node can be the top of a `related.field` chain; a bare
    // `Ident("related")` has no field and so contributes no path.
    let Expr::Select { .. } = expr else {
        return None;
    };
    let mut fields_reversed = Vec::new();
    if !collect_chain(expr, &mut fields_reversed) {
        return None;
    }
    // `fields_reversed` holds the selected field names from outermost to the one
    // directly on `related`; reverse to source order.
    fields_reversed.reverse();
    // `collect_chain` only succeeds when the chain bottoms out at
    // `Ident("related")`, so there is always at least one field: the relation.
    let mut iter = fields_reversed.into_iter();
    let relation = iter.next()?;
    let trailing: Vec<String> = iter.collect();
    Some(RelatedPath { relation, trailing })
}

/// Walk a select chain from the outside in, pushing each field name. Returns
/// `true` only if the chain is composed solely of plain (non-`has`, non-index,
/// non-call) field selects bottoming out at `Ident("related")`.
fn collect_chain(expr: &Expr, fields_reversed: &mut Vec<String>) -> bool {
    match expr {
        Expr::Select {
            operand,
            field,
            test_only,
            ..
        } => {
            // A `has(related.f)` presence test is not a value dereference; treat
            // it as not-a-related-path so it falls through to ordinary walking.
            if *test_only {
                return false;
            }
            fields_reversed.push(field.clone());
            collect_chain(operand, fields_reversed)
        }
        Expr::Ident(name) => name == RELATED_ROOT,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse;

    fn paths(src: &str) -> Vec<RelatedPath> {
        related_paths(&parse(src).expect("test expression must parse"))
    }

    fn rp(relation: &str, trailing: &[&str]) -> RelatedPath {
        RelatedPath {
            relation: relation.to_string(),
            trailing: trailing.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn simple_related_col() {
        assert_eq!(
            paths("related.approval.state"),
            vec![rp("approval", &["state"])]
        );
    }

    #[test]
    fn bare_related_field_only() {
        // `related.approval` with no column select is still a path (relation,
        // empty trailing) — the resolver can detect a missing column later.
        assert_eq!(paths("related.approval == 'x'"), vec![rp("approval", &[])]);
    }

    #[test]
    fn bare_related_ident_yields_nothing() {
        // A lone `related` with no select has no relation field to resolve.
        assert!(paths("related == null").is_empty());
    }

    #[test]
    fn none_present() {
        assert!(paths("status != 'closed'").is_empty());
    }

    #[test]
    fn inside_disjunction() {
        let got = paths("status != 'closed' || related.approval.state == 'granted'");
        assert_eq!(got, vec![rp("approval", &["state"])]);
    }

    #[test]
    fn inside_function_arg() {
        let got = paths("size(related.approval.notes) > 0");
        assert_eq!(got, vec![rp("approval", &["notes"])]);
    }

    #[test]
    fn inside_ternary() {
        let got = paths("status == 'closed' ? related.approval.state == 'granted' : true");
        assert_eq!(got, vec![rp("approval", &["state"])]);
    }

    #[test]
    fn inside_comprehension() {
        let got = paths("tags.exists(t, t == related.owner.name)");
        assert_eq!(got, vec![rp("owner", &["name"])]);
    }

    #[test]
    fn multi_hop_trailing_captured_in_order() {
        // related.approval.owner.name → relation=approval, trailing=[owner, name].
        assert_eq!(
            paths("related.approval.owner.name == 'x'"),
            vec![rp("approval", &["owner", "name"])]
        );
    }

    #[test]
    fn multiple_distinct_paths() {
        let got = paths("related.approval.state == 'granted' && related.reviewer.active");
        assert_eq!(
            got,
            vec![rp("approval", &["state"]), rp("reviewer", &["active"])]
        );
    }

    #[test]
    fn has_test_on_related_yields_underlying_relation() {
        // `has(related.approval.state)` is a presence test on the column `state`
        // (the outermost select is `test_only`), so the column itself is not a
        // value dereference. But the inner `related.approval` IS a plain value
        // select, so the resolver is still told to load `approval` (relation,
        // empty trailing). This is the correct conservative behavior: the
        // related row is prefetched so `has(...)` can test for the column.
        assert_eq!(
            paths("has(related.approval.state)"),
            vec![rp("approval", &[])]
        );
    }

    #[test]
    fn index_into_related_is_not_a_dotted_path() {
        // `related.approval["state"]` reaches a column via index, not a plain
        // select; the inner `related.approval` is still a path.
        assert_eq!(
            paths("related.approval[\"state\"] == 1"),
            vec![rp("approval", &[])]
        );
    }
}
