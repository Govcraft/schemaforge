//! The tree-walking CEL evaluator (#108).
//!
//! [`eval`] walks a parsed [`Expr`] against a [`Scope`] and produces a
//! [`CelValue`]. The evaluator is pure and guaranteed-terminating: it performs no
//! I/O, holds no ambient authority, iterates comprehensions over a materialized
//! finite range, and bounds its own recursion with a depth guard
//! ([`DEFAULT_MAX_DEPTH`]) so a deeply nested expression yields an [`EvalError`]
//! rather than overflowing the stack — a government-production DoS-hardening
//! requirement.
//!
//! Operator semantics (arithmetic, comparison, the CEL `==` operator, indexing,
//! membership) live as pure functions in [`ops`]; function-call dispatch lives in
//! [`funcs`]. This module owns control flow: scoping, short-circuiting logical
//! operators with commutative error absorption, the ternary, and comprehension
//! evaluation with `@not_strictly_false`-style error deferral.

pub mod funcs;
pub mod ops;

use std::collections::BTreeMap;

use crate::ast::{BinaryOp, Comprehension, Expr, Literal, UnaryOp};
use crate::error::EvalError;
use crate::value::{CelKey, CelValue};
use crate::Bindings;

/// Maximum recursive evaluation depth before an [`EvalError`] is returned.
///
/// Bounds stack use for adversarial / pathological inputs. 250 comfortably
/// exceeds any realistic policy expression while staying far below the native
/// stack limit.
pub const DEFAULT_MAX_DEPTH: usize = 250;

/// A lexical scope: the caller's [`Bindings`] plus an overlay of locals bound by
/// comprehensions (`iter_var`, `@result`). Lookups consult the overlay first.
///
/// Cloning a `Scope` clones only the (small) local overlay; the base bindings are
/// borrowed. This keeps the evaluator allocation-light and free of interior
/// mutability.
#[derive(Clone)]
pub struct Scope<'a> {
    base: &'a Bindings,
    locals: BTreeMap<String, CelValue>,
}

impl<'a> Scope<'a> {
    /// A root scope over the supplied bindings, with no locals.
    pub fn root(base: &'a Bindings) -> Self {
        Self {
            base,
            locals: BTreeMap::new(),
        }
    }

    /// A child scope that additionally binds `name` to `value`.
    fn with(&self, name: &str, value: CelValue) -> Self {
        let mut locals = self.locals.clone();
        locals.insert(name.to_string(), value);
        Self {
            base: self.base,
            locals,
        }
    }

    /// A child scope that binds two names (the comprehension iter var and
    /// accumulator) in one allocation.
    fn with2(&self, n1: &str, v1: CelValue, n2: &str, v2: CelValue) -> Self {
        let mut locals = self.locals.clone();
        locals.insert(n1.to_string(), v1);
        locals.insert(n2.to_string(), v2);
        Self {
            base: self.base,
            locals,
        }
    }

    fn lookup(&self, name: &str) -> Option<&CelValue> {
        self.locals.get(name).or_else(|| self.base.get(name))
    }
}

/// Evaluate `expr` against `scope`, producing a [`CelValue`] or an [`EvalError`].
pub fn eval(expr: &Expr, scope: &Scope) -> Result<CelValue, EvalError> {
    eval_depth(expr, scope, 0)
}

fn eval_depth(expr: &Expr, scope: &Scope, depth: usize) -> Result<CelValue, EvalError> {
    if depth >= DEFAULT_MAX_DEPTH {
        return Err(EvalError::new("recursion limit exceeded"));
    }
    let d = depth + 1;
    match expr {
        Expr::Literal(lit) => Ok(literal_to_value(lit)),
        Expr::Ident(name) => scope
            .lookup(name)
            .cloned()
            .or_else(|| type_denotation(name))
            .ok_or_else(|| EvalError::new(format!("undeclared reference to '{name}'"))),
        Expr::Unary { op, operand } => eval_unary(*op, operand, scope, d),
        Expr::Binary { op, lhs, rhs } => eval_binary(*op, lhs, rhs, scope, d),
        Expr::Ternary { cond, then, els } => eval_ternary(cond, then, els, scope, d),
        Expr::Index { operand, index } => {
            let coll = eval_depth(operand, scope, d)?;
            let idx = eval_depth(index, scope, d)?;
            ops::index_value(&coll, &idx)
        }
        Expr::Select {
            operand,
            field,
            test_only,
        } => eval_select(operand, field, *test_only, scope, d),
        Expr::List(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(eval_depth(item, scope, d)?);
            }
            Ok(CelValue::List(out))
        }
        Expr::Map(entries) => eval_map(entries, scope, d),
        Expr::Struct { .. } => Err(EvalError::new("no such overload")),
        Expr::Call {
            target,
            function,
            args,
        } => eval_call(target.as_deref(), function, args, scope, d),
        Expr::Comprehension(c) => eval_comprehension(c, scope, d),
    }
}

