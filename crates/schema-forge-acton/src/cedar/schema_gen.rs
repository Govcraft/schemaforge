//! Cedar schema source generator.
//!
//! Produces the Cedar schema text that declares every entity type, action,
//! and attribute the policy generator emits references to. The output is
//! parsed by `cedar_policy::Schema::from_cedarschema_str`, then handed to
//! the strict-mode validator to gate every generated and custom policy.
//!
//! # Layout
//!
//! - The `Forge::` namespace declares SchemaForge's built-in types
//!   (`Principal`, `Group`, `Tenant`, `Schema`) plus the schema-administration
//!   actions.
//! - Application schemas appear at the top level as bare entity types
//!   (`entity Contact = { ... };`) with a `[Forge::Tenant]` parent declaration
//!   so resources can carry a `_tenant` reference.
//! - Per-action `appliesTo` declarations live at the top level so action
//!   UIDs render as `Action::"ReadContact"` rather than
//!   `Forge::Action::"ReadContact"` — matching Cedar's conventional idioms.

use std::collections::BTreeSet;
use std::fmt::Write;

use schema_forge_core::types::{Cardinality, FieldAnnotation, FieldType, SchemaDefinition};

use crate::authz::principal_claims::PrincipalClaimMappings;

/// Errors raised while generating Cedar schema source.
#[derive(Debug, thiserror::Error)]
pub enum SchemaGenError {
    /// String formatting failed (should not happen in practice).
    #[error("schema generation write failure: {0}")]
    Write(#[from] std::fmt::Error),
}

/// Inputs to the Cedar schema generator.
///
/// Bundled into a struct so callers don't need to thread additional positional
/// arguments through every layer when new generator inputs are added (custom
/// principal-claim attribute mappings, eventually entity-ref claim types,
/// etc.).
pub struct CedarSchemaInputs<'a> {
    /// Application schemas to emit.
    pub schemas: &'a [SchemaDefinition],
    /// Operator-defined principal-claim → Cedar attribute mappings. Each
    /// entry produces one optional attribute on `Forge::Principal`.
    pub principal_claims: &'a PrincipalClaimMappings,
}

impl<'a> CedarSchemaInputs<'a> {
    /// Inputs covering `schemas` with no principal-claim mappings. Matches
    /// pre-#50 behaviour byte-for-byte.
    pub fn new(schemas: &'a [SchemaDefinition]) -> Self {
        // Borrows a shared empty mapping so the schema fragment is empty and
        // no attribute lines are spliced in.
        Self {
            schemas,
            principal_claims: empty_principal_claims(),
        }
    }
}

/// Returns a static reference to a shared empty `PrincipalClaimMappings` so
/// callers can build a `CedarSchemaInputs` without owning the mappings.
fn empty_principal_claims() -> &'static PrincipalClaimMappings {
    use std::sync::OnceLock;
    static EMPTY: OnceLock<PrincipalClaimMappings> = OnceLock::new();
    EMPTY.get_or_init(PrincipalClaimMappings::default)
}

/// Generates Cedar schema source covering `schemas` with no extra inputs.
///
/// Convenience wrapper around [`generate_cedar_schema_with_inputs`]; equivalent
/// to passing `CedarSchemaInputs::new(schemas)`. Output is byte-identical to
/// the pre-#50 generator when no principal-claim mappings are configured.
pub fn generate_cedar_schema(schemas: &[SchemaDefinition]) -> Result<String, SchemaGenError> {
    generate_cedar_schema_with_inputs(CedarSchemaInputs::new(schemas))
}

/// Generates Cedar schema source from full `inputs`.
///
/// The returned string is a complete `cedarschema` document. It always
/// declares the `Forge::` namespace (Principal/Group/Tenant/Schema), the
/// schema-administration actions, and one entity-type plus CRUD action set
/// per entry in `inputs.schemas`. Per-field `ReadField{Schema}_{field}` /
/// `WriteField{Schema}_{field}` actions are emitted only for fields with
/// a `@field_access` annotation, keeping the action namespace bounded.
/// Operator-supplied `inputs.principal_claims` are emitted as **optional**
/// attributes on `Forge::Principal`; custom policies must guard them with
/// `principal has X && ...` per Cedar 4.x strict-mode semantics.
pub fn generate_cedar_schema_with_inputs(
    inputs: CedarSchemaInputs<'_>,
) -> Result<String, SchemaGenError> {
    let mut out = String::new();
    write_forge_namespace(&mut out, inputs.principal_claims)?;

    for schema in inputs.schemas {
        write_schema_entity(&mut out, schema)?;
        write_schema_actions(&mut out, schema)?;
        write_per_field_actions(&mut out, schema)?;
    }

    Ok(out)
}

