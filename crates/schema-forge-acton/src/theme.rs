use std::collections::{BTreeMap, HashMap};

use schema_forge_core::types::DynamicValue;
use serde::{Deserialize, Serialize};

use crate::state::{DynForgeBackend, SchemaRegistry};
use crate::views::snake_to_label;

// ---------------------------------------------------------------------------
// Style enums
// ---------------------------------------------------------------------------

/// How entity lists are rendered.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ListStyle {
    Table,
    Cards,
    Compact,
    Kanban,
}

/// How entity detail pages are rendered.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DetailStyle {
    Full,
    Split,
    Tabbed,
}

/// Navigation layout.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NavStyle {
    Sidebar,
    #[serde(rename = "topnav")]
    TopNav,
    Minimal,
}

/// UI density.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Density {
    Compact,
    Comfortable,
    Spacious,
}

/// Per-schema style overrides.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThemeOverride {
    pub list_style: Option<ListStyle>,
    pub detail_style: Option<DetailStyle>,
}

// ---------------------------------------------------------------------------
// Theme
// ---------------------------------------------------------------------------

/// Runtime representation of the Theme system schema.
#[derive(Debug, Clone)]
pub struct Theme {
    pub name: String,
    pub primary_color: String,
    pub secondary_color: String,
    pub accent_color: String,
    pub error_color: String,
    pub background_color: String,
    pub surface_color: String,
    pub text_color: String,
    pub border_radius: String,
    pub font_family: String,
    pub list_style: ListStyle,
    pub detail_style: DetailStyle,
    pub nav_style: NavStyle,
    pub density: Density,
    pub schema_labels: HashMap<String, String>,
    pub field_labels: HashMap<String, HashMap<String, String>>,
    pub schema_overrides: HashMap<String, ThemeOverride>,
    pub view_overrides: HashMap<String, ThemeOverride>,
    pub dashboard_schemas: Vec<String>,
    pub logo_url: Option<String>,
    pub custom_css: Option<String>,
    pub active: bool,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            name: "Default".to_string(),
            primary_color: "#3B82F6".to_string(),
            secondary_color: "#6B7280".to_string(),
            accent_color: "#10B981".to_string(),
            error_color: "#EF4444".to_string(),
            background_color: "#FFFFFF".to_string(),
            surface_color: "#F3F4F6".to_string(),
            text_color: "#111827".to_string(),
            border_radius: "0.5rem".to_string(),
            font_family: "system-ui, sans-serif".to_string(),
            list_style: ListStyle::Table,
            detail_style: DetailStyle::Full,
            nav_style: NavStyle::Sidebar,
            density: Density::Comfortable,
            schema_labels: HashMap::new(),
            field_labels: HashMap::new(),
            schema_overrides: HashMap::new(),
            view_overrides: HashMap::new(),
            dashboard_schemas: Vec::new(),
            logo_url: None,
            custom_css: None,
            active: true,
        }
    }
}

impl Theme {
    /// Construct a `Theme` from entity fields, falling back to defaults for
    /// missing or unparseable values.
    pub fn from_entity(fields: &BTreeMap<String, DynamicValue>) -> Self {
        let defaults = Self::default();

        Self {
            name: extract_text(fields, "name", &defaults.name),
            primary_color: extract_text(fields, "primary_color", &defaults.primary_color),
            secondary_color: extract_text(fields, "secondary_color", &defaults.secondary_color),
            accent_color: extract_text(fields, "accent_color", &defaults.accent_color),
            error_color: extract_text(fields, "error_color", &defaults.error_color),
            background_color: extract_text(fields, "background_color", &defaults.background_color),
            surface_color: extract_text(fields, "surface_color", &defaults.surface_color),
            text_color: extract_text(fields, "text_color", &defaults.text_color),
            border_radius: extract_text(fields, "border_radius", &defaults.border_radius),
            font_family: extract_text(fields, "font_family", &defaults.font_family),
            list_style: extract_enum(fields, "list_style").unwrap_or(defaults.list_style),
            detail_style: extract_enum(fields, "detail_style").unwrap_or(defaults.detail_style),
            nav_style: extract_enum(fields, "nav_style").unwrap_or(defaults.nav_style),
            density: extract_enum(fields, "density").unwrap_or(defaults.density),
            schema_labels: extract_json_map(fields, "schema_labels"),
            field_labels: extract_json_map(fields, "field_labels"),
            schema_overrides: extract_json_map(fields, "schema_overrides"),
            view_overrides: extract_json_map(fields, "view_overrides"),
            dashboard_schemas: extract_text_array(fields, "dashboard_schemas"),
            logo_url: extract_optional_text(fields, "logo_url"),
            custom_css: extract_optional_text(fields, "custom_css"),
            active: extract_bool(fields, "active", defaults.active),
        }
    }

