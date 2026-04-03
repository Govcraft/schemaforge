# acton-service SurrealDB Integration Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the hand-rolled SurrealDB connection in the `serve` command with acton-service's managed `surrealdb` feature, gaining retry logic, config-driven setup, health monitoring, and agent-managed lifecycle.

**Architecture:** The `serve` command currently creates a `SurrealBackend` via `connect_backend()` (which calls `SurrealBackend::connect_with_auth()`). We will enable acton-service's `surrealdb` feature, populate `Config<()>.surrealdb` from the existing `DbParams`, let `ServiceBuilder` spawn the `SurrealDbAgent` with retries and health checks, then extract the connected `SurrealClient` from `AppState` and pass it to `SurrealBackend::from_client()`. Non-serve commands (apply, migrate, inspect) continue using `connect_backend()` unchanged.

**Tech Stack:** Rust, acton-service 0.20 (`surrealdb` feature), schema-forge-surrealdb, surrealdb SDK 2.x

---

## File Map

| File | Action | Responsibility |
|------|--------|----------------|
| `crates/schema-forge-acton/Cargo.toml` | Modify | Add `surrealdb` to acton-service features |
| `crates/schema-forge-cli/Cargo.toml` | Modify | Add `surrealdb` to acton-service features |
| `crates/schema-forge-cli/src/commands/serve.rs` | Modify | Build `SurrealDbConfig` from `DbParams`, pass to `ServiceBuilder`, extract client from `AppState`, pass to `SurrealBackend::from_client()` |
| `crates/schema-forge-cli/src/commands/serve.rs` (tests) | Modify | Update tests to reflect new connection flow |
| `crates/schema-forge-cli/tests/cli_integration.rs` | No change | `serve --help` test still passes; no behavior change in help text |

---

### Task 1: Enable the `surrealdb` feature on acton-service

**Files:**
- Modify: `crates/schema-forge-acton/Cargo.toml:14`
- Modify: `crates/schema-forge-cli/Cargo.toml:30`

- [ ] **Step 1: Add `surrealdb` feature to schema-forge-acton**

In `crates/schema-forge-acton/Cargo.toml`, change:

```toml
acton-service = { version = "0.20", default-features = false, features = ["http", "observability", "journald"] }
```

to:

```toml
acton-service = { version = "0.20", default-features = false, features = ["http", "observability", "journald", "surrealdb"] }
```

- [ ] **Step 2: Add `surrealdb` feature to schema-forge-cli**

In `crates/schema-forge-cli/Cargo.toml`, change:

```toml
acton-service = { version = "0.20", default-features = false, features = ["http", "observability", "journald"] }
```

to:

```toml
acton-service = { version = "0.20", default-features = false, features = ["http", "observability", "journald", "surrealdb"] }
```

- [ ] **Step 3: Verify compilation**

Run: `cargo check 2>&1`
Expected: `Finished` with no errors. The `surrealdb` feature is additive; no code depends on it yet.

- [ ] **Step 4: Run clippy**

