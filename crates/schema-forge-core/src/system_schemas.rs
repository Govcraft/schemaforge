/// DSL text for the system User schema.
pub const USER_SCHEMA: &str = r#"
@system @display("email")
schema User {
    email:          text(max: 512) required indexed
    display_name:   text(max: 255) required
    roles:          -> Role[]
    active:         boolean default(true)
    last_login:     datetime
    metadata:       json
}
"#;

/// DSL text for the system Role schema.
pub const ROLE_SCHEMA: &str = r#"
@system @display("name")
schema Role {
    name:        text(max: 128) required indexed
    description: text(max: 512)
    permissions: -> Permission[]
}
"#;

/// DSL text for the system Permission schema.
pub const PERMISSION_SCHEMA: &str = r#"
@system @display("name")
schema Permission {
    name:        text(max: 128) required indexed
    description: text(max: 512)
    resource:    text(max: 255) required
    action:      text(max: 64) required
}
"#;

/// DSL text for the system TenantMembership schema.
pub const TENANT_MEMBERSHIP_SCHEMA: &str = r#"
@system
schema TenantMembership {
    user:         -> User required
    tenant_type:  text(max: 128) required
    tenant_id:    text(max: 255) required
    role:         -> Role
}
"#;

/// DSL text for the system Theme schema.
pub const THEME_SCHEMA: &str = r##"
@system @display("name")
schema Theme {
    name:              text(max: 128) required indexed
    primary_color:     text(max: 32) default("#3B82F6")
    secondary_color:   text(max: 32) default("#6B7280")
    accent_color:      text(max: 32) default("#10B981")
    error_color:       text(max: 32) default("#EF4444")
    background_color:  text(max: 32) default("#111827")
    surface_color:     text(max: 32) default("#1F2937")
    text_color:        text(max: 32) default("#F1F5F9")
    border_radius:     text(max: 16) default("0.5rem")
    font_family:       text(max: 256) default("system-ui, sans-serif")
    list_style:        enum("table", "cards", "compact", "grid_badge", "grid_profile", "grid_directory", "grid_link", "grid_gallery", "grid_detail", "grid_actions") default("table")
    detail_style:      enum("full", "split", "tabbed") default("full")
    nav_style:         enum("sidebar", "topnav", "minimal") default("sidebar")
    density:           enum("compact", "comfortable", "spacious") default("comfortable")
    heading_style:     enum("with_actions", "with_actions_and_breadcrumbs", "card_with_avatar_and_stats", "with_avatar_and_actions", "with_banner_image", "with_filters_and_actions", "with_logo_meta_and_actions", "with_meta_actions_and_breadcrumbs", "with_meta_and_actions") default("with_actions")
    stats_style:       enum("simple", "cards", "with_icons", "shared_borders", "trending", "grid_actions", "grid_badge") default("simple")
    card_style:        enum("basic", "well", "edge_to_edge", "well_edge_to_edge") default("basic")
    container_style:   enum("standard", "full_mobile", "breakpoint", "breakpoint_full_mobile", "narrow") default("standard")
    schema_labels:     json
    field_labels:      json
    schema_overrides:  json
    view_overrides:    json
    dashboard_schemas: text[]
    logo_url:          text
    favicon_url:       text
    head_html:         richtext
    nav_extra_html:    richtext
    footer_html:       richtext
    custom_css:        richtext
    active:            boolean default(true)
}
"##;

/// Returns all system schema DSL texts in dependency order.
///
/// Permission is first (no dependencies), then Role (depends on Permission),
/// then User (depends on Role), and finally TenantMembership (depends on User and Role).
pub fn all_system_schemas() -> Vec<&'static str> {
    vec![
        PERMISSION_SCHEMA,
        ROLE_SCHEMA,
        USER_SCHEMA,
        TENANT_MEMBERSHIP_SCHEMA,
        THEME_SCHEMA,
    ]
}
