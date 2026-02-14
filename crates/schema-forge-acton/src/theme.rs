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
#[serde(rename_all = "snake_case")]
pub enum ListStyle {
    Table,
    Cards,
    Compact,
    Kanban,
    GridBadge,
    GridProfile,
    GridDirectory,
    GridLink,
    GridGallery,
    GridDetail,
    GridActions,
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

/// Dashboard stats display style.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StatsStyle {
    Simple,
    Cards,
    WithIcons,
    SharedBorders,
    Trending,
    GridActions,
    GridBadge,
}

/// Card container style.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CardStyle {
    Basic,
    Well,
    EdgeToEdge,
    WellEdgeToEdge,
}

/// Content container width constraint and mobile padding.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContainerStyle {
    Standard,
    FullMobile,
    Breakpoint,
    BreakpointFullMobile,
    Narrow,
}

/// Page heading style.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HeadingStyle {
    WithActions,
    WithActionsAndBreadcrumbs,
    CardWithAvatarAndStats,
    WithAvatarAndActions,
    WithBannerImage,
    WithFiltersAndActions,
    WithLogoMetaAndActions,
    WithMetaActionsAndBreadcrumbs,
    WithMetaAndActions,
}

/// Per-schema style overrides.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThemeOverride {
    pub list_style: Option<ListStyle>,
    pub detail_style: Option<DetailStyle>,
    pub heading_style: Option<HeadingStyle>,
    pub stats_style: Option<StatsStyle>,
    pub card_style: Option<CardStyle>,
    pub container_style: Option<ContainerStyle>,
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
    pub heading_style: HeadingStyle,
    pub stats_style: StatsStyle,
    pub card_style: CardStyle,
    pub container_style: ContainerStyle,
    pub schema_labels: HashMap<String, String>,
    pub field_labels: HashMap<String, HashMap<String, String>>,
    pub schema_overrides: HashMap<String, ThemeOverride>,
    pub view_overrides: HashMap<String, ThemeOverride>,
    pub dashboard_schemas: Vec<String>,
    pub logo_url: Option<String>,
    pub favicon_url: Option<String>,
    pub head_html: Option<String>,
    pub nav_extra_html: Option<String>,
    pub footer_html: Option<String>,
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
            background_color: "#111827".to_string(),
            surface_color: "#1F2937".to_string(),
            text_color: "#F1F5F9".to_string(),
            border_radius: "0.5rem".to_string(),
            font_family: "system-ui, sans-serif".to_string(),
            list_style: ListStyle::Table,
            detail_style: DetailStyle::Full,
            nav_style: NavStyle::Sidebar,
            density: Density::Comfortable,
            heading_style: HeadingStyle::WithActions,
            stats_style: StatsStyle::Simple,
            card_style: CardStyle::Basic,
            container_style: ContainerStyle::Standard,
            schema_labels: HashMap::new(),
            field_labels: HashMap::new(),
            schema_overrides: HashMap::new(),
            view_overrides: HashMap::new(),
            dashboard_schemas: Vec::new(),
            logo_url: None,
            favicon_url: None,
            head_html: None,
            nav_extra_html: None,
            footer_html: None,
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
            heading_style: extract_enum(fields, "heading_style").unwrap_or(defaults.heading_style),
            stats_style: extract_enum(fields, "stats_style").unwrap_or(defaults.stats_style),
            card_style: extract_enum(fields, "card_style").unwrap_or(defaults.card_style),
            container_style: extract_enum(fields, "container_style").unwrap_or(defaults.container_style),
            schema_labels: extract_json_map(fields, "schema_labels"),
            field_labels: extract_json_map(fields, "field_labels"),
            schema_overrides: extract_json_map(fields, "schema_overrides"),
            view_overrides: extract_json_map(fields, "view_overrides"),
            dashboard_schemas: extract_text_array(fields, "dashboard_schemas"),
            logo_url: extract_optional_text(fields, "logo_url"),
            favicon_url: extract_optional_text(fields, "favicon_url"),
            head_html: extract_optional_text(fields, "head_html"),
            nav_extra_html: extract_optional_text(fields, "nav_extra_html"),
            footer_html: extract_optional_text(fields, "footer_html"),
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

    /// Resolve the heading style for a given schema, cascading:
    /// schema override → global → system default.
    pub fn resolve_heading_style(&self, schema: &str) -> &HeadingStyle {
        if let Some(ovr) = self.schema_overrides.get(schema) {
            if let Some(ref hs) = ovr.heading_style {
                return hs;
            }
        }
        &self.heading_style
    }

    /// String name of the current heading style (for template selection).
    pub fn heading_style_name(&self, schema: &str) -> &str {
        match self.resolve_heading_style(schema) {
            HeadingStyle::WithActions => "with_actions",
            HeadingStyle::WithActionsAndBreadcrumbs => "with_actions_and_breadcrumbs",
            HeadingStyle::CardWithAvatarAndStats => "card_with_avatar_and_stats",
            HeadingStyle::WithAvatarAndActions => "with_avatar_and_actions",
            HeadingStyle::WithBannerImage => "with_banner_image",
            HeadingStyle::WithFiltersAndActions => "with_filters_and_actions",
            HeadingStyle::WithLogoMetaAndActions => "with_logo_meta_and_actions",
            HeadingStyle::WithMetaActionsAndBreadcrumbs => "with_meta_actions_and_breadcrumbs",
            HeadingStyle::WithMetaAndActions => "with_meta_and_actions",
        }
    }

    /// Resolve the stats style, cascading: schema override → global → system default.
    pub fn resolve_stats_style(&self, schema: &str) -> &StatsStyle {
        if let Some(ovr) = self.schema_overrides.get(schema) {
            if let Some(ref ss) = ovr.stats_style {
                return ss;
            }
        }
        &self.stats_style
    }

    /// String name of the current stats style (for template selection).
    pub fn stats_style_name(&self, schema: &str) -> &str {
        match self.resolve_stats_style(schema) {
            StatsStyle::Simple => "simple",
            StatsStyle::Cards => "cards",
            StatsStyle::WithIcons => "with_icons",
            StatsStyle::SharedBorders => "shared_borders",
            StatsStyle::Trending => "trending",
            StatsStyle::GridActions => "grid_actions",
            StatsStyle::GridBadge => "grid_badge",
        }
    }

    /// Resolve the card style, cascading: schema override → global → system default.
    pub fn resolve_card_style(&self, schema: &str) -> &CardStyle {
        if let Some(ovr) = self.schema_overrides.get(schema) {
            if let Some(ref cs) = ovr.card_style {
                return cs;
            }
        }
        &self.card_style
    }

    /// String name of the current card style (for CSS class selection).
    pub fn card_style_name(&self, schema: &str) -> &str {
        match self.resolve_card_style(schema) {
            CardStyle::Basic => "basic",
            CardStyle::Well => "well",
            CardStyle::EdgeToEdge => "edge-to-edge",
            CardStyle::WellEdgeToEdge => "well-edge-to-edge",
        }
    }

    /// Resolve the container style, cascading: schema override → global → system default.
    pub fn resolve_container_style(&self, schema: &str) -> &ContainerStyle {
        if let Some(ovr) = self.schema_overrides.get(schema) {
            if let Some(ref cs) = ovr.container_style {
                return cs;
            }
        }
        &self.container_style
    }

    /// String name of the current container style (for CSS class selection).
    pub fn container_style_name(&self, schema: &str) -> &str {
        match self.resolve_container_style(schema) {
            ContainerStyle::Standard => "standard",
            ContainerStyle::FullMobile => "full-mobile",
            ContainerStyle::Breakpoint => "breakpoint",
            ContainerStyle::BreakpointFullMobile => "breakpoint-full-mobile",
            ContainerStyle::Narrow => "narrow",
        }
    }

    /// String name of the current list style (for template selection).
    pub fn list_style_name(&self, schema: &str) -> &str {
        match self.resolve_list_style(schema) {
            ListStyle::Table => "table",
            ListStyle::Cards => "cards",
            ListStyle::Compact => "compact",
            ListStyle::Kanban => "kanban",
            ListStyle::GridBadge => "grid_badge",
            ListStyle::GridProfile => "grid_profile",
            ListStyle::GridDirectory => "grid_directory",
            ListStyle::GridLink => "grid_link",
            ListStyle::GridGallery => "grid_gallery",
            ListStyle::GridDetail => "grid_detail",
            ListStyle::GridActions => "grid_actions",
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
                ..Default::default()
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
    fn default_theme_has_no_slots() {
        let theme = Theme::default();
        assert!(theme.favicon_url.is_none());
        assert!(theme.head_html.is_none());
        assert!(theme.nav_extra_html.is_none());
        assert!(theme.footer_html.is_none());
    }

    #[test]
    fn from_entity_slot_fields() {
        let mut fields = BTreeMap::new();
        fields.insert(
            "favicon_url".to_string(),
            DynamicValue::Text("/favicon.ico".to_string()),
        );
        fields.insert(
            "head_html".to_string(),
            DynamicValue::Text(r#"<meta name="robots" content="noindex">"#.to_string()),
        );
        fields.insert(
            "nav_extra_html".to_string(),
            DynamicValue::Text(r#"<a href="/docs">Docs</a>"#.to_string()),
        );
        fields.insert(
            "footer_html".to_string(),
            DynamicValue::Text("&copy; 2024 Acme Corp".to_string()),
        );

        let theme = Theme::from_entity(&fields);
        assert_eq!(theme.favicon_url.as_deref(), Some("/favicon.ico"));
        assert_eq!(
            theme.head_html.as_deref(),
            Some(r#"<meta name="robots" content="noindex">"#)
        );
        assert_eq!(
            theme.nav_extra_html.as_deref(),
            Some(r#"<a href="/docs">Docs</a>"#)
        );
        assert_eq!(
            theme.footer_html.as_deref(),
            Some("&copy; 2024 Acme Corp")
        );
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
            heading_style: Some(HeadingStyle::WithBannerImage),
            stats_style: Some(StatsStyle::Cards),
            card_style: Some(CardStyle::Well),
            container_style: Some(ContainerStyle::Narrow),
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
    fn serde_roundtrip_heading_style() {
        let json = serde_json::to_string(&HeadingStyle::WithMetaAndActions).unwrap();
        assert_eq!(json, "\"with_meta_and_actions\"");
        let back: HeadingStyle = serde_json::from_str(&json).unwrap();
        assert_eq!(back, HeadingStyle::WithMetaAndActions);
    }

    #[test]
    fn resolve_heading_style_global() {
        let theme = Theme {
            heading_style: HeadingStyle::WithBannerImage,
            ..Theme::default()
        };
        assert_eq!(
            theme.resolve_heading_style("Contact"),
            &HeadingStyle::WithBannerImage
        );
    }

    #[test]
    fn resolve_heading_style_schema_override() {
        let mut overrides = HashMap::new();
        overrides.insert(
            "Deal".to_string(),
            ThemeOverride {
                heading_style: Some(HeadingStyle::CardWithAvatarAndStats),
                ..Default::default()
            },
        );
        let theme = Theme {
            heading_style: HeadingStyle::WithActions,
            schema_overrides: overrides,
            ..Theme::default()
        };
        assert_eq!(
            theme.resolve_heading_style("Deal"),
            &HeadingStyle::CardWithAvatarAndStats
        );
        assert_eq!(
            theme.resolve_heading_style("Contact"),
            &HeadingStyle::WithActions
        );
    }

    #[test]
    fn heading_style_name_values() {
        let theme = Theme::default();
        assert_eq!(theme.heading_style_name("Contact"), "with_actions");

        let theme = Theme {
            heading_style: HeadingStyle::WithMetaActionsAndBreadcrumbs,
            ..Theme::default()
        };
        assert_eq!(
            theme.heading_style_name("Contact"),
            "with_meta_actions_and_breadcrumbs"
        );
    }

    #[test]
    fn from_entity_heading_style() {
        let mut fields = BTreeMap::new();
        fields.insert(
            "heading_style".to_string(),
            DynamicValue::Text("with_banner_image".to_string()),
        );
        let theme = Theme::from_entity(&fields);
        assert_eq!(theme.heading_style, HeadingStyle::WithBannerImage);
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

    #[test]
    fn serde_roundtrip_stats_style() {
        let json = serde_json::to_string(&StatsStyle::WithIcons).unwrap();
        assert_eq!(json, "\"with_icons\"");
        let back: StatsStyle = serde_json::from_str(&json).unwrap();
        assert_eq!(back, StatsStyle::WithIcons);
    }

    #[test]
    fn from_entity_stats_style() {
        let mut fields = BTreeMap::new();
        fields.insert(
            "stats_style".to_string(),
            DynamicValue::Text("shared_borders".to_string()),
        );
        let theme = Theme::from_entity(&fields);
        assert_eq!(theme.stats_style, StatsStyle::SharedBorders);
    }

    #[test]
    fn resolve_stats_style_global() {
        let theme = Theme {
            stats_style: StatsStyle::Cards,
            ..Theme::default()
        };
        assert_eq!(theme.resolve_stats_style("Contact"), &StatsStyle::Cards);
    }

    #[test]
    fn resolve_stats_style_schema_override() {
        let mut overrides = HashMap::new();
        overrides.insert(
            "Deal".to_string(),
            ThemeOverride {
                stats_style: Some(StatsStyle::Trending),
                ..Default::default()
            },
        );
        let theme = Theme {
            stats_style: StatsStyle::Simple,
            schema_overrides: overrides,
            ..Theme::default()
        };
        assert_eq!(theme.resolve_stats_style("Deal"), &StatsStyle::Trending);
        assert_eq!(theme.resolve_stats_style("Contact"), &StatsStyle::Simple);
    }

    #[test]
    fn stats_style_name_values() {
        let theme = Theme::default();
        assert_eq!(theme.stats_style_name("Contact"), "simple");

        let theme = Theme {
            stats_style: StatsStyle::WithIcons,
            ..Theme::default()
        };
        assert_eq!(theme.stats_style_name("Contact"), "with_icons");
    }

    #[test]
    fn serde_roundtrip_card_style() {
        let json = serde_json::to_string(&CardStyle::WellEdgeToEdge).unwrap();
        assert_eq!(json, "\"well_edge_to_edge\"");
        let back: CardStyle = serde_json::from_str(&json).unwrap();
        assert_eq!(back, CardStyle::WellEdgeToEdge);
    }

    #[test]
    fn from_entity_card_style() {
        let mut fields = BTreeMap::new();
        fields.insert(
            "card_style".to_string(),
            DynamicValue::Text("well".to_string()),
        );
        let theme = Theme::from_entity(&fields);
        assert_eq!(theme.card_style, CardStyle::Well);
    }

    #[test]
    fn resolve_card_style_global() {
        let theme = Theme {
            card_style: CardStyle::Well,
            ..Theme::default()
        };
        assert_eq!(theme.resolve_card_style("Contact"), &CardStyle::Well);
    }

    #[test]
    fn resolve_card_style_schema_override() {
        let mut overrides = HashMap::new();
        overrides.insert(
            "Deal".to_string(),
            ThemeOverride {
                card_style: Some(CardStyle::EdgeToEdge),
                ..Default::default()
            },
        );
        let theme = Theme {
            card_style: CardStyle::Basic,
            schema_overrides: overrides,
            ..Theme::default()
        };
        assert_eq!(theme.resolve_card_style("Deal"), &CardStyle::EdgeToEdge);
        assert_eq!(theme.resolve_card_style("Contact"), &CardStyle::Basic);
    }

    #[test]
    fn card_style_name_values() {
        let theme = Theme::default();
        assert_eq!(theme.card_style_name("Contact"), "basic");

        let theme = Theme {
            card_style: CardStyle::WellEdgeToEdge,
            ..Theme::default()
        };
        assert_eq!(theme.card_style_name("Contact"), "well-edge-to-edge");
    }

    #[test]
    fn serde_roundtrip_container_style() {
        let json = serde_json::to_string(&ContainerStyle::BreakpointFullMobile).unwrap();
        assert_eq!(json, "\"breakpoint_full_mobile\"");
        let back: ContainerStyle = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ContainerStyle::BreakpointFullMobile);
    }

    #[test]
    fn from_entity_container_style() {
        let mut fields = BTreeMap::new();
        fields.insert(
            "container_style".to_string(),
            DynamicValue::Text("narrow".to_string()),
        );
        let theme = Theme::from_entity(&fields);
        assert_eq!(theme.container_style, ContainerStyle::Narrow);
    }

    #[test]
    fn resolve_container_style_global() {
        let theme = Theme {
            container_style: ContainerStyle::Breakpoint,
            ..Theme::default()
        };
        assert_eq!(
            theme.resolve_container_style("Contact"),
            &ContainerStyle::Breakpoint
        );
    }

    #[test]
    fn resolve_container_style_schema_override() {
        let mut overrides = HashMap::new();
        overrides.insert(
            "Deal".to_string(),
            ThemeOverride {
                container_style: Some(ContainerStyle::Narrow),
                ..Default::default()
            },
        );
        let theme = Theme {
            container_style: ContainerStyle::Standard,
            schema_overrides: overrides,
            ..Theme::default()
        };
        assert_eq!(
            theme.resolve_container_style("Deal"),
            &ContainerStyle::Narrow
        );
        assert_eq!(
            theme.resolve_container_style("Contact"),
            &ContainerStyle::Standard
        );
    }

    #[test]
    fn container_style_name_values() {
        let theme = Theme::default();
        assert_eq!(theme.container_style_name("Contact"), "standard");

        let theme = Theme {
            container_style: ContainerStyle::BreakpointFullMobile,
            ..Theme::default()
        };
        assert_eq!(
            theme.container_style_name("Contact"),
            "breakpoint-full-mobile"
        );
    }

    #[test]
    fn serde_roundtrip_list_style_grid() {
        let json = serde_json::to_string(&ListStyle::GridBadge).unwrap();
        assert_eq!(json, "\"grid_badge\"");
        let back: ListStyle = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ListStyle::GridBadge);
    }

    #[test]
    fn list_style_name_grid_variants() {
        let cases = [
            (ListStyle::GridBadge, "grid_badge"),
            (ListStyle::GridProfile, "grid_profile"),
            (ListStyle::GridDirectory, "grid_directory"),
            (ListStyle::GridLink, "grid_link"),
            (ListStyle::GridGallery, "grid_gallery"),
            (ListStyle::GridDetail, "grid_detail"),
            (ListStyle::GridActions, "grid_actions"),
        ];
        for (style, expected) in cases {
            let theme = Theme {
                list_style: style,
                ..Theme::default()
            };
            assert_eq!(theme.list_style_name("Contact"), expected);
        }
    }

    #[test]
    fn from_entity_grid_list_style() {
        let mut fields = BTreeMap::new();
        fields.insert(
            "list_style".to_string(),
            DynamicValue::Text("grid_detail".to_string()),
        );
        let theme = Theme::from_entity(&fields);
        assert_eq!(theme.list_style, ListStyle::GridDetail);
    }

    #[test]
    fn resolve_list_style_grid_override() {
        let mut overrides = HashMap::new();
        overrides.insert(
            "Contact".to_string(),
            ThemeOverride {
                list_style: Some(ListStyle::GridProfile),
                ..Default::default()
            },
        );
        let theme = Theme {
            list_style: ListStyle::Table,
            schema_overrides: overrides,
            ..Theme::default()
        };
        assert_eq!(theme.resolve_list_style("Contact"), &ListStyle::GridProfile);
        assert_eq!(theme.resolve_list_style("Company"), &ListStyle::Table);
    }
}