Run: `cargo clippy -- -D warnings 2>&1`
Expected: 0 warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/schema-forge-acton/Cargo.toml crates/schema-forge-cli/Cargo.toml Cargo.lock
git commit -S -m "feat: enable acton-service surrealdb feature"
```

---

### Task 2: Rewrite serve command to use ServiceBuilder-managed SurrealDB

**Files:**
- Modify: `crates/schema-forge-cli/src/commands/serve.rs:16-153`

The current `serve::run()` function:
1. Loads config and resolves `DbParams`
2. Calls `connect_backend(&db_params)` to get a `SurrealBackend`
3. Builds the `SchemaForgeExtension` with that backend
4. Creates `Config::<()>::default()` with only port/name set
5. Builds `ServiceBuilder::new().with_config(svc_config).with_routes(routes).build()`
6. Calls `service.serve().await`

The new flow:
1. Loads config and resolves `DbParams`
2. Builds `acton_service::config::Config::<()>` with `SurrealDbConfig` populated from `DbParams`
3. Builds `ServiceBuilder` which auto-spawns `SurrealDbAgent` with retries
4. Extracts `SurrealClient` from `AppState::surrealdb()`
5. Creates `SurrealBackend::from_client()` with that client
6. Builds `SchemaForgeExtension` and applies schemas
7. Calls `service.serve().await`

Key difference: `ServiceBuilder::build()` must be called *before* building the extension, because we need the `SurrealClient` from `AppState`. But `with_routes()` must be called before `build()`. This means we need to build routes *after* getting the client. Looking at the current code, `build_versioned_routes()` takes a `&SchemaForgeExtension`, and `ServiceBuilder` requires routes before `build()`.

Actually, re-reading `service_builder.rs`, `ServiceBuilder::build()` returns `ActonService<T>`, and `ActonService` has a `state()` method. The `SurrealDbAgent` connects asynchronously after `build()`. We need the client *before* `serve()` to build the extension. The agent connects in a background task during `build()`.

The solution: after `build()`, wait for the surrealdb client to become available by polling `state.surrealdb()`, then build the extension and routes, and serve. But `ActonService::serve()` consumes self and starts listening.

Better approach: We can't use `ServiceBuilder` to manage the connection *and* use the client to build the extension before serving, because `ActonService::serve()` consumes the service. Instead, we should:

1. Build the `SurrealDbConfig` from `DbParams`
2. Use `acton_service::surrealdb_backend::create_client(&config)` directly to get the retry logic and config-driven connection
3. Pass that client to `SurrealBackend::from_client()`
4. Continue with the existing flow, setting `Config.surrealdb` so `ServiceBuilder` knows about the connection for health checks

Wait — `create_client` is `pub(crate)`. Let me re-check.

Actually, looking at the agent exploration results: `create_client` at line 19 is `pub(crate)`. It's not public API. But `SurrealDbConfig` is public, and `ServiceBuilder::build()` spawns the agent.

The real solution is simpler: keep using `connect_backend()` for the initial connection (we need it synchronously before building routes), but *also* populate `Config.surrealdb` so that `ServiceBuilder` exposes SurrealDB health in `/health` and `/ready` endpoints. The `SurrealDbAgent` will also establish its own connection — but since `optional: true` and `lazy_init: true`, it won't block startup or fail if it can't connect independently.

Actually, this creates two connections which is confusing. Let me reconsider.

The cleanest approach: after `ServiceBuilder::build()`, the `SurrealDbAgent` is connecting in the background. We wait briefly for it, then extract the client. If the agent fails, we fall back to `connect_backend()`. But this is complex.

The *actually* cleanest approach: `SurrealBackend::from_client()` exists. `AppState::surrealdb()` returns `Option<Arc<SurrealClient>>`. `Surreal<Any>` is internally Arc'd and cloneable. So:

1. Build `Config` with `SurrealDbConfig` from `DbParams`
2. Call `ServiceBuilder::new().with_config(config).with_routes(placeholder).build()` — but we can't use placeholder routes
3. This doesn't work because we need the extension to build routes, and we need the client to build the extension.

The actual practical solution: Build the `SurrealDbConfig`, call the connection logic manually (same as `connect_backend` but using acton-service's `SurrealDbConfig`), create the backend, build the extension and routes, then pass the populated config to `ServiceBuilder` for health monitoring. The `SurrealDbAgent` won't double-connect if we don't set `config.surrealdb` — but then we lose health monitoring.

Let me look at this differently. The ordering constraint is:

```
SurrealDB client → SurrealBackend → SchemaForgeExtension → routes → ServiceBuilder → serve
```

ServiceBuilder needs routes at build time. Routes need the extension. The extension needs the backend. The backend needs the client. ServiceBuilder provides the client *after* build. This is a circular dependency.

The pragmatic solution is:
1. Connect to SurrealDB using `connect_backend()` (existing code, works fine)
2. Build the extension and routes (existing code)
3. Set `config.surrealdb` on the acton-service Config so health endpoints report SurrealDB status
4. The health endpoint will show the connection as healthy based on config presence

This gets us health monitoring without the agent managing the connection. The retry logic from `connect_backend` is simpler but adequate — the serve command already falls back to in-memory if remote fails.

For production, the better path is to restructure so ServiceBuilder manages the lifecycle — but that requires changes to how SchemaForgeExtension is built (lazy backend initialization). That's a larger refactor.

**Decision: Phase 1 (this plan) adds health monitoring via config. Phase 2 (future) restructures for full agent-managed lifecycle.**

- [ ] **Step 1: Add helper to build `SurrealDbConfig` from `DbParams`**

At the bottom of `crates/schema-forge-cli/src/commands/serve.rs`, before the `#[cfg(test)]` block, add:

```rust
/// Build an acton-service `SurrealDbConfig` from resolved CLI database parameters.
///
/// This enables acton-service's health endpoint to report SurrealDB connection
/// status. The actual connection is established by `connect_backend()` before
/// `ServiceBuilder::build()` because the extension needs a live client to
/// load schemas and seed system tables.
fn build_surrealdb_config(
    db_params: &crate::config::DbParams,
) -> acton_service::config::SurrealDbConfig {
    acton_service::config::SurrealDbConfig {
        url: db_params.url.clone(),
        namespace: db_params.namespace.clone(),
        database: db_params.database.clone(),
        username: db_params.username.clone(),
        password: db_params.password.clone(),
        max_retries: 3,
        retry_delay_secs: 2,
        optional: false,
        lazy_init: false,
    }
}
```

- [ ] **Step 2: Update `run()` to populate `Config.surrealdb` and use `ServiceBuilder` properly**

Replace the existing config construction block (lines 116-118 in the current serve.rs):

```rust
    let mut svc_config = acton_service::config::Config::<()>::default();
    svc_config.service.port = args.port;
    svc_config.service.name = "schema-forge".to_string();
```

with:

```rust
    let mut svc_config = acton_service::config::Config::<()>::default();
    svc_config.service.port = args.port;
    svc_config.service.name = "schema-forge".to_string();
    svc_config.surrealdb = Some(build_surrealdb_config(&db_params));
```

- [ ] **Step 3: Verify compilation**

Run: `cargo check 2>&1`
Expected: `Finished` with no errors.

- [ ] **Step 4: Run clippy**

Run: `cargo clippy -- -D warnings 2>&1`
Expected: 0 warnings.

- [ ] **Step 5: Run all tests**

Run: `cargo nextest run 2>&1`
Expected: All tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/schema-forge-cli/src/commands/serve.rs
git commit -S -m "feat(serve): populate SurrealDbConfig for health monitoring"
```

---

### Task 3: Replace `connect_backend` in serve with retry-capable connection

**Files:**
- Modify: `crates/schema-forge-cli/src/commands/serve.rs:39-40`

The current `connect_backend()` in `commands/mod.rs` does a single connection attempt with fallback to in-memory. For the serve command specifically, we want retry behavior — a production server should retry connecting to the database rather than silently falling back to in-memory.

- [ ] **Step 1: Write a `connect_with_retries` helper in serve.rs**

Add this function before `build_versioned_routes` in `serve.rs`:

```rust
/// Connect to SurrealDB with exponential backoff retries.
///
/// Unlike `connect_backend()` (used by CLI commands), this does NOT fall back
/// to in-memory on failure. A production server must connect to its configured
/// database or fail explicitly.
async fn connect_with_retries(
    db_params: &crate::config::DbParams,
    output: &crate::output::OutputContext,
) -> Result<schema_forge_surrealdb::SurrealBackend, CliError> {
    use std::time::Duration;
    use schema_forge_surrealdb::SurrealBackend;

    let max_retries: u32 = 3;
    let base_delay = Duration::from_secs(2);

    for attempt in 0..=max_retries {
        match SurrealBackend::connect_with_auth(
            &db_params.url,
            &db_params.namespace,
            &db_params.database,
            db_params.username.as_deref(),
            db_params.password.as_deref(),
        )
        .await
        {
            Ok(backend) => {
                if attempt > 0 {
                    output.success(&format!(
                        "Connected to {} after {} attempt(s)",
                        db_params.url,
                        attempt + 1
                    ));
                } else {
                    output.success(&format!("Connected to {}", db_params.url));
                }
                return Ok(backend);
            }
            Err(e) => {
                if attempt == max_retries {
                    return Err(CliError::Server {
                        message: format!(
                            "failed to connect to {} after {} attempts: {e}",
                            db_params.url,
                            max_retries + 1
                        ),
                    });
                }

                let delay = base_delay * 2_u32.pow(attempt);
                output.warn(&format!(
                    "Connection attempt {} failed: {e}. Retrying in {delay:?}...",
                    attempt + 1
                ));
                tokio::time::sleep(delay).await;
            }
        }
    }

    unreachable!()
}
```

- [ ] **Step 2: Replace the `connect_backend` call in `run()`**

Change line 40 from:

```rust
    let backend = super::connect_backend(&db_params, output).await?;