    /// Resolve the list style for a given schema, cascading:
    /// schema override → global → system default.
    pub fn resolve_list_style(&self, schema: &str) -> &ListStyle {
        if let Some(ovr) = self.schema_overrides.get(schema) {
            if let Some(ref ls) = ovr.list_style {
                return ls;
            }
        }
        &self.list_style
    }

    /// Resolve the detail style for a given schema, cascading:
    /// schema override → global → system default.
    pub fn resolve_detail_style(&self, schema: &str) -> &DetailStyle {
        if let Some(ovr) = self.schema_overrides.get(schema) {
            if let Some(ref ds) = ovr.detail_style {
                return ds;
            }
        }
        &self.detail_style
    }

    /// Get the application title. Checks `schema_labels["_app"]`, falls back to `"SchemaForge"`.
    pub fn app_title(&self) -> String {
        self.schema_labels
            .get("_app")
            .cloned()
            .unwrap_or_else(|| "SchemaForge".to_string())
    }

    /// Get the display label for a schema, with fallback to the raw name.
    pub fn schema_label(&self, name: &str) -> String {
        self.schema_labels
            .get(name)
            .cloned()
            .unwrap_or_else(|| name.to_string())
    }

    /// Get the display label for a field, with fallback to `snake_to_label()`.
    pub fn field_label(&self, schema: &str, field: &str) -> String {
        self.field_labels
            .get(schema)
            .and_then(|fields| fields.get(field))
            .cloned()
            .unwrap_or_else(|| snake_to_label(field))
    }

    /// Generate CSS custom properties for this theme.
    pub fn to_css_vars(&self) -> String {
        format!(
            ":root {{\n\
            \x20   --sf-primary: {};\n\
            \x20   --sf-secondary: {};\n\
            \x20   --sf-accent: {};\n\
            \x20   --sf-error: {};\n\
            \x20   --sf-background: {};\n\
            \x20   --sf-surface: {};\n\
            \x20   --sf-text: {};\n\
            \x20   --sf-border-radius: {};\n\
            \x20   --sf-font-family: {};\n\
            \x20   --sf-density-padding: {};\n\
            }}\n",
            self.primary_color,
            self.secondary_color,
            self.accent_color,
            self.error_color,
            self.background_color,
            self.surface_color,
            self.text_color,
            self.border_radius,
            self.font_family,
            self.density_padding(),
        )
    }

    /// CSS padding value for the current density setting.
    pub fn density_padding(&self) -> &str {
        match self.density {
            Density::Compact => "0.25rem",
            Density::Comfortable => "0.5rem",
            Density::Spacious => "1rem",
        }
    }

    /// String name of the current list style (for template selection).
    pub fn list_style_name(&self, schema: &str) -> &str {
        match self.resolve_list_style(schema) {
            ListStyle::Table => "table",
            ListStyle::Cards => "cards",
            ListStyle::Compact => "compact",
            ListStyle::Kanban => "kanban",
        }
    }
}

// ---------------------------------------------------------------------------
// Theme loading
// ---------------------------------------------------------------------------

/// Load the active theme from the database, falling back to defaults.
pub async fn load_active_theme(
    registry: &SchemaRegistry,
    backend: &dyn DynForgeBackend,
) -> Theme {
    // Check if Theme schema is registered
    let theme_def = match registry.get("Theme").await {
        Some(def) => def,
        None => return Theme::default(),
    };

    // Query for the most recently created active theme
    let query = schema_forge_core::query::Query::new(theme_def.id.clone())
        .with_filter(schema_forge_core::query::Filter::eq(
            schema_forge_core::query::FieldPath::single("active"),
            DynamicValue::Boolean(true),
        ))
        .with_sort(
            schema_forge_core::query::FieldPath::single("id"),
            schema_forge_core::query::SortOrder::Descending,
        )
        .with_limit(1);

    let result = match backend.query(&query).await {
        Ok(r) => r,
        Err(_) => return Theme::default(),
    };

    match result.entities.into_iter().next() {
        Some(entity) => Theme::from_entity(&entity.fields),
        None => Theme::default(),
    }
}

