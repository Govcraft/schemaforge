//! CEL-backed write-time validation rules.
//!
//! This module holds the pure rule-evaluation logic for `@require` (and, in
//! future, `@compute`/`@default` via #93/#94), deliberately decoupled from the
//! axum handlers so it can be unit-tested without any HTTP machinery.
//!
//! ## Security: fail-closed
//!
//! These rules run *in-transaction, before persistence* on a government
//! production target. The contract is **fail-closed**: a `@require` predicate
//! that errors, references an undeclared field, or yields a non-boolean must
//! *block* the write — never let it through. A predicate is only permitted to
//! pass when it evaluates to exactly `Ok(CelValue::Bool(true))`. Any other
//! outcome surfaces as either a rejection (the predicate definitively returned
//! `false`) or a [`RuleError::Eval`] (the predicate could not yield a definite
//! boolean — treated as a schema-authoring/server fault, mapped to 500).

use std::collections::BTreeMap;
use std::fmt;

use acton_service::middleware::Claims;
use schema_forge_cel::{cel_to_dynamic, dynamic_to_cel, CelKey, CelValue};
use schema_forge_core::types::{DynamicValue, FieldAnnotation, FieldType, SchemaDefinition};

/// The outcome of a failed rule evaluation.
#[derive(Debug, Clone, PartialEq)]
pub enum RuleError {
    /// One or more `@require` predicates evaluated to `false`. Carries the
    /// human-readable messages, in deterministic (schema declaration) order.
    /// Maps to HTTP 422.
    Rejected(Vec<String>),
    /// A `@require` predicate could not be evaluated to a definite boolean —
    /// it errored or returned a non-boolean. This is a schema-authoring or
    /// server fault, so it fails closed and maps to HTTP 500.
    Eval {
        /// The field whose `@require` annotation could not be evaluated.
        field: String,
        /// A human-readable detail (the CEL error, or the non-bool reason).
        detail: String,
    },
}

impl fmt::Display for RuleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rejected(messages) => {
                write!(f, "validation rejected: {}", messages.join("; "))
            }
            Self::Eval { field, detail } => {
                write!(
                    f,
                    "@require on field '{field}' could not be evaluated: {detail}"
                )
            }
        }
    }
}

impl std::error::Error for RuleError {}

/// Build the CEL [`Bindings`](schema_forge_cel::Bindings) for a write.
///
/// Each entity field is bound under its own name (its CEL value via
/// [`dynamic_to_cel`]). A field that fails conversion is *skipped* rather than
/// aborting the whole build: a predicate that references it then sees an
/// "undeclared reference" eval error, which [`check_requires`] handles
/// fail-closed.
///
/// A `principal` map is always bound (even when `claims` is `None`, in which
/// case it is an empty map) so that `has(principal.sub)` is a clean `false`
/// rather than an undeclared-reference error.
pub fn build_bindings(
    fields: &BTreeMap<String, DynamicValue>,
    claims: Option<&Claims>,
) -> schema_forge_cel::Bindings {
    let mut bindings = schema_forge_cel::Bindings::new();

    for (name, value) in fields {
        if let Ok(cel) = dynamic_to_cel(value) {
            bindings.insert(name.clone(), cel);
        }
        // On conversion failure we intentionally omit the binding; a predicate
        // referencing it will error and be handled fail-closed downstream.
    }

    bindings.insert("principal".to_string(), principal_map(claims));
    bindings
}

/// Build the `principal` CEL map from optional claims.
///
/// When `claims` is `None`, returns an empty map so membership checks like
/// `has(principal.sub)` evaluate to `false` instead of raising an
/// undeclared-reference error.
fn principal_map(claims: Option<&Claims>) -> CelValue {
    let mut map = BTreeMap::new();
    if let Some(c) = claims {
        map.insert(
            CelKey::String("sub".to_string()),
            CelValue::String(c.sub.clone()),
        );
        if let Some(email) = &c.email {
            map.insert(
                CelKey::String("email".to_string()),
                CelValue::String(email.clone()),
            );
        }
        if let Some(username) = &c.username {
            map.insert(
                CelKey::String("username".to_string()),
                CelValue::String(username.clone()),
            );
        }
        map.insert(
            CelKey::String("roles".to_string()),
            CelValue::List(c.roles.iter().cloned().map(CelValue::String).collect()),
        );
        map.insert(
            CelKey::String("perms".to_string()),
            CelValue::List(c.perms.iter().cloned().map(CelValue::String).collect()),
        );
    }
    CelValue::Map(map)
}

