//! The typed CEL abstract syntax tree (#107) and an [`unparse`] for debugging.
//!
//! The parser produces these nodes; the evaluator (#108) consumes them and maps
//! AST [`Literal`]s onto [`crate::value::CelValue`]. The AST is independent of
//! `CelValue` — it carries only syntactic literal forms.
//!
//! ## Operator representation
//! Unlike cel-spec, which encodes operators as overload-named calls (`_+_`,
//! `_&&_`, `_?_:_`, …), this small engine uses dedicated [`Expr::Unary`],
//! [`Expr::Binary`], and [`Expr::Ternary`] nodes. This keeps evaluator dispatch
//! and unparsing simple and self-documenting. Named function and method calls use
//! [`Expr::Call`]. Macro-internal accumulator names (`@result`) DO follow
//! cel-spec exactly so the evaluator (#108) and conformance corpus align.

/// A syntactic literal value.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Literal {
    /// The `null` literal.
    Null,
    /// A boolean literal.
    Bool(bool),
    /// A signed integer literal.
    Int(i64),
    /// An unsigned integer literal.
    Uint(u64),
    /// A floating-point literal.
    Double(f64),
    /// A string literal (already escape-decoded).
    String(String),
    /// A bytes literal (already escape-decoded).
    Bytes(Vec<u8>),
}

/// A prefix unary operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum UnaryOp {
    /// Logical negation, `!`.
    Not,
    /// Arithmetic negation, `-`.
    Neg,
}

/// A binary operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BinaryOp {
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `*`
    Mul,
    /// `/`
    Div,
    /// `%`
    Rem,
    /// `==`
    Eq,
    /// `!=`
    Ne,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
    /// `in`
    In,
    /// `&&`
    And,
    /// `||`
    Or,
}

impl BinaryOp {
    /// The source spelling of this operator.
    pub fn symbol(self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Sub => "-",
            Self::Mul => "*",
            Self::Div => "/",
            Self::Rem => "%",
            Self::Eq => "==",
            Self::Ne => "!=",
            Self::Lt => "<",
            Self::Le => "<=",
            Self::Gt => ">",
            Self::Ge => ">=",
            Self::In => "in",
            Self::And => "&&",
            Self::Or => "||",
        }
    }
}

/// A typed CEL expression node.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Expr {
    /// A literal value.
    Literal(Literal),
    /// An identifier reference.
    Ident(String),
    /// Field selection (`operand.field`), an optional select (`operand.?field`,
    /// with `optional == true`), or a presence test (`has(operand.field)`, with
    /// `test_only == true`).
    Select {
        /// The value selected from.
        operand: Box<Expr>,
        /// The field name.
        field: String,
        /// Whether this is a `has()` presence test rather than a value select.
        test_only: bool,
        /// Whether this is an optional select (`a.?b`): yields `optional.of(v)`
        /// when the field is present and `optional.none()` when absent, rather
        /// than erroring.
        optional: bool,
    },
    /// Index access, `operand[index]`, or optional index `operand[?index]` (with
    /// `optional == true`).
    Index {
        /// The collection being indexed.
        operand: Box<Expr>,
        /// The index expression.
        index: Box<Expr>,
        /// Whether this is an optional index (`m[?k]`): yields `optional.none()`
        /// when the key/index is absent rather than erroring.
        optional: bool,
    },
    /// A function call (`function(args)`) or method call (`target.function(args)`).
    Call {
        /// The receiver for a method call, or `None` for a global function call.
        target: Option<Box<Expr>>,
        /// The function or method name.
        function: String,
        /// The argument expressions.
        args: Vec<Expr>,
    },
    /// A list construction, `[a, b, ...]`, whose entries may be optional
    /// (`[?optExpr]`): an optional entry is spliced in only when it has a value.
    List(Vec<ListEntry>),
    /// A map construction, `{k: v, ...}`, whose entries may be optional
    /// (`{?k: optExpr}`): an optional entry is included only when it has a value.
    Map(Vec<MapEntry>),
    /// A message/struct construction, `Type{field: v, ...}` (parsed
    /// syntactically; the evaluator does not build proto messages).
    Struct {
        /// The (possibly dotted) type name.
        type_name: String,
        /// The field initializers.
        fields: Vec<(String, Expr)>,
    },
    /// A prefix unary operation.
    Unary {
        /// The operator.
        op: UnaryOp,
        /// The operand.
        operand: Box<Expr>,
    },
    /// A binary operation.
    Binary {
        /// The operator.
        op: BinaryOp,
        /// The left-hand side.
        lhs: Box<Expr>,
        /// The right-hand side.
        rhs: Box<Expr>,
    },
    /// The ternary conditional, `cond ? then : els`.
    Ternary {
        /// The condition.
        cond: Box<Expr>,
        /// The value when the condition is true.
        then: Box<Expr>,
        /// The value when the condition is false.
        els: Box<Expr>,
    },
    /// A comprehension, the lowered form of the iteration macros (`all`, `exists`,
    /// `exists_one`, `map`, `filter`).
    Comprehension(Box<Comprehension>),
}

