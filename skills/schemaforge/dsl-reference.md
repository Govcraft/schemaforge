# SchemaForge DSL — Complete Syntax Reference

## EBNF Grammar

```ebnf
program         = { schema_def } ;

schema_def      = { annotation } "schema" PASCAL_IDENT "{" { field_def } "}" ;

annotation      = "@" annotation_name [ "(" annotation_params ")" ] ;

annotation_name = "version" | "display" | "system" | "access"
                | "tenant" | "dashboard" | "webhook" | "hook" ;

field_def       = SNAKE_IDENT ":" field_type { modifier } { field_annotation } ;

field_annotation_name
                = "owner" | "widget" | "kanban_column" | "format"
                | "field_access" | "list" | "enum_colors" ;

field_type      = primitive_type [ "[]" ]
                | "->" PASCAL_IDENT [ "[]" ]
                | "composite" "{" { field_def } "}"
                ;

primitive_type  = "text" [ "(" text_params ")" ]
                | "richtext"
                | "integer" [ "(" integer_params ")" ]
                | "float" [ "(" float_params ")" ]
                | "boolean"
                | "datetime"
                | "enum" "(" enum_variants ")"
                | "json"
                | "file" "(" file_params ")"
                ;

text_params     = "max" ":" INTEGER ;
integer_params  = [ "min" ":" INTEGER ] [ "," ] [ "max" ":" INTEGER ] ;
float_params    = "precision" ":" INTEGER ;
enum_variants   = STRING { "," STRING } ;
file_params     = "bucket" ":" STRING "," "max_size" ":" size_literal "," "mime" ":" "[" STRING { "," STRING } "]" [ "," "access" ":" STRING ] ;
size_literal    = INTEGER | STRING ;  (* string carries KB/MB/GB/KiB/MiB/GiB suffix *)

modifier        = "required" | "indexed" | "default" "(" value ")" ;
value           = STRING | INTEGER | FLOAT | "true" | "false" ;

field_annotation = "@" field_annotation_name [ "(" field_annotation_params ")" ] ;
```

## Lexer Tokens

**Keywords:** `schema`, `text`, `richtext`, `integer`, `float`, `boolean`, `datetime`, `enum`, `json`, `file`, `composite`, `required`, `indexed`, `default`, `true`, `false`

**Punctuation:** `{` `}` `(` `)` `[` `]` `:` `,` `->` `@`

**Literals:**
- Strings: `"double-quoted"` with escape sequences
- Integers: `42`, `-10` (optional negative sign)
- Floats: `3.14`, `-2.5` (must have decimal point)

**Identifiers:** `[a-zA-Z_][a-zA-Z0-9_]*`

**Comments:** `// line comment` and `/* block comment */`

## Field Types — Complete Details

### text

Unconstrained or length-limited string.

```
name: text                    // no limit
name: text(max: 255)          // max 255 characters
```

Constraint: `max` is `u32` (0 to 4,294,967,295).

### richtext

Formatted/HTML content. No constraints.

```
description: richtext
```

### integer

Whole number with optional min/max bounds.

```
count: integer                         // no bounds
age: integer(min: 0)                   // min only
score: integer(min: 0, max: 100)       // both bounds
priority: integer(max: 10)             // max only
```

Constraints: `min` and `max` are `i64`. Parser validates `min <= max`.

### float

Decimal number with optional precision.

```
amount: float                  // no precision limit
price: float(precision: 2)    // 2 decimal places
```

Constraint: `precision` is `u32` (decimal places).

### boolean

True/false value.

```
active: boolean
active: boolean default(true)
```

### datetime

ISO 8601 timestamp.

```
created_at: datetime
hire_date: datetime required
```

### enum

Restricted set of string values.

```
status: enum("active", "inactive")
priority: enum("low", "medium", "high") default("medium")
```

Rules:
- At least 1 variant required
- No duplicate variants
- No empty strings
- All variants are strings (double-quoted)

### json

Arbitrary unstructured data. No schema enforcement.

```
metadata: json
settings: json
```

### file

S3-backed binary attachment. The column stores a JSON metadata object
(`{ key, size, mime, status, uploaded_at, checksum }`); the bytes live in a
configured object storage bucket. Uploads go client → S3 directly via a
presigned PUT — the runtime never proxies upload bytes.

