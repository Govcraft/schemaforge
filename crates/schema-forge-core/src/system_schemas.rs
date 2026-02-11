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
    ]
}
