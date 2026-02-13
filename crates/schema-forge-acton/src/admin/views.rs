// Re-export shared view types for backward compatibility.
pub use crate::views::{
    field_type_label, snake_to_label, EntityView, FieldView, PaginationView, SchemaView,
};

use schema_forge_core::migration::{MigrationPlan, MigrationSafety, MigrationStep};
use schema_forge_core::types::{
    Annotation, Cardinality, FieldDefinition, FieldModifier, FieldType, SchemaDefinition,
};

/// Dashboard entry for a schema with entity count.
#[derive(Debug, Clone)]
pub struct DashboardEntry {
    pub schema: SchemaView,
    pub entity_count: usize,
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
        let indexed = field
            .modifiers
            .iter()
            .any(|m| matches!(m, FieldModifier::Indexed));

        let (default_enabled, default_value) = field
            .modifiers
            .iter()
            .find_map(|m| match m {
                FieldModifier::Default { value } => Some((true, value.to_string())),
                _ => None,
            })
            .unwrap_or((false, String::new()));

        let (
            field_type,
            text_max_length,
            integer_min,
            integer_max,
            float_precision,
            enum_variants,
            relation_target,
            relation_cardinality,
        ) = match &field.field_type {
            FieldType::Text(c) => (
                "text".to_string(),
                c.max_length,
                None,
                None,
                None,
                String::new(),
                String::new(),
                "one".to_string(),
            ),
            FieldType::RichText => (
                "richtext".to_string(),
                None,
                None,
                None,
                None,
                String::new(),
                String::new(),
                "one".to_string(),
            ),
            FieldType::Integer(c) => (
                "integer".to_string(),
                None,
                c.min,
                c.max,
                None,
                String::new(),
                String::new(),
                "one".to_string(),
            ),
            FieldType::Float(c) => (
                "float".to_string(),
                None,
                None,
                None,
                c.precision,
                String::new(),
                String::new(),
                "one".to_string(),
            ),
            FieldType::Boolean => (
                "boolean".to_string(),
                None,
                None,
                None,
                None,
                String::new(),
                String::new(),
                "one".to_string(),
            ),
            FieldType::DateTime => (
                "datetime".to_string(),
                None,
                None,
                None,
                None,
                String::new(),
                String::new(),
                "one".to_string(),
            ),
            FieldType::Enum(variants) => (
                "enum".to_string(),
                None,
                None,
                None,
                None,
                variants.as_slice().join("\n"),
                String::new(),
                "one".to_string(),
            ),
            FieldType::Json => (
                "json".to_string(),
                None,
                None,
                None,
                None,
                String::new(),
                String::new(),
                "one".to_string(),
            ),
            FieldType::Relation {
                target,
                cardinality,
            } => {
                let card = match cardinality {
                    Cardinality::One => "one",
                    Cardinality::Many => "many",
                    _ => "one",
                };
                (
                    "relation".to_string(),
                    None,
                    None,
                    None,
                    None,
                    String::new(),
                    target.as_str().to_string(),
                    card.to_string(),
                )
            }
            _ => (
                "text".to_string(),
                None,
                None,
                None,
                None,
                String::new(),
                String::new(),
                "one".to_string(),
            ),
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
            MigrationSafety::Safe => ("Safe", "badge-success"),
            MigrationSafety::RequiresConfirmation => ("Requires Confirmation", "badge-warning"),
            MigrationSafety::Destructive => ("Destructive", "badge-error"),
            _ => ("Unknown", "badge-warning"),
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

    // --- SchemaEditorView tests ---

    #[test]
    fn schema_editor_view_from_definition() {
        let schema = SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Contact").unwrap(),
            vec![
                make_required_field(
                    "name",
                    FieldType::Text(TextConstraints::with_max_length(100)),
                ),
                make_field(
                    "age",
                    FieldType::Integer(IntegerConstraints::with_range(0, 150).unwrap()),
                ),
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
        assert!(
            !graph.has_edges,
            "edge to missing target should be excluded"
        );
        assert!(graph.json.contains("\"edges\":[]"));
    }
}
