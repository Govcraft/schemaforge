---
name: schemaforge
description: Use when writing, creating, editing, or reviewing SchemaForge .schema files. Use when defining data models, entity schemas, field types, relations, access control, or multi-tenant hierarchies in the SchemaForge DSL syntax. Use when declaring `file` fields backed by S3-compatible storage (MinIO, AWS S3, R2, Wasabi), configuring `[schema_forge.storage]` backends, or wiring upload/download/scan flows. Use when scaffolding or regenerating the React site with `schema-forge site generate`, wiring the `/app/*` per-entity pages or the runtime-dynamic `/admin/*` shell, or iterating with the `--templates-dir` override loader. Use when querying entities via the REST API, filtering, sorting, pagination, or building query parameters. Use when declaring lifecycle hooks via @hook annotations (including `before_upload`, `after_upload`, `on_scan_complete` for file fields), scaffolding hook gRPC services with `schema-forge hooks generate`, configuring [schema_forge.hooks] bindings, or auditing hooks with `hooks list` / `hooks diff`. Use when hitting `/api/v1/forge/auth/login`, `/auth/refresh`, or `/api/v1/forge/users` for the PASETO bootstrap and user management flows.
---

# SchemaForge — Schema Authoring & CLI Guide

## Overview

SchemaForge is an Adaptive Object Model runtime with a human-readable DSL. One `.schema` file produces database tables, REST API endpoints, migrations, Cedar authorization policies, and OpenAPI specs — no recompilation required.

**Version:** 0.21.0

**Core principle:** Schemas are the single source of truth for the entire entity lifecycle.

**Database backends:** SurrealDB (default) or PostgreSQL (feature-gated, mutually exclusive).

**Object storage:** Any S3-compatible backend (MinIO in dev, AWS S3 / R2 / Wasabi / Ceph in prod) for `file` field types.

**Toolchain:** Pinned to `rustc 1.91.1` via `rust-toolchain.toml` (required by `aws-sdk-s3` 1.122).

**Workspace crates:**

| Crate | Version | Purpose |
|-------|---------|---------|
| `schema-forge-core` | 0.12.0 | Core types: schemas, fields (incl. `FieldType::File`), annotations, migrations, queries, hook events |
| `schema-forge-dsl` | 0.7.0 | Lexer/parser for `.schema` DSL (logos-based) incl. `file(...)` syntax and size literals |
| `schema-forge-backend` | 0.7.0 | Backend trait abstraction (depends on acton-service); owns the `PLATFORM_ADMIN_ROLE` constant |
| `schema-forge-surrealdb` | 0.7.1 | SurrealDB backend implementation |
| `schema-forge-postgres` | 0.5.1 | PostgreSQL backend implementation (via sqlx), incl. JSONB-backed file columns |
| `schema-forge-acton` | 0.23.0 | Axum/acton-service integration: REST API, auth, hook dispatcher, S3 storage registry (`aws-sdk-s3`); platform_admin role guards on `/users` and file scan-complete |
| `schema-forge-cli` | 0.21.0 | CLI binary (`schemaforge`) built with clap derive; routes all configuration through `acton_service::Config<SchemaForgeConfig>` (single source of truth) |

## When to Use

