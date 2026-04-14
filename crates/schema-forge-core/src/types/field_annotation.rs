use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Closed vocabulary of UI widget hints accepted by `@widget("...")`.
///
/// The JSON / DSL representation is the `snake_case` form of each variant,
/// as enforced by `#[serde(rename_all = "snake_case")]` and the `Display` /
/// [`WidgetType::as_str`] implementations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum WidgetType {
    StatusBadge,
    CountBadge,
    Progress,
    Markdown,
    RichText,
    Color,
    File,
    Image,
    Avatar,
    Slider,
    Rating,
    Code,
    Phone,
    Tags,
    Email,
    Url,
    Json,
}

impl WidgetType {
    /// Every widget variant in declaration order.
    pub const VARIANTS: &'static [Self] = &[
        Self::StatusBadge,
        Self::CountBadge,
        Self::Progress,
        Self::Markdown,
        Self::RichText,
        Self::Color,
        Self::File,
        Self::Image,
        Self::Avatar,
        Self::Slider,
        Self::Rating,
        Self::Code,
        Self::Phone,
        Self::Tags,
        Self::Email,
        Self::Url,
        Self::Json,
    ];

    /// Returns the canonical `snake_case` token for this widget type.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StatusBadge => "status_badge",
            Self::CountBadge => "count_badge",
            Self::Progress => "progress",
            Self::Markdown => "markdown",
            Self::RichText => "rich_text",
            Self::Color => "color",
            Self::File => "file",
            Self::Image => "image",
            Self::Avatar => "avatar",
            Self::Slider => "slider",
            Self::Rating => "rating",
            Self::Code => "code",
            Self::Phone => "phone",
            Self::Tags => "tags",
            Self::Email => "email",
            Self::Url => "url",
            Self::Json => "json",
        }
    }
}

impl fmt::Display for WidgetType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned when [`WidgetType::from_str`] cannot match the input to a
/// known variant. Carries the unknown value and the full list of valid
/// widget tokens for "did you mean?" style error rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownWidgetType {
    /// The value supplied by the caller (trimmed of enclosing quotes).
    pub value: String,
    /// All valid widget tokens in canonical order.
    pub valid: &'static [&'static str],
}

impl UnknownWidgetType {
    /// Constructs a new error for the given unknown value.
    pub fn new(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            valid: VALID_WIDGET_TYPES,
        }
    }
}

impl fmt::Display for UnknownWidgetType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown widget type '{}'; valid widget types: {}",
            self.value,
            self.valid.join(", "),
        )
    }
}

impl std::error::Error for UnknownWidgetType {}

impl FromStr for WidgetType {
    type Err = UnknownWidgetType;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "status_badge" => Ok(Self::StatusBadge),
            "count_badge" => Ok(Self::CountBadge),
            "progress" => Ok(Self::Progress),
            "markdown" => Ok(Self::Markdown),
            "rich_text" => Ok(Self::RichText),
            "color" => Ok(Self::Color),
            "file" => Ok(Self::File),
            "image" => Ok(Self::Image),
            "avatar" => Ok(Self::Avatar),
            "slider" => Ok(Self::Slider),
            "rating" => Ok(Self::Rating),
            "code" => Ok(Self::Code),
            "phone" => Ok(Self::Phone),
            "tags" => Ok(Self::Tags),
            "email" => Ok(Self::Email),
            "url" => Ok(Self::Url),
            "json" => Ok(Self::Json),
            other => Err(UnknownWidgetType::new(other)),
        }
    }
}

/// Canonical widget token list for error reporting.
const VALID_WIDGET_TYPES: &[&str] = &[
    "status_badge",
    "count_badge",
    "progress",
    "markdown",
    "rich_text",
    "color",
    "file",
    "image",
    "avatar",
    "slider",
    "rating",
    "code",
    "phone",
    "tags",
    "email",
    "url",
    "json",
];

/// Closed vocabulary of display format hints accepted by `@format("...")`.
///
/// The JSON / DSL representation is the `snake_case` form of each variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FormatType {
    Currency,
    Percent,
    Date,
    Datetime,
    Relative,
    Bytes,
    Duration,
}