```
contract:  file(bucket: "documents", max_size: "25MB", mime: ["application/pdf"])
evidence:  file(bucket: "evidence",  max_size: "100MB", mime: ["image/*", "application/pdf"], access: "proxied")
avatar:    file(bucket: "avatars",   max_size: 524288,  mime: ["image/jpeg", "image/png"]) required
```

Parameters (order-insensitive, comma-separated):

| Parameter | Required | Meaning |
|---|---|---|
| `bucket` | yes | Name of a `[schema_forge.storage.backends.<name>]` entry in config. Boot fails if unresolved. |
| `max_size` | yes | Upper size bound. Plain integer (bytes) or quoted string with suffix (`"25MB"`, `"1KiB"`). Must be > 0. |
| `mime` | yes | Allowlist. Non-empty. Each entry is either `"type/subtype"` or `"type/*"` (family wildcard, case-insensitive). |
| `access` | no (`"presigned"`) | `"presigned"` → 302 redirect to signed S3 GET on download. `"proxied"` → runtime streams bytes, re-checking authz per fetch. |

**Size-literal suffixes** are base-1024: `KB`/`K`/`KiB` = 1024, `MB`/`M`/`MiB` = 1024², `GB`/`G`/`GiB` = 1024³, `TB`/`T`/`TiB` = 1024⁴. Case-insensitive. An optional `B` (`"512B"`) means bytes. Invalid forms (`"25XB"`, `"-25MB"`, suffix-only) raise a parse error.

**MIME wildcards** match the family prefix: `"image/*"` matches `image/png`, `image/jpeg`, `image/svg+xml`, etc.

File fields cannot currently appear inside `composite { ... }` blocks or in arrays (`file[]`). Multi-file needs are handled today by declaring a dedicated child schema with an `@tenant(parent: ...)` or relation to the parent.

See [storage-reference.md](storage-reference.md) for the full upload flow, state machine, configuration, scanner integration, and operational failure modes.

### Relations

Link to another schema by PascalCase name.

```
company: -> Company              // one-to-one (stored FK)
contacts: -> Contact[]           // collection (derived when paired, see below)
manager: -> Employee             // self-referencing relation
```

Target schema name must be PascalCase. The `[]` suffix indicates many cardinality.

**Inverse collections.** A `-> X[]` field whose target schema declares a
`-> Self` FK pointing back is automatically resolved as a **derived
inverse view**:

```
schema Company {
    name:     text required
    contacts: -> Contact[]       // derived — no physical column
}

schema Contact {
    name:    text required
    company: -> Company          // FK on the child
}
```

- `GET /schemas/Company/entities/<id>` resolves `contacts` at read time
  by querying `Contact` filtered on the `company` FK. Missing relations
  show as `[]`, never `null`.
- Writes to the derived field are rejected with `422` — persist the
  relationship by writing `Contact.company` on the child.
- Migrations never emit a column for a derived field. Nothing to drift.

If the target schema has **no** FK pointing back, `-> X[]` keeps its
older stored-array behavior (use it for many-to-many / tag-style lists
where both sides are independent).

Two FKs from the same child back to the same parent is rejected at
schema-load time with an "ambiguous inverse" error. Fix it by removing
the duplicate FK — the DSL does not currently support an `@inverse`
annotation to disambiguate.

### Arrays

Array of any primitive type.

```
tags: text[]
scores: integer[]
flags: boolean[]
labels: enum("bug", "feature", "docs")[]
```

`file[]` is **not** supported in v1. Use a sibling schema with a relation back to the parent instead.

### Composites

Nested object with its own field definitions. Fields inside composites follow the same rules (type, modifiers, annotations).

```
address: composite {
    street:      text
    city:        text required
    state:       text
    postal_code: text(max: 20)
    country:     text(max: 100) required
}
```

Composites can contain any field type except relations, other composites, and `file` fields.

## Modifiers — Complete Details

Modifiers appear after the type, before field annotations.

### required

Field must have a non-null value on create/update.

```
name: text required
email: text(max: 512) required indexed
```

### indexed

Field is indexed for fast lookups and queries.

```
email: text required indexed
slug: text(max: 100) required indexed
```

### default(value)

Default value assigned when field is omitted.

```
active: boolean default(true)
status: enum("draft", "published") default("draft")
priority: integer default(0)
rate: float default(9.99)
currency: text(max: 3) default("USD")
```