- Writing new `.schema` files for SchemaForge projects
- Adding entities, fields, relations, or annotations to existing schemas
- Reviewing or validating DSL syntax
- Designing multi-tenant data models with access control
- Running SchemaForge CLI commands (init, parse, apply, serve, migrate, inspect, export, policies, token, hooks, `site generate`)
- Scaffolding or regenerating the React site with `schema-forge site generate`
- Wiring `/app/*` per-entity pages (codegen'd, Preserve-mode) or iterating on the runtime-dynamic `/admin/*` admin shell
- Iterating on bundled site templates via the `--templates-dir` override loader
- Declaring lifecycle hooks on schemas with `@hook(event)` annotations
- Scaffolding, implementing, and deploying hook gRPC services generated from annotated schemas
- Configuring `[schema_forge.hooks]` bindings, timeouts, `required` flags, and descriptor paths
- Declaring `file` fields, configuring `[schema_forge.storage.backends.<name>]`, or wiring the three-endpoint upload flow (`/upload-url` → S3 PUT → `/confirm-upload`)
- Plugging in a scanner / AV / OCR pipeline via `@hook(on_scan_complete)` and the `/scan-complete` callback
- Hitting `/api/v1/forge/auth/login`, `/auth/refresh`, or `/api/v1/forge/users` for PASETO bootstrap and user management

## CLI Reference

### Global Options

All commands accept these flags:

| Flag | Env Var | Purpose |
|------|---------|---------|
| `-c, --config <PATH>` | `SCHEMA_FORGE_CONFIG` | Config file path |
| `--format <human\|json\|plain>` | — | Output format (default: human) |
| `-v, --verbose` | — | Increase verbosity (-v, -vv, -vvv) |
| `-q, --quiet` | — | Suppress non-error output |
| `--no-color` | `NO_COLOR` | Disable colored output |
| `--db-url <URL>` | `SCHEMA_FORGE_DB_URL` | Database connection URL (auto-detects backend from scheme) |
| `--db-ns <NS>` | `SCHEMA_FORGE_DB_NS` | Database namespace (SurrealDB only) |
| `--db-name <NAME>` | `SCHEMA_FORGE_DB_NAME` | Database name (SurrealDB only) |

**Backend auto-detection:** `postgres://` or `postgresql://` URLs select PostgreSQL. Everything else (ws://, wss://, mem://, http://, https://) selects SurrealDB.

### Commands

#### `schema-forge init <NAME>`

Initialize a new project directory with scaffold files.

```
schema-forge init my-project
schema-forge init my-project -t minimal    # minimal template
schema-forge init my-project -t api-only   # API-only template
schema-forge init my-project -y            # skip prompts, use defaults
schema-forge init my-project -f            # force overwrite existing dir
```

Templates: `full` (default), `minimal`, `api-only`.

#### `schema-forge parse [PATHS...]`

Parse and validate `.schema` files without applying to a database.

```
schema-forge parse                     # default: schemas/
schema-forge parse src/schemas/
schema-forge parse --print             # show round-trip DSL output
schema-forge parse --debug             # show token-level parse info
schema-forge parse --format json       # JSON output for tooling
```

#### `schema-forge apply [PATHS...]`

Parse schemas and apply to a running database backend. Computes diffs against stored metadata and runs migrations.

```
schema-forge apply                              # apply schemas/ to default backend
schema-forge apply --db-url postgres://user:pass@host/db   # PostgreSQL
schema-forge apply --db-url ws://localhost:8000  # SurrealDB
schema-forge apply --dry-run                     # show plan without executing
schema-forge apply --force                       # skip confirmation for destructive changes
schema-forge apply --with-policies               # auto-generate Cedar policies
```

#### `schema-forge migrate [PATHS...]`

Plan and optionally execute schema migrations. Dry-run by default.

```
schema-forge migrate                        # show migration plan (dry-run)
schema-forge migrate --execute              # apply the plan
schema-forge migrate --schema Contact       # plan for a specific schema only
schema-forge migrate --execute --force      # skip destructive change confirmation
```

#### `schema-forge serve`

Start the HTTP server with the SchemaForge extension via acton-service.

```
schema-forge serve                                         # default: localhost:3000
schema-forge serve -H 0.0.0.0 -p 8080                     # custom host/port
schema-forge serve --db-url postgres://user:pass@host/db   # PostgreSQL backend
schema-forge serve --db-url ws://localhost:8000             # SurrealDB backend
schema-forge serve --schemas src/schemas/                   # custom schema directory
schema-forge serve --watch                                  # hot-reload (not yet implemented)
schema-forge serve --log-level debug                        # log level override
schema-forge serve --admin-user admin --admin-password secret  # bootstrap admin credentials
```

Environment variables for admin: `FORGE_ADMIN_USER`, `FORGE_ADMIN_PASSWORD`.

The HTMX site surface was removed in commit `fdd4976`. The site UI is now a separate React + Vite + Tailwind + shadcn project generated by `schema-forge site generate` (see below). The backend serves only the REST API and auth endpoints — it does not serve HTML.

#### `schema-forge site generate`

Generates a standalone React app that talks to the running `schemaforge serve` instance via `/api/v1/forge/*`:

```
schema-forge site generate -s schemas -o site            # scaffold into ./site
schema-forge site generate --schema Order                # single schema only
schema-forge site generate --check                       # dry-run; exits non-zero on drift
schema-forge site generate --templates-dir ./site-templates  # override bundled .jinja templates
schema-forge site generate --force-user-files            # rare: re-scaffold Preserve shells too
```

Layout:

- `src/app/pages/<kebab>/{list,detail,edit}.tsx` — **Preserve** shells under `/app/*`. Thin files that import schema-driven symbols from their `.generated.tsx` sibling and compose the final page. Users own layout, charts, custom state, mutation intercepts. Scaffolded once; left alone on regen.
- `src/app/pages/<kebab>/{list,detail,edit}.generated.tsx` — **Owned** schema-driven siblings. Carry `columns`, `SORTABLE_FIELDS`, `FILTERABLE_FIELDS`, `ENUM_COLORS`, `<EntityFormFields>`, `<EntityDetailRows>`, `normalize*InitialValues`, `normalize*Payload`. Rewritten on every run so schema edits flow through automatically without touching the preserve shell. This is the #40 split — you should never need `--force-user-files` just to pick up new columns or form fields.
- `src/admin/*` — **Owned** runtime-dynamic admin shell mounted at `/admin/*`. Uses `describeSchema` + `listEntities` to render any schema the user has read access to, without per-entity codegen.
- `src/generated/*` — **Owned** typed API client, entity types, zod schemas, route manifest, formatters. Regenerated every run.
- `src/components/ui/*` — **Owned** vendored shadcn primitives (button, input, card, form, table, relation-select, error-block).
- `src/lib/auth.ts` — **Owned** PASETO token store, login, refresh scheduler.
- `Cargo.toml`-equivalent files (`package.json`, `src/main.tsx`, etc.) are **Owned**. User-land code lives in the per-entity Preserve shells under `src/app/pages/**`.

Use `--force-user-files` only when you deliberately want to reset the preserve shells back to the default scaffold — e.g. after a major template change you want to pick up, or to abandon experimental customizations. The common "I changed a schema" workflow needs no flag.

Use `--templates-dir` to shadow any `.jinja` file in the site templates tree without rebuilding the CLI; files present there override the baked-in templates. Iterate on a template, re-run `schema-forge site generate`, `pnpm dev`, and see the change immediately.

#### `schema-forge inspect [SCHEMA]`

Inspect registered schemas from the backend.

```
schema-forge inspect                    # list all schemas
schema-forge inspect Contact            # show specific schema details
schema-forge inspect Contact --detail   # detailed field information
schema-forge inspect --counts           # include entity counts per schema
schema-forge inspect --format json      # JSON output
```

#### `schema-forge export openapi [PATHS...]`

Export OpenAPI specification from schema files.

```
schema-forge export openapi                              # stdout
schema-forge export openapi -o api.json                  # write to file
schema-forge export openapi --base-path /api             # custom base path
schema-forge export openapi --spec-version 3.1.0         # OpenAPI version
```

#### `schema-forge policies list [SCHEMA]`

List generated Cedar authorization policies.

```
schema-forge policies list              # all schemas
schema-forge policies list Contact      # specific schema
```

#### `schema-forge policies regenerate [SCHEMA]`

Regenerate Cedar policy templates from schema `@access` annotations.

```
schema-forge policies regenerate                          # all schemas
schema-forge policies regenerate Contact                  # specific schema
schema-forge policies regenerate -o policies/generated/   # output directory
schema-forge policies regenerate --force                  # overwrite existing
```

#### `schema-forge hooks generate`

Scaffold a gRPC hook service (an `acton-service` Rust project) from schemas annotated with `@hook(...)`. You never hand-write the protobufs — `build.rs` compiles them and emits a `FileDescriptorSet` that SchemaForge loads at startup.

```
schema-forge hooks generate --all --schema-dir schemas --out-dir hooks-service
schema-forge hooks generate --schema Translation --out-dir translation-hooks
schema-forge hooks generate --all --regenerate     # one-shot: rewrite every Preserve file
```

- `--all` — combined project for every schema with hooks (recommended topology).
- `--schema <Name>` — per-schema project for independently-deployed services.
- `--regenerate` — full-rewrite escape hatch. Clobbers `main.rs`, `build.rs`, `src/hooks/mod.rs`, and `src/hooks/<schema>.rs` back to the default scaffold. Subsumes the legacy `--force-user-files` flag. Use only when you want to abandon customizations.

**Default mode is additive.** Adding a new `@hook`-annotated schema and re-running `schema-forge hooks generate --all` (with no flags) will splice the new schema into `src/main.rs` and `src/hooks/mod.rs` between stable `SCHEMAFORGE_HOOKS_*` marker comments, leaving every byte outside the markers untouched. Custom module imports (`mod api; mod guard;`), env-var validation, per-service constructor wiring, and hand-written `pub mod` lines all survive regen. Legacy projects (generated before the markers existed) are transparently upgraded to the marker-bounded layout on the first run under the new CLI — no user action required.

Per-schema Owned artifacts (proto files and `.prompt.md`) are always rewritten on every run regardless of flags — those are schema-derived and safe to regenerate.

Layout produced:

- **Preserve** — `Cargo.toml`, `build.rs`, `src/main.rs`, `src/hooks/mod.rs`, `src/hooks/<schema>.rs`. Written once, then user-owned. `main.rs` and `mod.rs` carry insertion markers so the generator can splice new schemas in without clobbering them. Keep the markers in place — remove them only if you want to opt out of additive updates.
- **Owned** — `proto/<schema>_hooks.proto`, `src/hooks/<schema>/<event>.prompt.md`. Rewritten on every run.

#### `schema-forge hooks list`

Enumerate every `@hook` annotation across a schema directory.

```
schema-forge hooks list --schema-dir schemas
```

#### `schema-forge hooks diff`

Compare two schema directories and report hook-level additions, removals, and intent changes. Use in CI to gate schema PRs on whether downstream hook services need regeneration.

```
schema-forge hooks diff schemas/old schemas/new
```

Markers: `+` added hook, `-` removed hook, `~` intent changed. The diff engine emits three migration steps — `AddHook`, `RemoveHook`, `ChangeHookIntent` — all **metadata-only** (no on-disk migration). The operator action is regenerating and redeploying the hook service so its proto matches the new schema shape.

#### `schema-forge token init-key`

Generate a 32-byte PASETO V4 symmetric key file.

```
schema-forge token init-key                       # default: ./keys/paseto.key
schema-forge token init-key --output /path/to/key
```

#### `schema-forge token generate`

Generate a PASETO token with specified claims.

```
schema-forge token generate --key ./keys/paseto.key --sub "user:admin" --roles platform_admin
schema-forge token generate --key ./keys/paseto.key --sub "user:jane" --roles sales,member --lifetime 7200
schema-forge token generate --key ./keys/paseto.key --sub "user:admin" --roles platform_admin --tenant-chain '[{"schema":"Organization","entity_id":"org-1"}]'
```

Use `platform_admin` for tokens that need to manage users or hit the file scan-complete endpoint. `admin` (or any other string) is just an application-defined role for use in `@access(...)` annotations and carries no platform-wide privileges.

| Flag | Default | Purpose |
|------|---------|---------|
| `--key <PATH>` | `./keys/paseto.key` | Path to symmetric key file |
| `--sub <SUBJECT>` | (required) | Subject claim, use `user:<id>` format |
| `--roles <ROLES>` | — | Comma-separated roles |
| `--lifetime <SECS>` | 3600 | Token lifetime in seconds |
| `--issuer <ISSUER>` | `schema-forge` | Issuer claim |
| `--tenant-chain <JSON>` | — | Tenant scope as JSON array |

#### `schema-forge completions <SHELL>`

Generate shell completion scripts.

```
schema-forge completions bash > ~/.bash_completion.d/schema-forge
schema-forge completions zsh > ~/.zfunc/_schema-forge
schema-forge completions fish > ~/.config/fish/completions/schema-forge.fish
```

Supported shells: `bash`, `zsh`, `fish`, `powershell`, `elvish`.

## REST API Endpoints

When running `schema-forge serve`, these routes are available:

### Core API

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

All API routes (except `/health`, `/ready`, and `/api/v1/forge/auth/login`) require a PASETO bearer token in the `Authorization` header.

### File Field Endpoints (`/api/v1/forge/schemas/:schema/entities/:id/fields/:field/*`)

Present for every `file`-typed field. The runtime never handles upload bytes — clients PUT directly to S3 using a presigned URL minted by the runtime. Downloads follow the field's `access` setting (presigned redirect or proxied stream).

| Method | Path | Purpose |
|--------|------|---------|
| POST | `.../upload-url` | Mint a presigned PUT URL. Requires `Write` access. Body: `{ filename, mime, size }`. Response: `{ upload_url, key, headers, expires_at }`. Fires `before_upload` hook (blocking). |
| POST | `.../confirm-upload` | Verify the upload landed via `HeadObject` and persist a `FileAttachment` onto the entity. Body: `{ key, checksum_sha256? }`. Transitions to `scanning` (if `on_scan_complete` hook exists) or `available`. Fires `after_upload` hook (detached). |
| GET | `.../fields/{field}` | Download. Presigned mode: 302 to signed S3 URL (`?redirect=false` returns JSON `{url}`). Proxied mode: streams bytes through the runtime, re-checking authz. Refuses with 409 unless `status == "available"`. |
| POST | `.../scan-complete` | Scanner callback. **Requires `platform_admin` role.** Body: `{ status: "available"\|"quarantined", reason? }`. Only valid from state `scanning`. Fires `on_scan_complete` hook. |

See [storage-reference.md](storage-reference.md) for the full upload flow, state machine, bucket layout, and scanner integration walkthrough.

### Auth (`/api/v1/forge/auth/*`)

| Method | Path | Purpose |
|--------|------|---------|
| POST | `/api/v1/forge/auth/login` | Exchange username+password for a PASETO token. Response body: `{ token, expires_at, roles }`. |
| POST | `/api/v1/forge/auth/refresh` | Exchange a still-valid bearer for a fresh token (same 1-hour expiry). Same response body as login. Returns 401 if no/expired token. |

The React site's `src/lib/auth.ts` stores the token in `sessionStorage`, schedules a silent refresh ~5 minutes before expiry, and retries any 401 once through `/auth/refresh` before bouncing the user back to `/login`.

### Users (`/api/v1/forge/users`)

Schema-forge-native user management. All routes require a bearer token; list/create/delete require the dedicated `platform_admin` role (the only role that gates platform-level user management — *not* the same as an in-app `"admin"` role you might use in `@access(...)` annotations). Password changes are allowed for self-edits.

| Method | Path | Purpose |
|--------|------|---------|
| GET | `/api/v1/forge/users` | List users. Requires `platform_admin`. **Non-`platform_admin` callers (once admitted) never see `platform_admin` rows in the response** — those are filtered out before serialization. |
| POST | `/api/v1/forge/users` | Create a user. Requires `platform_admin`. Body: `{ username, password, roles, display_name? }`. **Callers who don't already hold `platform_admin` cannot grant the `platform_admin` role** — that escalation path is closed at the route guard. |
| DELETE | `/api/v1/forge/users/:username` | Delete a user. Requires `platform_admin`. **Refuses to delete the last `platform_admin` with `409 Conflict { error: "conflict", reason: "last_platform_admin", message: "..." }`** so the instance can never be left without one. |
| POST | `/api/v1/forge/users/:username/password` | Change password. `platform_admin` may target any user; everyone else may only change their own (`sub` claim must equal `:username`). Body: `{ password }`. |

**Bootstrap**: when `--admin-password` (or `FORGE_ADMIN_PASSWORD`) seeds the initial user on first boot, that user is granted `["platform_admin"]` — not `["admin"]`. Use `schema-forge token generate ... --roles platform_admin` to mint a token with the equivalent permissions.

**Distinction**: `"admin"` is now a free string for application authors. Declaring `@access(write: ["admin"])` on a schema names an in-app role with no platform-wide privileges. Only `platform_admin` bypasses schema-/field-/tenant-level access checks and gates the `/users` endpoints.

## Configuration

### config.toml

Schema-forge does **not** maintain its own config layer. All runtime configuration goes through acton-service's canonical `Config<SchemaForgeConfig>` — schema-forge-specific fields live under `[schema_forge.*]` (the `T` parameter), everything else uses acton-service's standard sections.

**Discovery order**, highest priority first:
1. `--config <PATH>` flag (passes through to `acton_service::Config::load_from`)
2. Acton's XDG search: `./config.toml`, `~/.config/acton-service/schemaforge/config.toml`, `/etc/acton-service/schemaforge/config.toml`
3. `ACTON_*` env vars layer on top of whatever file was loaded (highest priority below the CLI flags)

```toml
# SurrealDB backend — a SurrealDB-flavored URL goes here.
[surrealdb]
url = "ws://localhost:8000"
namespace = "schemaforge"
database = "dev"
# Optional credentials (or set ACTON_SURREALDB_USERNAME / _PASSWORD env vars)
# username = "root"
# password = "..."

# PostgreSQL backend — uncomment to switch (mutually exclusive with [surrealdb]).
# [database]
# url = "postgres://user:pass@localhost:5432/schemaforge"
# max_connections = 50
# min_connections = 5

[token]
format = "paseto"
version = "v4"
purpose = "local"
key_path = "./keys/paseto.key"
issuer = "schemaforge"

# Storage backends for `file` field types. Each schema `file(bucket: "NAME")`
# declaration must resolve to a backend declared here, or startup fails.
[schema_forge.storage]
default_presign_ttl_secs = 300

[schema_forge.storage.backends.documents]
endpoint = "http://127.0.0.1:9100"       # MinIO in dev; omit for AWS regional
region = "us-east-1"
bucket = "forge-documents"
access_key_id = "${S3_ACCESS_KEY}"
secret_access_key = "${S3_SECRET_KEY}"
force_path_style = true                   # required for MinIO
presign_ttl_secs = 300
```

**Backend selection** is by section: `[database]` → PostgreSQL, `[surrealdb]` → SurrealDB. Declaring both is a startup error (the operator must remove one or override with `--db-url`). Neither declared falls back to a dev SurrealDB at `ws://localhost:8000` for zero-config development.

**CLI overrides on the canonical config**: `--db-url <URL>` rewrites the matching section in-place (postgres URL → `[database].url`, anything else → `[surrealdb].url`) **and clears the other section** so acton-service can never spawn a leftover pool against a different backend. `--db-ns` / `--db-name` override `[surrealdb].namespace` / `.database`. Pool-sizing knobs the operator set in config.toml (`max_connections`, retries, etc.) survive the URL override — only the URL is rewritten.

### Environment Variables

Acton-service-native overrides use the `ACTON_*` prefix (these are the canonical env vars):

| Variable | Purpose |
|----------|---------|
| `ACTON_DATABASE_URL` | Override `[database].url` (PostgreSQL) |
| `ACTON_SURREALDB_URL` | Override `[surrealdb].url` |
| `ACTON_SURREALDB_NAMESPACE` | Override `[surrealdb].namespace` |
| `ACTON_SURREALDB_DATABASE` | Override `[surrealdb].database` |
| `ACTON_SURREALDB_USERNAME` | SurrealDB credentials (replaces the removed `SCHEMA_FORGE_DB_USER`) |
| `ACTON_SURREALDB_PASSWORD` | SurrealDB credentials (replaces the removed `SCHEMA_FORGE_DB_PASS`) |

Schema-forge CLI-flag aliases (clap `env = "..."` mappings; equivalent to passing the flag):

| Variable | Equivalent flag | Purpose |
|----------|-----------------|---------|
| `SCHEMA_FORGE_DB_URL` | `--db-url` | Connection URL; backend is auto-detected from the scheme |
| `SCHEMA_FORGE_DB_NS` | `--db-ns` | SurrealDB namespace |
| `SCHEMA_FORGE_DB_NAME` | `--db-name` | SurrealDB database name |
| `SCHEMA_FORGE_CONFIG` | `--config` | Config file path |
| `FORGE_ADMIN_USER` | `--admin-user` | Seed admin username (bootstraps the PASETO login store on first run; user is granted `["platform_admin"]`) |
| `FORGE_ADMIN_PASSWORD` | `--admin-password` | Seed admin password |

> **Migration notes (v0.21.0, breaking)**:
> - The old hybrid `[database] url = "ws://..."` (URL-scheme-detected) layout is gone — move SurrealDB URLs to `[surrealdb]`, leave `[database]` for PostgreSQL.
> - The `[cli]` section (`default_schema_dir` / `default_policy_dir`) was never read at runtime; remove it.
> - `SCHEMA_FORGE_DB_USER` / `SCHEMA_FORGE_DB_PASS` env vars are removed; use `ACTON_SURREALDB_USERNAME` / `ACTON_SURREALDB_PASSWORD`.
> - The bootstrap admin user is now granted `platform_admin` (not `admin`) — see [Users](#users-apiv1forgeusers) for the role split.

## Database Backends

### SurrealDB (default feature)

The default backend. Uses WebSocket (ws://) or HTTP connections with namespace/database selection.

```
schema-forge serve --db-url ws://localhost:8000 --db-ns myapp --db-name prod
```

### PostgreSQL (postgres feature)

Available when built with `--features postgres`. Uses connection URL with embedded credentials. Creates real PostgreSQL tables with proper types, CHECK constraints, indexes, and foreign keys.

```
schema-forge serve --db-url postgres://user:pass@host:5432/dbname
```

The two backends are **mutually exclusive** at build time (enforced by acton-service). The binary ships with one or the other.

## Quick Reference — DSL Field Types

| Type | Syntax | Constraints |
|------|--------|-------------|
| Text | `text` or `text(max: N)` | max character length |
| Rich Text | `richtext` | formatted/HTML content |
| Integer | `integer` or `integer(min: M, max: N)` | min/max bounds |
| Float | `float` or `float(precision: N)` | decimal places |
| Boolean | `boolean` | none |
| DateTime | `datetime` | ISO 8601 timestamps |
| Enum | `enum("a", "b", "c")` | 1+ variants, no duplicates |
| JSON | `json` | flexible unstructured data |
| File | `file(bucket: "docs", max_size: "25MB", mime: [...], access: "presigned")` | S3-backed attachment; see [storage-reference.md](storage-reference.md) |
| Relation (one) | `-> TargetSchema` | target must be PascalCase |
| Relation (many) | `-> TargetSchema[]` | Derived inverse view if target has `-> Self` FK back (read-only); else stored array of refs. |
| Array | `text[]`, `integer[]`, etc. | `[]` suffix on primitives |
| Composite | `composite { field: type }` | nested field definitions |

## Quick Reference — Modifiers

| Modifier | Syntax | Effect |
|----------|--------|--------|
| Required | `required` | field must have a non-null value |
| Indexed | `indexed` | indexed for fast lookups |
| Default | `default(value)` | value when field omitted |

**Default value syntax:** `default("text")`, `default(42)`, `default(3.14)`, `default(true)`

## Quick Reference — Annotations

### Schema-Level (before `schema` keyword)

| Annotation | Syntax | Purpose |
|------------|--------|---------|
| Version | `@version(N)` | schema version (positive integer) |
| Display | `@display("field_name")` | primary display field |
| System | `@system` | protected system schema |
| Tenant Root | `@tenant(root)` | multi-tenant root entity |
| Tenant Child | `@tenant(parent: "ParentSchema")` | scoped to parent tenant |
| Access | `@access(read: [...], write: [...], delete: [...])` | role-based access control |
| Dashboard | `@dashboard(widgets: [...], layout: "...", ...)` | dashboard configuration |
| Hook | `@hook(event) """intent"""` | declare a lifecycle hook (see hooks-reference.md) |

### Field-Level (after modifiers on a field line)

| Annotation | Syntax | Purpose |
|------------|--------|---------|
| Owner | `@owner` | record ownership tracking |
| Widget | `@widget("type")` | UI widget hint (closed 17-token vocabulary) |
| Kanban Column | `@kanban_column` | kanban grouping column |
| Format | `@format("type")` | display format (closed 7-token vocabulary) |
| Field Access | `@field_access(read: [...], write: [...])` | field-level access control |
| List Hint | `@list(primary\|column\|hidden)` | list-view column curation |
| Enum Colors | `@enum_colors(variant: "color", ...)` | semantic color tokens per enum variant |

**New in v0.17.0:**

- `@list(hint)` drives the generated list page. Resolution ladder: explicit hint wins → the `@display("...")` field auto-promotes to `primary` when no explicit primary is declared → `rich_text`, `composite`, `array`, `relation_one`, `relation_many`, and `json` fields default to `hidden` → everything else defaults to `column`. At most one `@list(primary)` per schema (parse error otherwise). `@list(column)` on a relation field opts it back in to list display and the generator renders the resolved `<field>__display` label as a linked cell.
- `@enum_colors(...)` maps enum variant names to a closed color vocabulary: `neutral`, `gray`, `red`, `amber`, `green`, `blue`, `purple`, `violet`, `teal`, `rose`. Only allowed on enum fields; every key must match an existing variant (parse error otherwise). Drives the generated `EnumBadge` component in `list.tsx` with Tailwind classes per token.

## Quick Reference — Lifecycle Hooks

Hooks let schemas call out to an external gRPC service at well-defined lifecycle events. The implementation lives in a separate `acton-service` project — SchemaForge itself only dispatches. Three properties matter:

- **Declared in the schema.** Adding `@hook(event)` is the only change you make inside SchemaForge.
- **Typed per-schema wire format.** `schema-forge hooks generate` emits a proto whose fields match the schema exactly — no untyped JSON envelope.
- **Zero cost when unused.** Schemas without `@hook` annotations pay no dispatcher overhead; read-side hooks early-exit on a per-event check.

### Declaring a hook

```schema
@hook(before_change) """Normalize source_text and call the external translation API"""
@hook(after_change) """Publish a translation.completed event to NATS"""
schema Translation {
    source_text: text required
    translated_text: text
    language: text
    created_at: datetime
}
```

The intent string is natural-language documentation baked into generated stubs and `.prompt.md` files — it is not executed code. A schema may declare multiple `@hook` lines (one per event); declaring the same event twice is a parse error. Hooks are **opt-in per event** — SchemaForge only dispatches for events that appear on the schema.

### Lifecycle events

| Event (DSL) | Fires on | Blocking? | May abort? | May modify? |
|---|---|---|---|---|
| `before_change` | POST/PUT | yes | yes | yes |
| `after_change` | POST/PUT | no (fire-and-forget) | no | no |
| `before_delete` | DELETE | yes | yes | n/a |
| `after_delete` | DELETE | no (fire-and-forget) | no | n/a |
| `before_read` | GET one, GET list, POST query | yes | yes | n/a |
| `after_read` | GET one | yes | yes | yes |
| `before_upload` | `POST /upload-url` (file fields) | yes | yes | n/a |
| `after_upload` | `POST /confirm-upload` (file fields) | no (detached) | no | n/a |
| `on_scan_complete` | `POST /scan-complete` (file fields) | no (detached) | no | n/a |
| `before_validate` | *reserved* — parses but not yet dispatched | — | — | — |

Use `before_change` for pre-persistence logic until `before_validate` is wired in. For async work use the corresponding `after_*` event — fire-and-forget failures are logged, never reach the client, and the entity is already committed when they fire. For file-field scanners, use `after_upload` to run AV/OCR against the presigned `download_url` in the request, then post the verdict back via the `/scan-complete` endpoint (which in turn fires `on_scan_complete`).

### Workflow summary

1. Annotate schemas with `@hook(event) """intent"""`.
2. `schema-forge hooks generate --all --schema-dir schemas --out-dir hooks-service`.
3. Implement each stub in `src/hooks/<schema>.rs`. Return `abort_reason: Some(...)` to reject (becomes a 422 `hook_aborted`); set optional response fields to overwrite the entity before persistence.
4. `cargo run` the hook service on its own port.
5. Configure `[schema_forge.hooks]` with `enabled = true` and a `[[schema_forge.hooks.bindings]]` entry per `(schema, event)` pair (see config fragment below).
6. Restart SchemaForge; startup logs `Hook dispatcher initialized with N binding(s)`.

### Config fragment

```toml
[schema_forge.hooks]
enabled = true
default_timeout_ms = 5000
max_concurrent_async = 100

[[schema_forge.hooks.bindings]]
schema = "Translation"
event = "BeforeChange"              # PascalCase in config, snake_case in DSL
endpoint = "http://hooks-service:9090"
required = true
descriptor_path = "/var/lib/schemaforge/hooks_descriptor.bin"
```

- `required = true`: transport failures (timeout, unreachable) fail the CRUD request (503 `hook_timeout` / `hook_unavailable`).
- `required = false`: transport failures are logged and the operation proceeds.
- **Explicit aborts always propagate** regardless of `required` — a returned `abort_reason` is always a 422.
- `descriptor_path` must point to the `FileDescriptorSet` binary the scaffold's `build.rs` emits (available via `HOOKS_DESCRIPTOR_PATH` build-env). SchemaForge validates bindings at startup and fails fast if descriptors are missing or don't contain the expected `{Schema}Hooks` service.

### Common pitfalls

| Mistake | Fix |
|---|---|
| `@hook(BeforeChange)` in DSL | Use `snake_case` (`before_change`) in `.schema` files |
| `event = "before_change"` in config | Use PascalCase (`BeforeChange`) in `config.toml` |
| Using `--regenerate` to pick up a new schema | Don't. Additive default splices it in — `--regenerate` wipes your customizations |
| Removing the `SCHEMAFORGE_HOOKS_*` marker comments from `main.rs` / `mod.rs` | You'll silently opt out of additive updates and get a "markers missing" warning on the next run |
| Expecting `after_change` to block writes | It's fire-and-forget — use `before_change` for anything load-bearing |
| Deploying schema change without regenerating hooks | Schema field additions change request messages — rerun `hooks generate` and redeploy the hook service before rolling the schema forward |
| Empty response body on fire-and-forget | Correct — `after_*` response messages are empty by design |

For the full walkthrough — dispatch flow diagrams, wire format contract (service/method naming, field tag layout, DSL→proto type mapping), the complete failure-mode matrix, observability/log lines, and hook migration semantics — see [hooks-reference.md](hooks-reference.md).

## Core Pattern — Minimal Schema

```
schema Contact {
    name:    text(max: 255) required indexed
    email:   text required indexed
    phone:   text
    active:  boolean default(true)
}
```

Every schema needs:
- `schema` keyword + PascalCase name + `{ fields }`
- At least one field
- Fields use snake_case names

## What SchemaForge Generates

From a `.schema` file, SchemaForge produces:

1. **Database tables** — DDL matching the backend (PostgreSQL: `CREATE TABLE` with constraints; SurrealDB: `DEFINE TABLE` + `DEFINE FIELD`)
2. **REST API routes** — CRUD endpoints at `/api/v1/forge/schemas/{schema}/entities`
3. **Migrations** — diff-based, atomic steps with safety classification
4. **Cedar policies** — role-based authorization from `@access` annotations
5. **OpenAPI spec** — dynamic generation from schema registry

## Common Mistakes

| Mistake | Fix |
|---------|-----|
| `schema contact` (lowercase) | `schema Contact` (PascalCase) |
| `firstName: text` (camelCase) | `first_name: text` (snake_case) |
| `enum()` (empty variants) | `enum("a", "b")` (at least 1 variant) |
| `integer(min: 100, max: 50)` | `integer(min: 50, max: 100)` (min <= max) |
| `@version(0)` | `@version(1)` (must be >= 1) |
| Missing `{}` after schema name | `schema Name { ... }` |
| Field without type | `name: text` (colon + type required) |
| `-> contact` (lowercase relation) | `-> Contact` (PascalCase target) |
| Entity body without `fields` wrapper | `{"fields": {"name": "value"}}` |

## Additional Resources

For complete details, load these supporting files:

- For **complete syntax reference** including EBNF grammar, all annotation parameters, and constraint details, see [dsl-reference.md](dsl-reference.md)
- For **annotated real-world examples** covering CRM, multi-tenant, project management, and HR domains, see [examples.md](examples.md)
- For **design patterns** including multi-tenancy, access control, dashboards, composites, relations, and widget selection, see [patterns.md](patterns.md)
- For the **React site generator** — `schema-forge site generate` workflow, Preserve vs Owned files, `/app/*` vs `/admin/*` route trees, `--templates-dir` override loader, and the PASETO login flow — see `docs/site-guide.md` in the schemaforge repo
- For **query API** including filtering, sorting, pagination, query-string operators, JSON body queries, type coercion, and access control, see [query-api-reference.md](query-api-reference.md)
- For **lifecycle hooks** including the `@hook` annotation, all lifecycle events, dispatch flow diagrams, the hook-service scaffold layout, wire format contract, config bindings, failure-mode matrix, and `hooks list` / `hooks diff` evolution, see [hooks-reference.md](hooks-reference.md)
- For **`file` fields and S3 storage** — DSL syntax (`bucket`, `max_size`, `mime`, `access`), `[schema_forge.storage]` config for MinIO / AWS / R2 / Wasabi, the three-endpoint upload flow, `pending → uploaded → scanning → available|quarantined|rejected` state machine, scanner integration via `before_upload` / `after_upload` / `on_scan_complete` hooks and the `/scan-complete` callback, bucket layout, and operational failure modes, see [storage-reference.md](storage-reference.md)
