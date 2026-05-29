//! Static, conservative type-checking of CEL rule expressions against schema
//! field types (#104).
//!
//! The capability layer (`@require`/`@compute`/`@default`) carries raw CEL
//! expression strings. Because the engine is pure and its value domain maps onto
//! SchemaForge's `DynamicValue`, a rule expression can be type-checked against the
//! schema's field types at parse/apply time — so a bad rule fails before deploy
//! instead of on a live request.
//!
//! ## Guiding principle: false-positive-averse
//! The checker is deliberately CONSERVATIVE. [`infer`] is a TOTAL function that
//! returns [`InferredType::Dyn`] ("statically unknown") whenever it cannot prove a
//! concrete type; `Dyn` is treated as compatible with everything. The checker only
//! errors when a type is DEFINITELY known and DEFINITELY wrong. It must never
//! reject a rule that would actually succeed at runtime.
//!
//! ## Runtime fidelity
//! [`field_accepts`] mirrors the runtime value coercions in
//! `schema-forge-acton`'s `rules.rs` (`coerce_to_field_type`) and
//! [`crate::value::bridge::cel_to_dynamic`]: e.g. a `Float` field accepts an `Int`
//! result, a `DateTime` field accepts a `String` (parsed as RFC 3339), and an
//! `Enum` field accepts a `String`. Anything looser than runtime acceptance would
//! produce false positives.

use std::collections::BTreeMap;

use schema_forge_core::types::FieldType;

use crate::ast::{BinaryOp, Comprehension, Expr, Literal, UnaryOp};
use crate::value::CelType;

/// A type used during static inference.
///
/// `Dyn` means the type is statically unknown and is treated as compatible with
/// everything, so the checker only errors when a type is DEFINITELY known and
/// DEFINITELY wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InferredType {
    /// Statically unknown — compatible with any field type.
    Dyn,
    /// A definitely-known CEL type.
    Known(CelType),
}

/// A static type environment: identifier name -> inferred type.
pub type TypeEnv = BTreeMap<String, InferredType>;

/// An expression-level type error.
///
/// Carries no source position: the CEL [`Expr`] AST is position-free, so the DSL
/// layer maps the error back onto a `line:column` using the annotation's source
/// span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeError {
    /// A human-readable description of the type mismatch.
    pub message: String,
}

impl std::fmt::Display for TypeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for TypeError {}

/// Which capability annotation a rule expression belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleRole {
    /// `@require(expr, message)` — `expr` must be boolean.
    Require,
    /// `@compute(expr)` — `expr`'s result must be assignable to the field.
    Compute,
    /// `@default(expr)` — `expr`'s result must be assignable to the field.
    Default,
}

/// A clean human-readable label for a [`CelType`] used in error messages.
fn cel_type_label(t: CelType) -> &'static str {
    match t {
        CelType::Null => "null",
        CelType::Bool => "bool",
        CelType::Int => "int",
        CelType::Uint => "uint",
        CelType::Double => "double",
        CelType::String => "string",
        CelType::Bytes => "bytes",
        CelType::Timestamp => "timestamp",
        CelType::Duration => "duration",
        CelType::List => "list",
        CelType::Map => "map",
        CelType::Type => "type",
        CelType::Optional => "optional",
    }
}

/// A human-readable label for an [`InferredType`].
fn inferred_label(inferred: &InferredType) -> String {
    match inferred {
        InferredType::Dyn => "dyn".to_string(),
        InferredType::Known(t) => cel_type_label(*t).to_string(),
    }
}

