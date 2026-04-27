# SchemaForge DSL — Design Patterns

## Multi-Tenancy Pattern

Use `@tenant` to scope data to organizational boundaries.

### Structure

```
@tenant(root)          → The top-level entity (e.g., Organization)
@tenant(parent: "X")   → Child entities scoped to the root
```

### Rules

1. Exactly one schema should be `@tenant(root)` — it anchors the hierarchy
2. Child schemas reference the parent with `@tenant(parent: "ParentName")`
3. All data in child schemas is automatically scoped to the root tenant
4. The root schema should have `owner_id: text required @owner` for ownership

### Example

```
@tenant(root)
@display("name")
schema Organization {
    name:      text(max: 255) required indexed
    slug:      text(max: 100) required indexed
    plan:      enum("free", "pro", "enterprise") default("free")
    owner_id:  text required @owner
    active:    boolean default(true)
}

@tenant(parent: "Organization")
@display("name")
schema Team {
    name:       text(max: 255) required
    org:        -> Organization required
    members:    -> Employee[]
    active:     boolean default(true)
}
```

## Access Control Pattern

Layer schema-level and field-level access for defense in depth. Every annotation in this section lowers into a **Cedar policy** and is enforced by the embedded Cedar engine — there is no parallel custom-guard path the runtime falls through to. The Cedar bundle (generated policies + your `policies/custom/*.cedar` files) is strict-mode-validated on every load; CI should run `schema-forge policies validate` before merging schema changes.

> **A note on `"admin"`** — role names in `@access(...)` are application-defined strings the schema-forge runtime treats opaquely. `"admin"`, `"member"`, `"hr"`, `"superadmin"` etc. are *your* in-app tier labels and carry no platform-wide privileges. The single reserved name is `platform_admin`, which gates schema-forge's `/api/v1/forge/users` endpoints and the file scan-complete callback — keep it out of `@access` lists unless you mean to grant schema-bypass and user-management rights along with whatever schema action you're naming.

> **Role ranks** — every role name you reference in `@access(...)` (or hand-written Cedar policies) needs a numeric rank in `policies/role_ranks.toml`. The rank drives the no-upward-visibility rule for user management (`principal.role_rank >= resource.role_rank`) and is also where the operator declares the in-app hierarchy. `platform_admin` is reserved at `i64::MAX` and must NOT appear in the file.

### Schema-Level Access

Controls who can perform CRUD operations on the entire entity:

```
@access(
    read: ["member", "manager", "admin"],
    write: ["manager", "admin"],
    delete: ["admin"]
)
```

Optional `cross_tenant_read` for superadmin access across tenants:

```
@access(
    read: ["member"],
    write: ["admin"],
    delete: ["admin"],
    cross_tenant_read: ["superadmin"]
)
```

### Field-Level Access

Restricts specific sensitive fields to authorized roles:

```
salary:        float(precision: 2) @field_access(read: ["hr", "admin"], write: ["hr"])
ssn_last_four: text(max: 4) @field_access(read: ["hr"], write: ["hr"])
```

### Layering Pattern

1. `@access` on schema — broad access for the entity
2. `@field_access` on sensitive fields — narrow access overrides
3. `@owner` on ownership field — record-level ownership checks

```
@access(read: ["member", "hr", "admin"], write: ["hr", "admin"], delete: ["admin"])
schema Employee {
    full_name: text(max: 255) required indexed          // readable by all (schema access)
    email:     text(max: 512) required indexed           // readable by all
    salary:    float(precision: 2)
               @field_access(read: ["hr", "admin"], write: ["hr"])  // restricted
    owner_id:  text @owner                               // ownership tracking
}
```

## Hidden Field Pattern (`@hidden`)

`@hidden` is the language-level secret guard. A field annotated `@hidden`:

- **Never appears in any API response.** REST get/list/query, GraphQL queries, file-field metadata — every serializer strips it before bytes leave the process.
- **Is rejected in any client-supplied request body.** Create / update / patch / GraphQL inputs that mention a hidden field's name return `422 hidden_field_in_body` without touching storage.
- **Is invisible to Cedar.** Policy generation skips hidden fields entirely, so a custom policy can't accidentally gate decisions on a secret value.
- **Stays readable to backend code.** Internal consumers (e.g. `EntityAuthStore` reading `password_hash` to check argon2 hashes during login) read the entity directly from the storage layer, bypassing the API surface that does the strip.

This is the first-class replacement for the "remember to never expose this column" pattern. If a hidden field ever shows up in an API response, that's a parser-level regression — not a missed code review.