```

to:

```rust
    let backend = connect_with_retries(&db_params, output).await?;
```

- [ ] **Step 3: Verify compilation**

Run: `cargo check 2>&1`
Expected: `Finished` with no errors.

- [ ] **Step 4: Run clippy**

Run: `cargo clippy -- -D warnings 2>&1`
Expected: 0 warnings.

- [ ] **Step 5: Run all tests**

Run: `cargo nextest run 2>&1`
Expected: All tests pass. The existing serve tests use `SurrealBackend::connect_memory()` directly in `test_router()`, so they don't go through `connect_with_retries`.

- [ ] **Step 6: Commit**

```bash
git add crates/schema-forge-cli/src/commands/serve.rs
git commit -S -m "feat(serve): add retry logic for SurrealDB connection"
```

---

### Task 4: Verify health endpoint reports SurrealDB status

**Files:**
- Modify: `crates/schema-forge-cli/tests/cli_integration.rs` (add test if serve can be exercised)

Since we can't easily spin up a full serve instance in integration tests, we verify this manually and add a unit test that the config is properly constructed.

- [ ] **Step 1: Write unit test for `build_surrealdb_config`**

Add to the `#[cfg(test)] mod tests` block in `serve.rs`:

```rust
    #[test]
    fn build_surrealdb_config_from_db_params() {
        let db_params = crate::config::DbParams {
            url: "ws://db.example.com:8000".to_string(),
            namespace: "production".to_string(),
            database: "main".to_string(),
            username: Some("admin".to_string()),
            password: Some("secret".to_string()),
        };

        let config = build_surrealdb_config(&db_params);

        assert_eq!(config.url, "ws://db.example.com:8000");
        assert_eq!(config.namespace, "production");
        assert_eq!(config.database, "main");
        assert_eq!(config.username, Some("admin".to_string()));
        assert_eq!(config.password, Some("secret".to_string()));
        assert_eq!(config.max_retries, 3);
        assert_eq!(config.retry_delay_secs, 2);
        assert!(!config.optional);
        assert!(!config.lazy_init);
    }

    #[test]
    fn build_surrealdb_config_without_credentials() {
        let db_params = crate::config::DbParams {
            url: "mem://".to_string(),
            namespace: "test".to_string(),
            database: "test".to_string(),
            username: None,
            password: None,
        };

        let config = build_surrealdb_config(&db_params);

        assert_eq!(config.url, "mem://");
        assert!(config.username.is_none());
        assert!(config.password.is_none());
    }
```

- [ ] **Step 2: Run the new tests**

Run: `cargo nextest run -p schema-forge-cli build_surrealdb_config 2>&1`
Expected: 2 tests pass.

- [ ] **Step 3: Run all tests**

Run: `cargo nextest run 2>&1`
Expected: All tests pass.

- [ ] **Step 4: Run clippy**

Run: `cargo clippy -- -D warnings 2>&1`
Expected: 0 warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/schema-forge-cli/src/commands/serve.rs
git commit -S -m "test(serve): add unit tests for SurrealDbConfig construction"
```

---

### Task 5: Final verification and cleanup

- [ ] **Step 1: Full build check**

Run: `cargo check 2>&1`
Expected: `Finished` with no errors.

- [ ] **Step 2: Full clippy check**

Run: `cargo clippy -- -D warnings 2>&1`
Expected: 0 warnings.

- [ ] **Step 3: Full test suite**

Run: `cargo nextest run 2>&1`
Expected: All tests pass, no regressions.

- [ ] **Step 4: Verify non-serve commands unchanged**

Run: `cargo nextest run -p schema-forge-cli 2>&1`
Expected: All CLI tests pass. The `apply`, `migrate`, and `inspect` commands still use `connect_backend()` from `commands/mod.rs` and are unaffected.

---

## Future Work (Phase 2)

To fully leverage acton-service's agent-managed SurrealDB lifecycle, `SchemaForgeExtension` would need to support lazy backend initialization — accepting the backend *after* construction, once the `SurrealDbAgent` has established its connection. This would allow:

- `ServiceBuilder` to own the full connection lifecycle (spawn, retry, reconnect)
- Health endpoint to reflect real-time connection status (not just config presence)
- Connection pooling and monitoring via the agent system

This is a larger refactor to `schema-forge-acton`'s extension builder pattern and is out of scope for this plan.