/// Statically infer the type of `expr` under `env`.
///
/// TOTAL: never returns an error. Returns [`InferredType::Dyn`] whenever the type
/// cannot be proven, so callers only ever see a `Known` type when it is certain.
pub fn infer(expr: &Expr, env: &TypeEnv) -> InferredType {
    match expr {
        Expr::Literal(lit) => infer_literal(lit),
        Expr::Ident(name) => env.get(name).cloned().unwrap_or(InferredType::Dyn),
        Expr::Select { test_only, .. } => {
            if *test_only {
                InferredType::Known(CelType::Bool)
            } else {
                InferredType::Dyn
            }
        }
        Expr::Index { .. } => InferredType::Dyn,
        Expr::Struct { .. } => InferredType::Dyn,
        Expr::List(_) => InferredType::Known(CelType::List),
        Expr::Map(_) => InferredType::Known(CelType::Map),
        Expr::Unary { op, operand } => infer_unary(*op, operand, env),
        Expr::Binary { op, lhs, rhs } => infer_binary(*op, lhs, rhs, env),
        Expr::Ternary { then, els, .. } => infer_ternary(then, els, env),
        Expr::Call { function, .. } => infer_call(function),
        Expr::Comprehension(c) => infer_comprehension(c, env),
    }
}

fn infer_literal(lit: &Literal) -> InferredType {
    match lit {
        // `null` is conservatively Dyn: it is assignable in many positions.
        Literal::Null => InferredType::Dyn,
        Literal::Bool(_) => InferredType::Known(CelType::Bool),
        Literal::Int(_) => InferredType::Known(CelType::Int),
        Literal::Uint(_) => InferredType::Known(CelType::Uint),
        Literal::Double(_) => InferredType::Known(CelType::Double),
        Literal::String(_) => InferredType::Known(CelType::String),
        Literal::Bytes(_) => InferredType::Known(CelType::Bytes),
    }
}

fn infer_unary(op: UnaryOp, operand: &Expr, env: &TypeEnv) -> InferredType {
    match op {
        UnaryOp::Not => InferredType::Known(CelType::Bool),
        UnaryOp::Neg => match infer(operand, env) {
            InferredType::Known(CelType::Int) => InferredType::Known(CelType::Int),
            InferredType::Known(CelType::Double) => InferredType::Known(CelType::Double),
            InferredType::Known(CelType::Duration) => InferredType::Known(CelType::Duration),
            _ => InferredType::Dyn,
        },
    }
}

fn infer_binary(op: BinaryOp, lhs: &Expr, rhs: &Expr, env: &TypeEnv) -> InferredType {
    use BinaryOp::*;
    match op {
        // Comparisons, equality, membership, and logical ops are always boolean,
        // regardless of operand types.
        Eq | Ne | Lt | Le | Gt | Ge | In | And | Or => InferredType::Known(CelType::Bool),
        Add => infer_add(lhs, rhs, env),
        Sub | Mul | Div | Rem => infer_arith(op, lhs, rhs, env),
    }
}

fn infer_add(lhs: &Expr, rhs: &Expr, env: &TypeEnv) -> InferredType {
    let l = infer(lhs, env);
    let r = infer(rhs, env);
    match (known(&l), known(&r)) {
        (Some(CelType::String), Some(CelType::String)) => InferredType::Known(CelType::String),
        (Some(CelType::Bytes), Some(CelType::Bytes)) => InferredType::Known(CelType::Bytes),
        (Some(CelType::List), Some(CelType::List)) => InferredType::Known(CelType::List),
        (Some(CelType::Timestamp), Some(CelType::Duration))
        | (Some(CelType::Duration), Some(CelType::Timestamp)) => {
            InferredType::Known(CelType::Timestamp)
        }
        (Some(CelType::Duration), Some(CelType::Duration)) => {
            InferredType::Known(CelType::Duration)
        }
        (Some(CelType::Int), Some(CelType::Int)) => InferredType::Known(CelType::Int),
        (Some(CelType::Uint), Some(CelType::Uint)) => InferredType::Known(CelType::Uint),
        (Some(CelType::Double), Some(CelType::Double)) => InferredType::Known(CelType::Double),
        _ => InferredType::Dyn,
    }
}