impl FormatType {
    /// Every format variant in declaration order.
    pub const VARIANTS: &'static [Self] = &[
        Self::Currency,
        Self::Percent,
        Self::Date,
        Self::Datetime,
        Self::Relative,
        Self::Bytes,
        Self::Duration,
    ];

    /// Returns the canonical `snake_case` token for this format type.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Currency => "currency",
            Self::Percent => "percent",
            Self::Date => "date",
            Self::Datetime => "datetime",
            Self::Relative => "relative",
            Self::Bytes => "bytes",
            Self::Duration => "duration",
        }
    }
}

impl fmt::Display for FormatType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned when [`FormatType::from_str`] cannot match the input to a
/// known variant. Carries the unknown value and the full list of valid
/// format tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownFormatType {
    /// The value supplied by the caller.
    pub value: String,
    /// All valid format tokens in canonical order.
    pub valid: &'static [&'static str],
}

impl UnknownFormatType {
    /// Constructs a new error for the given unknown value.
    pub fn new(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            valid: VALID_FORMAT_TYPES,
        }
    }
}

impl fmt::Display for UnknownFormatType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown format type '{}'; valid format types: {}",
            self.value,
            self.valid.join(", "),
        )
    }
}

impl std::error::Error for UnknownFormatType {}

impl FromStr for FormatType {
    type Err = UnknownFormatType;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "currency" => Ok(Self::Currency),
            "percent" => Ok(Self::Percent),
            "date" => Ok(Self::Date),
            "datetime" => Ok(Self::Datetime),
            "relative" => Ok(Self::Relative),
            "bytes" => Ok(Self::Bytes),
            "duration" => Ok(Self::Duration),
            other => Err(UnknownFormatType::new(other)),
        }
    }
}

/// Canonical format token list for error reporting.
const VALID_FORMAT_TYPES: &[&str] = &[
    "currency", "percent", "date", "datetime", "relative", "bytes", "duration",
];

/// Closed vocabulary of semantic color tokens accepted by `@enum_colors(...)`.
///
/// Each variant maps to a stable CSS token emitted by the site generator and
/// wired to Tailwind / shadcn palette classes. The names are intentionally
/// semantic ("amber" for in-progress, "green" for success, "red" for failure)
/// so that schema authors can communicate intent rather than specific hues.
///
/// The JSON / DSL representation is the `snake_case` form, enforced by
/// `#[serde(rename_all = "snake_case")]` and
/// [`EnumColor::as_str`] / [`EnumColor::from_str`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum EnumColor {
    Neutral,
    Gray,
    Red,
    Amber,
    Green,
    Blue,
    Purple,
    Violet,
    Teal,
    Rose,
}

impl EnumColor {
    /// Every color variant in declaration order.
    pub const VARIANTS: &'static [Self] = &[
        Self::Neutral,
        Self::Gray,
        Self::Red,
        Self::Amber,
        Self::Green,
        Self::Blue,
        Self::Purple,
        Self::Violet,
        Self::Teal,
        Self::Rose,
    ];

    /// Returns the canonical `snake_case` token for this color.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Neutral => "neutral",
            Self::Gray => "gray",
            Self::Red => "red",
            Self::Amber => "amber",
            Self::Green => "green",
            Self::Blue => "blue",
            Self::Purple => "purple",
            Self::Violet => "violet",
            Self::Teal => "teal",
            Self::Rose => "rose",
        }
    }
}

impl fmt::Display for EnumColor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned when [`EnumColor::from_str`] cannot match the input to a
/// known variant. Carries the unknown value and the full list of valid
/// color tokens for "did you mean?" style error rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownEnumColor {
    /// The value supplied by the caller (trimmed of enclosing quotes).
    pub value: String,
    /// All valid color tokens in canonical order.
    pub valid: &'static [&'static str],
}

impl UnknownEnumColor {
    /// Constructs a new error for the given unknown value.
    pub fn new(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            valid: VALID_ENUM_COLORS,
        }
    }
}

impl fmt::Display for UnknownEnumColor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown enum color '{}'; valid colors: {}",
            self.value,
            self.valid.join(", "),
        )
    }
}

impl std::error::Error for UnknownEnumColor {}

