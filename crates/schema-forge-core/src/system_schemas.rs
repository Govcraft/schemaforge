//! Built-in system schemas seeded into every SchemaForge deployment.
//!
//! Authorization is owned end-to-end by the Cedar policy engine. SchemaForge
//! does not store roles or permissions as database rows — those are policy
//! artifacts. The schemas here are limited to durable account and
//! infrastructure data that the platform itself needs to operate.
//!
//! - [`USER_SCHEMA`] — user accounts and the role membership / rank fields
//!   the Cedar adapter reads when building principal entities.
//! - [`TENANT_MEMBERSHIP_SCHEMA`] — durable record of which user belongs to
//!   which tenant, together with the role string that scopes their access
//!   inside that tenant.
//! - [`WEBHOOK_SUBSCRIPTION_SCHEMA`] — subscriber registrations consumed by
//!   the webhook dispatcher.

/// DSL text for the system User schema.
///
/// `roles` carries plain role-name strings; the role-name → rank mapping
/// lives in `policies/role_ranks.toml`. `role_rank` is computed server-side
/// on every user mutation as the maximum rank of any role in `roles`, and
/// is the attribute Cedar policies inspect to enforce the no-upward
/// visibility / creation rule for user management.
pub const USER_SCHEMA: &str = r#"
@system @display("email")
schema User {
    email:          text(max: 512) required indexed
    display_name:   text(max: 255) required
    roles:          text[]
    role_rank:      integer required
    active:         boolean default(true)
    password_hash:  text(max: 512) @hidden
    last_login:     datetime
    metadata:       json
}
"#;

/// DSL text for the system TenantMembership schema.
///
/// `role` is a plain role-name string. The role-name → rank mapping lives
/// in `policies/role_ranks.toml`; Cedar policies enforce the membership
/// semantics via the `@tenant` annotation contract.
pub const TENANT_MEMBERSHIP_SCHEMA: &str = r#"
@system
schema TenantMembership {
    user:         -> User required
    tenant_type:  text(max: 128) required
    tenant_id:    text(max: 255) required
    role:         text(max: 128)
}
"#;

/// DSL text for the system WebhookSubscription schema.
pub const WEBHOOK_SUBSCRIPTION_SCHEMA: &str = r#"
@system @display("name")
schema WebhookSubscription {
    name:            text(max: 255) required indexed
    target_schema:   text(max: 128) required indexed
    url:             text(max: 2048) required
    secret:          text(max: 512)
    events:          text[]
    active:          boolean default(true)
    retry_count:     integer(min: 0, max: 10) default(3)
    timeout_seconds: integer(min: 1, max: 30) default(10)
    created_by:      text(max: 255)
}
"#;

/// Returns all system schema DSL texts in dependency order.
///
/// `User` has no dependencies. `TenantMembership` depends on `User`.
/// `WebhookSubscription` is independent.
pub fn all_system_schemas() -> Vec<&'static str> {
    vec![
        USER_SCHEMA,
        TENANT_MEMBERSHIP_SCHEMA,
        WEBHOOK_SUBSCRIPTION_SCHEMA,
    ]
}