fn infer_arith(op: BinaryOp, lhs: &Expr, rhs: &Expr, env: &TypeEnv) -> InferredType {
    let l = infer(lhs, env);
    let r = infer(rhs, env);
    match (known(&l), known(&r)) {
        (Some(CelType::Int), Some(CelType::Int)) => InferredType::Known(CelType::Int),
        (Some(CelType::Uint), Some(CelType::Uint)) => InferredType::Known(CelType::Uint),
        (Some(CelType::Double), Some(CelType::Double)) => InferredType::Known(CelType::Double),
        (Some(CelType::Timestamp), Some(CelType::Timestamp)) if op == BinaryOp::Sub => {
            InferredType::Known(CelType::Duration)
        }
        (Some(CelType::Timestamp), Some(CelType::Duration)) if op == BinaryOp::Sub => {
            InferredType::Known(CelType::Timestamp)
        }
        (Some(CelType::Duration), Some(CelType::Duration))
            if op == BinaryOp::Sub || op == BinaryOp::Add =>
        {
            InferredType::Known(CelType::Duration)
        }
        _ => InferredType::Dyn,
    }
}

fn infer_ternary(then: &Expr, els: &Expr, env: &TypeEnv) -> InferredType {
    let t = infer(then, env);
    let e = infer(els, env);
    if t == e && matches!(t, InferredType::Known(_)) {
        t
    } else {
        InferredType::Dyn
    }
}

fn infer_call(function: &str) -> InferredType {
    let ty = match function {
        "size" | "int" => CelType::Int,
        "uint" => CelType::Uint,
        "double" => CelType::Double,
        "string" => CelType::String,
        "bool" => CelType::Bool,
        "bytes" => CelType::Bytes,
        "timestamp" => CelType::Timestamp,
        "duration" => CelType::Duration,
        // Encoders extension (cel-spec): `base64.encode(bytes) -> string`,
        // `base64.decode(string) -> bytes`.
        "base64.encode" => CelType::String,
        "base64.decode" => CelType::Bytes,
        "type" => CelType::Type,
        "matches" | "contains" | "startsWith" | "endsWith" | "hasValue" => CelType::Bool,
        "getFullYear" | "getMonth" | "getDayOfMonth" | "getDate" | "getDayOfWeek"
        | "getDayOfYear" | "getHours" | "getMinutes" | "getSeconds" | "getMilliseconds" => {
            CelType::Int
        }
        "lowerAscii" | "upperAscii" | "trim" | "replace" | "substring" | "charAt" => {
            CelType::String
        }
        "split" => CelType::List,
        "indexOf" | "lastIndexOf" => CelType::Int,
        // `dyn` and any unmodeled function -> Dyn (never error on unknown calls).
        _ => return InferredType::Dyn,
    };
    InferredType::Known(ty)
}

fn infer_comprehension(c: &Comprehension, env: &TypeEnv) -> InferredType {
    let mut child = env.clone();
    child.insert(c.accu_var.clone(), infer(&c.accu_init, env));
    child.insert(c.iter_var.clone(), InferredType::Dyn);
    if let Some(v2) = &c.iter_var2 {
        child.insert(v2.clone(), InferredType::Dyn);
    }
    infer(&c.result, &child)
}

/// Extract the concrete [`CelType`] from an [`InferredType`], if known.
fn known(inferred: &InferredType) -> Option<CelType> {
    match inferred {
        InferredType::Known(t) => Some(*t),
        InferredType::Dyn => None,
    }
}

/// The CEL type a field's stored value presents as inside a rule expression.
///
/// This is the type seen when the field is referenced as a sibling in a rule —
/// e.g. an `Enum` field reads back as a `String`, a one-cardinality `Relation`
/// reads back as a `String` id.
pub fn field_type_to_inferred(ft: &FieldType) -> InferredType {
    use schema_forge_core::types::Cardinality;
    match ft {
        FieldType::Text(_) | FieldType::RichText => InferredType::Known(CelType::String),
        FieldType::Integer(_) => InferredType::Known(CelType::Int),
        FieldType::Float(_) => InferredType::Known(CelType::Double),
        FieldType::Boolean => InferredType::Known(CelType::Bool),
        FieldType::DateTime => InferredType::Known(CelType::Timestamp),
        FieldType::Duration => InferredType::Known(CelType::Duration),
        FieldType::Bytes(_) => InferredType::Known(CelType::Bytes),
        FieldType::Enum(_) => InferredType::Known(CelType::String),
        FieldType::Json => InferredType::Dyn,
        FieldType::Relation { cardinality, .. } => match cardinality {
            Cardinality::One => InferredType::Known(CelType::String),
            Cardinality::Many => InferredType::Known(CelType::List),
            // `Cardinality` is non_exhaustive; an unknown future variant is
            // treated as statically unknown rather than guessing.
            _ => InferredType::Dyn,
        },
        FieldType::Array(_) => InferredType::Known(CelType::List),
        FieldType::Composite(_) => InferredType::Known(CelType::Map),
        FieldType::File(_) => InferredType::Dyn,
        // `FieldType` is non_exhaustive; an unknown future type is statically
        // unknown so it never produces a false positive.
        _ => InferredType::Dyn,
    }
}