impl FromStr for EnumColor {
    type Err = UnknownEnumColor;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "neutral" => Ok(Self::Neutral),
            "gray" => Ok(Self::Gray),
            "red" => Ok(Self::Red),
            "amber" => Ok(Self::Amber),
            "green" => Ok(Self::Green),
            "blue" => Ok(Self::Blue),
            "purple" => Ok(Self::Purple),
            "violet" => Ok(Self::Violet),
            "teal" => Ok(Self::Teal),
            "rose" => Ok(Self::Rose),
            other => Err(UnknownEnumColor::new(other)),
        }
    }
}

/// Canonical color token list for error reporting.
const VALID_ENUM_COLORS: &[&str] = &[
    "neutral", "gray", "red", "amber", "green", "blue", "purple", "violet", "teal", "rose",
];

/// Closed set of hints accepted by `@list(hint)` — controls whether the
/// field appears in the generated list view and how it is rendered.
///
/// The JSON / DSL representation is the `snake_case` form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ListHint {
    /// The headline cell of the row — rendered with display styling.
    /// At most one `Primary` per schema; the `@display("...")` field
    /// auto-promotes to `Primary` if no explicit hint is declared.
    Primary,
    /// Show the field in the default list view. Ordering follows schema
    /// declaration order.
    Column,
    /// Never show the field in a list view, even if it would otherwise
    /// be included by default.
    Hidden,
}

impl ListHint {
    /// Every list-hint variant in declaration order.
    pub const VARIANTS: &'static [Self] = &[Self::Primary, Self::Column, Self::Hidden];

    /// Returns the canonical `snake_case` token for this hint.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Column => "column",
            Self::Hidden => "hidden",
        }
    }
}

impl fmt::Display for ListHint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned when [`ListHint::from_str`] cannot match the input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownListHint {
    /// The token supplied by the caller.
    pub value: String,
    /// All valid list-hint tokens in canonical order.
    pub valid: &'static [&'static str],
}

impl UnknownListHint {
    /// Constructs a new error for the given unknown value.
    pub fn new(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            valid: VALID_LIST_HINTS,
        }
    }
}

impl fmt::Display for UnknownListHint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown list hint '{}'; valid hints: {}",
            self.value,
            self.valid.join(", "),
        )
    }
}

impl std::error::Error for UnknownListHint {}

impl FromStr for ListHint {
    type Err = UnknownListHint;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "primary" => Ok(Self::Primary),
            "column" => Ok(Self::Column),
            "hidden" => Ok(Self::Hidden),
            other => Err(UnknownListHint::new(other)),
        }
    }
}

/// Canonical list-hint token list for error reporting.
const VALID_LIST_HINTS: &[&str] = &["primary", "column", "hidden"];

/// Annotations that can be applied to individual fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "annotation")]
#[non_exhaustive]
pub enum FieldAnnotation {
    /// `@field_access(...)` -- role-based access control on a specific field.
    FieldAccess {
        read: Vec<String>,
        write: Vec<String>,
    },
    /// `@owner` -- marks this field as the ownership field for the record.
    Owner,
    /// `@widget("widget_type")` -- UI widget hint for rendering this field.
    Widget { widget_type: WidgetType },
    /// `@kanban_column` -- marks this field as the grouping column for kanban views.
    KanbanColumn,
    /// `@format("format_type")` -- display format hint for field values.
    Format { format_type: FormatType },
    /// `@enum_colors(variant: "color", ...)` -- semantic color tokens for
    /// specific enum variants. Keyed by the variant name; the generator is
    /// responsible for ensuring every key names a valid variant of the
    /// enum field the annotation is attached to. Missing keys render with
    /// the default neutral badge.
    EnumColors {
        colors: BTreeMap<String, EnumColor>,
    },
    /// `@list(primary|column|hidden)` -- controls whether the field
    /// appears in the generated list view and, for `primary`, marks it
    /// as the headline cell rendered with display styling.
    List { hint: ListHint },
}

impl FieldAnnotation {
    /// Returns the annotation kind as a string, for dedup checking.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::FieldAccess { .. } => "field_access",
            Self::Owner => "owner",
            Self::Widget { .. } => "widget",
            Self::KanbanColumn => "kanban_column",
            Self::Format { .. } => "format",
            Self::EnumColors { .. } => "enum_colors",
            Self::List { .. } => "list",
        }
    }
}

