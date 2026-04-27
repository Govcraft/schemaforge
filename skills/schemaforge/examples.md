# SchemaForge DSL — Annotated Examples

## Minimal Schema

The simplest valid schema — one entity with basic fields:

```
schema Contact {
    name:    text(max: 255) required indexed
    email:   text required
    active:  boolean default(true)
}
```

**Key points:**
- PascalCase schema name
- snake_case field names
- At least one field required
- `required indexed` modifiers on lookup fields
- `default(true)` for sensible defaults

## CRM Domain

A complete CRM with companies, contacts, and deals demonstrating relations, enums, composites, and dashboards.

### Company

```
@display("name")
@dashboard(widgets: ["count"])
@access(
    read: ["sales", "marketing", "finance", "manager", "admin"],
    write: ["sales", "admin"],
    delete: ["admin"]
)
schema Company {
    name:           text(max: 255) required indexed
    domain:         text(max: 255) indexed @widget("url")
    industry:       enum("technology", "finance", "healthcare", "education",
                         "retail", "manufacturing", "consulting", "other")
                    @widget("status_badge")
    size:           enum("startup", "smb", "mid_market", "enterprise")
                    @widget("status_badge")
    employee_count: integer(min: 0)
    annual_revenue: float(precision: 2)
                    @field_access(read: ["finance", "sales", "admin"], write: ["finance"])
                    @format("currency")
    website:        text @widget("url")
    description:    richtext
    headquarters:   composite {
        street:     text
        city:       text required
        state:      text
        postal_code: text(max: 20)
        country:    text(max: 100) required
    }
    contacts:       -> Contact[]
    tags:           text[] @widget("tags")
    metadata:       json
    owner_id:       text @owner
    active:         boolean default(true)
}
```

**Notable features:**
- `@display("name")` — records show by company name
- `@dashboard(widgets: ["count"])` — count widget on dashboard
- `@access` — sales/marketing can read, only sales/admin can write
- `@field_access` on `annual_revenue` — finance-restricted field
- `composite` for `headquarters` — nested address object
- `-> Contact[]` — derived inverse view: paired with `Contact.company`
  below, so `Company.contacts` is resolved at read time by querying
  Contact filtered on the company FK. No column on Company; writes are
  rejected, set `Contact.company` on the child instead.
- `@widget` and `@format` — UI rendering hints

### Contact

```
@display("full_name")
@dashboard(widgets: ["count"])
@access(
    read: ["sales", "marketing", "manager", "admin"],
    write: ["sales", "admin"],
    delete: ["admin"]
)
schema Contact {
    full_name:        text(max: 255) required indexed
    email:            text(max: 512) indexed @widget("email")
    phone:            text(max: 50) @widget("phone")
    title:            text(max: 255)
    company:          -> Company
    source:           enum("inbound", "outbound", "referral", "event", "website", "other")
                      default("other") @widget("status_badge")
    lead_score:       integer(min: 0, max: 100) default(0) @widget("progress")
    lifecycle_stage:  enum("subscriber", "lead", "mql", "sql", "opportunity",
                           "customer", "evangelist")
                      default("lead") @widget("status_badge") @kanban_column
    last_contacted:   datetime @format("relative")
    tags:             text[] @widget("tags")
    social_profiles:  composite {
        linkedin:     text
        twitter:      text
        github:       text
    }
    notes:            richtext
    owner_id:         text @owner
    active:           boolean default(true)
}
```

**Notable features:**
- `lead_score` with bounded integer and progress widget
- `lifecycle_stage` as `@kanban_column` — enables kanban view
- `-> Company` — many-to-one relation (each contact belongs to one company)
- `social_profiles` composite — clean nested structure

### Deal (Pipeline)

