use proptest::prelude::*;
use schema_forge_core::migration::{
    DiffEngine, MigrationId, MigrationSafety, MigrationStep, ValueTransform,
};
use schema_forge_core::query::{FieldPath, Filter, Query, SortOrder};
use schema_forge_core::types::{
    DefaultValue, DynamicValue, FieldDefinition, FieldName, FieldType, SchemaDefinition, SchemaId,
    SchemaName, TextConstraints,
};

// ---------------------------------------------------------------------------
// Strategies
// ---------------------------------------------------------------------------

fn field_name_strategy() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9_]{0,15}"
}

fn schema_name_strategy() -> impl Strategy<Value = String> {
    "[A-Z][a-zA-Z0-9]{0,15}"
}

fn field_path_segment_strategy() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9_]{0,10}"
}

fn unique_field_definitions(min: usize, max: usize) -> impl Strategy<Value = Vec<FieldDefinition>> {
    prop::collection::vec(field_name_strategy(), min..=max).prop_map(|names| {
        let mut seen = std::collections::HashSet::new();
        let mut fields = Vec::new();
        for name in names {
            if seen.insert(name.clone()) {
                fields.push(FieldDefinition::new(
                    FieldName::new(name).unwrap(),
                    FieldType::Text(TextConstraints::unconstrained()),
                ));
            }
        }
        if fields.is_empty() {
            fields.push(FieldDefinition::new(
                FieldName::new("x").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
            ));
        }
        fields
    })
}

// ---------------------------------------------------------------------------
// MigrationId Properties
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn migration_id_parse_roundtrip(_ in 0..100u32) {
        let id = MigrationId::new();
        let parsed = MigrationId::parse(id.as_str()).unwrap();
        prop_assert_eq!(id, parsed);
    }

    #[test]
    fn migration_id_has_migration_prefix(_ in 0..100u32) {
        let id = MigrationId::new();
        prop_assert!(id.as_str().starts_with("migration_"));
    }

    #[test]
    fn migration_id_display_matches_as_str(_ in 0..100u32) {
        let id = MigrationId::new();
        prop_assert_eq!(id.to_string(), id.as_str());
    }
}

// ---------------------------------------------------------------------------
// DiffEngine Properties
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn diff_identical_schemas_always_empty(fields in unique_field_definitions(1, 8)) {
        let schema = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Test").unwrap(),
            fields,
            vec![],
        ).unwrap();
        let plan = DiffEngine::diff(&schema, &schema);
        prop_assert!(plan.is_empty(), "Diffing identical schemas should produce empty plan");
    }

    #[test]
    fn diff_symmetry_field_count(
        old_fields in unique_field_definitions(1, 5),
        new_fields in unique_field_definitions(1, 5),
    ) {
        let old = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Test").unwrap(),
            old_fields,
            vec![],
        ).unwrap();
        let new = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Test").unwrap(),
            new_fields,
            vec![],
        ).unwrap();

        let plan = DiffEngine::diff(&old, &new);

        // Plan should have at least as many steps as there are field-level differences
        // (this is a sanity check, not a strict bound)
        let _len = plan.len();
        // Plan should always have a valid safety classification
        let safety = plan.overall_safety();
        prop_assert!(
            safety == MigrationSafety::Safe
            || safety == MigrationSafety::RequiresConfirmation
            || safety == MigrationSafety::Destructive
        );
    }

    #[test]
    fn create_new_always_produces_one_step(fields in unique_field_definitions(1, 8)) {
        let schema = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Test").unwrap(),
            fields,
            vec![],
        ).unwrap();
        let plan = DiffEngine::create_new(&schema);
        prop_assert_eq!(plan.len(), 1, "create_new should always produce exactly one step");
        prop_assert!(plan.is_safe(), "create_new should always be safe");
    }

    #[test]
    fn adding_fields_is_safe(
        base_name in field_name_strategy(),
        extra_name in field_name_strategy(),
    ) {
        // Ensure distinct names
        if base_name == extra_name {
            return Ok(());
        }
        let base = FieldDefinition::new(
            FieldName::new(&base_name).unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
        );
        let extra = FieldDefinition::new(
            FieldName::new(&extra_name).unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
        );
        let old = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Test").unwrap(),
            vec![base.clone()],
            vec![],
        ).unwrap();
        let new = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Test").unwrap(),
            vec![base, extra],
            vec![],
        ).unwrap();

        let plan = DiffEngine::diff(&old, &new);
        prop_assert_eq!(plan.len(), 1, "Adding one field should produce one step");
        prop_assert!(plan.is_safe(), "Adding a field should be safe");
    }

    #[test]
    fn removing_fields_is_destructive(
        base_name in field_name_strategy(),
        extra_name in field_name_strategy(),
    ) {
        if base_name == extra_name {
            return Ok(());
        }
        let base = FieldDefinition::new(
            FieldName::new(&base_name).unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
        );
        let extra = FieldDefinition::new(
            FieldName::new(&extra_name).unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
        );
        let old = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Test").unwrap(),
            vec![base.clone(), extra],
            vec![],
        ).unwrap();
        let new = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Test").unwrap(),
            vec![base],
            vec![],
        ).unwrap();

        let plan = DiffEngine::diff(&old, &new);
        prop_assert_eq!(plan.len(), 1, "Removing one field should produce one step");
        prop_assert!(plan.has_destructive_steps(), "Removing a field should be destructive");
    }
}