Value types:
- String: `default("text")`
- Integer: `default(42)` or `default(-10)`
- Float: `default(3.14)` or `default(-2.5)`
- Boolean: `default(true)` or `default(false)`

## Schema-Level Annotations — Complete Details

Schema-level annotations appear before the `schema` keyword.

### @version(N)

Declares schema version. Must be a positive integer (>= 1).

```
@version(2)
schema Contact { ... }
```

Used by the migration engine to track schema evolution.

### @display("field_name")

Identifies the field used as the primary display/label for records.

```
@display("full_name")
schema Employee { ... }
```

The field name must be a valid field in the schema.

### @system

Marks a schema as a protected system entity. System schemas are auto-created at startup and not user-editable.

```
@system
@display("name")
schema Theme { ... }
```

### @tenant(root) / @tenant(parent: "ParentSchema")

Configures multi-tenant data scoping.

```
// Tenant root — all data scoped to this entity
@tenant(root)
schema Organization { ... }

// Tenant child — scoped to parent tenant
@tenant(parent: "Organization")
schema Department { ... }
```

### @access(read: [...], write: [...], delete: [...], cross_tenant_read: [...])

Role-based access control. Generates Cedar authorization policies.

```
@access(
    read: ["member", "admin"],
    write: ["admin"],
    delete: ["admin"],
    cross_tenant_read: ["superadmin"]
)
schema Organization { ... }
```

All arrays contain role name strings. `cross_tenant_read` is optional.

> **Role names are application-defined.** `"admin"`, `"superadmin"`, `"member"`, `"hr"`, etc. in `@access(...)` are just strings the application interprets — they carry no platform-wide privileges and don't bypass any check. The single reserved name is `platform_admin`, which gates schema-forge's user-management endpoints (`/api/v1/forge/users`) and the file scan-complete callback. Don't grant `platform_admin` from `@access(...)` unless you really mean to hand callers schema-bypass and user-management rights — pick a different name (e.g. `"superadmin"` as in the example above) for high-tier in-app roles.

### @dashboard(widgets: [...], layout: "...", group_by: "...", sort_default: "...")

Dashboard configuration for UI rendering.

```
@dashboard(
    widgets: ["count", "sum:value", "avg:value"],
    layout: "kanban",
    group_by: "stage",
    sort_default: "-expected_close"
)
schema Deal { ... }
```

- `widgets`: aggregation functions (`"count"`, `"sum:field"`, `"avg:field"`)
- `layout`: `"kanban"` or default list
- `group_by`: field name to group by (typically an enum field with `@kanban_column`)
- `sort_default`: field name, prefix `-` for descending

### @webhook / @webhook(events: [...], url: "...", secret: "...")

Enables outbound webhook notifications when entities of this schema are created, updated, or deleted. Supports both DSL inline subscriptions and runtime-managed subscriptions via the `WebhookSubscription` system schema.

```
// Enable all events, subscriptions managed at runtime
@webhook
schema Deal { ... }

// Enable specific events only
@webhook(events: ["created", "updated"])
schema Contact { ... }

// Inline static subscription with HMAC signing
@webhook(events: ["created"], url: "https://ext.example.com/hook", secret: "hmac-secret")
schema Order { ... }

// URL without event filter (all events)
@webhook(url: "https://internal.svc/notify")
schema Ticket { ... }
```

- `events`: list of `"created"`, `"updated"`, `"deleted"` — empty or omitted = all events
- `url`: optional static webhook endpoint (inline subscription)
- `secret`: optional HMAC-SHA256 signing secret for the inline subscription

Webhook delivery is non-blocking (background tasks with exponential backoff retry). Payloads include the full entity fields. Runtime subscriptions are managed via the `WebhookSubscription` system schema.

## Field-Level Annotations — Complete Details

Field-level annotations appear after modifiers on the field line.

### @owner

Marks a field as the record ownership tracker.

```
owner_id: text @owner
owner_id: text required @owner
```

Enables record-level ownership checks in authorization.

### @widget("type")

UI widget rendering hint. The accepted set is a **closed vocabulary** — unknown tokens are a parse error.

```
email: text @widget("email")
status: enum("active", "inactive") @widget("status_badge")
score: integer(min: 0, max: 100) @widget("progress")
site:  text @widget("url")
```