/// Reload the active theme into the ArcSwap.
#[cfg(any(feature = "widget-ui", feature = "admin-ui"))]
pub async fn reload_theme(state: &crate::state::ForgeState) {
    let theme = load_active_theme(&state.registry, state.backend.as_ref()).await;
    state.theme.store(std::sync::Arc::new(theme));
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn extract_text(fields: &BTreeMap<String, DynamicValue>, key: &str, default: &str) -> String {
    match fields.get(key) {
        Some(DynamicValue::Text(s)) if !s.is_empty() => s.clone(),
        _ => default.to_string(),
    }
}

fn extract_optional_text(fields: &BTreeMap<String, DynamicValue>, key: &str) -> Option<String> {
    match fields.get(key) {
        Some(DynamicValue::Text(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

fn extract_bool(fields: &BTreeMap<String, DynamicValue>, key: &str, default: bool) -> bool {
    match fields.get(key) {
        Some(DynamicValue::Boolean(b)) => *b,
        _ => default,
    }
}

fn extract_text_array(fields: &BTreeMap<String, DynamicValue>, key: &str) -> Vec<String> {
    match fields.get(key) {
        Some(DynamicValue::Array(arr)) => arr
            .iter()
            .filter_map(|v| match v {
                DynamicValue::Text(s) => Some(s.clone()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn extract_enum<T: serde::de::DeserializeOwned>(
    fields: &BTreeMap<String, DynamicValue>,
    key: &str,
) -> Option<T> {
    match fields.get(key) {
        Some(DynamicValue::Text(s)) => serde_json::from_value(serde_json::Value::String(s.clone())).ok(),
        _ => None,
    }
}

fn extract_json_map<T: serde::de::DeserializeOwned + Default>(
    fields: &BTreeMap<String, DynamicValue>,
    key: &str,
) -> T {
    match fields.get(key) {
        Some(DynamicValue::Json(val)) => {
            serde_json::from_value(val.clone()).unwrap_or_default()
        }
        _ => T::default(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_theme_has_sensible_values() {
        let theme = Theme::default();
        assert_eq!(theme.name, "Default");
        assert_eq!(theme.primary_color, "#3B82F6");
        assert_eq!(theme.list_style, ListStyle::Table);
        assert_eq!(theme.detail_style, DetailStyle::Full);
        assert_eq!(theme.nav_style, NavStyle::Sidebar);
        assert_eq!(theme.density, Density::Comfortable);
        assert!(theme.active);
    }

    #[test]
    fn from_entity_empty_fields_gives_defaults() {
        let fields = BTreeMap::new();
        let theme = Theme::from_entity(&fields);
        assert_eq!(theme.primary_color, "#3B82F6");
        assert_eq!(theme.list_style, ListStyle::Table);
        assert!(theme.active);
    }

    #[test]
    fn from_entity_overrides_fields() {
        let mut fields = BTreeMap::new();
        fields.insert(
            "name".to_string(),
            DynamicValue::Text("My Theme".to_string()),
        );
        fields.insert(
            "primary_color".to_string(),
            DynamicValue::Text("#FF0000".to_string()),
        );
        fields.insert(
            "list_style".to_string(),
            DynamicValue::Text("cards".to_string()),
        );
        fields.insert("active".to_string(), DynamicValue::Boolean(false));

        let theme = Theme::from_entity(&fields);
        assert_eq!(theme.name, "My Theme");
        assert_eq!(theme.primary_color, "#FF0000");
        assert_eq!(theme.list_style, ListStyle::Cards);
        assert!(!theme.active);
    }

    #[test]
    fn resolve_list_style_global() {
        let theme = Theme {
            list_style: ListStyle::Cards,
            ..Theme::default()
        };
        assert_eq!(theme.resolve_list_style("Contact"), &ListStyle::Cards);
    }

    #[test]
    fn resolve_list_style_schema_override() {
        let mut overrides = HashMap::new();
        overrides.insert(
            "Contact".to_string(),
            ThemeOverride {
                list_style: Some(ListStyle::Compact),
                detail_style: None,
            },
        );
        let theme = Theme {
            list_style: ListStyle::Table,
            schema_overrides: overrides,
            ..Theme::default()
        };
        assert_eq!(theme.resolve_list_style("Contact"), &ListStyle::Compact);
        assert_eq!(theme.resolve_list_style("Company"), &ListStyle::Table);
    }

    #[test]
    fn schema_label_with_override() {
        let mut labels = HashMap::new();
        labels.insert("Contact".to_string(), "People".to_string());
        let theme = Theme {
            schema_labels: labels,
            ..Theme::default()
        };
        assert_eq!(theme.schema_label("Contact"), "People");
        assert_eq!(theme.schema_label("Company"), "Company");
    }

    #[test]
    fn field_label_with_override() {
        let mut labels = HashMap::new();
        let mut contact_labels = HashMap::new();
        contact_labels.insert("first_name".to_string(), "Given Name".to_string());
        labels.insert("Contact".to_string(), contact_labels);
        let theme = Theme {
            field_labels: labels,
            ..Theme::default()
        };
        assert_eq!(theme.field_label("Contact", "first_name"), "Given Name");
        assert_eq!(theme.field_label("Contact", "last_name"), "Last Name");
        assert_eq!(theme.field_label("Company", "name"), "Name");
    }

    #[test]
    fn to_css_vars_produces_valid_css() {
        let theme = Theme::default();
        let css = theme.to_css_vars();
        assert!(css.starts_with(":root {"));
        assert!(css.contains("--sf-primary: #3B82F6;"));
        assert!(css.contains("--sf-secondary: #6B7280;"));
        assert!(css.contains("--sf-density-padding: 0.5rem;"));
        assert!(css.ends_with("}\n"));
    }

    #[test]
    fn density_padding_values() {
        let mut theme = Theme {
            density: Density::Compact,
            ..Default::default()
        };
        assert_eq!(theme.density_padding(), "0.25rem");
        theme.density = Density::Comfortable;
        assert_eq!(theme.density_padding(), "0.5rem");
        theme.density = Density::Spacious;
        assert_eq!(theme.density_padding(), "1rem");
    }

    #[test]
    fn list_style_name_values() {
        let theme = Theme::default();
        assert_eq!(theme.list_style_name("Contact"), "table");

        let theme = Theme {
            list_style: ListStyle::Cards,
            ..Theme::default()
        };
        assert_eq!(theme.list_style_name("Contact"), "cards");
    }

    #[test]
    fn serde_roundtrip_list_style() {
        let json = serde_json::to_string(&ListStyle::Cards).unwrap();
        assert_eq!(json, "\"cards\"");
        let back: ListStyle = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ListStyle::Cards);
    }

    #[test]
    fn serde_roundtrip_theme_override() {
        let ovr = ThemeOverride {
            list_style: Some(ListStyle::Compact),
            detail_style: Some(DetailStyle::Split),
        };
        let json = serde_json::to_string(&ovr).unwrap();
        let back: ThemeOverride = serde_json::from_str(&json).unwrap();
        assert_eq!(ovr, back);
    }

    #[test]
    fn from_entity_json_schema_labels() {
        let mut fields = BTreeMap::new();
        let json_val = serde_json::json!({"Contact": "People", "Company": "Org"});
        fields.insert("schema_labels".to_string(), DynamicValue::Json(json_val));

        let theme = Theme::from_entity(&fields);
        assert_eq!(theme.schema_label("Contact"), "People");
        assert_eq!(theme.schema_label("Company"), "Org");
    }

    #[test]
    fn from_entity_dashboard_schemas() {
        let mut fields = BTreeMap::new();
        fields.insert(
            "dashboard_schemas".to_string(),
            DynamicValue::Array(vec![
                DynamicValue::Text("Deal".to_string()),
                DynamicValue::Text("Contact".to_string()),
            ]),
        );

        let theme = Theme::from_entity(&fields);
        assert_eq!(theme.dashboard_schemas, vec!["Deal", "Contact"]);
    }
}
