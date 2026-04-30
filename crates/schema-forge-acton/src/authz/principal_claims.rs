//! Operator-defined principal claim → Cedar attribute mappings.
//!
//! The Cedar `Forge::Principal` entity ships with three intrinsic attributes
//! (`id`, `role_rank`, `roles`). Custom Cedar policies frequently need to
//! reach further — comparing the bearer's organisation, team membership, or
//! customer tier against fields on the resource. This module lets operators
//! expose arbitrary PASETO `custom` claims as additional attributes on the
//! generated principal.
//!
//! # Lifecycle
//!
//! 1. The CLI's `serve` command parses the `[schema_forge.authz.principal_claims]`
//!    TOML table into [`PrincipalClaimsConfig`].
//! 2. [`PrincipalClaimMappings::from_config`] validates names, types, and
//!    defaults, returning a runtime mapping. Validation errors abort startup —
//!    a misconfigured claim mapping is a hard error, identical to other
//!    config-time failures (role_ranks.toml, schema validation).
//! 3. [`PrincipalClaimMappings::cedar_schema_fragment`] is spliced into the
//!    `Forge::Principal` block of the generated Cedar schema. Every mapped
//!    attribute is emitted as **optional** so policies can guard with
//!    `principal has X && ...` and tokens that omit the claim still satisfy
//!    strict-mode validation. The strict-mode behaviour is pinned by the
//!    spike at `tests/cedar_optional_principal_attr_spike.rs`.
//! 4. At request time, [`PrincipalClaimMappings::extract_into`] reads the
//!    bearer's `Claims.custom` map and writes typed attributes onto the
//!    Cedar principal entity built by `build_principal_entities`.
//!
//! # Why optional in the schema, enforced in the adapter?
//!
//! Cedar 4.x treats absent optional attributes as "not present" — `has` works
//! as the guard. If a mapping declares `required = true`, that promise is
//! enforced at runtime: a token missing the claim raises
//! [`AdapterError::UnrepresentableValue`], which propagates to a 401 well
//! before any policy is evaluated. Encoding `required` directly in the Cedar
//! schema would force every absent claim into a strict-mode rejection of the
//! validator itself, which destroys the "load-shed missing claim" path.
//!
//! # Why not coerce mismatched types?
//!
//! A token claim whose JSON kind doesn't match the operator's declared type
//! (e.g. claim is JSON `42`, mapping says `string`) is rejected, never
//! coerced. The operator declared the contract; a violating token is an
//! integrity failure that should fail closed, not silently lose data.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write;

use acton_service::middleware::Claims;
use cedar_policy::RestrictedExpression;
use schema_forge_backend::Entity;
use schema_forge_core::types::{Cardinality, DynamicValue, FieldType, SchemaDefinition};
use serde::{Deserialize, Serialize};

use crate::authz::adapters::AdapterError;

/// Reserved attribute names already present on every `Forge::Principal`.
///
/// Operator-supplied mappings cannot collide with these — doing so would
/// shadow the intrinsic semantics (`role_rank` drives the no-upward-visibility
/// guard) and silently change generated-policy behaviour.
const RESERVED_ATTRIBUTE_NAMES: &[&str] = &["id", "role_rank", "roles"];

/// Cedar value type used to represent a mapped claim.
///
/// Restricted to the four shapes that already flow through
/// `RestrictedExpression::new_*` in `adapters.rs`. An `entity_ref` type
/// (mapping a string claim to a `Forge::Tenant`-style UID) is intentionally
/// out of scope for v1; see issue #50 for the deferred follow-up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrincipalClaimType {
    /// Cedar `String`. Source JSON must be a string.
    String,
    /// Cedar `Long`. Source JSON must be an integer.
    Long,
    /// Cedar `Bool`. Source JSON must be a boolean.
    Bool,
    /// Cedar `Set<String>`. Source JSON must be an array of strings.
    SetOfString,
}

impl PrincipalClaimType {
    /// Cedar schema fragment for this type (the right-hand side of `name?:`).
    fn cedar_type_fragment(self) -> &'static str {
        match self {
            Self::String => "String",
            Self::Long => "Long",
            Self::Bool => "Bool",
            Self::SetOfString => "Set<String>",
        }
    }
}

/// Raw TOML representation of a single principal-claim mapping.
///
/// The map key (TOML section name) is the Cedar attribute name; this struct
/// holds the rest of the row. `claim` defaults to the section name when
/// omitted, mirroring the common case where the token key matches the Cedar
/// attribute name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrincipalClaimConfigEntry {
    /// Token claim key. Defaults to the section name.
    #[serde(default)]
    pub claim: Option<String>,
    /// Declared Cedar type for the claim's value.
    #[serde(rename = "type")]
    pub claim_type: PrincipalClaimType,
    /// When `true`, a token missing this claim is rejected at request time
    /// with `AdapterError::UnrepresentableValue` (→ 401).
    #[serde(default)]
    pub required: bool,
    /// Optional fallback value used when `required = false` and the claim is
    /// absent. JSON kind must match `claim_type`; validated at config load.
    #[serde(default)]
    pub default: Option<serde_json::Value>,
    /// Optional IN-side source: when set, the claim is populated at login
    /// time by projecting a User entity field into the PASETO `custom` map.
    #[serde(default)]
    pub source: Option<PrincipalClaimSourceConfig>,
}

/// IN-side source for a principal claim (TOML config shape).
///
/// `source = { user_field = "<f>" }` reads the named column off the User
/// entity row at login time and projects it into the `custom` claim map per
/// the projection table in [`UserFieldProjection`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PrincipalClaimSourceConfig {
    /// Read the value from a column on the User entity row.
    UserField {
        /// Name of the User schema field whose value to project.
        user_field: String,
    },
}

/// Raw config map: section name → entry. This is what
/// `[schema_forge.authz.principal_claims]` deserializes into.
pub type PrincipalClaimsConfig = BTreeMap<String, PrincipalClaimConfigEntry>;

/// A single validated mapping.
#[derive(Debug, Clone, PartialEq)]
pub struct PrincipalClaimMapping {
    /// Cedar attribute name on `Forge::Principal`.
    pub attribute_name: String,
    /// Token claim key.
    pub claim_name: String,
    /// Declared Cedar type.
    pub claim_type: PrincipalClaimType,
    /// Whether absence is fatal.
    pub required: bool,
    /// Optional default applied when the claim is absent and `required` is false.
    pub default: Option<serde_json::Value>,
    /// Resolved IN-side source.
    ///
    /// `Some` when the operator declared `source = { user_field = ... }` and
    /// validation against the User schema succeeded. `None` for OUT-only
    /// mappings (the bearer is expected to supply the claim out-of-band).
    pub source: Option<ResolvedClaimSource>,
}

