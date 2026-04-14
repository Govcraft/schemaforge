//! View-model structs passed into site generator templates.
//!
//! These are the "IR → view" layer: we translate a [`SchemaDefinition`] into
//! small, serde-serializable structs that minijinja can render against
//! without the templates ever touching `schema-forge-core` types directly.

use std::collections::BTreeMap;

use heck::{ToKebabCase, ToLowerCamelCase, ToPascalCase, ToSnakeCase, ToTitleCase};
use serde::Serialize;

use schema_forge_core::types::{FieldDefinition, SchemaDefinition};

use crate::error::CliError;
use crate::output::OutputContext;

use super::mapping::{field_to_view, FieldMapError};

/// Compact metadata about a schema's identity — used to look up relation
/// targets (their kebab name, display field, etc.) without cloning the
/// full [`EntityView`].
#[derive(Debug, Clone, Serialize)]
pub struct SchemaMeta {
    /// Original schema name (matches DSL casing, e.g. `Department`).
    pub schema_name: String,
    /// PascalCase identifier used as the TS type name.
    pub pascal: String,
    /// English plural of `pascal` used for list helpers (`listOpportunities`).
    pub pascal_plural: String,
    /// kebab-case slug used in URL paths.
    pub kebab: String,
    /// snake_case slug used in identifiers like React Query keys.
    pub snake: String,
    /// The `@display("field")` target, if the schema declares one.
    pub display_field: Option<String>,
}

impl SchemaMeta {
    /// Build a [`SchemaMeta`] from a schema definition. Pure.
    pub fn from_schema(def: &SchemaDefinition) -> Self {
        let name = def.name.as_str();
        let pascal = name.to_pascal_case();
        Self {
            schema_name: name.to_string(),
            pascal_plural: pluralize(&pascal),
            pascal,
            kebab: name.to_kebab_case(),
            snake: name.to_snake_case(),
            display_field: def.display_field().map(|s| s.to_string()),
        }
    }
}

/// English pluralizer for the site generator. Handles the three rules that
/// actually come up in SchemaForge schema names:
///
/// 1. A consonant followed by `y` → drop `y`, append `ies` (`Opportunity` → `Opportunities`).
/// 2. Ends in a sibilant (`s`, `x`, `z`, `ch`, `sh`) → append `es` (`Box` → `Boxes`, `Address` → `Addresses`).
/// 3. Everything else → append `s`.
///
/// This is intentionally narrow. It's enough to stop emitting `listOpportunitys`
/// and `listForecastEntrys`, and it has no foreign irregulars (matrices, indices,
/// criteria) — those need a real inflector library and don't appear in any
/// production SchemaForge schema today.
pub fn pluralize(word: &str) -> String {
    if word.is_empty() {
        return String::new();
    }
    let lower = word.to_ascii_lowercase();
    if lower.ends_with("ch") || lower.ends_with("sh") {
        return format!("{word}es");
    }
    if let Some(last) = lower.chars().last() {
        if matches!(last, 's' | 'x' | 'z') {
            return format!("{word}es");
        }
        if last == 'y' {
            let prev = lower.chars().rev().nth(1);
            if let Some(c) = prev {
                if !matches!(c, 'a' | 'e' | 'i' | 'o' | 'u') {
                    let mut out = word[..word.len() - 1].to_string();
                    out.push_str("ies");
                    return out;
                }
            }
        }
    }
    format!("{word}s")
}

/// Top-level context rendered against every global site-generator template.
///
/// Per-entity templates (`pages/<entity>/list.tsx`, etc.) receive a
/// [`PageContext`] instead so that they have direct access to their single
/// `entity` without reaching through the list.
#[derive(Debug, Clone, Serialize)]
pub struct SiteContext {
    /// Kebab-cased project name (for `package.json`, `<title>`, etc.).
    pub project_name: String,
    /// Every non-system schema, projected into a generator-friendly view.
    pub entities: Vec<EntityView>,
}

/// Context passed to per-entity page templates. Carries the same
/// `project_name` as the global context so both kinds of templates see a
/// consistent variable, plus the single focused [`EntityView`].
#[derive(Debug, Clone, Serialize)]
pub struct PageContext {
    pub project_name: String,
    pub entity: EntityView,
}

/// One schema projected into a template-friendly view.
#[derive(Debug, Clone, Serialize)]
pub struct EntityView {
    /// PascalCase name — `Employee`.
    pub pascal: String,
    /// English plural of `pascal` — `Employees`, `Opportunities`, `Addresses`.
    pub pascal_plural: String,
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
    /// The field nominated by `@display("...")`, if any. Used for
    /// breadcrumbs and list-view "headline" rendering.
    pub display_field: Option<String>,
    /// `true` iff any top-level or composite-nested field on this entity
    /// is a `Relation(One)`. Templates consult this flag so they only
    /// import the `RelationSelect` component when it will actually be
    /// referenced.
    pub has_relation_one: bool,
}

impl EntityView {
    /// Project a [`SchemaDefinition`] into an [`EntityView`], dropping fields
    /// whose type is not supported by the v0 generator. `catalog` is the
    /// map of all known schemas keyed by their canonical name — used so the
    /// mapper can look up relation targets and fill in their display field.
    pub fn from_schema(
        def: &SchemaDefinition,
        catalog: &BTreeMap<String, SchemaMeta>,
        output: &OutputContext,
    ) -> Result<Self, CliError> {
        let name = def.name.as_str();
        let mut fields = Vec::with_capacity(def.fields.len());
        for f in &def.fields {
            match field_to_view(f, catalog) {
                Ok(v) => fields.push(v),
                Err(FieldMapError::Unsupported { field, reason }) => {
                    output.warn(&format!(
                        "site v0: skipping field `{name}.{field}` — {reason}"
                    ));
                }
            }
        }
        let has_relation_one = fields.iter().any(has_relation_one_field);
        let pascal = name.to_pascal_case();
        Ok(Self {
            pascal_plural: pluralize(&pascal),
            pascal,
            snake: name.to_snake_case(),
            kebab: name.to_kebab_case(),
            title: name.to_title_case(),
            schema_name: name.to_string(),
            fields,
            display_field: def.display_field().map(|s| s.to_string()),
            has_relation_one,
        })
    }
}

