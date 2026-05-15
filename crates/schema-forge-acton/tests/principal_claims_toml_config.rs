//! TOML-deserialisation pinning for `[schema_forge.authz.principal_claims]`
//! (issue #54).
//!
//! Goes through the exact same loader entry point the `serve` command uses:
//!
//!   1. `toml::from_str::<SchemaForgeConfig>` — what `acton_service::Config`
//!      wraps when `svc_config.custom` is populated.
//!   2. `PrincipalClaimMappings::from_config(...)` — the next call
//!      `commands::serve` makes (see `crates/schema-forge-cli/src/commands/serve.rs:88`).
//!   3. `resolve_user_field_sources(...)` — phase 2, also called by `serve`.
//!
//! Pins the documented inline form in `docs/principal-claims-reference.md`:
//!
//! ```toml
//! [schema_forge.authz.principal_claims.client_org_id]
//! type     = "string"
//! required = false
//! source   = { user_field = "client_org_id" }
//! ```
//!
//! Prior to the fix, deserialising this TOML failed with
//! `invalid type: found string "client_org_id", expected struct variant`
//! because `PrincipalClaimSourceConfig` was externally tagged. The fix
//! makes the enum untagged so the inline table shape matches by structure.

use schema_forge_acton::authz::principal_claims::{
    PrincipalClaimMappings, PrincipalClaimSourceConfig, PrincipalClaimType, ResolvedClaimSource,
    UserFieldProjection,
};
use schema_forge_acton::config::SchemaForgeConfig;
use schema_forge_core::types::{
    Annotation, FieldDefinition, FieldName, FieldType, SchemaDefinition, SchemaId, SchemaName,
    TextConstraints,
};

/// Fixture: the User schema the loaded mappings are resolved against.
/// Mirrors the deployment override shown in §9.8 of the reference doc but
/// trimmed to just the field under test.
fn user_schema() -> SchemaDefinition {
    SchemaDefinition::new(
        SchemaId::new(),
        SchemaName::new("User").unwrap(),
        vec![
            FieldDefinition::new(
                FieldName::new("email").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
            ),
            FieldDefinition::new(
                FieldName::new("client_org_id").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
            ),
        ],
        vec![Annotation::System],
    )
    .unwrap()
}

#[test]
fn documented_inline_source_form_parses_and_resolves() {
    // Exact byte-for-byte copy of the documented form in
    // docs/principal-claims-reference.md §9.1 (and the issue body).
    let toml_input = r#"
[schema_forge.authz.principal_claims.client_org_id]
type     = "string"
required = false
source   = { user_field = "client_org_id" }
"#;

    // Path used by `commands::serve`: toml::from_str on the wrapper config.
    let config: SchemaForgeConfig = toml::from_str(toml_input)
        .expect("documented inline `source = { user_field = ... }` must parse");

    let raw = &config.schema_forge.authz.principal_claims;
    assert_eq!(raw.len(), 1, "exactly one claim configured");
    let entry = raw
        .get("client_org_id")
        .expect("client_org_id mapping must be present");
    assert_eq!(entry.claim_type, PrincipalClaimType::String);
    assert!(!entry.required);
    assert_eq!(
        entry.source.as_ref().expect("source block must be parsed"),
        &PrincipalClaimSourceConfig::UserField {
            user_field: "client_org_id".to_string(),
        },
    );

    // Phase 1: identical to `serve.rs` line 88.
    let mut mappings = PrincipalClaimMappings::from_config(raw)
        .expect("from_config must validate the parsed mapping");

    // Phase 2: identical to `serve.rs` line 179.
    mappings
        .resolve_user_field_sources(&user_schema())
        .expect("resolve_user_field_sources must succeed against a matching User schema");

    let mapping = mappings
        .iter()
        .find(|m| m.attribute_name == "client_org_id")
        .expect("mapping must survive validation");

    assert_eq!(
        mapping.source.as_ref().expect("source must resolve"),
        &ResolvedClaimSource {
            user_field: "client_org_id".to_string(),
            projection: UserFieldProjection::TextToString,
        },
    );
}

#[test]
fn principal_claims_source_round_trips_through_toml() {
    // Round-trip pin: a parsed config must re-serialize to a form that
    // re-parses to the same value. Guards against an accidental
    // non-symmetric `Serialize` / `Deserialize` pair when we move the enum
    // from externally-tagged to untagged.
    let toml_input = r#"
[schema_forge.authz.principal_claims.client_org_id]
type     = "string"
required = false
source   = { user_field = "client_org_id" }
"#;

    let parsed_once: SchemaForgeConfig =
        toml::from_str(toml_input).expect("first parse must succeed");
    let re_emitted =
        toml::to_string(&parsed_once).expect("re-serialisation must succeed");
    let parsed_twice: SchemaForgeConfig =
        toml::from_str(&re_emitted).expect("re-parse of re-emitted TOML must succeed");

    let original = &parsed_once.schema_forge.authz.principal_claims;
    let round_tripped = &parsed_twice.schema_forge.authz.principal_claims;
    let a = original
        .get("client_org_id")
        .expect("original mapping present");
    let b = round_tripped
        .get("client_org_id")
        .expect("round-tripped mapping present");
    assert_eq!(a.claim_type, b.claim_type);
    assert_eq!(a.required, b.required);
    assert_eq!(a.source, b.source);
}

#[test]
fn omitted_source_block_still_parses() {
    // Negative-control: mappings with no `source` line continue to parse
    // (pre-#51 deployments must keep working). Pins that the untagged enum
    // doesn't accidentally make the field non-optional.
    let toml_input = r#"
[schema_forge.authz.principal_claims.client_org_id]
type = "string"
"#;
    let config: SchemaForgeConfig =
        toml::from_str(toml_input).expect("source-less mapping must parse");
    let entry = config
        .schema_forge
        .authz
        .principal_claims
        .get("client_org_id")
        .expect("mapping present");
    assert!(entry.source.is_none(), "no source declared → source is None");
}
