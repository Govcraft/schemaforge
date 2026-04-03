# AppState Unification Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace SchemaForge's separate `ForgeState` with a `ForgeActor` registered via acton-service v0.22's actor extensions, unifying state so handlers access audit, tracing, and broker alongside forge-specific state.

**Architecture:** A single `ForgeActor` owns the schema registry, database backend, tenant config, and access policy. All handlers move from `State<ForgeState>` to `State<AppState<SchemaForgeConfig>>` and interact with forge state via actor messages. Sync handlers serve reads; async handlers serve mutations and backend operations.

**Tech Stack:** acton-service 0.22.0, acton-reactive (actor extensions, supervised actors), tracing (instrumentation), serde_json (audit metadata)

**Spec:** `docs/superpowers/specs/2026-04-03-appstate-unification-design.md`

---

### Task 1: Bump acton-service to 0.22.0

**Files:**
- Modify: `crates/schema-forge-acton/Cargo.toml`
- Modify: `crates/schema-forge-backend/Cargo.toml`
- Modify: `crates/schema-forge-cli/Cargo.toml`

- [ ] **Step 1: Update acton-service in schema-forge-acton**

```bash
cd crates/schema-forge-acton && cargo add acton-service@0.22 --no-default-features --features http,observability,otel-metrics,journald,surrealdb,governor,resilience,audit,openapi
```

- [ ] **Step 2: Update acton-service in schema-forge-backend**

```bash
cd crates/schema-forge-backend && cargo add acton-service@0.22 --no-default-features
```

- [ ] **Step 3: Update acton-service in schema-forge-cli**

```bash
cd crates/schema-forge-cli && cargo add acton-service@0.22 --no-default-features --features http,observability,otel-metrics,journald,surrealdb,governor,resilience,audit,openapi,auth
```

- [ ] **Step 4: Verify the workspace compiles**

```bash
cargo check
```

Expected: compiles cleanly. If there are breaking API changes in 0.22, fix them before proceeding.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -S -m "chore: bump acton-service to 0.22.0"
```

---

### Task 2: Add tracing dependencies

**Files:**
- Modify: `crates/schema-forge-acton/Cargo.toml`
- Modify: `crates/schema-forge-core/Cargo.toml`
- Modify: `crates/schema-forge-dsl/Cargo.toml`

- [ ] **Step 1: Make tracing required in schema-forge-acton**

Currently `tracing` is optional (only for graphql). Make it a required dependency:

```bash
cd crates/schema-forge-acton && cargo add tracing@0.1
```

Then remove `dep:tracing` from the `graphql` feature list in Cargo.toml (tracing is now always available).

- [ ] **Step 2: Add tracing to schema-forge-core**

```bash
cd crates/schema-forge-core && cargo add tracing@0.1
```

- [ ] **Step 3: Add tracing to schema-forge-dsl**

```bash
cd crates/schema-forge-dsl && cargo add tracing@0.1
```

- [ ] **Step 4: Verify compilation**

```bash
cargo check
```

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -S -m "chore: add tracing as required dependency to core, dsl, acton"
```

---

### Task 3: Create ForgeActor with message types

**Files:**
- Create: `crates/schema-forge-acton/src/actor.rs`
- Create: `crates/schema-forge-acton/src/messages.rs`
- Modify: `crates/schema-forge-acton/src/lib.rs`

- [ ] **Step 1: Create message types**

Create `crates/schema-forge-acton/src/messages.rs`:

```rust
use std::sync::Arc;

use schema_forge_backend::auth::RecordAccessPolicy;
use schema_forge_backend::entity::{Entity, QueryResult};
use schema_forge_backend::error::BackendError;
use schema_forge_backend::tenant::TenantConfig;
use schema_forge_core::migration::MigrationStep;
use schema_forge_core::query::{AggregateQuery, AggregateResult, Query};
use schema_forge_core::types::{EntityId, SchemaDefinition, SchemaName};

// ---------------------------------------------------------------------------
// Registry reads (sync handlers)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct GetSchema {
    pub name: String,
}

#[derive(Clone, Debug)]
pub struct GetSchemaResponse(pub Option<SchemaDefinition>);

#[derive(Clone, Debug)]
pub struct ListSchemas;

#[derive(Clone, Debug)]
pub struct ListSchemasResponse(pub Vec<SchemaDefinition>);

#[derive(Clone, Debug)]
pub struct GetTenantConfig;

#[derive(Clone, Debug)]
pub struct GetTenantConfigResponse(pub Option<TenantConfig>);

#[derive(Clone, Debug)]
pub struct GetRecordAccessPolicy;

#[derive(Clone, Debug)]
pub struct GetRecordAccessPolicyResponse(pub Option<Arc<dyn RecordAccessPolicy>>);

// ---------------------------------------------------------------------------
// Registry mutations (async handlers)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct InsertSchema {
    pub name: String,
    pub definition: SchemaDefinition,
}

#[derive(Clone, Debug)]
pub struct RemoveSchema {
    pub name: String,
}

#[derive(Clone, Debug)]
pub struct RemoveSchemaResponse(pub Option<SchemaDefinition>);

#[derive(Clone, Debug)]
pub struct UpdateTenantConfig {
    pub config: Option<TenantConfig>,
}

// ---------------------------------------------------------------------------
// Backend operations (async handlers — supervised)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct CreateEntity {
    pub entity: Entity,
}

#[derive(Clone, Debug)]
pub struct CreateEntityResponse(pub Result<Entity, BackendError>);

#[derive(Clone, Debug)]
pub struct GetEntity {
    pub schema: SchemaName,
    pub id: EntityId,
}

#[derive(Clone, Debug)]
pub struct GetEntityResponse(pub Result<Entity, BackendError>);

#[derive(Clone, Debug)]
pub struct UpdateEntity {
    pub entity: Entity,
}

#[derive(Clone, Debug)]
pub struct UpdateEntityResponse(pub Result<Entity, BackendError>);

#[derive(Clone, Debug)]
pub struct DeleteEntity {
    pub schema: SchemaName,
    pub id: EntityId,
}

#[derive(Clone, Debug)]
pub struct DeleteEntityResponse(pub Result<(), BackendError>);

#[derive(Clone, Debug)]
pub struct QueryEntities {
    pub query: Query,
}

#[derive(Clone, Debug)]
pub struct QueryEntitiesResponse(pub Result<QueryResult, BackendError>);

#[derive(Clone, Debug)]
pub struct CountEntities {
    pub query: Query,
}

#[derive(Clone, Debug)]
pub struct CountEntitiesResponse(pub Result<usize, BackendError>);

#[derive(Clone, Debug)]
pub struct AggregateEntities {
    pub query: AggregateQuery,
}

#[derive(Clone, Debug)]
pub struct AggregateEntitiesResponse(pub Result<Vec<AggregateResult>, BackendError>);

#[derive(Clone, Debug)]
pub struct ApplyMigration {
    pub schema_name: SchemaName,
    pub steps: Vec<MigrationStep>,
}

#[derive(Clone, Debug)]
pub struct ApplyMigrationResponse(pub Result<(), BackendError>);

#[derive(Clone, Debug)]
pub struct StoreSchemaMetadata {
    pub definition: SchemaDefinition,
}

#[derive(Clone, Debug)]
pub struct StoreSchemaMetadataResponse(pub Result<(), BackendError>);

#[derive(Clone, Debug)]
pub struct LoadSchemaMetadata {
    pub name: SchemaName,
}

#[derive(Clone, Debug)]
pub struct LoadSchemaMetadataResponse(pub Result<Option<SchemaDefinition>, BackendError>);
```

- [ ] **Step 2: Create the ForgeActor**

Create `crates/schema-forge-acton/src/actor.rs`:

```rust
use std::collections::HashMap;
use std::sync::Arc;

use acton_reactive::prelude::*;
use acton_service::prelude::*;

use schema_forge_backend::auth::RecordAccessPolicy;
use schema_forge_backend::tenant::TenantConfig;
use schema_forge_core::types::SchemaDefinition;

use crate::messages::*;
use crate::state::{DynForgeBackend};

#[acton_actor]
pub struct ForgeActor {
    pub(crate) registry: HashMap<String, SchemaDefinition>,
    pub(crate) backend: Option<Arc<dyn DynForgeBackend>>,
    pub(crate) tenant_config: Option<TenantConfig>,
    pub(crate) record_access_policy: Option<Arc<dyn RecordAccessPolicy>>,
}

impl ActorExtension for ForgeActor {
    fn configure(actor: &mut ManagedActor<Idle, Self>) {
        // --- Sync registry reads ---

        actor.act_on_sync::<GetSchema>(|actor, envelope| {
            let name = &envelope.message().name;
            let _result = actor.model.registry.get(name).cloned();
            // Response sent via reply envelope in act_on_sync
        });

        actor.act_on_sync::<ListSchemas>(|actor, _envelope| {
            let _result: Vec<SchemaDefinition> = actor.model.registry.values().cloned().collect();
        });

        actor.act_on_sync::<GetTenantConfig>(|actor, _envelope| {
            let _result = actor.model.tenant_config.clone();
        });

        actor.act_on_sync::<GetRecordAccessPolicy>(|actor, _envelope| {
            let _result = actor.model.record_access_policy.clone();
        });

        // --- Async registry mutations ---

        actor.mutate_on::<InsertSchema>(|actor, envelope| {
            let msg = envelope.message();
            actor.model.registry.insert(msg.name.clone(), msg.definition.clone());
            Reply::ready()
        });

        actor.mutate_on::<RemoveSchema>(|actor, envelope| {
            let msg = envelope.message();
            let _removed = actor.model.registry.remove(&msg.name);
            Reply::ready()
        });

        actor.mutate_on::<UpdateTenantConfig>(|actor, envelope| {
            actor.model.tenant_config = envelope.message().config.clone();
            Reply::ready()
        });

        // --- Async backend operations (supervised) ---

        actor.act_on::<CreateEntity>(|actor, envelope| {
            let entity = envelope.message().entity.clone();
            let backend = actor.model.backend.clone();
            let reply = envelope.reply_envelope();
            Reply::pending(async move {
                if let Some(backend) = backend {
                    let result = backend.create(&entity).await;
                    reply.send(CreateEntityResponse(result)).await;
                }
            })
        });

        actor.act_on::<GetEntity>(|actor, envelope| {
            let msg = envelope.message();
            let schema = msg.schema.clone();
            let id = msg.id.clone();
            let backend = actor.model.backend.clone();
            let reply = envelope.reply_envelope();
            Reply::pending(async move {
                if let Some(backend) = backend {
                    let result = backend.get(&schema, &id).await;
                    reply.send(GetEntityResponse(result)).await;
                }
            })
        });

        actor.act_on::<UpdateEntity>(|actor, envelope| {
            let entity = envelope.message().entity.clone();
            let backend = actor.model.backend.clone();
            let reply = envelope.reply_envelope();
            Reply::pending(async move {
                if let Some(backend) = backend {
                    let result = backend.update(&entity).await;
                    reply.send(UpdateEntityResponse(result)).await;
                }
            })
        });

        actor.act_on::<DeleteEntity>(|actor, envelope| {
            let msg = envelope.message();
            let schema = msg.schema.clone();
            let id = msg.id.clone();
            let backend = actor.model.backend.clone();
            let reply = envelope.reply_envelope();
            Reply::pending(async move {
                if let Some(backend) = backend {
                    let result = backend.delete(&schema, &id).await;
                    reply.send(DeleteEntityResponse(result)).await;
                }
            })
        });

        actor.act_on::<QueryEntities>(|actor, envelope| {
            let query = envelope.message().query.clone();
            let backend = actor.model.backend.clone();
            let reply = envelope.reply_envelope();
            Reply::pending(async move {
                if let Some(backend) = backend {
                    let result = backend.query(&query).await;
                    reply.send(QueryEntitiesResponse(result)).await;
                }
            })
        });

        actor.act_on::<CountEntities>(|actor, envelope| {
            let query = envelope.message().query.clone();
            let backend = actor.model.backend.clone();
            let reply = envelope.reply_envelope();
            Reply::pending(async move {
                if let Some(backend) = backend {
                    let result = backend.count(&query).await;
                    reply.send(CountEntitiesResponse(result)).await;
                }
            })
        });

        actor.act_on::<AggregateEntities>(|actor, envelope| {
            let query = envelope.message().query.clone();
            let backend = actor.model.backend.clone();
            let reply = envelope.reply_envelope();
            Reply::pending(async move {
                if let Some(backend) = backend {
                    let result = backend.aggregate(&query).await;
                    reply.send(AggregateEntitiesResponse(result)).await;
                }
            })
        });

        actor.act_on::<ApplyMigration>(|actor, envelope| {
            let msg = envelope.message();
            let schema_name = msg.schema_name.clone();
            let steps = msg.steps.clone();
            let backend = actor.model.backend.clone();
            let reply = envelope.reply_envelope();
            Reply::pending(async move {
                if let Some(backend) = backend {
                    let result = backend.apply_migration(&schema_name, &steps).await;
                    reply.send(ApplyMigrationResponse(result)).await;
                }
            })
        });

        actor.act_on::<StoreSchemaMetadata>(|actor, envelope| {
            let definition = envelope.message().definition.clone();
            let backend = actor.model.backend.clone();
            let reply = envelope.reply_envelope();
            Reply::pending(async move {
                if let Some(backend) = backend {
                    let result = backend.store_schema_metadata(&definition).await;
                    reply.send(StoreSchemaMetadataResponse(result)).await;
                }
            })
        });

        actor.act_on::<LoadSchemaMetadata>(|actor, envelope| {
            let name = envelope.message().name.clone();
            let backend = actor.model.backend.clone();
            let reply = envelope.reply_envelope();
            Reply::pending(async move {
                if let Some(backend) = backend {
                    let result = backend.load_schema_metadata(&name).await;
                    reply.send(LoadSchemaMetadataResponse(result)).await;
                }
            })
        });
    }

    fn restart_policy() -> RestartPolicy {
        RestartPolicy::Permanent
    }
}
```