impl fmt::Display for FieldAnnotation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FieldAccess { read, write } => {
                write!(
                    f,
                    "@field_access(read=[{}], write=[{}])",
                    format_role_list(read),
                    format_role_list(write),
                )
            }
            Self::Owner => write!(f, "@owner"),
            Self::Widget { widget_type } => write!(f, "@widget(\"{widget_type}\")"),
            Self::KanbanColumn => write!(f, "@kanban_column"),
            Self::Format { format_type } => write!(f, "@format(\"{format_type}\")"),
            Self::EnumColors { colors } => {
                let parts: Vec<String> = colors
                    .iter()
                    .map(|(k, v)| format!("{k}: \"{v}\""))
                    .collect();
                write!(f, "@enum_colors({})", parts.join(", "))
            }
            Self::List { hint } => write!(f, "@list({hint})"),
        }
    }
}

/// A single repair performed by [`sanitize_schema_metadata_json`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WidgetRepair {
    /// A legacy widget token was rewritten to its modern equivalent
    /// (e.g. `"link"` → `"url"`).
    Remapped { from: String, to: String },
    /// A widget annotation using a removed/unknown token was stripped
    /// from the annotation list. The field retains the rest of its
    /// annotations.
    Dropped { token: String },
}

/// Walk a JSON-serialized [`SchemaDefinition`] and migrate any legacy
/// `@widget("...")` annotations that would otherwise fail strict
/// deserialization.
///
/// SchemaForge stores full [`SchemaDefinition`]s as JSONB in the
/// `_schema_metadata` table. When a widget variant is removed from the core
/// enum, the live DB can still carry annotations serialized under the old
/// vocabulary, and `serde_json::from_value::<SchemaDefinition>` will hard-fail
/// on startup. This helper preprocesses the JSON value before that call so
/// the server can recover gracefully:
///
/// 1. Known-valid widget tokens are left untouched.
/// 2. The legacy alias `"link"` is remapped to `"url"`.
/// 3. Any other unrecognized token causes the whole `@widget` annotation to
///    be dropped from the field's annotation list.
///
/// Returns the list of repairs performed so the caller can log a warning and
/// point the operator at the stale rows.
pub fn sanitize_schema_metadata_json(value: &mut serde_json::Value) -> Vec<WidgetRepair> {
    let mut repairs = Vec::new();
    sanitize_walk(value, &mut repairs);
    repairs
}

fn sanitize_walk(value: &mut serde_json::Value, repairs: &mut Vec<WidgetRepair>) {
    match value {
        serde_json::Value::Array(items) => {
            items.retain_mut(|item| {
                if let Some(repair) = inspect_widget_object(item) {
                    match repair {
                        WidgetRepair::Dropped { .. } => {
                            repairs.push(repair);
                            return false;
                        }
                        WidgetRepair::Remapped { .. } => {
                            repairs.push(repair);
                        }
                    }
                }
                sanitize_walk(item, repairs);
                true
            });
        }
        serde_json::Value::Object(map) => {
            for (_, v) in map.iter_mut() {
                sanitize_walk(v, repairs);
            }
        }
        _ => {}
    }
}

/// If `item` is a `{"annotation":"Widget","widget_type":"..."}` object whose
/// `widget_type` is unknown or legacy, mutate it (or flag it for drop) and
/// return the repair performed. Returns `None` for any other value.
fn inspect_widget_object(item: &mut serde_json::Value) -> Option<WidgetRepair> {
    let obj = item.as_object_mut()?;
    if obj.get("annotation").and_then(|v| v.as_str()) != Some("Widget") {
        return None;
    }
    let token = obj.get("widget_type")?.as_str()?.to_string();
    if WidgetType::from_str(&token).is_ok() {
        return None;
    }
    if token == "link" {
        obj.insert(
            "widget_type".to_string(),
            serde_json::Value::String("url".to_string()),
        );
        return Some(WidgetRepair::Remapped {
            from: token,
            to: "url".to_string(),
        });
    }
    Some(WidgetRepair::Dropped { token })
}

