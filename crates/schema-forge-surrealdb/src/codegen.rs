//! Pure functions for compiling MigrationStep values to SurrealQL DDL strings.
//!
//! No I/O. No side effects. Each function takes schema-forge-core types
//! and returns one or more SurrealQL statement strings.

use schema_forge_core::migration::MigrationStep;
use schema_forge_core::types::{
    Cardinality, FieldDefinition, FieldModifier, FieldType, FloatConstraints, IntegerConstraints,
    TextConstraints,
};

/// Compile a single `MigrationStep` into a list of SurrealQL DDL statements.
///
/// A single step may produce multiple statements (e.g., `CreateSchema` emits
/// `DEFINE TABLE` plus one `DEFINE FIELD` per field).
///
/// # Arguments
/// * `table` - The SurrealDB table name (derived from `SchemaName`).
/// * `step`  - The migration step to compile.
pub fn migration_step_to_surql(table: &str, step: &MigrationStep) -> Vec<String> {
    match step {
        MigrationStep::CreateSchema { name: _, fields } => {
            let mut stmts = vec![format!("DEFINE TABLE {table} SCHEMAFULL;")];
            for field in fields {
                stmts.extend(define_field_stmts(table, field));
            }
            stmts
        }
        MigrationStep::DropSchema { name: _ } => {
            vec![format!("REMOVE TABLE {table};")]
        }
        MigrationStep::AddField { field } => define_field_stmts(table, field),
        MigrationStep::RemoveField { name } => {
            vec![format!("REMOVE FIELD {name} ON {table};")]
        }
        MigrationStep::RenameField { old_name, new_name } => {
            // SurrealDB does not have a native RENAME FIELD command.
            // We define the new field, copy data, then remove the old one.
            vec![
                format!("DEFINE FIELD {new_name} ON {table} TYPE any;"),
                format!("UPDATE {table} SET {new_name} = {old_name};"),
                format!("REMOVE FIELD {old_name} ON {table};"),
            ]
        }
        MigrationStep::ChangeType {
            name,
            old_type: _,
            new_type,
            transform: _,
        } => {
            let surql_type = field_type_to_surql(new_type);
            let assertions = field_assertions(new_type);
            let mut stmt = format!("DEFINE FIELD OVERWRITE {name} ON {table} TYPE {surql_type}");
            if !assertions.is_empty() {
                stmt.push_str(&format!(" ASSERT {}", assertions.join(" AND ")));
            }
            stmt.push(';');
            vec![stmt]
        }
        MigrationStep::AddIndex { field } => {
            let idx_name = format!("idx_{table}_{field}");
            vec![format!(
                "DEFINE INDEX {idx_name} ON {table} FIELDS {field};"
            )]
        }
        MigrationStep::RemoveIndex { field } => {
            let idx_name = format!("idx_{table}_{field}");
            vec![format!("REMOVE INDEX {idx_name} ON {table};")]
        }
        MigrationStep::AddRelation {
            name,
            target,
            cardinality,
        } => match cardinality {
            Cardinality::One => {
                vec![format!(
                    "DEFINE FIELD {name} ON {table} TYPE option<record<{target}>>;"
                )]
            }
            Cardinality::Many => {
                vec![format!(
                    "DEFINE FIELD {name} ON {table} TYPE option<array<record<{target}>>>;"
                )]
            }
            _ => {
                vec![format!(
                    "DEFINE FIELD {name} ON {table} TYPE option<record<{target}>>;"
                )]
            }
        },
        MigrationStep::RemoveRelation { name } => {
            vec![format!("REMOVE FIELD {name} ON {table};")]
        }
        MigrationStep::BackfillRequired {
            field,
            default_value,
        } => {
            let literal = crate::query::dynamic_value_to_surql_literal(default_value);
            vec![format!(
                "UPDATE {table} SET {field} = {literal} WHERE {field} = NONE;"
            )]
        }
        MigrationStep::AddRequired { field } => {
            // Re-define the field with a NOT NONE assertion.
            // Since we do not have the full field type here, use a flexible assertion.
            vec![format!(
                "DEFINE FIELD OVERWRITE {field} ON {table} ASSERT $value != NONE;"
            )]
        }
        MigrationStep::RemoveRequired { field } => {
            // Re-define the field without the assertion. Use `any` type to be permissive.
            vec![format!(
                "DEFINE FIELD OVERWRITE {field} ON {table} TYPE any;"
            )]
        }
        MigrationStep::SetDefault { field, value } => {
            let literal = default_value_to_surql(value);
            vec![format!(
                "DEFINE FIELD OVERWRITE {field} ON {table} VALUE $value OR {literal};"
            )]
        }
        MigrationStep::RemoveDefault { field } => {
            // Re-define without VALUE clause.
            vec![format!(
                "DEFINE FIELD OVERWRITE {field} ON {table} TYPE any;"
            )]
        }
        _ => {
            // Future MigrationStep variants -- produce a no-op comment.
            vec![format!("-- unsupported migration step for table {table}")]
        }
    }
}