**Important:** The `act_on_sync` handler pattern and reply mechanics depend on the exact acton-reactive 0.22 API. The code above is a best-effort match against the documented API. During implementation, consult the `@agent-acton-reactive-expert` agent for the correct handler signatures and reply patterns for sync vs async handlers.

- [ ] **Step 3: Register modules in lib.rs**

Add to `crates/schema-forge-acton/src/lib.rs`:

```rust
pub mod actor;
pub mod messages;
```

And update the public exports to include `ForgeActor`:

```rust
pub use actor::ForgeActor;
```

- [ ] **Step 4: Verify compilation**

```bash
cargo check -p schema-forge-acton
```

Expected: compiles. The actor won't be wired up yet — this just verifies the types are correct.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -S -m "feat: add ForgeActor with message types and ActorExtension impl"
```

---

### Task 4: Migrate route types and forge_routes()

**Files:**
- Modify: `crates/schema-forge-acton/src/routes/mod.rs`
- Modify: `crates/schema-forge-acton/src/routes/entities.rs` (imports only)
- Modify: `crates/schema-forge-acton/src/routes/schemas.rs` (imports only)

- [ ] **Step 1: Change forge_routes() return type**

In `crates/schema-forge-acton/src/routes/mod.rs`, change:

```rust
use crate::state::ForgeState;

