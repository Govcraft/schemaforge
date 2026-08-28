//! Pure functions for compiling MigrationStep values to PostgreSQL DDL strings.
//!
//! No I/O. No side effects. Each function takes schema-forge-core types
//! and returns one or more PostgreSQL statement strings.

use schema_forge_core::migration::MigrationStep;
use schema_forge_core::types::{
    BytesConstraints, Cardinality, FieldDefinition, FieldModifier, FieldType, IntegerConstraints,
    TextConstraints,
};

/// Compile a single `MigrationStep` into a list of PostgreSQL DDL statements.
///
/// A single step may produce multiple statements (e.g., `CreateSchema` emits
/// `CREATE TABLE` plus constraint definitions).
///
/// # Arguments
/// * `table` - The PostgreSQL table name (derived from `SchemaName`).
/// * `step`  - The migration step to compile.
pub fn migration_step_to_sql(table: &str, step: &MigrationStep) -> Vec<String> {
    match step {
        MigrationStep::CreateSchema {
            name: _,
            fields,
            tenanted,
        } => {
            let mut column_defs = vec!["\"id\" TEXT PRIMARY KEY".to_string()];
            let mut post_stmts = Vec::new();

            for field in fields {
                let (col_def, extra_stmts) = field_to_column_def(table, field);
                column_defs.push(col_def);
                post_stmts.extend(extra_stmts);
            }

            let mut stmts = vec![format!(
                "CREATE TABLE IF NOT EXISTS \"{table}\" ({});",
                column_defs.join(", ")
            )];
            // Tenant column + tenant index must land before any
            // post-statements (e.g. follow-on `AddUnique { per_tenant: true }`
            // steps later in the same migration plan reference `_tenant`).
            if *tenanted {
                stmts.extend(tenant_ddl_statements(table));
            }
            stmts.extend(post_stmts);
            stmts
        }
        MigrationStep::DropSchema { name: _ } => {
            vec![format!("DROP TABLE IF EXISTS \"{table}\" CASCADE;")]
        }
        MigrationStep::AddField { field } => {
            let pg_type = field_type_to_pg(&field.field_type);
            let mut constraints =
                field_check_constraints(table, field.name.as_ref(), &field.field_type);

            if field.is_required() {
                constraints.push("NOT NULL".to_string());
            }

            let constraint_str = if constraints.is_empty() {
                String::new()
            } else {
                format!(" {}", constraints.join(" "))
            };

            let mut stmts = vec![format!(
                "ALTER TABLE \"{table}\" ADD COLUMN IF NOT EXISTS \"{}\" {pg_type}{constraint_str};",
                field.name
            )];

            // Add default value
            for modifier in &field.modifiers {
                if let FieldModifier::Default { value } = modifier {
                    let literal = default_value_to_sql(value);
                    stmts.push(format!(
                        "ALTER TABLE \"{table}\" ALTER COLUMN \"{}\" SET DEFAULT {literal};",
                        field.name
                    ));
                }
            }

            // Add index
            if field.is_indexed() {
                let idx_name = format!("idx_{table}_{}", field.name);
                stmts.push(format!(
                    "CREATE INDEX IF NOT EXISTS \"{idx_name}\" ON \"{table}\" (\"{}\");",
                    field.name
                ));
            }

            stmts
        }
        MigrationStep::RemoveField { name } => {
            vec![format!(
                "ALTER TABLE \"{table}\" DROP COLUMN IF EXISTS \"{name}\";"
            )]
        }
        MigrationStep::RenameField { old_name, new_name } => {
            vec![format!(
                "ALTER TABLE \"{table}\" RENAME COLUMN \"{old_name}\" TO \"{new_name}\";"
            )]
        }
        MigrationStep::ChangeType {
            name,
            old_type,
            new_type,
            transform: _,
        } => {
            let old_is_enum = matches!(old_type, FieldType::Enum(_));
            let new_is_enum = matches!(new_type, FieldType::Enum(_));
            let constraint_name = format!("chk_{table}_{name}_enum");
            let mut stmts = Vec::new();

            if old_is_enum {
                stmts.push(format!(
                    "ALTER TABLE \"{table}\" DROP CONSTRAINT IF EXISTS \"{constraint_name}\";"
                ));
            }

            let old_pg = field_type_to_pg(old_type);
            let new_pg = field_type_to_pg(new_type);
            if old_pg != new_pg {
                stmts.push(format!(
                    "ALTER TABLE \"{table}\" ALTER COLUMN \"{name}\" TYPE {new_pg} USING \"{name}\"::{new_pg};"
                ));
            }

            if new_is_enum {
                for clause in field_check_constraints(table, name.as_ref(), new_type) {
                    stmts.push(format!("ALTER TABLE \"{table}\" ADD {clause};"));
                }
            }

            if stmts.is_empty() {
                stmts.push(format!(
                    "ALTER TABLE \"{table}\" ALTER COLUMN \"{name}\" TYPE {new_pg} USING \"{name}\"::{new_pg};"
                ));
            }

            stmts
        }
        MigrationStep::AddIndex { field } => {
            let idx_name = format!("idx_{table}_{field}");
            vec![format!(
                "CREATE INDEX IF NOT EXISTS \"{idx_name}\" ON \"{table}\" (\"{field}\");"
            )]
        }
        MigrationStep::RemoveIndex { field } => {
            let idx_name = format!("idx_{table}_{field}");
            vec![format!("DROP INDEX IF EXISTS \"{idx_name}\";")]
        }
        MigrationStep::AddRelation {
            name,
            target,
            cardinality,
        } => match cardinality {
            Cardinality::One => {
                vec![format!(
                    "ALTER TABLE \"{table}\" ADD COLUMN IF NOT EXISTS \"{name}\" TEXT REFERENCES \"{target}\"(\"id\");"
                )]
            }
            Cardinality::Many => {
                vec![format!(
                    "ALTER TABLE \"{table}\" ADD COLUMN IF NOT EXISTS \"{name}\" TEXT[];"
                )]
            }
            _ => {
                vec![format!(
                    "ALTER TABLE \"{table}\" ADD COLUMN IF NOT EXISTS \"{name}\" TEXT REFERENCES \"{target}\"(\"id\");"
                )]
            }
        },
        MigrationStep::RemoveRelation { name } => {
            vec![format!(
                "ALTER TABLE \"{table}\" DROP COLUMN IF EXISTS \"{name}\";"
            )]
        }
        MigrationStep::BackfillRequired {
            field,
            default_value,
        } => {
            let literal = dynamic_value_to_sql_literal(default_value);
            vec![format!(
                "UPDATE \"{table}\" SET \"{field}\" = {literal} WHERE \"{field}\" IS NULL;"
            )]
        }
        MigrationStep::AddRequired { field } => {
            vec![format!(
                "ALTER TABLE \"{table}\" ALTER COLUMN \"{field}\" SET NOT NULL;"
            )]
        }
        MigrationStep::RemoveRequired { field } => {
            vec![format!(
                "ALTER TABLE \"{table}\" ALTER COLUMN \"{field}\" DROP NOT NULL;"
            )]
        }
        MigrationStep::SetDefault { field, value } => {
            let literal = default_value_to_sql(value);
            vec![format!(
                "ALTER TABLE \"{table}\" ALTER COLUMN \"{field}\" SET DEFAULT {literal};"
            )]
        }
        MigrationStep::RemoveDefault { field } => {
            vec![format!(
                "ALTER TABLE \"{table}\" ALTER COLUMN \"{field}\" DROP DEFAULT;"
            )]
        }
        MigrationStep::AddUnique { field, per_tenant } => {
            vec![add_unique_sql(table, field.as_ref(), *per_tenant)]
        }
        MigrationStep::RemoveUnique { field, per_tenant } => {
            vec![remove_unique_sql(table, field.as_ref(), *per_tenant)]
        }
        _ => {
            // Future MigrationStep variants -- produce a no-op comment.
            vec![format!(
                "-- unsupported migration step for table \"{table}\""
            )]
        }
    }
}