/// Recursive check: does this field or any nested composite sub-field
/// contain a `relation_one`?
fn has_relation_one_field(f: &FieldView) -> bool {
    f.kind == "relation_one" || f.sub_fields.iter().any(has_relation_one_field)
}

/// One schema field projected into a TS/Zod-aware view model.
#[derive(Debug, Clone, Serialize)]
pub struct FieldView {
    /// Original DSL name (`full_name`). For nested composite sub-fields,
    /// this is the dot-path from the top-level entity field
    /// (`emergency_contact.phone`). React Hook Form consumes dot-paths
    /// natively for nested object state.
    pub name: String,
    /// Leaf name (no dot-path) — used for labels and grouping.
    pub leaf: String,
    /// lowerCamelCase JS property name for the leaf (`fullName`).
    pub camel: String,
    /// Human-readable label (`Full Name`).
    pub label: String,
    /// `true` if the source field carries the `Required` modifier.
    pub required: bool,
    /// TypeScript type expression — `"string"`, `"number"`, `"\"a\" | \"b\""`, etc.
    pub ts_type: String,
    /// Zod schema expression — `z.string().max(255).optional()`.
    pub zod: String,
    /// `true` if this field is a `Relation(One)` or `Relation(Many)`.
    pub is_relation: bool,
    /// Target schema name for a relation field.
    pub relation_target: Option<String>,
    /// kebab-case slug of the relation target (for URL composition).
    pub relation_target_kebab: Option<String>,
    /// The relation target's `@display("...")` field, if any — lets the
    /// edit template render `"{display}: {id}"` option labels.
    pub relation_display_field: Option<String>,
    /// Enum variants in declaration order (empty for non-enums).
    pub enum_variants: Vec<String>,
    /// High-level kind used by page templates for UI branching.
    pub kind: String,
    /// `@widget("...")` hint as the canonical snake_case token, if present.
    pub widget: Option<String>,
    /// `@format("...")` hint as the canonical snake_case token, if present.
    pub format: Option<String>,
    /// For `kind == "array"`: the scalar kind of each element
    /// (`"text"`, `"integer"`, `"float"`, `"boolean"`, `"enum"`).
    pub item_kind: Option<String>,
    /// For `kind == "array"` whose elements are enums: the variant list.
    pub item_enum_variants: Vec<String>,
    /// For `kind == "composite"`: the flattened sub-fields, each with
    /// `name` set to its dot-path. Empty for non-composite fields.
    pub sub_fields: Vec<FieldView>,
    /// For `kind == "enum"`: map from variant name to its `@enum_colors`
    /// color token (one of `neutral|gray|red|amber|green|blue|purple|violet|teal|rose`).
    /// Empty when the field carries no `@enum_colors` annotation; variants
    /// without an explicit entry render with the default neutral badge.
    pub enum_colors: BTreeMap<String, String>,
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
/// heck again at every call site. The caller supplies the leaf name and
/// optional dot-path prefix; the returned view's `name` will be
/// `prefix.leaf` (or just `leaf` if prefix is empty).
pub fn make_field_view(
    field: &FieldDefinition,
    ts_type: String,
    zod: String,
    kind: &'static str,
    is_relation: bool,
    relation_target: Option<String>,
    enum_variants: Vec<String>,
) -> FieldView {
    let leaf = field.name.as_str().to_string();
    FieldView {
        name: leaf.clone(),
        leaf: leaf.clone(),
        camel: camel_of(&leaf),
        label: label_of(&leaf),
        required: field.is_required(),
        ts_type,
        zod,
        is_relation,
        relation_target,
        relation_target_kebab: None,
        relation_display_field: None,
        enum_variants,
        kind: kind.to_string(),
        widget: field.widget_type_hint().map(|w| w.as_str().to_string()),
        format: field.format_type_hint().map(|fmt| fmt.as_str().to_string()),
        item_kind: None,
        item_enum_variants: Vec::new(),
        sub_fields: Vec::new(),
        enum_colors: field
            .enum_colors()
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().to_string()))
                    .collect()
            })
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::pluralize;

    #[test]
    fn consonant_y_becomes_ies() {
        assert_eq!(pluralize("Opportunity"), "Opportunities");
        assert_eq!(pluralize("ForecastEntry"), "ForecastEntries");
        assert_eq!(pluralize("Category"), "Categories");
    }

    #[test]
    fn vowel_y_just_appends_s() {
        assert_eq!(pluralize("Day"), "Days");
        assert_eq!(pluralize("Survey"), "Surveys");
    }

    #[test]
    fn sibilants_get_es() {
        assert_eq!(pluralize("Box"), "Boxes");
        assert_eq!(pluralize("Dish"), "Dishes");
        assert_eq!(pluralize("Watch"), "Watches");
        assert_eq!(pluralize("Address"), "Addresses");
    }

    #[test]
    fn defaults_to_s() {
        assert_eq!(pluralize("Employee"), "Employees");
        assert_eq!(pluralize("Task"), "Tasks");
        assert_eq!(pluralize("Certification"), "Certifications");
    }

    #[test]
    fn empty_stays_empty() {
        assert_eq!(pluralize(""), "");
    }
}
