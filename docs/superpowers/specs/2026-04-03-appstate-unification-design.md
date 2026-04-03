# AppState Unification via Actor Extensions

**Date:** 2026-04-03
**Status:** Draft
**Scope:** schema-forge-acton, schema-forge-core, schema-forge-dsl, schema-forge-cli

## Problem

SchemaForge maintains a separate `ForgeState` struct that is used as the Axum router state for all forge HTTP handlers. This state is completely disconnected from acton-service's `AppState<T>`, which means handlers cannot access framework-provided services like the audit logger, key manager, account service, or broker.

The `register_versioned_routes` method works around this by calling `nest_service()`, which replaces `AppState<T>` with `ForgeState`, severing access to all framework services. This makes it impossible to add audit logging, metrics, or any other acton-service feature to SchemaForge handlers without bolting them on through a separate channel.

## Solution

Replace `ForgeState` with a `ForgeActor` — an acton-reactive actor extension registered via `ServiceBuilder::with_actor::<ForgeActor>()`. All forge handlers move from `State<ForgeState>` to `State<AppState<SchemaForgeConfig>>`, giving them access to both forge-specific state (via `state.actor::<ForgeActor>()`) and all framework services (audit, tracing, broker, etc.).

This requires upgrading acton-service from 0.21.4 to 0.22.0, which introduces the `ActorExtension` trait and `with_actor()` on `ServiceBuilder`.

## ForgeActor

### State

The actor owns all runtime state currently held by `ForgeState`:

```rust
#[acton_actor]
pub struct ForgeActor {
    registry: HashMap<String, SchemaDefinition>,
    backend: Arc<dyn DynForgeBackend>,
    tenant_config: Option<TenantConfig>,
    record_access_policy: Option<Arc<dyn RecordAccessPolicy>>,
}
```

Note: `SchemaRegistry` (the `Arc<RwLock<HashMap>>` wrapper) is deleted. The actor's single-threaded message processing replaces the lock. The inner `HashMap` is sufficient.

The `DynForgeBackend` trait (and `DynSchemaBackend`/`DynEntityStore`) remain as internal implementation details — the actor uses them for dynamic dispatch over concrete backends. What's deleted is their public exposure on `ForgeState`. Consumers never interact with these traits directly; they send messages to the actor.

**`Default` and initialization:** `ActorExtension` requires `Default`. Since `ForgeActor` contains non-defaultable fields (`backend`), a two-phase initialization is used: `Default::default()` creates a placeholder, and an `after_start` lifecycle hook (or a dedicated `InitForge` message sent immediately after actor registration) populates the real backend and loads schemas from the database. Alternatively, if `with_actor()` accepts a pre-constructed instance rather than requiring `Default`, the actor is built with all state before registration.

### Message Types

**Registry reads (sync handlers — zero async overhead):**

| Message | Response | Purpose |
|---------|----------|---------|
| `GetSchema { name: String }` | `Option<SchemaDefinition>` | Look up schema by name |
| `ListSchemas` | `Vec<SchemaDefinition>` | List all registered schemas |
| `GetTenantConfig` | `Option<TenantConfig>` | Get current tenant configuration |
| `GetRecordAccessPolicy` | `Option<Arc<dyn RecordAccessPolicy>>` | Get access policy |

**Registry mutations (async handlers):**

| Message | Response | Purpose |
|---------|----------|---------|
| `InsertSchema { name: String, definition: SchemaDefinition }` | `()` | Add or update a schema in the registry |
| `RemoveSchema { name: String }` | `Option<SchemaDefinition>` | Remove a schema from the registry |
| `UpdateTenantConfig { config: Option<TenantConfig> }` | `()` | Replace tenant configuration |

**Backend operations (async handlers — supervised database calls):**

| Message | Response | Purpose |
|---------|----------|---------|
| `CreateEntity { entity: Entity }` | `Result<Entity, BackendError>` | Create entity in database |
| `GetEntity { schema: SchemaName, id: EntityId }` | `Result<Entity, BackendError>` | Retrieve entity by ID |
| `UpdateEntity { entity: Entity }` | `Result<Entity, BackendError>` | Update entity in database |
| `DeleteEntity { schema: SchemaName, id: EntityId }` | `Result<(), BackendError>` | Delete entity from database |
| `QueryEntities { query: Query }` | `Result<QueryResult, BackendError>` | Execute filtered/sorted/paginated query |
| `CountEntities { query: Query }` | `Result<usize, BackendError>` | Count matching entities |
| `AggregateEntities { query: AggregateQuery }` | `Result<Vec<AggregateResult>, BackendError>` | Compute aggregates |
| `ApplyMigration { schema_name: SchemaName, steps: Vec<MigrationStep> }` | `Result<(), BackendError>` | Apply DDL migration steps |
| `StoreSchemaMetadata { definition: SchemaDefinition }` | `Result<(), BackendError>` | Persist schema metadata |
| `LoadSchemaMetadata { name: SchemaName }` | `Result<Option<SchemaDefinition>, BackendError>` | Load schema metadata from backend |