/// The CEL standard environment denotes the built-in type names as `type`
/// values: bare `int` evaluates to `type(int)`, `string` to `type(string)`, and
/// so on. This is a stdlib environment binding (the type identifiers are part of
/// the standard library), resolved only as a fallback after a real binding lookup
/// misses, so a user binding of the same name still wins.
///
/// `dyn` is deliberately excluded: it is a pseudo-function with no denotation
/// (the corpus's `dyn_no_denotation` expects an unknown-variable error).
fn type_denotation(name: &str) -> Option<CelValue> {
    matches!(
        name,
        "bool"
            | "int"
            | "uint"
            | "double"
            | "string"
            | "bytes"
            | "list"
            | "map"
            | "null_type"
            | "type"
    )
    .then(|| CelValue::Type(name.to_string()))
}

fn literal_to_value(lit: &Literal) -> CelValue {
    match lit {
        Literal::Null => CelValue::Null,
        Literal::Bool(b) => CelValue::Bool(*b),
        Literal::Int(i) => CelValue::Int(*i),
        Literal::Uint(u) => CelValue::Uint(*u),
        Literal::Double(d) => CelValue::Double(*d),
        Literal::String(s) => CelValue::String(s.clone()),
        Literal::Bytes(b) => CelValue::Bytes(b.clone()),
    }
}

fn eval_unary(
    op: UnaryOp,
    operand: &Expr,
    scope: &Scope,
    depth: usize,
) -> Result<CelValue, EvalError> {
    let v = eval_depth(operand, scope, depth)?;
    ops::unary(op, &v)
}

fn eval_binary(
    op: BinaryOp,
    lhs: &Expr,
    rhs: &Expr,
    scope: &Scope,
    depth: usize,
) -> Result<CelValue, EvalError> {
    match op {
        BinaryOp::And => eval_and(lhs, rhs, scope, depth),
        BinaryOp::Or => eval_or(lhs, rhs, scope, depth),
        BinaryOp::Eq | BinaryOp::Ne => {
            let a = eval_depth(lhs, scope, depth)?;
            let b = eval_depth(rhs, scope, depth)?;
            let eq = ops::cel_equals(&a, &b)?;
            Ok(CelValue::Bool(if op == BinaryOp::Eq { eq } else { !eq }))
        }
        BinaryOp::In => {
            let a = eval_depth(lhs, scope, depth)?;
            let b = eval_depth(rhs, scope, depth)?;
            ops::membership(&a, &b)
        }
        BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => {
            let a = eval_depth(lhs, scope, depth)?;
            let b = eval_depth(rhs, scope, depth)?;
            ops::compare(op, &a, &b)
        }
        BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Rem => {
            let a = eval_depth(lhs, scope, depth)?;
            let b = eval_depth(rhs, scope, depth)?;
            ops::arithmetic(op, &a, &b)
        }
    }
}

/// `&&` with CEL's commutative error/short-circuit absorption: a definite `false`
/// on either side yields `false` regardless of the other side (absorbing its
/// error or type mismatch); otherwise a non-`false` side that is an error or a
/// non-bool surfaces the appropriate failure.
fn eval_and(lhs: &Expr, rhs: &Expr, scope: &Scope, depth: usize) -> Result<CelValue, EvalError> {
    let l = eval_depth(lhs, scope, depth);
    if matches!(l, Ok(CelValue::Bool(false))) {
        return Ok(CelValue::Bool(false));
    }
    let r = eval_depth(rhs, scope, depth);
    if matches!(r, Ok(CelValue::Bool(false))) {
        return Ok(CelValue::Bool(false));
    }
    combine_logical(l, r, true)
}