/// IN-side source resolved against a concrete User schema.
///
/// Built by [`PrincipalClaimMappings::resolve_user_field_sources`]; the
/// projection rule is computed once at startup and consumed at every
/// login/refresh by [`PrincipalClaimMappings::project_user_fields`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedClaimSource {
    /// Name of the User schema field whose value to project.
    pub user_field: String,
    /// How to project the field's `DynamicValue` into a JSON claim value.
    pub projection: UserFieldProjection,
}

/// Mapping from a User schema field's DSL type to its login-time projection
/// into a PASETO `custom` claim.
///
/// The vocabulary is fixed (no escape hatches): out-of-band field types
/// (richtext, json, file, datetime, enum, composite, integer arrays) are
/// rejected at config load to keep token serialisation canonical and the
/// IN-side / OUT-side contracts symmetric.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserFieldProjection {
    /// `text` → JSON string.
    TextToString,
    /// `integer` → JSON number (Cedar `Long`).
    IntegerToLong,
    /// `boolean` → JSON bool.
    BooleanToBool,
    /// `text[]` → JSON array of strings (Cedar `Set<String>`).
    TextArrayToSetOfString,
    /// `-> Target` (relation, one) → target entity id as JSON string.
    RelationOneToString,
    /// `-> Target[]` (relation, many) → JSON array of target ids.
    RelationManyToSetOfString,
}

/// Validated, deterministically ordered set of principal-claim mappings.
///
/// Ordering matters: `cedar_schema_fragment` emits attribute lines in name
/// order so the generated Cedar schema is stable byte-for-byte across
/// restarts (audit-log diff, content-addressed `policy_hash`).
///
/// Built in two phases:
///
/// 1. [`from_config`](Self::from_config) validates names, types, and defaults.
///    Raw `source` declarations are stored alongside each mapping but not
///    yet resolved against any schema.
/// 2. [`resolve_user_field_sources`](Self::resolve_user_field_sources) walks
///    every raw `source = { user_field = ... }` declaration and binds it to
///    a [`ResolvedClaimSource`]. Mismatches abort the daemon at startup.
///
/// The two-phase split exists because the `[schema_forge.authz.principal_claims]`
/// table is parsed before schemas are loaded; the User schema (which may have
/// been overridden via `@access(admin)`) only becomes available after
/// `seed_system_schemas_into_map` runs.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PrincipalClaimMappings {
    by_name: BTreeMap<String, PrincipalClaimMapping>,
    raw_sources: BTreeMap<String, PrincipalClaimSourceConfig>,
}

/// Errors raised while validating a principal-claim mapping config.
#[derive(Debug, thiserror::Error)]
pub enum PrincipalClaimsError {
    /// The Cedar attribute name collides with one of the intrinsic names.
    #[error(
        "principal claim attribute '{name}' collides with a reserved name \
         (id, role_rank, roles); pick a different name"
    )]
    ReservedName { name: String },
    /// The Cedar attribute name is not a Cedar identifier.
    #[error(
        "principal claim attribute '{name}' is not a valid Cedar identifier \
         (must match [A-Za-z_][A-Za-z0-9_]*)"
    )]
    InvalidName { name: String },
    /// The token claim key is empty.
    #[error("principal claim '{name}' has an empty token claim key")]
    EmptyClaimKey { name: String },
    /// The configured `default` value's JSON kind does not match the declared type.
    #[error(
        "principal claim '{name}': default value does not match declared type \
         {declared:?} (got: {actual})"
    )]
    DefaultTypeMismatch {
        name: String,
        declared: PrincipalClaimType,
        actual: String,
    },
    /// The configured `source.user_field` does not exist on the User schema.
    #[error(
        "principal claim '{name}': source.user_field references missing field \
         '{field}' on User schema"
    )]
    UnknownUserField { name: String, field: String },
    /// The User schema field's type cannot project to the declared claim type.
    #[error(
        "principal claim '{name}': source.user_field '{field}' has type {actual} \
         which cannot project to declared claim type {declared:?}"
    )]
    UserFieldTypeMismatch {
        name: String,
        field: String,
        actual: String,
        declared: PrincipalClaimType,
    },
    /// The User schema field is `@hidden`; refuse to leak its value into tokens.
    #[error(
        "principal claim '{name}': source.user_field '{field}' is @hidden; \
         refuse to leak hidden user data into tokens"
    )]
    HiddenUserField { name: String, field: String },
    /// The User schema field's type is not in the projectable vocabulary.
    #[error(
        "principal claim '{name}': source.user_field '{field}' has unsupported \
         type {actual} (richtext, json, file, datetime, enum, composite, and \
         integer arrays are not projectable)"
    )]
    UnprojectableFieldType {
        name: String,
        field: String,
        actual: String,
    },
    /// A `required` mapping's source field resolved to null/missing at login.
    ///
    /// Raised at request time, not config load. Surfaces through
    /// [`AdapterError::UnrepresentableValue`] when used by the IN-side path.
    #[error(
        "principal claim '{name}': required source.user_field '{field}' \
         resolved to null/missing on the user row"
    )]
    NullRequiredUserField { name: String, field: String },
}