/// One entry of a list literal.
///
/// A plain entry (`optional == false`) contributes its value unconditionally; an
/// optional entry (`?expr`, `optional == true`) contributes its inner value only
/// when the optional has one and is omitted otherwise.
#[derive(Debug, Clone, PartialEq)]
pub struct ListEntry {
    /// The element expression.
    pub value: Expr,
    /// Whether this is an optional (`?`) entry.
    pub optional: bool,
}

impl ListEntry {
    /// A plain (non-optional) list entry.
    pub fn plain(value: Expr) -> Self {
        Self {
            value,
            optional: false,
        }
    }
}

/// One entry of a map literal.
///
/// A plain entry (`optional == false`) is always inserted; an optional entry
/// (`?k: expr`, `optional == true`) is inserted only when the value optional has
/// a value.
#[derive(Debug, Clone, PartialEq)]
pub struct MapEntry {
    /// The key expression.
    pub key: Expr,
    /// The value expression.
    pub value: Expr,
    /// Whether this is an optional (`?`) entry.
    pub optional: bool,
}

impl MapEntry {
    /// A plain (non-optional) map entry.
    pub fn plain(key: Expr, value: Expr) -> Self {
        Self {
            key,
            value,
            optional: false,
        }
    }
}

/// The lowered form of an iteration macro (cel-spec's comprehension model).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Comprehension {
    /// The loop variable name (the macro's first argument).
    ///
    /// For a two-variable macro this binds the index (lists) or key (maps); for a
    /// single-variable macro it binds the element (lists) or key (maps).
    pub iter_var: String,
    /// The second iteration variable, for the two-variable macros (`all(i, v, …)`,
    /// `exists(i, v, …)`, `existsOne`, `transformList`, `transformMap`).
    ///
    /// `None` selects single-variable behavior; `Some(name)` binds `name` to the
    /// list element / map value while `iter_var` binds the index / key.
    pub iter_var2: Option<String>,
    /// The range being iterated.
    pub iter_range: Expr,
    /// The accumulator variable name (always `@result`).
    pub accu_var: String,
    /// The initial accumulator value.
    pub accu_init: Expr,
    /// The loop continuation condition (encodes short-circuiting).
    pub loop_condition: Expr,
    /// The per-element accumulator update.
    pub loop_step: Expr,
    /// The final result expression.
    pub result: Expr,
}

/// Render an expression back to re-parseable CEL source.
///
/// The output is not necessarily byte-identical to the original — binary, unary,
/// and ternary nodes are fully parenthesized — but it re-parses to an equal AST
/// (`parse(unparse(parse(s))) == parse(s)`).
pub fn unparse(expr: &Expr) -> String {
    let mut out = String::new();
    write_expr(&mut out, expr);
    out
}

fn write_expr(out: &mut String, expr: &Expr) {
    match expr {
        Expr::Literal(lit) => write_literal(out, lit),
        Expr::Ident(name) => out.push_str(name),
        Expr::Select {
            operand,
            field,
            test_only,
            optional,
        } => write_select(out, operand, field, *test_only, *optional),
        Expr::Index {
            operand,
            index,
            optional,
        } => {
            write_parens(out, operand);
            out.push('[');
            if *optional {
                out.push('?');
            }
            write_expr(out, index);
            out.push(']');
        }
        Expr::Call {
            target,
            function,
            args,
        } => write_call(out, target.as_deref(), function, args),
        Expr::List(items) => write_list(out, items),
        Expr::Map(entries) => write_map(out, entries),
        Expr::Struct { type_name, fields } => write_struct(out, type_name, fields),
        Expr::Unary { op, operand } => {
            out.push(match op {
                UnaryOp::Not => '!',
                UnaryOp::Neg => '-',
            });
            write_parens(out, operand);
        }
        Expr::Binary { op, lhs, rhs } => {
            write_parens(out, lhs);
            out.push(' ');
            out.push_str(op.symbol());
            out.push(' ');
            write_parens(out, rhs);
        }
        Expr::Ternary { cond, then, els } => {
            write_parens(out, cond);
            out.push_str(" ? ");
            write_parens(out, then);
            out.push_str(" : ");
            write_parens(out, els);
        }
        Expr::Comprehension(c) => write_comprehension(out, c),
    }
}