/// Convert a `FieldType` to its SurrealQL TYPE string.
pub fn field_type_to_surql(field_type: &FieldType) -> String {
    match field_type {
        FieldType::Text(_) | FieldType::RichText => "string".to_string(),
        FieldType::Integer(_) => "int".to_string(),
        FieldType::Float(_) => "float".to_string(),
        FieldType::Boolean => "bool".to_string(),
        FieldType::DateTime => "datetime".to_string(),
        FieldType::Enum(_) => "string".to_string(),
        FieldType::Json => "object".to_string(),
        FieldType::Relation {
            target,
            cardinality,
        } => match cardinality {
            Cardinality::One => format!("option<record<{target}>>"),
            Cardinality::Many => format!("option<array<record<{target}>>>"),
            _ => format!("option<record<{target}>>"),
        },
        FieldType::Array(inner) => {
            let inner_type = field_type_to_surql(inner);
            format!("array<{inner_type}>")
        }
        FieldType::Composite(_) => "object".to_string(),
        _ => "any".to_string(),
    }
}

/// Build the ASSERT clause(s) for a field type's constraints.
///
/// Returns an empty vector if no assertions are needed.
pub fn field_assertions(field_type: &FieldType) -> Vec<String> {
    match field_type {
        FieldType::Text(TextConstraints {
            max_length: Some(max),
        }) => {
            vec![format!("string::len($value) <= {max}")]
        }
        FieldType::Integer(IntegerConstraints { min, max }) => {
            let mut assertions = Vec::new();
            if let Some(min_val) = min {
                assertions.push(format!("$value >= {min_val}"));
            }
            if let Some(max_val) = max {
                assertions.push(format!("$value <= {max_val}"));
            }
            assertions
        }
        FieldType::Float(FloatConstraints { precision: _ }) => {
            // Precision is a display concern, not a storage assertion.
            Vec::new()
        }
        FieldType::Enum(variants) => {
            let values: Vec<String> = variants.iter().map(|v| format!("'{v}'")).collect();
            vec![format!("$value IN [{}]", values.join(", "))]
        }
        _ => Vec::new(),
    }
}