### When to use `@hidden`

- **Password hashes** — argon2 / bcrypt outputs that must never round-trip to a client. The system `User` schema's `password_hash: text(max: 512) @hidden` is the canonical example.
- **API credentials stored on entities** — third-party access tokens, webhook signing secrets, encryption keys held alongside business data.
- **Server-internal counters or accumulators** — values the runtime mutates (e.g. login-attempt counters) that the client should never see or set.

### Example

```
@system @display("email")
schema User {
    email:          text(max: 512) required indexed
    display_name:   text(max: 255) required
    roles:          text[]
    role_rank:      integer required
    active:         boolean default(true)
    password_hash:  text(max: 512) @hidden    // argon2 hash; never serialized, never accepted in body
    last_login:     datetime
    metadata:       json
}
```

```
@display("name")
schema Integration {
    name:           text(max: 255) required indexed
    provider:       enum("stripe", "twilio", "sendgrid") required
    api_key:        text(max: 512) required @hidden    // server-side outbound credential
    webhook_secret: text(max: 128) @hidden             // verifies inbound webhooks
    active:         boolean default(true)
    owner_id:       text required @owner
}
```

### Layering with `@field_access`

Use `@hidden` for fields that **no API caller** should ever see, regardless of role. Use `@field_access` for fields where some roles legitimately read the value through the API. They compose — adding `@hidden` to a field with `@field_access` makes the field-access read list moot (the field is stripped before role evaluation runs), so pick the stronger guard intentionally:

```
salary:         float(precision: 2)
                @field_access(read: ["hr", "admin"], write: ["hr"])   // HR can see it via API

password_hash:  text(max: 512) @hidden                                // nobody sees it via API; backend reads directly
```

## Dashboard & Kanban Pattern

Configure visual dashboards with aggregation widgets and kanban layouts.

### Basic Dashboard

```
@dashboard(widgets: ["count"])
schema Contact { ... }
```

### Kanban Pipeline

Three pieces work together:

1. `@dashboard(layout: "kanban", group_by: "field_name")` on the schema
2. `@kanban_column` on the enum field used for grouping
3. `@widget("status_badge")` on the same field for visual display

```
@dashboard(
    widgets: ["count", "sum:value"],
    layout: "kanban",
    group_by: "stage",
    sort_default: "-created_at"
)
schema Deal {
    stage: enum("prospecting", "proposal", "negotiation", "closed_won", "closed_lost")
           default("prospecting") @widget("status_badge") @kanban_column
    value: float(precision: 2) @format("currency")
    ...
}
```

### Aggregation Widgets

- `"count"` — total record count
- `"sum:field"` — sum of a numeric field
- `"avg:field"` — average of a numeric field

### Sort Default

- `"field_name"` — ascending sort
- `"-field_name"` — descending sort (prefix `-`)

## Composite Field Pattern

Use composites for nested objects that belong to the parent record.

### When to Use Composites

- **Address** — street, city, state, zip, country
- **Contact info** — name, phone, relationship (e.g., emergency contact)
- **Social profiles** — linkedin, twitter, github
- **Metadata groups** — related settings that travel together

### When NOT to Use Composites

- Data shared across records → use a **relation** (`-> Schema`)
- Data that needs its own CRUD → use a **separate schema**
- Simple key-value pairs → use **json**

### Structure

```
address: composite {
    street:      text
    suite:       text
    city:        text required
    state:       text
    postal_code: text(max: 20)
    country:     text(max: 100) required
    timezone:    text(max: 50)
}
```

Composite fields support:
- All primitive types (text, integer, float, boolean, datetime, enum, json)
- Modifiers (required, indexed, default)
- Field annotations (@widget, @format, etc.)

## Relation Patterns

### One-to-One

Each record links to exactly one related record:

```
department: -> Department
manager:    -> Employee
company:    -> Company required     // required relation
```

### One-to-Many

The **correct** shape is: define the collection on the parent AND the
FK on the child. SchemaForge pairs them automatically and resolves the
parent's collection as a derived inverse view — you never write to it
directly, you write the FK on the child.

```
schema Company {
    name:     text required
    contacts: -> Contact[]          // derived view — read-only
}

schema Contact {
    name:    text required
    company: -> Company             // FK — the real storage
}
```

Under the hood: no column is created for `Company.contacts`; a `GET`
against the parent issues a batched `WHERE company = <id>` against the
child table. `POST`/`PUT`/`PATCH` of `Company.contacts` returns `422`
with a "derived inverse" error. Create contacts and set their
`company` field — the parent's `contacts` will reflect them on the
next read.