/// Write a sub-expression wrapped in parentheses unless it is atomic. Keeps the
/// unparse unambiguous without over-parenthesizing simple leaves.
fn write_parens(out: &mut String, expr: &Expr) {
    if is_atomic(expr) {
        write_expr(out, expr);
    } else {
        out.push('(');
        write_expr(out, expr);
        out.push(')');
    }
}

fn is_atomic(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::Literal(_)
            | Expr::Ident(_)
            | Expr::List(_)
            | Expr::Map(_)
            | Expr::Struct { .. }
            | Expr::Call { target: None, .. }
            | Expr::Index { .. }
            | Expr::Select { .. }
    )
}

fn write_select(out: &mut String, operand: &Expr, field: &str, test_only: bool, optional: bool) {
    if test_only {
        out.push_str("has(");
        write_parens(out, operand);
        out.push('.');
        if optional {
            out.push('?');
        }
        out.push_str(field);
        out.push(')');
    } else {
        write_parens(out, operand);
        out.push('.');
        if optional {
            out.push('?');
        }
        out.push_str(field);
    }
}

fn write_call(out: &mut String, target: Option<&Expr>, function: &str, args: &[Expr]) {
    if let Some(t) = target {
        write_parens(out, t);
        out.push('.');
    }
    out.push_str(function);
    out.push('(');
    for (i, arg) in args.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        write_expr(out, arg);
    }
    out.push(')');
}

fn write_list(out: &mut String, items: &[ListEntry]) {
    out.push('[');
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        if item.optional {
            out.push('?');
        }
        write_expr(out, &item.value);
    }
    out.push(']');
}

fn write_map(out: &mut String, entries: &[MapEntry]) {
    out.push('{');
    for (i, entry) in entries.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        if entry.optional {
            out.push('?');
        }
        write_expr(out, &entry.key);
        out.push_str(": ");
        write_expr(out, &entry.value);
    }
    out.push('}');
}

fn write_struct(out: &mut String, type_name: &str, fields: &[(String, Expr)]) {
    out.push_str(type_name);
    out.push('{');
    for (i, (name, value)) in fields.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(name);
        out.push_str(": ");
        write_expr(out, value);
    }
    out.push('}');
}

fn write_literal(out: &mut String, lit: &Literal) {
    match lit {
        Literal::Null => out.push_str("null"),
        Literal::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Literal::Int(i) => out.push_str(&i.to_string()),
        Literal::Uint(u) => {
            out.push_str(&u.to_string());
            out.push('u');
        }
        // `{:?}` keeps a decimal point / exponent so the value re-parses as a
        // double rather than an int (e.g. `1.0`, not `1`).
        Literal::Double(d) => out.push_str(&format!("{d:?}")),
        Literal::String(s) => write_quoted_string(out, s),
        Literal::Bytes(b) => write_quoted_bytes(out, b),
    }
}

/// Write a string literal as a double-quoted, escaped form.
fn write_quoted_string(out: &mut String, s: &str) {
    out.push('"');
    for ch in s.chars() {
        push_escaped_char(out, ch);
    }
    out.push('"');
}

fn push_escaped_char(out: &mut String, ch: char) {
    match ch {
        '"' => out.push_str("\\\""),
        '\\' => out.push_str("\\\\"),
        '\n' => out.push_str("\\n"),
        '\r' => out.push_str("\\r"),
        '\t' => out.push_str("\\t"),
        c if (c as u32) < 0x20 || c as u32 == 0x7F => {
            out.push_str(&format!("\\u{:04x}", c as u32));
        }
        c => out.push(c),
    }
}

/// Write a bytes literal as `b"..."` with `\xHH` for any non-printable byte.
fn write_quoted_bytes(out: &mut String, b: &[u8]) {
    out.push_str("b\"");
    for &byte in b {
        match byte {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            0x20..=0x7E => out.push(char::from(byte)),
            _ => out.push_str(&format!("\\x{byte:02x}")),
        }
    }
    out.push('"');
}