### ActorExtension Implementation

```rust
impl ActorExtension for ForgeActor {
    fn configure(actor: &mut ManagedActor<Idle, Self>) {
        // Sync registry reads
        actor.act_on_sync::<GetSchema>(|actor, envelope| { /* ... */ });
        actor.act_on_sync::<ListSchemas>(|actor, envelope| { /* ... */ });
        actor.act_on_sync::<GetTenantConfig>(|actor, envelope| { /* ... */ });
        actor.act_on_sync::<GetRecordAccessPolicy>(|actor, envelope| { /* ... */ });

        // Async registry mutations
        actor.mutate_on::<InsertSchema>(|actor, envelope| { /* ... */ });
        actor.mutate_on::<RemoveSchema>(|actor, envelope| { /* ... */ });
        actor.mutate_on::<UpdateTenantConfig>(|actor, envelope| { /* ... */ });

        // Async backend operations (supervised)
        actor.act_on::<CreateEntity>(|actor, envelope| { /* ... */ });
        actor.act_on::<GetEntity>(|actor, envelope| { /* ... */ });
        actor.act_on::<UpdateEntity>(|actor, envelope| { /* ... */ });
        actor.act_on::<DeleteEntity>(|actor, envelope| { /* ... */ });
        actor.act_on::<QueryEntities>(|actor, envelope| { /* ... */ });
        actor.act_on::<CountEntities>(|actor, envelope| { /* ... */ });
        actor.act_on::<AggregateEntities>(|actor, envelope| { /* ... */ });
        actor.act_on::<ApplyMigration>(|actor, envelope| { /* ... */ });
        actor.act_on::<StoreSchemaMetadata>(|actor, envelope| { /* ... */ });
        actor.act_on::<LoadSchemaMetadata>(|actor, envelope| { /* ... */ });
    }

    fn restart_policy() -> RestartPolicy {
        RestartPolicy::Permanent
    }
}
```

## SchemaForgeConfig

The serializable `T` in `AppState<T>`. Holds TOML-deserializable configuration only — no runtime state.

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SchemaForgeConfig {
    pub schema_dir: Option<PathBuf>,
    pub admin_user: Option<String>,
}
```

## Handler Migration

All ~50 entity and schema management handlers change their state extractor:

```rust
// Before:
async fn create_entity(
    State(state): State<ForgeState>,
    // ...
)

// After:
async fn create_entity(
    State(state): State<AppState<SchemaForgeConfig>>,
    // ...
)
```

Field access becomes actor messages:

```rust
// Before:
let schema_def = state.registry.get(&name).await;
let created = state.backend.create(&entity).await?;

// After:
let forge = state.actor::<ForgeActor>().unwrap();
let schema_def = forge.send(GetSchema { name }).await;
let created = forge.send(CreateEntity { entity }).await?;
```

## Route Registration

`forge_routes()` changes return type:

```rust
// Before:
pub fn forge_routes() -> Router<ForgeState>

// After:
pub fn forge_routes() -> Router<AppState<SchemaForgeConfig>>
```

`register_versioned_routes` uses regular `nest` instead of `nest_service`:

```rust
// Before:
pub fn register_versioned_routes<T>(&self, router: Router<AppState<T>>) -> Router<AppState<T>> {
    let forge_router: Router<()> = forge_routes().with_state(self.state.clone());
    router.nest_service("/forge", forge_router)  // severs AppState
}

// After:
pub fn register_versioned_routes(
    router: Router<AppState<SchemaForgeConfig>>,
) -> Router<AppState<SchemaForgeConfig>> {
    router.nest("/forge", forge_routes())  // preserves AppState
}
```

## Service Builder Integration

The `serve` command in schema-forge-cli changes from manually constructing `ForgeState` to registering the actor:

```rust
// Before:
let extension = SchemaForgeExtension::builder()
    .with_backend(backend)
    .build().await?;
let routes = build_versioned_routes(&extension);
let service = ServiceBuilder::new()
    .with_config(svc_config)
    .with_routes(routes)
    .build();

// After:
let forge_actor = ForgeActor::new(backend, /* ... */).await?;
let routes = build_versioned_routes();
let service = ServiceBuilder::new()
    .with_config(svc_config)
    .with_actor(forge_actor)
    .with_routes(routes)
    .build();
```

## Instrumentation

### Tracing

Add `tracing` as a required dependency to `schema-forge-acton`, `schema-forge-core`, and `schema-forge-dsl`.

Add `#[instrument]` to:

**schema-forge-acton (handlers):**
- `create_entity`, `list_entities`, `query_entities`, `get_entity`, `update_entity`, `delete_entity`
- `create_schema`, `list_schemas`, `get_schema`, `update_schema`, `delete_schema`

All handler instruments skip `state` and `body` fields, and record the schema name:

```rust
#[instrument(skip(state, body), fields(schema = %schema))]
async fn create_entity(/* ... */) { /* ... */ }
```

**schema-forge-core:**
- `DiffEngine::diff` — fields: old schema name, new schema name
- `DiffEngine::create_new` — fields: schema name
- `MigrationPlan::overall_safety` — fields: schema name, step count

**schema-forge-dsl:**
- `parse()` — fields: source length

### Audit

Audit events are emitted in HTTP handlers after successful state-changing operations. The handler has request context (claims, path) needed for `AuditSource`.

| Operation | Event Name | Severity | Metadata |
|-----------|-----------|----------|----------|
| Entity created | `forge.entity.created` | Informational | schema, entity_id, user |
| Entity updated | `forge.entity.updated` | Informational | schema, entity_id, user |
| Entity deleted | `forge.entity.deleted` | Warning | schema, entity_id, user |
| Schema created | `forge.schema.created` | Notice | schema_name, field_count, user |
| Schema migrated | `forge.schema.migrated` | Notice | schema_name, safety_level, step_count, user |
| Schema deleted | `forge.schema.deleted` | Warning | schema_name, user |
| Access denied | `forge.access.denied` | Warning | schema, action, user |

Audit call pattern in handlers:

```rust
if let Some(logger) = state.audit_logger() {
    logger.log_custom(
        "forge.entity.created",
        AuditSeverity::Informational,
        Some(json!({
            "schema": schema_name,
            "entity_id": created.id.as_str(),
            "user": claims.as_ref().map(|c| &c.sub),
        })),
    ).await;
}
```

## What Gets Deleted

- `ForgeState` struct (`state.rs`)
- `SchemaRegistry` struct and its `Arc<RwLock<HashMap>>` internals (`state.rs`)
- `DynSchemaBackend`, `DynEntityStore`, `DynForgeBackend` — removed from public API, retained as internal implementation details within the actor module
- `SchemaForgeExtensionBuilder` struct (`extension.rs`)

## What Gets Refactored

- `SchemaForgeExtension` — simplified to construct `ForgeActor` and provide `register_routes()`
- All entity CRUD handlers in `routes/entities.rs` (~6 handlers)
- All schema management handlers in `routes/schemas.rs` (~5 handlers)
- `forge_routes()` return type in `routes/mod.rs`
- `execute_entity_query()` shared helper in `routes/entities.rs`
- `serve.rs` command — use `ServiceBuilder::with_actor::<ForgeActor>()`
- Access control helpers (`check_schema_access`, `filter_entity_fields`, `inject_tenant_scope`, `inject_tenant_on_create`) — parameter changes from `ForgeState` fields to values retrieved from actor
- All tests constructing `ForgeState`

## What Gets Added

- `ForgeActor` struct with `ActorExtension` impl
- ~15 message type structs
- `SchemaForgeConfig` struct
- `#[instrument]` on ~15 functions across 3 crates
- Audit emissions in ~7 handlers
- `tracing` dependency in `schema-forge-core` and `schema-forge-dsl`

## Out of Scope

- PostgreSQL backend implementation (separate future spec)
- Admin UI handler migration (`admin/handlers.rs`, `admin/auth.rs`) — deferred, uses compatibility shim
- Widget UI handler migration (`widget/handlers.rs`) — deferred, uses compatibility shim
- GraphQL handler migration — deferred
- CLI commands other than `serve`

## Compatibility Shim for Deferred UI Handlers

Admin, widget, and GraphQL handlers continue using `ForgeState` temporarily. The `ForgeActor` exposes a method to reconstruct the legacy struct:

```rust
impl ForgeActor {
    pub fn to_forge_state(&self) -> ForgeState {
        ForgeState {
            registry: self.build_legacy_registry(),
            backend: Arc::clone(&self.backend),
            tenant_config: self.tenant_config.clone(),
            record_access_policy: self.record_access_policy.clone(),
            // feature-gated fields populated as needed
        }
    }
}
```

UI routes are registered with `.with_state(forge_state)` as before, keeping them functional until their own migration.

## Dependency Changes

| Crate | Change |
|-------|--------|
| `schema-forge-acton` | `acton-service` 0.21.4 → 0.22.0, `tracing` optional → required |
| `schema-forge-backend` | `acton-service` 0.21.4 → 0.22.0 |
| `schema-forge-cli` | `acton-service` 0.21.4 → 0.22.0 |
| `schema-forge-core` | Add `tracing = "0.1"` |
| `schema-forge-dsl` | Add `tracing = "0.1"` |
