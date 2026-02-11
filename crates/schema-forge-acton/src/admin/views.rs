use schema_forge_backend::entity::Entity;
use schema_forge_core::migration::{MigrationPlan, MigrationSafety, MigrationStep};
use schema_forge_core::types::{
    Annotation, Cardinality, DynamicValue, FieldDefinition, FieldModifier, FieldType,
    SchemaDefinition,
};

/// Template-friendly representation of a field definition.
#[derive(Debug, Clone)]
pub struct FieldView {
    pub name: String,
    pub label: String,
    pub input_type: String,
    pub attrs: Vec<(String, String)>,
    pub required: bool,
    pub options: Vec<(String, String)>,
    pub multiple: bool,
    pub children: Vec<FieldView>,
    pub type_label: String,
    pub default_value: Option<String>,
    pub current_value: Option<String>,
    pub relation_target: Option<String>,
}

/// Template-friendly representation of a schema.
#[derive(Debug, Clone)]
pub struct SchemaView {
    pub name: String,
    pub display_field: Option<String>,
    pub version: Option<u32>,
    pub fields: Vec<FieldView>,
    pub url_name: String,
}

/// Template-friendly representation of an entity.
#[derive(Debug, Clone)]
pub struct EntityView {
    pub id: String,
    pub display_value: String,
    pub field_values: Vec<(String, String)>,
}

/// Dashboard entry for a schema with entity count.
#[derive(Debug, Clone)]
pub struct DashboardEntry {
    pub schema: SchemaView,
    pub entity_count: usize,
}

/// Pagination view model.
#[derive(Debug, Clone)]
pub struct PaginationView {
    pub current_page: usize,
    pub total_pages: usize,
    pub total_count: usize,
    pub limit: usize,
    pub offset: usize,
    pub has_previous: bool,
    pub has_next: bool,
}

impl PaginationView {
    pub fn new(total_count: usize, limit: usize, offset: usize) -> Self {
        let limit = if limit == 0 { 25 } else { limit };
        let total_pages = if total_count == 0 {
            1
        } else {
            total_count.div_ceil(limit)
        };
        let current_page = offset / limit + 1;
        Self {
            current_page,
            total_pages,
            total_count,
            limit,
            offset,
            has_previous: offset > 0,
            has_next: offset + limit < total_count,
        }
    }

    pub fn end_showing(&self) -> usize {
        std::cmp::min(self.offset + self.limit, self.total_count)
    }

    pub fn previous_offset(&self) -> usize {
        self.offset.saturating_sub(self.limit)
    }

    pub fn next_offset(&self) -> usize {
        self.offset + self.limit
    }
}