/// `||` with CEL's commutative error/short-circuit absorption: a definite `true`
/// on either side yields `true` regardless of the other side.
fn eval_or(lhs: &Expr, rhs: &Expr, scope: &Scope, depth: usize) -> Result<CelValue, EvalError> {
    let l = eval_depth(lhs, scope, depth);
    if matches!(l, Ok(CelValue::Bool(true))) {
        return Ok(CelValue::Bool(true));
    }
    let r = eval_depth(rhs, scope, depth);
    if matches!(r, Ok(CelValue::Bool(true))) {
        return Ok(CelValue::Bool(true));
    }
    combine_logical(l, r, false)
}

/// Resolve a logical operator once the short-circuit value is known to be absent.
/// `identity` is the result when both sides are the operator's identity bool
/// (`true` for `&&`, `false` for `||`). Errors propagate; a non-bool operand on a
/// non-short-circuiting side is a `"no matching overload"`.
fn combine_logical(
    l: Result<CelValue, EvalError>,
    r: Result<CelValue, EvalError>,
    identity: bool,
) -> Result<CelValue, EvalError> {
    match (l, r) {
        (Ok(CelValue::Bool(_)), Ok(CelValue::Bool(_))) => Ok(CelValue::Bool(identity)),
        // A surviving error (the other side did not short-circuit) propagates.
        (Err(e), _) | (_, Err(e)) => Err(e),
        // A surviving non-bool operand has no logical overload.
        _ => Err(EvalError::new("no matching overload")),
    }
}

/// `cond ? then : els`. The condition must be a bool (else `"no matching
/// overload"`); only the taken branch is evaluated.
fn eval_ternary(
    cond: &Expr,
    then: &Expr,
    els: &Expr,
    scope: &Scope,
    depth: usize,
) -> Result<CelValue, EvalError> {
    match eval_depth(cond, scope, depth)? {
        CelValue::Bool(true) => eval_depth(then, scope, depth),
        CelValue::Bool(false) => eval_depth(els, scope, depth),
        _ => Err(EvalError::new("no matching overload")),
    }
}

fn eval_select(
    operand: &Expr,
    field: &str,
    test_only: bool,
    scope: &Scope,
    depth: usize,
) -> Result<CelValue, EvalError> {
    let target = eval_depth(operand, scope, depth)?;
    let key = CelKey::String(field.to_string());
    match (&target, test_only) {
        // has(map.field): presence test, never errors on absence.
        (CelValue::Map(m), true) => Ok(CelValue::Bool(m.contains_key(&key))),
        (CelValue::Map(m), false) => m
            .get(&key)
            .cloned()
            .ok_or_else(|| EvalError::new(format!("no such key: {field}"))),
        // Presence test on a non-map: per spec, absent.
        (_, true) => Ok(CelValue::Bool(false)),
        (_, false) => Err(EvalError::new("no such overload")),
    }
}

fn eval_map(entries: &[(Expr, Expr)], scope: &Scope, depth: usize) -> Result<CelValue, EvalError> {
    let mut out = BTreeMap::new();
    for (k, v) in entries {
        let key_val = eval_depth(k, scope, depth)?;
        let key = ops::to_key(&key_val).ok_or_else(|| EvalError::new("no such overload"))?;
        let val = eval_depth(v, scope, depth)?;
        if out.insert(key, val).is_some() {
            return Err(EvalError::new("Failed with repeated key"));
        }
    }
    Ok(CelValue::Map(out))
}

fn eval_call(
    target: Option<&Expr>,
    function: &str,
    args: &[Expr],
    scope: &Scope,
    depth: usize,
) -> Result<CelValue, EvalError> {
    let recv = match target {
        Some(t) => Some(eval_depth(t, scope, depth)?),
        None => None,
    };
    let mut arg_vals = Vec::with_capacity(args.len());
    for arg in args {
        arg_vals.push(eval_depth(arg, scope, depth)?);
    }
    funcs::dispatch(recv.as_ref(), function, &arg_vals)
}