**Valid widget types (17 total):** `status_badge`, `count_badge`, `progress`, `markdown`, `rich_text`, `color`, `file`, `image`, `avatar`, `slider`, `rating`, `code`, `phone`, `tags`, `email`, `url`, `json`.

**Legacy tokens that were removed** (use these replacements):

| Removed     | Replacement                  |
|-------------|------------------------------|
| `currency`  | `@format("currency")`        |
| `link`      | `@widget("url")`             |
| `relative_time` | `@format("relative")`    |

Stored metadata that still carries an unknown token is silently stripped at startup; legacy `"link"` is auto-remapped to `"url"` for backward compatibility with persisted `_schema_metadata`.

### @kanban_column

Designates an enum field as the kanban board grouping column. Pair with `@dashboard(layout: "kanban", group_by: "field_name")`.

```
stage: enum("backlog", "in_progress", "done") @widget("status_badge") @kanban_column
```

### @format("type")

Display format hint. Also a **closed vocabulary** — unknown tokens are a parse error. Colon-suffixed forms like `currency:$` were removed in v0.16 and are rejected.

```
salary: float(precision: 2) @format("currency")
probability: integer(min: 0, max: 100) @format("percent")
filesize: integer @format("bytes")
elapsed: integer @format("duration")
created_at: datetime @format("relative")
```

**Valid format types (7 total):** `currency`, `percent`, `date`, `datetime`, `relative`, `bytes`, `duration`.

The React site's `formatFieldValue` helper honors `@format` first, then `@widget`, then falls back to the field's native kind.

### @list(primary|column|hidden)

Controls how the field appears in the generated list-view page (`src/app/pages/<entity>/list.tsx`). The hint is a bare identifier inside parens — not a string literal.

```
title:        text(max: 500) required @list(primary)
stage:        enum("qualifying", "won", "lost") @list(column)
pwin:         integer(min: 0, max: 100) @list(column)
internal_note: richtext @list(hidden)
```

**Resolution order** (first match wins):

1. **Explicit `@list(hint)`** — wins unconditionally.
2. **`@display("field")` auto-promotion** — the schema's `@display` field is promoted to `primary` if no explicit `@list(primary)` exists anywhere on the schema.
3. **Auto-hide** — fields whose kind is `rich_text`, `composite`, `array`, `relation_one`, `relation_many`, or `json` default to `hidden`.
4. **Default** — everything else becomes `column`.

**Rules:**

- At most one `@list(primary)` per schema (parse error on duplicate).
- `@list(column)` on a relation field opts it back into the list view; the generator renders it as a linked cell showing the resolved `<field>__display` label (see `@enum_colors` and the relation display resolution below).
- The generator honors placement when emitting columns, `SORTABLE_FIELDS`, and `FILTERABLE_FIELDS`.

### @enum_colors(variant: "color", ...)

Attaches semantic color tokens to specific variants of an enum field. The closed color set maps to Tailwind badge classes in the generated `EnumBadge` component.

```
pipeline_stage: enum(
                    "qualifying",
                    "collateral_prep",
                    "meeting_requested",
                    "solicitation_active",
                    "proposal_submitted",
                    "awarded",
                    "lost",
                    "no_bid"
                )
                @enum_colors(
                    qualifying: "neutral",
                    collateral_prep: "blue",
                    meeting_requested: "amber",
                    solicitation_active: "purple",
                    proposal_submitted: "violet",
                    awarded: "green",
                    lost: "red",
                    no_bid: "gray"
                )
```

**Valid color tokens (10 total):** `neutral`, `gray`, `red`, `amber`, `green`, `blue`, `purple`, `violet`, `teal`, `rose`.

**Rules:**

- Only allowed on `enum` fields — applying `@enum_colors` to a non-enum field is a parse error.
- Every key must match an existing variant of the enum (parse error otherwise).
- No duplicate variant keys within one annotation.
- Variants without an explicit entry render with the default neutral badge — partial coverage is fine.
- The generator emits a per-entity `ENUM_COLORS` map plus a local `EnumBadge` component; both live inside `list.tsx` so Tailwind's JIT picks up the class names without a safelist.

### @field_access(read: [...], write: [...])

Field-level access control — restricts who can read/write specific fields.

```
salary: float(precision: 2) @field_access(read: ["hr", "admin"], write: ["hr", "admin"])
ssn: text(max: 4) @field_access(read: ["hr"], write: ["hr"])
budget: float(precision: 2) @field_access(read: ["finance", "manager"], write: ["finance"])
```