/// Evaluate every `@require` annotation on the schema's fields against a write.
///
/// Fields are visited in schema declaration order, and each field's
/// annotations in their declared order, so collected rejection messages are
/// deterministic.
///
/// Fail-closed (see the module docs): a predicate passes only on
/// `Ok(CelValue::Bool(true))`. A definite `false` is collected as a rejection
/// (→ [`RuleError::Rejected`], 422). A non-boolean result or an evaluation
/// error short-circuits immediately to [`RuleError::Eval`] (500) so a broken
/// predicate can never let a write through.
pub fn check_requires(
    schema: &SchemaDefinition,
    fields: &BTreeMap<String, DynamicValue>,
    claims: Option<&Claims>,
) -> Result<(), RuleError> {
    let bindings = build_bindings(fields, claims);
    let mut rejections = Vec::new();

    for field in &schema.fields {
        for annotation in &field.annotations {
            let FieldAnnotation::Require { expr, message } = annotation else {
                continue;
            };

            match schema_forge_cel::evaluate(expr, &bindings) {
                Ok(CelValue::Bool(true)) => {}
                Ok(CelValue::Bool(false)) => rejections.push(message.clone()),
                Ok(_) => {
                    return Err(RuleError::Eval {
                        field: field.name.as_str().to_string(),
                        detail: "require expression did not evaluate to a boolean".to_string(),
                    });
                }
                Err(e) => {
                    return Err(RuleError::Eval {
                        field: field.name.as_str().to_string(),
                        detail: e.to_string(),
                    });
                }
            }
        }
    }

    if rejections.is_empty() {
        Ok(())
    } else {
        Err(RuleError::Rejected(rejections))
    }
}

/// Evaluate every `@compute` annotation on the schema's fields and store the
/// derived value into `fields`.
///
/// ## Decision: computed values are STORED and OVERWRITE client input
///
/// `@compute` fields are *server-derived*: the value is computed at write time
/// and persisted (not virtual/recomputed on read), and any client-supplied
/// value for a compute field is **overwritten** by the computed result. This
/// keeps the stored record self-consistent and means a malicious or mistaken
/// client cannot smuggle a value into a derived field.
///
/// Fields are visited in schema declaration order, and the bindings are rebuilt
/// from the *current* `fields` before each compute, so a later computed field
/// can read an earlier computed field's freshly-stored value (deterministic,
/// chainable).
///
/// Fail-closed: an evaluation error or a value that cannot be converted /
/// coerced to the field's declared type returns [`RuleError::Eval`] (500) and
/// stores nothing for that field — a half-evaluated value is never persisted.
/// This runs *before* [`check_requires`] so `@require` predicates validate the
/// computed values.
pub fn apply_computed(
    schema: &SchemaDefinition,
    fields: &mut BTreeMap<String, DynamicValue>,
    claims: Option<&Claims>,
) -> Result<(), RuleError> {
    for field in &schema.fields {
        for annotation in &field.annotations {
            let FieldAnnotation::Compute { expr } = annotation else {
                continue;
            };

            // Rebuild bindings from the current fields so this compute sees the
            // results of any earlier computed fields (chaining).
            let bindings = build_bindings(fields, claims);
            let field_name = field.name.as_str();

            let cel_value = schema_forge_cel::evaluate(expr, &bindings).map_err(|e| {
                RuleError::Eval {
                    field: field_name.to_string(),
                    detail: e.to_string(),
                }
            })?;

            let natural = cel_to_dynamic(&cel_value).map_err(|e| RuleError::Eval {
                field: field_name.to_string(),
                detail: e.to_string(),
            })?;

            let coerced = coerce_to_field_type(natural, &field.field_type, field_name)?;
            fields.insert(field_name.to_string(), coerced);
        }
    }

    Ok(())
}

