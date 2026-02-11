use std::collections::HashSet;
use std::fmt;

use schema_forge_core::types::{Annotation, FieldName, SchemaDefinition, SchemaName, TenantKind};

/// Configuration for the multi-tenancy model, derived from `@tenant` annotations.
///
/// Built once during extension initialization by scanning all registered schemas.
/// When `is_enabled()` is true, the system will auto-inject `_tenant` fields
/// into entity creation and scope queries by tenant.
#[derive(Debug, Clone)]
pub struct TenantConfig {
    /// The root tenant schema (if any).
    pub root_schema: Option<SchemaName>,
    /// Ordered hierarchy of tenant levels (root first, children after).
    pub hierarchy: Vec<TenantLevel>,
}

/// A single level in the tenant hierarchy.
#[derive(Debug, Clone)]
pub struct TenantLevel {
    /// The schema name of this tenant level.
    pub schema: SchemaName,
    /// Parent tenant schema (`None` for root).
    pub parent: Option<SchemaName>,
    /// Field on this schema that references the parent tenant.
    pub parent_field: Option<FieldName>,
}

/// Errors in tenant configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TenantConfigError {
    /// Multiple schemas have `@tenant(root)`.
    MultipleRoots { first: String, second: String },
    /// A `@tenant(child: "X")` references a non-existent schema.
    InvalidParent { schema: String, parent: String },
    /// The tenant hierarchy has a cycle.
    CycleDetected { schema: String },
}

impl fmt::Display for TenantConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MultipleRoots { first, second } => {
                write!(
                    f,
                    "multiple tenant roots: '{first}' and '{second}' both declare @tenant(root)"
                )
            }
            Self::InvalidParent { schema, parent } => {
                write!(
                    f,
                    "tenant schema '{schema}' references non-existent parent '{parent}'"
                )
            }
            Self::CycleDetected { schema } => {
                write!(f, "cycle detected in tenant hierarchy at schema '{schema}'")
            }
        }
    }
}

impl std::error::Error for TenantConfigError {}

impl TenantConfig {
    /// Build a `TenantConfig` from a list of schema definitions.
    ///
    /// Scans all schemas for `@tenant` annotations and validates:
    /// - At most one root tenant
    /// - All `@tenant(child: "X")` reference existing schemas with `@tenant` annotations
    /// - No cycles in the parent chain
    ///
    /// Returns a disabled config (no root) when no `@tenant` annotations are found.
    pub fn from_schemas(schemas: &[SchemaDefinition]) -> Result<Self, TenantConfigError> {
        let mut root: Option<SchemaName> = None;
        let mut levels = Vec::new();

        // Collect all schemas with @tenant annotations
        for schema in schemas {
            for annotation in &schema.annotations {
                if let Annotation::Tenant(kind) = annotation {
                    match kind {
                        TenantKind::Root => {
                            if let Some(ref existing) = root {
                                return Err(TenantConfigError::MultipleRoots {
                                    first: existing.as_str().to_string(),
                                    second: schema.name.as_str().to_string(),
                                });
                            }
                            root = Some(schema.name.clone());
                            levels.push(TenantLevel {
                                schema: schema.name.clone(),
                                parent: None,
                                parent_field: None,
                            });
                        }
                        TenantKind::Child { parent } => {
                            // Find the field on this schema that references the parent.
                            // By convention, look for a field whose name matches the
                            // parent schema name in snake_case, or fall back to None.
                            let parent_field = find_parent_field(schema, parent);
                            levels.push(TenantLevel {
                                schema: schema.name.clone(),
                                parent: Some(parent.clone()),
                                parent_field,
                            });
                        }
                    }
                }
            }
        }

        // If no tenant annotations, return disabled config
        if root.is_none() && levels.is_empty() {
            return Ok(Self {
                root_schema: None,
                hierarchy: Vec::new(),
            });
        }

        // Collect all tenant schema names for validation
        let tenant_schemas: HashSet<String> = levels
            .iter()
            .map(|l| l.schema.as_str().to_string())
            .collect();

        // Validate parent references and detect cycles
        for level in &levels {
            if let Some(ref parent) = level.parent {
                if !tenant_schemas.contains(parent.as_str()) {
                    return Err(TenantConfigError::InvalidParent {
                        schema: level.schema.as_str().to_string(),
                        parent: parent.as_str().to_string(),
                    });
                }
            }
        }

        // Cycle detection: for each child, walk the parent chain
        for level in &levels {
            if level.parent.is_some() {
                let mut visited = HashSet::new();
                visited.insert(level.schema.as_str().to_string());
                let mut current = level.parent.as_ref();

                while let Some(parent_name) = current {
                    let parent_str = parent_name.as_str().to_string();
                    if !visited.insert(parent_str.clone()) {
                        return Err(TenantConfigError::CycleDetected {
                            schema: level.schema.as_str().to_string(),
                        });
                    }
                    // Find the parent level
                    current = levels
                        .iter()
                        .find(|l| l.schema.as_str() == parent_name.as_str())
                        .and_then(|l| l.parent.as_ref());
                }
            }
        }

        // Sort hierarchy: root first, then children in dependency order
        let mut ordered = Vec::new();
        let mut placed: HashSet<String> = HashSet::new();

        // Place root first
        if let Some(ref root_name) = root {
            ordered.push(
                levels
                    .iter()
                    .find(|l| l.schema.as_str() == root_name.as_str())
                    .expect("root must be in levels")
                    .clone(),
            );
            placed.insert(root_name.as_str().to_string());
        }

        // Iteratively place children whose parents are already placed
        let mut remaining: Vec<_> = levels
            .iter()
            .filter(|l| l.parent.is_some())
            .cloned()
            .collect();

        while !remaining.is_empty() {
            let before = remaining.len();
            remaining.retain(|l| {
                if let Some(ref parent) = l.parent {
                    if placed.contains(parent.as_str()) {
                        ordered.push(l.clone());
                        placed.insert(l.schema.as_str().to_string());
                        return false; // remove from remaining
                    }
                }
                true // keep in remaining
            });
            if remaining.len() == before {
                // No progress -- should not happen if cycle detection passed
                break;
            }
        }

        Ok(Self {
            root_schema: root,
            hierarchy: ordered,
        })
    }