```
@version(3)
@display("name")
@dashboard(
    widgets: ["count", "sum:value", "avg:value"],
    layout: "kanban",
    group_by: "stage",
    sort_default: "-expected_close"
)
@access(
    read: ["sales", "finance", "manager", "admin"],
    write: ["sales", "admin"],
    delete: ["admin"]
)
@webhook(events: ["created", "updated"])
schema Deal {
    name:           text(max: 255) required indexed
    value:          float(precision: 2) required
                    @field_access(read: ["sales", "finance", "admin"], write: ["sales"])
                    @format("currency")
    stage:          enum("prospecting", "qualification", "proposal",
                         "negotiation", "closed_won", "closed_lost")
                    default("prospecting") @widget("status_badge") @kanban_column
    probability:    integer(min: 0, max: 100) default(10)
                    @widget("progress") @format("percent")
    expected_close: datetime @format("relative")
    contact:        -> Contact required
    company:        -> Company
    assigned_to:    -> Employee
    notes:          richtext
    owner_id:       text @owner
    active:         boolean default(true)
}
```

**Notable features:**
- `@version(3)` — schema has been through 3 iterations
- `@dashboard` with aggregation widgets (`sum:value`, `avg:value`)
- `layout: "kanban"` + `group_by: "stage"` — pipeline view
- `@webhook(events: ["created", "updated"])` — fires webhook on create/update
- `sort_default: "-expected_close"` — descending sort (prefix `-`)
- `@kanban_column` on `stage` — the kanban grouping field
- Multiple relations: `-> Contact required`, `-> Company`, `-> Employee`

## Multi-Tenant Setup

Organization as tenant root, departments scoped to organization:

```
@tenant(root)
@display("name")
@dashboard(widgets: ["count"])
@access(read: ["member", "admin"], write: ["admin"], delete: ["admin"])
schema Organization {
    name:          text(max: 255) required indexed
    slug:          text(max: 100) required indexed
    billing_email: text(max: 512) @widget("email")
    plan:          enum("free", "starter", "business", "enterprise")
                   default("free") @widget("status_badge")
    max_seats:     integer(min: 1) default(5)
    logo_url:      text @widget("image")
    settings:      json
    active:        boolean default(true)
    owner_id:      text required @owner
}

@tenant(parent: "Organization")
@display("name")
@access(
    read: ["member", "manager", "admin"],
    write: ["manager", "admin"],
    delete: ["admin"]
)
schema Department {
    name:            text(max: 255) required
    code:            text(max: 20) required indexed
    description:     text
    head:            -> Employee
    parent_org:      -> Organization required
    budget:          float(precision: 2)
                     @field_access(read: ["finance", "manager", "admin"], write: ["finance", "admin"])
                     @format("currency")
    headcount_limit: integer(min: 0) default(50)
    active:          boolean default(true)
}
```

**Key pattern:** `@tenant(root)` on Organization, `@tenant(parent: "Organization")` on Department. All department data is automatically scoped to its organization.

## HR Schema with Field-Level Access Control

Employee records with sensitive fields restricted to HR:

```
@version(2)
@display("full_name")
@access(
    read: ["member", "hr", "manager", "admin"],
    write: ["hr", "admin"],
    delete: ["admin"]
)
schema Employee {
    full_name:        text(max: 255) required indexed
    email:            text(max: 512) required indexed @widget("email")
    phone:            text(max: 50) @widget("phone")
    title:            text(max: 255)
    department:       -> Department
    manager:          -> Employee
    hire_date:        datetime required @format("relative")
    salary:           float(precision: 2)
                      @field_access(read: ["hr", "admin"], write: ["hr", "admin"])
                      @format("currency")
    ssn_last_four:    text(max: 4)
                      @field_access(read: ["hr", "admin"], write: ["hr"])
    employment_type:  enum("full_time", "part_time", "contractor", "intern")
                      default("full_time") @widget("status_badge")
    status:           enum("active", "on_leave", "terminated")
                      default("active") @widget("status_badge")
    skills:           text[]
    emergency_contact: composite {
        name:         text required
        phone:        text required
        relationship: text
    }
    home_address:     composite {
        street:       text
        city:         text required
        state:        text
        postal_code:  text(max: 20)
        country:      text(max: 100) required
    }
    owner_id:         text @owner
    active:           boolean default(true)
}
```

**Key pattern:** Schema-level `@access` gives broad read access, while `@field_access` on `salary` and `ssn_last_four` restricts those specific fields to HR roles only.

## System Schema

Protected system schemas auto-created at startup:

```
@system
@display("name")
schema Workflow {
    name:             text(max: 255) required indexed
    description:      text
    target_schema:    text(max: 100) required
    trigger_field:    text(max: 100) required
    rules:            json required
    enabled:          boolean default(true)
    execution_count:  integer(min: 0) default(0)
    last_executed:    datetime
}
```

**Key pattern:** `@system` makes this a protected entity — it exists for infrastructure, not user-managed content.

## Project Management with Task Dependencies

Tasks with self-referencing relations and array enums:

```
@display("title")
@dashboard(widgets: ["count"], layout: "kanban", group_by: "status")
@access(
    read: ["member", "manager", "admin"],
    write: ["member", "manager", "admin"],
    delete: ["manager", "admin"]
)
schema Task {
    title:           text(max: 500) required indexed
    description:     richtext @widget("markdown")
    status:          enum("backlog", "todo", "in_progress", "in_review",
                          "done", "cancelled")
                     default("backlog") @widget("status_badge") @kanban_column
    priority:        enum("critical", "high", "medium", "low")
                     default("medium") @widget("status_badge")
    story_points:    integer(min: 0, max: 100) @widget("count_badge")
    due_date:        datetime @format("relative")
    estimated_hours: float(precision: 1)
    actual_hours:    float(precision: 1)
    project:         -> Project required
    assignee:        -> Employee
    reviewer:        -> Employee
    blocked_by:      -> Task[]
    tags:            text[] @widget("tags")
    labels:          enum("bug", "feature", "improvement",
                          "documentation", "infrastructure")[]
                     @widget("tags")
    owner_id:        text @owner
}
```

**Notable features:**
- `blocked_by: -> Task[]` — self-referencing many relation for dependencies.
  This is NOT derived (Task has no single `-> Task` FK back), so it stays
  as a stored array of refs and is writable as a normal field.
- `labels: enum(...)[]` — array of enum values
- Multiple relation targets: `-> Project`, `-> Employee`, `-> Task[]`
- Kanban dashboard grouped by status

## Example 5: Contract Lifecycle with File Attachments and Scanner Integration

Government-facing contract workflow. Each `Contract` carries a single signed
PDF backed by S3 storage. The supporting `Proposal` sidecar schema lets a
deal carry an arbitrary number of proposal documents (since `file[]` is not
supported in v1 — use a related schema instead). Uploads are gated by a
scanner hook so no file becomes downloadable until it clears AV.

```
// Matching config.toml fragment:
//
// [schema_forge.storage]
// default_presign_ttl_secs = 300
//
// [schema_forge.storage.backends.contracts]
// region = "us-east-1"
// bucket = "forge-contracts"
// # IAM role — no keys
//
// [schema_forge.storage.backends.proposals]
// region = "us-east-1"
// bucket = "forge-proposals"
//
// [schema_forge.hooks]
// enabled = true
// default_timeout_ms = 5000
//
// [[schema_forge.hooks.bindings]]
// schema   = "Contract"
// event    = "AfterUpload"
// endpoint = "http://scanner-service:9090"
// required = true
// descriptor_path = "/var/lib/schemaforge/hooks_descriptor.bin"

@version(1)
@display("name")
@tenant(root)
schema Organization {
    name:      text(max: 255) required indexed
    slug:      text(max: 100) required indexed
    owner_id:  text required @owner
    active:    boolean default(true)
}

@version(1)
@display("title")
@tenant(parent: "Organization")
@access(
    read:   ["member", "admin"],
    write:  ["contracts_officer", "admin"],
    delete: ["admin"]
)
@dashboard(
    widgets: ["count", "sum:value"],
    layout:  "kanban",
    group_by: "status",
    sort_default: "-effective_date"
)
@hook(before_upload) """Reject signed PDFs during the nightly maintenance window."""
@hook(after_upload) """Enqueue the contract PDF on the ClamAV scanner."""
@hook(on_scan_complete) """Audit-log the scanner verdict and notify the owning officer."""
schema Contract {
    title:          text(max: 500) required indexed @list(primary)
    status:         enum("draft", "under_review", "executed", "terminated")
                    default("draft")
                    @widget("status_badge") @kanban_column
                    @enum_colors(
                        draft:        "gray",
                        under_review: "amber",
                        executed:     "green",
                        terminated:   "red"
                    )
    value:          float(precision: 2) @format("currency") @list(column)
    effective_date: datetime @format("date") @list(column)
    counterparty:   -> Organization required
    // Single binding signed PDF — proxied so every fetch re-checks authz.
    signed_pdf:     file(
                        bucket: "contracts",
                        max_size: "50MB",
                        mime: ["application/pdf"],
                        access: "proxied"
                    )
                    @field_access(
                        read:  ["member", "contracts_officer", "admin"],
                        write: ["contracts_officer", "admin"]
                    )
    proposals:      -> Proposal[]              // inverse derived
    owner_id:       text required @owner
}

@version(1)
@display("filename")
@tenant(parent: "Organization")
@access(read: ["member"], write: ["member"], delete: ["admin"])
@hook(after_upload) """Enqueue the proposal on the scanner; share dispatcher config with Contract."""
@hook(on_scan_complete) """Audit-log scanner verdict."""
schema Proposal {
    filename:  text(max: 500) required indexed @list(primary)
    contract:  -> Contract required
    // Presigned is fine for non-classified proposals; S3 handles the bandwidth.
    document:  file(
                   bucket: "proposals",
                   max_size: "100MB",
                   mime: ["application/pdf", "image/*"]
               ) required
    submitted_by: text required @owner
    notes:     richtext
}
```

