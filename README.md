# SchemaForge

**Define your data model once. Get migrations, CRUD endpoints, and API docs automatically.**

SchemaForge is a Rust toolkit that turns human-readable schema definitions into fully operational backends -- no recompilation required. Write a schema file (or describe what you need in plain English), and SchemaForge generates database tables, CRUD API endpoints, migrations, authorization policies, and API documentation at runtime.

```
schema Contact {
    name:       text(max: 255) required indexed
    email:      text(max: 512) required indexed
    phone:      text
    priority:   enum("low", "medium", "high") default("medium")
    company:    -> Company
    tags:       text[]
    notes:      richtext
    is_active:  boolean default(true)
}
```

From this single file, SchemaForge generates:

- **Database tables** with type enforcement and constraints (SurrealDB)
- **REST API routes** with input validation for every entity
- **Migration plans** that diff against your existing schema
- **Cedar authorization policies** for access control
- **OpenAPI specifications** that stay in sync with your schemas

## Try it in 2 minutes

The repo ships a self-contained demo: an in-memory backend with twelve seeded entities and a React admin UI. There is no database to install.

### Prerequisites

| Tool | Why it's needed |
|---|---|
| [Rust 1.75+](https://rustup.rs) | The demo builds the CLI from source |
| [Task](https://taskfile.dev/installation/) | Runs the bundled demo recipes |
| [pnpm](https://pnpm.io/installation) + Node 20+ | Builds and serves the React admin |

### Run the demo

```bash
git clone https://github.com/Govcraft/schemaforge
cd schemaforge

task demo        # builds the CLI, starts an in-memory backend on :3000, seeds 12 entities
```

In a second terminal:

```bash
task site:dev    # regenerates the React admin and serves it on :5173
```

Open <http://localhost:5173> and sign in with `admin` / `changeme`.

That's the whole demo. The backend uses an embedded in-memory store, so nothing persists between runs; press Ctrl+C in the `task demo` terminal to stop. When you're ready to point SchemaForge at SurrealDB or PostgreSQL, see [Install for a Real Project](#install-for-a-real-project).

## Table of Contents

- [Try it in 2 minutes](#try-it-in-2-minutes)
- [Why SchemaForge](#why-schemaforge)
- [Install for a Real Project](#install-for-a-real-project)
- [Architecture](#architecture)
- [SchemaDSL Reference](#schemadsl-reference)
- [CLI Reference](#cli-reference)
- [AI Agent](#ai-agent)
- [Programmatic Usage](#programmatic-usage)
- [Federal Deployment](#federal-deployment)
- [Project Status](#project-status)
- [Design Decisions](#design-decisions)
- [Contributing](#contributing)

## Why SchemaForge

Traditional backend development requires you to define your model in code, write a migration, build CRUD handlers, add validation, wire up authorization, and generate API docs -- separately, for every entity. When a schema changes, you repeat the cycle.

SchemaForge collapses that workflow. One schema file is the single source of truth for your entire entity lifecycle. Change the schema, and everything downstream updates automatically: migrations are computed by diffing versions, routes adapt, validation adjusts, and the OpenAPI spec regenerates.

The AI agent takes this further. Describe what you need in plain English, and an LLM generates the schema, validates it through the tool execution loop (self-correcting any errors), and applies it to your database -- all without writing DSL by hand.

## Install for a Real Project

This section walks through running SchemaForge against a real SurrealDB or PostgreSQL instance: install the binary, scaffold a project, define a schema, and serve it. If you only want to kick the tires, the [self-contained demo](#try-it-in-2-minutes) above is faster.

### Prerequisites

- A running SurrealDB 2.x or PostgreSQL 14+ instance (SurrealDB embedded mode works for development)
- Rust 1.75+ only if you intend to build from source

### Install the Prebuilt Binary

Each release ships a single statically-linked `schemaforge` binary for Linux x86_64, built against exactly one database backend. Pick the flavor that matches your target database and run the matching one-liner:

```bash
# PostgreSQL build
TAG=$(curl -fsSL https://api.github.com/repos/Govcraft/schemaforge/releases/latest | grep '"tag_name"' | cut -d'"' -f4) && \
  curl -fsSL "https://github.com/Govcraft/schemaforge/releases/download/${TAG}/schemaforge-${TAG}-x86_64-unknown-linux-gnu-postgres.tar.gz" \
  | sudo tar -xz -C /usr/local/bin
```

```bash
# SurrealDB build
TAG=$(curl -fsSL https://api.github.com/repos/Govcraft/schemaforge/releases/latest | grep '"tag_name"' | cut -d'"' -f4) && \
  curl -fsSL "https://github.com/Govcraft/schemaforge/releases/download/${TAG}/schemaforge-${TAG}-x86_64-unknown-linux-gnu-surrealdb.tar.gz" \
  | sudo tar -xz -C /usr/local/bin
```

Each command downloads the latest release tarball, extracts the `schemaforge` binary into `/usr/local/bin`, and leaves it immediately runnable. To install somewhere on your `PATH` without `sudo`, swap `/usr/local/bin` for a user-writable directory such as `~/.local/bin`.

The backend is compiled in — there is no runtime flag to switch between PostgreSQL and SurrealDB. If you need both, download both tarballs and rename the binaries (e.g. `schemaforge-pg`, `schemaforge-surreal`).

Verify the install:

```bash
schemaforge --version
```

### Verify Release Authenticity

Every release ships a `SHA256SUMS` manifest plus a Sigstore keyless signature (`SHA256SUMS.sig` + `SHA256SUMS.pem`). The signing certificate is bound via OIDC to the `release.yml` workflow on a tag in this repo and logged to the public Rekor transparency log — no long-lived signing keys are involved.

For production or audited deployments, verify before running the binary:

```bash
TAG=$(curl -fsSL https://api.github.com/repos/Govcraft/schemaforge/releases/latest | grep '"tag_name"' | cut -d'"' -f4)
BASE="https://github.com/Govcraft/schemaforge/releases/download/${TAG}"

# 1. Download the tarball you want plus the manifest and signature
curl -fsSLO "${BASE}/schemaforge-${TAG}-x86_64-unknown-linux-gnu-postgres.tar.gz"
curl -fsSLO "${BASE}/SHA256SUMS"
curl -fsSLO "${BASE}/SHA256SUMS.sig"
curl -fsSLO "${BASE}/SHA256SUMS.pem"

# 2. Verify the checksum matches
sha256sum --ignore-missing -c SHA256SUMS

# 3. Verify the signature was produced by this repo's release workflow
cosign verify-blob \
  --certificate SHA256SUMS.pem \
  --signature SHA256SUMS.sig \
  --certificate-identity-regexp "^https://github.com/Govcraft/schemaforge/\\.github/workflows/release\\.yml@refs/tags/" \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  SHA256SUMS
```

A successful run prints `Verified OK`. The `cosign` binary is available from [sigstore/cosign](https://github.com/sigstore/cosign/releases) (single static binary, no daemon).

### Build from Source (alternative)

If you need a different target triple, or want to track `main`:

```bash
# PostgreSQL build
cargo install --git https://github.com/Govcraft/schemaforge schema-forge-cli \
  --no-default-features --features postgres

# SurrealDB build
cargo install --git https://github.com/Govcraft/schemaforge schema-forge-cli \
  --no-default-features --features surrealdb

# FIPS-validated PostgreSQL build (federal deployments)
cargo install --git https://github.com/Govcraft/schemaforge schema-forge-cli \
  --no-default-features --features postgres,fips
```

#### FIPS builds

The `fips` feature routes rustls through `aws-lc-rs` compiled against the
FIPS-validated AWS-LC C library (`aws-lc-fips-sys`). When enabled,
SchemaForge installs `aws_lc_rs` as the process-wide rustls
[`CryptoProvider`] at startup so every TLS-using subsystem — sqlx
(PostgreSQL), reqwest (S3), tonic (hook dispatcher) — terminates through
the FIPS module.

Build requirements:

- CMake 3.10+
- **Clang 14–18** (FIPS module integrity checks reject the relocation
  sections clang ≥19 emits; AWS-LC-FIPS 3.0.x targets clang ≤18). Point
  cargo at the right compiler with `CC=/path/to/clang-18
  CXX=/path/to/clang++-18` for the build.
- Perl
- Go 1.18+ (required by AWS-LC's `delocate` tool when targeting FIPS)
- Supported platform per [aws-lc-fips-sys](https://github.com/aws/aws-lc-rs/tree/main/aws-lc-fips-sys)

Note: the `surrealdb` backend pulls `rustls` with `ring` transitively and
is **not** suitable for FIPS deployments. Pair `fips` with `postgres`.

[`CryptoProvider`]: https://docs.rs/rustls/0.23/rustls/crypto/struct.CryptoProvider.html

### Scaffold a Project

```bash
schemaforge init my-platform
cd my-platform
```

This creates:

```
my-platform/
├── Cargo.toml
├── config.toml
├── acton-ai.toml
├── schemas/
├── policies/
│   ├── generated/
│   └── custom/
└── src/
    └── main.rs
```

The generated `config.toml` is seeded with `[schema_forge] project_name = "my-platform"` — the human-facing name shown to users in invitation emails and used as the default email `From` display-name. Edit it to whatever your users should recognize.

### Define a Schema

Create a file at `schemas/crm.schema`:

```
@version(1)
@display("name")
schema Company {
    name:            text(max: 255) required indexed
    website:         text(max: 500)
    industry:        enum("fintech", "saas", "healthcare", "other")
    employee_count:  integer(min: 1)
    address:         composite {
        street:      text
        city:        text required
        state:       text
        zip:         text
        country:     text required
    }
}

@version(1)
@display("email")
schema Contact {
    first_name:      text(max: 100) required
    last_name:       text(max: 100) required
    email:           text(max: 255) required indexed
    phone:           text(max: 20)
    status:          enum("active", "inactive", "lead") default("lead")
    company:         -> Company
    tags:            text[]
    notes:           richtext
}
```

### Validate, Apply, and Serve

```bash
# Parse and validate your schemas
schemaforge parse schemas/

# Apply schemas to SurrealDB (creates tables, fields, indexes)
schemaforge apply schemas/ --db-url ws://localhost:8000

# Preview migration steps without applying
schemaforge apply schemas/ --dry-run

# Start the API server with dynamic CRUD routes
schemaforge serve --schemas schemas/ --db-url ws://localhost:8000 --db-ns app --db-name main
```

Once served, every registered schema automatically gets REST endpoints:

```
POST   /forge/entities/contact        Create a contact
GET    /forge/entities/contact        List/query contacts
GET    /forge/entities/contact/:id    Get a contact by ID
PUT    /forge/entities/contact/:id    Update a contact
DELETE /forge/entities/contact/:id    Delete a contact

GET    /forge/schemas                 List all schemas
GET    /forge/openapi.json            Dynamic OpenAPI specification
```

The paths above use the unversioned `/forge/...` prefix. The `schemaforge entity` CLI calls the same routes under the versioned base `/api/v1/forge/...`; see [Drive a running instance over HTTP](#drive-a-running-instance-over-http) below.

### Generate Schemas with AI

Instead of writing DSL by hand, describe what you need:

```bash
# One-shot generation
schemaforge generate "A ticketing system with tickets linked to contacts,
    priority levels, status tracking, and assignment" --batch -o schemas/ticketing.schema

# Interactive conversational mode
schemaforge generate
```

The AI agent calls `list_schemas` to see what already exists, generates DSL, calls `validate_schema` to check correctness, fixes any errors automatically, and applies the result after confirmation. No custom retry logic -- the LLM's tool execution loop handles self-correction naturally.

### Drive a running instance over HTTP

`schemaforge login` and the `schemaforge entity` subcommands are the ergonomic way to call the entity REST API that `schemaforge serve` exposes. Unlike `apply`, `inspect`, and `migrate` -- which connect straight to the database via `--db-url` -- these speak HTTP to a *running* server and authenticate with a Bearer PASETO token. They replace the hand-built `curl` plus offline-token workflow with typed input, real output formats, and stable exit codes you can branch on.

```bash
# 1. Authenticate once; the token is cached and reused automatically.
schemaforge login --server https://forge.agency.gov -u alice

# 2. Create an entity with typed fields (numbers, booleans, and JSON are coerced).
schemaforge entity create Contact --server https://forge.agency.gov \
  --set first_name=Alice --set last_name=Stone --set 'tags:=["vip"]'

# 3. List, filter, and sort.
schemaforge entity list Contact --server https://forge.agency.gov \
  --eq status=active --sort -created_at --limit 25
```

The full command surface, token-source precedence, output formats, exit codes, and the `[schema_forge.client]` config section are documented in [`docs/entity-cli-reference.md`](docs/entity-cli-reference.md).

## Architecture

SchemaForge is a Cargo workspace of seven composable crates. Each layer depends only on the layers below it.

```
                            ┌─────────────────┐
  "I need a CRM..."  ─────>│ schema-forge-ai  │ LLM agent + tools
                            └────────┬────────┘
                                     │ generates DSL
                            ┌────────▼────────┐
  .schema files  ──────────>│ schema-forge-dsl │ lexer + parser + printer
                            └────────┬────────┘
                                     │ produces SchemaDefinition
                            ┌────────▼────────┐
                            │schema-forge-core │ types, validation, migration, query IR
                            └────────┬────────┘
                                     │ implements traits
  ┌─────────────────┐       ┌────────▼────────┐
  │schema-forge-acton│──────>│schema-forge-    │ SchemaBackend + EntityStore traits
  │ HTTP routes      │       │backend          │
  └─────────────────┘       └────────┬────────┘
                                     │
  ┌─────────────────┐       ┌────────▼────────┐
  │ schema-forge-cli│       │schema-forge-    │ SurrealQL codegen + CRUD
  │ commands         │──────>│surrealdb        │
  └─────────────────┘       └─────────────────┘
```

| Crate | Purpose |
|-------|---------|
| `schema-forge-core` | Runtime type system, validation, migration planner, query IR. Zero I/O, pure logic. |
| `schema-forge-dsl` | Lexer (logos) and recursive descent parser for `.schema` files, plus a printer for round-trip fidelity. |
| `schema-forge-backend` | `SchemaBackend` and `EntityStore` trait definitions. Storage-agnostic interface. |
| `schema-forge-surrealdb` | SurrealDB implementation: MigrationStep to SurrealQL compilation, entity CRUD, query translation. |
| `schema-forge-postgres` | PostgreSQL implementation: DDL codegen, entity CRUD, query translation via SQLx. |
| `schema-forge-acton` | Axum-based JSON API layer: dynamic CRUD routes, auth/login, Cedar policies, OpenAPI spec, schema registry. |
| `schema-forge-cli` | Command-line interface: `init`, `parse`, `apply`, `migrate`, `generate`, `serve`, `inspect`, `export`, `policies`. |

### Core Type System

The foundational types in `schema-forge-core` model schemas with validated newtypes:

- **SchemaName** -- PascalCase identifier (e.g., `Contact`, `OrderItem`)
- **FieldName** -- snake_case identifier (e.g., `first_name`, `email_address`)
- **SchemaVersion** -- Positive integer, auto-incremented
- **SchemaId** -- TypeID-based unique identifier (UUIDv7)
- **FieldType** -- The complete type system: `Text`, `RichText`, `Integer`, `Float`, `Boolean`, `DateTime`, `Enum`, `Json`, `Relation`, `Array`, `Composite`
- **FieldModifier** -- `Required`, `Indexed`, `Default(value)`

All types derive `Serialize`/`Deserialize` and use `#[non_exhaustive]` for forward compatibility.

### Migration Engine

The `DiffEngine` compares two schema versions and produces a `MigrationPlan` -- an ordered list of atomic `MigrationStep` operations:

| Step | Safety Level |
|------|-------------|
| `CreateSchema`, `AddField`, `AddIndex`, `AddRelation` | Safe |
| `RenameField`, `ChangeType`, `AddRequired` | Requires confirmation |
| `DropSchema`, `RemoveField`, `RemoveRelation` | Destructive |

Each step carries a safety classification. The CLI shows the migration plan and prompts for confirmation before executing destructive steps.

Type changes include automatic value transforms where possible (integer to float, any scalar to string) and fall back to `SetNull` for incompatible conversions.

### Query IR

A storage-agnostic `Filter` enum compiles to native backend queries. It supports comparison operators (`Eq`, `Ne`, `Gt`, `Gte`, `Lt`, `Lte`), string operations (`Contains`, `StartsWith`), set membership (`In`), and logical combinators (`And`, `Or`, `Not`).

`FieldPath` enables dotted notation for relation traversal. The query `company.industry = "fintech"` traverses the `company` relation and filters on the `industry` field -- translated to native SurrealDB dot-notation without JOINs.

### SurrealDB Backend

SurrealDB is the primary backend. Its data model aligns naturally:

| SchemaForge Concept | SurrealDB Equivalent |
|---------------------|---------------------|
| `SchemaDefinition` | `DEFINE TABLE` + `DEFINE FIELD` statements |
| `FieldType::Text` | `TYPE string` with assertion on length |
| `FieldType::Enum` | `TYPE string` with `ASSERT $value IN [...]` |
| `FieldType::Relation` (one) | `TYPE option<record<Target>>` |
| `FieldType::Relation` (many) | `TYPE option<array<record<Target>>>` |
| `FieldType::Composite` | `TYPE object` with nested `DEFINE FIELD` |
| `FieldType::Json` | `FLEXIBLE TYPE object` |
| Relation traversal | Native dot-notation (no JOINs) |

The embedded SurrealDB mode (`kv-mem`) enables development and testing without running a separate database process.

## SchemaDSL Reference

### Field Types

| Type | Syntax | Constraints |
|------|--------|-------------|
| Text | `text` or `text(max: 255)` | `min`, `max` character length |
| Rich Text | `richtext` | `min`, `max` character length |
| Integer | `integer` or `integer(min: 0, max: 100)` | `min`, `max` bounds |
| Float | `float` or `float(precision: 2)` | `precision` (decimal places) |
| Boolean | `boolean` | None |
| DateTime | `datetime` | None |
| Enum | `enum("a", "b", "c")` | At least 1 variant, no duplicates |
| Relation (one) | `-> SchemaName` | Target must be PascalCase |
| Relation (many) | `-> SchemaName[]` | Target must be PascalCase. See *Inverse collections* below. |
| Array | `text[]`, `integer[]`, etc. | Suffix `[]` on any field type |
| Composite | `composite { field: type ... }` | Nested field definitions |
| JSON | `json` | Arbitrary unstructured data |

### Inverse Collections (`-> X[]`)

A collection relation on a parent (`documents: -> Document[]`) is resolved
as an **inverse view** when the target schema declares a foreign-key
relation pointing back:

```
schema Opportunity {
    title:     text required
    documents: -> Document[]       // derived — no physical column
}

schema Document {
    title:        text required
    opportunity:  -> Opportunity   // FK on the child side
}
```

- Reads of `Opportunity.documents` query `Document` filtered by the
  `opportunity` FK and return the matching child IDs (plus `__display`
  entries via the usual display-field machinery). Parents with no
  children get an empty array — never `null`.
- Writes to `Opportunity.documents` are rejected with `422`. Persist
  the relationship by setting `Document.opportunity` on the child.
- Migrations for a paired field emit **no** column: nothing to backfill,
  nothing to drop, no drift.

If the target schema has **no** FK pointing back, `-> X[]` keeps its
older behavior as a stored `TEXT[]` / `array<record<X>>` column — use
that for many-to-many / tag-style lists where both sides are
independent.

Exactly one FK back is required. Two FKs from the same child pointing at
the same parent is rejected at schema-load time with an *ambiguous
inverse* error — disambiguate by removing the duplicate FK (SchemaForge
does not currently support `@inverse(field: "...")` to pick between
competing FKs).

### Modifiers

| Modifier | Effect |
|----------|--------|
| `required` | Field must have a non-null value |
| `indexed` | Field is indexed for fast lookups |
| `default(value)` | Sets a default when the field is omitted |

### Annotations

Annotations appear before the `schema` keyword:

```
@version(2)
@display("email")
schema Contact { ... }
```

- `@version(N)` -- Declares the schema version (positive integer).
- `@display("field_name")` -- Identifies the display field for the schema.

### Naming Conventions

- **Schema names** must be PascalCase: `Contact`, `OrderItem`, `UserProfile`
- **Field names** must be snake_case: `first_name`, `email_address`, `created_at`

### Grammar (EBNF)

```ebnf
program        = { schema_def } ;
schema_def     = { annotation } "schema" PASCAL_IDENT "{" { field_def } "}" ;
field_def      = SNAKE_IDENT ":" field_type { modifier } ;

field_type     = primitive_type [ "[]" ]
               | "->" PASCAL_IDENT [ "[]" ]
               | "composite" "{" { field_def } "}"
               ;

primitive_type = "text" [ "(" text_params ")" ]
               | "richtext" [ "(" text_params ")" ]
               | "integer" [ "(" int_params ")" ]
               | "float" [ "(" float_params ")" ]
               | "boolean"
               | "datetime"
               | "enum" "(" enum_variants ")"
               | "json"
               ;

modifier       = "required" | "indexed" | "default" "(" value ")" ;
annotation     = "@version" "(" INTEGER ")"
               | "@display" "(" STRING ")"
               | "@system"
               | "@access" "(" named_string_lists ")"
               | "@tenant" "(" tenant_kind ")"
               | "@dashboard" "(" dashboard_params ")"
               | "@webhook" [ "(" webhook_params ")" ]
               ;

webhook_params = "events" ":" string_list
                 [ "," "url" ":" STRING [ "," "secret" ":" STRING ] ] ;
```

## CLI Reference

```
schemaforge <command> [options]
```

| Command | Description |
|---------|-------------|
| `init <name>` | Scaffold a new project (`--template minimal\|full\|api-only`) |
| `parse <paths>` | Validate `.schema` files and show diagnostics (`--print` for round-trip output) |
| `apply <paths>` | Apply schemas to the backend (`--dry-run`, `--force`, `--with-policies`) |
| `migrate <paths>` | Show migration plan (`--execute` to apply, `--schema` for a specific schema) |
| `generate [desc]` | Generate schemas from natural language (`--batch`, `--provider`, `--model`) |
| `serve` | Start HTTP server with dynamic routes (`--host`, `--port`, `--watch`) |
| `login` | Authenticate against a running instance and cache a Bearer token (`--username`, `--password-stdin`, `--print-token`) |
| `entity <verb>` | Call entity REST endpoints on a running instance: `list`, `get`, `create`, `replace`, `patch`, `delete`, `query` (see [`docs/entity-cli-reference.md`](docs/entity-cli-reference.md)) |
| `inspect [schema]` | Show registered schemas and details (`--detail`, `--counts`) |
| `export openapi` | Export OpenAPI spec (`-o file`) |
| `policies list` | List Cedar authorization policies |
| `policies regenerate` | Regenerate Cedar policy templates (`--force`) |
| `sign <paths>` | Sign schemas (`--ed25519-key`, `--ed25519-generate`, `--ssh-key`, `--ssh-principal`, `--keyless`, `--print-pubkey`). Produces per-file `.sig`s and a signed `schemas.manifest.toml`. |
| `verify <paths>` | Verify schemas against the configured trust policy without applying anything. Suitable as a pre-merge CI gate. |
| `trust-bundle refresh` | Fetch the Sigstore TUF trust-root snapshot (online TUF) and write it to disk. Used to seed `trust_root_bundle` in airgap deployments. Flags: `--output`, `--instance`, `--force`. |
| `trust-bundle inspect <path>` | Print a one-line fulcio/rekor/TSA cert-count summary for a trust-root JSON file. Sanity check after copying a refreshed bundle. |
| `completions <shell>` | Generate shell completions (bash, zsh, fish, powershell, elvish) |

### Global Options

| Option | Description |
|--------|-------------|
| `-c, --config <path>` | Configuration file path (env: `SCHEMA_FORGE_CONFIG`) |
| `--format human\|json\|plain` | Output format (default: `human`) |
| `-v, --verbose` | Increase verbosity (`-v`, `-vv`, `-vvv`) |
| `-q, --quiet` | Suppress non-error output |
| `--no-color` | Disable colored output (env: `NO_COLOR`) |
| `--db-url <url>` | SurrealDB connection URL (env: `SCHEMA_FORGE_DB_URL`) |
| `--db-ns <name>` | SurrealDB namespace (env: `SCHEMA_FORGE_DB_NS`) |
| `--db-name <name>` | SurrealDB database name (env: `SCHEMA_FORGE_DB_NAME`) |
| `--trust-policy <path>` | Standalone trust-policy TOML; overrides `[schema_forge.signing]` (env: `SCHEMAFORGE_TRUST_POLICY`) |
| `--no-verify` | Skip schema signature verification (refused under `signing.mode = "enforce"` unless `SCHEMAFORGE_ALLOW_NO_VERIFY=1`) |

> `login` and the `entity` subcommands do not use `--db-url`. They speak HTTP to a running instance and take their own connection flags -- `--server`, `--token-file` / `--token-stdin`, `--ca-cert`, `--insecure`, `--timeout` -- documented in [`docs/entity-cli-reference.md`](docs/entity-cli-reference.md).

### Signed-Schema Enforcement

SchemaForge can require that every `.schema` file under `schemas/` be
cryptographically signed by an operator-controlled key before any
command will read it. The control closes two threat classes that an
unsigned schema directory leaves open: on-disk tampering of any file,
and untrusted-author additions made by dropping a `.schema` into the
directory.

> **Full reference:** [`docs/signing-reference.md`](docs/signing-reference.md) — TOML schema for every signer kind, full CLI flag tables, manifest format spec, rollout playbook, and airgap/SCIF workflow.

The trust policy lives under `[schema_forge.signing]` in `config.toml`
(or in a standalone file via `--trust-policy`). Three modes:

- `off` — default; skip verification (preserves pre-signing behaviour).
- `warn` — run all checks, log failures, keep going. Useful for
  migrating an existing deployment.
- `enforce` — run all checks; any failure aborts with exit code 13.
  Recommended for production.

Quick start:

```bash
# Generate a keypair, sign every schema in ./schemas/, and print the
# public key in the form that goes into the trust policy.
schemaforge sign schemas/ \
    --ed25519-generate ./keys/sign.pem \
    --print-pubkey

# Splice the printed public key into config.toml:
#
# [schema_forge.signing]
# mode = "enforce"
#
# [[schema_forge.signing.trusted_signers]]
# kind = "ed25519"
# name = "ops-key"
# public_key_b64 = "<PASTED-FROM-ABOVE>"

# Standalone verifier — exits non-zero on any failure. Use as a
# pre-merge CI gate.
schemaforge verify schemas/
```

Behind the scenes `schemaforge sign` writes a `<file>.sig` next to
every `.schema` and a `schemas.manifest.toml` (plus its own `.sig`)
that pins every file's SHA-256. The verifier checks both: per-file
signatures defeat tampering, the signed manifest defeats add/remove
attacks. Three signer kinds are defined — `ed25519`, `ssh-allowed-signers`,
and `cosign-keyless` — all shipped today. Trust evaluation uses
OR-semantics so adding a new key is additive.

#### Rollout: off → warn → enforce

A fresh `schemaforge init` ships with `mode = "off"` (the
`[schema_forge.signing]` block is commented out), so day-one
deployments behave exactly as before signing was added. The
recommended path to full enforcement runs through three stages, in
order:

1. **Stage 1 — `off` (default).** Write and edit schemas freely;
   no verification runs. Use this until the schema set is stable
   enough to start signing.
2. **Stage 2 — `warn`.** Generate keys and sign every schema with
   `schemaforge sign`. Uncomment the signing block in `config.toml`;
   the shipped scaffold starts you on `mode = "warn"`. Every command
   now runs the verifier end-to-end (manifest + per-file signatures +
   manifest-vs-disk cross-check + pinned SHA-256s), but failures are
   logged rather than fatal. Run this in CI for one release cycle to
   confirm every operator's environment passes verification.
3. **Stage 3 — `enforce`.** Once `schemaforge verify` is green on
   every branch, change the line to `mode = "enforce"`. Verification
   failures now abort with exit code 13. `--no-verify` is refused in
   enforce unless `SCHEMAFORGE_ALLOW_NO_VERIFY=1` is set —
   production deployments cannot silently skip verification.

OR-semantics over `[[trusted_signers]]` means each stage can also be
used to *rotate* keys: add a new anchor, sign new schemas under it,
let old signatures verify under the old anchor until they're all
re-signed, then drop the retired anchor in a later release.

#### SSH allowed_signers (alternative to ed25519)

Operators who already manage an SSH key for code review or `git commit`
signing can reuse it. SchemaForge produces SSHSIG-format signatures
identical to `ssh-keygen -Y sign -n schema-forge-signing@govcraft.ai`
and verifies them against an OpenSSH `allowed_signers` file (the same
format `git config gpg.ssh.allowedSignersFile` consumes).

```bash
# Sign with an existing OpenSSH key (e.g., ~/.ssh/id_ed25519).
schemaforge sign schemas/ \
    --ssh-key ~/.ssh/id_ed25519 \
    --ssh-principal roland@govcraft.ai \
    --print-pubkey
```

The `--print-pubkey` advisory output is the `allowed_signers` line to
append to your trust file plus the corresponding TOML block:

```toml
[[schema_forge.signing.trusted_signers]]
kind = "ssh-allowed-signers"
name = "ops-allowed-signers"
path = "/etc/schemaforge/allowed_signers"
```

Supported per-entry options: `namespaces="ns1,ns2"`, `valid-after`,
`valid-before` (`YYYYMMDD[HHMMSS][Z]`, UTC). Entries restricted to a
namespace other than `schema-forge-signing@govcraft.ai` are
silently skipped at match time, so an SSH key authorised to attest git
commits can't accidentally become trusted for schemas.

#### Cosign keyless (workload OIDC, CI-friendly)

The CI flow most teams already use to sign release binaries also signs
schemas. SchemaForge shells out to `cosign sign-blob --yes --bundle …`
so each `<file>.sig` is a self-contained Sigstore Bundle JSON carrying
the ephemeral Fulcio certificate, the detached signature, and the
Rekor inclusion proof. Verification rides the
[`sigstore-verify`](https://crates.io/crates/sigstore-verify) crate's
full chain check, then post-matches the certificate's OIDC subject
against a SchemaForge-side glob:

```bash
# Inside a GitHub Actions workflow with `id-token: write`:
schemaforge sign schemas/ --keyless
```

```toml
[[schema_forge.signing.trusted_signers]]
kind = "cosign-keyless"
name = "release-pipeline"
issuer = "https://token.actions.githubusercontent.com"
subject_pattern = "https://github.com/govcraft/schemaforge/.github/workflows/release.yml@refs/tags/v*"
```

`issuer` is matched exactly (the value Fulcio embeds in the cert's
`1.3.6.1.4.1.57264.1.1` extension); `subject_pattern` is a
[`globset`](https://crates.io/crates/globset) glob matched against the
cert's SAN URI. Verification works long after the (~10 minute) Fulcio
cert expires because the bundle preserves Rekor's `integratedTime` as
the historical signing instant.

Requirements: `cosign` ≥ v3 on the runner (override with
`--cosign-bin <path>`) and a working OIDC token source. GitHub Actions
sets `ACTIONS_ID_TOKEN_REQUEST_TOKEN` / `…_URL` automatically when the
workflow grants `permissions: id-token: write`.

##### Airgapped / SCIF deployments

The cosign-keyless verifier consults the Sigstore production trust
root by default — a snapshot baked into the `sigstore-trust-root`
crate. For SCIF or otherwise network-isolated deployments that
embedded copy will eventually age out as Fulcio rotates
intermediates, and there is no way to refresh it from inside the
enclave. The `trust_root_bundle` config field plus the
`trust-bundle` subcommand cover that workflow:

```bash
# On a connected jump host with internet access:
schemaforge trust-bundle refresh --output trust_root.json
schemaforge trust-bundle inspect trust_root.json    # sanity check

# Copy trust_root.json across the airgap into the deployment, then:
```

```toml
[schema_forge.signing]
mode = "enforce"
trust_root_bundle = "/etc/schemaforge/trust_root.json"

[[schema_forge.signing.trusted_signers]]
kind = "cosign-keyless"
name = "release-pipeline"
issuer = "https://token.actions.githubusercontent.com"
subject_pattern = "https://github.com/govcraft/schemaforge/.github/workflows/release.yml@refs/tags/v*"
```

The trust root is loaded once per process and shared across every
cosign-keyless verifier in the policy. Missing or malformed bundle
files fail loud at startup — fallback to the embedded snapshot is
deliberately not implemented, because silent fallback would hide
rotation drift in exactly the deployments that need to catch it.
`trust-bundle refresh --instance github` fetches the GitHub
artifact-attestation trust root instead of public-good; `--instance
staging` fetches Sigstore's staging environment (useful only for
validating new tooling).

## AI Agent

The AI agent uses [acton-ai](https://crates.io/crates/acton-ai) to connect an LLM to SchemaForge through four custom tools:

| Tool | Purpose |
|------|---------|
| `validate_schema` | Parse and validate DSL; returns structured errors for self-correction |
| `list_schemas` | Show existing schemas as DSL for context |
| `apply_schema` | Register schemas and execute migrations (supports dry-run) |
| `generate_cedar` | Create Cedar authorization policy templates |

The agent workflow is straightforward: the LLM generates DSL, calls `validate_schema`, reads any errors, fixes the DSL, and validates again. This self-correction loop is not custom retry logic -- it is the natural behavior of an LLM tool execution loop. The grammar is small enough that even 7B parameter local models produce valid schemas consistently.

### Provider Configuration

Configure AI providers in `acton-ai.toml`:

```toml
default_provider = "ollama"

[providers.ollama]
type = "ollama"
model = "qwen2.5:7b"
base_url = "http://localhost:11434/v1"
timeout_secs = 120
max_tokens = 4096
temperature = 0.3

[providers.cloud]
type = "anthropic"
model = "claude-sonnet-4-20250514"
api_key_env = "ANTHROPIC_API_KEY"
```

Use `--provider` to select a provider at generation time, or let `auto` pick from the configuration.

## Programmatic Usage

### As a Library

Add the crates you need to your `Cargo.toml`:

```toml
[dependencies]
schema-forge-core = "0.2"
schema-forge-dsl = "0.1"
schema-forge-backend = "0.1"
schema-forge-surrealdb = "0.2"
schema-forge-acton = "0.1"
```

### Parsing Schemas

```rust
use schema_forge_dsl::{parse, print};

let source = r#"
schema Contact {
    name: text(max: 255) required
    email: text required indexed
    active: boolean default(true)
}
"#;

let schemas = parse(source).expect("parse failed");
assert_eq!(schemas[0].name.as_str(), "Contact");

// Round-trip: parse -> print -> parse produces equivalent AST
let dsl_text = print(&schemas[0]);
```

### Building an HTTP Server

```rust
use schema_forge_acton::SchemaForgeExtension;
use schema_forge_surrealdb::SurrealBackend;

let backend = SurrealBackend::connect("ws://localhost:8000").await?;

let extension = SchemaForgeExtension::builder()
    .with_backend(backend.clone())
    .with_auth_store(backend)
    .with_admin_credentials("admin".into(), "changeme".into())
    .build()
    .await?;

// Register JSON forge routes under /forge on any axum Router.
// The UI is generated separately with `schemaforge site generate`, which
// writes a React + Vite project that talks to this API. See
// docs/site-guide.md for the end-to-end workflow.
let app = extension.register_routes(axum::Router::new());
```

See [`docs/site-guide.md`](docs/site-guide.md) for the React site generator workflow, including the `/app/*` route tree, template override loader, auth bootstrap, and field-type widget reference. (The runtime-dynamic admin console moved to the [`schemaforge-console`](https://github.com/Govcraft/schemaforge-console) repo.)

### Computing Migrations

```rust
use schema_forge_core::migration::DiffEngine;

// Compare two schema versions
let plan = DiffEngine::diff(&old_schema, &new_schema);

println!("{}", plan);
// Migration plan for 'Contact' (3 steps, safe)
//   1. ADD field 'phone' [safe]
//   2. ADD field 'status' [safe]
//   3. ADD INDEX on 'email' [safe]

if plan.has_destructive_steps() {
    // Prompt for confirmation before applying
}
```

## Federal Deployment

SchemaForge generated sites target Section 508 (36 CFR Part 1194, 2017 ICT Refresh) and WCAG 2.0 Level A/AA conformance. Federal-facing deployments must satisfy three obligations OMB M-24-08 and 36 CFR 1194 §§603.2–603.3 place on every page — a published accessibility statement, a working feedback mechanism, and a named §508 program manager / accessibility coordinator. The generator wires all three when you pass `--accessibility-contact`.

### Federal deployment checklist

Before submitting to a federal procurement reviewer, every generated tenant deployment should:

1. **Regenerate the site with a named accessibility contact:**
   ```bash
   schemaforge site generate \
     --schema-dir schemas \
     --out-dir site \
     --accessibility-contact roland@govcraft.ai \
     --force-user-files
   ```
   Omitting `--accessibility-contact` is allowed but the generated `/accessibility` page will render a visible "contact not configured" notice so the gap stays auditable.

2. **Confirm the `/accessibility` route loads** in both unauthed and authed states. The footer link is rendered on the sidebar rail and the login page; from any route a Tab traversal should reach it.

3. **Run the axe scan and design-token contrast tests** (see [Contrast verification](#contrast-verification) below) and attach the output to the procurement package.

4. **Sign the ACR.** Use the Govcraft `vpat-508` pipeline (`schemaforge-federal`) to produce a signed VPAT 2.5Rev 508 Accessibility Conformance Report from the live source tree.

### Contrast verification

The generated `src/index.css` contains documented token math that clears WCAG 2.0 SC 1.4.3 (Contrast Minimum, 4.5:1) against every consumer background. Procurement reviewers typically want runtime evidence in addition to source-level math. Two complementary checks satisfy this:

- **Continuous (automated):** `@axe-core/playwright` runs against the deployed instance in CI on every PR. Findings are surfaced in the pipeline summary; a non-zero a11y count fails the build. See `.github/workflows/site-e2e.yml`.
- **Procurement (point-in-time):** Color Contrast Analyzer (TPGi) screenshot pass on a deployed instance, one row per text-on-background token pair, attached to the next ACR revision as Appendix B.

Token pairs to verify on light theme: `--gc-ink` on `--gc-paper`, `--gc-steel` / `--gc-mist` on `--gc-paper` and `--gc-paper-2`, `--accent` on `--gc-paper`. On dark theme: `--app-fg-1` / `--app-fg-2` / `--app-fg-3` on `--app-bg` and `--app-bg-2`. The dashed-border error state and required-indicator symbols are paired with text so they also satisfy SC 1.4.1 (Use of Color).

### Reduced motion, page titles, session timing

The baseline also honors three platform-accessibility expectations federal reviewers test for explicitly:

- **`prefers-reduced-motion`** — `src/index.css` clamps all animation and transition durations to ~0.01ms when the OS-level reduce-motion setting is on (Section 508 FPC §302.9, WCAG 2.1 SC 2.3.3 advisory).
- **Descriptive page titles** — `index.html` ships `<title>{project_name}</title>` for pre-hydration paint and text-mode browsers; `useDocumentTitle` refines per route at runtime (SC 2.4.2).
- **Session-timeout warning** — `src/lib/auth.ts` surfaces a T-30s warning toast with an "Extend session" action before the PASETO refresh window closes (SC 2.2.1).

## Project Status

SchemaForge is under active development. All seven crates compile and pass 1123 tests across the workspace (unit, integration, property-based, and round-trip tests).

| Crate | Version | Tests |
|-------|---------|-------|
| `schema-forge-core` | 0.6.0 | 293 |
| `schema-forge-dsl` | 0.3.0 | 170 |
| `schema-forge-backend` | 0.5.0 | 41 |
| `schema-forge-surrealdb` | 0.4.0 | 60 |
| `schema-forge-postgres` | 0.2.0 | 48 |
| `schema-forge-acton` | 0.11.0 | 388 |
| `schema-forge-cli` | 0.8.0 | 123 |

### Implemented

- Full runtime type system with validated newtypes and serde round-trips
- DSL lexer (logos), recursive descent parser, and printer with round-trip fidelity
- Migration engine: schema diffing, safety classification, value transforms
- Storage-agnostic query IR with relation traversal
- SurrealDB backend: DDL codegen, entity CRUD, query translation
- PostgreSQL backend: DDL codegen, entity CRUD, query translation
- Axum JSON API with dynamic CRUD routes and schema management
- React site generator (`schemaforge site generate`) producing a Vite + Tailwind + shadcn app against the JSON API
- Token-based authentication (PASETO) with an auth-store-backed login endpoint
- Email-based user invitation and onboarding with single-use PASETO invite links and SMTP delivery (see [`docs/invitations-reference.md`](docs/invitations-reference.md))
- Cedar authorization policy generation
- Schema-level and field-level access control via `@access` and `@field_access` annotations
- Record-level ownership-based access control
- Multi-tenant support via `@tenant` annotations
- Operator-defined PASETO custom-claim → Cedar principal attributes for hand-written policies (see `docs/principal-claims-reference.md`)
- Webhook notifications for entity CRUD events
- OpenAPI spec generation from the schema registry
- OpenTelemetry tracing and metrics integration
- CLI with 10 commands, global options, environment variable support, and shell completions

### Planned

- SQLite backend
- Watch mode for hot-reloading schema changes during development

## Design Decisions

**Why a custom DSL?** The grammar is small, git-trackable, and code-reviewable. Its simplicity gives LLMs a high success rate even with small local models. The parser and printer guarantee lossless round-trip conversion.

**Why SurrealDB and PostgreSQL?** SurrealDB's native record links map directly to SchemaForge relations, and its embedded mode means no external process for development. PostgreSQL provides production-grade reliability and ecosystem compatibility. Both backends implement the same `SchemaBackend` and `EntityStore` traits — switching is a feature flag change.

**Why acton-ai for the agent?** The tool execution loop is the self-correction loop. No custom retry logic, no bespoke error handling -- the LLM calls tools, reads results, and adapts. Multi-provider support (local and cloud models) comes free from the configuration.

**Why storage-agnostic traits?** The `SchemaBackend` and `EntityStore` traits use RPITIT (return position impl Trait in trait) for async methods without `async-trait`. Adding a new backend means implementing two traits -- everything else (parsing, validation, migration planning, query construction, route handling) stays the same.

## Contributing

Contributions are welcome. The project uses standard Rust tooling:

```bash
# Run all tests
cargo test --workspace

# Check formatting
cargo fmt --all -- --check

# Run clippy
cargo clippy --workspace -- -D warnings
```

## License

See the project repository for license information.