impl PrincipalClaimMappings {
    /// Builds a validated mapping set from the raw TOML config map.
    ///
    /// Phase 1: validates names, types, and defaults. Raw `source` blocks
    /// are stored for later resolution by
    /// [`resolve_user_field_sources`](Self::resolve_user_field_sources).
    pub fn from_config(config: &PrincipalClaimsConfig) -> Result<Self, PrincipalClaimsError> {
        let mut by_name = BTreeMap::new();
        let mut raw_sources = BTreeMap::new();
        for (name, entry) in config {
            let attribute_name = name.clone();

            if RESERVED_ATTRIBUTE_NAMES.contains(&attribute_name.as_str()) {
                return Err(PrincipalClaimsError::ReservedName {
                    name: attribute_name,
                });
            }
            if !is_cedar_identifier(&attribute_name) {
                return Err(PrincipalClaimsError::InvalidName {
                    name: attribute_name,
                });
            }

            let claim_name = entry
                .claim
                .clone()
                .unwrap_or_else(|| attribute_name.clone());
            if claim_name.is_empty() {
                return Err(PrincipalClaimsError::EmptyClaimKey {
                    name: attribute_name,
                });
            }

            if let Some(default) = entry.default.as_ref() {
                if !json_matches_type(default, entry.claim_type) {
                    return Err(PrincipalClaimsError::DefaultTypeMismatch {
                        name: attribute_name,
                        declared: entry.claim_type,
                        actual: describe_json_kind(default),
                    });
                }
            }

            if let Some(source) = entry.source.clone() {
                raw_sources.insert(attribute_name.clone(), source);
            }

            by_name.insert(
                attribute_name.clone(),
                PrincipalClaimMapping {
                    attribute_name,
                    claim_name,
                    claim_type: entry.claim_type,
                    required: entry.required,
                    default: entry.default.clone(),
                    source: None,
                },
            );
        }
        Ok(Self {
            by_name,
            raw_sources,
        })
    }

    /// Phase 2: resolve every raw `source = { user_field = ... }` against the
    /// supplied User schema.
    ///
    /// Aborts on missing fields, `@hidden` fields, type-vocabulary violations,
    /// and projection-rule mismatches. After this returns `Ok`, every mapping
    /// that declared a `source` carries a [`ResolvedClaimSource`] usable by
    /// [`project_user_fields`](Self::project_user_fields).
    pub fn resolve_user_field_sources(
        &mut self,
        user_schema: &SchemaDefinition,
    ) -> Result<(), PrincipalClaimsError> {
        for (attr_name, source) in &self.raw_sources {
            let PrincipalClaimSourceConfig::UserField { user_field } = source;
            let mapping = self
                .by_name
                .get_mut(attr_name)
                .expect("raw source recorded for missing mapping; from_config invariant");

            let field_def = user_schema.field(user_field).ok_or_else(|| {
                PrincipalClaimsError::UnknownUserField {
                    name: attr_name.clone(),
                    field: user_field.clone(),
                }
            })?;

            if field_def.is_hidden() {
                return Err(PrincipalClaimsError::HiddenUserField {
                    name: attr_name.clone(),
                    field: user_field.clone(),
                });
            }

            let projection = resolve_projection(
                attr_name,
                user_field,
                &field_def.field_type,
                mapping.claim_type,
            )?;
            mapping.source = Some(ResolvedClaimSource {
                user_field: user_field.clone(),
                projection,
            });
        }
        Ok(())
    }

    /// True when at least one mapping declares a `source = { user_field }`.
    ///
    /// Used by the login/refresh handlers to decide whether to fetch the
    /// User entity row before building claims — when no IN-side source is
    /// configured the existing fast path (no entity lookup) still applies.
    pub fn has_user_field_sources(&self) -> bool {
        self.by_name.values().any(|m| m.source.is_some())
    }

    /// Project the User entity row's declared fields into a custom-claims map.
    ///
    /// Walks every mapping that has a [`ResolvedClaimSource`], reads the
    /// corresponding column off `user_entity`, and projects it into JSON per
    /// the [`UserFieldProjection`] rule. Mappings without a `source` are
    /// skipped (their values are bearer-supplied and OUT-only). Mappings with
    /// `required = true` whose source field is null/missing raise
    /// [`PrincipalClaimsError::NullRequiredUserField`].
    ///
    /// Pure: no I/O. Returns the JSON values keyed by the *claim name*
    /// (token key), suitable for `ClaimsBuilder::custom_claim`.
    pub fn project_user_fields(
        &self,
        user_entity: &Entity,
    ) -> Result<BTreeMap<String, serde_json::Value>, PrincipalClaimsError> {
        let mut out = BTreeMap::new();
        for mapping in self.by_name.values() {
            let Some(source) = mapping.source.as_ref() else {
                continue;
            };
            let raw = user_entity.fields.get(&source.user_field);
            let value = match raw {
                None | Some(DynamicValue::Null) => {
                    if mapping.required {
                        return Err(PrincipalClaimsError::NullRequiredUserField {
                            name: mapping.attribute_name.clone(),
                            field: source.user_field.clone(),
                        });
                    }
                    continue;
                }
                Some(v) => project_value(source.projection, v).ok_or_else(|| {
                    PrincipalClaimsError::UserFieldTypeMismatch {
                        name: mapping.attribute_name.clone(),
                        field: source.user_field.clone(),
                        actual: describe_dynamic_kind(v),
                        declared: mapping.claim_type,
                    }
                })?,
            };
            out.insert(mapping.claim_name.clone(), value);
        }
        Ok(out)
    }

    /// True when no operator mappings are configured.
    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }

    /// Ordered iterator over the mappings. Stable across runs.
    pub fn iter(&self) -> impl Iterator<Item = &PrincipalClaimMapping> {
        self.by_name.values()
    }

    /// Cedar schema attribute lines for `Forge::Principal`.
    ///
    /// Each line is fully-formed: `    "name"?: Type,\n`. Splice into the
    /// `entity Principal` body after the intrinsic attributes. Empty mappings
    /// produce an empty string so the surrounding generator can stay byte-
    /// identical to the pre-feature output.
    pub fn cedar_schema_fragment(&self) -> String {
        let mut out = String::new();
        for mapping in self.by_name.values() {
            // unwrap: writing into a String never fails.
            writeln!(
                &mut out,
                "    \"{}\"?: {},",
                mapping.attribute_name,
                mapping.claim_type.cedar_type_fragment()
            )
            .unwrap();
        }
        out
    }

    /// Populates `attrs` with the Cedar `RestrictedExpression` for every
    /// mapping whose claim is present (or has a default).
    ///
    /// Returns `AdapterError::UnrepresentableValue` when a `required` mapping's
    /// claim is missing, or when a present claim's JSON kind doesn't match the
    /// declared type. Optional claims with no default and no token entry are
    /// simply omitted from `attrs` — the Cedar attribute stays absent and
    /// `principal has X` returns false.
    pub fn extract_into(
        &self,
        claims: &Claims,
        attrs: &mut HashMap<String, RestrictedExpression>,
    ) -> Result<(), AdapterError> {
        for mapping in self.by_name.values() {
            let raw = claims
                .custom
                .get(&mapping.claim_name)
                .or(mapping.default.as_ref());

            let value = match raw {
                Some(v) => v,
                None => {
                    if mapping.required {
                        return Err(AdapterError::UnrepresentableValue {
                            field: format!("principal.{}", mapping.attribute_name),
                            detail: format!(
                                "required claim '{}' missing from token",
                                mapping.claim_name
                            ),
                        });
                    }
                    continue;
                }
            };

            let expr = build_expression(mapping, value)?;
            attrs.insert(mapping.attribute_name.clone(), expr);
        }
        Ok(())
    }
}