/// Convert a snake_case field name to a human-readable label.
pub fn snake_to_label(name: &str) -> String {
    name.split('_')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => {
                    let mut s = c.to_uppercase().to_string();
                    s.extend(chars);
                    s
                }
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

impl FieldView {
    /// Create a FieldView from a field definition (no current value).
    pub fn from_definition(field: &FieldDefinition) -> Self {
        Self::from_definition_with_value(field, None)
    }

    /// Create a FieldView from a field definition with an optional current value.
    pub fn from_definition_with_value(
        field: &FieldDefinition,
        value: Option<&DynamicValue>,
    ) -> Self {
        let name = field.name.as_str().to_string();
        let label = snake_to_label(&name);
        let required = field.is_required();
        let default_value = field
            .modifiers
            .iter()
            .find_map(|m| match m {
                schema_forge_core::types::FieldModifier::Default { value } => {
                    Some(value.to_string())
                }
                _ => None,
            });
        let current_value = value.map(dynamic_value_display);

        let (input_type, attrs, options, multiple, children, relation_target) =
            field_type_to_input(&field.field_type, value);

        let type_label = field_type_label(&field.field_type);

        Self {
            name,
            label,
            input_type,
            attrs,
            required,
            options,
            multiple,
            children,
            type_label,
            default_value,
            current_value,
            relation_target,
        }
    }
}

impl SchemaView {
    /// Create a SchemaView from a schema definition.
    pub fn from_definition(schema: &SchemaDefinition) -> Self {
        let name = schema.name.as_str().to_string();
        let url_name = name.clone();

        let display_field = schema.annotations.iter().find_map(|a| match a {
            Annotation::Display { field } => Some(field.as_str().to_string()),
            _ => None,
        });

        let version = schema.annotations.iter().find_map(|a| match a {
            Annotation::Version { version } => Some(version.get()),
            _ => None,
        });

        let fields = schema.fields.iter().map(FieldView::from_definition).collect();

        Self {
            name,
            display_field,
            version,
            fields,
            url_name,
        }
    }
}

impl EntityView {
    /// Create an EntityView from an entity and its schema.
    pub fn from_entity(entity: &Entity, schema: &SchemaDefinition) -> Self {
        Self::from_entity_with_refs(entity, schema, &std::collections::HashMap::new())
    }

    /// Create an EntityView with resolved relation display values.
    ///
    /// `ref_display` maps entity IDs to their human-readable display strings.
    pub fn from_entity_with_refs(
        entity: &Entity,
        schema: &SchemaDefinition,
        ref_display: &std::collections::HashMap<String, String>,
    ) -> Self {
        let id = entity.id.as_str().to_string();

        let display_field = schema.annotations.iter().find_map(|a| match a {
            Annotation::Display { field } => Some(field.as_str().to_string()),
            _ => None,
        });

        let display_value = if let Some(ref df) = display_field {
            entity
                .field(df)
                .map(|v| display_with_refs(v, ref_display))
                .unwrap_or_else(|| id.clone())
        } else {
            // Use first text field or ID
            schema
                .fields
                .iter()
                .find_map(|f| {
                    if matches!(
                        f.field_type,
                        FieldType::Text(_) | FieldType::RichText | FieldType::Enum(_)
                    ) {
                        entity
                            .field(f.name.as_str())
                            .map(|v| display_with_refs(v, ref_display))
                    } else {
                        None
                    }
                })
                .unwrap_or_else(|| id.clone())
        };

        let field_values = schema
            .fields
            .iter()
            .map(|f| {
                let val = entity
                    .field(f.name.as_str())
                    .map(|v| display_with_refs(v, ref_display))
                    .unwrap_or_default();
                (snake_to_label(f.name.as_str()), val)
            })
            .collect();

        Self {
            id,
            display_value,
            field_values,
        }
    }
}

/// Display a DynamicValue, resolving Ref/RefArray via a lookup map.
fn display_with_refs(
    value: &DynamicValue,
    ref_display: &std::collections::HashMap<String, String>,
) -> String {
    match value {
        DynamicValue::Ref(id) => {
            let id_str = id.as_str();
            ref_display
                .get(id_str)
                .cloned()
                .unwrap_or_else(|| id_str.to_string())
        }
        DynamicValue::RefArray(ids) => ids
            .iter()
            .map(|id| {
                let id_str = id.as_str();
                ref_display
                    .get(id_str)
                    .cloned()
                    .unwrap_or_else(|| id_str.to_string())
            })
            .collect::<Vec<_>>()
            .join(", "),
        other => dynamic_value_display(other),
    }
}

/// Convert a DynamicValue to a display string.
fn dynamic_value_display(value: &DynamicValue) -> String {
    match value {
        DynamicValue::Null => String::new(),
        DynamicValue::Text(s) => s.clone(),
        DynamicValue::Integer(i) => i.to_string(),
        DynamicValue::Float(f) => f.to_string(),
        DynamicValue::Boolean(b) => b.to_string(),
        DynamicValue::DateTime(dt) => dt.format("%Y-%m-%dT%H:%M").to_string(),
        DynamicValue::Enum(s) => s.clone(),
        DynamicValue::Json(v) => v.to_string(),
        DynamicValue::Ref(id) => id.as_str().to_string(),
        DynamicValue::RefArray(ids) => ids
            .iter()
            .map(|id| id.as_str().to_string())
            .collect::<Vec<_>>()
            .join(", "),
        DynamicValue::Array(arr) => {
            let items: Vec<String> = arr.iter().map(dynamic_value_display).collect();
            format!("[{}]", items.join(", "))
        }
        DynamicValue::Composite(map) => {
            let items: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{k}: {}", dynamic_value_display(v)))
                .collect();
            format!("{{{}}}", items.join(", "))
        }
        _ => String::new(),
    }
}

/// (input_type, attrs, options, multiple, children, relation_target)
type FieldInputInfo = (
    String,
    Vec<(String, String)>,
    Vec<(String, String)>,
    bool,
    Vec<FieldView>,
    Option<String>,
);

/// Map FieldType to HTML input_type, attrs, options, multiple, children, relation_target.
fn field_type_to_input(
    field_type: &FieldType,
    value: Option<&DynamicValue>,
) -> FieldInputInfo {
    match field_type {
        FieldType::Text(constraints) => {
            let mut attrs = Vec::new();
            if let Some(max) = constraints.max_length {
                attrs.push(("maxlength".to_string(), max.to_string()));
            }
            ("text".to_string(), attrs, vec![], false, vec![], None)
        }
        FieldType::RichText => {
            let attrs = vec![("rows".to_string(), "6".to_string())];
            ("textarea".to_string(), attrs, vec![], false, vec![], None)
        }
        FieldType::Integer(constraints) => {
            let mut attrs = vec![("step".to_string(), "1".to_string())];
            if let Some(min) = constraints.min {
                attrs.push(("min".to_string(), min.to_string()));
            }
            if let Some(max) = constraints.max {
                attrs.push(("max".to_string(), max.to_string()));
            }
            ("number".to_string(), attrs, vec![], false, vec![], None)
        }
        FieldType::Float(constraints) => {
            let step = constraints
                .precision
                .map(|p| {
                    if p == 0 {
                        "1".to_string()
                    } else {
                        format!("0.{}{}", "0".repeat(p as usize - 1), "1")
                    }
                })
                .unwrap_or_else(|| "any".to_string());
            let attrs = vec![("step".to_string(), step)];
            ("number".to_string(), attrs, vec![], false, vec![], None)
        }
        FieldType::Boolean => ("checkbox".to_string(), vec![], vec![], false, vec![], None),
        FieldType::DateTime => (
            "datetime-local".to_string(),
            vec![],
            vec![],
            false,
            vec![],
            None,
        ),
        FieldType::Enum(variants) => {
            let options = variants
                .as_slice()
                .iter()
                .map(|v| (v.clone(), v.clone()))
                .collect();
            ("select".to_string(), vec![], options, false, vec![], None)
        }
        FieldType::Json => {
            let attrs = vec![
                ("rows".to_string(), "6".to_string()),
                ("placeholder".to_string(), "Enter JSON...".to_string()),
            ];
            ("json".to_string(), attrs, vec![], false, vec![], None)
        }
        FieldType::Relation {
            target,
            cardinality,
        } => {
            let multiple = matches!(cardinality, Cardinality::Many);
            let relation_target = Some(target.as_str().to_string());
            (
                "select".to_string(),
                vec![],
                vec![],
                multiple,
                vec![],
                relation_target,
            )
        }
        FieldType::Array(inner) => {
            let inner_field = FieldDefinition::new(
                schema_forge_core::types::FieldName::new("item").unwrap_or_else(|_| {
                    // Fallback — should never happen
                    panic!("'item' is a valid field name")
                }),
                *inner.clone(),
            );
            let child = FieldView::from_definition_with_value(&inner_field, None);
            ("array".to_string(), vec![], vec![], false, vec![child], None)
        }
        FieldType::Composite(fields) => {
            let children: Vec<FieldView> = fields
                .iter()
                .map(|f| {
                    let child_value = value.and_then(|v| {
                        if let DynamicValue::Composite(map) = v {
                            map.get(f.name.as_str())
                        } else {
                            None
                        }
                    });
                    FieldView::from_definition_with_value(f, child_value)
                })
                .collect();
            (
                "composite".to_string(),
                vec![],
                vec![],
                false,
                children,
                None,
            )
        }
        _ => ("text".to_string(), vec![], vec![], false, vec![], None),
    }
}

/// Generate a human-readable type label for schema detail display.
fn field_type_label(field_type: &FieldType) -> String {
    match field_type {
        FieldType::Text(c) => match c.max_length {
            Some(max) => format!("Text(max: {max})"),
            None => "Text".to_string(),
        },
        FieldType::RichText => "RichText".to_string(),
        FieldType::Integer(c) => {
            let parts: Vec<String> = [
                c.min.map(|v| format!("min: {v}")),
                c.max.map(|v| format!("max: {v}")),
            ]
            .into_iter()
            .flatten()
            .collect();
            if parts.is_empty() {
                "Integer".to_string()
            } else {
                format!("Integer({})", parts.join(", "))
            }
        }
        FieldType::Float(c) => match c.precision {
            Some(p) => format!("Float(precision: {p})"),
            None => "Float".to_string(),
        },
        FieldType::Boolean => "Boolean".to_string(),
        FieldType::DateTime => "DateTime".to_string(),
        FieldType::Enum(v) => format!("Enum({})", v.as_slice().join(", ")),
        FieldType::Json => "Json".to_string(),
        FieldType::Relation {
            target,
            cardinality,
        } => format!("Relation({target}, {cardinality})"),
        FieldType::Array(inner) => format!("Array<{}>", field_type_label(inner)),
        FieldType::Composite(fields) => format!("Composite({} fields)", fields.len()),
        _ => "Unknown".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Schema editor view models
// ---------------------------------------------------------------------------

/// A single field row in the schema editor form.
#[derive(Debug, Clone)]
pub struct FieldEditorRow {
    pub index: usize,
    pub name: String,
    pub old_name: Option<String>,
    pub field_type: String,
    pub required: bool,
    pub indexed: bool,
    pub default_enabled: bool,
    pub default_value: String,
    pub text_max_length: Option<u32>,
    pub integer_min: Option<i64>,
    pub integer_max: Option<i64>,
    pub float_precision: Option<u32>,
    pub enum_variants: String,
    pub relation_target: String,
    pub relation_cardinality: String,
}

impl FieldEditorRow {
    /// Create an empty field row for "Add Field".
    pub fn empty(index: usize) -> Self {
        Self {
            index,
            name: String::new(),
            old_name: None,
            field_type: "text".to_string(),
            required: false,
            indexed: false,
            default_enabled: false,
            default_value: String::new(),
            text_max_length: None,
            integer_min: None,
            integer_max: None,
            float_precision: None,
            enum_variants: String::new(),
            relation_target: String::new(),
            relation_cardinality: "one".to_string(),
        }
    }

    /// Create a field editor row from a field definition.
    pub fn from_definition(index: usize, field: &FieldDefinition) -> Self {
        let name = field.name.as_str().to_string();
        let required = field.is_required();
        let indexed = field.modifiers.iter().any(|m| matches!(m, FieldModifier::Indexed));

        let (default_enabled, default_value) = field
            .modifiers
            .iter()
            .find_map(|m| match m {
                FieldModifier::Default { value } => Some((true, value.to_string())),
                _ => None,
            })
            .unwrap_or((false, String::new()));

        let (field_type, text_max_length, integer_min, integer_max, float_precision, enum_variants, relation_target, relation_cardinality) = match &field.field_type {
            FieldType::Text(c) => ("text".to_string(), c.max_length, None, None, None, String::new(), String::new(), "one".to_string()),
            FieldType::RichText => ("richtext".to_string(), None, None, None, None, String::new(), String::new(), "one".to_string()),
            FieldType::Integer(c) => ("integer".to_string(), None, c.min, c.max, None, String::new(), String::new(), "one".to_string()),
            FieldType::Float(c) => ("float".to_string(), None, None, None, c.precision, String::new(), String::new(), "one".to_string()),
            FieldType::Boolean => ("boolean".to_string(), None, None, None, None, String::new(), String::new(), "one".to_string()),
            FieldType::DateTime => ("datetime".to_string(), None, None, None, None, String::new(), String::new(), "one".to_string()),
            FieldType::Enum(variants) => ("enum".to_string(), None, None, None, None, variants.as_slice().join("\n"), String::new(), "one".to_string()),
            FieldType::Json => ("json".to_string(), None, None, None, None, String::new(), String::new(), "one".to_string()),
            FieldType::Relation { target, cardinality } => {
                let card = match cardinality {
                    Cardinality::One => "one",
                    Cardinality::Many => "many",
                    _ => "one",
                };
                ("relation".to_string(), None, None, None, None, String::new(), target.as_str().to_string(), card.to_string())
            }
            _ => ("text".to_string(), None, None, None, None, String::new(), String::new(), "one".to_string()),
        };

        Self {
            index,
            old_name: Some(name.clone()),
            name,
            field_type,
            required,
            indexed,
            default_enabled,
            default_value,
            text_max_length,
            integer_min,
            integer_max,
            float_precision,
            enum_variants,
            relation_target,
            relation_cardinality,
        }
    }
}

/// Schema editor form view model.
#[derive(Debug, Clone)]
pub struct SchemaEditorView {
    pub schema_name: String,
    pub version: String,
    pub display_field: String,
    pub fields: Vec<FieldEditorRow>,
    pub is_edit: bool,
    pub existing_name: Option<String>,
}

impl SchemaEditorView {
    /// Create an empty editor view for new schema creation.
    pub fn new_empty() -> Self {
        Self {
            schema_name: String::new(),
            version: String::new(),
            display_field: String::new(),
            fields: vec![FieldEditorRow::empty(0)],
            is_edit: false,
            existing_name: None,
        }
    }

    /// Create an editor view from an existing schema definition.
    pub fn from_definition(schema: &SchemaDefinition) -> Self {
        let schema_name = schema.name.as_str().to_string();

        let version = schema
            .annotations
            .iter()
            .find_map(|a| match a {
                Annotation::Version { version } => Some(version.get().to_string()),
                _ => None,
            })
            .unwrap_or_default();

        let display_field = schema
            .annotations
            .iter()
            .find_map(|a| match a {
                Annotation::Display { field } => Some(field.as_str().to_string()),
                _ => None,
            })
            .unwrap_or_default();

        let fields = schema
            .fields
            .iter()
            .enumerate()
            .map(|(i, f)| FieldEditorRow::from_definition(i, f))
            .collect();

        Self {
            schema_name: schema_name.clone(),
            version,
            display_field,
            fields,
            is_edit: true,
            existing_name: Some(schema_name),
        }
    }
}

// ---------------------------------------------------------------------------
// Schema relationship graph view models
// ---------------------------------------------------------------------------

/// A node in the schema relationship graph.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SchemaGraphNode {
    pub id: String,
    pub entity_count: usize,
}

/// An edge in the schema relationship graph.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SchemaGraphEdge {
    pub from: String,
    pub to: String,
    pub label: String,
    pub cardinality: String,
}

/// Full graph data for the dashboard relationship visualization.
#[derive(Debug, Clone)]
pub struct SchemaGraphView {
    pub json: String,
    pub has_edges: bool,
}

impl SchemaGraphView {
    /// Build graph data from dashboard entries and schema definitions.
    ///
    /// Nodes come from `entries` (name + entity_count). Edges come from scanning
    /// each schema's fields for `FieldType::Relation`. Only edges where both
    /// source and target exist as nodes are included.
    pub fn from_entries(entries: &[DashboardEntry], schemas: &[SchemaDefinition]) -> Self {
        let nodes: Vec<SchemaGraphNode> = entries
            .iter()
            .map(|e| SchemaGraphNode {
                id: e.schema.name.clone(),
                entity_count: e.entity_count,
            })
            .collect();

        let node_names: std::collections::HashSet<&str> =
            nodes.iter().map(|n| n.id.as_str()).collect();

        let mut edges = Vec::new();
        for schema in schemas {
            let from = schema.name.as_str();
            if !node_names.contains(from) {
                continue;
            }
            for field in &schema.fields {
                if let FieldType::Relation {
                    target,
                    cardinality,
                } = &field.field_type
                {
                    let to = target.as_str();
                    if node_names.contains(to) {
                        edges.push(SchemaGraphEdge {
                            from: from.to_string(),
                            to: to.to_string(),
                            label: field.name.as_str().to_string(),
                            cardinality: match cardinality {
                                Cardinality::Many => "Many".to_string(),
                                _ => "One".to_string(),
                            },
                        });
                    }
                }
            }
        }

        let has_edges = !edges.is_empty();

        #[derive(serde::Serialize)]
        struct GraphData<'a> {
            nodes: &'a [SchemaGraphNode],
            edges: &'a [SchemaGraphEdge],
        }

        let json = serde_json::to_string(&GraphData {
            nodes: &nodes,
            edges: &edges,
        })
        .unwrap_or_else(|_| r#"{"nodes":[],"edges":[]}"#.to_string());

        Self { json, has_edges }
    }
}

/// A single migration step for display.
#[derive(Debug, Clone)]
pub struct MigrationStepView {
    pub description: String,
    pub safety: String,
    pub safety_class: String,
}

impl MigrationStepView {
    fn from_step(step: &MigrationStep) -> Self {
        let safety = step.safety();
        let (safety_label, safety_class) = match safety {
            MigrationSafety::Safe => ("Safe", "safety-safe"),
            MigrationSafety::RequiresConfirmation => ("Requires Confirmation", "safety-warning"),
            MigrationSafety::Destructive => ("Destructive", "safety-danger"),
            _ => ("Unknown", "safety-warning"),
        };
        Self {
            description: step.to_string(),
            safety: safety_label.to_string(),
            safety_class: safety_class.to_string(),
        }
    }
}

/// Migration preview panel view model.
#[derive(Debug, Clone)]
pub struct MigrationPreviewView {
    pub steps: Vec<MigrationStepView>,
    pub overall_safety: String,
    pub is_empty: bool,
}

impl MigrationPreviewView {
    /// Create a migration preview from a migration plan.
    pub fn from_plan(plan: &MigrationPlan) -> Self {
        let steps: Vec<MigrationStepView> = plan
            .steps
            .iter()
            .map(MigrationStepView::from_step)
            .collect();

        let overall_safety = match plan.overall_safety() {
            MigrationSafety::Safe => "Safe",
            MigrationSafety::RequiresConfirmation => "Requires Confirmation",
            MigrationSafety::Destructive => "Destructive",
            _ => "Unknown",
        };

        Self {
            is_empty: steps.is_empty(),
            steps,
            overall_safety: overall_safety.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use schema_forge_core::types::*;

    fn make_field(name: &str, ft: FieldType) -> FieldDefinition {
        FieldDefinition::new(FieldName::new(name).unwrap(), ft)
    }

    fn make_required_field(name: &str, ft: FieldType) -> FieldDefinition {
        FieldDefinition::with_modifiers(
            FieldName::new(name).unwrap(),
            ft,
            vec![FieldModifier::Required],
        )
    }

    // --- snake_to_label tests ---

    #[test]
    fn label_single_word() {
        assert_eq!(snake_to_label("name"), "Name");
    }

    #[test]
    fn label_multi_word() {
        assert_eq!(snake_to_label("email_address"), "Email Address");
    }

    #[test]
    fn label_already_capitalized() {
        assert_eq!(snake_to_label("id"), "Id");
    }

    // --- FieldView from Text ---

    #[test]
    fn field_view_text_unconstrained() {
        let field = make_field("name", FieldType::Text(TextConstraints::unconstrained()));
        let view = FieldView::from_definition(&field);
        assert_eq!(view.input_type, "text");
        assert_eq!(view.name, "name");
        assert_eq!(view.label, "Name");
        assert!(!view.required);
        assert!(view.attrs.is_empty());
    }

    #[test]
    fn field_view_text_with_max() {
        let field = make_field("email", FieldType::Text(TextConstraints::with_max_length(255)));
        let view = FieldView::from_definition(&field);
        assert_eq!(view.input_type, "text");
        assert_eq!(
            view.attrs,
            vec![("maxlength".to_string(), "255".to_string())]
        );
    }

    #[test]
    fn field_view_required() {
        let field = make_required_field(
            "name",
            FieldType::Text(TextConstraints::unconstrained()),
        );
        let view = FieldView::from_definition(&field);
        assert!(view.required);
    }

    // --- FieldView from RichText ---

    #[test]
    fn field_view_rich_text() {
        let field = make_field("description", FieldType::RichText);
        let view = FieldView::from_definition(&field);
        assert_eq!(view.input_type, "textarea");
        assert!(view.attrs.iter().any(|(k, v)| k == "rows" && v == "6"));
    }

    // --- FieldView from Integer ---

    #[test]
    fn field_view_integer_unconstrained() {
        let field = make_field(
            "age",
            FieldType::Integer(IntegerConstraints::unconstrained()),
        );
        let view = FieldView::from_definition(&field);
        assert_eq!(view.input_type, "number");
        assert!(view.attrs.iter().any(|(k, v)| k == "step" && v == "1"));
    }

    #[test]
    fn field_view_integer_with_range() {
        let field = make_field(
            "score",
            FieldType::Integer(IntegerConstraints::with_range(0, 100).unwrap()),
        );
        let view = FieldView::from_definition(&field);
        assert_eq!(view.input_type, "number");
        assert!(view.attrs.iter().any(|(k, v)| k == "min" && v == "0"));
        assert!(view.attrs.iter().any(|(k, v)| k == "max" && v == "100"));
    }

    // --- FieldView from Float ---

    #[test]
    fn field_view_float_with_precision() {
        let field = make_field(
            "price",
            FieldType::Float(FloatConstraints::with_precision(2)),
        );
        let view = FieldView::from_definition(&field);
        assert_eq!(view.input_type, "number");
        assert!(view
            .attrs
            .iter()
            .any(|(k, v)| k == "step" && v == "0.01"));
    }

    #[test]
    fn field_view_float_unconstrained() {
        let field = make_field(
            "value",
            FieldType::Float(FloatConstraints::unconstrained()),
        );
        let view = FieldView::from_definition(&field);
        assert!(view
            .attrs
            .iter()
            .any(|(k, v)| k == "step" && v == "any"));
    }

    // --- FieldView from Boolean ---

    #[test]
    fn field_view_boolean() {
        let field = make_field("active", FieldType::Boolean);
        let view = FieldView::from_definition(&field);
        assert_eq!(view.input_type, "checkbox");
    }

    // --- FieldView from DateTime ---

    #[test]
    fn field_view_datetime() {
        let field = make_field("created_at", FieldType::DateTime);
        let view = FieldView::from_definition(&field);
        assert_eq!(view.input_type, "datetime-local");
    }

    // --- FieldView from Enum ---

    #[test]
    fn field_view_enum() {
        let variants =
            EnumVariants::new(vec!["Active".into(), "Inactive".into(), "Pending".into()]).unwrap();
        let field = make_field("status", FieldType::Enum(variants));
        let view = FieldView::from_definition(&field);
        assert_eq!(view.input_type, "select");
        assert_eq!(view.options.len(), 3);
        assert_eq!(view.options[0], ("Active".to_string(), "Active".to_string()));
    }

    // --- FieldView from Json ---

    #[test]
    fn field_view_json() {
        let field = make_field("metadata", FieldType::Json);
        let view = FieldView::from_definition(&field);
        assert_eq!(view.input_type, "json");
    }

    // --- FieldView from Relation ---

    #[test]
    fn field_view_relation_one() {
        let field = make_field(
            "company",
            FieldType::Relation {
                target: SchemaName::new("Company").unwrap(),
                cardinality: Cardinality::One,
            },
        );
        let view = FieldView::from_definition(&field);
        assert_eq!(view.input_type, "select");
        assert!(!view.multiple);
        assert_eq!(view.relation_target, Some("Company".to_string()));
    }

    #[test]
    fn field_view_relation_many() {
        let field = make_field(
            "tags",
            FieldType::Relation {
                target: SchemaName::new("Tag").unwrap(),
                cardinality: Cardinality::Many,
            },
        );
        let view = FieldView::from_definition(&field);
        assert_eq!(view.input_type, "select");
        assert!(view.multiple);
    }

    // --- FieldView from Array ---

    #[test]
    fn field_view_array() {
        let field = make_field("items", FieldType::Array(Box::new(FieldType::Boolean)));
        let view = FieldView::from_definition(&field);
        assert_eq!(view.input_type, "array");
        assert_eq!(view.children.len(), 1);
        assert_eq!(view.children[0].input_type, "checkbox");
    }

    // --- FieldView from Composite ---

    #[test]
    fn field_view_composite() {
        let sub_fields = vec![
            make_field("street", FieldType::Text(TextConstraints::unconstrained())),
            make_field("city", FieldType::Text(TextConstraints::unconstrained())),
        ];
        let field = make_field("address", FieldType::Composite(sub_fields));
        let view = FieldView::from_definition(&field);
        assert_eq!(view.input_type, "composite");
        assert_eq!(view.children.len(), 2);
        assert_eq!(view.children[0].name, "street");
        assert_eq!(view.children[1].name, "city");
    }

    // --- FieldView with value ---

    #[test]
    fn field_view_with_current_value() {
        let field = make_field("name", FieldType::Text(TextConstraints::unconstrained()));
        let value = DynamicValue::Text("Alice".into());
        let view = FieldView::from_definition_with_value(&field, Some(&value));
        assert_eq!(view.current_value, Some("Alice".to_string()));
    }

    // --- type_label tests ---

    #[test]
    fn type_label_text() {
        assert_eq!(
            field_type_label(&FieldType::Text(TextConstraints::with_max_length(100))),
            "Text(max: 100)"
        );
        assert_eq!(
            field_type_label(&FieldType::Text(TextConstraints::unconstrained())),
            "Text"
        );
    }

    #[test]
    fn type_label_integer() {
        assert_eq!(
            field_type_label(&FieldType::Integer(
                IntegerConstraints::with_range(0, 100).unwrap()
            )),
            "Integer(min: 0, max: 100)"
        );
    }

    #[test]
    fn type_label_enum() {
        let v = EnumVariants::new(vec!["A".into(), "B".into()]).unwrap();
        assert_eq!(field_type_label(&FieldType::Enum(v)), "Enum(A, B)");
    }

    // --- SchemaView tests ---

    #[test]
    fn schema_view_from_definition() {
        let schema = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Product").unwrap(),
            vec![
                make_field("name", FieldType::Text(TextConstraints::unconstrained())),
                make_field("price", FieldType::Float(FloatConstraints::with_precision(2))),
            ],
            vec![
                Annotation::Version {
                    version: SchemaVersion::new(1).unwrap(),
                },
                Annotation::Display {
                    field: FieldName::new("name").unwrap(),
                },
            ],
        )
        .unwrap();

        let view = SchemaView::from_definition(&schema);
        assert_eq!(view.name, "Product");
        assert_eq!(view.display_field, Some("name".to_string()));
        assert_eq!(view.version, Some(1));
        assert_eq!(view.fields.len(), 2);
        assert_eq!(view.url_name, "Product");
    }

    #[test]
    fn schema_view_no_annotations() {
        let schema = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Simple").unwrap(),
            vec![make_field(
                "value",
                FieldType::Text(TextConstraints::unconstrained()),
            )],
            vec![],
        )
        .unwrap();

        let view = SchemaView::from_definition(&schema);
        assert_eq!(view.display_field, None);
        assert_eq!(view.version, None);
    }

    // --- EntityView tests ---

    #[test]
    fn entity_view_from_entity() {
        let schema = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Contact").unwrap(),
            vec![
                make_field("name", FieldType::Text(TextConstraints::unconstrained())),
                make_field("age", FieldType::Integer(IntegerConstraints::unconstrained())),
            ],
            vec![Annotation::Display {
                field: FieldName::new("name").unwrap(),
            }],
        )
        .unwrap();

        let mut fields = std::collections::BTreeMap::new();
        fields.insert("name".to_string(), DynamicValue::Text("Alice".into()));
        fields.insert("age".to_string(), DynamicValue::Integer(30));
        let entity = Entity::new(SchemaName::new("Contact").unwrap(), fields);

        let view = EntityView::from_entity(&entity, &schema);
        assert_eq!(view.display_value, "Alice");
        assert_eq!(view.field_values.len(), 2);
    }

    #[test]
    fn entity_view_fallback_display() {
        let schema = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Task").unwrap(),
            vec![make_field(
                "title",
                FieldType::Text(TextConstraints::unconstrained()),
            )],
            vec![],
        )
        .unwrap();

        let mut fields = std::collections::BTreeMap::new();
        fields.insert("title".to_string(), DynamicValue::Text("Buy milk".into()));
        let entity = Entity::new(SchemaName::new("Task").unwrap(), fields);

        let view = EntityView::from_entity(&entity, &schema);
        assert_eq!(view.display_value, "Buy milk");
    }

    // --- PaginationView tests ---

    #[test]
    fn pagination_basic() {
        let p = PaginationView::new(100, 25, 0);
        assert_eq!(p.current_page, 1);
        assert_eq!(p.total_pages, 4);
        assert!(!p.has_previous);
        assert!(p.has_next);
    }

    #[test]
    fn pagination_middle() {
        let p = PaginationView::new(100, 25, 50);
        assert_eq!(p.current_page, 3);
        assert!(p.has_previous);
        assert!(p.has_next);
        assert_eq!(p.previous_offset(), 25);
        assert_eq!(p.next_offset(), 75);
    }

    #[test]
    fn pagination_last_page() {
        let p = PaginationView::new(100, 25, 75);
        assert_eq!(p.current_page, 4);
        assert!(p.has_previous);
        assert!(!p.has_next);
    }

    #[test]
    fn pagination_empty() {
        let p = PaginationView::new(0, 25, 0);
        assert_eq!(p.current_page, 1);
        assert_eq!(p.total_pages, 1);
        assert!(!p.has_previous);
        assert!(!p.has_next);
    }

    #[test]
    fn pagination_zero_limit_defaults() {
        let p = PaginationView::new(50, 0, 0);
        assert_eq!(p.limit, 25);
        assert_eq!(p.total_pages, 2);
    }

    #[test]
    fn pagination_not_evenly_divisible() {
        let p = PaginationView::new(26, 25, 0);
        assert_eq!(p.total_pages, 2);
    }

    // --- dynamic_value_display tests ---

    #[test]
    fn display_null() {
        assert_eq!(dynamic_value_display(&DynamicValue::Null), "");
    }

    #[test]
    fn display_text() {
        assert_eq!(
            dynamic_value_display(&DynamicValue::Text("hello".into())),
            "hello"
        );
    }

    #[test]
    fn display_integer() {
        assert_eq!(dynamic_value_display(&DynamicValue::Integer(42)), "42");
    }

    #[test]
    fn display_boolean() {
        assert_eq!(
            dynamic_value_display(&DynamicValue::Boolean(true)),
            "true"
        );
    }

    // --- FieldView with default value ---

    #[test]
    fn field_view_with_default() {
        let field = FieldDefinition::with_modifiers(
            FieldName::new("active").unwrap(),
            FieldType::Boolean,
            vec![FieldModifier::Default {
                value: DefaultValue::Boolean(true),
            }],
        );
        let view = FieldView::from_definition(&field);
        assert_eq!(view.default_value, Some("true".to_string()));
    }

    // --- SchemaEditorView tests ---

    #[test]
    fn schema_editor_view_from_definition() {
        let schema = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Contact").unwrap(),
            vec![
                make_required_field("name", FieldType::Text(TextConstraints::with_max_length(100))),
                make_field("age", FieldType::Integer(IntegerConstraints::with_range(0, 150).unwrap())),
            ],
            vec![
                Annotation::Version {
                    version: SchemaVersion::new(2).unwrap(),
                },
                Annotation::Display {
                    field: FieldName::new("name").unwrap(),
                },
            ],
        )
        .unwrap();

        let view = SchemaEditorView::from_definition(&schema);
        assert_eq!(view.schema_name, "Contact");
        assert_eq!(view.version, "2");
        assert_eq!(view.display_field, "name");
        assert!(view.is_edit);
        assert_eq!(view.existing_name, Some("Contact".to_string()));
        assert_eq!(view.fields.len(), 2);

        let f0 = &view.fields[0];
        assert_eq!(f0.index, 0);
        assert_eq!(f0.name, "name");
        assert_eq!(f0.field_type, "text");
        assert!(f0.required);
        assert_eq!(f0.text_max_length, Some(100));

        let f1 = &view.fields[1];
        assert_eq!(f1.index, 1);
        assert_eq!(f1.name, "age");
        assert_eq!(f1.field_type, "integer");
        assert!(!f1.required);
        assert_eq!(f1.integer_min, Some(0));
        assert_eq!(f1.integer_max, Some(150));
    }

    #[test]
    fn schema_editor_view_new_empty() {
        let view = SchemaEditorView::new_empty();
        assert_eq!(view.schema_name, "");
        assert!(!view.is_edit);
        assert_eq!(view.existing_name, None);
        assert_eq!(view.fields.len(), 1);
        assert_eq!(view.fields[0].field_type, "text");
    }

    #[test]
    fn field_editor_row_from_enum() {
        let variants = EnumVariants::new(vec!["Active".into(), "Inactive".into()]).unwrap();
        let field = make_field("status", FieldType::Enum(variants));
        let row = FieldEditorRow::from_definition(0, &field);
        assert_eq!(row.field_type, "enum");
        assert_eq!(row.enum_variants, "Active\nInactive");
    }

    #[test]
    fn field_editor_row_from_relation() {
        let field = make_field(
            "company",
            FieldType::Relation {
                target: SchemaName::new("Company").unwrap(),
                cardinality: Cardinality::Many,
            },
        );
        let row = FieldEditorRow::from_definition(0, &field);
        assert_eq!(row.field_type, "relation");
        assert_eq!(row.relation_target, "Company");
        assert_eq!(row.relation_cardinality, "many");
    }

    #[test]
    fn field_editor_row_empty() {
        let row = FieldEditorRow::empty(5);
        assert_eq!(row.index, 5);
        assert_eq!(row.name, "");
        assert_eq!(row.old_name, None);
        assert_eq!(row.field_type, "text");
        assert!(!row.required);
    }

    #[test]
    fn field_editor_row_has_old_name() {
        let field = make_field("email", FieldType::Text(TextConstraints::unconstrained()));
        let row = FieldEditorRow::from_definition(0, &field);
        assert_eq!(row.old_name, Some("email".to_string()));
    }

    // --- MigrationPreviewView tests ---

    #[test]
    fn migration_preview_from_plan() {
        use schema_forge_core::migration::DiffEngine;

        let schema = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Contact").unwrap(),
            vec![make_field(
                "name",
                FieldType::Text(TextConstraints::unconstrained()),
            )],
            vec![],
        )
        .unwrap();
        let plan = DiffEngine::create_new(&schema);
        let view = MigrationPreviewView::from_plan(&plan);
        assert!(!view.is_empty);
        assert!(!view.steps.is_empty());
        assert_eq!(view.overall_safety, "Safe");
    }

    #[test]
    fn migration_preview_empty_plan() {
        let plan = schema_forge_core::migration::MigrationPlan::new(
            SchemaId::new(),
            SchemaName::new("Test").unwrap(),
            vec![],
        );
        let view = MigrationPreviewView::from_plan(&plan);
        assert!(view.is_empty);
        assert!(view.steps.is_empty());
    }

    // --- SchemaGraphView tests ---

    fn make_dashboard_entry(name: &str, count: usize) -> DashboardEntry {
        let schema_def = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new(name).unwrap(),
            vec![make_field(
                "name",
                FieldType::Text(TextConstraints::unconstrained()),
            )],
            vec![],
        )
        .unwrap();
        DashboardEntry {
            schema: SchemaView::from_definition(&schema_def),
            entity_count: count,
        }
    }

    #[test]
    fn graph_from_entries_no_relations() {
        let entries = vec![make_dashboard_entry("Contact", 5)];
        let schemas = vec![SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Contact").unwrap(),
            vec![make_field(
                "name",
                FieldType::Text(TextConstraints::unconstrained()),
            )],
            vec![],
        )
        .unwrap()];

        let graph = SchemaGraphView::from_entries(&entries, &schemas);
        assert!(!graph.has_edges);
        assert!(graph.json.contains("\"edges\":[]"));
    }

    #[test]
    fn graph_from_entries_with_relation() {
        let entries = vec![
            make_dashboard_entry("Employee", 10),
            make_dashboard_entry("Company", 3),
        ];
        let schemas = vec![
            SchemaDefinition::new(
                SchemaId::new(),
                SchemaName::new("Employee").unwrap(),
                vec![make_field(
                    "company",
                    FieldType::Relation {
                        target: SchemaName::new("Company").unwrap(),
                        cardinality: Cardinality::One,
                    },
                )],
                vec![],
            )
            .unwrap(),
            SchemaDefinition::new(
                SchemaId::new(),
                SchemaName::new("Company").unwrap(),
                vec![make_field(
                    "name",
                    FieldType::Text(TextConstraints::unconstrained()),
                )],
                vec![],
            )
            .unwrap(),
        ];

        let graph = SchemaGraphView::from_entries(&entries, &schemas);
        assert!(graph.has_edges);
        assert!(graph.json.contains("\"from\":\"Employee\""));
        assert!(graph.json.contains("\"to\":\"Company\""));
        assert!(graph.json.contains("\"label\":\"company\""));
        assert!(graph.json.contains("\"cardinality\":\"One\""));
    }

    #[test]
    fn graph_from_entries_many_cardinality() {
        let entries = vec![
            make_dashboard_entry("Article", 5),
            make_dashboard_entry("Tag", 8),
        ];
        let schemas = vec![
            SchemaDefinition::new(
                SchemaId::new(),
                SchemaName::new("Article").unwrap(),
                vec![make_field(
                    "tags",
                    FieldType::Relation {
                        target: SchemaName::new("Tag").unwrap(),
                        cardinality: Cardinality::Many,
                    },
                )],
                vec![],
            )
            .unwrap(),
            SchemaDefinition::new(
                SchemaId::new(),
                SchemaName::new("Tag").unwrap(),
                vec![make_field(
                    "name",
                    FieldType::Text(TextConstraints::unconstrained()),
                )],
                vec![],
            )
            .unwrap(),
        ];

        let graph = SchemaGraphView::from_entries(&entries, &schemas);
        assert!(graph.has_edges);
        assert!(graph.json.contains("\"cardinality\":\"Many\""));
    }

    #[test]
    fn graph_from_entries_missing_target_excluded() {
        let entries = vec![make_dashboard_entry("Employee", 10)];
        // Employee has a relation to Company, but Company is not in entries
        let schemas = vec![SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Employee").unwrap(),
            vec![make_field(
                "company",
                FieldType::Relation {
                    target: SchemaName::new("Company").unwrap(),
                    cardinality: Cardinality::One,
                },
            )],
            vec![],
        )
        .unwrap()];

        let graph = SchemaGraphView::from_entries(&entries, &schemas);
        assert!(!graph.has_edges, "edge to missing target should be excluded");
        assert!(graph.json.contains("\"edges\":[]"));
    }
}