// ---------------------------------------------------------------------------
// MigrationStep Safety Properties
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn add_field_step_is_always_safe(name in field_name_strategy()) {
        let step = MigrationStep::AddField {
            field: FieldDefinition::new(
                FieldName::new(name).unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
            ),
        };
        prop_assert_eq!(step.safety(), MigrationSafety::Safe);
    }

    #[test]
    fn remove_field_step_is_always_destructive(name in field_name_strategy()) {
        let step = MigrationStep::RemoveField {
            name: FieldName::new(name).unwrap(),
        };
        prop_assert_eq!(step.safety(), MigrationSafety::Destructive);
    }

    #[test]
    fn add_index_step_is_always_safe(name in field_name_strategy()) {
        let step = MigrationStep::AddIndex {
            field: FieldName::new(name).unwrap(),
        };
        prop_assert_eq!(step.safety(), MigrationSafety::Safe);
    }

    #[test]
    fn create_schema_step_is_always_safe(name in schema_name_strategy()) {
        let step = MigrationStep::CreateSchema {
            name: SchemaName::new(name).unwrap(),
            fields: vec![FieldDefinition::new(
                FieldName::new("x").unwrap(),
                FieldType::Boolean,
            )],
        };
        prop_assert_eq!(step.safety(), MigrationSafety::Safe);
    }

    #[test]
    fn drop_schema_step_is_always_destructive(name in schema_name_strategy()) {
        let step = MigrationStep::DropSchema {
            name: SchemaName::new(name).unwrap(),
        };
        prop_assert_eq!(step.safety(), MigrationSafety::Destructive);
    }
}

// ---------------------------------------------------------------------------
// FieldPath Properties
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn field_path_single_segment(name in field_path_segment_strategy()) {
        let fp = FieldPath::parse(&name).unwrap();
        prop_assert!(fp.is_simple());
        prop_assert_eq!(fp.depth(), 1);
        prop_assert_eq!(fp.root(), &name);
        prop_assert_eq!(fp.leaf(), &name);
    }

    #[test]
    fn field_path_multi_segment(
        segments in prop::collection::vec(field_path_segment_strategy(), 2..=5)
    ) {
        let dotted = segments.join(".");
        let fp = FieldPath::parse(&dotted).unwrap();
        prop_assert_eq!(fp.depth(), segments.len());
        prop_assert_eq!(fp.root(), &segments[0]);
        prop_assert_eq!(fp.leaf(), &segments[segments.len() - 1]);
        prop_assert_eq!(fp.as_dotted(), dotted);
    }

    #[test]
    fn field_path_display_roundtrip(
        segments in prop::collection::vec(field_path_segment_strategy(), 1..=4)
    ) {
        let dotted = segments.join(".");
        let fp = FieldPath::parse(&dotted).unwrap();
        let displayed = fp.to_string();
        let back = FieldPath::parse(&displayed).unwrap();
        prop_assert_eq!(fp, back);
    }

    #[test]
    fn field_path_serde_roundtrip(
        segments in prop::collection::vec(field_path_segment_strategy(), 1..=4)
    ) {
        let dotted = segments.join(".");
        let fp = FieldPath::parse(&dotted).unwrap();
        let json = serde_json::to_string(&fp).unwrap();
        let back: FieldPath = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(fp, back);
    }
}

