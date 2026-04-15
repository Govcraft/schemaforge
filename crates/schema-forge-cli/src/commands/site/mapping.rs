//! Pure-function mapping from [`FieldDefinition`] to [`FieldView`].
//!
//! Supported: Text, RichText, Integer, Float, Boolean, DateTime, Enum,
//! Json, Relation(One|Many), Array(scalar|enum), and Composite (recursive,
//! flattened into dot-path sub-fields). Array-of-array and
//! array-of-composite fall back to a JSON textarea: the field is projected
//! as `kind = "json"` with a best-effort TS type, and the edit form
//! round-trips it through `JSON.parse`/`JSON.stringify`. See issue #18.

use std::collections::BTreeMap;

use schema_forge_core::types::{Cardinality, FieldDefinition, FieldType};

use super::context::{make_field_view, FieldView, SchemaMeta};

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
/// [`FieldMapError::Unsupported`] if the type is not in the v0 subset. The
/// `catalog` lets relation fields resolve their target's display field and
/// kebab slug.
pub fn field_to_view(
    field: &FieldDefinition,
    catalog: &BTreeMap<String, SchemaMeta>,
) -> Result<FieldView, FieldMapError> {
    field_to_view_with_prefix(field, catalog, "")
}

/// Recursive worker: `prefix` is the dot-path of the enclosing composite,
/// used to give sub-fields fully-qualified names so React Hook Form can
/// address them natively.
fn field_to_view_with_prefix(
    field: &FieldDefinition,
    catalog: &BTreeMap<String, SchemaMeta>,
    prefix: &str,
) -> Result<FieldView, FieldMapError> {
    let required = field.is_required();
    match &field.field_type {
        FieldType::Text(c) => {
            let mut zod = "z.string()".to_string();
            if let Some(max) = c.max_length {
                zod.push_str(&format!(".max({max})"));
            }
            if !required {
                zod.push_str(".nullish()");
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
                zod.push_str(".nullish()");
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
                zod.push_str(".nullish()");
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
                zod.push_str(".nullish()");
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
                "z.string().nullish()".to_string()
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
                zod.push_str(".nullish()");
            }
            Ok(make_field_view(
                field, ts_type, zod, "enum", false, None, variants,
            ))
        }
        FieldType::Relation {
            target,
            cardinality: Cardinality::One,
        } => {
            let mut zod = "z.string()".to_string();
            if !required {
                zod.push_str(".nullish()");
            }
            let base = make_field_view(
                field,
                "string".to_string(),
                zod,
                "relation_one",
                true,
                Some(target.as_str().to_string()),
                Vec::new(),
            );
            Ok(with_relation_metadata(base, target.as_str(), catalog))
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
                "z.string().nullish()".to_string()
            };
            let base = make_field_view(
                field,
                "string[]".to_string(),
                zod,
                "relation_many",
                true,
                Some(target.as_str().to_string()),
                Vec::new(),
            );
            Ok(with_relation_metadata(base, target.as_str(), catalog))
        }
        FieldType::Array(inner) => {
            let zod = if required {
                "z.string().min(1, \"Required\")".to_string()
            } else {
                "z.string().nullish()".to_string()
            };
            match describe_array_element(inner) {
                Ok((inner_kind, inner_ts, inner_variants)) => {
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
                Err(_) => {
                    // Array-of-array and array-of-composite fall back to the
                    // JSON textarea path: the form renders a `<textarea>` and
                    // the edit handler runs JSON.parse before submit. The TS
                    // type still captures the real shape for type-safe reads.
                    let ts_type = format!("{}[]", ts_type_for_field_type(inner));
                    Ok(make_field_view(
                        field,
                        ts_type,
                        zod,
                        "json",
                        false,
                        None,
                        Vec::new(),
                    ))
                }
            }
        }
        FieldType::RichText => {
            let zod = if required {
                "z.string().min(1, \"Required\")".to_string()
            } else {
                "z.string().nullish()".to_string()
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
                "z.string().nullish()".to_string()
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
        FieldType::Composite(sub_defs) => {
            // Project each sub-field recursively, naming them with a
            // dot-path so react-hook-form can address them natively.
            let my_path = if prefix.is_empty() {
                field.name.as_str().to_string()
            } else {
                format!("{prefix}.{}", field.name.as_str())
            };
            let mut sub_fields = Vec::with_capacity(sub_defs.len());
            for sub_def in sub_defs {
                // Composites that contain unsupported sub-fields cause the
                // entire composite to be dropped (with a targeted error).
                match field_to_view_with_prefix(sub_def, catalog, &my_path) {
                    Ok(v) => sub_fields.push(v),
                    Err(FieldMapError::Unsupported { field: f, reason }) => {
                        return Err(FieldMapError::Unsupported {
                            field: field.name.as_str().to_string(),
                            reason: format!("composite sub-field `{f}`: {reason}"),
                        });
                    }
                }
            }
            // TS type for a composite is an inline object literal whose
            // entries match the sub-fields' ts_types. Required markers are
            // reflected via `?`.
            let mut ts_parts = Vec::with_capacity(sub_fields.len());
            let mut zod_parts = Vec::with_capacity(sub_fields.len());
            for sv in &sub_fields {
                let opt = if sv.required { "" } else { "?" };
                ts_parts.push(format!("{}{}: {}", sv.leaf, opt, sv.ts_type));
                zod_parts.push(format!("{}: {}", sv.leaf, sv.zod));
            }
            let ts_type = format!("{{ {} }}", ts_parts.join(", "));
            let mut zod = format!("z.object({{ {} }})", zod_parts.join(", "));
            if !required {
                zod.push_str(".nullish()");
            }
            let mut view =
                make_field_view(field, ts_type, zod, "composite", false, None, Vec::new());
            // Overwrite `name` with the dot-path so templates render the
            // correct nested FormField path.
            view.name = my_path;
            view.sub_fields = sub_fields;
            Ok(view)
        }
        other => Err(FieldMapError::Unsupported {
            field: field.name.as_str().to_string(),
            reason: format!("unsupported field type `{other}` in v0 site generator"),
        }),
    }
    .map(|mut v| {
        if !prefix.is_empty() && v.kind != "composite" {
            v.name = format!("{prefix}.{}", v.leaf);
        }
        v
    })
}

/// Fill in the relation target's `kebab` slug and `@display("...")` field
/// from the catalog, if we know about it. Missing entries leave the
/// metadata as `None` (they shouldn't happen in well-formed schema sets).
fn with_relation_metadata(
    mut view: FieldView,
    target: &str,
    catalog: &BTreeMap<String, SchemaMeta>,
) -> FieldView {
    if let Some(meta) = catalog.get(target) {
        view.relation_target_kebab = Some(meta.kebab.clone());
        view.relation_display_field = meta.display_field.clone();
    }
    view
}

/// Best-effort TypeScript type for a raw [`FieldType`], used by the
/// array-of-array / array-of-composite fallback paths. Scalars and enums
/// are captured precisely; relations, richtext, and json fall back to
/// `unknown` since the JSON wire shape is opaque to the generator.
fn ts_type_for_field_type(ft: &FieldType) -> String {
    match ft {
        FieldType::Text(_) | FieldType::RichText | FieldType::DateTime => "string".to_string(),
        FieldType::Integer(_) | FieldType::Float(_) => "number".to_string(),
        FieldType::Boolean => "boolean".to_string(),
        FieldType::Enum(v) => {
            let parts: Vec<String> =
                v.as_slice().iter().map(|s| format!("\"{s}\"")).collect();
            format!("({})", parts.join(" | "))
        }
        FieldType::Json => "unknown".to_string(),
        FieldType::Relation { .. } => "string".to_string(),
        FieldType::Array(inner) => format!("{}[]", ts_type_for_field_type(inner)),
        FieldType::Composite(sub_defs) => {
            let mut parts = Vec::with_capacity(sub_defs.len());
            for sub in sub_defs {
                let opt = if sub.is_required() { "" } else { "?" };
                parts.push(format!(
                    "{}{opt}: {}",
                    sub.name.as_str(),
                    ts_type_for_field_type(&sub.field_type)
                ));
            }
            format!("{{ {} }}", parts.join(", "))
        }
        _ => "unknown".to_string(),
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
        FieldType::Json => Err("arrays of json are not supported in v0 site generator".into()),
        other => Err(format!(
            "array element type `{other}` is not supported in v0 site generator"
        )),
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

    fn empty_catalog() -> BTreeMap<String, SchemaMeta> {
        BTreeMap::new()
    }

    // Convenience wrapper to avoid threading an empty catalog through
    // every existing test.
    fn project(field: &FieldDefinition) -> Result<FieldView, FieldMapError> {
        field_to_view(field, &empty_catalog())
    }

    #[test]
    fn text_with_max_required() {
        let v = project(&field(
            "name",
            FieldType::Text(TextConstraints::with_max_length(120)),
            true,
        ))
        .unwrap();
        assert_eq!(v.ts_type, "string");
        assert_eq!(v.zod, "z.string().max(120)");
        assert!(v.required);
    }

    #[test]
    fn text_optional_no_max() {
        let v = project(&field(
            "bio",
            FieldType::Text(TextConstraints::unconstrained()),
            false,
        ))
        .unwrap();
        assert_eq!(v.zod, "z.string().nullish()");
        assert!(!v.required);
    }

    #[test]
    fn integer_required() {
        let v = project(&field(
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
        let v = project(&field(
            "ratio",
            FieldType::Float(FloatConstraints::unconstrained()),
            false,
        ))
        .unwrap();
        assert_eq!(v.zod, "z.coerce.number().nullish()");
    }

    #[test]
    fn boolean_required() {
        let v = project(&field("active", FieldType::Boolean, true)).unwrap();
        assert_eq!(v.ts_type, "boolean");
        assert_eq!(v.zod, "z.boolean()");
    }

    #[test]
    fn datetime_optional() {
        let v = project(&field("created_at", FieldType::DateTime, false)).unwrap();
        assert_eq!(v.ts_type, "string");
        assert_eq!(v.kind, "datetime");
        assert_eq!(v.zod, "z.string().nullish()");
    }

    #[test]
    fn datetime_required() {
        let v = project(&field("created_at", FieldType::DateTime, true)).unwrap();
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
        let v = project(&fd).unwrap();
        assert_eq!(v.widget.as_deref(), Some("progress"));
        assert_eq!(v.format.as_deref(), Some("currency"));
    }

    #[test]
    fn no_annotations_yields_none_hints() {
        let v = project(&field("x", FieldType::Boolean, false)).unwrap();
        assert!(v.widget.is_none());
        assert!(v.format.is_none());
    }

    #[test]
    fn enum_required() {
        let v = project(&field(
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
        let v = project(&field(
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
        let v = project(&field(
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
        assert_eq!(v.zod, "z.string().nullish()");
        assert!(v.is_relation);
        assert_eq!(v.relation_target.as_deref(), Some("Project"));
    }

    #[test]
    fn array_of_text_optional() {
        let v = project(&field(
            "tags",
            FieldType::Array(Box::new(FieldType::Text(TextConstraints::unconstrained()))),
            false,
        ))
        .unwrap();
        assert_eq!(v.kind, "array");
        assert_eq!(v.ts_type, "string[]");
        assert_eq!(v.item_kind.as_deref(), Some("text"));
        assert_eq!(v.zod, "z.string().nullish()");
    }

    #[test]
    fn array_of_integer_required() {
        let v = project(&field(
            "scores",
            FieldType::Array(Box::new(FieldType::Integer(
                IntegerConstraints::unconstrained(),
            ))),
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
        let v = project(&field(
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
        assert_eq!(
            v.item_enum_variants,
            vec!["bug".to_string(), "feature".to_string()]
        );
    }

    #[test]
    fn array_of_array_falls_back_to_json_textarea() {
        let v = project(&field(
            "matrix",
            FieldType::Array(Box::new(FieldType::Array(Box::new(FieldType::Boolean)))),
            false,
        ))
        .unwrap();
        assert_eq!(v.kind, "json");
        assert_eq!(v.ts_type, "boolean[][]");
        assert_eq!(v.zod, "z.string().nullish()");
    }

    #[test]
    fn array_of_composite_falls_back_to_json_textarea() {
        let inner = FieldDefinition::with_modifiers(
            FieldName::new("base_months").unwrap(),
            FieldType::Integer(IntegerConstraints::unconstrained()),
            vec![FieldModifier::Required],
        );
        let extra = FieldDefinition::new(
            FieldName::new("option_months").unwrap(),
            FieldType::Integer(IntegerConstraints::unconstrained()),
        );
        let v = project(&field(
            "rows",
            FieldType::Array(Box::new(FieldType::Composite(vec![inner, extra]))),
            true,
        ))
        .unwrap();
        assert_eq!(v.kind, "json");
        assert!(v.ts_type.contains("base_months: number"));
        assert!(v.ts_type.contains("option_months?: number"));
        assert!(v.ts_type.ends_with("[]"));
        assert_eq!(v.zod, "z.string().min(1, \"Required\")");
    }

    #[test]
    fn json_field_yields_json_kind() {
        let v = project(&field("metadata", FieldType::Json, false)).unwrap();
        assert_eq!(v.kind, "json");
        assert_eq!(v.ts_type, "unknown");
        assert_eq!(v.zod, "z.string().nullish()");
    }

    #[test]
    fn rich_text_yields_rich_text_kind() {
        let v = project(&field("body", FieldType::RichText, false)).unwrap();
        assert_eq!(v.kind, "rich_text");
        assert_eq!(v.ts_type, "string");
        assert_eq!(v.zod, "z.string().nullish()");
    }

    #[test]
    fn composite_flattens_to_sub_fields_with_dot_paths() {
        let fd = FieldDefinition::new(
            FieldName::new("address").unwrap(),
            FieldType::Composite(vec![
                FieldDefinition::with_modifiers(
                    FieldName::new("city").unwrap(),
                    FieldType::Text(TextConstraints::unconstrained()),
                    vec![FieldModifier::Required],
                ),
                FieldDefinition::new(
                    FieldName::new("postal_code").unwrap(),
                    FieldType::Text(TextConstraints::unconstrained()),
                ),
            ]),
        );
        let v = project(&fd).unwrap();
        assert_eq!(v.kind, "composite");
        assert_eq!(v.name, "address");
        assert_eq!(v.sub_fields.len(), 2);
        assert_eq!(v.sub_fields[0].name, "address.city");
        assert!(v.sub_fields[0].required);
        assert_eq!(v.sub_fields[1].name, "address.postal_code");
        assert!(!v.sub_fields[1].required);
        // Inline object TS type reflects required/optional markers.
        assert!(v.ts_type.contains("city: string"));
        assert!(v.ts_type.contains("postal_code?: string"));
        // Zod is a z.object(...) + .nullish() (since the composite itself
        // is not required on this test fixture).
        assert!(v.zod.starts_with("z.object({ city:"));
        assert!(v.zod.ends_with(".nullish()"));
    }

    #[test]
    fn composite_containing_nested_array_falls_back_to_json_leaf() {
        let fd = FieldDefinition::new(
            FieldName::new("address").unwrap(),
            FieldType::Composite(vec![FieldDefinition::new(
                FieldName::new("matrix").unwrap(),
                FieldType::Array(Box::new(FieldType::Array(Box::new(FieldType::Boolean)))),
            )]),
        );
        let v = project(&fd).unwrap();
        assert_eq!(v.kind, "composite");
        assert_eq!(v.sub_fields.len(), 1);
        assert_eq!(v.sub_fields[0].kind, "json");
        assert_eq!(v.sub_fields[0].ts_type, "boolean[][]");
    }

#[test]
    fn derived_relation_many_marks_view_as_derived() {
        let mut fd = field(
            "documents",
            FieldType::Relation {
                target: SchemaName::new("Document").unwrap(),
                cardinality: Cardinality::Many,
            },
            false,
        );
        fd.derived_from = Some(FieldName::new("opportunity").unwrap());
        let v = project(&fd).unwrap();
        assert!(v.derived, "derived fields must flag through to FieldView");
        assert_eq!(v.kind, "relation_many");
    }

    #[test]
    fn non_derived_relation_many_is_not_flagged() {
        let v = project(&field(
            "projects",
            FieldType::Relation {
                target: SchemaName::new("Project").unwrap(),
                cardinality: Cardinality::Many,
            },
            false,
        ))
        .unwrap();
        assert!(!v.derived);
    }

    #[test]
    fn relation_one_picks_up_target_metadata_from_catalog() {
        let mut catalog = BTreeMap::new();
        catalog.insert(
            "Department".to_string(),
            SchemaMeta {
                schema_name: "Department".to_string(),
                pascal: "Department".to_string(),
                pascal_plural: "Departments".to_string(),
                kebab: "department".to_string(),
                snake: "department".to_string(),
                display_field: Some("name".to_string()),
            },
        );
        let v = field_to_view(
            &field(
                "department",
                FieldType::Relation {
                    target: SchemaName::new("Department").unwrap(),
                    cardinality: Cardinality::One,
                },
                false,
            ),
            &catalog,
        )
        .unwrap();
        assert_eq!(v.relation_target_kebab.as_deref(), Some("department"));
        assert_eq!(v.relation_display_field.as_deref(), Some("name"));
    }
}
