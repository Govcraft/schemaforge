//! View-model structs passed into site generator templates.
//!
//! These are the "IR → view" layer: we translate a [`SchemaDefinition`] into
//! small, serde-serializable structs that minijinja can render against
//! without the templates ever touching `schema-forge-core` types directly.

use heck::{ToKebabCase, ToLowerCamelCase, ToPascalCase, ToSnakeCase, ToTitleCase};
use serde::Serialize;

use schema_forge_core::types::{FieldDefinition, SchemaDefinition};

use crate::error::CliError;
use crate::output::OutputContext;

use super::mapping::{field_to_view, FieldMapError};

/// Top-level context rendered against every site-generator template.
#[derive(Debug, Clone, Serialize)]
pub struct SiteContext {
    /// Kebab-cased project name (for `package.json`, `<title>`, etc.).
    pub project_name: String,
    /// The single entity v0 generates pages for.
    pub entity: EntityView,
}

/// One schema projected into a template-friendly view.
#[derive(Debug, Clone, Serialize)]
pub struct EntityView {
    /// PascalCase name — `Employee`.
    pub pascal: String,
    /// snake_case name — `employee`.
    pub snake: String,
    /// kebab-case name — `employee`.
    pub kebab: String,
    /// Human title — `Employee`.
    pub title: String,
    /// Original schema name used as the REST path segment (`/schemas/{name}`).
    pub schema_name: String,
    /// v0-supported fields only. Unsupported fields are dropped with a stderr warning.
    pub fields: Vec<FieldView>,
}

impl EntityView {
    /// Project a [`SchemaDefinition`] into an [`EntityView`], dropping fields
    /// whose type is not supported by the v0 generator.
    pub fn from_schema(def: &SchemaDefinition, output: &OutputContext) -> Result<Self, CliError> {
        let name = def.name.as_str();
        let mut fields = Vec::with_capacity(def.fields.len());
        for f in &def.fields {
            match field_to_view(f) {
                Ok(v) => fields.push(v),
                Err(FieldMapError::Unsupported { field, reason }) => {
                    output.warn(&format!(
                        "site v0: skipping field `{name}.{field}` — {reason}"
                    ));
                }
            }
        }
        Ok(Self {
            pascal: name.to_pascal_case(),
            snake: name.to_snake_case(),
            kebab: name.to_kebab_case(),
            title: name.to_title_case(),
            schema_name: name.to_string(),
            fields,
        })
    }
}

/// One schema field projected into a TS/Zod-aware view model.
#[derive(Debug, Clone, Serialize)]
pub struct FieldView {
    /// Original DSL name (`full_name`).
    pub name: String,
    /// lowerCamelCase JS property name (`fullName`).
    pub camel: String,
    /// Human-readable label (`Full Name`).
    pub label: String,
    /// `true` if the source field carries the `Required` modifier.
    pub required: bool,
    /// TypeScript type expression — `"string"`, `"number"`, `"\"a\" | \"b\""`, etc.
    pub ts_type: String,
    /// Zod schema expression — `z.string().max(255).optional()`.
    pub zod: String,
    /// `true` if this field is a `Relation(One)`.
    pub is_relation: bool,
    /// Target schema name for a relation field.
    pub relation_target: Option<String>,
    /// Enum variants in declaration order (empty for non-enums).
    pub enum_variants: Vec<String>,
    /// High-level kind used by page templates for UI branching.
    pub kind: String,
}

/// Derive the canonical lowerCamelCase JS property name for a DSL field name.
pub fn camel_of(name: &str) -> String {
    name.to_lower_camel_case()
}

/// Derive a human-readable label from a DSL field name.
pub fn label_of(name: &str) -> String {
    name.to_title_case()
}

/// Helper used by [`field_to_view`] to build a `FieldView` without touching
/// heck again at every call site.
pub fn make_field_view(
    field: &FieldDefinition,
    ts_type: String,
    zod: String,
    kind: &'static str,
    is_relation: bool,
    relation_target: Option<String>,
    enum_variants: Vec<String>,
) -> FieldView {
    FieldView {
        name: field.name.as_str().to_string(),
        camel: camel_of(field.name.as_str()),
        label: label_of(field.name.as_str()),
        required: field.is_required(),
        ts_type,
        zod,
        is_relation,
        relation_target,
        enum_variants,
        kind: kind.to_string(),
    }
}