pub fn forge_routes() -> Router<ForgeState> {
```

to:

```rust
use acton_service::state::AppState;
use crate::config::SchemaForgeConfig;

pub fn forge_routes() -> Router<AppState<SchemaForgeConfig>> {
```

The route definitions inside the function stay the same — only the return type and imports change.

- [ ] **Step 2: Update entity handler imports**

In `crates/schema-forge-acton/src/routes/entities.rs`, change:

```rust
use crate::state::ForgeState;
```

to:

```rust
use acton_service::state::AppState;
use crate::config::SchemaForgeConfig;
use crate::actor::ForgeActor;
use crate::messages::*;
```

- [ ] **Step 3: Update schema handler imports**

In `crates/schema-forge-acton/src/routes/schemas.rs`, change:

```rust
use crate::state::ForgeState;
```

to:

```rust
use acton_service::state::AppState;
use crate::config::SchemaForgeConfig;
use crate::actor::ForgeActor;
use crate::messages::*;
```

- [ ] **Step 4: This will NOT compile yet** — handlers still reference `State<ForgeState>`. That's expected. Verify with:

```bash
cargo check -p schema-forge-acton 2>&1 | head -20
```

Expected: type mismatch errors on handler signatures.

- [ ] **Step 5: Commit (work-in-progress)**

```bash
git add -A && git commit -S -m "refactor: change forge_routes return type to AppState<SchemaForgeConfig>"
```

---

### Task 5: Migrate entity handlers

**Files:**
- Modify: `crates/schema-forge-acton/src/routes/entities.rs`

This is the largest task. Every handler's state extractor changes, and every `state.registry.*` / `state.backend.*` call becomes an actor message.

- [ ] **Step 1: Migrate execute_entity_query helper**

Change the signature and body of `execute_entity_query` (around line 408):

```rust
async fn execute_entity_query(
    forge: &ActorHandle,  // actor handle, not ForgeState
    tenant_config: &Option<TenantConfig>,
    record_access_policy: &Option<Arc<dyn RecordAccessPolicy>>,
    schema_def: &SchemaDefinition,
    claims: Option<&Claims>,
    query: &mut schema_forge_core::query::Query,
) -> Result<ListEntitiesResponse, ForgeError> {
    inject_tenant_scope(query, claims, tenant_config);

    let response = forge.send(QueryEntities { query: query.clone() }).await;
    let result = response.0.map_err(ForgeError::from)?;

    let visible_entities =
        if let (Some(ref policy), Some(c)) = (record_access_policy, claims) {
            policy.filter_visible(schema_def, c, result.entities).await
        } else {
            result.entities
        };

    let entities: Vec<EntityResponse> = visible_entities
        .into_iter()
        .map(|mut e| {
            filter_entity_fields(&mut e, schema_def, claims, FieldFilterDirection::Read);
            entity_to_response(&e)
        })
        .collect();
    let count = entities.len();

    Ok(ListEntitiesResponse {
        entities,
        count,
        total_count: result.total_count,
    })
}
```

- [ ] **Step 2: Migrate create_entity handler**

Change the handler to use `AppState` and actor messages. Add `#[instrument]` and audit:

```rust
#[instrument(skip(state, body), fields(schema = %schema))]
pub async fn create_entity(
    State(state): State<AppState<SchemaForgeConfig>>,
    Path(schema): Path<String>,
    OptionalClaims(claims): OptionalClaims,
    Json(body): Json<EntityRequest>,
) -> Result<impl IntoResponse, ForgeError> {
    let schema_name = validate_schema_name(&schema)?;
    let forge = state.actor::<ForgeActor>().expect("ForgeActor not registered");

    // Look up schema via actor
    let schema_def = forge.send(GetSchema { name: schema_name.as_str().to_string() }).await;
    let schema_def = schema_def.0.ok_or(ForgeError::SchemaNotFound {
        name: schema_name.as_str().to_string(),
    })?;

    check_schema_access(&schema_def, claims.as_ref(), AccessAction::Write)?;

    let mut fields = json_to_entity_fields(&schema_def, &body.fields)
        .map_err(|errors| ForgeError::ValidationFailed { details: errors })?;

    // Get tenant config via actor
    let tc = forge.send(GetTenantConfig).await;
    inject_tenant_on_create(&mut fields, claims.as_ref(), &tc.0);

    let mut entity = Entity::new(schema_name, fields);
    filter_entity_fields(&mut entity, &schema_def, claims.as_ref(), FieldFilterDirection::Write);

    let response = forge.send(CreateEntity { entity }).await;
    let mut created = response.0.map_err(ForgeError::from)?;

    filter_entity_fields(&mut created, &schema_def, claims.as_ref(), FieldFilterDirection::Read);

    // Audit
    if let Some(logger) = state.audit_logger() {
        logger.log_custom(
            "forge.entity.created",
            acton_service::prelude::AuditSeverity::Informational,
            Some(serde_json::json!({
                "schema": schema,
                "entity_id": created.id.as_str(),
                "user": claims.as_ref().map(|c| &c.sub),
            })),
        ).await;
    }

    Ok((StatusCode::CREATED, Json(entity_to_response(&created))))
}
```

- [ ] **Step 3: Migrate list_entities, query_entities, get_entity, update_entity, delete_entity**

Apply the same pattern to each remaining handler:
1. Change `State(state): State<ForgeState>` → `State(state): State<AppState<SchemaForgeConfig>>`
2. Get actor handle: `let forge = state.actor::<ForgeActor>().expect("ForgeActor not registered");`
3. Replace `state.registry.get(...)` → `forge.send(GetSchema { ... }).await`
4. Replace `state.backend.*()` → `forge.send(XxxEntity { ... }).await` and unwrap the response
5. Replace `state.tenant_config` → `forge.send(GetTenantConfig).await`
6. Replace `state.record_access_policy` → `forge.send(GetRecordAccessPolicy).await`
7. Add `#[instrument(skip(state, ...), fields(schema = ...))]`
8. Add audit emissions for `update_entity` and `delete_entity`

For `delete_entity`, use `AuditSeverity::Warning`:

```rust
if let Some(logger) = state.audit_logger() {
    logger.log_custom(
        "forge.entity.deleted",
        acton_service::prelude::AuditSeverity::Warning,
        Some(serde_json::json!({
            "schema": schema,
            "entity_id": id,
            "user": claims.as_ref().map(|c| &c.sub),
        })),
    ).await;
}
```

- [ ] **Step 4: Verify compilation**

```bash
cargo check -p schema-forge-acton
```

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -S -m "refactor: migrate entity handlers to AppState with audit and tracing"
```

---

### Task 6: Migrate schema handlers

**Files:**
- Modify: `crates/schema-forge-acton/src/routes/schemas.rs`

- [ ] **Step 1: Migrate all 5 schema handlers**

Apply the same pattern as entity handlers. For `create_schema`:

```rust
#[instrument(skip(state, body))]
pub async fn create_schema(
    State(state): State<AppState<SchemaForgeConfig>>,
    OptionalClaims(claims): OptionalClaims,
    Json(body): Json<CreateSchemaRequest>,
) -> Result<impl IntoResponse, ForgeError> {
    let claims = require_auth(&claims)?;
    require_admin(claims)?;

    let schema_name = SchemaName::new(&body.name).map_err(|_| ForgeError::InvalidSchemaName {
        name: body.name.clone(),
    })?;

    let forge = state.actor::<ForgeActor>().expect("ForgeActor not registered");

    // Check conflict via actor
    let existing = forge.send(GetSchema { name: schema_name.as_str().to_string() }).await;
    if existing.0.is_some() {
        return Err(ForgeError::SchemaAlreadyExists {
            name: schema_name.as_str().to_string(),
        });
    }

    // ... field parsing unchanged ...

    let plan = DiffEngine::create_new(&definition);

    // Apply migration via actor
    let result = forge.send(ApplyMigration {
        schema_name: schema_name.clone(),
        steps: plan.steps,
    }).await;
    result.0.map_err(ForgeError::from)?;

    // Store metadata via actor
    let result = forge.send(StoreSchemaMetadata {
        definition: definition.clone(),
    }).await;
    result.0.map_err(ForgeError::from)?;

    // Update registry via actor
    forge.send(InsertSchema {
        name: schema_name.as_str().to_string(),
        definition: definition.clone(),
    }).await;

    // Audit
    if let Some(logger) = state.audit_logger() {
        logger.log_custom(
            "forge.schema.created",
            acton_service::prelude::AuditSeverity::Notice,
            Some(serde_json::json!({
                "schema_name": body.name,
                "field_count": definition.fields.len(),
                "user": claims.sub,
            })),
        ).await;
    }

    Ok((StatusCode::CREATED, Json(schema_to_response(&definition))))
}
```

For `update_schema`, add audit with `forge.schema.migrated` event including `safety_level` and `step_count`.

For `delete_schema`, add audit with `forge.schema.deleted` event at `Warning` severity.

For `list_schemas` and `get_schema`, just change the state type and add `#[instrument]`. No audit (read-only).

- [ ] **Step 2: Verify compilation**

```bash
cargo check -p schema-forge-acton
```

- [ ] **Step 3: Commit**

```bash
git add -A && git commit -S -m "refactor: migrate schema handlers to AppState with audit and tracing"
```

---

### Task 7: Refactor extension.rs

**Files:**
- Modify: `crates/schema-forge-acton/src/extension.rs`

- [ ] **Step 1: Simplify SchemaForgeExtension**

The extension no longer builds a `ForgeState`. It constructs a `ForgeActor` (pre-populated with backend, loaded schemas, seeded system schemas) and provides route registration.

Replace `SchemaForgeExtensionBuilder` with a simpler construction that:
1. Accepts a backend
2. Loads schemas from backend into a `HashMap`
3. Seeds system schemas
4. Builds tenant config
5. Returns a `ForgeActor` ready for `ServiceBuilder::with_actor()`

The route registration method changes to:

```rust
pub fn register_versioned_routes(
    router: Router<AppState<SchemaForgeConfig>>,
) -> Router<AppState<SchemaForgeConfig>> {
    router.nest("/forge", forge_routes())
}
```

Note: this is now a free function (or associated function), not a method on `&self`, since the state lives in the actor, not the extension.

- [ ] **Step 2: Add compatibility shim for deferred UI handlers**

Keep `ForgeState` in `state.rs` for now (don't delete it yet). Add a method on the extension or a helper that builds a `ForgeState` from the actor's initial data, so UI routes can still use `.with_state(forge_state)`.

- [ ] **Step 3: Verify compilation**

```bash
cargo check -p schema-forge-acton
```

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -S -m "refactor: simplify SchemaForgeExtension for actor-based state"
```

---

### Task 8: Migrate serve.rs

**Files:**
- Modify: `crates/schema-forge-cli/src/commands/serve.rs`

- [ ] **Step 1: Refactor serve command to use ServiceBuilder::with_actor()**

The serve command currently:
1. Connects to SurrealDB
2. Builds `SchemaForgeExtension` with a builder
3. Applies schemas
4. Builds versioned routes (passing `&extension`)
5. Creates `ServiceBuilder` with routes
6. Serves

Change to:
1. Connect to SurrealDB (unchanged)
2. Construct `ForgeActor` with backend, loaded schemas, tenant config
3. Apply schemas via the actor (or pre-load during construction)
4. Build versioned routes (no extension reference needed — routes are typed to `AppState`)
5. Create `ServiceBuilder::with_actor::<ForgeActor>()` with routes
6. Serve

```rust
// Build the ForgeActor with initial state
let forge_actor = ForgeActor::with_backend(backend).await?;

// Build routes
let routes = build_versioned_routes();

// Configure and serve
let service = ServiceBuilder::<SchemaForgeConfig>::new()
    .with_config(svc_config)
    .with_actor::<ForgeActor>()  // or .with_actor(forge_actor) if pre-constructed
    .with_routes(routes)
    .build();

service.serve().await?;
```

**Note:** The exact `with_actor` API depends on whether acton-service 0.22 accepts a pre-constructed actor or requires `Default`. Consult the `@agent-acton-reactive-expert` agent for the correct pattern.

- [ ] **Step 2: Update build_versioned_routes**

Change from taking `&SchemaForgeExtension` to being a standalone function:

```rust
fn build_versioned_routes() -> acton_service::service_builder::VersionedRoutes<SchemaForgeConfig> {
    let builder = VersionedApiBuilder::<SchemaForgeConfig>::with_config()
        .with_base_path("/api")
        .add_version(ApiVersion::V1, |router| {
            router.nest("/forge", forge_routes())
        });

    builder.build_routes()
}
```

- [ ] **Step 3: Verify compilation**

```bash
cargo check -p schema-forge-cli
```

- [ ] **Step 4: Run the full test suite**

```bash
cargo nextest run
```

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -S -m "refactor: migrate serve command to use ForgeActor with ServiceBuilder"
```

---

### Task 9: Add instrumentation to core and DSL

**Files:**
- Modify: `crates/schema-forge-core/src/migration.rs`
- Modify: `crates/schema-forge-dsl/src/parser.rs`

- [ ] **Step 1: Instrument DiffEngine methods**

In `crates/schema-forge-core/src/migration.rs`, add `use tracing::instrument;` and:

```rust
#[instrument(skip(old, new), fields(old_schema = %old.name.as_str(), new_schema = %new.name.as_str()))]
pub fn diff(old: &SchemaDefinition, new: &SchemaDefinition) -> MigrationPlan {
    // ... existing body unchanged ...
}

#[instrument(skip(schema), fields(schema = %schema.name.as_str()))]
pub fn create_new(schema: &SchemaDefinition) -> MigrationPlan {
    // ... existing body unchanged ...
}
```

- [ ] **Step 2: Instrument DSL parse()**

In `crates/schema-forge-dsl/src/parser.rs`, add:

```rust
use tracing::instrument;

#[instrument(skip(source), fields(source_len = source.len()))]
pub fn parse(source: &str) -> Result<Vec<SchemaDefinition>, Vec<DslError>> {
    // ... existing body unchanged ...
}
```

- [ ] **Step 3: Verify compilation and tests**

```bash
cargo check && cargo nextest run
```

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -S -m "feat: add tracing instrumentation to DiffEngine and DSL parser"
```

---

### Task 10: Add access-denied audit events

**Files:**
- Modify: `crates/schema-forge-acton/src/access.rs`

- [ ] **Step 1: The `check_schema_access` function is a pure function that returns `Result<(), ForgeError>`**

Audit for access denial should be emitted in the handlers, not in the pure function. In each handler that calls `check_schema_access`, wrap the error case:

```rust
if let Err(ref e) = check_schema_access(&schema_def, claims.as_ref(), AccessAction::Write) {
    if let Some(logger) = state.audit_logger() {
        logger.log_custom(
            "forge.access.denied",
            acton_service::prelude::AuditSeverity::Warning,
            Some(serde_json::json!({
                "schema": schema,
                "action": "write",
                "user": claims.as_ref().map(|c| &c.sub),
            })),
        ).await;
    }
    return Err(e.clone());
}
```

This pattern is applied in `create_entity`, `update_entity`, `delete_entity`, and `create_schema`, `update_schema`, `delete_schema` — the state-changing endpoints.

Note: `ForgeError` must derive `Clone` for this pattern (or restructure to avoid needing clone). If `ForgeError` doesn't derive `Clone`, call `check_schema_access` and handle the audit before the `?` operator.

- [ ] **Step 2: Verify compilation**

```bash
cargo check -p schema-forge-acton
```

- [ ] **Step 3: Commit**

```bash
git add -A && git commit -S -m "feat: add access-denied audit events to state-changing handlers"
```

---

### Task 11: Update lib.rs exports and clean up

**Files:**
- Modify: `crates/schema-forge-acton/src/lib.rs`

- [ ] **Step 1: Update public exports**

```rust
pub use actor::ForgeActor;
pub use config::SchemaForgeConfig;
pub use error::ForgeError;
pub use extension::SchemaForgeExtension;
// Remove: pub use state::{DynEntityStore, DynForgeBackend, DynSchemaBackend, ForgeState, SchemaRegistry};
// Keep state module as pub(crate) for compatibility shim
```

- [ ] **Step 2: Run clippy**

```bash
cargo clippy --workspace -- -D warnings
```

Fix any warnings.

- [ ] **Step 3: Run full test suite**

```bash
cargo nextest run
```

Fix any failing tests. Tests that construct `ForgeState` directly need to be updated to use the actor pattern or mocks.

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -S -m "refactor: update public API exports, remove ForgeState from public API"
```

---

### Task 12: Update tests

**Files:**
- Modify: `crates/schema-forge-acton/src/routes/entities.rs` (test module)
- Modify: `crates/schema-forge-acton/src/routes/schemas.rs` (test module)
- Modify: `crates/schema-forge-cli/src/commands/serve.rs` (test module)
- Modify: any integration tests in `tests/`

- [ ] **Step 1: Update entity handler tests**

Unit tests for pure functions (`json_to_entity_fields`, `json_to_filter`, `entity_to_response`) are unchanged — they don't depend on state.

Integration tests that call handlers with `State<ForgeState>` need to construct an `AppState<SchemaForgeConfig>` with a registered `ForgeActor`. This requires setting up the acton-reactive runtime in tests.

- [ ] **Step 2: Update schema handler tests**

Pure function tests (`parse_field_type`, `request_field_to_definition`, `schema_to_response`) are unchanged.

- [ ] **Step 3: Update serve.rs tests**

The `test_router()` helper needs to construct an actor-backed `AppState` instead of a bare `ForgeState`.

- [ ] **Step 4: Run the full test suite**

```bash
cargo nextest run
```

Expected: all tests pass.

- [ ] **Step 5: Run clippy one final time**

```bash
cargo clippy --workspace -- -D warnings
```

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -S -m "test: update tests for AppState-based handlers"
```