fn write_forge_namespace(
    out: &mut String,
    principal_claims: &PrincipalClaimMappings,
) -> Result<(), SchemaGenError> {
    let principal_extras = principal_claims.cedar_schema_fragment();
    writeln!(
        out,
        r#"namespace Forge {{
    entity Group = {{
        name: String,
        rank: Long,
    }};

    entity Tenant in [Tenant] = {{
        schema: String,
        entity_id: String,
    }};

    entity Schema = {{
        name: String,
    }};

    entity Principal in [Group, Tenant] = {{
        id: String,
        role_rank: Long,
        roles: Set<String>,
{principal_extras}    }};
}}

action UpdateSchema, DeleteSchema appliesTo {{
    principal: [Forge::Principal],
    resource: [Forge::Schema],
}};
"#
    )?;
    Ok(())
}

fn write_schema_entity(out: &mut String, schema: &SchemaDefinition) -> Result<(), SchemaGenError> {
    let name = schema.name.as_str();
    writeln!(out, "entity {name} in [Forge::Tenant] = {{")?;

    // _tenant is the standardized reference field. Optional because not every
    // schema participates in the tenant hierarchy.
    writeln!(out, "    \"_tenant\"?: Forge::Tenant,")?;

    for field in &schema.fields {
        // `@hidden` fields never surface as Cedar attributes. They live only
        // in the storage layer; the API and the policy engine must remain
        // unaware of them. (See `FieldAnnotation::Hidden` for the contract.)
        if field.is_hidden() {
            continue;
        }
        let cedar_type = match cedar_type_for(&field.field_type) {
            Some(t) => t,
            None => continue,
        };
        // Required-by-DSL fields are declared required in the Cedar schema
        // so strict-mode policies can dereference them without `has` guards.
        // Optional fields stay optional because adapters skip nulls, and
        // Cedar's `has` operator covers the absence path.
        let optional_marker = if field.is_required() { "" } else { "?" };
        writeln!(
            out,
            "    \"{}\"{}: {},",
            field.name.as_str(),
            optional_marker,
            cedar_type
        )?;
    }
    writeln!(out, "}};\n")?;
    Ok(())
}

fn write_schema_actions(out: &mut String, schema: &SchemaDefinition) -> Result<(), SchemaGenError> {
    let name = schema.name.as_str();
    writeln!(
        out,
        "action Read{name}, List{name}, Create{name}, Update{name}, Delete{name} appliesTo {{
    principal: [Forge::Principal],
    resource: [{name}],
}};\n"
    )?;
    Ok(())
}

fn write_per_field_actions(
    out: &mut String,
    schema: &SchemaDefinition,
) -> Result<(), SchemaGenError> {
    let name = schema.name.as_str();
    let mut field_actions: BTreeSet<String> = BTreeSet::new();
    for field in &schema.fields {
        if field.is_hidden() {
            // @hidden fields are invisible to Cedar — no actions, no
            // attributes. Internal callers read them out-of-band.
            continue;
        }
        if !field
            .annotations
            .iter()
            .any(|a| matches!(a, FieldAnnotation::FieldAccess { .. }))
        {
            continue;
        }
        let f = field.name.as_str();
        field_actions.insert(format!("ReadField{name}_{f}"));
        field_actions.insert(format!("WriteField{name}_{f}"));
    }
    if field_actions.is_empty() {
        return Ok(());
    }
    let joined = field_actions.into_iter().collect::<Vec<_>>().join(", ");
    writeln!(
        out,
        "action {joined} appliesTo {{
    principal: [Forge::Principal],
    resource: [{name}],
}};\n"
    )?;
    Ok(())
}

