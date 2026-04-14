//! Pure-function mapping from [`FieldDefinition`] to [`FieldView`].
//!
//! v0 supports: Text, Integer, Float, Boolean, DateTime, Enum, Relation(One),
//! Relation(Many), RichText, Json, and Array of scalars (including enum).
//! Composite fields and array-of-array / array-of-composite are still
//! rejected with [`FieldMapError::Unsupported`].

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
            target,
            cardinality: Cardinality::Many,
        } => {
            // Form-side is a CSV string of ids; the edit handler splits on
            // comma and the API type is `string[]`.
            let zod = if required {
                "z.string().min(1, \"Required\")".to_string()
            } else {
                "z.string().optional()".to_string()
            };
            Ok(make_field_view(
                field,
                "string[]".to_string(),
                zod,
                "relation_many",
                true,
                Some(target.as_str().to_string()),
                Vec::new(),
            ))
        }
        FieldType::Array(inner) => {
            let (inner_kind, inner_ts, inner_variants) = describe_array_element(inner)
                .map_err(|reason| FieldMapError::Unsupported {
                    field: field.name.as_str().to_string(),
                    reason,
                })?;
            let zod = if required {
                "z.string().min(1, \"Required\")".to_string()
            } else {
                "z.string().optional()".to_string()
            };
            let base = make_field_view(
                field,
                format!("{inner_ts}[]"),
                zod,
                "array",
                false,
                None,
                Vec::new(),
            );
            Ok(FieldView {
                item_kind: Some(inner_kind.to_string()),
                item_enum_variants: inner_variants,
                ..base
            })
        }
        FieldType::RichText => {
            let zod = if required {
                "z.string().min(1, \"Required\")".to_string()
            } else {
                "z.string().optional()".to_string()
            };
            Ok(make_field_view(
                field,
                "string".to_string(),
                zod,
                "rich_text",
                false,
                None,
                Vec::new(),
            ))
        }
        FieldType::Json => {
            // Form-side is a raw string; edit handler runs JSON.parse before
            // submit. The API type is unknown — we don't know the shape.
            let zod = if required {
                "z.string().min(1, \"Required\")".to_string()
            } else {
                "z.string().optional()".to_string()
            };
            Ok(make_field_view(
                field,
                "unknown".to_string(),
                zod,
                "json",
                false,
                None,
                Vec::new(),
            ))
        }
        FieldType::Composite(_) => Err(FieldMapError::Unsupported {
            field: field.name.as_str().to_string(),
            reason: "composite fields are not yet supported in v0 site generator".to_string(),
        }),
        other => Err(FieldMapError::Unsupported {
            field: field.name.as_str().to_string(),
            reason: format!("unsupported field type `{other}` in v0 site generator"),
        }),
    }
}

