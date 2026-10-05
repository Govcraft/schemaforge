use schema_forge_core::migration::{DiffEngine, MigrationStep};

fn schema(source: &str) -> schema_forge_core::types::SchemaDefinition {
    schema_forge_dsl::parse(source).unwrap().remove(0)
}

#[test]
fn declared_rename_preserves_data_plan_and_becomes_noop() {
    let old = schema("schema Line { number: text required unique }");
    let new =
        schema(r#"schema Line { business_number: text required unique @renamed_from("number") }"#);
    DiffEngine::validate_transition(&old, &new).unwrap();
    let plan = DiffEngine::plan_update(&old, &new).unwrap();
    assert!(matches!(
        plan.steps.as_slice(),
        [MigrationStep::RenameField { .. }]
    ));
    DiffEngine::validate_transition(&new, &new).unwrap();
    assert!(DiffEngine::diff(&new, &new).is_empty());
    let printed = schema_forge_dsl::print_all(std::slice::from_ref(&new));
    assert_eq!(schema(&printed).fields, new.fields);
}

#[test]
fn invalid_rename_hints_fail_before_migration() {
    let old = schema("schema Line { number: text other: text }");
    for source in [
        r#"schema Line { business_number: text @renamed_from("missing") }"#,
        r#"schema Line { number: text @renamed_from("number") }"#,
        r#"schema Line { number: text other: text @renamed_from("number") }"#,
        r#"schema Line { first: text @renamed_from("number") second: text @renamed_from("number") }"#,
    ] {
        assert!(
            DiffEngine::plan_update(&old, &schema(source)).is_err(),
            "{source}"
        );
    }
}

#[test]
fn all_tenant_transitions_require_explicit_manual_migration() {
    let annotations = [
        "",
        "@tenant(root)",
        r#"@tenant(parent: "Org")"#,
        r#"@tenant(parent: "Other")"#,
    ];
    for old in annotations {
        for new in annotations {
            let old_schema = schema(&format!("{old} schema Contact {{ phone: text unique }}"));
            let new_schema = schema(&format!("{new} schema Contact {{ phone: text unique }}"));
            assert_eq!(
                DiffEngine::plan_update(&old_schema, &new_schema).is_ok(),
                old == new
            );
        }
    }
}

#[test]
fn cel_defaults_are_not_implicitly_evaluated_for_existing_rows() {
    let old = schema("schema Widget { name: text }");
    for expression in ["0", "now()", "fields.other"] {
        let new = schema(&format!(
            r#"schema Widget {{ name: text priority: integer required @default("{expression}") }}"#
        ));
        let error = DiffEngine::plan_update(&old, &new).unwrap_err();
        assert!(error
            .to_string()
            .contains("CEL @default expressions are not evaluated"));
        assert!(!DiffEngine::create_new(&new).is_empty());
        assert!(DiffEngine::plan_update(&new, &new).unwrap().is_empty());
    }
}