/// Maps a SchemaForge [`FieldType`] to a Cedar attribute type expression.
///
/// Returns `None` for types that have no clean Cedar representation
/// (composites, arbitrary JSON). Such fields will not appear as resource
/// attributes; policies cannot test them.
fn cedar_type_for(ft: &FieldType) -> Option<String> {
    match ft {
        FieldType::Text(_) | FieldType::RichText => Some("String".into()),
        FieldType::Integer(_) => Some("Long".into()),
        FieldType::Float(_) => Some("Long".into()),
        FieldType::Boolean => Some("Bool".into()),
        FieldType::DateTime => Some("Long".into()),
        FieldType::Enum(_) => Some("String".into()),
        FieldType::Json => None,
        FieldType::Relation { cardinality, .. } => match cardinality {
            Cardinality::One => Some("String".into()),
            Cardinality::Many => Some("Set<String>".into()),
            _ => None,
        },
        FieldType::Array(inner) => cedar_type_for(inner).map(|t| format!("Set<{t}>")),
        FieldType::Composite(_) => None,
        FieldType::File(_) => Some("String".into()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use schema_forge_core::types::{
        FieldAnnotation, FieldDefinition, FieldName, FieldType, IntegerConstraints, SchemaId,
        SchemaName, TextConstraints,
    };

    fn contact_schema() -> SchemaDefinition {
        SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Contact").unwrap(),
            vec![
                FieldDefinition::new(
                    FieldName::new("name").unwrap(),
                    FieldType::Text(TextConstraints::unconstrained()),
                ),
                FieldDefinition::new(
                    FieldName::new("age").unwrap(),
                    FieldType::Integer(IntegerConstraints::default()),
                ),
            ],
            vec![],
        )
        .unwrap()
    }

    fn employee_schema_with_field_access() -> SchemaDefinition {
        SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Employee").unwrap(),
            vec![
                FieldDefinition::new(
                    FieldName::new("name").unwrap(),
                    FieldType::Text(TextConstraints::unconstrained()),
                ),
                FieldDefinition::with_annotations(
                    FieldName::new("salary").unwrap(),
                    FieldType::Integer(IntegerConstraints::default()),
                    vec![],
                    vec![FieldAnnotation::FieldAccess {
                        read: vec!["hr".into()],
                        write: vec!["hr".into()],
                    }],
                ),
            ],
            vec![],
        )
        .unwrap()
    }

    #[test]
    fn generates_forge_namespace() {
        let src = generate_cedar_schema(&[contact_schema()]).unwrap();
        assert!(src.contains("namespace Forge"));
        assert!(src.contains("entity Principal"));
        assert!(src.contains("entity Group"));
        assert!(src.contains("entity Tenant"));
    }

    #[test]
    fn generates_app_schema_entity() {
        let src = generate_cedar_schema(&[contact_schema()]).unwrap();
        assert!(src.contains("entity Contact in [Forge::Tenant]"));
        assert!(src.contains("\"name\"?: String"));
        assert!(src.contains("\"age\"?: Long"));
    }

    #[test]
    fn generates_crud_actions_per_schema() {
        let src = generate_cedar_schema(&[contact_schema()]).unwrap();
        assert!(src.contains("action ReadContact, ListContact, CreateContact, UpdateContact, DeleteContact"));
    }

    #[test]
    fn generates_per_field_actions_only_for_field_access_fields() {
        let src = generate_cedar_schema(&[employee_schema_with_field_access()]).unwrap();
        assert!(src.contains("ReadFieldEmployee_salary"));
        assert!(src.contains("WriteFieldEmployee_salary"));
        assert!(!src.contains("ReadFieldEmployee_name"));
    }

    #[test]
    fn user_schema_role_rank_field_is_emitted_when_present_and_required() {
        // The system User schema declares role_rank as `integer required`,
        // so the Cedar schema must emit it without the optional marker —
        // strict-mode policies dereference `resource.role_rank` directly.
        use schema_forge_core::types::{FieldModifier, IntegerConstraints};
        let user = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("User").unwrap(),
            vec![
                FieldDefinition::new(
                    FieldName::new("email").unwrap(),
                    FieldType::Text(TextConstraints::unconstrained()),
                ),
                FieldDefinition::with_annotations(
                    FieldName::new("role_rank").unwrap(),
                    FieldType::Integer(IntegerConstraints::default()),
                    vec![FieldModifier::Required],
                    vec![],
                ),
            ],
            vec![],
        )
        .unwrap();
        let src = generate_cedar_schema(&[user]).unwrap();
        assert!(
            src.contains("\"role_rank\": Long,"),
            "expected required role_rank declaration, got:\n{src}"
        );
    }

    #[test]
    fn output_parses_as_cedar_schema() {
        let src = generate_cedar_schema(&[
            contact_schema(),
            employee_schema_with_field_access(),
        ])
        .unwrap();
        let result = cedar_policy::Schema::from_cedarschema_str(&src);
        assert!(
            result.is_ok(),
            "generated Cedar schema must parse:\n{}\nError: {:?}",
            src,
            result.err()
        );
    }

    fn principal_claims_for_test() -> PrincipalClaimMappings {
        use crate::authz::principal_claims::{
            PrincipalClaimConfigEntry, PrincipalClaimType, PrincipalClaimsConfig,
        };
        let mut cfg = PrincipalClaimsConfig::new();
        cfg.insert(
            "client_org_id".into(),
            PrincipalClaimConfigEntry {
                claim: None,
                claim_type: PrincipalClaimType::String,
                required: false,
                default: None,
            },
        );
        cfg.insert(
            "team_ids".into(),
            PrincipalClaimConfigEntry {
                claim: None,
                claim_type: PrincipalClaimType::SetOfString,
                required: false,
                default: None,
            },
        );
        PrincipalClaimMappings::from_config(&cfg).unwrap()
    }

    #[test]
    fn empty_mappings_yield_byte_identical_principal_block() {
        // Regression guard: an unconfigured deployment must produce the same
        // schema source as before issue #50. Compares the namespace block
        // byte-for-byte against the pre-#50 expected output.
        let src = generate_cedar_schema(&[contact_schema()]).unwrap();
        let expected_principal = "    entity Principal in [Group, Tenant] = {\n\
                                  \x20       id: String,\n\
                                  \x20       role_rank: Long,\n\
                                  \x20       roles: Set<String>,\n\
                                  \x20   };";
        assert!(
            src.contains(expected_principal),
            "expected pre-#50 Principal block, got:\n{src}",
        );
    }

    #[test]
    fn principal_claim_mappings_emit_optional_attributes_in_order() {
        let inputs = CedarSchemaInputs {
            schemas: &[contact_schema()],
            principal_claims: &principal_claims_for_test(),
        };
        let src = generate_cedar_schema_with_inputs(inputs).unwrap();
        // Both attributes appear, marked optional, after the intrinsic ones,
        // and the BTreeMap-keyed iterator gives stable lexical ordering.
        let principal_start = src.find("entity Principal").expect("principal block");
        let principal_block = &src[principal_start..];
        let order = [
            "id: String",
            "role_rank: Long",
            "roles: Set<String>",
            "\"client_org_id\"?: String",
            "\"team_ids\"?: Set<String>",
        ];
        let mut last = 0usize;
        for needle in order {
            let pos = principal_block[last..]
                .find(needle)
                .unwrap_or_else(|| panic!("missing or out-of-order: {needle}\n{src}"));
            last += pos + needle.len();
        }
    }

    #[test]
    fn output_with_principal_claims_parses_and_strict_validates_with_has_guarded_policy() {
        use cedar_policy::{ValidationMode, Validator};

        let inputs = CedarSchemaInputs {
            schemas: &[contact_schema()],
            principal_claims: &principal_claims_for_test(),
        };
        let schema_src = generate_cedar_schema_with_inputs(inputs).unwrap();
        let (schema, _warnings) = cedar_policy::Schema::from_cedarschema_str(&schema_src)
            .expect("schema must parse");

        // A custom policy that reads the operator-supplied attribute, guarded
        // with `has` so strict mode is happy.
        let policy_src = r#"
permit (
    principal,
    action == Action::"ReadContact",
    resource is Contact
)
when {
    principal has client_org_id &&
    principal.client_org_id == "org-42"
};
"#;
        let policy_set: cedar_policy::PolicySet = policy_src.parse().expect("policy must parse");
        let validator = Validator::new(schema);
        let result = validator.validate(&policy_set, ValidationMode::Strict);
        assert!(
            result.validation_passed(),
            "strict-mode validation must accept guarded references to operator-mapped \
             principal attributes.\nErrors:\n{}\nSchema:\n{schema_src}\nPolicy:\n{policy_src}",
            result
                .validation_errors()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}