/// Describe the element type of an array for projection into a [`FieldView`].
///
/// Returns `(kind, ts_type, enum_variants)` on success. Rejects nested
/// arrays, composites, relations, rich text, and json — v0 only supports
/// arrays of scalars and enums.
fn describe_array_element(
    inner: &FieldType,
) -> Result<(&'static str, String, Vec<String>), String> {
    match inner {
        FieldType::Text(_) => Ok(("text", "string".to_string(), Vec::new())),
        FieldType::Integer(_) => Ok(("integer", "number".to_string(), Vec::new())),
        FieldType::Float(_) => Ok(("float", "number".to_string(), Vec::new())),
        FieldType::Boolean => Ok(("boolean", "boolean".to_string(), Vec::new())),
        FieldType::Enum(v) => {
            let variants: Vec<String> = v.as_slice().iter().map(|s| s.to_string()).collect();
            let ts_type = variants
                .iter()
                .map(|s| format!("\"{s}\""))
                .collect::<Vec<_>>()
                .join(" | ");
            Ok(("enum", format!("({ts_type})"), variants))
        }
        FieldType::DateTime => Ok(("datetime", "string".to_string(), Vec::new())),
        FieldType::Array(_) => Err("nested arrays are not supported in v0 site generator".into()),
        FieldType::Composite(_) => {
            Err("arrays of composites are not supported in v0 site generator".into())
        }
        FieldType::Relation { .. } => {
            Err("arrays of relations are not supported here — use `-> T[]` instead".into())
        }
        FieldType::RichText => {
            Err("arrays of rich text are not supported in v0 site generator".into())
        }
        FieldType::Json => {
            Err("arrays of json are not supported in v0 site generator".into())
        }
        other => Err(format!("array element type `{other}` is not supported in v0 site generator")),
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
        assert_eq!(v.kind, "datetime");
        assert_eq!(v.zod, "z.string().optional()");
    }

    #[test]
    fn datetime_required() {
        let v = field_to_view(&field("created_at", FieldType::DateTime, true)).unwrap();
        assert_eq!(v.zod, "z.string().min(1, \"Required\")");
    }

    #[test]
    fn widget_and_format_hints_propagate() {
        use schema_forge_core::types::{FieldAnnotation, FormatType, WidgetType};

        let fd = FieldDefinition::with_annotations(
            FieldName::new("amount").unwrap(),
            FieldType::Integer(IntegerConstraints::unconstrained()),
            vec![],
            vec![
                FieldAnnotation::Widget {
                    widget_type: WidgetType::Progress,
                },
                FieldAnnotation::Format {
                    format_type: FormatType::Currency,
                },
            ],
        );
        let v = field_to_view(&fd).unwrap();
        assert_eq!(v.widget.as_deref(), Some("progress"));
        assert_eq!(v.format.as_deref(), Some("currency"));
    }

    #[test]
    fn no_annotations_yields_none_hints() {
        let v = field_to_view(&field("x", FieldType::Boolean, false)).unwrap();
        assert!(v.widget.is_none());
        assert!(v.format.is_none());
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
    fn relation_many_optional() {
        let v = field_to_view(&field(
            "projects",
            FieldType::Relation {
                target: SchemaName::new("Project").unwrap(),
                cardinality: Cardinality::Many,
            },
            false,
        ))
        .unwrap();
        assert_eq!(v.kind, "relation_many");
        assert_eq!(v.ts_type, "string[]");
        assert_eq!(v.zod, "z.string().optional()");
        assert!(v.is_relation);
        assert_eq!(v.relation_target.as_deref(), Some("Project"));
    }

    #[test]
    fn array_of_text_optional() {
        let v = field_to_view(&field(
            "tags",
            FieldType::Array(Box::new(FieldType::Text(TextConstraints::unconstrained()))),
            false,
        ))
        .unwrap();
        assert_eq!(v.kind, "array");
        assert_eq!(v.ts_type, "string[]");
        assert_eq!(v.item_kind.as_deref(), Some("text"));
        assert_eq!(v.zod, "z.string().optional()");
    }

    #[test]
    fn array_of_integer_required() {
        let v = field_to_view(&field(
            "scores",
            FieldType::Array(Box::new(FieldType::Integer(IntegerConstraints::unconstrained()))),
            true,
        ))
        .unwrap();
        assert_eq!(v.kind, "array");
        assert_eq!(v.ts_type, "number[]");
        assert_eq!(v.item_kind.as_deref(), Some("integer"));
        assert_eq!(v.zod, "z.string().min(1, \"Required\")");
    }

    #[test]
    fn array_of_enum_carries_variants() {
        let v = field_to_view(&field(
            "labels",
            FieldType::Array(Box::new(FieldType::Enum(
                EnumVariants::new(vec!["bug".into(), "feature".into()]).unwrap(),
            ))),
            false,
        ))
        .unwrap();
        assert_eq!(v.kind, "array");
        assert_eq!(v.ts_type, "(\"bug\" | \"feature\")[]");
        assert_eq!(v.item_kind.as_deref(), Some("enum"));
        assert_eq!(v.item_enum_variants, vec!["bug".to_string(), "feature".to_string()]);
    }

    #[test]
    fn array_of_array_is_unsupported() {
        let err = field_to_view(&field(
            "matrix",
            FieldType::Array(Box::new(FieldType::Array(Box::new(FieldType::Boolean)))),
            false,
        ))
        .unwrap_err();
        let FieldMapError::Unsupported { reason, .. } = err;
        assert!(reason.contains("nested arrays"));
    }

    #[test]
    fn json_field_yields_json_kind() {
        let v = field_to_view(&field("metadata", FieldType::Json, false)).unwrap();
        assert_eq!(v.kind, "json");
        assert_eq!(v.ts_type, "unknown");
        assert_eq!(v.zod, "z.string().optional()");
    }

    #[test]
    fn rich_text_yields_rich_text_kind() {
        let v = field_to_view(&field("body", FieldType::RichText, false)).unwrap();
        assert_eq!(v.kind, "rich_text");
        assert_eq!(v.ts_type, "string");
        assert_eq!(v.zod, "z.string().optional()");
    }

    #[test]
    fn composite_is_still_unsupported() {
        let err = field_to_view(&field(
            "address",
            FieldType::Composite(vec![]),
            false,
        ))
        .unwrap_err();
        let FieldMapError::Unsupported { reason, .. } = err;
        assert!(reason.contains("composite"));
    }
}