> **If the child has no FK back**, `-> X[]` stays as a stored array of
> refs (useful for tag-style lists where both sides are independent).
> See the *Many-to-Many* section below.

### Self-Referencing

A schema relating to itself (hierarchies, dependencies):

```
schema Employee {
    manager:    -> Employee         // reporting chain
}

schema Task {
    blocked_by: -> Task[]           // dependency graph — NOT derived:
                                    // there is no single `-> Task` FK
                                    // back from Task to itself, so this
                                    // stays a stored array of refs.
}

schema Comment {
    parent_comment: -> Comment      // threaded discussions
}
```

### Cross-Domain Relations

Linking schemas across different domains:

```
schema Deal {
    contact:    -> Contact required  // CRM → People
    company:    -> Company           // CRM → CRM
    assigned_to: -> Employee         // CRM → HR
}
```

## Widget & Format Selection Guide

Both `@widget("...")` and `@format("...")` accept **closed vocabularies** — the parser rejects anything outside the tables below. Widget drives edit/render semantics; format drives the display string.

**Valid widgets (17):** `status_badge`, `count_badge`, `progress`, `markdown`, `rich_text`, `color`, `file`, `image`, `avatar`, `slider`, `rating`, `code`, `phone`, `tags`, `email`, `url`, `json`

**Valid formats (7):** `currency`, `percent`, `date`, `datetime`, `relative`, `bytes`, `duration`

Match field semantics to the right pair:

| Field Semantics   | Type          | Widget          | Format      |
|-------------------|---------------|-----------------|-------------|
| Status/category   | enum          | `status_badge`  | —           |
| Email address     | text          | `email`         | —           |
| Phone number      | text          | `phone`         | —           |
| URL/website       | text          | `url`           | —           |
| Image URL         | text          | `image`         | —           |
| Hex color         | text          | `color`         | —           |
| Code/identifier   | text          | `code`          | —           |
| Monetary value    | float/integer | —               | `currency`  |
| Percentage        | integer/float | `progress`      | `percent`   |
| Score (0-100)     | integer       | `progress`      | —           |
| Slider input      | integer/float | `slider`        | —           |
| Small count       | integer       | `count_badge`   | —           |
| Rating            | integer/float | `rating`        | —           |
| Timestamp (ago)   | datetime      | —               | `relative`  |
| Calendar date     | datetime      | —               | `date`      |
| Full timestamp    | datetime      | —               | `datetime`  |
| File size         | integer       | —               | `bytes`     |
| Duration (sec)    | integer       | —               | `duration`  |
| Tag list          | text[]        | `tags`          | —           |
| File upload       | text          | `file`          | —           |
| Long text content | richtext      | `markdown` or `rich_text` | — |
| Profile image     | text          | `avatar`        | —           |
| JSON blob         | json          | `json`          | —           |

### Common Pairings

```
// Monetary — widget omitted; format does all the work
price: float(precision: 2) @format("currency")

// Percentage
probability: integer(min: 0, max: 100) @widget("progress") @format("percent")

// Status enum
status: enum("active", "inactive") @widget("status_badge")

// Kanban column with semantic colors
stage: enum("todo", "doing", "done")
       @widget("status_badge")
       @kanban_column
       @enum_colors(todo: "neutral", doing: "amber", done: "green")

// Contact info
email:   text(max: 512) @widget("email")
phone:   text(max: 50) @widget("phone")
website: text @widget("url")

// Timestamps
created_at: datetime @format("relative")
closed_at:  datetime @format("date")

// File metrics
file_size: integer @format("bytes")
run_time:  integer @format("duration")

// Tags
tags: text[] @widget("tags")
```

### Legacy widget tokens (removed)

These appear in older schemas and must be migrated before the parser will accept them again:

| Old token     | New form                     |
|---------------|------------------------------|
| `currency`    | `@format("currency")`        |
| `link`        | `@widget("url")`             |
| `relative_time` | `@format("relative")`      |
| `currency:$`  | `@format("currency")` (colon-suffix variants dropped) |

The backend silently auto-remaps persisted `"link"` → `"url"` in `_schema_metadata` at startup and drops any other unrecognized widget token — but the DSL parser is strict.

## List-View Column Curation Pattern

Use `@list(primary|column|hidden)` to control which fields appear in the generated list page and how they're rendered. Without hints, the generator falls back to sensible defaults; annotations only matter when you need to override them.