/// Formats a list of role strings as a comma-separated, quoted list.
fn format_role_list(roles: &[String]) -> String {
    roles
        .iter()
        .map(|r| format!("\"{r}\""))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- FieldAnnotation --

    #[test]
    fn display_field_access() {
        let a = FieldAnnotation::FieldAccess {
            read: vec!["admin".into(), "viewer".into()],
            write: vec!["admin".into()],
        };
        assert_eq!(
            a.to_string(),
            "@field_access(read=[\"admin\", \"viewer\"], write=[\"admin\"])"
        );
    }

    #[test]
    fn display_owner() {
        let a = FieldAnnotation::Owner;
        assert_eq!(a.to_string(), "@owner");
    }

    #[test]
    fn kind_returns_correct_strings() {
        assert_eq!(
            FieldAnnotation::FieldAccess {
                read: vec![],
                write: vec![],
            }
            .kind(),
            "field_access"
        );
        assert_eq!(FieldAnnotation::Owner.kind(), "owner");
    }

    #[test]
    fn serde_roundtrip_field_access() {
        let a = FieldAnnotation::FieldAccess {
            read: vec!["admin".into(), "viewer".into()],
            write: vec!["admin".into()],
        };
        let json = serde_json::to_string(&a).unwrap();
        let back: FieldAnnotation = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
    }

    #[test]
    fn serde_roundtrip_owner() {
        let a = FieldAnnotation::Owner;
        let json = serde_json::to_string(&a).unwrap();
        let back: FieldAnnotation = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
    }

    #[test]
    fn display_widget() {
        let a = FieldAnnotation::Widget {
            widget_type: WidgetType::StatusBadge,
        };
        assert_eq!(a.to_string(), "@widget(\"status_badge\")");
    }

    #[test]
    fn display_kanban_column() {
        let a = FieldAnnotation::KanbanColumn;
        assert_eq!(a.to_string(), "@kanban_column");
    }

    #[test]
    fn kind_widget() {
        assert_eq!(
            FieldAnnotation::Widget {
                widget_type: WidgetType::Progress,
            }
            .kind(),
            "widget"
        );
    }

    #[test]
    fn kind_kanban_column() {
        assert_eq!(FieldAnnotation::KanbanColumn.kind(), "kanban_column");
    }

    #[test]
    fn serde_roundtrip_widget() {
        let a = FieldAnnotation::Widget {
            widget_type: WidgetType::StatusBadge,
        };
        let json = serde_json::to_string(&a).unwrap();
        let back: FieldAnnotation = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
    }

    #[test]
    fn sanitize_strips_removed_widget_tokens() {
        let mut json: serde_json::Value = serde_json::json!({
            "name": "Opportunity",
            "fields": [
                {
                    "name": "amount",
                    "annotations": [
                        {"annotation": "Widget", "widget_type": "currency"},
                        {"annotation": "Owner"}
                    ]
                },
                {
                    "name": "site",
                    "annotations": [
                        {"annotation": "Widget", "widget_type": "link"}
                    ]
                }
            ]
        });
        let repairs = sanitize_schema_metadata_json(&mut json);
        assert_eq!(repairs.len(), 2);
        assert!(repairs.contains(&WidgetRepair::Dropped {
            token: "currency".to_string()
        }));
        assert!(repairs.contains(&WidgetRepair::Remapped {
            from: "link".to_string(),
            to: "url".to_string()
        }));

        // The `amount` field should retain `@owner` but lose `@widget("currency")`.
        let amount_annotations = json["fields"][0]["annotations"].as_array().unwrap();
        assert_eq!(amount_annotations.len(), 1);
        assert_eq!(amount_annotations[0]["annotation"], "Owner");

        // The `site` field's `@widget("link")` should now be `"url"`.
        let site_widget = &json["fields"][1]["annotations"][0];
        assert_eq!(site_widget["widget_type"], "url");
    }

    #[test]
    fn sanitize_is_noop_for_valid_metadata() {
        let mut json = serde_json::json!({
            "fields": [{
                "name": "status",
                "annotations": [
                    {"annotation": "Widget", "widget_type": "status_badge"}
                ]
            }]
        });
        let original = json.clone();
        let repairs = sanitize_schema_metadata_json(&mut json);
        assert!(repairs.is_empty());
        assert_eq!(json, original);
    }

    #[test]
    fn serde_widget_json_shape_matches_snake_case() {
        // Old JSON written before the enum change used the same snake_case
        // string literals as the DSL tokens, so the new enum must still
        // deserialize them verbatim.
        let json = r#"{"annotation":"Widget","widget_type":"status_badge"}"#;
        let back: FieldAnnotation = serde_json::from_str(json).unwrap();
        assert_eq!(
            back,
            FieldAnnotation::Widget {
                widget_type: WidgetType::StatusBadge,
            }
        );
    }

    #[test]
    fn serde_roundtrip_kanban_column() {
        let a = FieldAnnotation::KanbanColumn;
        let json = serde_json::to_string(&a).unwrap();
        let back: FieldAnnotation = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
    }

    #[test]
    fn display_format() {
        let a = FieldAnnotation::Format {
            format_type: FormatType::Currency,
        };
        assert_eq!(a.to_string(), "@format(\"currency\")");
    }

    #[test]
    fn kind_format() {
        assert_eq!(
            FieldAnnotation::Format {
                format_type: FormatType::Percent,
            }
            .kind(),
            "format"
        );
    }

    #[test]
    fn serde_roundtrip_format() {
        let a = FieldAnnotation::Format {
            format_type: FormatType::Currency,
        };
        let json = serde_json::to_string(&a).unwrap();
        let back: FieldAnnotation = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
    }

    #[test]
    fn serde_format_json_shape_matches_snake_case() {
        let json = r#"{"annotation":"Format","format_type":"percent"}"#;
        let back: FieldAnnotation = serde_json::from_str(json).unwrap();
        assert_eq!(
            back,
            FieldAnnotation::Format {
                format_type: FormatType::Percent,
            }
        );
    }

    #[test]
    fn serde_field_access_empty_vecs() {
        let a = FieldAnnotation::FieldAccess {
            read: vec![],
            write: vec![],
        };
        let json = serde_json::to_string(&a).unwrap();
        let back: FieldAnnotation = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
    }

    // -- WidgetType --

    #[test]
    fn widget_type_from_str_valid_all_variants() {
        for variant in WidgetType::VARIANTS {
            let token = variant.as_str();
            let parsed = WidgetType::from_str(token).expect("variant token must parse");
            assert_eq!(parsed, *variant);
        }
    }

    #[test]
    fn widget_type_from_str_rejects_unknown() {
        let err = WidgetType::from_str("fancy_picker").unwrap_err();
        assert_eq!(err.value, "fancy_picker");
        assert!(!err.valid.is_empty());
        let msg = err.to_string();
        assert!(msg.contains("fancy_picker"));
        assert!(msg.contains("status_badge"));
    }

    #[test]
    fn widget_type_from_str_rejects_removed_legacy_tokens() {
        // Legacy tokens that existed in the old ad-hoc allow list but are
        // intentionally NOT part of the canonical vocabulary.
        for legacy in ["relative_time", "link", "currency"] {
            assert!(WidgetType::from_str(legacy).is_err(), "{legacy}");
        }
    }

    #[test]
    fn widget_type_display_matches_as_str() {
        for variant in WidgetType::VARIANTS {
            assert_eq!(variant.to_string(), variant.as_str());
        }
    }

    #[test]
    fn widget_type_display_roundtrip_via_from_str() {
        for variant in WidgetType::VARIANTS {
            let parsed = WidgetType::from_str(&variant.to_string()).unwrap();
            assert_eq!(parsed, *variant);
        }
    }

    #[test]
    fn widget_type_serde_roundtrip_all_variants() {
        for variant in WidgetType::VARIANTS {
            let json = serde_json::to_string(variant).unwrap();
            let back: WidgetType = serde_json::from_str(&json).unwrap();
            assert_eq!(back, *variant);
        }
    }

    #[test]
    fn widget_type_serde_serializes_as_snake_case_bare_string() {
        let json = serde_json::to_string(&WidgetType::StatusBadge).unwrap();
        assert_eq!(json, "\"status_badge\"");
    }

    #[test]
    fn widget_type_variants_count() {
        assert_eq!(WidgetType::VARIANTS.len(), 17);
    }

    #[test]
    fn unknown_widget_type_error_lists_all_valid() {
        let err = UnknownWidgetType::new("bogus");
        assert_eq!(err.valid.len(), 17);
        let msg = err.to_string();
        for valid in WidgetType::VARIANTS {
            assert!(
                msg.contains(valid.as_str()),
                "error message missing {}",
                valid.as_str()
            );
        }
    }

    // -- FormatType --

    #[test]
    fn format_type_from_str_valid_all_variants() {
        for variant in FormatType::VARIANTS {
            let token = variant.as_str();
            let parsed = FormatType::from_str(token).expect("variant token must parse");
            assert_eq!(parsed, *variant);
        }
    }

    #[test]
    fn format_type_from_str_rejects_unknown() {
        let err = FormatType::from_str("bogus").unwrap_err();
        assert_eq!(err.value, "bogus");
        let msg = err.to_string();
        assert!(msg.contains("bogus"));
        assert!(msg.contains("currency"));
    }

    #[test]
    fn format_type_from_str_rejects_colon_suffix() {
        // Colon-suffix support (e.g. `currency:$`) has been removed.
        assert!(FormatType::from_str("currency:$").is_err());
        assert!(FormatType::from_str("currency:€").is_err());
    }

    #[test]
    fn format_type_display_matches_as_str() {
        for variant in FormatType::VARIANTS {
            assert_eq!(variant.to_string(), variant.as_str());
        }
    }

    #[test]
    fn format_type_display_roundtrip_via_from_str() {
        for variant in FormatType::VARIANTS {
            let parsed = FormatType::from_str(&variant.to_string()).unwrap();
            assert_eq!(parsed, *variant);
        }
    }

    #[test]
    fn format_type_serde_roundtrip_all_variants() {
        for variant in FormatType::VARIANTS {
            let json = serde_json::to_string(variant).unwrap();
            let back: FormatType = serde_json::from_str(&json).unwrap();
            assert_eq!(back, *variant);
        }
    }

    #[test]
    fn format_type_variants_count() {
        assert_eq!(FormatType::VARIANTS.len(), 7);
    }

    #[test]
    fn unknown_format_type_error_lists_all_valid() {
        let err = UnknownFormatType::new("xyz");
        assert_eq!(err.valid.len(), 7);
        let msg = err.to_string();
        for valid in FormatType::VARIANTS {
            assert!(
                msg.contains(valid.as_str()),
                "error message missing {}",
                valid.as_str()
            );
        }
    }

    // -- EnumColor --

    #[test]
    fn enum_color_from_str_valid_all_variants() {
        for variant in EnumColor::VARIANTS {
            assert_eq!(EnumColor::from_str(variant.as_str()).unwrap(), *variant);
        }
    }

    #[test]
    fn enum_color_from_str_rejects_unknown() {
        let err = EnumColor::from_str("magenta").unwrap_err();
        assert_eq!(err.value, "magenta");
        let msg = err.to_string();
        assert!(msg.contains("magenta"));
        assert!(msg.contains("green"));
    }

    #[test]
    fn enum_color_variants_count() {
        assert_eq!(EnumColor::VARIANTS.len(), 10);
    }

    #[test]
    fn enum_color_serde_is_snake_case() {
        let json = serde_json::to_string(&EnumColor::Amber).unwrap();
        assert_eq!(json, "\"amber\"");
    }

    #[test]
    fn display_enum_colors_annotation() {
        let mut colors = BTreeMap::new();
        colors.insert("awarded".to_string(), EnumColor::Green);
        colors.insert("lost".to_string(), EnumColor::Red);
        let a = FieldAnnotation::EnumColors { colors };
        // BTreeMap gives stable alphabetical key ordering.
        assert_eq!(
            a.to_string(),
            "@enum_colors(awarded: \"green\", lost: \"red\")"
        );
    }

    #[test]
    fn kind_enum_colors() {
        let a = FieldAnnotation::EnumColors {
            colors: BTreeMap::new(),
        };
        assert_eq!(a.kind(), "enum_colors");
    }

    #[test]
    fn serde_roundtrip_enum_colors() {
        let mut colors = BTreeMap::new();
        colors.insert("pending".to_string(), EnumColor::Amber);
        colors.insert("done".to_string(), EnumColor::Green);
        let a = FieldAnnotation::EnumColors { colors };
        let json = serde_json::to_string(&a).unwrap();
        let back: FieldAnnotation = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
    }
}
