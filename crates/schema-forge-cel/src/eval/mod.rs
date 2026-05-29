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

use crate::ast::{BinaryOp, Comprehension, Expr, ListEntry, Literal, MapEntry, UnaryOp};
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

    /// A child scope that binds three names (a two-variable comprehension's two
    /// iteration vars plus the accumulator) in one allocation.
    fn with3(
        &self,
        n1: &str,
        v1: CelValue,
        n2: &str,
        v2: CelValue,
        n3: &str,
        v3: CelValue,
    ) -> Self {
        let mut locals = self.locals.clone();
        locals.insert(n1.to_string(), v1);
        locals.insert(n2.to_string(), v2);
        locals.insert(n3.to_string(), v3);
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
        Expr::Index {
            operand,
            index,
            optional,
        } => eval_index(operand, index, *optional, scope, d),
        Expr::Select {
            operand,
            field,
            test_only,
            optional,
        } => eval_select(operand, field, *test_only, *optional, scope, d),
        Expr::List(items) => eval_list(items, scope, d),
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
            | "optional_type"
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

/// The outcome of looking a field up in a value: a present value, a definite
/// absence (no such key on a map), or `None` for "the operand type has no field
/// access at all" (a non-map, non-optional value).
enum FieldLookup {
    Present(CelValue),
    Absent,
    NoOverload,
}

/// Look up `field` (a string key) directly on a plain map value.
fn lookup_field(target: &CelValue, field: &str) -> FieldLookup {
    match target {
        CelValue::Map(m) => match m.get(&CelKey::String(field.to_string())) {
            Some(v) => FieldLookup::Present(v.clone()),
            None => FieldLookup::Absent,
        },
        _ => FieldLookup::NoOverload,
    }
}

fn eval_select(
    operand: &Expr,
    field: &str,
    test_only: bool,
    optional: bool,
    scope: &Scope,
    depth: usize,
) -> Result<CelValue, EvalError> {
    let target = eval_depth(operand, scope, depth)?;
    if test_only {
        return Ok(CelValue::Bool(presence_test(&target, field)));
    }
    // A select on an optional operand always propagates optionality (regardless of
    // the `.?` marker): `optional.of(m).c`, `optional.none().c`, `optional.of(m).?c`
    // all yield an optional.
    if let CelValue::Optional(inner) = &target {
        return select_through_optional(inner.as_deref(), field);
    }
    match (lookup_field(&target, field), optional) {
        (FieldLookup::Present(v), false) => Ok(v),
        (FieldLookup::Present(v), true) => Ok(CelValue::optional_of(v)),
        (FieldLookup::Absent, false) => Err(EvalError::new(format!("no such key: {field}"))),
        (FieldLookup::Absent, true) => Ok(CelValue::optional_none()),
        // Optional select on a non-map (e.g. `optional.none()` is handled above;
        // any other non-map) is absent; a plain select has no overload.
        (FieldLookup::NoOverload, true) => Ok(CelValue::optional_none()),
        (FieldLookup::NoOverload, false) => Err(EvalError::new("no such overload")),
    }
}

/// Select `field` through an optional operand, yielding an optional result.
///
/// `optional.none().field` → none (short-circuits, never inspecting a field);
/// `optional.of(map).field` → `of(map[field])` when present, `none()` when the
/// key is absent. Selecting a field on a present-but-non-map inner value (e.g.
/// `optional.of(0).field`) is a `"no such key"` error, matching cel-spec — only
/// `optional.none()` absorbs the missing field.
fn select_through_optional(inner: Option<&CelValue>, field: &str) -> Result<CelValue, EvalError> {
    match inner {
        None => Ok(CelValue::optional_none()),
        Some(v) => match lookup_field(v, field) {
            FieldLookup::Present(found) => Ok(CelValue::optional_of(found)),
            FieldLookup::Absent => Ok(CelValue::optional_none()),
            FieldLookup::NoOverload => Err(EvalError::new(format!("no such key: {field}"))),
        },
    }
}

/// `has(target.field)` presence test.
///
/// On a map: whether the key is present. On an optional: whether the inner value
/// has the field present (none → false). On any other type: absent.
fn presence_test(target: &CelValue, field: &str) -> bool {
    match target {
        CelValue::Map(m) => m.contains_key(&CelKey::String(field.to_string())),
        CelValue::Optional(inner) => match inner.as_deref() {
            Some(v) => matches!(lookup_field(v, field), FieldLookup::Present(_)),
            None => false,
        },
        _ => false,
    }
}

fn eval_index(
    operand: &Expr,
    index: &Expr,
    optional: bool,
    scope: &Scope,
    depth: usize,
) -> Result<CelValue, EvalError> {
    let coll = eval_depth(operand, scope, depth)?;
    let idx = eval_depth(index, scope, depth)?;
    // Indexing an optional operand propagates optionality.
    if let CelValue::Optional(inner) = &coll {
        return index_through_optional(inner.as_deref(), &idx);
    }
    if optional {
        // `m[?k]` / `l[?i]`: absent → none, present → of(value).
        return Ok(match ops::index_value(&coll, &idx) {
            Ok(v) => CelValue::optional_of(v),
            Err(_) => CelValue::optional_none(),
        });
    }
    ops::index_value(&coll, &idx)
}

/// Index through an optional operand, yielding an optional result.
///
/// `optional.none()[k]` → none. `optional.of(coll)[k]` → `of(coll[k])` when the
/// key/index is present, `none()` when it is absent. Indexing into a present
/// inner value that is neither a list nor a map has no overload and errors.
fn index_through_optional(inner: Option<&CelValue>, idx: &CelValue) -> Result<CelValue, EvalError> {
    match inner {
        None => Ok(CelValue::optional_none()),
        Some(v @ (CelValue::List(_) | CelValue::Map(_))) => Ok(match ops::index_value(v, idx) {
            Ok(found) => CelValue::optional_of(found),
            Err(_) => CelValue::optional_none(),
        }),
        Some(_) => Err(EvalError::new("no such overload")),
    }
}

fn eval_list(items: &[ListEntry], scope: &Scope, depth: usize) -> Result<CelValue, EvalError> {
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let v = eval_depth(&item.value, scope, depth)?;
        if item.optional {
            // An optional entry contributes its inner value only when present.
            match v {
                CelValue::Optional(Some(inner)) => out.push(*inner),
                CelValue::Optional(None) => {}
                _ => return Err(EvalError::new("no such overload")),
            }
        } else {
            out.push(v);
        }
    }
    Ok(CelValue::List(out))
}