### The default policy

No annotation is needed for the common case. The resolution ladder is:

1. **Explicit `@list(hint)`** wins.
2. **`@display("field")`** auto-promotes to `primary` when no explicit primary is declared anywhere on the schema.
3. **Auto-hide** for `rich_text`, `composite`, `array`, `relation_one`, `relation_many`, and `json` — these rarely make sense as list columns.
4. **Everything else** defaults to `column`, in declaration order.

At most **one** `@list(primary)` per schema (parse error otherwise).

### Pattern: opt a relation back into the list

When a relation carries a useful display label (company name, agency name) you want visible in the list, flip it from its auto-hidden default:

```
@display("title")
schema Opportunity {
    title:         text(max: 500) required      // auto-promoted to primary
    company:       -> Company required @list(column)  // linked cell with resolved name
    stage:         enum("new", "won", "lost") required
    estimated_value: integer(min: 0) @list(column)
    notes:         richtext                     // auto-hidden (rich_text)
}
```

The generator renders `agency` as a linked cell using the resolved `<field>__display` label (see the relation display resolution in the query API reference) and falls back to the raw ID if resolution is missing.

### Pattern: curate a wide schema down to its signal columns

Schemas with 30+ fields produce unusable horizontal dumps by default. Mark only the fields that matter:

```
schema Opportunity {
    title:         text(max: 500) required @list(primary)
    pipeline_stage: enum(...) required @kanban_column @list(column)
    probability:   integer(min: 0, max: 100) @list(column)
    estimated_value: integer(min: 0) @list(column)
    company:       -> Company @list(column)
    status:        enum(...) @list(column)

    // Everything else defaults to column unless it's heavyweight.
    // Explicit hidden overrides keep internals out of the list.
    internal_notes: richtext
    description:    richtext
    debug_flags:    json @list(hidden)
    working_dir:    text @list(hidden)
}
```

## Semantic Enum Colors Pattern

`@enum_colors` maps specific variants to semantic color tokens so the generated list badges communicate meaning instead of random hashed colors.

**Closed color set:** `neutral`, `gray`, `red`, `amber`, `green`, `blue`, `purple`, `violet`, `teal`, `rose`.

### Pipeline stages (traffic-light shape)

```
pipeline_stage: enum(
                    "prospecting",
                    "proposal",
                    "negotiation",
                    "closed_won",
                    "closed_lost"
                )
                required default("prospecting")
                @widget("status_badge")
                @kanban_column
                @enum_colors(
                    prospecting: "neutral",
                    proposal: "violet",
                    negotiation: "amber",
                    closed_won: "green",
                    closed_lost: "red"
                )
```

### Task status (workflow progression)

```
status: enum("backlog", "todo", "in_progress", "in_review", "done", "cancelled")
        default("backlog")
        @widget("status_badge") @kanban_column
        @enum_colors(
            backlog: "gray",
            todo: "blue",
            in_progress: "amber",
            in_review: "purple",
            done: "green",
            cancelled: "red"
        )
```

### Rules & gotchas

- Only valid on `enum` fields — attaching `@enum_colors` to any other field type is a parse error.
- Every variant key must match an actual variant of the enum (parse error on typos).
- You do **not** need to cover every variant — uncovered ones render with the default neutral badge.
- Pair with `@widget("status_badge")` for best results, though the generator's `EnumBadge` component runs regardless of the widget hint.

## Schema Versioning Pattern

Use `@version(N)` to track schema evolution:

```
@version(1)   // Initial release
@version(2)   // Added fields, changed types
@version(3)   // Breaking changes
```

**When to increment:**
- Adding new fields (safe, but good practice)
- Changing field types (requires migration confirmation)
- Removing fields (destructive — requires confirmation)
- Changing constraints (may require data validation)

The migration engine diffs versions and classifies each step:
- **Safe:** `CreateSchema`, `AddField`, `AddIndex`, `AddRelation`
- **Requires confirmation:** `RenameField`, `ChangeType`, `AddRequired`
- **Destructive:** `DropSchema`, `RemoveField`, `RemoveRelation`

## Standard Field Conventions

Common fields that appear across most schemas:

```
// Record ownership
owner_id: text @owner

// Soft delete
active: boolean default(true)

// Display name (pair with @display annotation)
name: text(max: 255) required indexed

// Flexible metadata
metadata: json
settings: json

// Categorization
tags: text[] @widget("tags")
```

### Recommended Schema Template