/// Builds a `RestrictedExpression` from a JSON value, type-checking against
/// the mapping's declared type.
fn build_expression(
    mapping: &PrincipalClaimMapping,
    value: &serde_json::Value,
) -> Result<RestrictedExpression, AdapterError> {
    let mismatch = || AdapterError::UnrepresentableValue {
        field: format!("principal.{}", mapping.attribute_name),
        detail: format!(
            "claim '{}' has JSON kind {} but mapping declares type {:?}",
            mapping.claim_name,
            describe_json_kind(value),
            mapping.claim_type
        ),
    };

    match (mapping.claim_type, value) {
        (PrincipalClaimType::String, serde_json::Value::String(s)) => {
            Ok(RestrictedExpression::new_string(s.clone()))
        }
        (PrincipalClaimType::Long, serde_json::Value::Number(n)) => {
            let i = n.as_i64().ok_or_else(mismatch)?;
            Ok(RestrictedExpression::new_long(i))
        }
        (PrincipalClaimType::Bool, serde_json::Value::Bool(b)) => {
            Ok(RestrictedExpression::new_bool(*b))
        }
        (PrincipalClaimType::SetOfString, serde_json::Value::Array(items)) => {
            let mut elements = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    serde_json::Value::String(s) => {
                        elements.push(RestrictedExpression::new_string(s.clone()));
                    }
                    other => {
                        return Err(AdapterError::UnrepresentableValue {
                            field: format!("principal.{}", mapping.attribute_name),
                            detail: format!(
                                "claim '{}' set element has JSON kind {} but mapping requires \
                                 strings",
                                mapping.claim_name,
                                describe_json_kind(other),
                            ),
                        });
                    }
                }
            }
            Ok(RestrictedExpression::new_set(elements))
        }
        _ => Err(mismatch()),
    }
}

/// Cedar identifier rule: ASCII letter / underscore start, then letters,
/// digits, or underscores. Mirrors the lexer in `cedar_policy_core::ast`.
fn is_cedar_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c == '_' || c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

fn describe_json_kind(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Null => "null".into(),
        serde_json::Value::Bool(_) => "bool".into(),
        serde_json::Value::Number(_) => "number".into(),
        serde_json::Value::String(_) => "string".into(),
        serde_json::Value::Array(_) => "array".into(),
        serde_json::Value::Object(_) => "object".into(),
    }
}

/// Pure: resolve a User schema field type + declared claim type pair to a
/// [`UserFieldProjection`]. Errors when the pair isn't in the projection
/// vocabulary or the field type doesn't match the declared claim type.
fn resolve_projection(
    claim_name: &str,
    field_name: &str,
    field_type: &FieldType,
    declared: PrincipalClaimType,
) -> Result<UserFieldProjection, PrincipalClaimsError> {
    let projection = match (field_type, declared) {
        (FieldType::Text(_), PrincipalClaimType::String) => UserFieldProjection::TextToString,
        (FieldType::Integer(_), PrincipalClaimType::Long) => UserFieldProjection::IntegerToLong,
        (FieldType::Boolean, PrincipalClaimType::Bool) => UserFieldProjection::BooleanToBool,
        (FieldType::Array(inner), PrincipalClaimType::SetOfString) => match inner.as_ref() {
            FieldType::Text(_) => UserFieldProjection::TextArrayToSetOfString,
            FieldType::Relation {
                cardinality: Cardinality::One,
                ..
            } => UserFieldProjection::RelationManyToSetOfString,
            _ => {
                return Err(PrincipalClaimsError::UnprojectableFieldType {
                    name: claim_name.to_string(),
                    field: field_name.to_string(),
                    actual: field_type.to_string(),
                });
            }
        },
        (
            FieldType::Relation {
                cardinality: Cardinality::One,
                ..
            },
            PrincipalClaimType::String,
        ) => UserFieldProjection::RelationOneToString,
        (
            FieldType::Relation {
                cardinality: Cardinality::Many,
                ..
            },
            PrincipalClaimType::SetOfString,
        ) => UserFieldProjection::RelationManyToSetOfString,
        (
            FieldType::RichText
            | FieldType::Float(_)
            | FieldType::DateTime
            | FieldType::Enum(_)
            | FieldType::Json
            | FieldType::Composite(_)
            | FieldType::File(_),
            _,
        ) => {
            return Err(PrincipalClaimsError::UnprojectableFieldType {
                name: claim_name.to_string(),
                field: field_name.to_string(),
                actual: field_type.to_string(),
            });
        }
        (FieldType::Array(_), _) => {
            return Err(PrincipalClaimsError::UnprojectableFieldType {
                name: claim_name.to_string(),
                field: field_name.to_string(),
                actual: field_type.to_string(),
            });
        }
        _ => {
            return Err(PrincipalClaimsError::UserFieldTypeMismatch {
                name: claim_name.to_string(),
                field: field_name.to_string(),
                actual: field_type.to_string(),
                declared,
            });
        }
    };
    Ok(projection)
}