/// Reconstruct an iteration macro's surface syntax from its lowered form.
///
/// The lowering in [`crate::parser`] is deterministic, so the predicate /
/// transform can be recovered from `loop_step` / `result` to produce a
/// re-parseable `range.macro(var, ...)` call.
fn write_comprehension(out: &mut String, c: &Comprehension) {
    let recovered = if c.iter_var2.is_some() {
        recover_macro_v2(c)
    } else {
        recover_macro(c)
    };
    if let Some((name, extra_args)) = recovered {
        write_parens(out, &c.iter_range);
        out.push('.');
        out.push_str(name);
        out.push('(');
        out.push_str(&c.iter_var);
        if let Some(v2) = &c.iter_var2 {
            out.push_str(", ");
            out.push_str(v2);
        }
        for arg in extra_args {
            out.push_str(", ");
            write_expr(out, &arg);
        }
        out.push(')');
    } else {
        // Unknown shape: emit a readable, clearly non-CEL marker. Standard macros
        // always recover, so this is only a debugging fallback.
        out.push_str("__comprehension__(");
        out.push_str(&c.iter_var);
        out.push_str(", ");
        write_expr(out, &c.iter_range);
        out.push(')');
    }
}

/// Recover the trailing arguments of a two-variable macro (`all`/`exists`/
/// `existsOne`/`transformList`/`transformMap`), inverting the lowerings in
/// [`crate::parser`]. The iteration variables are emitted by the caller; this
/// returns the camelCase macro name and the predicate / filter / transform args.
fn recover_macro_v2(c: &Comprehension) -> Option<(&'static str, Vec<Expr>)> {
    let accu = c.accu_var.as_str();
    match &c.accu_init {
        // all: init true, step `@result && p`.
        Expr::Literal(Literal::Bool(true)) => recover_and_pred(c, accu, "all"),
        // exists: init false, step `@result || p`.
        Expr::Literal(Literal::Bool(false)) => recover_or_pred(c, accu, "exists"),
        // existsOne: init 0, step `p ? @result + 1 : @result`.
        Expr::Literal(Literal::Int(0)) => {
            if let Expr::Ternary { cond, .. } = &c.loop_step {
                return Some(("existsOne", vec![(**cond).clone()]));
            }
            None
        }
        // transformList: init [].
        Expr::List(items) if items.is_empty() => recover_transform_list(c, accu),
        // transformMap: init {}.
        Expr::Map(entries) if entries.is_empty() => recover_transform_map(c, accu),
        // (Optional list/map literals are never produced as comprehension
        // accumulator inits, so they do not appear here.)
        _ => None,
    }
}

fn recover_and_pred(
    c: &Comprehension,
    accu: &str,
    name: &'static str,
) -> Option<(&'static str, Vec<Expr>)> {
    if let Expr::Binary {
        op: BinaryOp::And,
        lhs,
        rhs,
    } = &c.loop_step
    {
        if is_accu(lhs, accu) {
            return Some((name, vec![(**rhs).clone()]));
        }
    }
    None
}

fn recover_or_pred(
    c: &Comprehension,
    accu: &str,
    name: &'static str,
) -> Option<(&'static str, Vec<Expr>)> {
    if let Expr::Binary {
        op: BinaryOp::Or,
        lhs,
        rhs,
    } = &c.loop_step
    {
        if is_accu(lhs, accu) {
            return Some((name, vec![(**rhs).clone()]));
        }
    }
    None
}

/// transformList: 3-arg step `@result + [t]`; 4-arg step `f ? @result + [t] : @result`.
fn recover_transform_list(c: &Comprehension, accu: &str) -> Option<(&'static str, Vec<Expr>)> {
    match &c.loop_step {
        Expr::Binary {
            op: BinaryOp::Add,
            lhs,
            rhs,
        } if is_accu(lhs, accu) => single_list_item(rhs).map(|t| ("transformList", vec![t])),
        Expr::Ternary { cond, then, .. } => {
            if let Expr::Binary {
                op: BinaryOp::Add,
                lhs,
                rhs,
            } = then.as_ref()
            {
                if is_accu(lhs, accu) {
                    return single_list_item(rhs)
                        .map(|t| ("transformList", vec![(**cond).clone(), t]));
                }
            }
            None
        }
        _ => None,
    }
}

/// transformMap: 3-arg step `@mapInsert(@result, k, t)`; 4-arg step
/// `f ? @mapInsert(@result, k, t) : @result`.
fn recover_transform_map(c: &Comprehension, accu: &str) -> Option<(&'static str, Vec<Expr>)> {
    match &c.loop_step {
        Expr::Call { .. } => {
            map_insert_transform(&c.loop_step, accu).map(|t| ("transformMap", vec![t]))
        }
        Expr::Ternary { cond, then, .. } => {
            map_insert_transform(then, accu).map(|t| ("transformMap", vec![(**cond).clone(), t]))
        }
        _ => None,
    }
}

/// Extract the transform value from a `@mapInsert(@result, key, transform)` step.
fn map_insert_transform(step: &Expr, accu: &str) -> Option<Expr> {
    if let Expr::Call {
        target: None,
        function,
        args,
    } = step
    {
        if function == "@mapInsert" && args.len() == 3 && is_accu(&args[0], accu) {
            return Some(args[2].clone());
        }
    }
    None
}