```
@version(1)
@display("name")
@access(read: ["member", "admin"], write: ["admin"], delete: ["admin"])
schema EntityName {
    name:     text(max: 255) required indexed
    // ... domain-specific fields ...
    tags:     text[] @widget("tags")
    metadata: json
    owner_id: text @owner
    active:   boolean default(true)
}
```

## File Attachment Pattern

`file` fields store one S3-backed attachment per entity row. Bytes never
transit the runtime; clients PUT directly to object storage through a
presigned URL. Pick `access: "presigned"` for the default (S3 handles
bandwidth, short-TTL bearer URLs) and `access: "proxied"` only for
sensitive/classified documents where every fetch must be re-authorized
against current entity permissions.

### Single attachment per entity

When each record has exactly one file:

```
@version(1)
@display("title")
@access(read: ["member", "admin"], write: ["admin"], delete: ["admin"])
@hook(before_upload) """Reject uploads during scheduled read-only windows."""
@hook(after_upload) """Enqueue the attachment on the AV scanner."""
@hook(on_scan_complete) """Audit-log the terminal scan verdict."""
schema Contract {
    title:        text(max: 500) required indexed @list(primary)
    status:       enum("draft", "under_review", "signed")
                    @enum_colors(
                        draft: "gray",
                        under_review: "amber",
                        signed: "green"
                    )
    counterparty: -> Organization required
    pdf:          file(
                      bucket: "documents",
                      max_size: "25MB",
                      mime: ["application/pdf"],
                      access: "proxied"
                  ) required
    signed_at:    datetime
}
```

Note the three hooks work together: `before_upload` is blocking (can 422
the mint), `after_upload` dispatches the scan, and `on_scan_complete`
observes the verdict the scanner posts back through `/scan-complete`.
Without any hook annotations the runtime transitions `scanning →
available` automatically at confirm time — fine for dev, wrong for
production with untrusted uploaders.

### Many attachments per entity

`file[]` is not supported in v1. Model it as a sibling schema with a
relation back to the parent:

```
@display("filename")
@access(read: ["member"], write: ["member"], delete: ["admin"])
schema ContractAttachment {
    filename:  text(max: 500) required indexed
    contract:  -> Contract required
    blob:      file(
                   bucket: "documents",
                   max_size: "50MB",
                   mime: ["application/pdf", "image/*"]
               ) required
    uploaded_by: text required @owner
}

@display("title")
schema Contract {
    title:        text(max: 500) required indexed
    counterparty: -> Organization required
    attachments:  -> ContractAttachment[]   // derived inverse collection
}
```

The `attachments` field is a derived inverse view, so reads return
populated arrays automatically and writes go through `ContractAttachment`
as normal entity creates.

### Classification-aware access modes

Mix `presigned` and `proxied` modes within the same schema based on the
sensitivity of each attachment:

```
@access(read: ["member"], write: ["member"], delete: ["admin"])
schema CaseFile {
    title:           text(max: 500) required indexed @list(primary)

    // Thumbnail — public-ish, presigned is fine
    cover:           file(bucket: "media", max_size: "2MB",
                          mime: ["image/jpeg", "image/png"],
                          access: "presigned")

    // Body of the case file — classified, every fetch re-authorizes
    classified_doc:  file(bucket: "evidence", max_size: "250MB",
                          mime: ["application/pdf"],
                          access: "proxied")
                     @field_access(read: ["investigator", "admin"],
                                   write: ["investigator"])

    owner_id:        text required @owner
}
```

`@field_access` gates visibility of the attachment metadata itself —
users without `investigator` won't even see `classified_doc.key` in
entity responses, and download requests 403 at the schema-level check
before any S3 call.

### Backend selection per field

Declare multiple backends when different fields belong in different
buckets (retention tier, region, blast radius):

```toml
# config.toml
[schema_forge.storage.backends.assets]
endpoint = "https://<r2-account>.r2.cloudflarestorage.com"
region = "auto"
bucket = "forge-assets"

[schema_forge.storage.backends.evidence]
region = "us-east-1"
bucket = "forge-evidence"      # IAM role, no keys
presign_ttl_secs = 60          # tighter TTL for sensitive bucket
```

```
schema Evidence {
    case:        -> Case required
    thumbnail:   file(bucket: "assets",   max_size: "2MB",  mime: ["image/*"])
    raw_capture: file(bucket: "evidence", max_size: "5GB",  mime: ["application/octet-stream"])
}
```

Startup validates every `bucket:` resolves to a configured backend — a
typo fails the server to start, never at first upload.
