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
}

/// Validated, deterministically ordered set of principal-claim mappings.
///
/// Ordering matters: `cedar_schema_fragment` emits attribute lines in name
/// order so the generated Cedar schema is stable byte-for-byte across
/// restarts (audit-log diff, content-addressed `policy_hash`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PrincipalClaimMappings {
    by_name: BTreeMap<String, PrincipalClaimMapping>,
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
}

impl PrincipalClaimMappings {
    /// Builds a validated mapping set from the raw TOML config map.
    pub fn from_config(config: &PrincipalClaimsConfig) -> Result<Self, PrincipalClaimsError> {
        let mut by_name = BTreeMap::new();
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

            by_name.insert(
                attribute_name.clone(),
                PrincipalClaimMapping {
                    attribute_name,
                    claim_name,
                    claim_type: entry.claim_type,
                    required: entry.required,
                    default: entry.default.clone(),
                },
            );
        }
        Ok(Self { by_name })
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
}
