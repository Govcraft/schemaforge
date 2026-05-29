# SchemaForge — REST API Reference

When running `schema-forge serve`, these routes are available.

**Calling from the terminal:** the entity and auth routes below are exposed ergonomically by the CLI — `schemaforge login` for `/auth/login`, and `schemaforge entity <verb>` (list/get/create/replace/patch/delete/query) for the entity routes — with typed input and stable exit codes, so you rarely need raw `curl`. This page documents the underlying HTTP contract (methods, paths, bodies) that those commands call. See [cli-reference.md](cli-reference.md) and the query grammar in [query-api-reference.md](query-api-reference.md).

## Core API

| Method | Path | Purpose |
|--------|------|---------|
| GET | `/health` | Health check |
| GET | `/ready` | Readiness check |
| POST | `/api/v1/forge/schemas` | Create a schema (runtime) |
| GET | `/api/v1/forge/schemas` | List all schemas |
| GET | `/api/v1/forge/schemas/:name` | Get schema by name |
| PUT | `/api/v1/forge/schemas/:name` | Update a schema |
| DELETE | `/api/v1/forge/schemas/:name` | Delete a schema |
| POST | `/api/v1/forge/schemas/:schema/entities` | Create entity |
| GET | `/api/v1/forge/schemas/:schema/entities` | List entities (filter, sort, paginate, `?resolve=false` via query params) |
| POST | `/api/v1/forge/schemas/:schema/entities/query` | Query entities with JSON filter body (body field `resolve: bool`) |
| GET | `/api/v1/forge/schemas/:schema/entities/:id` | Get entity by ID (supports `?resolve=false`) |
| PUT | `/api/v1/forge/schemas/:schema/entities/:id` | Update entity |
| DELETE | `/api/v1/forge/schemas/:schema/entities/:id` | Delete entity |

Entity create/update request body format:
```json
{"fields": {"name": "value", "active": true}}
```

**Write-time rules.** Entity create/update run the schema's CEL rules before persistence (`@default` → `@compute` → `@require`, ahead of any hook). A failing `@require` returns **422** with the rule's message; `@compute`/`@default` fields are server-derived and overwrite/fill client input. A `@require` asserting over a related row (`related.<field>.<col>`) is tenant-scoped and fail-closed. See [dsl-reference.md](dsl-reference.md).

All API routes (except `/health`, `/ready`, and `/api/v1/forge/auth/login`) require a PASETO bearer token in the `Authorization` header.

## File Field Endpoints

Path prefix: `/api/v1/forge/schemas/:schema/entities/:id/fields/:field/*`

Present for every `file`-typed field. The runtime never handles upload bytes — clients PUT directly to S3 using a presigned URL minted by the runtime. Downloads follow the field's `access` setting (presigned redirect or proxied stream).

| Method | Path | Purpose |
|--------|------|---------|
| POST | `.../upload-url` | Mint a presigned PUT URL. Requires `Write` access. Body: `{ filename, mime, size }`. Response: `{ upload_url, key, headers, expires_at }`. Fires `before_upload` hook (blocking). |
| POST | `.../confirm-upload` | Verify the upload landed via `HeadObject` and persist a `FileAttachment` onto the entity. Body: `{ key, checksum_sha256? }`. Transitions to `scanning` (if `on_scan_complete` hook exists) or `available`. Fires `after_upload` hook (detached). |
| GET | `.../fields/{field}` | Download. Presigned mode: 302 to signed S3 URL (`?redirect=false` returns JSON `{url}`). Proxied mode: streams bytes through the runtime, re-checking authz. Refuses with 409 unless `status == "available"`. |
| POST | `.../scan-complete` | Scanner callback. **Requires `platform_admin` role.** Body: `{ status: "available"\|"quarantined", reason? }`. Only valid from state `scanning`. Fires `on_scan_complete` hook. |

See [storage-reference.md](storage-reference.md) for the full upload flow, state machine, bucket layout, and scanner integration walkthrough.

## Auth

Path prefix: `/api/v1/forge/auth/*`

| Method | Path | Purpose |
|--------|------|---------|
| POST | `/api/v1/forge/auth/login` | Exchange username+password for a PASETO token. Response body: `{ token, expires_at, roles }`. CLI: `schemaforge login --server <url> -u <user>` (caches the token for subsequent `entity` calls). |
| POST | `/api/v1/forge/auth/refresh` | Exchange a still-valid bearer for a fresh token (same 1-hour expiry). Same response body as login. Returns 401 if no/expired token. |

The React site's `src/lib/auth.ts` stores the token in `sessionStorage`, schedules a silent refresh ~5 minutes before expiry, and retries any 401 once through `/auth/refresh` before bouncing the user back to `/login`.

## Users

Path prefix: `/api/v1/forge/users`

Schema-forge-native user management backed by `EntityAuthStore` — the user table **is** the system `User` schema, not a parallel `_forge_users` store. Every endpoint routes through Cedar; there are no hand-written role string-matches in the handlers.

| Method | Path | Purpose |
|--------|------|---------|
| GET | `/api/v1/forge/users` | List users. Cedar evaluates `Action::"ListUser"` on each row; rows the principal can't read are filtered out before serialization. The `password_hash` field is stripped at the entity layer via `@hidden` regardless of role. |
| POST | `/api/v1/forge/users` | Create a user. Body: `{ username, password, roles, display_name? }`. Cedar evaluates `Action::"CreateUser"` against a synthetic target carrying the requested roles' computed `role_rank` — so a non-platform-admin caller cannot grant `platform_admin` (or any role outranking themselves) because the resulting principal would outrank them. |
| DELETE | `/api/v1/forge/users/:username` | Delete a user. Cedar evaluates `Action::"DeleteUser"` against the target's actual `role_rank`. Additionally refuses to delete the last `platform_admin` with `409 Conflict { error: "conflict", reason: "last_platform_admin", message: "..." }` so the instance can never be left without one. |
| POST | `/api/v1/forge/users/:username/password` | Change password. `platform_admin` may target any user; everyone else may only change their own (`sub` claim must equal `:username`). Body: `{ password }`. |

**No-upward-visibility guard**: list/create/delete are gated by the canonical role-rank rule `principal.role_rank >= resource.role_rank`. `role_rank` is computed server-side as the maximum rank in the user's `roles` list, looked up from `policies/role_ranks.toml` — `platform_admin` is hardcoded to `i64::MAX` and the loader rejects any attempt to redefine it.

**Bootstrap**: use `schema-forge bootstrap-admin --password "$ADMIN_PASSWORD"` for first-run provisioning. The bootstrap user is granted `["platform_admin"]` — not `["admin"]`. Use `schema-forge token generate ... --roles platform_admin` to mint a token with the equivalent permissions.

**Distinction**: `"admin"` is now a free string for application authors. Declaring `@access(write: ["admin"])` on a schema names an in-app role with no platform-wide privileges. Only `platform_admin` bypasses schema-/field-/tenant-level access checks and gates the `/users` endpoints.
