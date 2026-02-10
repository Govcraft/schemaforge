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

## Table of Contents

- [Why SchemaForge](#why-schemaforge)
- [Quick Start](#quick-start)
- [Architecture](#architecture)
- [SchemaDSL Reference](#schemadsl-reference)
- [CLI Reference](#cli-reference)
- [AI Agent](#ai-agent)
- [Programmatic Usage](#programmatic-usage)
- [Project Status](#project-status)
- [Design Decisions](#design-decisions)
- [Contributing](#contributing)

## Why SchemaForge

Traditional backend development requires you to define your model in code, write a migration, build CRUD handlers, add validation, wire up authorization, and generate API docs -- separately, for every entity. When a schema changes, you repeat the cycle.

SchemaForge collapses that workflow. One schema file is the single source of truth for your entire entity lifecycle. Change the schema, and everything downstream updates automatically: migrations are computed by diffing versions, routes adapt, validation adjusts, and the OpenAPI spec regenerates.

The AI agent takes this further. Describe what you need in plain English, and an LLM generates the schema, validates it through the tool execution loop (self-correcting any errors), and applies it to your database -- all without writing DSL by hand.

## Quick Start

### Prerequisites

- Rust 1.75+ (2021 edition)
- SurrealDB 2.x (for backend operations; embedded mode works for development)

### Install and Initialize

```bash
# Install the CLI
cargo install schema-forge-cli

# Scaffold a new project
schema-forge init my-platform
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
schema-forge parse schemas/

# Apply schemas to SurrealDB (creates tables, fields, indexes)
schema-forge apply schemas/ --db-url ws://localhost:8000

# Preview migration steps without applying
schema-forge apply schemas/ --dry-run

# Start the API server with dynamic CRUD routes
schema-forge serve --schemas schemas/ --db-url ws://localhost:8000 --db-ns app --db-name main
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

### Generate Schemas with AI

Instead of writing DSL by hand, describe what you need:

```bash
# One-shot generation
schema-forge generate "A ticketing system with tickets linked to contacts,
    priority levels, status tracking, and assignment" --batch -o schemas/ticketing.schema

# Interactive conversational mode
schema-forge generate
```

The AI agent calls `list_schemas` to see what already exists, generates DSL, calls `validate_schema` to check correctness, fixes any errors automatically, and applies the result after confirmation. No custom retry logic -- the LLM's tool execution loop handles self-correction naturally.

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
| `schema-forge-acton` | Axum-based HTTP layer: dynamic CRUD routes, Cedar policy generation, OpenAPI spec, schema registry. |
| `schema-forge-ai` | LLM agent integration via acton-ai: tool-based schema generation, validation, and application. |
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
| Relation (many) | `-> SchemaName[]` | Target must be PascalCase |
| Array | `text[]`, `integer[]`, etc. | Suffix `[]` on any field type |
| Composite | `composite { field: type ... }` | Nested field definitions |
| JSON | `json` | Arbitrary unstructured data |

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
annotation     = "@version" "(" INTEGER ")" | "@display" "(" STRING ")" ;
```

## CLI Reference

```
schema-forge <command> [options]
```

| Command | Description |
|---------|-------------|
| `init <name>` | Scaffold a new project (`--template minimal\|full\|api-only`) |
| `parse <paths>` | Validate `.schema` files and show diagnostics (`--print` for round-trip output) |
| `apply <paths>` | Apply schemas to the backend (`--dry-run`, `--force`, `--with-policies`) |
| `migrate <paths>` | Show migration plan (`--execute` to apply, `--schema` for a specific schema) |
| `generate [desc]` | Generate schemas from natural language (`--batch`, `--provider`, `--model`) |
| `serve` | Start HTTP server with dynamic routes (`--host`, `--port`, `--watch`) |
| `inspect [schema]` | Show registered schemas and details (`--detail`, `--counts`) |
| `export openapi` | Export OpenAPI spec (`-o file`) |
| `policies list` | List Cedar authorization policies |
| `policies regenerate` | Regenerate Cedar policy templates (`--force`) |
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
    .with_backend(backend)
    .build()
    .await?;

// Register forge routes under /forge on any axum Router
let app = extension.register_routes(axum::Router::new());
```

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

## Project Status

SchemaForge is under active development. All seven crates compile and pass 674 tests across the workspace (unit, integration, property-based, and round-trip tests).

| Crate | Version | Tests |
|-------|---------|-------|
| `schema-forge-core` | 0.2.0 | 209 |
| `schema-forge-dsl` | 0.1.0 | 108 |
| `schema-forge-backend` | 0.1.0 | 18 |
| `schema-forge-surrealdb` | 0.2.0 | 47 |
| `schema-forge-acton` | 0.1.0 | 81 |
| `schema-forge-ai` | 0.1.0 | 79 |
| `schema-forge-cli` | 0.2.0 | 132 |

### Implemented

- Full runtime type system with validated newtypes and serde round-trips
- DSL lexer (logos), recursive descent parser, and printer with round-trip fidelity
- Migration engine: schema diffing, safety classification, value transforms
- Storage-agnostic query IR with relation traversal
- SurrealDB backend: DDL codegen, entity CRUD, query translation
- Axum HTTP layer with dynamic CRUD routes and schema management
- Cedar authorization policy generation
- AI agent with tool-based self-correcting schema generation
- CLI with 10 commands, global options, environment variable support, and shell completions

### Planned

- PostgreSQL and SQLite backends
- OpenAPI spec generation from the schema registry
- Watch mode for hot-reloading schema changes during development
- OpenTelemetry tracing and metrics integration

## Design Decisions

**Why a custom DSL?** The grammar is small, git-trackable, and code-reviewable. Its simplicity gives LLMs a high success rate even with small local models. The parser and printer guarantee lossless round-trip conversion.

**Why SurrealDB first?** SurrealDB's native record links map directly to SchemaForge relations. Its SCHEMAFULL mode mirrors schema validation. Dot-notation query traversal eliminates JOINs. Embedded mode means no external process for development.

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