/// Build the DDL that introduces a unique constraint on `(field)` (or
/// `(_tenant, field)` when tenanted). Per-tenant uniqueness is implemented
/// as a unique index — Postgres has no way to express a composite
/// constraint referencing a column that may not exist on every table
/// path. Global uniqueness uses a named table constraint for catalog
/// clarity.
fn add_unique_sql(table: &str, field: &str, per_tenant: bool) -> String {
    let constraint_name = unique_constraint_name(table, field);
    if per_tenant {
        format!(
            "CREATE UNIQUE INDEX IF NOT EXISTS \"{constraint_name}\" ON \"{table}\" (\"_tenant\", \"{field}\");"
        )
    } else {
        format!(
            "ALTER TABLE \"{table}\" ADD CONSTRAINT \"{constraint_name}\" UNIQUE (\"{field}\");"
        )
    }
}

/// Inverse of [`add_unique_sql`]. The `per_tenant` flag must match what
/// was used at create time so the right kind of object is dropped.
fn remove_unique_sql(table: &str, field: &str, per_tenant: bool) -> String {
    let constraint_name = unique_constraint_name(table, field);
    if per_tenant {
        format!("DROP INDEX IF EXISTS \"{constraint_name}\";")
    } else {
        format!("ALTER TABLE \"{table}\" DROP CONSTRAINT IF EXISTS \"{constraint_name}\";")
    }
}

fn unique_constraint_name(table: &str, field: &str) -> String {
    format!("uq_{table}_{field}")
}

/// Convert a `FieldType` to its PostgreSQL TYPE string.
pub fn field_type_to_pg(field_type: &FieldType) -> String {
    match field_type {
        FieldType::Text(TextConstraints {
            max_length: Some(max),
        }) => format!("VARCHAR({max})"),
        FieldType::Text(_) | FieldType::RichText => "TEXT".to_string(),
        FieldType::Integer(_) => "BIGINT".to_string(),
        // `FloatConstraints.precision` is intentionally ignored on Postgres; a future `decimal`
        // type will handle fixed-scale currency. See issue #7.
        FieldType::Float(_) => "DOUBLE PRECISION".to_string(),
        FieldType::Boolean => "BOOLEAN".to_string(),
        FieldType::DateTime => "TIMESTAMPTZ".to_string(),
        // A `duration` is stored as a signed count of nanoseconds in a BIGINT:
        // exact, range-comparable, and indexable. Postgres `INTERVAL` cannot
        // round-trip a chrono `TimeDelta` losslessly, so nanoseconds are used.
        // Durations whose magnitude exceeds the i64-nanosecond range (~292
        // years) are out of range and fail closed on write.
        FieldType::Duration => "BIGINT".to_string(),
        // Inline binary stored as `BYTEA`: exact byte round-trip, indexable, and
        // `octet_length` lets the optional `max_size` constraint be enforced by a
        // CHECK (see `field_check_constraints`).
        FieldType::Bytes(_) => "BYTEA".to_string(),
        FieldType::Enum(_) => "TEXT".to_string(),
        FieldType::Json => "JSONB".to_string(),
        FieldType::Relation {
            cardinality: Cardinality::Many,
            ..
        } => "TEXT[]".to_string(),
        FieldType::Relation { .. } => "TEXT".to_string(),
        FieldType::Array(inner) => {
            let inner_type = field_type_to_pg(inner);
            format!("{inner_type}[]")
        }
        FieldType::Composite(_) => "JSONB".to_string(),
        // A typed `map<string, V>` is a string-keyed object → JSONB.
        FieldType::Map { .. } => "JSONB".to_string(),
        FieldType::File(_) => "JSONB".to_string(),
        _ => "TEXT".to_string(),
    }
}

