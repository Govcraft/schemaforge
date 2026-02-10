use proptest::prelude::*;
use schema_forge_dsl::{parse, print};

/// Strategy for generating valid PascalCase schema names.
fn pascal_case_name() -> impl Strategy<Value = String> {
    "[A-Z][a-zA-Z0-9]{0,15}"
}

/// Strategy for generating valid snake_case field names.
fn snake_case_name() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9_]{0,15}".prop_filter("not a keyword", |s| {
        !matches!(
            s.as_str(),
            "text"
                | "richtext"
                | "integer"
                | "float"
                | "boolean"
                | "datetime"
                | "enum"
                | "json"
                | "composite"
                | "required"
                | "indexed"
                | "default"
                | "true"
                | "false"
                | "schema"
        )
    })
}

/// Strategy for generating a simple field type keyword.
fn simple_type() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("text".to_string()),
        Just("richtext".to_string()),
        Just("integer".to_string()),
        Just("float".to_string()),
        Just("boolean".to_string()),
        Just("datetime".to_string()),
        Just("json".to_string()),
    ]
}

proptest! {
    /// Parsing a valid minimal schema should never fail.
    #[test]
    fn valid_minimal_schema_always_parses(
        name in pascal_case_name(),
        field in snake_case_name(),
        ft in simple_type(),
    ) {
        let source = format!("schema {name} {{ {field}: {ft} }}");
        let result = parse(&source);
        prop_assert!(result.is_ok(), "Failed to parse: {source}");
        let schemas = result.unwrap();
        prop_assert_eq!(schemas.len(), 1);
        prop_assert_eq!(schemas[0].name.as_str(), &name);
    }

    /// Lexer should never panic on arbitrary input.
    #[test]
    fn lexer_never_panics(input in "\\PC{0,200}") {
        // This may succeed or fail, but should never panic.
        let _ = parse(&input);
    }

    /// Parse then print then parse should produce structurally equivalent ASTs.
    #[test]
    fn round_trip_property(
        name in pascal_case_name(),
        field1 in snake_case_name(),
        field2_suffix in "[a-z][a-z0-9]{0,5}",
        ft1 in simple_type(),
        ft2 in simple_type(),
    ) {
        // Ensure field names are distinct
        let field2 = format!("{field2_suffix}_x");
        if field1 == field2 {
            return Ok(());
        }

        let source = format!("schema {name} {{ {field1}: {ft1}\n{field2}: {ft2} }}");
        let result = parse(&source);
        if let Ok(schemas) = result {
            let printed = print(&schemas[0]);
            let result2 = parse(&printed);
            prop_assert!(result2.is_ok(), "Re-parse failed for:\n{printed}");
            let schemas2 = result2.unwrap();

            // Compare structurally (ignoring SchemaId which is random)
            prop_assert_eq!(&schemas[0].name, &schemas2[0].name);
            prop_assert_eq!(schemas[0].fields.len(), schemas2[0].fields.len());
            for (f1, f2) in schemas[0].fields.iter().zip(schemas2[0].fields.iter()) {
                prop_assert_eq!(&f1.name, &f2.name);
                prop_assert_eq!(&f1.field_type, &f2.field_type);
                prop_assert_eq!(&f1.modifiers, &f2.modifiers);
            }
        }
    }

    /// Enum variants with valid strings should always parse.
    #[test]
    fn enum_with_valid_variants_parses(
        v1 in "[a-z]{1,10}",
        v2 in "[a-z]{1,10}",
    ) {
        if v1 == v2 {
            return Ok(());
        }
        let source = format!(r#"schema S {{ status: enum("{v1}", "{v2}") }}"#);
        let result = parse(&source);
        prop_assert!(result.is_ok(), "Failed to parse: {source}");
    }

    /// Integer constraints with valid ranges should always parse.
    #[test]
    fn integer_constraints_valid_range(
        min in -1000i64..1000,
        max in -1000i64..1000,
    ) {
        if min > max {
            return Ok(());
        }
        let source = format!("schema S {{ x: integer(min: {min}, max: {max}) }}");
        let result = parse(&source);
        prop_assert!(result.is_ok(), "Failed to parse: {source}");
    }
}