/// Evaluate a comprehension (the lowered form of `all`/`exists`/`exists_one`/
/// `map`/`filter`).
///
/// ## Error absorption (`@not_strictly_false`)
/// The parser lowers `all`/`exists` with a bare `@result` / `!@result`
/// loop_condition. CEL semantics require that a predicate error on one element
/// not abort the whole comprehension when a later element already determines the
/// result. We implement that here by *deferring* the first per-element error and
/// continuing; after the loop the deferred error is discarded only if the final
/// accumulator is already determinate — i.e. evaluating the loop_condition on it
/// yields `Ok(false)` (the comprehension would have stopped). Otherwise the
/// deferred error surfaces. For `map`/`filter`/`exists_one` the loop_condition is
/// the constant `true`, so it is never determinate and any deferred error always
/// surfaces — exactly matching the corpus.
fn eval_comprehension(
    c: &Comprehension,
    scope: &Scope,
    depth: usize,
) -> Result<CelValue, EvalError> {
    let range = eval_depth(&c.iter_range, scope, depth)?;
    let elements = range_elements(range)?;

    let mut accu = eval_depth(&c.accu_init, scope, depth)?;
    let mut deferred: Option<EvalError> = None;
    let mut last_elem: Option<CelValue> = None;

    for el in elements {
        let child = scope.with2(&c.iter_var, el.clone(), &c.accu_var, accu.clone());
        last_elem = Some(el);

        match eval_depth(&c.loop_condition, &child, depth) {
            Ok(CelValue::Bool(false)) => break, // determinate: stop iterating
            Ok(_) => {}                         // keep going
            Err(e) => defer(&mut deferred, e),  // absorb, keep going
        }

        match eval_depth(&c.loop_step, &child, depth) {
            Ok(v) => accu = v,
            Err(e) => defer(&mut deferred, e), // absorb; accumulator unchanged
        }
    }

    if let Some(err) = deferred {
        // The deferred error is discarded only if the final accumulator is
        // already determinate (the loop_condition would have stopped on it).
        let probe = match &last_elem {
            Some(el) => scope.with2(&c.iter_var, el.clone(), &c.accu_var, accu.clone()),
            None => scope.with(&c.accu_var, accu.clone()),
        };
        match eval_depth(&c.loop_condition, &probe, depth) {
            Ok(CelValue::Bool(false)) => {} // determinate → discard error
            _ => return Err(err),           // undetermined → surface error
        }
    }

    let result_scope = scope.with(&c.accu_var, accu);
    eval_depth(&c.result, &result_scope, depth)
}

/// Materialize the elements a comprehension iterates: list elements directly, or
/// a map's keys (per cel-spec, ranging a map ranges over its keys).
fn range_elements(range: CelValue) -> Result<Vec<CelValue>, EvalError> {
    match range {
        CelValue::List(items) => Ok(items),
        CelValue::Map(m) => Ok(m.into_keys().map(key_to_value).collect()),
        _ => Err(EvalError::new("no such overload")),
    }
}

fn key_to_value(k: CelKey) -> CelValue {
    match k {
        CelKey::Bool(b) => CelValue::Bool(b),
        CelKey::Int(i) => CelValue::Int(i),
        CelKey::Uint(u) => CelValue::Uint(u),
        CelKey::String(s) => CelValue::String(s),
    }
}