/// Generate a complete DEFINE FIELD statement (possibly multiple for composites).
fn define_field_stmts(table: &str, field: &FieldDefinition) -> Vec<String> {
    let name = &field.name;
    let base_type = field_type_to_surql(&field.field_type);

    // Non-required fields use option<type> so SurrealDB SCHEMAFULL tables accept NONE
    let surql_type = if !field.is_required() && !base_type.starts_with("option<") {
        format!("option<{base_type}>")
    } else {
        base_type
    };

    let mut parts = Vec::new();

    // Build base DEFINE FIELD
    let mut stmt = format!("DEFINE FIELD {name} ON {table} TYPE {surql_type}");

    // Gather assertions from type constraints
    let mut assertions = field_assertions(&field.field_type);

    // Add required assertion if modifier present
    if field.is_required() {
        assertions.push("$value != NONE".to_string());
    }

    if !assertions.is_empty() {
        stmt.push_str(&format!(" ASSERT {}", assertions.join(" AND ")));
    }

    // Add default value if modifier present
    for modifier in &field.modifiers {
        if let FieldModifier::Default { value } = modifier {
            let literal = default_value_to_surql(value);
            stmt.push_str(&format!(" VALUE $value OR {literal}"));
        }
    }

    stmt.push(';');
    parts.push(stmt);

    // If indexed, emit DEFINE INDEX
    if field.is_indexed() {
        let idx_name = format!("idx_{table}_{name}");
        parts.push(format!("DEFINE INDEX {idx_name} ON {table} FIELDS {name};"));
    }

    // If composite, emit nested DEFINE FIELD statements
    if let FieldType::Composite(sub_fields) = &field.field_type {
        for sub in sub_fields {
            let nested_name = format!("{name}.{}", sub.name);
            let nested_type = field_type_to_surql(&sub.field_type);
            let mut nested_stmt =
                format!("DEFINE FIELD {nested_name} ON {table} TYPE {nested_type}");

            let nested_assertions = field_assertions(&sub.field_type);
            if !nested_assertions.is_empty() {
                nested_stmt.push_str(&format!(" ASSERT {}", nested_assertions.join(" AND ")));
            }

            nested_stmt.push(';');
            parts.push(nested_stmt);
        }
    }

    parts
}