/// Build CHECK constraint fragments for a field type.
///
/// Returns constraint clauses to be appended to the column definition.
fn field_check_constraints(table: &str, field_name: &str, field_type: &FieldType) -> Vec<String> {
    match field_type {
        FieldType::Integer(IntegerConstraints { min, max }) => {
            let mut constraints = Vec::new();
            if min.is_some() || max.is_some() {
                let constraint_name = format!("chk_{table}_{field_name}_range");
                let mut parts = Vec::new();
                if let Some(min_val) = min {
                    parts.push(format!("\"{field_name}\" >= {min_val}"));
                }
                if let Some(max_val) = max {
                    parts.push(format!("\"{field_name}\" <= {max_val}"));
                }
                constraints.push(format!(
                    "CONSTRAINT \"{constraint_name}\" CHECK ({})",
                    parts.join(" AND ")
                ));
            }
            constraints
        }
        FieldType::Bytes(BytesConstraints {
            max_size: Some(max),
        }) => {
            // Enforce the byte-length cap at the column level so an oversized
            // value fails closed on write rather than being silently stored.
            let constraint_name = format!("chk_{table}_{field_name}_size");
            vec![format!(
                "CONSTRAINT \"{constraint_name}\" CHECK (\"{field_name}\" IS NULL OR \
                 octet_length(\"{field_name}\") <= {max})"
            )]
        }
        FieldType::Enum(variants) => {
            let constraint_name = format!("chk_{table}_{field_name}_enum");
            let values: Vec<String> = variants.iter().map(|v| format!("'{v}'")).collect();
            vec![format!(
                "CONSTRAINT \"{constraint_name}\" CHECK (\"{field_name}\" IN ({}))",
                values.join(", ")
            )]
        }
        FieldType::File(_) => {
            // Structural check only. Size/MIME/access are runtime-enforced so they can be
            // tightened without requiring a migration.
            let constraint_name = format!("chk_{table}_{field_name}_file");
            vec![format!(
                "CONSTRAINT \"{constraint_name}\" CHECK (\"{field_name}\" IS NULL OR \
                 (jsonb_typeof(\"{field_name}\") = 'object' \
                 AND \"{field_name}\" ? 'status' \
                 AND \"{field_name}\" ? 'key'))"
            )]
        }
        _ => Vec::new(),
    }
}

/// Generate a column definition and any extra statements (like indexes) for a field.
fn field_to_column_def(table: &str, field: &FieldDefinition) -> (String, Vec<String>) {
    let name = &field.name;
    let pg_type = field_type_to_pg(&field.field_type);
    let mut extra_stmts = Vec::new();

    let mut parts = vec![format!("\"{name}\" {pg_type}")];

    // Check constraints
    let constraints = field_check_constraints(table, name.as_ref(), &field.field_type);
    parts.extend(constraints);

    // Required
    if field.is_required() {
        parts.push("NOT NULL".to_string());
    }

    // Default
    for modifier in &field.modifiers {
        if let FieldModifier::Default { value } = modifier {
            let literal = default_value_to_sql(value);
            parts.push(format!("DEFAULT {literal}"));
        }
    }

    // Index (separate statement)
    if field.is_indexed() {
        let idx_name = format!("idx_{table}_{name}");
        extra_stmts.push(format!(
            "CREATE INDEX IF NOT EXISTS \"{idx_name}\" ON \"{table}\" (\"{name}\");"
        ));
    }

    (parts.join(" "), extra_stmts)
}

/// Convert a `DefaultValue` to its PostgreSQL literal representation.
fn default_value_to_sql(value: &schema_forge_core::types::DefaultValue) -> String {
    use schema_forge_core::types::DefaultValue;
    match value {
        DefaultValue::String(s) => format!("'{}'", escape_sql_string(s)),
        DefaultValue::Integer(i) => i.to_string(),
        DefaultValue::Float(s) => s.clone(),
        DefaultValue::Boolean(b) => b.to_string(),
    }
}

/// Convert a `DynamicValue` to a PostgreSQL literal string (for use in DDL/backfill).
fn dynamic_value_to_sql_literal(value: &schema_forge_core::types::DynamicValue) -> String {
    use schema_forge_core::types::DynamicValue;
    match value {
        DynamicValue::Null => "NULL".to_string(),
        DynamicValue::Text(s) => format!("'{}'", escape_sql_string(s)),
        DynamicValue::Integer(i) => i.to_string(),
        DynamicValue::Float(f) => format!("{f}"),
        DynamicValue::Boolean(b) => b.to_string(),
        DynamicValue::DateTime(dt) => format!("'{}'", dt.to_rfc3339()),
        // Stored as a signed nanosecond count (see `field_type_to_pg`).
        // Out-of-range durations cannot be represented; fall back to NULL.
        DynamicValue::Duration(d) => d
            .num_nanoseconds()
            .map_or_else(|| "NULL".to_string(), |n| n.to_string()),
        // BYTEA hex-format literal (`'\x...'::bytea`); standard_conforming_strings
        // is on by default in modern Postgres, so the single backslash is literal.
        DynamicValue::Bytes(b) => format!("'\\x{}'::bytea", hex_encode(b)),
        DynamicValue::Enum(s) => format!("'{}'", escape_sql_string(s)),
        _ => "NULL".to_string(),
    }
}