fn defer(slot: &mut Option<EvalError>, err: EvalError) {
    if slot.is_none() {
        *slot = Some(err);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse;

    fn run(src: &str) -> Result<CelValue, EvalError> {
        let expr = parse(src).expect("parse");
        let bindings = Bindings::new();
        eval(&expr, &Scope::root(&bindings))
    }

    fn run_with(src: &str, bindings: &Bindings) -> Result<CelValue, EvalError> {
        let expr = parse(src).expect("parse");
        eval(&expr, &Scope::root(bindings))
    }

    #[test]
    fn literals_and_arithmetic() {
        assert_eq!(run("1 + 1").unwrap(), CelValue::Int(2));
        assert_eq!(run("7u").unwrap(), CelValue::Uint(7));
        assert_eq!(run("2.5 * 2.0").unwrap(), CelValue::Double(5.0));
        assert_eq!(run(r#""a" + "b""#).unwrap(), CelValue::String("ab".into()));
    }

    #[test]
    fn ident_lookup_and_unbound() {
        let mut b = Bindings::new();
        b.insert("x".into(), CelValue::Int(41));
        assert_eq!(run_with("x + 1", &b).unwrap(), CelValue::Int(42));
        assert!(run("missing").is_err());
    }

    #[test]
    fn logical_absorption_truth_table() {
        // false absorbs a type error / error on the other side.
        assert_eq!(run("false && 32").unwrap(), CelValue::Bool(false));
        assert_eq!(run("'horses' && false").unwrap(), CelValue::Bool(false));
        assert_eq!(
            run("false && (2 / 0 > 3 ? false : true)").unwrap(),
            CelValue::Bool(false)
        );
        // true absorbs on ||.
        assert_eq!(run("true || 1/0 != 0").unwrap(), CelValue::Bool(true));
        // Both non-bool, no short-circuit → no matching overload.
        assert_eq!(
            run("'a' && 'b'").unwrap_err().message(),
            "no matching overload"
        );
    }

    #[test]
    fn ternary_only_taken_branch_and_bad_cond() {
        assert_eq!(run("true ? 1 : 1/0").unwrap(), CelValue::Int(1));
        assert_eq!(
            run("'cows' ? false : 17").unwrap_err().message(),
            "no matching overload"
        );
    }

    #[test]
    fn index_and_membership() {
        assert_eq!(run("[10, 20, 30][1]").unwrap(), CelValue::Int(20));
        assert_eq!(
            run("[10, 20, 30][5]").unwrap_err().message(),
            "invalid_argument"
        );
        assert_eq!(run("2 in [1, 2, 3]").unwrap(), CelValue::Bool(true));
        assert_eq!(run(r#"{"a": 1}["a"]"#).unwrap(), CelValue::Int(1));
    }

    #[test]
    fn has_presence_test() {
        assert_eq!(run(r#"has({"a": 1}.a)"#).unwrap(), CelValue::Bool(true));
        assert_eq!(run(r#"has({"a": 1}.b)"#).unwrap(), CelValue::Bool(false));
    }

    #[test]
    fn map_duplicate_key_errors() {
        assert!(run("{1: 1, 1: 2}")
            .unwrap_err()
            .message()
            .contains("repeated key"));
    }

    #[test]
    fn struct_is_unsupported() {
        let expr = parse("Foo{bar: 1}").expect("parse");
        let b = Bindings::new();
        assert_eq!(
            eval(&expr, &Scope::root(&b)).unwrap_err().message(),
            "no such overload"
        );
    }

    #[test]
    fn comprehension_all_error_shortcircuit() {
        // e=2 → 6/0 errors, but e=3 makes the predicate false → all is false.
        assert_eq!(
            run("[1, 2, 3].all(e, 6 / (2 - e) == 6)").unwrap(),
            CelValue::Bool(false)
        );
    }

    #[test]
    fn comprehension_all_error_exhaustive_surfaces() {
        assert_eq!(
            run("[1, 2, 3].all(e, e / 0 != 17)").unwrap_err().message(),
            "divide by zero"
        );
    }

    #[test]
    fn comprehension_exists_error_exhaustive_surfaces() {
        assert_eq!(
            run("[1, 2, 3].exists(e, e / 0 == 17)")
                .unwrap_err()
                .message(),
            "divide by zero"
        );
    }

    #[test]
    fn comprehension_basic_macros() {
        assert_eq!(
            run("[1, 2, 3].all(e, e > 0)").unwrap(),
            CelValue::Bool(true)
        );
        assert_eq!(
            run("[1, 2, 3].exists(e, e == 2)").unwrap(),
            CelValue::Bool(true)
        );
        assert_eq!(run("[].all(e, e > 0)").unwrap(), CelValue::Bool(true));
        assert_eq!(run("[].exists(e, e == 2)").unwrap(), CelValue::Bool(false));
        assert_eq!(
            run("[1, 2, 3].map(e, e + 1)").unwrap(),
            CelValue::List(vec![CelValue::Int(2), CelValue::Int(3), CelValue::Int(4)])
        );
        assert_eq!(
            run("[1, 2, 3].filter(e, e > 1)").unwrap(),
            CelValue::List(vec![CelValue::Int(2), CelValue::Int(3)])
        );
    }

    #[test]
    fn comprehension_over_map_ranges_keys() {
        assert_eq!(
            run(r#"{"key1": 1, "key2": 2}.exists(k, k == "key2")"#).unwrap(),
            CelValue::Bool(true)
        );
    }

    #[test]
    fn depth_guard_fires() {
        // Build a deeply left-nested addition exceeding the depth guard.
        let mut src = String::from("1");
        for _ in 0..(DEFAULT_MAX_DEPTH + 10) {
            src.push_str(" + 1");
        }
        let expr = parse(&src).expect("parse");
        let b = Bindings::new();
        let err = eval(&expr, &Scope::root(&b)).unwrap_err();
        assert!(err.message().contains("recursion limit"));
    }

    #[test]
    fn size_via_dispatch() {
        assert_eq!(run("size([1, 2, 3])").unwrap(), CelValue::Int(3));
        assert_eq!(run("dyn(1) == 1u").unwrap(), CelValue::Bool(true));
    }
}
