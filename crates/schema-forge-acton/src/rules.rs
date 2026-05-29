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
use schema_forge_cel::{dynamic_to_cel, CelKey, CelValue};
use schema_forge_core::types::{DynamicValue, FieldAnnotation, SchemaDefinition};

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

#[cfg(test)]
mod tests {
    use super::*;
    use schema_forge_core::types::{
        FieldDefinition, FieldName, FieldType, SchemaId, SchemaName, TextConstraints,
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
}