/// The single element of a one-element plain list literal, if `expr` is `[t]`.
fn single_list_item(expr: &Expr) -> Option<Expr> {
    if let Expr::List(items) = expr {
        if let [entry] = items.as_slice() {
            if !entry.optional {
                return Some(entry.value.clone());
            }
        }
    }
    None
}

/// Recover `(macro_name, trailing_args)` from a lowered comprehension, inverting
/// the standard expansions in [`crate::parser`].
fn recover_macro(c: &Comprehension) -> Option<(&'static str, Vec<Expr>)> {
    let accu = c.accu_var.as_str();
    match &c.accu_init {
        // all: init true, step `@result && p`.
        Expr::Literal(Literal::Bool(true)) => {
            if let Expr::Binary {
                op: BinaryOp::And,
                lhs,
                rhs,
            } = &c.loop_step
            {
                if is_accu(lhs, accu) {
                    return Some(("all", vec![(**rhs).clone()]));
                }
            }
            None
        }
        // exists: init false, step `@result || p`.
        Expr::Literal(Literal::Bool(false)) => {
            if let Expr::Binary {
                op: BinaryOp::Or,
                lhs,
                rhs,
            } = &c.loop_step
            {
                if is_accu(lhs, accu) {
                    return Some(("exists", vec![(**rhs).clone()]));
                }
            }
            None
        }
        // exists_one: init 0, result `@result == 1`, step `p ? @result + 1 : @result`.
        Expr::Literal(Literal::Int(0)) => recover_exists_one(c),
        // map / filter: init [].
        Expr::List(items) if items.is_empty() => recover_list_macro(c, accu),
        _ => None,
    }
}

fn recover_exists_one(c: &Comprehension) -> Option<(&'static str, Vec<Expr>)> {
    if let Expr::Ternary { cond, .. } = &c.loop_step {
        return Some(("exists_one", vec![(**cond).clone()]));
    }
    None
}

fn recover_list_macro(c: &Comprehension, accu: &str) -> Option<(&'static str, Vec<Expr>)> {
    match &c.loop_step {
        // map(x, t): step `@result + [t]`.
        Expr::Binary {
            op: BinaryOp::Add,
            lhs,
            rhs,
        } if is_accu(lhs, accu) => single_list_item(rhs).map(|t| ("map", vec![t])),
        // filter(x, p) or map(x, p, t): step is a ternary.
        Expr::Ternary { cond, then, .. } => recover_ternary_list_macro(c, accu, cond, then),
        _ => None,
    }
}

fn recover_ternary_list_macro(
    c: &Comprehension,
    accu: &str,
    cond: &Expr,
    then: &Expr,
) -> Option<(&'static str, Vec<Expr>)> {
    if let Expr::Binary {
        op: BinaryOp::Add,
        lhs,
        rhs,
    } = then
    {
        if is_accu(lhs, accu) {
            if let Some(item) = single_list_item(rhs) {
                // filter appends the iter var itself; map(3-arg) appends a transform.
                if item == Expr::Ident(c.iter_var.clone()) {
                    return Some(("filter", vec![cond.clone()]));
                }
                return Some(("map", vec![cond.clone(), item]));
            }
        }
    }
    None
}

fn is_accu(expr: &Expr, accu: &str) -> bool {
    matches!(expr, Expr::Ident(name) if name == accu)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unparse_double_keeps_decimal_point() {
        let mut s = String::new();
        write_literal(&mut s, &Literal::Double(1.0));
        assert_eq!(s, "1.0");
    }

    #[test]
    fn unparse_uint_has_suffix() {
        let mut s = String::new();
        write_literal(&mut s, &Literal::Uint(7));
        assert_eq!(s, "7u");
    }

    #[test]
    fn unparse_binary_is_parenthesized() {
        let e = Expr::Binary {
            op: BinaryOp::Add,
            lhs: Box::new(Expr::Literal(Literal::Int(1))),
            rhs: Box::new(Expr::Binary {
                op: BinaryOp::Mul,
                lhs: Box::new(Expr::Literal(Literal::Int(2))),
                rhs: Box::new(Expr::Literal(Literal::Int(3))),
            }),
        };
        assert_eq!(unparse(&e), "1 + (2 * 3)");
    }

    #[test]
    fn unparse_bytes_escapes_nonprintable() {
        let e = Expr::Literal(Literal::Bytes(vec![0x00, b'a', 0xFF]));
        assert_eq!(unparse(&e), r#"b"\x00a\xff""#);
    }
}