/// Whether a rule result of type `inferred` may be assigned to a field of type
/// `ft`.
///
/// Returns `true` immediately when `inferred` is [`InferredType::Dyn`]
/// (conservative). For a `Known(t)`, mirrors the runtime coercions so a rule that
/// would succeed at runtime is never rejected.
pub fn field_accepts(ft: &FieldType, inferred: &InferredType) -> bool {
    let Some(t) = known(inferred) else {
        return true;
    };
    match ft {
        FieldType::Text(_) | FieldType::RichText => t == CelType::String,
        FieldType::Integer(_) => matches!(t, CelType::Int | CelType::Uint),
        FieldType::Float(_) => matches!(t, CelType::Double | CelType::Int | CelType::Uint),
        FieldType::Boolean => t == CelType::Bool,
        FieldType::DateTime => matches!(t, CelType::Timestamp | CelType::String),
        FieldType::Duration => matches!(t, CelType::Duration | CelType::String),
        FieldType::Bytes(_) => matches!(t, CelType::Bytes | CelType::String),
        FieldType::Enum(_) => t == CelType::String,
        // Accept anything: the mapping is complex/rare; avoid false positives.
        FieldType::Json | FieldType::Relation { .. } | FieldType::File(_) => true,
        FieldType::Array(_) => t == CelType::List,
        FieldType::Composite(_) => t == CelType::Map,
        // `FieldType` is non_exhaustive; accept anything for an unknown future
        // type to stay false-positive-averse.
        _ => true,
    }
}

/// Type-check one rule expression for the given `role` against `field_type`.
///
/// Only a DEFINITELY-known, DEFINITELY-incompatible result produces an error;
/// any `Dyn` inference passes.
pub fn check_rule(
    role: RuleRole,
    field_type: &FieldType,
    env: &TypeEnv,
    expr: &Expr,
) -> Result<(), TypeError> {
    let inferred = infer(expr, env);
    match role {
        RuleRole::Require => check_require(&inferred),
        RuleRole::Compute => check_assignable(RuleRole::Compute, field_type, &inferred),
        RuleRole::Default => check_assignable(RuleRole::Default, field_type, &inferred),
    }
}

fn check_require(inferred: &InferredType) -> Result<(), TypeError> {
    if let InferredType::Known(t) = inferred {
        if *t != CelType::Bool {
            return Err(TypeError {
                message: format!(
                    "@require expression must be boolean, but evaluates to {}",
                    cel_type_label(*t)
                ),
            });
        }
    }
    Ok(())
}

fn check_assignable(
    role: RuleRole,
    field_type: &FieldType,
    inferred: &InferredType,
) -> Result<(), TypeError> {
    if field_accepts(field_type, inferred) {
        return Ok(());
    }
    let annotation = match role {
        RuleRole::Compute => "@compute",
        RuleRole::Default => "@default",
        RuleRole::Require => "@require",
    };
    Err(TypeError {
        message: format!(
            "{annotation} result type {} is not assignable to field type {field_type}",
            inferred_label(inferred)
        ),
    })
}

