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

/// Errors raised while generating Cedar schema source.
#[derive(Debug, thiserror::Error)]
pub enum SchemaGenError {
    /// String formatting failed (should not happen in practice).
    #[error("schema generation write failure: {0}")]
    Write(#[from] std::fmt::Error),
}

/// Generates Cedar schema source covering `schemas`.
///
/// The returned string is a complete `cedarschema` document. It always
/// declares the `Forge::` namespace (Principal/Group/Tenant/Schema), the
/// schema-administration actions, and one entity-type plus CRUD action set
/// per entry in `schemas`. Per-field `ReadField{Schema}_{field}` /
/// `WriteField{Schema}_{field}` actions are emitted only for fields with
/// a `@field_access` annotation, keeping the action namespace bounded.
pub fn generate_cedar_schema(schemas: &[SchemaDefinition]) -> Result<String, SchemaGenError> {
    let mut out = String::new();
    write_forge_namespace(&mut out)?;

    for schema in schemas {
        write_schema_entity(&mut out, schema)?;
        write_schema_actions(&mut out, schema)?;
        write_per_field_actions(&mut out, schema)?;
    }

    Ok(out)
}

fn write_forge_namespace(out: &mut String) -> Result<(), SchemaGenError> {
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
    }};
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
    write!(out, "entity {name} in [Forge::Tenant] = {{\n")?;

    // _tenant is the standardized reference field. Optional because not every
    // schema participates in the tenant hierarchy.
    writeln!(out, "    \"_tenant\"?: Forge::Tenant,")?;

    // role_rank only appears on the User schema, but we declare it
    // unconditionally as optional so user-management policies parse against
    // any future schema without a flow-typed condition.
    if name == "User" {
        writeln!(out, "    role_rank: Long,")?;
    }

    for field in &schema.fields {
        let cedar_type = match cedar_type_for(&field.field_type) {
            Some(t) => t,
            None => continue,
        };
        // Every field is declared optional in the Cedar schema even when the
        // domain marks it required: the resource adapter only inserts present
        // values, and Cedar will reject a Request whose entity is missing a
        // required attribute. Optional avoids spurious validation failures
        // for partial entities (e.g., during update workflows).
        writeln!(
            out,
            "    \"{}\"?: {},",
            field.name.as_str(),
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
    fn user_schema_carries_role_rank_attribute() {
        let user = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("User").unwrap(),
            vec![FieldDefinition::new(
                FieldName::new("email").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
            )],
            vec![],
        )
        .unwrap();
        let src = generate_cedar_schema(&[user]).unwrap();
        assert!(src.contains("role_rank: Long"));
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
}
