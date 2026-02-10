use proptest::prelude::*;
use schema_forge_core::types::{
    EnumVariants, FieldName, IntegerConstraints, SchemaName, SchemaVersion,
};

proptest! {
    #[test]
    fn schema_name_display_roundtrip(s in "[A-Z][a-zA-Z0-9]{0,30}") {
        let name = SchemaName::new(&s).unwrap();
        let displayed = name.to_string();
        let back = SchemaName::new(displayed).unwrap();
        prop_assert_eq!(name, back);
    }

    #[test]
    fn field_name_display_roundtrip(s in "[a-z][a-z0-9_]{0,30}") {
        let name = FieldName::new(&s).unwrap();
        let displayed = name.to_string();
        let back = FieldName::new(displayed).unwrap();
        prop_assert_eq!(name, back);
    }

    #[test]
    fn schema_version_valid_range(v in 1u32..=u32::MAX) {
        let version = SchemaVersion::new(v).unwrap();
        prop_assert_eq!(version.get(), v);
    }

    #[test]
    fn schema_version_serde_roundtrip(v in 1u32..=10000u32) {
        let version = SchemaVersion::new(v).unwrap();
        let json = serde_json::to_string(&version).unwrap();
        let back: SchemaVersion = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(version, back);
    }

    #[test]
    fn schema_name_rejects_lowercase_start(s in "[a-z][a-zA-Z0-9]{0,30}") {
        prop_assert!(SchemaName::new(&s).is_err());
    }

    #[test]
    fn field_name_rejects_uppercase_start(s in "[A-Z][a-z0-9_]{0,30}") {
        prop_assert!(FieldName::new(&s).is_err());
    }

    #[test]
    fn integer_constraints_min_le_max(min in -1000i64..=1000i64, max in -1000i64..=1000i64) {
        let result = IntegerConstraints::with_range(min, max);
        if min <= max {
            let c = result.unwrap();
            prop_assert_eq!(c.min, Some(min));
            prop_assert_eq!(c.max, Some(max));
        } else {
            prop_assert!(result.is_err());
        }
    }

    #[test]
    fn enum_variants_always_nonempty(
        variants in prop::collection::vec("[A-Za-z][A-Za-z0-9]{0,10}", 1..=10)
    ) {
        // Deduplicate for the test input
        let mut unique = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for v in variants {
            if seen.insert(v.clone()) {
                unique.push(v);
            }
        }
        let ev = EnumVariants::new(unique).unwrap();
        prop_assert!(!ev.is_empty());
        prop_assert!(!ev.is_empty());
    }

    #[test]
    fn schema_name_serde_roundtrip(s in "[A-Z][a-zA-Z0-9]{0,30}") {
        let name = SchemaName::new(&s).unwrap();
        let json = serde_json::to_string(&name).unwrap();
        let back: SchemaName = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(name, back);
    }

    #[test]
    fn field_name_serde_roundtrip(s in "[a-z][a-z0-9_]{0,30}") {
        let name = FieldName::new(&s).unwrap();
        let json = serde_json::to_string(&name).unwrap();
        let back: FieldName = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(name, back);
    }
}