/// Build the rule type environment from the schema's fields.
///
/// Each field is inserted by name with its [`field_type_to_inferred`] type, then
/// `principal` (Map) and `now` (Timestamp) are inserted last so they win over any
/// same-named field — mirroring the runtime bindings in `rules.rs::build_bindings`.
pub fn rule_type_env<'a>(fields: impl IntoIterator<Item = (&'a str, &'a FieldType)>) -> TypeEnv {
    let mut env = TypeEnv::new();
    for (name, ft) in fields {
        env.insert(name.to_string(), field_type_to_inferred(ft));
    }
    env.insert("principal".to_string(), InferredType::Known(CelType::Map));
    env.insert("now".to_string(), InferredType::Known(CelType::Timestamp));
    env
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse;
    use schema_forge_core::types::{
        BytesConstraints, Cardinality, EnumVariants, FloatConstraints, IntegerConstraints,
        SchemaName, TextConstraints,
    };

    fn infer_src(src: &str, env: &TypeEnv) -> InferredType {
        let expr = parse(src).expect("test expression must parse");
        infer(&expr, env)
    }

    fn empty() -> TypeEnv {
        TypeEnv::new()
    }

    // -- infer: literals --

    #[test]
    fn infer_literals() {
        let env = empty();
        assert_eq!(infer_src("true", &env), InferredType::Known(CelType::Bool));
        assert_eq!(infer_src("42", &env), InferredType::Known(CelType::Int));
        assert_eq!(infer_src("42u", &env), InferredType::Known(CelType::Uint));
        assert_eq!(infer_src("1.5", &env), InferredType::Known(CelType::Double));
        assert_eq!(
            infer_src("\"hi\"", &env),
            InferredType::Known(CelType::String)
        );
        assert_eq!(
            infer_src("b\"hi\"", &env),
            InferredType::Known(CelType::Bytes)
        );
        assert_eq!(infer_src("null", &env), InferredType::Dyn);
    }

    #[test]
    fn infer_ident_uses_env_else_dyn() {
        let mut env = empty();
        env.insert("age".to_string(), InferredType::Known(CelType::Int));
        assert_eq!(infer_src("age", &env), InferredType::Known(CelType::Int));
        assert_eq!(infer_src("unknown_ident", &env), InferredType::Dyn);
    }

    // -- infer: comparisons & logical --

    #[test]
    fn infer_comparisons_are_bool() {
        let mut env = empty();
        env.insert("age".to_string(), InferredType::Known(CelType::Int));
        assert_eq!(
            infer_src("age >= 18", &env),
            InferredType::Known(CelType::Bool)
        );
        assert_eq!(
            infer_src("age == 18 && age < 99", &env),
            InferredType::Known(CelType::Bool)
        );
        assert_eq!(
            infer_src("\"a\" in [\"a\", \"b\"]", &env),
            InferredType::Known(CelType::Bool)
        );
    }

    // -- infer: arithmetic --

    #[test]
    fn infer_arithmetic() {
        let env = empty();
        assert_eq!(infer_src("1 + 2", &env), InferredType::Known(CelType::Int));
        assert_eq!(
            infer_src("1.0 * 2.0", &env),
            InferredType::Known(CelType::Double)
        );
        assert_eq!(
            infer_src("\"a\" + \"b\"", &env),
            InferredType::Known(CelType::String)
        );
        // Mixed/unknown operands -> Dyn (conservative).
        let mut e2 = empty();
        e2.insert("x".to_string(), InferredType::Dyn);
        assert_eq!(infer_src("x + 1", &e2), InferredType::Dyn);
    }

    #[test]
    fn infer_neg() {
        let env = empty();
        assert_eq!(infer_src("-5", &env), InferredType::Known(CelType::Int));
        assert_eq!(
            infer_src("-5.0", &env),
            InferredType::Known(CelType::Double)
        );
    }

    // -- infer: ternary --

    #[test]
    fn infer_ternary_same_branches() {
        let mut env = empty();
        env.insert("c".to_string(), InferredType::Known(CelType::Bool));
        assert_eq!(
            infer_src("c ? 1 : 2", &env),
            InferredType::Known(CelType::Int)
        );
        // Divergent branches -> Dyn.
        assert_eq!(infer_src("c ? 1 : \"x\"", &env), InferredType::Dyn);
    }

    // -- infer: calls --

    #[test]
    fn infer_calls() {
        let env = empty();
        assert_eq!(
            infer_src("size([1, 2])", &env),
            InferredType::Known(CelType::Int)
        );
        assert_eq!(
            infer_src("string(42)", &env),
            InferredType::Known(CelType::String)
        );
        assert_eq!(
            infer_src("int(\"5\")", &env),
            InferredType::Known(CelType::Int)
        );
        assert_eq!(
            infer_src("\"abc\".startsWith(\"a\")", &env),
            InferredType::Known(CelType::Bool)
        );
        assert_eq!(
            infer_src("\"abc\".upperAscii()", &env),
            InferredType::Known(CelType::String)
        );
        // Encoders extension: base64.encode -> string, base64.decode -> bytes.
        assert_eq!(
            infer_src("base64.encode(b\"abc\")", &env),
            InferredType::Known(CelType::String)
        );
        assert_eq!(
            infer_src("base64.decode(\"YWJj\")", &env),
            InferredType::Known(CelType::Bytes)
        );
        // Unknown function -> Dyn.
        assert_eq!(infer_src("mysteryFn(1)", &env), InferredType::Dyn);
        // dyn() -> Dyn.
        assert_eq!(infer_src("dyn(1)", &env), InferredType::Dyn);
    }

    #[test]
    fn size_of_bytes_field_infers_int() {
        let bytes = FieldType::Bytes(BytesConstraints::unconstrained());
        let env = rule_type_env(std::iter::once(("sig", &bytes)));
        assert_eq!(
            infer_src("size(sig)", &env),
            InferredType::Known(CelType::Int)
        );
    }

    #[test]
    fn field_accepts_bytes_accepts_bytes_and_string() {
        let bytes = FieldType::Bytes(BytesConstraints::unconstrained());
        assert!(field_accepts(&bytes, &InferredType::Known(CelType::Bytes)));
        assert!(field_accepts(&bytes, &InferredType::Known(CelType::String)));
        assert!(!field_accepts(&bytes, &InferredType::Known(CelType::Int)));
    }

    // -- infer: comprehensions --

    #[test]
    fn infer_comprehensions() {
        let mut env = empty();
        env.insert("xs".to_string(), InferredType::Known(CelType::List));
        assert_eq!(
            infer_src("xs.all(x, x > 0)", &env),
            InferredType::Known(CelType::Bool)
        );
        assert_eq!(
            infer_src("xs.exists(x, x > 0)", &env),
            InferredType::Known(CelType::Bool)
        );
        assert_eq!(
            infer_src("xs.map(x, x + 1)", &env),
            InferredType::Known(CelType::List)
        );
        assert_eq!(
            infer_src("xs.filter(x, x > 0)", &env),
            InferredType::Known(CelType::List)
        );
    }

    // -- field_type_to_inferred --

    #[test]
    fn field_type_mapping() {
        assert_eq!(
            field_type_to_inferred(&FieldType::Text(TextConstraints::unconstrained())),
            InferredType::Known(CelType::String)
        );
        assert_eq!(
            field_type_to_inferred(&FieldType::Integer(IntegerConstraints::unconstrained())),
            InferredType::Known(CelType::Int)
        );
        assert_eq!(
            field_type_to_inferred(&FieldType::Float(FloatConstraints::unconstrained())),
            InferredType::Known(CelType::Double)
        );
        assert_eq!(
            field_type_to_inferred(&FieldType::DateTime),
            InferredType::Known(CelType::Timestamp)
        );
        assert_eq!(
            field_type_to_inferred(&FieldType::Duration),
            InferredType::Known(CelType::Duration)
        );
        assert_eq!(
            field_type_to_inferred(&FieldType::Bytes(BytesConstraints::unconstrained())),
            InferredType::Known(CelType::Bytes)
        );
        assert_eq!(field_type_to_inferred(&FieldType::Json), InferredType::Dyn);
        assert_eq!(
            field_type_to_inferred(&FieldType::Relation {
                target: SchemaName::new("Company").unwrap(),
                cardinality: Cardinality::One,
            }),
            InferredType::Known(CelType::String)
        );
        assert_eq!(
            field_type_to_inferred(&FieldType::Relation {
                target: SchemaName::new("Company").unwrap(),
                cardinality: Cardinality::Many,
            }),
            InferredType::Known(CelType::List)
        );
    }

    // -- field_accepts matrix --

    #[test]
    fn field_accepts_dyn_always() {
        assert!(field_accepts(&FieldType::Boolean, &InferredType::Dyn));
        assert!(field_accepts(&FieldType::DateTime, &InferredType::Dyn));
    }

    #[test]
    fn field_accepts_float_accepts_int() {
        let float = FieldType::Float(FloatConstraints::unconstrained());
        assert!(field_accepts(&float, &InferredType::Known(CelType::Double)));
        assert!(field_accepts(&float, &InferredType::Known(CelType::Int)));
        assert!(field_accepts(&float, &InferredType::Known(CelType::Uint)));
        assert!(!field_accepts(
            &float,
            &InferredType::Known(CelType::String)
        ));
    }

    #[test]
    fn field_accepts_datetime_accepts_string() {
        assert!(field_accepts(
            &FieldType::DateTime,
            &InferredType::Known(CelType::String)
        ));
        assert!(field_accepts(
            &FieldType::DateTime,
            &InferredType::Known(CelType::Timestamp)
        ));
        assert!(!field_accepts(
            &FieldType::DateTime,
            &InferredType::Known(CelType::Int)
        ));
    }

    #[test]
    fn field_accepts_duration_accepts_duration_and_string() {
        assert!(field_accepts(
            &FieldType::Duration,
            &InferredType::Known(CelType::Duration)
        ));
        assert!(field_accepts(
            &FieldType::Duration,
            &InferredType::Known(CelType::String)
        ));
        assert!(!field_accepts(
            &FieldType::Duration,
            &InferredType::Known(CelType::Int)
        ));
    }

    #[test]
    fn field_accepts_integer_rejects_double() {
        let int = FieldType::Integer(IntegerConstraints::unconstrained());
        assert!(field_accepts(&int, &InferredType::Known(CelType::Int)));
        assert!(!field_accepts(&int, &InferredType::Known(CelType::Double)));
    }

    #[test]
    fn field_accepts_text_only_string() {
        let text = FieldType::Text(TextConstraints::unconstrained());
        assert!(field_accepts(&text, &InferredType::Known(CelType::String)));
        assert!(!field_accepts(&text, &InferredType::Known(CelType::Int)));
    }

    #[test]
    fn field_accepts_json_relation_file_accept_all() {
        assert!(field_accepts(
            &FieldType::Json,
            &InferredType::Known(CelType::Int)
        ));
        assert!(field_accepts(
            &FieldType::Relation {
                target: SchemaName::new("Company").unwrap(),
                cardinality: Cardinality::One,
            },
            &InferredType::Known(CelType::Int)
        ));
    }

    // -- check_rule: pass cases --

    fn int_env() -> TypeEnv {
        let mut env = empty();
        env.insert("age".to_string(), InferredType::Known(CelType::Int));
        env.insert("count".to_string(), InferredType::Known(CelType::Int));
        env
    }

    #[test]
    fn check_require_pass_boolean() {
        let env = int_env();
        let expr = parse("age >= 18").unwrap();
        assert!(check_rule(
            RuleRole::Require,
            &FieldType::Integer(IntegerConstraints::unconstrained()),
            &env,
            &expr
        )
        .is_ok());
    }

    #[test]
    fn check_compute_pass_string_to_text() {
        let env = int_env();
        let expr = parse("string(count)").unwrap();
        assert!(check_rule(
            RuleRole::Compute,
            &FieldType::Text(TextConstraints::unconstrained()),
            &env,
            &expr
        )
        .is_ok());
    }

    #[test]
    fn check_default_pass_now_to_datetime() {
        let env = rule_type_env(std::iter::empty());
        let expr = parse("now").unwrap();
        assert!(check_rule(RuleRole::Default, &FieldType::DateTime, &env, &expr).is_ok());
    }

    #[test]
    fn check_default_pass_now_call_is_dyn() {
        // `now()` is a call (Dyn), not the `now` variable; must not be rejected.
        let env = rule_type_env(std::iter::empty());
        let expr = parse("now()").unwrap();
        assert!(check_rule(RuleRole::Default, &FieldType::DateTime, &env, &expr).is_ok());
    }

    #[test]
    fn check_require_timestamp_minus_now_against_duration_typechecks() {
        // The retention rule from issue #96: `now - created_at < duration('220752000s')`.
        // `now - created_at` is timestamp-timestamp = duration; comparing two durations
        // yields a bool, which @require accepts.
        let created = FieldType::DateTime;
        let env = rule_type_env(std::iter::once(("created_at", &created)));
        let expr = parse("now - created_at < duration('220752000s')").unwrap();
        assert!(check_rule(RuleRole::Require, &FieldType::Boolean, &env, &expr).is_ok());
    }

    #[test]
    fn check_compute_duration_assignable_to_duration_field() {
        let env = rule_type_env(std::iter::empty());
        let expr = parse("duration('60s')").unwrap();
        assert!(check_rule(RuleRole::Compute, &FieldType::Duration, &env, &expr).is_ok());
    }

    // -- check_rule: fail cases --

    #[test]
    fn check_require_fail_non_boolean() {
        let env = int_env();
        let expr = parse("age").unwrap();
        let err = check_rule(
            RuleRole::Require,
            &FieldType::Integer(IntegerConstraints::unconstrained()),
            &env,
            &expr,
        )
        .unwrap_err();
        assert!(err.message.contains("boolean"));
        assert!(err.message.contains("int"));
    }

    #[test]
    fn check_compute_fail_int_to_text() {
        let env = int_env();
        let expr = parse("count + 1").unwrap();
        let err = check_rule(
            RuleRole::Compute,
            &FieldType::Text(TextConstraints::unconstrained()),
            &env,
            &expr,
        )
        .unwrap_err();
        assert!(err.message.contains("@compute"));
        assert!(err.message.contains("not assignable"));
    }

    #[test]
    fn check_default_fail_double_to_integer() {
        let env = empty();
        let expr = parse("1.0").unwrap();
        let err = check_rule(
            RuleRole::Default,
            &FieldType::Integer(IntegerConstraints::unconstrained()),
            &env,
            &expr,
        )
        .unwrap_err();
        assert!(err.message.contains("@default"));
        assert!(err.message.contains("not assignable"));
    }

    // -- rule_type_env --

    #[test]
    fn rule_type_env_overrides_with_principal_and_now() {
        let int = FieldType::Integer(IntegerConstraints::unconstrained());
        // A field literally named `now` must be overridden by the Timestamp binding.
        let fields = vec![("now", &int), ("age", &int)];
        let env = rule_type_env(fields);
        assert_eq!(
            env.get("now"),
            Some(&InferredType::Known(CelType::Timestamp))
        );
        assert_eq!(
            env.get("principal"),
            Some(&InferredType::Known(CelType::Map))
        );
        assert_eq!(env.get("age"), Some(&InferredType::Known(CelType::Int)));
    }

    #[test]
    fn type_error_is_display_and_error() {
        let err = TypeError {
            message: "boom".to_string(),
        };
        assert_eq!(err.to_string(), "boom");
        let _e: &dyn std::error::Error = &err;
    }

    // Ensure EnumVariants is exercised so the enum arm is covered.
    #[test]
    fn field_accepts_enum_only_string() {
        let e = FieldType::Enum(EnumVariants::new(vec!["A".to_string(), "B".to_string()]).unwrap());
        assert!(field_accepts(&e, &InferredType::Known(CelType::String)));
        assert!(!field_accepts(&e, &InferredType::Known(CelType::Int)));
    }
}