fn eval_map(entries: &[MapEntry], scope: &Scope, depth: usize) -> Result<CelValue, EvalError> {
    let mut out = BTreeMap::new();
    for entry in entries {
        let val = eval_depth(&entry.value, scope, depth)?;
        // An optional entry is included only when its value optional is present.
        let val = if entry.optional {
            match val {
                CelValue::Optional(Some(inner)) => *inner,
                CelValue::Optional(None) => continue,
                _ => return Err(EvalError::new("no such overload")),
            }
        } else {
            val
        };
        let key_val = eval_depth(&entry.key, scope, depth)?;
        let key = ops::to_key(&key_val).ok_or_else(|| EvalError::new("no such overload"))?;
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
    // `optMap` / `optFlatMap` bind a variable to the optional's inner value, so
    // their body argument is evaluated lazily (only when the receiver has a value)
    // in an extended scope — they cannot go through the eager-args dispatch path.
    if let (Some(t), "optMap" | "optFlatMap", [var, body]) = (target, function, args) {
        return eval_opt_map(t, function, var, body, scope, depth);
    }

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

/// Evaluate `opt.optMap(var, body)` / `opt.optFlatMap(var, body)`.
///
/// Both require the receiver to be an optional. When it is absent the result is
/// `optional.none()` and `body` is never evaluated. When present, `var` is bound
/// to the inner value and `body` is evaluated: `optMap` wraps the result in
/// `optional.of(...)`; `optFlatMap`'s body already yields an optional and is
/// returned as-is.
fn eval_opt_map(
    target: &Expr,
    function: &str,
    var: &Expr,
    body: &Expr,
    scope: &Scope,
    depth: usize,
) -> Result<CelValue, EvalError> {
    let Expr::Ident(var_name) = var else {
        return Err(EvalError::new("no such overload"));
    };
    let recv = eval_depth(target, scope, depth)?;
    let CelValue::Optional(inner) = recv else {
        return Err(EvalError::new("no such overload"));
    };
    let Some(value) = inner else {
        return Ok(CelValue::optional_none());
    };
    let child = scope.with(var_name, *value);
    let result = eval_depth(body, &child, depth)?;
    if function == "optMap" {
        Ok(CelValue::optional_of(result))
    } else {
        // optFlatMap: the body must itself produce an optional.
        match result {
            CelValue::Optional(_) => Ok(result),
            _ => Err(EvalError::new("no such overload")),
        }
    }
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
    let elements = range_elements(range, c.iter_var2.is_some())?;

    let mut accu = eval_depth(&c.accu_init, scope, depth)?;
    let mut deferred: Option<EvalError> = None;
    let mut last_elem: Option<(CelValue, Option<CelValue>)> = None;

    for (v1, v2) in elements {
        let child = bind_iteration(c, scope, &v1, v2.as_ref(), accu.clone());
        last_elem = Some((v1, v2));

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
            Some((v1, v2)) => bind_iteration(c, scope, v1, v2.as_ref(), accu.clone()),
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

/// Build the per-element scope binding the iteration variable(s) and accumulator.
///
/// For a single-variable comprehension (`v2 == None`) only `iter_var` + `accu_var`
/// are bound (the historical behavior). For a two-variable comprehension both
/// `iter_var` (index/key) and `iter_var2` (element/value) are bound alongside the
/// accumulator.
fn bind_iteration<'a>(
    c: &Comprehension,
    scope: &Scope<'a>,
    v1: &CelValue,
    v2: Option<&CelValue>,
    accu: CelValue,
) -> Scope<'a> {
    match (&c.iter_var2, v2) {
        (Some(name2), Some(val2)) => scope.with3(
            &c.iter_var,
            v1.clone(),
            name2,
            val2.clone(),
            &c.accu_var,
            accu,
        ),
        _ => scope.with2(&c.iter_var, v1.clone(), &c.accu_var, accu),
    }
}

/// Materialize the iteration pairs a comprehension ranges over.
///
/// Single-variable (`two_var == false`): a list yields each element (no second
/// value); a map yields each key (per cel-spec, ranging a map ranges its keys).
///
/// Two-variable (`two_var == true`): a list yields `(int index, element)`; a map
/// yields `(key, value)`.
fn range_elements(
    range: CelValue,
    two_var: bool,
) -> Result<Vec<(CelValue, Option<CelValue>)>, EvalError> {
    match (range, two_var) {
        (CelValue::List(items), false) => Ok(items.into_iter().map(|e| (e, None)).collect()),
        (CelValue::List(items), true) => Ok(items
            .into_iter()
            .enumerate()
            .map(|(i, e)| (CelValue::Int(i as i64), Some(e)))
            .collect()),
        (CelValue::Map(m), false) => Ok(m.into_keys().map(|k| (key_to_value(k), None)).collect()),
        (CelValue::Map(m), true) => Ok(m
            .into_iter()
            .map(|(k, v)| (key_to_value(k), Some(v)))
            .collect()),
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
    fn two_var_all_exists_over_list_binds_index_and_value() {
        // i is the 0-based index, v the element.
        assert_eq!(
            run("[1, 2, 3].all(i, v, v > i)").unwrap(),
            CelValue::Bool(true)
        );
        assert_eq!(
            run("[1, 2, 3].exists(i, v, i == 1 && v == 2)").unwrap(),
            CelValue::Bool(true)
        );
        assert_eq!(
            run("[1, 2, 3].all(i, v, i == 0)").unwrap(),
            CelValue::Bool(false)
        );
    }

    #[test]
    fn two_var_macros_over_map_bind_key_and_value() {
        assert_eq!(
            run("{'key1':1, 'key2':2}.exists(k, v, k == 'key2' && v == 2)").unwrap(),
            CelValue::Bool(true)
        );
        assert_eq!(
            run("{'key1':1, 'key2':2}.all(k, v, k == 'key2' && v == 2)").unwrap(),
            CelValue::Bool(false)
        );
    }

    #[test]
    fn two_var_exists_one_over_list_and_map() {
        // Exactly one element with v % 5 == i (5%5==0==i at i=0).
        assert_eq!(
            run("[5, 7, 8].existsOne(i, v, v % 5 == i)").unwrap(),
            CelValue::Bool(true)
        );
        assert_eq!(
            run("[0, 1, 2, 3, 4].existsOne(i, v, v % 2 == i)").unwrap(),
            CelValue::Bool(false)
        );
        assert_eq!(
            run("{6: 'six', 7: 'seven', 8: 'eight'}.existsOne(k, v, k % 5 == 2 && v == 'seven')")
                .unwrap(),
            CelValue::Bool(true)
        );
    }

    #[test]
    fn two_var_all_error_shortcircuit_vs_exhaustive() {
        // v=2 at i=1 makes 6/(2-2) error, but i!=6/(2-1)=6 at i=0 yields false → all false.
        assert_eq!(
            run("[1, 2, 3].all(i, v, 6 / (2 - v) == i)").unwrap(),
            CelValue::Bool(false)
        );
        // No element makes the predicate determinately false → the error surfaces.
        assert_eq!(
            run("[1, 2, 3].all(i, v, v / i != 17)")
                .unwrap_err()
                .message(),
            "divide by zero"
        );
    }

    #[test]
    fn two_var_exists_error_surfaces() {
        assert_eq!(
            run("[1, 2, 3].exists(i, v, v / i == 17)")
                .unwrap_err()
                .message(),
            "divide by zero"
        );
    }

    #[test]
    fn transform_list_builds_list() {
        assert_eq!(
            run("[2, 4, 6].transformList(i, v, v / 2 + i)").unwrap(),
            CelValue::List(vec![CelValue::Int(1), CelValue::Int(3), CelValue::Int(5)])
        );
        // 4-arg filter drops i==1 and v==4.
        assert_eq!(
            run("[2, 4, 6].transformList(i, v, i != 1 && v != 4, v / 2 + i)").unwrap(),
            CelValue::List(vec![CelValue::Int(1), CelValue::Int(5)])
        );
        assert_eq!(
            run("[].transformList(i, v, i / v)").unwrap(),
            CelValue::List(Vec::new())
        );
    }

    #[test]
    fn transform_list_error_surfaces() {
        assert_eq!(
            run("[2, 1, 0].transformList(i, v, v / i)")
                .unwrap_err()
                .message(),
            "divide by zero"
        );
    }

    #[test]
    fn transform_map_keeps_keys_transforms_values() {
        let result = run("{'foo': 'bar'}.transformMap(k, v, k + v)").unwrap();
        let mut expected = BTreeMap::new();
        expected.insert(
            CelKey::String("foo".into()),
            CelValue::String("foobar".into()),
        );
        assert_eq!(result, CelValue::Map(expected));
    }

    #[test]
    fn transform_map_filter_drops_entries() {
        let result =
            run("{'foo': 'bar', 'baz': 'bux'}.transformMap(k, v, k != 'baz' && v != 'bux', k + v)")
                .unwrap();
        let mut expected = BTreeMap::new();
        expected.insert(
            CelKey::String("foo".into()),
            CelValue::String("foobar".into()),
        );
        assert_eq!(result, CelValue::Map(expected));
        // Empty map → empty map.
        assert_eq!(
            run("{}.transformMap(k, v, k + v)").unwrap(),
            CelValue::Map(BTreeMap::new())
        );
    }

    #[test]
    fn transform_map_error_surfaces() {
        assert_eq!(
            run("{'foo': 2, 'bar': 1, 'baz': 0}.transformMap(k, v, 4 / v)")
                .unwrap_err()
                .message(),
            "divide by zero"
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

    // -- Optional types (#100) --

    #[test]
    fn optional_constructors_and_predicates() {
        assert_eq!(
            run("optional.of(1).hasValue()").unwrap(),
            CelValue::Bool(true)
        );
        assert_eq!(
            run("optional.none().hasValue()").unwrap(),
            CelValue::Bool(false)
        );
        assert_eq!(run("optional.of(7).value()").unwrap(), CelValue::Int(7));
        // value() on an absent optional is an error.
        assert!(run("optional.none().value()").is_err());
    }

    #[test]
    fn optional_of_non_zero_value() {
        // Zero values yield none; non-zero yield of.
        assert_eq!(
            run("optional.ofNonZeroValue(0).hasValue()").unwrap(),
            CelValue::Bool(false)
        );
        assert_eq!(
            run("optional.ofNonZeroValue('').hasValue()").unwrap(),
            CelValue::Bool(false)
        );
        assert_eq!(
            run("optional.ofNonZeroValue('x').value()").unwrap(),
            CelValue::String("x".into())
        );
    }

    #[test]
    fn optional_or_value_and_or() {
        assert_eq!(
            run("optional.none().orValue(42)").unwrap(),
            CelValue::Int(42)
        );
        assert_eq!(run("optional.of(1).orValue(42)").unwrap(), CelValue::Int(1));
        // or() picks the first present optional.
        assert_eq!(
            run("optional.none().or(optional.of(5)).value()").unwrap(),
            CelValue::Int(5)
        );
    }

    #[test]
    fn optional_select_present_and_absent() {
        // Present field → of(value); absent field → none.
        assert_eq!(run("{'a': 1}.?a.value()").unwrap(), CelValue::Int(1));
        assert_eq!(
            run("{'a': 1}.?b.hasValue()").unwrap(),
            CelValue::Bool(false)
        );
        // Select through an optional propagates optionality.
        assert_eq!(
            run("optional.of({'a': 1}).a.value()").unwrap(),
            CelValue::Int(1)
        );
        assert_eq!(
            run("optional.of({}).a.hasValue()").unwrap(),
            CelValue::Bool(false)
        );
    }

    #[test]
    fn optional_index_present_and_absent() {
        assert_eq!(
            run("['foo'][?0].value()").unwrap(),
            CelValue::String("foo".into())
        );
        assert_eq!(run("[][?0].hasValue()").unwrap(), CelValue::Bool(false));
        assert_eq!(run("{'k': 9}[?'k'].value()").unwrap(), CelValue::Int(9));
        assert_eq!(run("{}[?'k'].hasValue()").unwrap(), CelValue::Bool(false));
    }

    #[test]
    fn optional_opt_map_and_flat_map() {
        // optMap wraps the body; runs only when present.
        assert_eq!(
            run("optional.of(42).optMap(y, y + 1).value()").unwrap(),
            CelValue::Int(43)
        );
        assert_eq!(
            run("optional.none().optMap(y, y + 1).hasValue()").unwrap(),
            CelValue::Bool(false)
        );
        // optFlatMap returns the body's own optional.
        assert_eq!(
            run("{'key': {'subkey': 'v'}}.?key.optFlatMap(k, k.?subkey).value()").unwrap(),
            CelValue::String("v".into())
        );
    }

    #[test]
    fn optional_list_and_map_entry_splicing() {
        // Optional list entries are spliced in only when present.
        assert_eq!(
            run("[?optional.of(42), ?optional.none(), 7]").unwrap(),
            CelValue::List(vec![CelValue::Int(42), CelValue::Int(7)])
        );
        // An optional map entry is omitted when its value is none.
        assert_eq!(
            run("{?'a': optional.none(), 'b': 1}").unwrap(),
            run("{'b': 1}").unwrap()
        );
        assert_eq!(
            run("{?'a': optional.of(9)}").unwrap(),
            run("{'a': 9}").unwrap()
        );
    }

    #[test]
    fn optional_equality_and_type() {
        assert_eq!(
            run("optional.none() == optional.none()").unwrap(),
            CelValue::Bool(true)
        );
        assert_eq!(
            run("optional.of(1) == optional.of(1)").unwrap(),
            CelValue::Bool(true)
        );
        assert_eq!(
            run("optional.of(1) == optional.none()").unwrap(),
            CelValue::Bool(false)
        );
        assert_eq!(
            run("type(optional.none()) == optional_type").unwrap(),
            CelValue::Bool(true)
        );
    }
}