/// Convert a `DefaultValue` to its SurrealQL literal representation.
fn default_value_to_surql(value: &schema_forge_core::types::DefaultValue) -> String {
    use schema_forge_core::types::DefaultValue;
    match value {
        DefaultValue::String(s) => format!("'{s}'"),
        DefaultValue::Integer(i) => i.to_string(),
        DefaultValue::Float(s) => s.clone(),
        DefaultValue::Boolean(b) => b.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use schema_forge_core::types::{DefaultValue, EnumVariants, FieldName, SchemaName};

    fn text_field(name: &str) -> FieldDefinition {
        FieldDefinition::new(
            FieldName::new(name).unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
        )
    }

    fn text_field_with_max(name: &str, max: u32) -> FieldDefinition {
        FieldDefinition::new(
            FieldName::new(name).unwrap(),
            FieldType::Text(TextConstraints::with_max_length(max)),
        )
    }

    #[test]
    fn create_schema_produces_define_table_and_fields() {
        let step = MigrationStep::CreateSchema {
            name: SchemaName::new("Contact").unwrap(),
            fields: vec![text_field("name"), text_field("email")],
        };
        let stmts = migration_step_to_surql("Contact", &step);
        assert_eq!(stmts[0], "DEFINE TABLE Contact SCHEMAFULL;");
        // Non-required fields get option<> wrapper
        assert!(stmts[1].contains("DEFINE FIELD name ON Contact TYPE option<string>;"));
        assert!(stmts[2].contains("DEFINE FIELD email ON Contact TYPE option<string>;"));
        assert_eq!(stmts.len(), 3);
    }

    #[test]
    fn drop_schema_produces_remove_table() {
        let step = MigrationStep::DropSchema {
            name: SchemaName::new("Contact").unwrap(),
        };
        let stmts = migration_step_to_surql("Contact", &step);
        assert_eq!(stmts, vec!["REMOVE TABLE Contact;"]);
    }

    #[test]
    fn add_field_text() {
        let step = MigrationStep::AddField {
            field: text_field("phone"),
        };
        let stmts = migration_step_to_surql("Contact", &step);
        assert_eq!(
            stmts,
            vec!["DEFINE FIELD phone ON Contact TYPE option<string>;"]
        );
    }

    #[test]
    fn add_field_text_with_max_length() {
        let step = MigrationStep::AddField {
            field: text_field_with_max("email", 255),
        };
        let stmts = migration_step_to_surql("Contact", &step);
        assert_eq!(
            stmts,
            vec!["DEFINE FIELD email ON Contact TYPE option<string> ASSERT string::len($value) <= 255;"]
        );
    }

    #[test]
    fn add_field_integer_with_range() {
        let step = MigrationStep::AddField {
            field: FieldDefinition::new(
                FieldName::new("score").unwrap(),
                FieldType::Integer(IntegerConstraints::with_range(0, 100).unwrap()),
            ),
        };
        let stmts = migration_step_to_surql("Contact", &step);
        assert_eq!(
            stmts,
            vec!["DEFINE FIELD score ON Contact TYPE option<int> ASSERT $value >= 0 AND $value <= 100;"]
        );
    }

    #[test]
    fn add_field_boolean() {
        let step = MigrationStep::AddField {
            field: FieldDefinition::new(FieldName::new("active").unwrap(), FieldType::Boolean),
        };
        let stmts = migration_step_to_surql("Contact", &step);
        assert_eq!(
            stmts,
            vec!["DEFINE FIELD active ON Contact TYPE option<bool>;"]
        );
    }

    #[test]
    fn add_field_enum() {
        let step = MigrationStep::AddField {
            field: FieldDefinition::new(
                FieldName::new("status").unwrap(),
                FieldType::Enum(
                    EnumVariants::new(vec!["Active".into(), "Inactive".into()]).unwrap(),
                ),
            ),
        };
        let stmts = migration_step_to_surql("Contact", &step);
        assert_eq!(
            stmts,
            vec![
                "DEFINE FIELD status ON Contact TYPE option<string> ASSERT $value IN ['Active', 'Inactive'];"
            ]
        );
    }

    #[test]
    fn add_field_required() {
        let step = MigrationStep::AddField {
            field: FieldDefinition::with_modifiers(
                FieldName::new("name").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
                vec![FieldModifier::Required],
            ),
        };
        let stmts = migration_step_to_surql("Contact", &step);
        assert_eq!(
            stmts,
            vec!["DEFINE FIELD name ON Contact TYPE string ASSERT $value != NONE;"]
        );
    }

    #[test]
    fn add_field_with_default() {
        let step = MigrationStep::AddField {
            field: FieldDefinition::with_modifiers(
                FieldName::new("status").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
                vec![FieldModifier::Default {
                    value: DefaultValue::String("active".into()),
                }],
            ),
        };
        let stmts = migration_step_to_surql("Contact", &step);
        assert_eq!(
            stmts,
            vec!["DEFINE FIELD status ON Contact TYPE option<string> VALUE $value OR 'active';"]
        );
    }

    #[test]
    fn add_field_indexed() {
        let step = MigrationStep::AddField {
            field: FieldDefinition::with_modifiers(
                FieldName::new("email").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
                vec![FieldModifier::Indexed],
            ),
        };
        let stmts = migration_step_to_surql("Contact", &step);
        assert_eq!(stmts.len(), 2);
        assert_eq!(
            stmts[0],
            "DEFINE FIELD email ON Contact TYPE option<string>;"
        );
        assert_eq!(
            stmts[1],
            "DEFINE INDEX idx_Contact_email ON Contact FIELDS email;"
        );
    }

    #[test]
    fn remove_field() {
        let step = MigrationStep::RemoveField {
            name: FieldName::new("old_field").unwrap(),
        };
        let stmts = migration_step_to_surql("Contact", &step);
        assert_eq!(stmts, vec!["REMOVE FIELD old_field ON Contact;"]);
    }

    #[test]
    fn add_index() {
        let step = MigrationStep::AddIndex {
            field: FieldName::new("email").unwrap(),
        };
        let stmts = migration_step_to_surql("Contact", &step);
        assert_eq!(
            stmts,
            vec!["DEFINE INDEX idx_Contact_email ON Contact FIELDS email;"]
        );
    }

    #[test]
    fn remove_index() {
        let step = MigrationStep::RemoveIndex {
            field: FieldName::new("email").unwrap(),
        };
        let stmts = migration_step_to_surql("Contact", &step);
        assert_eq!(stmts, vec!["REMOVE INDEX idx_Contact_email ON Contact;"]);
    }

    #[test]
    fn add_relation_one() {
        let step = MigrationStep::AddRelation {
            name: FieldName::new("company").unwrap(),
            target: SchemaName::new("Company").unwrap(),
            cardinality: Cardinality::One,
        };
        let stmts = migration_step_to_surql("Contact", &step);
        assert_eq!(
            stmts,
            vec!["DEFINE FIELD company ON Contact TYPE option<record<Company>>;"]
        );
    }

    #[test]
    fn add_relation_many() {
        let step = MigrationStep::AddRelation {
            name: FieldName::new("contacts").unwrap(),
            target: SchemaName::new("Contact").unwrap(),
            cardinality: Cardinality::Many,
        };
        let stmts = migration_step_to_surql("Company", &step);
        assert_eq!(
            stmts,
            vec!["DEFINE FIELD contacts ON Company TYPE option<array<record<Contact>>>;"]
        );
    }

    #[test]
    fn field_type_to_surql_all_types() {
        assert_eq!(
            field_type_to_surql(&FieldType::Text(TextConstraints::unconstrained())),
            "string"
        );
        assert_eq!(field_type_to_surql(&FieldType::RichText), "string");
        assert_eq!(
            field_type_to_surql(&FieldType::Integer(IntegerConstraints::unconstrained())),
            "int"
        );
        assert_eq!(
            field_type_to_surql(&FieldType::Float(FloatConstraints::unconstrained())),
            "float"
        );
        assert_eq!(field_type_to_surql(&FieldType::Boolean), "bool");
        assert_eq!(field_type_to_surql(&FieldType::DateTime), "datetime");
        assert_eq!(field_type_to_surql(&FieldType::Json), "object");
        assert_eq!(
            field_type_to_surql(&FieldType::Array(Box::new(FieldType::Boolean))),
            "array<bool>"
        );
    }

    #[test]
    fn rename_field_produces_three_statements() {
        let step = MigrationStep::RenameField {
            old_name: FieldName::new("name").unwrap(),
            new_name: FieldName::new("full_name").unwrap(),
        };
        let stmts = migration_step_to_surql("Contact", &step);
        assert_eq!(stmts.len(), 3);
        assert!(stmts[0].contains("DEFINE FIELD full_name"));
        assert!(stmts[1].contains("UPDATE Contact SET full_name = name"));
        assert!(stmts[2].contains("REMOVE FIELD name"));
    }

    #[test]
    fn change_type_enum_includes_assertion() {
        let step = MigrationStep::ChangeType {
            name: FieldName::new("status").unwrap(),
            old_type: FieldType::Enum(
                EnumVariants::new(vec!["active".into(), "inactive".into()]).unwrap(),
            ),
            new_type: FieldType::Enum(
                EnumVariants::new(vec!["active".into(), "inactive".into(), "archived".into()])
                    .unwrap(),
            ),
            transform: schema_forge_core::migration::ValueTransform::Identity,
        };
        let stmts = migration_step_to_surql("Contact", &step);
        assert_eq!(stmts.len(), 1);
        assert_eq!(
            stmts[0],
            "DEFINE FIELD OVERWRITE status ON Contact TYPE string ASSERT $value IN ['active', 'inactive', 'archived'];"
        );
    }

    #[test]
    fn change_type_text_with_max_includes_assertion() {
        let step = MigrationStep::ChangeType {
            name: FieldName::new("name").unwrap(),
            old_type: FieldType::Text(TextConstraints::unconstrained()),
            new_type: FieldType::Text(TextConstraints::with_max_length(100)),
            transform: schema_forge_core::migration::ValueTransform::Identity,
        };
        let stmts = migration_step_to_surql("Contact", &step);
        assert_eq!(stmts.len(), 1);
        assert_eq!(
            stmts[0],
            "DEFINE FIELD OVERWRITE name ON Contact TYPE string ASSERT string::len($value) <= 100;"
        );
    }

    #[test]
    fn change_type_plain_no_assertion() {
        let step = MigrationStep::ChangeType {
            name: FieldName::new("score").unwrap(),
            old_type: FieldType::Integer(IntegerConstraints::unconstrained()),
            new_type: FieldType::Float(FloatConstraints::unconstrained()),
            transform: schema_forge_core::migration::ValueTransform::Identity,
        };
        let stmts = migration_step_to_surql("Contact", &step);
        assert_eq!(stmts.len(), 1);
        assert_eq!(
            stmts[0],
            "DEFINE FIELD OVERWRITE score ON Contact TYPE float;"
        );
    }
}