/// Pure: project a `DynamicValue` to a JSON value per the resolved rule.
///
/// Returns `None` when the runtime value's kind doesn't match the
/// projection's expected input kind (a `@hidden`-violation-like state that
/// should never occur if startup validation succeeded, but the handler
/// surfaces it as a clean adapter error rather than panicking).
fn project_value(
    projection: UserFieldProjection,
    value: &DynamicValue,
) -> Option<serde_json::Value> {
    match (projection, value) {
        (UserFieldProjection::TextToString, DynamicValue::Text(s)) => {
            Some(serde_json::Value::String(s.clone()))
        }
        (UserFieldProjection::IntegerToLong, DynamicValue::Integer(i)) => {
            Some(serde_json::Value::Number((*i).into()))
        }
        (UserFieldProjection::BooleanToBool, DynamicValue::Boolean(b)) => {
            Some(serde_json::Value::Bool(*b))
        }
        (UserFieldProjection::TextArrayToSetOfString, DynamicValue::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    DynamicValue::Text(s) => out.push(serde_json::Value::String(s.clone())),
                    _ => return None,
                }
            }
            Some(serde_json::Value::Array(out))
        }
        (UserFieldProjection::RelationOneToString, DynamicValue::Ref(id)) => {
            Some(serde_json::Value::String(id.as_str().to_string()))
        }
        (UserFieldProjection::RelationManyToSetOfString, DynamicValue::RefArray(ids)) => Some(
            serde_json::Value::Array(
                ids.iter()
                    .map(|id| serde_json::Value::String(id.as_str().to_string()))
                    .collect(),
            ),
        ),
        // text[] declared as relation array (One inside Array) is handled
        // when the storage layer materialises Refs as a RefArray.
        (UserFieldProjection::RelationManyToSetOfString, DynamicValue::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    DynamicValue::Ref(id) => {
                        out.push(serde_json::Value::String(id.as_str().to_string()));
                    }
                    _ => return None,
                }
            }
            Some(serde_json::Value::Array(out))
        }
        _ => None,
    }
}

fn describe_dynamic_kind(v: &DynamicValue) -> String {
    match v {
        DynamicValue::Null => "null".into(),
        DynamicValue::Text(_) => "text".into(),
        DynamicValue::Integer(_) => "integer".into(),
        DynamicValue::Float(_) => "float".into(),
        DynamicValue::Boolean(_) => "boolean".into(),
        DynamicValue::DateTime(_) => "datetime".into(),
        DynamicValue::Enum(_) => "enum".into(),
        DynamicValue::Json(_) => "json".into(),
        DynamicValue::Array(_) => "array".into(),
        DynamicValue::Composite(_) => "composite".into(),
        DynamicValue::Ref(_) => "ref".into(),
        DynamicValue::RefArray(_) => "ref_array".into(),
        _ => "unknown".into(),
    }
}

