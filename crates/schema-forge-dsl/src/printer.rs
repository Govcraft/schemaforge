use schema_forge_core::types::{
    Annotation, Cardinality, DefaultValue, FieldDefinition, FieldModifier, FieldType,
    SchemaDefinition,
};

/// Print a single schema definition to DSL text.
///
/// The output is formatted with 4-space indentation and follows the
/// SchemaDSL grammar specification exactly, enabling round-trip fidelity.
pub fn print(schema: &SchemaDefinition) -> String {
    let mut output = String::new();
    print_schema(schema, &mut output);
    output
}

/// Print multiple schema definitions to DSL text, separated by blank lines.
pub fn print_all(schemas: &[SchemaDefinition]) -> String {
    let mut output = String::new();
    for (i, schema) in schemas.iter().enumerate() {
        if i > 0 {
            output.push('\n');
        }
        print_schema(schema, &mut output);
    }
    output
}

fn print_schema(schema: &SchemaDefinition, output: &mut String) {
    for annotation in &schema.annotations {
        print_annotation(annotation, output);
        output.push('\n');
    }

    output.push_str("schema ");
    output.push_str(schema.name.as_str());
    output.push_str(" {\n");

    for field in &schema.fields {
        output.push_str("    ");
        print_field(field, output, 1);
        output.push('\n');
    }

    output.push_str("}\n");
}

fn print_annotation(annotation: &Annotation, output: &mut String) {
    match annotation {
        Annotation::Version { version } => {
            output.push_str(&format!("@version({})", version.get()));
        }
        Annotation::Display { field } => {
            output.push_str(&format!("@display(\"{}\")", field.as_str()));
        }
        _ => {
            // Future annotation kinds -- print as @unknown for forward compatibility
            output.push_str("@unknown");
        }
    }
}

fn print_field(field: &FieldDefinition, output: &mut String, depth: usize) {
    output.push_str(field.name.as_str());
    output.push_str(": ");
    print_type(&field.field_type, output, depth);

    for modifier in &field.modifiers {
        output.push(' ');
        print_modifier(modifier, output);
    }
}

fn print_type(field_type: &FieldType, output: &mut String, depth: usize) {
    match field_type {
        FieldType::Text(constraints) => {
            output.push_str("text");
            if let Some(max) = constraints.max_length {
                output.push_str(&format!("(max: {max})"));
            }
        }
        FieldType::RichText => output.push_str("richtext"),
        FieldType::Integer(constraints) => {
            output.push_str("integer");
            let params = build_integer_params(constraints);
            if !params.is_empty() {
                output.push('(');
                output.push_str(&params.join(", "));
                output.push(')');
            }
        }
        FieldType::Float(constraints) => {
            output.push_str("float");
            if let Some(precision) = constraints.precision {
                output.push_str(&format!("(precision: {precision})"));
            }
        }
        FieldType::Boolean => output.push_str("boolean"),
        FieldType::DateTime => output.push_str("datetime"),
        FieldType::Enum(variants) => {
            output.push_str("enum(");
            for (i, variant) in variants.iter().enumerate() {
                if i > 0 {
                    output.push_str(", ");
                }
                output.push('"');
                output.push_str(variant);
                output.push('"');
            }
            output.push(')');
        }
        FieldType::Json => output.push_str("json"),
        FieldType::Relation {
            target,
            cardinality,
        } => {
            output.push_str("-> ");
            output.push_str(target.as_str());
            if *cardinality == Cardinality::Many {
                output.push_str("[]");
            }
        }
        FieldType::Array(inner) => {
            print_type(inner, output, depth);
            output.push_str("[]");
        }
        FieldType::Composite(fields) => {
            output.push_str("composite {\n");
            let indent = "    ".repeat(depth + 1);
            for field in fields {
                output.push_str(&indent);
                print_field(field, output, depth + 1);
                output.push('\n');
            }
            output.push_str(&"    ".repeat(depth));
            output.push('}');
        }
        _ => {
            // Future field types -- print as unknown for forward compatibility
            output.push_str("unknown");
        }
    }
}

fn build_integer_params(
    constraints: &schema_forge_core::types::IntegerConstraints,
) -> Vec<String> {
    let mut params = Vec::new();
    if let Some(min) = constraints.min {
        params.push(format!("min: {min}"));
    }
    if let Some(max) = constraints.max {
        params.push(format!("max: {max}"));
    }
    params
}