    /// Returns `true` when multi-tenancy is configured (a root tenant exists).
    pub fn is_enabled(&self) -> bool {
        self.root_schema.is_some()
    }
}

/// Find the field on a schema that references the parent tenant schema.
///
/// Looks for a field whose name matches the parent schema name in snake_case
/// (e.g., parent "Organization" -> field "organization"). Returns `None` if
/// no such field exists.
fn find_parent_field(schema: &SchemaDefinition, parent: &SchemaName) -> Option<FieldName> {
    // Convert PascalCase parent name to snake_case for field lookup
    let parent_lower = parent.as_str().to_lowercase();
    schema
        .fields
        .iter()
        .find(|f| f.name.as_str() == parent_lower)
        .map(|f| f.name.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use schema_forge_core::types::{
        FieldDefinition, FieldName, FieldType, SchemaId, TextConstraints,
    };

    fn make_field(name: &str) -> FieldDefinition {
        FieldDefinition::new(
            FieldName::new(name).unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
        )
    }

    fn make_schema(name: &str, annotations: Vec<Annotation>) -> SchemaDefinition {
        SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new(name).unwrap(),
            vec![make_field("name")],
            annotations,
        )
        .unwrap()
    }

    fn make_schema_with_fields(
        name: &str,
        fields: Vec<FieldDefinition>,
        annotations: Vec<Annotation>,
    ) -> SchemaDefinition {
        SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new(name).unwrap(),
            fields,
            annotations,
        )
        .unwrap()
    }

    // -----------------------------------------------------------------------
    // from_schemas tests
    // -----------------------------------------------------------------------

    #[test]
    fn from_schemas_no_tenant_annotations_returns_disabled() {
        let schemas = vec![
            make_schema("Contact", vec![]),
            make_schema("Company", vec![]),
        ];
        let config = TenantConfig::from_schemas(&schemas).unwrap();
        assert!(!config.is_enabled());
        assert!(config.root_schema.is_none());
        assert!(config.hierarchy.is_empty());
    }

    #[test]
    fn from_schemas_empty_returns_disabled() {
        let config = TenantConfig::from_schemas(&[]).unwrap();
        assert!(!config.is_enabled());
    }

    #[test]
    fn from_schemas_root_only_returns_enabled() {
        let schemas = vec![make_schema(
            "Organization",
            vec![Annotation::Tenant(TenantKind::Root)],
        )];
        let config = TenantConfig::from_schemas(&schemas).unwrap();
        assert!(config.is_enabled());
        assert_eq!(
            config.root_schema.as_ref().unwrap().as_str(),
            "Organization"
        );
        assert_eq!(config.hierarchy.len(), 1);
        assert_eq!(config.hierarchy[0].schema.as_str(), "Organization");
        assert!(config.hierarchy[0].parent.is_none());
    }

    #[test]
    fn from_schemas_root_plus_child_builds_hierarchy() {
        let schemas = vec![
            make_schema("Organization", vec![Annotation::Tenant(TenantKind::Root)]),
            make_schema_with_fields(
                "Team",
                vec![make_field("name"), make_field("organization")],
                vec![Annotation::Tenant(TenantKind::Child {
                    parent: SchemaName::new("Organization").unwrap(),
                })],
            ),
        ];
        let config = TenantConfig::from_schemas(&schemas).unwrap();
        assert!(config.is_enabled());
        assert_eq!(config.hierarchy.len(), 2);
        assert_eq!(config.hierarchy[0].schema.as_str(), "Organization");
        assert!(config.hierarchy[0].parent.is_none());
        assert_eq!(config.hierarchy[1].schema.as_str(), "Team");
        assert_eq!(
            config.hierarchy[1].parent.as_ref().unwrap().as_str(),
            "Organization"
        );
        // parent_field should be "organization" (lowercase of parent schema name)
        assert_eq!(
            config.hierarchy[1].parent_field.as_ref().unwrap().as_str(),
            "organization"
        );
    }

    #[test]
    fn from_schemas_rejects_multiple_roots() {
        let schemas = vec![
            make_schema("Organization", vec![Annotation::Tenant(TenantKind::Root)]),
            make_schema("Company", vec![Annotation::Tenant(TenantKind::Root)]),
        ];
        let err = TenantConfig::from_schemas(&schemas).unwrap_err();
        assert!(matches!(err, TenantConfigError::MultipleRoots { .. }));
    }

    #[test]
    fn from_schemas_rejects_invalid_parent_reference() {
        let schemas = vec![make_schema(
            "Team",
            vec![Annotation::Tenant(TenantKind::Child {
                parent: SchemaName::new("Nonexistent").unwrap(),
            })],
        )];
        let err = TenantConfig::from_schemas(&schemas).unwrap_err();
        assert!(matches!(err, TenantConfigError::InvalidParent { .. }));
        if let TenantConfigError::InvalidParent { schema, parent } = &err {
            assert_eq!(schema, "Team");
            assert_eq!(parent, "Nonexistent");
        }
    }

    #[test]
    fn from_schemas_detects_cycle() {
        let schemas = vec![
            make_schema(
                "Alpha",
                vec![Annotation::Tenant(TenantKind::Child {
                    parent: SchemaName::new("Beta").unwrap(),
                })],
            ),
            make_schema(
                "Beta",
                vec![Annotation::Tenant(TenantKind::Child {
                    parent: SchemaName::new("Alpha").unwrap(),
                })],
            ),
        ];
        let err = TenantConfig::from_schemas(&schemas).unwrap_err();
        assert!(matches!(err, TenantConfigError::CycleDetected { .. }));
    }

    // -----------------------------------------------------------------------
    // Error trait tests
    // -----------------------------------------------------------------------

    #[test]
    fn tenant_config_error_display_variants() {
        let multiple = TenantConfigError::MultipleRoots {
            first: "Org".into(),
            second: "Company".into(),
        };
        let msg = multiple.to_string();
        assert!(msg.contains("Org"));
        assert!(msg.contains("Company"));
        assert!(msg.contains("multiple tenant roots"));

        let invalid = TenantConfigError::InvalidParent {
            schema: "Team".into(),
            parent: "Missing".into(),
        };
        let msg = invalid.to_string();
        assert!(msg.contains("Team"));
        assert!(msg.contains("Missing"));
        assert!(msg.contains("non-existent parent"));

        let cycle = TenantConfigError::CycleDetected {
            schema: "Alpha".into(),
        };
        let msg = cycle.to_string();
        assert!(msg.contains("Alpha"));
        assert!(msg.contains("cycle detected"));
    }

    #[test]
    fn tenant_config_error_is_std_error() {
        let err: Box<dyn std::error::Error> = Box::new(TenantConfigError::CycleDetected {
            schema: "Test".into(),
        });
        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn tenant_config_error_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<TenantConfigError>();
    }
}