/// Coerce a naturally-converted [`DynamicValue`] toward a field's declared
/// [`FieldType`] for the safe, lossless cases used by `@compute`/`@default`.
///
/// Only a small, well-defined set of coercions is performed:
/// - `Float` field + `Integer(i)` → `Float(i as f64)` (lossless widening).
/// - `Enum` field + `Text(s)` → `Enum(s)` when `s` is a declared variant,
///   otherwise [`RuleError::Eval`].
/// - `DateTime` field + `Text(s)` → parse `s` as RFC 3339, otherwise
///   [`RuleError::Eval`].
///
/// Every other combination passes the natural value through unchanged. Full
/// strict type-checking of compute results against the declared field type at
/// schema-apply time is tracked separately (#104) and is intentionally not done
/// here.
fn coerce_to_field_type(
    value: DynamicValue,
    field_type: &FieldType,
    field: &str,
) -> Result<DynamicValue, RuleError> {
    match (field_type, value) {
        (FieldType::Float(_), DynamicValue::Integer(i)) => Ok(DynamicValue::Float(i as f64)),
        (FieldType::Enum(variants), DynamicValue::Text(s)) => {
            if variants.iter().any(|v| v == &s) {
                Ok(DynamicValue::Enum(s))
            } else {
                Err(RuleError::Eval {
                    field: field.to_string(),
                    detail: format!(
                        "computed value '{s}' is not a variant of enum field '{field}'"
                    ),
                })
            }
        }
        (FieldType::DateTime, DynamicValue::Text(s)) => {
            chrono::DateTime::parse_from_rfc3339(&s)
                .map(|dt| DynamicValue::DateTime(dt.with_timezone(&chrono::Utc)))
                .map_err(|e| RuleError::Eval {
                    field: field.to_string(),
                    detail: format!("computed value '{s}' is not a valid RFC 3339 datetime: {e}"),
                })
        }
        // No coercion applies; store the natural value as-is.
        (_, value) => Ok(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use schema_forge_core::types::{
        EnumVariants, FieldDefinition, FieldName, FieldType, FloatConstraints, SchemaId,
        SchemaName, TextConstraints,
    };

    fn text_field(name: &str, annotations: Vec<FieldAnnotation>) -> FieldDefinition {
        FieldDefinition::with_annotations(
            FieldName::new(name).unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
            vec![],
            annotations,
        )
    }

    fn schema_with(fields: Vec<FieldDefinition>) -> SchemaDefinition {
        SchemaDefinition::new(SchemaId::new(), SchemaName::new("Thing").unwrap(), fields, vec![])
            .unwrap()
    }

    fn require(expr: &str, message: &str) -> FieldAnnotation {
        FieldAnnotation::Require {
            expr: expr.to_string(),
            message: message.to_string(),
        }
    }

    fn fields(pairs: &[(&str, DynamicValue)]) -> BTreeMap<String, DynamicValue> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect()
    }

    fn claims(roles: &[&str]) -> Claims {
        Claims {
            sub: "user:alice".to_string(),
            email: Some("alice@example.gov".to_string()),
            username: Some("alice".to_string()),
            roles: roles.iter().map(|r| r.to_string()).collect(),
            perms: vec![],
            exp: 9_999_999_999,
            iat: None,
            jti: None,
            iss: None,
            aud: None,
            custom: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn passing_require_ok() {
        let schema = schema_with(vec![text_field(
            "age",
            vec![require("age >= 18", "must be at least 18")],
        )]);
        let f = fields(&[("age", DynamicValue::Integer(21))]);
        assert_eq!(check_requires(&schema, &f, None), Ok(()));
    }

    #[test]
    fn single_failing_require_surfaces_message() {
        let schema = schema_with(vec![text_field(
            "age",
            vec![require("age >= 18", "must be at least 18")],
        )]);
        let f = fields(&[("age", DynamicValue::Integer(16))]);
        assert_eq!(
            check_requires(&schema, &f, None),
            Err(RuleError::Rejected(vec!["must be at least 18".to_string()]))
        );
    }

    #[test]
    fn multiple_failing_requires_collected_in_order() {
        let schema = schema_with(vec![
            text_field("age", vec![require("age >= 18", "too young")]),
            text_field(
                "name",
                vec![require("size(name) > 0", "name required")],
            ),
        ]);
        let f = fields(&[
            ("age", DynamicValue::Integer(10)),
            ("name", DynamicValue::Text(String::new())),
        ]);
        assert_eq!(
            check_requires(&schema, &f, None),
            Err(RuleError::Rejected(vec![
                "too young".to_string(),
                "name required".to_string(),
            ]))
        );
    }

    #[test]
    fn cross_field_invariant() {
        // A closed item must carry a close reason.
        let schema = schema_with(vec![text_field(
            "status",
            vec![require(
                "status != 'closed' || close_reason != null",
                "closed items need a reason",
            )],
        )]);

        // Valid: open, no reason needed.
        let open = fields(&[
            ("status", DynamicValue::Text("open".to_string())),
            ("close_reason", DynamicValue::Null),
        ]);
        assert_eq!(check_requires(&schema, &open, None), Ok(()));

        // Invalid: closed with null reason.
        let closed_no_reason = fields(&[
            ("status", DynamicValue::Text("closed".to_string())),
            ("close_reason", DynamicValue::Null),
        ]);
        assert_eq!(
            check_requires(&schema, &closed_no_reason, None),
            Err(RuleError::Rejected(vec![
                "closed items need a reason".to_string()
            ]))
        );

        // Valid: closed with a reason.
        let closed_with_reason = fields(&[
            ("status", DynamicValue::Text("closed".to_string())),
            ("close_reason", DynamicValue::Text("done".to_string())),
        ]);
        assert_eq!(check_requires(&schema, &closed_with_reason, None), Ok(()));
    }

    #[test]
    fn principal_referencing_predicate() {
        let schema = schema_with(vec![text_field(
            "level",
            vec![require(
                "level <= 1 || 'admin' in principal.roles",
                "only admins may set a high level",
            )],
        )]);

        let high = fields(&[("level", DynamicValue::Integer(5))]);

        // Non-admin caller is rejected.
        assert_eq!(
            check_requires(&schema, &high, Some(&claims(&["member"]))),
            Err(RuleError::Rejected(vec![
                "only admins may set a high level".to_string()
            ]))
        );

        // Admin caller passes.
        assert_eq!(
            check_requires(&schema, &high, Some(&claims(&["admin"]))),
            Ok(())
        );
    }

    #[test]
    fn principal_has_check_is_false_when_no_claims() {
        // `has(principal.sub)` must be a clean false (not an error) with no claims.
        let schema = schema_with(vec![text_field(
            "x",
            vec![require("has(principal.sub)", "must be authenticated")],
        )]);
        let f = fields(&[("x", DynamicValue::Integer(1))]);
        assert_eq!(
            check_requires(&schema, &f, None),
            Err(RuleError::Rejected(vec![
                "must be authenticated".to_string()
            ]))
        );
        // With claims, the same predicate passes.
        assert_eq!(
            check_requires(&schema, &f, Some(&claims(&[]))),
            Ok(())
        );
    }

    #[test]
    fn non_bool_predicate_is_eval_error() {
        let schema = schema_with(vec![text_field(
            "age",
            vec![require("age + 1", "nonsense")],
        )]);
        let f = fields(&[("age", DynamicValue::Integer(21))]);
        match check_requires(&schema, &f, None) {
            Err(RuleError::Eval { field, detail }) => {
                assert_eq!(field, "age");
                assert!(detail.contains("boolean"), "detail was: {detail}");
            }
            other => panic!("expected Eval error, got {other:?}"),
        }
    }

    #[test]
    fn erroring_predicate_is_eval_error() {
        // References an undeclared field → undeclared-reference eval error.
        let schema = schema_with(vec![text_field(
            "age",
            vec![require("missing_field > 0", "nonsense")],
        )]);
        let f = fields(&[("age", DynamicValue::Integer(21))]);
        match check_requires(&schema, &f, None) {
            Err(RuleError::Eval { field, detail }) => {
                assert_eq!(field, "age");
                assert!(!detail.is_empty());
            }
            other => panic!("expected Eval error, got {other:?}"),
        }
    }

    #[test]
    fn skipped_binding_fails_closed() {
        // A field that converts fine but an annotation references a field that
        // was never supplied → undeclared reference → Eval (fail-closed).
        let schema = schema_with(vec![text_field(
            "a",
            vec![require("b == 1", "needs b")],
        )]);
        let f = fields(&[("a", DynamicValue::Integer(1))]);
        assert!(matches!(
            check_requires(&schema, &f, None),
            Err(RuleError::Eval { .. })
        ));
    }

    // -- @compute (#93) --

    fn typed_field(
        name: &str,
        field_type: FieldType,
        annotations: Vec<FieldAnnotation>,
    ) -> FieldDefinition {
        FieldDefinition::with_annotations(
            FieldName::new(name).unwrap(),
            field_type,
            vec![],
            annotations,
        )
    }

    fn compute(expr: &str) -> FieldAnnotation {
        FieldAnnotation::Compute {
            expr: expr.to_string(),
        }
    }

    #[test]
    fn numeric_compute_stores_float_with_int_coercion() {
        // quantity * unit_price → Int product, coerced to the Float field.
        let schema = schema_with(vec![
            typed_field(
                "quantity",
                FieldType::Integer(
                    schema_forge_core::types::IntegerConstraints::unconstrained(),
                ),
                vec![],
            ),
            typed_field(
                "unit_price",
                FieldType::Integer(
                    schema_forge_core::types::IntegerConstraints::unconstrained(),
                ),
                vec![],
            ),
            typed_field(
                "total",
                FieldType::Float(FloatConstraints::unconstrained()),
                vec![compute("quantity * unit_price")],
            ),
        ]);
        let mut f = fields(&[
            ("quantity", DynamicValue::Integer(3)),
            ("unit_price", DynamicValue::Integer(7)),
        ]);
        assert_eq!(apply_computed(&schema, &mut f, None), Ok(()));
        assert_eq!(f.get("total"), Some(&DynamicValue::Float(21.0)));
    }

    #[test]
    fn string_concat_compute() {
        let schema = schema_with(vec![
            text_field("first", vec![]),
            text_field("last", vec![]),
            text_field("full_name", vec![compute("first + ' ' + last")]),
        ]);
        let mut f = fields(&[
            ("first", DynamicValue::Text("Ada".to_string())),
            ("last", DynamicValue::Text("Lovelace".to_string())),
        ]);
        assert_eq!(apply_computed(&schema, &mut f, None), Ok(()));
        assert_eq!(
            f.get("full_name"),
            Some(&DynamicValue::Text("Ada Lovelace".to_string()))
        );
    }

    #[test]
    fn compute_overwrites_client_supplied_value() {
        let schema = schema_with(vec![
            text_field("first", vec![]),
            text_field("last", vec![]),
            text_field("full_name", vec![compute("first + ' ' + last")]),
        ]);
        let mut f = fields(&[
            ("first", DynamicValue::Text("Ada".to_string())),
            ("last", DynamicValue::Text("Lovelace".to_string())),
            // Client tries to smuggle a value into the derived field.
            ("full_name", DynamicValue::Text("HACKED".to_string())),
        ]);
        assert_eq!(apply_computed(&schema, &mut f, None), Ok(()));
        assert_eq!(
            f.get("full_name"),
            Some(&DynamicValue::Text("Ada Lovelace".to_string()))
        );
    }

    #[test]
    fn chained_compute_sees_earlier_computed_value() {
        // `b` is computed from `a`, which is itself computed (and declared first).
        let schema = schema_with(vec![
            typed_field(
                "base",
                FieldType::Integer(
                    schema_forge_core::types::IntegerConstraints::unconstrained(),
                ),
                vec![],
            ),
            typed_field(
                "a",
                FieldType::Integer(
                    schema_forge_core::types::IntegerConstraints::unconstrained(),
                ),
                vec![compute("base + 1")],
            ),
            typed_field(
                "b",
                FieldType::Integer(
                    schema_forge_core::types::IntegerConstraints::unconstrained(),
                ),
                vec![compute("a * 10")],
            ),
        ]);
        let mut f = fields(&[("base", DynamicValue::Integer(4))]);
        assert_eq!(apply_computed(&schema, &mut f, None), Ok(()));
        assert_eq!(f.get("a"), Some(&DynamicValue::Integer(5)));
        // b must see a's freshly-computed value (5), not an undeclared ref.
        assert_eq!(f.get("b"), Some(&DynamicValue::Integer(50)));
    }

    #[test]
    fn principal_referencing_compute() {
        let schema = schema_with(vec![text_field(
            "created_by",
            vec![compute("principal.sub")],
        )]);
        let mut f = fields(&[]);
        assert_eq!(
            apply_computed(&schema, &mut f, Some(&claims(&[]))),
            Ok(())
        );
        assert_eq!(
            f.get("created_by"),
            Some(&DynamicValue::Text("user:alice".to_string()))
        );
    }

    #[test]
    fn eval_error_compute_is_eval() {
        let schema = schema_with(vec![text_field(
            "x",
            vec![compute("missing_field + 1")],
        )]);
        let mut f = fields(&[]);
        match apply_computed(&schema, &mut f, None) {
            Err(RuleError::Eval { field, detail }) => {
                assert_eq!(field, "x");
                assert!(!detail.is_empty());
            }
            other => panic!("expected Eval error, got {other:?}"),
        }
        // Nothing was stored for the failed field.
        assert!(!f.contains_key("x"));
    }

    #[test]
    fn enum_coercion_success() {
        let variants =
            EnumVariants::new(vec!["low".to_string(), "high".to_string()]).unwrap();
        let schema = schema_with(vec![
            text_field("level", vec![]),
            typed_field(
                "tier",
                FieldType::Enum(variants),
                vec![compute("level")],
            ),
        ]);
        let mut f = fields(&[("level", DynamicValue::Text("high".to_string()))]);
        assert_eq!(apply_computed(&schema, &mut f, None), Ok(()));
        assert_eq!(f.get("tier"), Some(&DynamicValue::Enum("high".to_string())));
    }

    #[test]
    fn enum_coercion_invalid_variant_is_eval() {
        let variants =
            EnumVariants::new(vec!["low".to_string(), "high".to_string()]).unwrap();
        let schema = schema_with(vec![
            text_field("level", vec![]),
            typed_field(
                "tier",
                FieldType::Enum(variants),
                vec![compute("level")],
            ),
        ]);
        let mut f = fields(&[("level", DynamicValue::Text("medium".to_string()))]);
        match apply_computed(&schema, &mut f, None) {
            Err(RuleError::Eval { field, detail }) => {
                assert_eq!(field, "tier");
                assert!(detail.contains("medium"), "detail was: {detail}");
                assert!(detail.contains("not a variant"), "detail was: {detail}");
            }
            other => panic!("expected Eval error, got {other:?}"),
        }
    }

    #[test]
    fn datetime_coercion_parses_rfc3339() {
        let schema = schema_with(vec![typed_field(
            "at",
            FieldType::DateTime,
            vec![compute("'2024-01-02T03:04:05Z'")],
        )]);
        let mut f = fields(&[]);
        assert_eq!(apply_computed(&schema, &mut f, None), Ok(()));
        match f.get("at") {
            Some(DynamicValue::DateTime(_)) => {}
            other => panic!("expected DateTime, got {other:?}"),
        }
    }
}
