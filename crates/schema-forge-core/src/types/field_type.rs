use serde::{Deserialize, Serialize};

use super::bytes_constraints::BytesConstraints;
use super::cardinality::Cardinality;
use super::constraint_violation::ConstraintViolation;
use super::dynamic_value::DynamicValue;
use super::enum_variants::EnumVariants;
use super::field_definition::FieldDefinition;
use super::file_constraints::FileConstraints;
use super::float_constraints::FloatConstraints;
use super::integer_constraints::IntegerConstraints;
use super::schema_name::SchemaName;
use super::text_constraints::TextConstraints;

/// The core DSL type system for schema fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
#[non_exhaustive]
pub enum FieldType {
    Text(TextConstraints),
    RichText,
    Integer(IntegerConstraints),
    Float(FloatConstraints),
    Boolean,
    DateTime,
    Duration,
    Bytes(BytesConstraints),
    Enum(EnumVariants),
    Json,
    Relation {
        target: SchemaName,
        cardinality: Cardinality,
    },
    Array(Box<FieldType>),
    Composite(Vec<FieldDefinition>),
    /// A typed, open-keyed map with a homogeneous value type.
    ///
    /// Distinct from [`FieldType::Composite`] (a fixed, declared field set) and
    /// [`FieldType::Json`] (untyped): a `Map` has arbitrary keys but every value
    /// is validated against the single `value` type. CEL surfaces this as a
    /// `map<K, V>` so comprehensions (`all`/`exists`/`map`) work over it.
    ///
    /// `key` is boxed for forward-compatibility, but the DSL currently only
    /// accepts `string` keys — JSON objects, Postgres JSONB, and SurrealDB
    /// objects are all string-keyed, and non-string keys cannot round-trip
    /// through that storage without lossy string key-encoding.
    Map {
        key: Box<FieldType>,
        value: Box<FieldType>,
    },
    File(FileConstraints),
}

impl FieldType {
    /// Check a value against the constraints declared on this type.
    ///
    /// This is the in-process half of constraint enforcement. Every
    /// constraint the DSL can express — `text(max:)`, `integer(min:/max:)`,
    /// `enum(...)`, `bytes(max:)` — also lands as a database CHECK or column
    /// type, but a database refusal arrives as a driver error with no field
    /// name and no allowed-variant list, and the SQLSTATE for a check
    /// violation is indistinguishable from a genuine backend fault. Checking
    /// here lets the caller answer with an actionable 422 instead. See #133.
    ///
    /// Pure and total: never panics, does no I/O, and returns `Ok(())` for
    /// every combination it does not have an opinion about.
    ///
    /// Deliberately silent about:
    /// - `Null` — nullability is the `required` modifier's job, checked
    ///   against the schema's field list rather than against a type.
    /// - Shape mismatches (an `Integer` value on a `Text` field) — conversion
    ///   already rejects those, and re-reporting them here would double up
    ///   the error list.
    /// - `Float` precision — `FloatConstraints::precision` is a display and
    ///   storage hint that no backend currently enforces (see #7); rejecting
    ///   writes on it would be a new restriction, not a fix.
    pub fn check_value(&self, value: &DynamicValue) -> Result<(), ConstraintViolation> {
        match (self, value) {
            (Self::Text(constraints), DynamicValue::Text(s)) => {
                let Some(max) = constraints.max_length else {
                    return Ok(());
                };
                // Characters, not bytes: `max_length` becomes `VARCHAR(n)` on
                // PostgreSQL, which counts characters. Measuring bytes here
                // would reject multi-byte text the database would accept.
                let len = s.chars().count();
                if len > max as usize {
                    return Err(ConstraintViolation::TextTooLong { len, max });
                }
                Ok(())
            }
            (Self::Integer(constraints), DynamicValue::Integer(i)) => {
                if let Some(min) = constraints.min {
                    if *i < min {
                        return Err(ConstraintViolation::IntegerBelowMin { value: *i, min });
                    }
                }
                if let Some(max) = constraints.max {
                    if *i > max {
                        return Err(ConstraintViolation::IntegerAboveMax { value: *i, max });
                    }
                }
                Ok(())
            }
            // A hook or a `@compute` rule can hand back a bare `Text` where
            // the schema declares an enum, so both carriers are checked.
            (Self::Enum(variants), DynamicValue::Enum(s) | DynamicValue::Text(s)) => {
                if variants.iter().any(|v| v == s) {
                    return Ok(());
                }
                Err(ConstraintViolation::NotAnEnumVariant {
                    value: s.clone(),
                    allowed: variants.iter().cloned().collect(),
                })
            }
            (Self::Bytes(constraints), DynamicValue::Bytes(b)) => {
                let Some(max) = constraints.max_size else {
                    return Ok(());
                };
                if b.len() > max {
                    return Err(ConstraintViolation::BytesTooLarge { len: b.len(), max });
                }
                Ok(())
            }
            // Containers carry the constraint on their element type, so the
            // check recurses rather than stopping at the outer value.
            (Self::Array(inner), DynamicValue::Array(items)) => {
                items.iter().try_for_each(|item| inner.check_value(item))
            }
            (Self::Map { value: vt, .. }, DynamicValue::Map(entries)) => {
                entries.values().try_for_each(|v| vt.check_value(v))
            }
            _ => Ok(()),
        }
    }
}