fn json_matches_type(v: &serde_json::Value, t: PrincipalClaimType) -> bool {
    match (t, v) {
        (PrincipalClaimType::String, serde_json::Value::String(_)) => true,
        (PrincipalClaimType::Long, serde_json::Value::Number(n)) => n.is_i64(),
        (PrincipalClaimType::Bool, serde_json::Value::Bool(_)) => true,
        (PrincipalClaimType::SetOfString, serde_json::Value::Array(items)) => {
            items.iter().all(|i| matches!(i, serde_json::Value::String(_)))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use acton_service::middleware::Claims;
    use std::collections::HashMap;

    fn entry(t: PrincipalClaimType) -> PrincipalClaimConfigEntry {
        PrincipalClaimConfigEntry {
            claim: None,
            claim_type: t,
            required: false,
            default: None,
            source: None,
        }
    }

    fn config(pairs: &[(&str, PrincipalClaimConfigEntry)]) -> PrincipalClaimsConfig {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect()
    }

    fn mapping_set(pairs: &[(&str, PrincipalClaimConfigEntry)]) -> PrincipalClaimMappings {
        PrincipalClaimMappings::from_config(&config(pairs)).unwrap()
    }

    fn make_claims(custom: HashMap<String, serde_json::Value>) -> Claims {
        Claims {
            sub: "user:test".into(),
            email: None,
            username: None,
            roles: vec![],
            perms: vec![],
            exp: 9_999_999_999,
            iat: None,
            jti: None,
            iss: None,
            aud: None,
            custom,
        }
    }

    #[test]
    fn from_config_rejects_reserved_names() {
        for reserved in RESERVED_ATTRIBUTE_NAMES {
            let cfg = config(&[(*reserved, entry(PrincipalClaimType::String))]);
            let err = PrincipalClaimMappings::from_config(&cfg).unwrap_err();
            assert!(matches!(err, PrincipalClaimsError::ReservedName { .. }));
        }
    }

    #[test]
    fn from_config_rejects_non_identifier_names() {
        for bad in ["my-key", "1leading", "with space", "", "🦀"] {
            let cfg = config(&[(bad, entry(PrincipalClaimType::String))]);
            let err = PrincipalClaimMappings::from_config(&cfg).unwrap_err();
            assert!(
                matches!(err, PrincipalClaimsError::InvalidName { .. }),
                "expected InvalidName for {bad:?}, got {err:?}"
            );
        }
    }

    #[test]
    fn from_config_accepts_identifier_names() {
        for ok in ["org_id", "_internal", "ClientOrg42"] {
            let cfg = config(&[(ok, entry(PrincipalClaimType::String))]);
            assert!(PrincipalClaimMappings::from_config(&cfg).is_ok());
        }
    }

    #[test]
    fn from_config_rejects_empty_claim_key() {
        let mut e = entry(PrincipalClaimType::String);
        e.claim = Some(String::new());
        let cfg = config(&[("client_org_id", e)]);
        let err = PrincipalClaimMappings::from_config(&cfg).unwrap_err();
        assert!(matches!(err, PrincipalClaimsError::EmptyClaimKey { .. }));
    }

    #[test]
    fn from_config_defaults_claim_to_section_name() {
        let cfg = config(&[("org_id", entry(PrincipalClaimType::String))]);
        let m = PrincipalClaimMappings::from_config(&cfg).unwrap();
        assert_eq!(m.by_name["org_id"].claim_name, "org_id");
    }

    #[test]
    fn from_config_rejects_default_type_mismatch() {
        let mut e = entry(PrincipalClaimType::Long);
        e.default = Some(serde_json::json!("not a number"));
        let cfg = config(&[("level", e)]);
        let err = PrincipalClaimMappings::from_config(&cfg).unwrap_err();
        assert!(matches!(
            err,
            PrincipalClaimsError::DefaultTypeMismatch { .. }
        ));
    }

    #[test]
    fn from_config_accepts_well_typed_defaults() {
        let cases = [
            (PrincipalClaimType::String, serde_json::json!("ok")),
            (PrincipalClaimType::Long, serde_json::json!(42)),
            (PrincipalClaimType::Bool, serde_json::json!(true)),
            (
                PrincipalClaimType::SetOfString,
                serde_json::json!(["a", "b"]),
            ),
        ];
        for (t, default) in cases {
            let mut e = entry(t);
            e.default = Some(default);
            let cfg = config(&[("k", e)]);
            assert!(PrincipalClaimMappings::from_config(&cfg).is_ok());
        }
    }

    #[test]
    fn cedar_schema_fragment_is_deterministic_and_optional() {
        let mappings = mapping_set(&[
            ("zeta", entry(PrincipalClaimType::Bool)),
            ("alpha", entry(PrincipalClaimType::String)),
            ("beta", entry(PrincipalClaimType::SetOfString)),
        ]);
        let fragment = mappings.cedar_schema_fragment();
        let lines: Vec<&str> = fragment.lines().collect();
        assert_eq!(
            lines,
            vec![
                "    \"alpha\"?: String,",
                "    \"beta\"?: Set<String>,",
                "    \"zeta\"?: Bool,",
            ]
        );
    }

    #[test]
    fn cedar_schema_fragment_empty_when_no_mappings() {
        let mappings = PrincipalClaimMappings::default();
        assert!(mappings.cedar_schema_fragment().is_empty());
    }

    #[test]
    fn extract_into_writes_string_when_present() {
        let mappings = mapping_set(&[("client_org_id", entry(PrincipalClaimType::String))]);
        let claims = make_claims(HashMap::from([(
            "client_org_id".into(),
            serde_json::json!("org-42"),
        )]));
        let mut attrs = HashMap::new();
        mappings.extract_into(&claims, &mut attrs).unwrap();
        assert!(attrs.contains_key("client_org_id"));
    }

    #[test]
    fn extract_into_writes_long_from_number() {
        let mappings = mapping_set(&[("level", entry(PrincipalClaimType::Long))]);
        let claims = make_claims(HashMap::from([("level".into(), serde_json::json!(7))]));
        let mut attrs = HashMap::new();
        mappings.extract_into(&claims, &mut attrs).unwrap();
        assert!(attrs.contains_key("level"));
    }

    #[test]
    fn extract_into_writes_set_from_string_array() {
        let mappings = mapping_set(&[("team_ids", entry(PrincipalClaimType::SetOfString))]);
        let claims = make_claims(HashMap::from([(
            "team_ids".into(),
            serde_json::json!(["t-1", "t-2"]),
        )]));
        let mut attrs = HashMap::new();
        mappings.extract_into(&claims, &mut attrs).unwrap();
        assert!(attrs.contains_key("team_ids"));
    }

    #[test]
    fn extract_into_omits_optional_when_absent_with_no_default() {
        let mappings = mapping_set(&[("client_org_id", entry(PrincipalClaimType::String))]);
        let claims = make_claims(HashMap::new());
        let mut attrs = HashMap::new();
        mappings.extract_into(&claims, &mut attrs).unwrap();
        assert!(!attrs.contains_key("client_org_id"));
    }

    #[test]
    fn extract_into_uses_default_when_claim_absent() {
        let mut e = entry(PrincipalClaimType::String);
        e.default = Some(serde_json::json!("fallback"));
        let mappings = mapping_set(&[("client_org_id", e)]);
        let claims = make_claims(HashMap::new());
        let mut attrs = HashMap::new();
        mappings.extract_into(&claims, &mut attrs).unwrap();
        assert!(attrs.contains_key("client_org_id"));
    }

    #[test]
    fn extract_into_errors_when_required_claim_missing() {
        let mut e = entry(PrincipalClaimType::String);
        e.required = true;
        let mappings = mapping_set(&[("client_org_id", e)]);
        let claims = make_claims(HashMap::new());
        let mut attrs = HashMap::new();
        let err = mappings.extract_into(&claims, &mut attrs).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("client_org_id") && msg.contains("required"),
            "expected required-claim error, got: {msg}"
        );
    }

    #[test]
    fn extract_into_errors_on_type_mismatch() {
        let mappings = mapping_set(&[("level", entry(PrincipalClaimType::Long))]);
        let claims = make_claims(HashMap::from([(
            "level".into(),
            serde_json::json!("not a number"),
        )]));
        let mut attrs = HashMap::new();
        let err = mappings.extract_into(&claims, &mut attrs).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("level") && msg.contains("Long"),
            "expected type-mismatch error, got: {msg}"
        );
    }

    #[test]
    fn extract_into_errors_on_mixed_set_elements() {
        let mappings = mapping_set(&[("team_ids", entry(PrincipalClaimType::SetOfString))]);
        let claims = make_claims(HashMap::from([(
            "team_ids".into(),
            serde_json::json!(["a", 7]),
        )]));
        let mut attrs = HashMap::new();
        let err = mappings.extract_into(&claims, &mut attrs).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("team_ids"), "expected team_ids error, got: {msg}");
    }

    #[test]
    fn empty_mappings_is_no_op() {
        let mappings = PrincipalClaimMappings::default();
        let claims = make_claims(HashMap::from([(
            "client_org_id".into(),
            serde_json::json!("org-42"),
        )]));
        let mut attrs = HashMap::new();
        mappings.extract_into(&claims, &mut attrs).unwrap();
        assert!(attrs.is_empty());
    }

    // ---------------------------------------------------------------------
    // IN-side: resolve_user_field_sources + project_user_fields
    //
    // Pin every cell of the projection table from the issue spec:
    //   text          → string
    //   integer       → long
    //   boolean       → bool
    //   text[]        → set_of_string
    //   -> Target     → string  (target id)
    //   -> Target[]   → set_of_string
    // and reject every type outside that vocabulary at config load.
    // ---------------------------------------------------------------------

    use schema_forge_core::types::{
        Annotation, Cardinality, EnumVariants, FieldAnnotation, FieldDefinition, FieldName,
        FieldType, FileAccess, FileConstraints, FloatConstraints, IntegerConstraints, SchemaId,
        SchemaName, TextConstraints,
    };
    use schema_forge_backend::Entity;

    fn user_schema_with(fields: Vec<FieldDefinition>) -> SchemaDefinition {
        SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("User").unwrap(),
            fields,
            vec![Annotation::System],
        )
        .unwrap()
    }

    fn fd(name: &str, ft: FieldType) -> FieldDefinition {
        FieldDefinition::new(FieldName::new(name).unwrap(), ft)
    }

    fn fd_hidden(name: &str, ft: FieldType) -> FieldDefinition {
        FieldDefinition::with_annotations(
            FieldName::new(name).unwrap(),
            ft,
            vec![],
            vec![FieldAnnotation::Hidden],
        )
    }

    fn config_with_user_field(
        attr: &str,
        t: PrincipalClaimType,
        user_field: &str,
    ) -> PrincipalClaimsConfig {
        let mut cfg = PrincipalClaimsConfig::new();
        cfg.insert(
            attr.to_string(),
            PrincipalClaimConfigEntry {
                claim: None,
                claim_type: t,
                required: false,
                default: None,
                source: Some(PrincipalClaimSourceConfig::UserField {
                    user_field: user_field.to_string(),
                }),
            },
        );
        cfg
    }

    fn entity_with(fields: &[(&str, DynamicValue)]) -> Entity {
        let mut map = std::collections::BTreeMap::new();
        for (k, v) in fields {
            map.insert((*k).to_string(), v.clone());
        }
        Entity::new(SchemaName::new("User").unwrap(), map)
    }

    #[test]
    fn resolve_rejects_unknown_user_field() {
        let user = user_schema_with(vec![fd(
            "email",
            FieldType::Text(TextConstraints::unconstrained()),
        )]);
        let cfg = config_with_user_field("client_org_id", PrincipalClaimType::String, "missing");
        let mut mappings = PrincipalClaimMappings::from_config(&cfg).unwrap();
        let err = mappings.resolve_user_field_sources(&user).unwrap_err();
        assert!(matches!(err, PrincipalClaimsError::UnknownUserField { .. }));
    }

    #[test]
    fn resolve_rejects_hidden_user_field() {
        let user = user_schema_with(vec![fd_hidden(
            "password_hash",
            FieldType::Text(TextConstraints::unconstrained()),
        )]);
        let cfg = config_with_user_field("client_org_id", PrincipalClaimType::String, "password_hash");
        let mut mappings = PrincipalClaimMappings::from_config(&cfg).unwrap();
        let err = mappings.resolve_user_field_sources(&user).unwrap_err();
        assert!(matches!(err, PrincipalClaimsError::HiddenUserField { .. }));
    }

    #[test]
    fn resolve_rejects_richtext_json_file_datetime_enum_composite() {
        let bad = [
            ("rt", FieldType::RichText),
            ("js", FieldType::Json),
            ("dt", FieldType::DateTime),
            (
                "fl",
                FieldType::Float(FloatConstraints::unconstrained()),
            ),
            (
                "en",
                FieldType::Enum(EnumVariants::new(vec!["A".into(), "B".into()]).unwrap()),
            ),
            (
                "fi",
                FieldType::File(FileConstraints {
                    bucket: "b".into(),
                    max_size_bytes: 1,
                    mime_allowlist: vec![],
                    access: FileAccess::Presigned,
                }),
            ),
            ("co", FieldType::Composite(vec![])),
        ];
        for (name, ft) in bad {
            let user = user_schema_with(vec![fd(name, ft.clone())]);
            let cfg =
                config_with_user_field("client_org_id", PrincipalClaimType::String, name);
            let mut mappings = PrincipalClaimMappings::from_config(&cfg).unwrap();
            let err = mappings.resolve_user_field_sources(&user).unwrap_err();
            assert!(
                matches!(err, PrincipalClaimsError::UnprojectableFieldType { .. }),
                "expected UnprojectableFieldType for {ft:?}, got {err:?}"
            );
        }
    }

    #[test]
    fn resolve_rejects_integer_array() {
        let user = user_schema_with(vec![fd(
            "scores",
            FieldType::Array(Box::new(FieldType::Integer(
                IntegerConstraints::unconstrained(),
            ))),
        )]);
        let cfg = config_with_user_field("scores", PrincipalClaimType::SetOfString, "scores");
        let mut mappings = PrincipalClaimMappings::from_config(&cfg).unwrap();
        let err = mappings.resolve_user_field_sources(&user).unwrap_err();
        assert!(matches!(
            err,
            PrincipalClaimsError::UnprojectableFieldType { .. }
        ));
    }

    #[test]
    fn resolve_rejects_text_declared_as_long() {
        let user = user_schema_with(vec![fd(
            "tier",
            FieldType::Text(TextConstraints::unconstrained()),
        )]);
        let cfg = config_with_user_field("tier", PrincipalClaimType::Long, "tier");
        let mut mappings = PrincipalClaimMappings::from_config(&cfg).unwrap();
        let err = mappings.resolve_user_field_sources(&user).unwrap_err();
        assert!(matches!(
            err,
            PrincipalClaimsError::UserFieldTypeMismatch { .. }
        ));
    }

    #[test]
    fn resolve_text_to_string() {
        let user = user_schema_with(vec![fd(
            "client_org_id",
            FieldType::Text(TextConstraints::unconstrained()),
        )]);
        let cfg =
            config_with_user_field("client_org_id", PrincipalClaimType::String, "client_org_id");
        let mut mappings = PrincipalClaimMappings::from_config(&cfg).unwrap();
        mappings.resolve_user_field_sources(&user).unwrap();
        let m = &mappings.by_name["client_org_id"];
        assert_eq!(
            m.source.as_ref().unwrap().projection,
            UserFieldProjection::TextToString
        );
    }

    #[test]
    fn resolve_integer_to_long() {
        let user = user_schema_with(vec![fd(
            "tier",
            FieldType::Integer(IntegerConstraints::unconstrained()),
        )]);
        let cfg = config_with_user_field("tier", PrincipalClaimType::Long, "tier");
        let mut mappings = PrincipalClaimMappings::from_config(&cfg).unwrap();
        mappings.resolve_user_field_sources(&user).unwrap();
        assert_eq!(
            mappings.by_name["tier"].source.as_ref().unwrap().projection,
            UserFieldProjection::IntegerToLong
        );
    }

    #[test]
    fn resolve_boolean_to_bool() {
        let user = user_schema_with(vec![fd("vip", FieldType::Boolean)]);
        let cfg = config_with_user_field("vip", PrincipalClaimType::Bool, "vip");
        let mut mappings = PrincipalClaimMappings::from_config(&cfg).unwrap();
        mappings.resolve_user_field_sources(&user).unwrap();
        assert_eq!(
            mappings.by_name["vip"].source.as_ref().unwrap().projection,
            UserFieldProjection::BooleanToBool
        );
    }

    #[test]
    fn resolve_text_array_to_set() {
        let user = user_schema_with(vec![fd(
            "regions",
            FieldType::Array(Box::new(FieldType::Text(TextConstraints::unconstrained()))),
        )]);
        let cfg = config_with_user_field("regions", PrincipalClaimType::SetOfString, "regions");
        let mut mappings = PrincipalClaimMappings::from_config(&cfg).unwrap();
        mappings.resolve_user_field_sources(&user).unwrap();
        assert_eq!(
            mappings.by_name["regions"]
                .source
                .as_ref()
                .unwrap()
                .projection,
            UserFieldProjection::TextArrayToSetOfString
        );
    }

    #[test]
    fn resolve_relation_one_to_string() {
        let user = user_schema_with(vec![fd(
            "client_org",
            FieldType::Relation {
                target: SchemaName::new("ClientOrg").unwrap(),
                cardinality: Cardinality::One,
            },
        )]);
        let cfg = config_with_user_field("client_org", PrincipalClaimType::String, "client_org");
        let mut mappings = PrincipalClaimMappings::from_config(&cfg).unwrap();
        mappings.resolve_user_field_sources(&user).unwrap();
        assert_eq!(
            mappings.by_name["client_org"]
                .source
                .as_ref()
                .unwrap()
                .projection,
            UserFieldProjection::RelationOneToString
        );
    }

    #[test]
    fn resolve_relation_many_to_set() {
        let user = user_schema_with(vec![fd(
            "teams",
            FieldType::Relation {
                target: SchemaName::new("Team").unwrap(),
                cardinality: Cardinality::Many,
            },
        )]);
        let cfg = config_with_user_field("teams", PrincipalClaimType::SetOfString, "teams");
        let mut mappings = PrincipalClaimMappings::from_config(&cfg).unwrap();
        mappings.resolve_user_field_sources(&user).unwrap();
        assert_eq!(
            mappings.by_name["teams"]
                .source
                .as_ref()
                .unwrap()
                .projection,
            UserFieldProjection::RelationManyToSetOfString
        );
    }

    #[test]
    fn project_text_value_emits_string() {
        let user = user_schema_with(vec![fd(
            "client_org_id",
            FieldType::Text(TextConstraints::unconstrained()),
        )]);
        let mut mappings = PrincipalClaimMappings::from_config(&config_with_user_field(
            "client_org_id",
            PrincipalClaimType::String,
            "client_org_id",
        ))
        .unwrap();
        mappings.resolve_user_field_sources(&user).unwrap();
        let entity = entity_with(&[("client_org_id", DynamicValue::Text("org-42".into()))]);
        let out = mappings.project_user_fields(&entity).unwrap();
        assert_eq!(out["client_org_id"], serde_json::json!("org-42"));
    }

    #[test]
    fn project_text_array_emits_set() {
        let user = user_schema_with(vec![fd(
            "regions",
            FieldType::Array(Box::new(FieldType::Text(TextConstraints::unconstrained()))),
        )]);
        let mut mappings = PrincipalClaimMappings::from_config(&config_with_user_field(
            "regions",
            PrincipalClaimType::SetOfString,
            "regions",
        ))
        .unwrap();
        mappings.resolve_user_field_sources(&user).unwrap();
        let entity = entity_with(&[(
            "regions",
            DynamicValue::Array(vec![
                DynamicValue::Text("us".into()),
                DynamicValue::Text("eu".into()),
            ]),
        )]);
        let out = mappings.project_user_fields(&entity).unwrap();
        assert_eq!(out["regions"], serde_json::json!(["us", "eu"]));
    }

    #[test]
    fn project_relation_one_emits_target_id() {
        use schema_forge_core::types::EntityId;
        let user = user_schema_with(vec![fd(
            "client_org",
            FieldType::Relation {
                target: SchemaName::new("ClientOrg").unwrap(),
                cardinality: Cardinality::One,
            },
        )]);
        let mut mappings = PrincipalClaimMappings::from_config(&config_with_user_field(
            "client_org",
            PrincipalClaimType::String,
            "client_org",
        ))
        .unwrap();
        mappings.resolve_user_field_sources(&user).unwrap();
        let target_id = EntityId::new("clientorg");
        let target_str = target_id.as_str().to_string();
        let entity = entity_with(&[("client_org", DynamicValue::Ref(target_id))]);
        let out = mappings.project_user_fields(&entity).unwrap();
        assert_eq!(out["client_org"], serde_json::Value::String(target_str));
    }

    #[test]
    fn project_required_null_errors_with_null_required_user_field() {
        let user = user_schema_with(vec![fd(
            "client_org_id",
            FieldType::Text(TextConstraints::unconstrained()),
        )]);
        let mut cfg = config_with_user_field(
            "client_org_id",
            PrincipalClaimType::String,
            "client_org_id",
        );
        cfg.get_mut("client_org_id").unwrap().required = true;
        let mut mappings = PrincipalClaimMappings::from_config(&cfg).unwrap();
        mappings.resolve_user_field_sources(&user).unwrap();
        let entity = entity_with(&[("client_org_id", DynamicValue::Null)]);
        let err = mappings.project_user_fields(&entity).unwrap_err();
        assert!(matches!(
            err,
            PrincipalClaimsError::NullRequiredUserField { .. }
        ));
    }

    #[test]
    fn project_optional_null_skips_quietly() {
        let user = user_schema_with(vec![fd(
            "client_org_id",
            FieldType::Text(TextConstraints::unconstrained()),
        )]);
        let mut mappings = PrincipalClaimMappings::from_config(&config_with_user_field(
            "client_org_id",
            PrincipalClaimType::String,
            "client_org_id",
        ))
        .unwrap();
        mappings.resolve_user_field_sources(&user).unwrap();
        let entity = entity_with(&[("client_org_id", DynamicValue::Null)]);
        let out = mappings.project_user_fields(&entity).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn has_user_field_sources_reflects_resolved_state() {
        let user = user_schema_with(vec![fd(
            "client_org_id",
            FieldType::Text(TextConstraints::unconstrained()),
        )]);
        let mut mappings = PrincipalClaimMappings::from_config(&config_with_user_field(
            "client_org_id",
            PrincipalClaimType::String,
            "client_org_id",
        ))
        .unwrap();
        // before resolve: source is None on the mapping struct
        assert!(!mappings.has_user_field_sources());
        mappings.resolve_user_field_sources(&user).unwrap();
        assert!(mappings.has_user_field_sources());
    }
}