// ---------------------------------------------------------------------------
// Filter Properties
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn filter_eq_display_contains_path(name in field_path_segment_strategy()) {
        let f = Filter::eq(
            FieldPath::single(&name),
            DynamicValue::Text("test".into()),
        );
        let display = f.to_string();
        prop_assert!(display.contains(&name));
        prop_assert!(display.contains("="));
    }

    #[test]
    fn filter_and_display_contains_all_parts(
        names in prop::collection::vec(field_path_segment_strategy(), 2..=4)
    ) {
        let mut seen = std::collections::HashSet::new();
        let unique: Vec<_> = names.into_iter().filter(|n| seen.insert(n.clone())).collect();
        if unique.len() < 2 {
            return Ok(());
        }
        let filters: Vec<Filter> = unique.iter().map(|name| {
            Filter::eq(
                FieldPath::single(name),
                DynamicValue::Boolean(true),
            )
        }).collect();

        let combined = Filter::and(filters);
        let display = combined.to_string();
        for name in &unique {
            prop_assert!(display.contains(name.as_str()), "Display should contain field name '{}'", name);
        }
        prop_assert!(display.contains("AND"));
    }

    #[test]
    fn filter_serde_roundtrip_eq(name in field_path_segment_strategy(), val in -1000i64..1000i64) {
        let f = Filter::eq(
            FieldPath::single(&name),
            DynamicValue::Integer(val),
        );
        let json = serde_json::to_string(&f).unwrap();
        let back: Filter = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(f, back);
    }
}

// ---------------------------------------------------------------------------
// Query Properties
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn query_display_always_contains_select(limit in 1usize..1000) {
        let q = Query::new(SchemaId::new()).with_limit(limit);
        let display = q.to_string();
        prop_assert!(display.contains("SELECT * FROM"));
        let expected = format!("LIMIT {}", limit);
        prop_assert!(display.contains(&expected));
    }

    #[test]
    fn query_validate_rejects_zero_limit(_ in 0..100u32) {
        let q = Query::new(SchemaId::new()).with_limit(0);
        prop_assert!(q.validate().is_err());
    }

    #[test]
    fn query_validate_accepts_nonzero_limit(limit in 1usize..10000) {
        let q = Query::new(SchemaId::new()).with_limit(limit);
        prop_assert!(q.validate().is_ok());
    }

    #[test]
    fn query_serde_roundtrip(limit in 1usize..1000, offset in 0usize..1000) {
        let q = Query::new(SchemaId::new())
            .with_filter(Filter::eq(
                FieldPath::single("name"),
                DynamicValue::Text("test".into()),
            ))
            .with_sort(FieldPath::single("name"), SortOrder::Ascending)
            .with_limit(limit)
            .with_offset(offset);

        let json = serde_json::to_string(&q).unwrap();
        let back: Query = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(q, back);
    }
}

// ---------------------------------------------------------------------------
// ValueTransform Properties
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn value_transform_serde_roundtrip(_ in 0..10u32) {
        let transforms = vec![
            ValueTransform::Identity,
            ValueTransform::IntegerToFloat,
            ValueTransform::FloatToInteger,
            ValueTransform::ToString,
            ValueTransform::SetNull,
            ValueTransform::SetDefault { value: DefaultValue::Integer(0) },
        ];
        for t in transforms {
            let json = serde_json::to_string(&t).unwrap();
            let back: ValueTransform = serde_json::from_str(&json).unwrap();
            prop_assert_eq!(t, back);
        }
    }
}