impl std::fmt::Display for FieldType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Text(_) => write!(f, "Text"),
            Self::RichText => write!(f, "RichText"),
            Self::Integer(_) => write!(f, "Integer"),
            Self::Float(_) => write!(f, "Float"),
            Self::Boolean => write!(f, "Boolean"),
            Self::DateTime => write!(f, "DateTime"),
            Self::Duration => write!(f, "Duration"),
            Self::Bytes(_) => write!(f, "Bytes"),
            Self::Enum(v) => write!(f, "Enum{v}"),
            Self::Json => write!(f, "Json"),
            Self::Relation {
                target,
                cardinality,
            } => write!(f, "Relation({target}, {cardinality})"),
            Self::Array(inner) => write!(f, "Array<{inner}>"),
            Self::Composite(fields) => write!(f, "Composite({} fields)", fields.len()),
            Self::Map { key, value } => write!(f, "Map<{key}, {value}>"),
            Self::File(_) => write!(f, "File"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_simple_types() {
        assert_eq!(FieldType::Boolean.to_string(), "Boolean");
        assert_eq!(FieldType::DateTime.to_string(), "DateTime");
        assert_eq!(FieldType::Duration.to_string(), "Duration");
        assert_eq!(FieldType::RichText.to_string(), "RichText");
        assert_eq!(FieldType::Json.to_string(), "Json");
    }

    #[test]
    fn display_text() {
        let t = FieldType::Text(TextConstraints::with_max_length(255));
        assert_eq!(t.to_string(), "Text");
    }

    #[test]
    fn display_bytes() {
        assert_eq!(
            FieldType::Bytes(BytesConstraints::unconstrained()).to_string(),
            "Bytes"
        );
        assert_eq!(
            FieldType::Bytes(BytesConstraints::with_max_size(1024)).to_string(),
            "Bytes"
        );
    }

    #[test]
    fn serde_roundtrip_bytes() {
        for ft in [
            FieldType::Bytes(BytesConstraints::unconstrained()),
            FieldType::Bytes(BytesConstraints::with_max_size(1024)),
        ] {
            let json = serde_json::to_string(&ft).unwrap();
            let back: FieldType = serde_json::from_str(&json).unwrap();
            assert_eq!(ft, back);
        }
    }

    #[test]
    fn display_map() {
        let t = FieldType::Map {
            key: Box::new(FieldType::Text(TextConstraints::unconstrained())),
            value: Box::new(FieldType::Integer(IntegerConstraints::unconstrained())),
        };
        assert_eq!(t.to_string(), "Map<Text, Integer>");
    }

    #[test]
    fn serde_roundtrip_map() {
        for value in [
            FieldType::Integer(IntegerConstraints::unconstrained()),
            FieldType::Text(TextConstraints::unconstrained()),
        ] {
            let ft = FieldType::Map {
                key: Box::new(FieldType::Text(TextConstraints::unconstrained())),
                value: Box::new(value),
            };
            let json = serde_json::to_string(&ft).unwrap();
            let back: FieldType = serde_json::from_str(&json).unwrap();
            assert_eq!(ft, back);
        }
    }

    #[test]
    fn display_relation() {
        let t = FieldType::Relation {
            target: SchemaName::new("Company").unwrap(),
            cardinality: Cardinality::One,
        };
        assert_eq!(t.to_string(), "Relation(Company, One)");
    }

    #[test]
    fn display_array() {
        let t = FieldType::Array(Box::new(FieldType::Boolean));
        assert_eq!(t.to_string(), "Array<Boolean>");
    }

    #[test]
    fn serde_roundtrip_simple() {
        for ft in [
            FieldType::Boolean,
            FieldType::DateTime,
            FieldType::Duration,
            FieldType::RichText,
            FieldType::Json,
        ] {
            let json = serde_json::to_string(&ft).unwrap();
            let back: FieldType = serde_json::from_str(&json).unwrap();
            assert_eq!(ft, back);
        }
    }

    #[test]
    fn serde_roundtrip_text() {
        let ft = FieldType::Text(TextConstraints::with_max_length(100));
        let json = serde_json::to_string(&ft).unwrap();
        let back: FieldType = serde_json::from_str(&json).unwrap();
        assert_eq!(ft, back);
    }

    #[test]
    fn serde_roundtrip_relation() {
        let ft = FieldType::Relation {
            target: SchemaName::new("Contact").unwrap(),
            cardinality: Cardinality::Many,
        };
        let json = serde_json::to_string(&ft).unwrap();
        let back: FieldType = serde_json::from_str(&json).unwrap();
        assert_eq!(ft, back);
    }

    #[test]
    fn serde_roundtrip_array() {
        let ft = FieldType::Array(Box::new(FieldType::Integer(
            IntegerConstraints::unconstrained(),
        )));
        let json = serde_json::to_string(&ft).unwrap();
        let back: FieldType = serde_json::from_str(&json).unwrap();
        assert_eq!(ft, back);
    }

    #[test]
    fn serde_roundtrip_enum() {
        let ft = FieldType::Enum(EnumVariants::new(vec!["A".into(), "B".into()]).unwrap());
        let json = serde_json::to_string(&ft).unwrap();
        let back: FieldType = serde_json::from_str(&json).unwrap();
        assert_eq!(ft, back);
    }

    #[test]
    fn display_file() {
        use super::super::file_constraints::{FileAccess, FileConstraints};
        let ft = FieldType::File(FileConstraints {
            bucket: "documents".into(),
            max_size_bytes: 1024,
            mime_allowlist: vec![],
            access: FileAccess::Presigned,
        });
        assert_eq!(ft.to_string(), "File");
    }

    #[test]
    fn serde_roundtrip_file() {
        use super::super::file_constraints::{FileAccess, FileConstraints, MimePattern};
        let ft = FieldType::File(FileConstraints {
            bucket: "documents".into(),
            max_size_bytes: 25 * 1024 * 1024,
            mime_allowlist: vec![MimePattern::Exact("application/pdf".into())],
            access: FileAccess::Proxied,
        });
        let json = serde_json::to_string(&ft).unwrap();
        let back: FieldType = serde_json::from_str(&json).unwrap();
        assert_eq!(ft, back);
    }

    // ---- check_value: in-process constraint enforcement (#133) ----

    fn dv_text(s: &str) -> DynamicValue {
        DynamicValue::Text(s.to_string())
    }

    fn alpha_beta() -> FieldType {
        FieldType::Enum(EnumVariants::new(vec!["alpha".into(), "beta".into()]).unwrap())
    }

    #[test]
    fn check_value_text_within_max_length() {
        let ft = FieldType::Text(TextConstraints::with_max_length(10));
        assert_eq!(ft.check_value(&dv_text("short")), Ok(()));
        assert_eq!(ft.check_value(&dv_text("exactly-10")), Ok(()));
    }

    #[test]
    fn check_value_text_over_max_length() {
        let ft = FieldType::Text(TextConstraints::with_max_length(10));
        assert_eq!(
            ft.check_value(&dv_text("this is far too long")),
            Err(ConstraintViolation::TextTooLong { len: 20, max: 10 })
        );
    }

    #[test]
    fn check_value_text_counts_characters_not_bytes() {
        // Ten multi-byte characters are 30 bytes but fit VARCHAR(10),
        // which counts characters. Measuring bytes would reject a value
        // PostgreSQL accepts.
        let ft = FieldType::Text(TextConstraints::with_max_length(10));
        let ten_chars = "日本語日本語日本語語";
        assert_eq!(ten_chars.chars().count(), 10);
        assert!(ten_chars.len() > 10);
        assert_eq!(ft.check_value(&dv_text(ten_chars)), Ok(()));
    }

    #[test]
    fn check_value_unconstrained_text_accepts_anything() {
        let ft = FieldType::Text(TextConstraints::unconstrained());
        assert_eq!(ft.check_value(&dv_text(&"x".repeat(100_000))), Ok(()));
    }

    #[test]
    fn check_value_integer_range() {
        let ft = FieldType::Integer(IntegerConstraints::with_range(1, 5).unwrap());
        assert_eq!(ft.check_value(&DynamicValue::Integer(1)), Ok(()));
        assert_eq!(ft.check_value(&DynamicValue::Integer(5)), Ok(()));
        assert_eq!(
            ft.check_value(&DynamicValue::Integer(0)),
            Err(ConstraintViolation::IntegerBelowMin { value: 0, min: 1 })
        );
        assert_eq!(
            ft.check_value(&DynamicValue::Integer(6)),
            Err(ConstraintViolation::IntegerAboveMax { value: 6, max: 5 })
        );
    }

    #[test]
    fn check_value_integer_half_open_bounds() {
        let min_only = FieldType::Integer(IntegerConstraints::with_min(1));
        assert_eq!(min_only.check_value(&DynamicValue::Integer(i64::MAX)), Ok(()));
        assert!(min_only.check_value(&DynamicValue::Integer(0)).is_err());

        let max_only = FieldType::Integer(IntegerConstraints::with_max(5));
        assert_eq!(max_only.check_value(&DynamicValue::Integer(i64::MIN)), Ok(()));
        assert!(max_only.check_value(&DynamicValue::Integer(6)).is_err());
    }

    #[test]
    fn check_value_enum_membership() {
        let ft = alpha_beta();
        assert_eq!(ft.check_value(&DynamicValue::Enum("alpha".into())), Ok(()));
        assert_eq!(
            ft.check_value(&DynamicValue::Enum("gamma".into())),
            Err(ConstraintViolation::NotAnEnumVariant {
                value: "gamma".into(),
                allowed: vec!["alpha".into(), "beta".into()],
            })
        );
    }

    #[test]
    fn check_value_enum_accepts_a_text_carrier() {
        // Hook responses and `@compute` results arrive as Text even where
        // the schema declares an enum; membership must still be checked.
        let ft = alpha_beta();
        assert_eq!(ft.check_value(&dv_text("beta")), Ok(()));
        assert!(ft.check_value(&dv_text("gamma")).is_err());
    }

    #[test]
    fn check_value_enum_is_case_sensitive() {
        assert!(alpha_beta().check_value(&DynamicValue::Enum("Alpha".into())).is_err());
    }

    #[test]
    fn check_value_bytes_max_size() {
        let ft = FieldType::Bytes(BytesConstraints::with_max_size(4));
        assert_eq!(ft.check_value(&DynamicValue::Bytes(vec![0; 4])), Ok(()));
        assert_eq!(
            ft.check_value(&DynamicValue::Bytes(vec![0; 5])),
            Err(ConstraintViolation::BytesTooLarge { len: 5, max: 4 })
        );
    }

    #[test]
    fn check_value_null_is_always_ok() {
        // Nullability is `required`'s job, checked against the schema's
        // field list rather than here.
        assert_eq!(alpha_beta().check_value(&DynamicValue::Null), Ok(()));
        assert_eq!(
            FieldType::Text(TextConstraints::with_max_length(1)).check_value(&DynamicValue::Null),
            Ok(())
        );
        assert_eq!(
            FieldType::Integer(IntegerConstraints::with_min(10))
                .check_value(&DynamicValue::Null),
            Ok(())
        );
    }

    #[test]
    fn check_value_ignores_shape_mismatches() {
        // Conversion already rejects these; re-reporting would double up
        // the error list the caller renders.
        let ft = FieldType::Text(TextConstraints::with_max_length(1));
        assert_eq!(ft.check_value(&DynamicValue::Integer(12345)), Ok(()));
        assert_eq!(
            FieldType::Integer(IntegerConstraints::with_min(0)).check_value(&dv_text("nope")),
            Ok(())
        );
    }

    #[test]
    fn check_value_recurses_into_arrays() {
        let ft = FieldType::Array(Box::new(alpha_beta()));
        let ok = DynamicValue::Array(vec![
            DynamicValue::Enum("alpha".into()),
            DynamicValue::Enum("beta".into()),
        ]);
        assert_eq!(ft.check_value(&ok), Ok(()));

        let bad = DynamicValue::Array(vec![
            DynamicValue::Enum("alpha".into()),
            DynamicValue::Enum("gamma".into()),
        ]);
        assert!(ft.check_value(&bad).is_err());
    }

    #[test]
    fn check_value_recurses_into_maps() {
        let ft = FieldType::Map {
            key: Box::new(FieldType::Text(TextConstraints::unconstrained())),
            value: Box::new(FieldType::Integer(IntegerConstraints::with_range(1, 5).unwrap())),
        };
        let mut ok = std::collections::BTreeMap::new();
        ok.insert("a".to_string(), DynamicValue::Integer(3));
        assert_eq!(ft.check_value(&DynamicValue::Map(ok)), Ok(()));

        let mut bad = std::collections::BTreeMap::new();
        bad.insert("a".to_string(), DynamicValue::Integer(99));
        assert!(ft.check_value(&DynamicValue::Map(bad)).is_err());
    }

    #[test]
    fn check_value_float_precision_is_not_enforced() {
        // `FloatConstraints::precision` is a storage hint no backend applies
        // (#7). Enforcing it here would be a new restriction, not a fix.
        let ft = FieldType::Float(FloatConstraints::with_precision(2));
        assert_eq!(ft.check_value(&DynamicValue::Float(1.23456)), Ok(()));
    }
}