fn print_modifier(modifier: &FieldModifier, output: &mut String) {
    match modifier {
        FieldModifier::Required => output.push_str("required"),
        FieldModifier::Indexed => output.push_str("indexed"),
        FieldModifier::Default { value } => {
            output.push_str("default(");
            print_default_value(value, output);
            output.push(')');
        }
        _ => {
            // Future modifier kinds
            output.push_str("unknown_modifier");
        }
    }
}

fn print_default_value(value: &DefaultValue, output: &mut String) {
    match value {
        DefaultValue::String(s) => {
            output.push('"');
            output.push_str(s);
            output.push('"');
        }
        DefaultValue::Integer(n) => output.push_str(&n.to_string()),
        DefaultValue::Float(s) => output.push_str(s),
        DefaultValue::Boolean(b) => output.push_str(if *b { "true" } else { "false" }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use schema_forge_core::types::{
        EnumVariants, FieldName, FloatConstraints, IntegerConstraints, SchemaId, SchemaName,
        SchemaVersion, TextConstraints,
    };

    fn make_schema(
        name: &str,
        fields: Vec<FieldDefinition>,
        annotations: Vec<Annotation>,
    ) -> SchemaDefinition {
        SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new(name).unwrap(),
            fields,
            annotations,
        )
        .unwrap()
    }

    fn make_field(name: &str, ft: FieldType) -> FieldDefinition {
        FieldDefinition::new(FieldName::new(name).unwrap(), ft)
    }

    fn make_field_with_mods(
        name: &str,
        ft: FieldType,
        mods: Vec<FieldModifier>,
    ) -> FieldDefinition {
        FieldDefinition::with_modifiers(FieldName::new(name).unwrap(), ft, mods)
    }

    #[test]
    fn print_minimal_schema() {
        let schema = make_schema(
            "Contact",
            vec![make_field("name", FieldType::Text(TextConstraints::unconstrained()))],
            vec![],
        );
        let output = print(&schema);
        assert_eq!(output, "schema Contact {\n    name: text\n}\n");
    }

    #[test]
    fn print_text_with_max() {
        let schema = make_schema(
            "S",
            vec![make_field(
                "name",
                FieldType::Text(TextConstraints::with_max_length(255)),
            )],
            vec![],
        );
        let output = print(&schema);
        assert!(output.contains("text(max: 255)"));
    }

    #[test]
    fn print_integer_with_range() {
        let schema = make_schema(
            "S",
            vec![make_field(
                "score",
                FieldType::Integer(IntegerConstraints::with_range(0, 100).unwrap()),
            )],
            vec![],
        );
        let output = print(&schema);
        assert!(output.contains("integer(min: 0, max: 100)"));
    }

    #[test]
    fn print_integer_min_only() {
        let schema = make_schema(
            "S",
            vec![make_field(
                "count",
                FieldType::Integer(IntegerConstraints::with_min(1)),
            )],
            vec![],
        );
        let output = print(&schema);
        assert!(output.contains("integer(min: 1)"));
    }

    #[test]
    fn print_float_with_precision() {
        let schema = make_schema(
            "S",
            vec![make_field(
                "price",
                FieldType::Float(FloatConstraints::with_precision(2)),
            )],
            vec![],
        );
        let output = print(&schema);
        assert!(output.contains("float(precision: 2)"));
    }

    #[test]
    fn print_enum() {
        let schema = make_schema(
            "S",
            vec![make_field(
                "status",
                FieldType::Enum(
                    EnumVariants::new(vec!["active".into(), "inactive".into()]).unwrap(),
                ),
            )],
            vec![],
        );
        let output = print(&schema);
        assert!(output.contains(r#"enum("active", "inactive")"#));
    }

    #[test]
    fn print_relation_one() {
        let schema = make_schema(
            "S",
            vec![make_field(
                "company",
                FieldType::Relation {
                    target: SchemaName::new("Company").unwrap(),
                    cardinality: Cardinality::One,
                },
            )],
            vec![],
        );
        let output = print(&schema);
        assert!(output.contains("-> Company"));
        assert!(!output.contains("[]"));
    }

    #[test]
    fn print_relation_many() {
        let schema = make_schema(
            "S",
            vec![make_field(
                "contacts",
                FieldType::Relation {
                    target: SchemaName::new("Contact").unwrap(),
                    cardinality: Cardinality::Many,
                },
            )],
            vec![],
        );
        let output = print(&schema);
        assert!(output.contains("-> Contact[]"));
    }

    #[test]
    fn print_array() {
        let schema = make_schema(
            "S",
            vec![make_field(
                "tags",
                FieldType::Array(Box::new(FieldType::Text(TextConstraints::unconstrained()))),
            )],
            vec![],
        );
        let output = print(&schema);
        assert!(output.contains("text[]"));
    }

    #[test]
    fn print_composite() {
        let schema = make_schema(
            "S",
            vec![make_field(
                "address",
                FieldType::Composite(vec![
                    make_field("street", FieldType::Text(TextConstraints::unconstrained())),
                    make_field_with_mods(
                        "city",
                        FieldType::Text(TextConstraints::unconstrained()),
                        vec![FieldModifier::Required],
                    ),
                ]),
            )],
            vec![],
        );
        let output = print(&schema);
        assert!(output.contains("composite {"));
        assert!(output.contains("        street: text"));
        assert!(output.contains("        city: text required"));
    }

    #[test]
    fn print_modifiers() {
        let schema = make_schema(
            "S",
            vec![make_field_with_mods(
                "email",
                FieldType::Text(TextConstraints::with_max_length(255)),
                vec![FieldModifier::Required, FieldModifier::Indexed],
            )],
            vec![],
        );
        let output = print(&schema);
        assert!(output.contains("text(max: 255) required indexed"));
    }

    #[test]
    fn print_default_string() {
        let schema = make_schema(
            "S",
            vec![make_field_with_mods(
                "status",
                FieldType::Text(TextConstraints::unconstrained()),
                vec![FieldModifier::Default {
                    value: DefaultValue::String("active".into()),
                }],
            )],
            vec![],
        );
        let output = print(&schema);
        assert!(output.contains(r#"default("active")"#));
    }

    #[test]
    fn print_default_integer() {
        let schema = make_schema(
            "S",
            vec![make_field_with_mods(
                "count",
                FieldType::Integer(IntegerConstraints::unconstrained()),
                vec![FieldModifier::Default {
                    value: DefaultValue::Integer(42),
                }],
            )],
            vec![],
        );
        let output = print(&schema);
        assert!(output.contains("default(42)"));
    }

    #[test]
    fn print_default_boolean() {
        let schema = make_schema(
            "S",
            vec![make_field_with_mods(
                "active",
                FieldType::Boolean,
                vec![FieldModifier::Default {
                    value: DefaultValue::Boolean(true),
                }],
            )],
            vec![],
        );
        let output = print(&schema);
        assert!(output.contains("default(true)"));
    }

    #[test]
    fn print_annotations() {
        let schema = make_schema(
            "Deal",
            vec![make_field("name", FieldType::Text(TextConstraints::unconstrained()))],
            vec![
                Annotation::Version {
                    version: SchemaVersion::new(2).unwrap(),
                },
                Annotation::Display {
                    field: FieldName::new("name").unwrap(),
                },
            ],
        );
        let output = print(&schema);
        assert!(output.starts_with("@version(2)\n@display(\"name\")\nschema Deal {"));
    }

    #[test]
    fn print_simple_types() {
        let schema = make_schema(
            "S",
            vec![
                make_field("a", FieldType::RichText),
                make_field("b", FieldType::Boolean),
                make_field("c", FieldType::DateTime),
                make_field("d", FieldType::Json),
            ],
            vec![],
        );
        let output = print(&schema);
        assert!(output.contains("a: richtext"));
        assert!(output.contains("b: boolean"));
        assert!(output.contains("c: datetime"));
        assert!(output.contains("d: json"));
    }

    #[test]
    fn print_all_schemas() {
        let schemas = vec![
            make_schema(
                "Contact",
                vec![make_field("name", FieldType::Text(TextConstraints::unconstrained()))],
                vec![],
            ),
            make_schema(
                "Company",
                vec![make_field("name", FieldType::Text(TextConstraints::unconstrained()))],
                vec![],
            ),
        ];
        let output = print_all(&schemas);
        assert!(output.contains("schema Contact {"));
        assert!(output.contains("schema Company {"));
    }
}