**Notable features:**

- **One-to-one attachment** on `Contract.signed_pdf` — `required` is omitted so
  the contract can exist in `draft` before anyone uploads. The attachment
  metadata is gated by `@field_access` (not visible to members until an
  officer adds it; `salary`-style containment).
- **One-to-many attachments** via the `Proposal` sidecar. `contract.proposals`
  is a derived inverse collection, so entity responses include proposals
  without a second API call. Writes to `proposals` are rejected (422) — add
  a `Proposal` directly instead.
- **`proxied` on sensitive documents** (`signed_pdf`), **`presigned` on bulk
  uploads** (`proposals.document`). Different fields within the same
  deployment use different bandwidth/security trade-offs.
- **Different buckets per classification.** `contracts` and `proposals` are
  declared as separate backends; lifecycle rules and access policies can
  diverge.
- **Scanner pipeline via hooks.** `after_upload` dispatches the scan (fires
  detached, carries a short-TTL `download_url`); the scanner's gRPC service
  runs ClamAV against the URL, then POSTs the verdict to
  `POST /schemas/Contract/entities/{id}/fields/signed_pdf/scan-complete`.
  That transitions the attachment to `available` or `quarantined` and
  synchronously fires `on_scan_complete` for audit. Without the
  `on_scan_complete` hook, the runtime would skip scanning entirely and
  transition straight to `available` — deliberate for dev, wrong for prod.
- **Tenant scoping** under `Organization` — every object key is prefixed with
  the tenant entity id, so retention rules can target a single tenant's
  objects.

Client-side upload flow (TypeScript, using `fetch`):

```ts
// 1. Mint upload URL.
const mint = await fetch(
  `/api/v1/forge/schemas/Contract/entities/${id}/fields/signed_pdf/upload-url`,
  {
    method: "POST",
    headers: { "Content-Type": "application/json", Authorization: bearer },
    body: JSON.stringify({
      filename: file.name,
      mime: file.type,        // must match allowlist
      size:  file.size,
    }),
  },
).then((r) => r.json())

// 2. Upload bytes directly to S3. No runtime hop.
await fetch(mint.upload_url, {
  method: "PUT",
  headers: mint.headers,      // includes exact Content-Type from step 1
  body: file,
})

// 3. Confirm and persist the attachment onto the entity.
const confirm = await fetch(
  `/api/v1/forge/schemas/Contract/entities/${id}/fields/signed_pdf/confirm-upload`,
  {
    method: "POST",
    headers: { "Content-Type": "application/json", Authorization: bearer },
    body: JSON.stringify({ key: mint.key }),
  },
).then((r) => r.json())

// confirm.attachment.status === "scanning" — poll /api/v1/forge/schemas/Contract/entities/{id}
// until signed_pdf.status === "available" (or display a status chip).
```
