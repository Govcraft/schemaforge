//! Pure-function mapping from [`FieldDefinition`] to [`FieldView`].
//!
//! v0 supports exactly: Text, Integer, Float, Boolean, DateTime, Enum,
//! Relation(One). Everything else yields [`FieldMapError::Unsupported`] so
//! the caller can choose to skip (with a warning) or hard-error.

use schema_forge_core::types::{Cardinality, FieldDefinition, FieldType};

use super::context::{make_field_view, FieldView};

/// Reason a field could not be projected into the v0 site view model.
#[derive(Debug, Clone)]
pub enum FieldMapError {
    /// The field's type is not yet supported by the v0 generator.
    Unsupported {
        /// Field name (no schema qualifier).
        field: String,
        /// Human-readable reason suitable for a warning.
        reason: String,
    },
}

/// Project a single [`FieldDefinition`] into a [`FieldView`], or return
/// [`FieldMapError::Unsupported`] if the type is not in the v0 subset.
pub fn field_to_view(field: &FieldDefinition) -> Result<FieldView, FieldMapError> {
    let required = field.is_required();
    match &field.field_type {
        FieldType::Text(c) => {
            let mut zod = "z.string()".to_string();
            if let Some(max) = c.max_length {
                zod.push_str(&format!(".max({max})"));
            }
            if !required {
                zod.push_str(".optional()");
            }
            Ok(make_field_view(
                field,
                "string".to_string(),
                zod,
                "text",
                false,
                None,
                Vec::new(),
            ))
        }
        FieldType::Integer(_) => {
            let mut zod = "z.coerce.number().int()".to_string();
            if !required {
                zod.push_str(".optional()");
            }
            Ok(make_field_view(
                field,
                "number".to_string(),
                zod,
                "integer",
                false,
                None,
                Vec::new(),
            ))
        }
        FieldType::Float(_) => {
            let mut zod = "z.coerce.number()".to_string();
            if !required {
                zod.push_str(".optional()");
            }
            Ok(make_field_view(
                field,
                "number".to_string(),
                zod,
                "float",
                false,
                None,
                Vec::new(),
            ))
        }
        FieldType::Boolean => {
            let mut zod = "z.boolean()".to_string();
            if !required {
                zod.push_str(".optional()");
            }
            Ok(make_field_view(
                field,
                "boolean".to_string(),
                zod,
                "boolean",
                false,
                None,
                Vec::new(),
            ))
        }
        FieldType::DateTime => {
            // The companion <input type="datetime-local"> emits `YYYY-MM-DDTHH:MM`
            // (no seconds, no timezone), which fails every strict `.datetime()` check.
            // We accept the loose local string here and convert to ISO-8601 with
            // timezone in the edit template's onSubmit handler before calling the API.
            let zod = if required {
                "z.string().min(1, \"Required\")".to_string()
            } else {
                "z.string().optional()".to_string()
            };
            Ok(make_field_view(
                field,
                "string".to_string(),
                zod,
                "datetime",
                false,
                None,
                Vec::new(),
            ))
        }
        FieldType::Enum(v) => {
            let variants: Vec<String> = v.as_slice().iter().map(|s| s.to_string()).collect();
            let ts_type = variants
                .iter()
                .map(|s| format!("\"{s}\""))
                .collect::<Vec<_>>()
                .join(" | ");
            let variant_list = variants
                .iter()
                .map(|s| format!("\"{s}\""))
                .collect::<Vec<_>>()
                .join(", ");
            let mut zod = format!("z.enum([{variant_list}] as const)");
            if !required {
                zod.push_str(".optional()");
            }
            Ok(make_field_view(
                field,
                ts_type,
                zod,
                "enum",
                false,
                None,
                variants,
            ))
        }
        FieldType::Relation {
            target,
            cardinality: Cardinality::One,
        } => {
            let mut zod = "z.string()".to_string();
            if !required {
                zod.push_str(".optional()");
            }
            Ok(make_field_view(
                field,
                "string".to_string(),
                zod,
                "relation_one",
                true,
                Some(target.as_str().to_string()),
                Vec::new(),
            ))
        }
        FieldType::Relation {
            cardinality: Cardinality::Many,
            ..
        } => Err(FieldMapError::Unsupported {
            field: field.name.as_str().to_string(),
            reason: "relation(many) is not yet supported in v0 site generator".to_string(),
        }),
        FieldType::Array(_) => Err(FieldMapError::Unsupported {
            field: field.name.as_str().to_string(),
            reason: "array fields are not yet supported in v0 site generator".to_string(),
        }),
        FieldType::Composite(_) => Err(FieldMapError::Unsupported {
            field: field.name.as_str().to_string(),
            reason: "composite fields are not yet supported in v0 site generator".to_string(),
        }),
        FieldType::RichText => Err(FieldMapError::Unsupported {
            field: field.name.as_str().to_string(),
            reason: "rich text is not yet supported in v0 site generator".to_string(),
        }),
        FieldType::Json => Err(FieldMapError::Unsupported {
            field: field.name.as_str().to_string(),
            reason: "json fields are not yet supported in v0 site generator".to_string(),
        }),
        other => Err(FieldMapError::Unsupported {
            field: field.name.as_str().to_string(),
            reason: format!("unsupported field type `{other}` in v0 site generator"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use schema_forge_core::types::{
        EnumVariants, FieldModifier, FieldName, FloatConstraints, IntegerConstraints, SchemaName,
        TextConstraints,
    };

    fn field(name: &str, ft: FieldType, required: bool) -> FieldDefinition {
        let mods = if required {
            vec![FieldModifier::Required]
        } else {
            vec![]
        };
        FieldDefinition::with_modifiers(FieldName::new(name).unwrap(), ft, mods)
    }

    #[test]
    fn text_with_max_required() {
        let v =
            field_to_view(&field("name", FieldType::Text(TextConstraints::with_max_length(120)), true))
                .unwrap();
        assert_eq!(v.ts_type, "string");
        assert_eq!(v.zod, "z.string().max(120)");
        assert!(v.required);
    }

    #[test]
    fn text_optional_no_max() {
        let v = field_to_view(&field(
            "bio",
            FieldType::Text(TextConstraints::unconstrained()),
            false,
        ))
        .unwrap();
        assert_eq!(v.zod, "z.string().optional()");
        assert!(!v.required);
    }

    #[test]
    fn integer_required() {
        let v = field_to_view(&field(
            "age",
            FieldType::Integer(IntegerConstraints::unconstrained()),
            true,
        ))
        .unwrap();
        assert_eq!(v.ts_type, "number");
        assert_eq!(v.zod, "z.coerce.number().int()");
    }

    #[test]
    fn float_optional() {
        let v = field_to_view(&field(
            "ratio",
            FieldType::Float(FloatConstraints::unconstrained()),
            false,
        ))
        .unwrap();
        assert_eq!(v.zod, "z.coerce.number().optional()");
    }

    #[test]
    fn boolean_required() {
        let v = field_to_view(&field("active", FieldType::Boolean, true)).unwrap();
        assert_eq!(v.ts_type, "boolean");
        assert_eq!(v.zod, "z.boolean()");
    }

    #[test]
    fn datetime_optional() {
        let v = field_to_view(&field("created_at", FieldType::DateTime, false)).unwrap();
        assert_eq!(v.ts_type, "string");
        assert!(v.zod.contains("datetime"));
    }

    #[test]
    fn enum_required() {
        let v = field_to_view(&field(
            "status",
            FieldType::Enum(EnumVariants::new(vec!["a".into(), "b".into()]).unwrap()),
            true,
        ))
        .unwrap();
        assert_eq!(v.ts_type, "\"a\" | \"b\"");
        assert!(v.zod.contains("z.enum([\"a\", \"b\"]"));
        assert_eq!(v.enum_variants, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn relation_one_required() {
        let v = field_to_view(&field(
            "department",
            FieldType::Relation {
                target: SchemaName::new("Department").unwrap(),
                cardinality: Cardinality::One,
            },
            true,
        ))
        .unwrap();
        assert_eq!(v.ts_type, "string");
        assert!(v.is_relation);
        assert_eq!(v.relation_target.as_deref(), Some("Department"));
    }

    #[test]
    fn relation_many_is_unsupported() {
        let err = field_to_view(&field(
            "projects",
            FieldType::Relation {
                target: SchemaName::new("Project").unwrap(),
                cardinality: Cardinality::Many,
            },
            false,
        ))
        .unwrap_err();
        let FieldMapError::Unsupported { reason, .. } = err;
        assert!(reason.contains("relation(many)"));
    }

    #[test]
    fn array_is_unsupported() {
        let err = field_to_view(&field(
            "tags",
            FieldType::Array(Box::new(FieldType::Text(TextConstraints::unconstrained()))),
            false,
        ))
        .unwrap_err();
        let FieldMapError::Unsupported { reason, .. } = err;
        assert!(reason.contains("array"));
    }

    #[test]
    fn json_is_unsupported() {
        let err = field_to_view(&field("metadata", FieldType::Json, false)).unwrap_err();
        let FieldMapError::Unsupported { reason, .. } = err;
        assert!(reason.contains("json"));
    }

    #[test]
    fn rich_text_is_unsupported() {
        let err = field_to_view(&field("body", FieldType::RichText, false)).unwrap_err();
        let FieldMapError::Unsupported { reason, .. } = err;
        assert!(reason.contains("rich text"));
    }
}