## Validation Rules Summary

| Rule | Parser Behavior |
|------|-----------------|
| Schema names must be PascalCase | Parse error |
| Field names must be snake_case | Parse error |
| No duplicate field names in a schema | Parse error |
| No duplicate annotation kinds on schema | Parse error |
| No duplicate annotation kinds on field | Parse error |
| Enum variants must be non-empty | Parse error |
| Enum variants must have no duplicates | Parse error |
| Enum variant strings must be non-empty | Parse error |
| Integer min must be <= max | Parse error |
| Schema version must be >= 1 | Parse error |
| Schemas must have at least 1 field | Parse error |
| @display field must exist in schema | Validation error |
| At most one `@list(primary)` per schema | Parse error (`MultiplePrimaryListHints`) |
| `@list(hint)` keyword must be `primary`, `column`, or `hidden` | Parse error (`UnknownListHint`) |
| `@enum_colors(...)` only allowed on enum fields | Parse error (`EnumColorsOnNonEnum`) |
| `@enum_colors` keys must match declared enum variants | Parse error (`UnknownEnumColorsVariant`) |
| `@enum_colors` color tokens must be in the 10-color closed set | Parse error (`UnknownEnumColor`) |
| `@enum_colors` variant keys must be unique within one annotation | Parse error (`DuplicateEnumColorsVariant`) |
| `@widget("...")` tokens must be in the 17-widget closed set | Parse error (`UnknownWidgetType`) |
| `@format("...")` tokens must be in the 7-format closed set | Parse error (`UnknownFormatType`) |
| `file(...)` requires `bucket`, `max_size`, and `mime` parameters | Parse error (`InvalidFileParam`) |
| `file(max_size: ...)` must be an integer or suffix-tagged string | Parse error (`InvalidSizeLiteral`) |
| `file(max_size: 0)` is rejected | Parse error (`InvalidFileParam`: "must be greater than zero") |
| `file(mime: [])` is rejected — empty allowlist is not allowed | Parse error (`InvalidFileParam`) |
| `file(mime: [...])` entries must parse as `type/subtype` or `type/*` | Parse error (`InvalidMimePattern`) |
| `file(access: ...)` must be exactly `"presigned"` or `"proxied"` | Parse error (`InvalidFileParam`) |
| `file(...)` rejects unknown parameters | Parse error (`InvalidFileParam`: "unknown file parameter") |
| `file(bucket: ...)` must resolve to a configured `[schema_forge.storage.backends.<name>]` | Startup error (`Internal`: "undeclared storage backends") |

## Round-Trip Fidelity

The DSL parser and printer guarantee lossless round-trips:

```
parse(source) -> print(schemas) -> parse(output)
```

Produces equivalent AST. Field order, modifier order, and annotation order are preserved. Whitespace is normalized on print.

## Database Backend Mapping

The DSL is backend-agnostic. The backend crate translates field types to native DDL:

| FieldType | SurrealDB | PostgreSQL |
|-----------|-----------|------------|
| `text` | `string` | `TEXT` |
| `text(max: N)` | `string` + ASSERT | `VARCHAR(N)` |
| `richtext` | `string` | `TEXT` |
| `integer` | `int` | `BIGINT` |
| `integer(min/max)` | `int` + ASSERT | `BIGINT` + CHECK |
| `float` | `float` | `DOUBLE PRECISION` |
| `float(precision: N)` | `float` | `NUMERIC(N)` |
| `boolean` | `bool` | `BOOLEAN` |
| `datetime` | `datetime` | `TIMESTAMPTZ` |
| `enum(...)` | `string` + ASSERT IN | `TEXT` + CHECK IN |
| `json` | `object` | `JSONB` |
| `-> Target` | `record<Target>` | `TEXT` (FK) |
| `-> Target[]` (stored) | `array<record<Target>>` | `TEXT[]` |
| `-> Target[]` (derived — paired with child FK) | *no column* | *no column* |
| `type[]` | `array<type>` | `type[]` |
| `composite { }` | `object` + nested fields | `JSONB` |
| `file(...)` | `object` (FLEXIBLE) | `JSONB` with structural CHECK (`jsonb_typeof = 'object' AND ? 'status' AND ? 'key'`) |