/// Generate PostgreSQL statements to define the `_tenant` column and index for a table.
///
/// Called by the backend after applying migration steps for `CreateSchema`
/// when multi-tenancy is configured. Pure function, no I/O.
pub fn tenant_ddl_statements(table: &str) -> Vec<String> {
    vec![
        format!("ALTER TABLE \"{table}\" ADD COLUMN IF NOT EXISTS \"_tenant\" TEXT;"),
        format!("CREATE INDEX IF NOT EXISTS \"idx_{table}_tenant\" ON \"{table}\" (\"_tenant\");"),
    ]
}

/// Escape single quotes in strings for PostgreSQL string literals.
fn escape_sql_string(s: &str) -> String {
    s.replace('\'', "''")
}

/// Lowercase hex encoding of a byte slice, for a Postgres `BYTEA` hex literal.
fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use schema_forge_core::types::{
        DefaultValue, EnumVariants, FieldName, FloatConstraints, SchemaName,
    };

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
    fn create_schema_produces_create_table() {
        let step = MigrationStep::CreateSchema {
            name: SchemaName::new("Contact").unwrap(),
            fields: vec![text_field("name"), text_field("email")],
            tenanted: false,
        };
        let stmts = migration_step_to_sql("Contact", &step);
        assert_eq!(stmts.len(), 1);
        assert!(stmts[0].starts_with("CREATE TABLE IF NOT EXISTS \"Contact\""));
        assert!(stmts[0].contains("\"id\" TEXT PRIMARY KEY"));
        assert!(stmts[0].contains("\"name\" TEXT"));
        assert!(stmts[0].contains("\"email\" TEXT"));
    }

    #[test]
    fn drop_schema_produces_drop_table() {
        let step = MigrationStep::DropSchema {
            name: SchemaName::new("Contact").unwrap(),
        };
        let stmts = migration_step_to_sql("Contact", &step);
        assert_eq!(stmts, vec!["DROP TABLE IF EXISTS \"Contact\" CASCADE;"]);
    }

    #[test]
    fn add_field_text() {
        let step = MigrationStep::AddField {
            field: text_field("phone"),
        };
        let stmts = migration_step_to_sql("Contact", &step);
        assert_eq!(
            stmts,
            vec!["ALTER TABLE \"Contact\" ADD COLUMN IF NOT EXISTS \"phone\" TEXT;"]
        );
    }

    #[test]
    fn add_field_text_with_max_length() {
        let step = MigrationStep::AddField {
            field: text_field_with_max("email", 255),
        };
        let stmts = migration_step_to_sql("Contact", &step);
        assert_eq!(
            stmts,
            vec!["ALTER TABLE \"Contact\" ADD COLUMN IF NOT EXISTS \"email\" VARCHAR(255);"]
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
        let stmts = migration_step_to_sql("Contact", &step);
        assert_eq!(stmts.len(), 1);
        assert!(stmts[0].contains("\"score\" BIGINT"));
        assert!(stmts[0].contains("CHECK"));
        assert!(stmts[0].contains("\"score\" >= 0"));
        assert!(stmts[0].contains("\"score\" <= 100"));
    }

    #[test]
    fn add_field_boolean() {
        let step = MigrationStep::AddField {
            field: FieldDefinition::new(FieldName::new("active").unwrap(), FieldType::Boolean),
        };
        let stmts = migration_step_to_sql("Contact", &step);
        assert_eq!(
            stmts,
            vec!["ALTER TABLE \"Contact\" ADD COLUMN IF NOT EXISTS \"active\" BOOLEAN;"]
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
        let stmts = migration_step_to_sql("Contact", &step);
        assert_eq!(stmts.len(), 1);
        assert!(stmts[0].contains("\"status\" TEXT"));
        assert!(stmts[0].contains("CHECK"));
        assert!(stmts[0].contains("'Active'"));
        assert!(stmts[0].contains("'Inactive'"));
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
        let stmts = migration_step_to_sql("Contact", &step);
        assert_eq!(stmts.len(), 1);
        assert!(stmts[0].contains("NOT NULL"));
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
        let stmts = migration_step_to_sql("Contact", &step);
        assert_eq!(stmts.len(), 2);
        assert!(stmts[0].contains("\"status\" TEXT"));
        assert!(stmts[1].contains("SET DEFAULT 'active'"));
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
        let stmts = migration_step_to_sql("Contact", &step);
        assert_eq!(stmts.len(), 2);
        assert!(stmts[0].contains("\"email\" TEXT"));
        assert!(stmts[1].contains("CREATE INDEX IF NOT EXISTS"));
        assert!(stmts[1].contains("\"idx_Contact_email\""));
    }

    #[test]
    fn remove_field() {
        let step = MigrationStep::RemoveField {
            name: FieldName::new("old_field").unwrap(),
        };
        let stmts = migration_step_to_sql("Contact", &step);
        assert_eq!(
            stmts,
            vec!["ALTER TABLE \"Contact\" DROP COLUMN IF EXISTS \"old_field\";"]
        );
    }

    #[test]
    fn rename_field() {
        let step = MigrationStep::RenameField {
            old_name: FieldName::new("name").unwrap(),
            new_name: FieldName::new("full_name").unwrap(),
        };
        let stmts = migration_step_to_sql("Contact", &step);
        assert_eq!(stmts.len(), 1);
        assert_eq!(
            stmts[0],
            "ALTER TABLE \"Contact\" RENAME COLUMN \"name\" TO \"full_name\";"
        );
    }

    #[test]
    fn add_index() {
        let step = MigrationStep::AddIndex {
            field: FieldName::new("email").unwrap(),
        };
        let stmts = migration_step_to_sql("Contact", &step);
        assert_eq!(
            stmts,
            vec!["CREATE INDEX IF NOT EXISTS \"idx_Contact_email\" ON \"Contact\" (\"email\");"]
        );
    }

    #[test]
    fn remove_index() {
        let step = MigrationStep::RemoveIndex {
            field: FieldName::new("email").unwrap(),
        };
        let stmts = migration_step_to_sql("Contact", &step);
        assert_eq!(stmts, vec!["DROP INDEX IF EXISTS \"idx_Contact_email\";"]);
    }

    #[test]
    fn add_relation_one() {
        let step = MigrationStep::AddRelation {
            name: FieldName::new("company").unwrap(),
            target: SchemaName::new("Company").unwrap(),
            cardinality: Cardinality::One,
        };
        let stmts = migration_step_to_sql("Contact", &step);
        assert_eq!(stmts.len(), 1);
        assert!(stmts[0].contains("\"company\" TEXT REFERENCES \"Company\"(\"id\")"));
    }

    #[test]
    fn add_relation_many() {
        let step = MigrationStep::AddRelation {
            name: FieldName::new("contacts").unwrap(),
            target: SchemaName::new("Contact").unwrap(),
            cardinality: Cardinality::Many,
        };
        let stmts = migration_step_to_sql("Company", &step);
        assert_eq!(stmts.len(), 1);
        assert!(stmts[0].contains("\"contacts\" TEXT[]"));
    }

    #[test]
    fn field_type_to_pg_all_types() {
        assert_eq!(
            field_type_to_pg(&FieldType::Text(TextConstraints::unconstrained())),
            "TEXT"
        );
        assert_eq!(
            field_type_to_pg(&FieldType::Text(TextConstraints::with_max_length(255))),
            "VARCHAR(255)"
        );
        assert_eq!(field_type_to_pg(&FieldType::RichText), "TEXT");
        assert_eq!(
            field_type_to_pg(&FieldType::Integer(IntegerConstraints::unconstrained())),
            "BIGINT"
        );
        assert_eq!(
            field_type_to_pg(&FieldType::Float(FloatConstraints::unconstrained())),
            "DOUBLE PRECISION"
        );
        assert_eq!(
            field_type_to_pg(&FieldType::Float(FloatConstraints::with_precision(2))),
            "DOUBLE PRECISION"
        );
        assert_eq!(field_type_to_pg(&FieldType::Boolean), "BOOLEAN");
        assert_eq!(field_type_to_pg(&FieldType::DateTime), "TIMESTAMPTZ");
        assert_eq!(field_type_to_pg(&FieldType::Duration), "BIGINT");
        assert_eq!(
            field_type_to_pg(&FieldType::Bytes(BytesConstraints::unconstrained())),
            "BYTEA"
        );
        assert_eq!(
            field_type_to_pg(&FieldType::Bytes(BytesConstraints::with_max_size(1024))),
            "BYTEA"
        );
        assert_eq!(field_type_to_pg(&FieldType::Json), "JSONB");
        assert_eq!(
            field_type_to_pg(&FieldType::Array(Box::new(FieldType::Boolean))),
            "BOOLEAN[]"
        );
        assert_eq!(field_type_to_pg(&sample_file_field_type()), "JSONB");
        assert_eq!(
            field_type_to_pg(&FieldType::Map {
                key: Box::new(FieldType::Text(TextConstraints::unconstrained())),
                value: Box::new(FieldType::Integer(IntegerConstraints::unconstrained())),
            }),
            "JSONB"
        );
    }

    fn sample_file_field_type() -> FieldType {
        use schema_forge_core::types::{FileAccess, FileConstraints, MimePattern};
        FieldType::File(FileConstraints {
            bucket: "documents".into(),
            max_size_bytes: 25 * 1024 * 1024,
            mime_allowlist: vec![MimePattern::Exact("application/pdf".into())],
            access: FileAccess::Presigned,
        })
    }

    #[test]
    fn add_field_file_emits_jsonb_with_structural_check() {
        let step = MigrationStep::AddField {
            field: FieldDefinition::new(
                FieldName::new("contract").unwrap(),
                sample_file_field_type(),
            ),
        };
        let stmts = migration_step_to_sql("Deal", &step);
        assert_eq!(stmts.len(), 1);
        assert!(
            stmts[0].contains("\"contract\" JSONB"),
            "expected JSONB column, got: {}",
            stmts[0]
        );
        assert!(
            stmts[0].contains("CONSTRAINT \"chk_Deal_contract_file\""),
            "expected file CHECK constraint, got: {}",
            stmts[0]
        );
        assert!(stmts[0].contains("jsonb_typeof"));
        assert!(stmts[0].contains("? 'status'"));
        assert!(stmts[0].contains("? 'key'"));
    }

    #[test]
    fn add_field_bytes_unconstrained_emits_bytea_no_check() {
        let step = MigrationStep::AddField {
            field: FieldDefinition::new(
                FieldName::new("sig").unwrap(),
                FieldType::Bytes(BytesConstraints::unconstrained()),
            ),
        };
        let stmts = migration_step_to_sql("Doc", &step);
        assert_eq!(stmts.len(), 1);
        assert!(stmts[0].contains("\"sig\" BYTEA"), "got: {}", stmts[0]);
        assert!(
            !stmts[0].contains("octet_length"),
            "unconstrained bytes must not emit a size CHECK, got: {}",
            stmts[0]
        );
    }

    #[test]
    fn add_field_bytes_with_max_emits_octet_length_check() {
        let step = MigrationStep::AddField {
            field: FieldDefinition::new(
                FieldName::new("sig").unwrap(),
                FieldType::Bytes(BytesConstraints::with_max_size(64)),
            ),
        };
        let stmts = migration_step_to_sql("Doc", &step);
        assert_eq!(stmts.len(), 1);
        assert!(stmts[0].contains("\"sig\" BYTEA"), "got: {}", stmts[0]);
        assert!(
            stmts[0].contains("CONSTRAINT \"chk_Doc_sig_size\"")
                && stmts[0].contains("octet_length(\"sig\") <= 64"),
            "expected size CHECK, got: {}",
            stmts[0]
        );
    }

    #[test]
    fn bytes_sql_literal_is_hex_bytea() {
        use schema_forge_core::types::DynamicValue;
        assert_eq!(
            dynamic_value_to_sql_literal(&DynamicValue::Bytes(vec![0xde, 0xad, 0xbe, 0xef])),
            "'\\xdeadbeef'::bytea"
        );
    }

    #[test]
    fn add_field_file_required_adds_not_null() {
        let step = MigrationStep::AddField {
            field: FieldDefinition::with_modifiers(
                FieldName::new("contract").unwrap(),
                sample_file_field_type(),
                vec![FieldModifier::Required],
            ),
        };
        let stmts = migration_step_to_sql("Deal", &step);
        assert_eq!(stmts.len(), 1);
        assert!(stmts[0].contains("JSONB"));
        assert!(stmts[0].contains("NOT NULL"));
    }

    #[test]
    fn create_schema_with_file_field_includes_check() {
        let step = MigrationStep::CreateSchema {
            name: SchemaName::new("Deal").unwrap(),
            fields: vec![FieldDefinition::new(
                FieldName::new("contract").unwrap(),
                sample_file_field_type(),
            )],
            tenanted: false,
        };
        let stmts = migration_step_to_sql("Deal", &step);
        assert_eq!(stmts.len(), 1);
        assert!(stmts[0].contains("\"contract\" JSONB"));
        assert!(stmts[0].contains("chk_Deal_contract_file"));
    }

    #[test]
    fn remove_field_file_drops_column() {
        let step = MigrationStep::RemoveField {
            name: FieldName::new("contract").unwrap(),
        };
        let stmts = migration_step_to_sql("Deal", &step);
        assert_eq!(
            stmts,
            vec!["ALTER TABLE \"Deal\" DROP COLUMN IF EXISTS \"contract\";"]
        );
    }

    #[test]
    fn tenant_ddl_statements_generates_correct_sql() {
        let stmts = tenant_ddl_statements("Contact");
        assert_eq!(stmts.len(), 2);
        assert_eq!(
            stmts[0],
            "ALTER TABLE \"Contact\" ADD COLUMN IF NOT EXISTS \"_tenant\" TEXT;"
        );
        assert_eq!(
            stmts[1],
            "CREATE INDEX IF NOT EXISTS \"idx_Contact_tenant\" ON \"Contact\" (\"_tenant\");"
        );
    }

    /// Regression for GH #56: a tenanted `CreateSchema` must emit
    /// `_tenant` column + index DDL *immediately after* `CREATE TABLE`
    /// so any follow-on `AddUnique { per_tenant: true }` step in the same
    /// plan can reference `_tenant` without crashing apply.
    #[test]
    fn create_schema_tenanted_emits_tenant_column_and_index() {
        let step = MigrationStep::CreateSchema {
            name: SchemaName::new("Contact").unwrap(),
            fields: vec![text_field("name"), text_field("email")],
            tenanted: true,
        };
        let stmts = migration_step_to_sql("Contact", &step);
        assert!(
            stmts[0].starts_with("CREATE TABLE IF NOT EXISTS \"Contact\""),
            "expected CREATE TABLE first, got: {}",
            stmts[0]
        );
        assert_eq!(
            stmts[1],
            "ALTER TABLE \"Contact\" ADD COLUMN IF NOT EXISTS \"_tenant\" TEXT;",
            "expected _tenant column DDL immediately after CREATE TABLE"
        );
        assert_eq!(
            stmts[2],
            "CREATE INDEX IF NOT EXISTS \"idx_Contact_tenant\" ON \"Contact\" (\"_tenant\");",
            "expected tenant index DDL after the _tenant column"
        );
    }

    /// Guard: a non-tenanted `CreateSchema` must not emit `_tenant` DDL.
    #[test]
    fn create_schema_non_tenanted_omits_tenant_ddl() {
        let step = MigrationStep::CreateSchema {
            name: SchemaName::new("Contact").unwrap(),
            fields: vec![text_field("name")],
            tenanted: false,
        };
        let stmts = migration_step_to_sql("Contact", &step);
        for stmt in &stmts {
            assert!(
                !stmt.contains("_tenant"),
                "non-tenanted schema must not reference _tenant, found: {stmt}"
            );
        }
    }

    /// Regression for GH #56 end-to-end: running the planner against a
    /// tenant-child schema with a `unique` field followed by the codegen
    /// must produce DDL where `_tenant` is established *before* the
    /// per-tenant unique index references it.
    ///
    /// The schema here is `@tenant(parent:)`, not `@tenant(root)`. A child
    /// is what actually yields a per-tenant index (#134), so it is what
    /// exercises the ordering hazard this test exists to pin. The root case
    /// is covered by `root_schema_unique_is_a_global_constraint` below.
    #[test]
    fn tenanted_schema_with_unique_emits_tenant_column_before_unique_index() {
        use schema_forge_core::migration::DiffEngine;
        use schema_forge_core::types::{
            Annotation, FieldModifier, SchemaDefinition, SchemaId, TenantKind,
        };

        let schema = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Account").unwrap(),
            vec![FieldDefinition::with_modifiers(
                FieldName::new("slug").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
                vec![FieldModifier::Unique],
            )],
            vec![Annotation::Tenant(TenantKind::Child {
                parent: SchemaName::new("Organization").unwrap(),
            })],
        )
        .expect("schema definition must be valid");

        let plan = DiffEngine::create_new(&schema);
        let mut all_stmts: Vec<String> = Vec::new();
        for step in &plan.steps {
            all_stmts.extend(migration_step_to_sql("Account", step));
        }

        let create_idx = all_stmts
            .iter()
            .position(|s| s.starts_with("CREATE TABLE"))
            .expect("CREATE TABLE statement missing");
        let tenant_col_idx = all_stmts
            .iter()
            .position(|s| s.contains("ADD COLUMN IF NOT EXISTS \"_tenant\""))
            .expect("_tenant column DDL missing — issue #56 regression");
        let unique_idx = all_stmts
            .iter()
            .position(|s| s.contains("\"uq_Account_slug\"") && s.contains("\"_tenant\""))
            .expect("per-tenant unique index missing");

        assert!(
            create_idx < tenant_col_idx,
            "_tenant column DDL must come after CREATE TABLE"
        );
        assert!(
            tenant_col_idx < unique_idx,
            "_tenant column DDL must come before the per-tenant unique index"
        );
    }

    /// Regression for GH #134: a `unique` field on a `@tenant(root)` schema
    /// must become a table-wide constraint, not an index over
    /// `(_tenant, field)`.
    ///
    /// The tenant-scoped shape does not weaken the constraint on a root, it
    /// removes it. `_tenant` is NULL on a platform-level create, and
    /// PostgreSQL treats NULLs as distinct, so a `(NULL, 'ACME')` row never
    /// collides with another `(NULL, 'ACME')` row. Two organizations could
    /// carry the same short code and the migration would apply cleanly.
    #[test]
    fn root_schema_unique_is_a_global_constraint() {
        use schema_forge_core::migration::DiffEngine;
        use schema_forge_core::types::{
            Annotation, FieldModifier, SchemaDefinition, SchemaId, TenantKind,
        };

        let schema = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Organization").unwrap(),
            vec![FieldDefinition::with_modifiers(
                FieldName::new("short_code").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
                vec![FieldModifier::Unique],
            )],
            vec![Annotation::Tenant(TenantKind::Root)],
        )
        .expect("schema definition must be valid");

        let plan = DiffEngine::create_new(&schema);
        let all_stmts: Vec<String> = plan
            .steps
            .iter()
            .flat_map(|step| migration_step_to_sql("Organization", step))
            .collect();

        let unique_stmt = all_stmts
            .iter()
            .find(|s| s.contains("\"uq_Organization_short_code\""))
            .expect("unique DDL missing");
        assert!(
            unique_stmt.contains("ADD CONSTRAINT") && unique_stmt.contains("UNIQUE"),
            "expected a table-wide UNIQUE constraint, got: {unique_stmt}"
        );
        assert!(
            !unique_stmt.contains("_tenant"),
            "a root schema's unique must not be scoped by _tenant, got: {unique_stmt}"
        );

        // The `_tenant` column itself is still emitted: a root row created
        // by a tenant-scoped caller carries one, and the two decisions are
        // independent.
        assert!(
            all_stmts
                .iter()
                .any(|s| s.contains("ADD COLUMN IF NOT EXISTS \"_tenant\"")),
            "the _tenant column must still be created for a root schema"
        );
    }

    #[test]
    fn backfill_required() {
        let step = MigrationStep::BackfillRequired {
            field: FieldName::new("status").unwrap(),
            default_value: schema_forge_core::types::DynamicValue::Text("active".into()),
        };
        let stmts = migration_step_to_sql("Contact", &step);
        assert_eq!(
            stmts,
            vec!["UPDATE \"Contact\" SET \"status\" = 'active' WHERE \"status\" IS NULL;"]
        );
    }

    #[test]
    fn add_required() {
        let step = MigrationStep::AddRequired {
            field: FieldName::new("name").unwrap(),
        };
        let stmts = migration_step_to_sql("Contact", &step);
        assert_eq!(
            stmts,
            vec!["ALTER TABLE \"Contact\" ALTER COLUMN \"name\" SET NOT NULL;"]
        );
    }

    #[test]
    fn remove_required() {
        let step = MigrationStep::RemoveRequired {
            field: FieldName::new("name").unwrap(),
        };
        let stmts = migration_step_to_sql("Contact", &step);
        assert_eq!(
            stmts,
            vec!["ALTER TABLE \"Contact\" ALTER COLUMN \"name\" DROP NOT NULL;"]
        );
    }

    #[test]
    fn set_default() {
        let step = MigrationStep::SetDefault {
            field: FieldName::new("status").unwrap(),
            value: DefaultValue::String("active".into()),
        };
        let stmts = migration_step_to_sql("Contact", &step);
        assert_eq!(
            stmts,
            vec!["ALTER TABLE \"Contact\" ALTER COLUMN \"status\" SET DEFAULT 'active';"]
        );
    }

    #[test]
    fn remove_default() {
        let step = MigrationStep::RemoveDefault {
            field: FieldName::new("status").unwrap(),
        };
        let stmts = migration_step_to_sql("Contact", &step);
        assert_eq!(
            stmts,
            vec!["ALTER TABLE \"Contact\" ALTER COLUMN \"status\" DROP DEFAULT;"]
        );
    }

    #[test]
    fn change_type_enum_to_enum_regenerates_check_constraint() {
        let old_type = FieldType::Enum(EnumVariants::new(vec!["a".into(), "b".into()]).unwrap());
        let new_type = FieldType::Enum(
            EnumVariants::new(vec!["a".into(), "b".into(), "c".into()]).unwrap(),
        );
        let step = MigrationStep::ChangeType {
            name: FieldName::new("status").unwrap(),
            old_type,
            new_type,
            transform: schema_forge_core::migration::ValueTransform::Identity,
        };
        let stmts = migration_step_to_sql("Thing", &step);
        assert_eq!(stmts.len(), 2);
        assert_eq!(
            stmts[0],
            "ALTER TABLE \"Thing\" DROP CONSTRAINT IF EXISTS \"chk_Thing_status_enum\";"
        );
        assert!(stmts[1].starts_with("ALTER TABLE \"Thing\" ADD CONSTRAINT \"chk_Thing_status_enum\""));
        assert!(stmts[1].contains("'a'"));
        assert!(stmts[1].contains("'b'"));
        assert!(stmts[1].contains("'c'"));
        assert!(!stmts.iter().any(|s| s.contains("ALTER COLUMN")));
    }

    #[test]
    fn change_type_enum_to_non_enum_drops_check_constraint() {
        let old_type = FieldType::Enum(EnumVariants::new(vec!["a".into(), "b".into()]).unwrap());
        let new_type = FieldType::Text(TextConstraints::unconstrained());
        let step = MigrationStep::ChangeType {
            name: FieldName::new("status").unwrap(),
            old_type,
            new_type,
            transform: schema_forge_core::migration::ValueTransform::Identity,
        };
        let stmts = migration_step_to_sql("Thing", &step);
        assert_eq!(
            stmts[0],
            "ALTER TABLE \"Thing\" DROP CONSTRAINT IF EXISTS \"chk_Thing_status_enum\";"
        );
        assert!(!stmts.iter().any(|s| s.contains("ADD CONSTRAINT")));
    }

    #[test]
    fn change_type_non_enum_to_enum_adds_check_constraint() {
        let old_type = FieldType::Text(TextConstraints::unconstrained());
        let new_type = FieldType::Enum(EnumVariants::new(vec!["a".into(), "b".into()]).unwrap());
        let step = MigrationStep::ChangeType {
            name: FieldName::new("status").unwrap(),
            old_type,
            new_type,
            transform: schema_forge_core::migration::ValueTransform::Identity,
        };
        let stmts = migration_step_to_sql("Thing", &step);
        assert!(stmts.iter().any(|s| s.contains("ADD CONSTRAINT \"chk_Thing_status_enum\"")));
        assert!(!stmts.iter().any(|s| s.contains("DROP CONSTRAINT")));
    }

    #[test]
    fn change_type_text_to_integer_still_alters_column() {
        let step = MigrationStep::ChangeType {
            name: FieldName::new("count").unwrap(),
            old_type: FieldType::Text(TextConstraints::unconstrained()),
            new_type: FieldType::Integer(IntegerConstraints::unconstrained()),
            transform: schema_forge_core::migration::ValueTransform::Identity,
        };
        let stmts = migration_step_to_sql("Thing", &step);
        assert_eq!(stmts.len(), 1);
        assert!(stmts[0].contains("ALTER COLUMN \"count\" TYPE BIGINT"));
    }

    #[test]
    fn escape_single_quotes() {
        assert_eq!(escape_sql_string("it's"), "it''s");
    }

    #[test]
    fn add_unique_global() {
        let step = MigrationStep::AddUnique {
            field: FieldName::new("email").unwrap(),
            per_tenant: false,
        };
        let stmts = migration_step_to_sql("Contact", &step);
        assert_eq!(
            stmts,
            vec![
                "ALTER TABLE \"Contact\" ADD CONSTRAINT \"uq_Contact_email\" UNIQUE (\"email\");"
            ]
        );
    }

    #[test]
    fn add_unique_per_tenant() {
        let step = MigrationStep::AddUnique {
            field: FieldName::new("email").unwrap(),
            per_tenant: true,
        };
        let stmts = migration_step_to_sql("Contact", &step);
        assert_eq!(
            stmts,
            vec![
                "CREATE UNIQUE INDEX IF NOT EXISTS \"uq_Contact_email\" ON \"Contact\" (\"_tenant\", \"email\");"
            ]
        );
    }

    #[test]
    fn remove_unique_global() {
        let step = MigrationStep::RemoveUnique {
            field: FieldName::new("email").unwrap(),
            per_tenant: false,
        };
        let stmts = migration_step_to_sql("Contact", &step);
        assert_eq!(
            stmts,
            vec!["ALTER TABLE \"Contact\" DROP CONSTRAINT IF EXISTS \"uq_Contact_email\";"]
        );
    }

    #[test]
    fn remove_unique_per_tenant() {
        let step = MigrationStep::RemoveUnique {
            field: FieldName::new("email").unwrap(),
            per_tenant: true,
        };
        let stmts = migration_step_to_sql("Contact", &step);
        assert_eq!(stmts, vec!["DROP INDEX IF EXISTS \"uq_Contact_email\";"]);
    }
}
