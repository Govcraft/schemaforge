# SchemaForge Widget UI Reference

SchemaForge serves data endpoints that return either **HTML fragments** (default) or **JSON** via `Accept` header content negotiation. All routes live under `/forge/` and are gated behind the `widget-ui` Cargo feature on `schema-forge-acton`.

You own layout, navigation, auth UI, and styling. SchemaForge provides the data.

---

## Table of Contents

1. [Quick Start](#1-quick-start)
2. [Endpoints](#2-endpoints)
3. [Content Negotiation](#3-content-negotiation)
4. [HTMX Interaction Patterns](#4-htmx-interaction-patterns)
5. [Data Shapes](#5-data-shapes)
6. [Field Input Types](#6-field-input-types)
7. [Widget Types (Display)](#7-widget-types-display)
8. [Badge Status Classification](#8-badge-status-classification)
9. [Annotations That Affect Rendering](#9-annotations-that-affect-rendering)
10. [Styling with `data-sf` Attributes](#10-styling-with-data-sf-attributes)
11. [Authentication](#11-authentication)
12. [Building Navigation](#12-building-navigation)
13. [Template File Inventory](#13-template-file-inventory)
14. [Error Responses](#14-error-responses)

---

## 1. Quick Start

### Build with the `widget-ui` feature

```bash
cargo build -p schema-forge-cli --features widget-ui
```

### Define a schema

Create a `schemas/` directory with a `.schema` file:

```
# schemas/Contact.schema
schema Contact {
    @display
    full_name: Text

    email: Text
    status: Enum(active, inactive, pending) @widget("status_badge")
    company: Relation(Company, One)
}

schema Company {
    @display
    name: Text

    industry: Text
}
```

### Start the server

```bash
# In-memory database (development)
schema-forge serve --db-url "mem://"

# Remote SurrealDB
schema-forge serve --db-url "ws://localhost:8000"
```

The server starts on `http://127.0.0.1:3000` by default. CLI options:

| Flag | Env Var | Default | Description |
|------|---------|---------|-------------|
| `--host` / `-H` | -- | `127.0.0.1` | Bind address |
| `--port` / `-p` | -- | `3000` | Listen port |
| `--schemas` | -- | `schemas/` | Schema file directory |
| `--db-url` | `SCHEMA_FORGE_DB_URL` | -- | SurrealDB connection URL |
| `--template-dir` | `FORGE_TEMPLATE_DIR` | -- | Override template directory |

### Embed fragments in your HTML page

```html
<!DOCTYPE html>
<html>
<head>
  <title>My App</title>
  <script src="https://unpkg.com/htmx.org@2"></script>
  <style>
    /* Style SchemaForge fragments with data-sf selectors */
    [data-sf="entity-list"] { width: 100%; border-collapse: collapse; }
    [data-sf="entity-list"] td { padding: 0.5rem; border-top: 1px solid #e5e7eb; }
    [data-sf="badge"] { padding: 0.125rem 0.5rem; border-radius: 9999px; font-size: 0.75rem; }
    [data-sf="badge"][data-status="success"] { background: #dcfce7; color: #166534; }
  </style>
</head>
<body>
  <h1>Contacts</h1>

  <!-- Load the entity list fragment -->
  <div hx-get="/forge/Contact/entities" hx-trigger="load" hx-target="this">
    Loading...
  </div>

  <h2>Add Contact</h2>

  <!-- Load the create form fragment -->
  <div hx-get="/forge/Contact/entities/new" hx-trigger="load" hx-target="this">
    Loading form...
  </div>
</body>
</html>
```

### Or use the JSON API

```bash
# List entities
curl -H "Accept: application/json" http://localhost:3000/forge/Contact/entities

# Create an entity
curl -X POST http://localhost:3000/forge/Contact/entities \
  -d "full_name=Jane+Doe&email=jane@example.com&status=active"

# Get JSON detail
curl -H "Accept: application/json" http://localhost:3000/forge/Contact/entities/Contact:abc123
```

---

## 2. Endpoints

All routes live under `/forge` and return **bare HTML fragments** by default (no `<html>`, no layout). Send `Accept: application/json` to get JSON instead.

Auth is applied via `register_widget_routes()`, which wraps all routes with a shared session layer and a `session_to_claims` middleware that bridges browser sessions into `Claims` for access control. PASETO token auth is also supported for API consumers. See [Authentication](#11-authentication) for details.

### GET `/forge/{schema}/entities` -- Entity List

Returns a paginated table of entities.

**Path params:**
| Param | Type | Example |
|-------|------|---------|
| `schema` | String | `Contact` |

**Query params:**
| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `limit` | usize | 25 | Page size |
| `offset` | usize | 0 | Starting offset |

**HTML response:**
```html
<div data-sf="entity-table" id="forge-table-Contact">
  <table data-sf="entity-list">
    <thead><tr><!-- field headers + Actions --></tr></thead>
    <tbody>
      <tr data-entity-id="{id}"><!-- field values + View/Edit/Delete --></tr>
    </tbody>
  </table>
  <nav data-sf="pagination"><!-- Previous / Next --></nav>
</div>
```

**JSON response:**
```json
{
  "schema": {
    "name": "Contact",
    "display_field": "full_name",
    "version": 1,
    "fields": [...],
    "url_name": "Contact"
  },
  "entities": [
    {
      "id": "Contact:abc123",
      "display_value": "John Doe",
      "field_values": [...],
      "summary": [...]
    }
  ],
  "pagination": {
    "current_page": 1,
    "total_pages": 2,
    "total_count": 42,
    "limit": 25,
    "offset": 0,
    "has_next": true,
    "has_previous": false,
    "end_showing": 25,
    "previous_offset": 0,
    "next_offset": 25
  }
}
```

---

### GET `/forge/{schema}/entities/_table` -- Pagination Fragment

Identical to the entity list endpoint. Exists as a dedicated HTMX target for pagination links.

---

### GET `/forge/{schema}/entities/new` -- Create Form

Returns an empty entity form.

**HTML response:**
```html
<div data-sf="entity-form" id="forge-form-Contact">
  <form hx-post="/forge/Contact/entities"
        hx-target="closest [data-sf='entity-form']"
        hx-swap="outerHTML">
    <div data-sf="field" data-field-name="name" data-input-type="text">
      <label for="field-name">Name *</label>
      <input type="text" id="field-name" name="name" required>
    </div>
    <!-- more fields -->
    <div data-sf="form-actions">
      <button type="submit">Create</button>
    </div>
  </form>
</div>
```

**JSON response:**
```json
{
  "schema": { ... },
  "fields": [
    {
      "name": "email",
      "label": "Email",
      "input_type": "text",
      "required": true,
      "options": [],
      "multiple": false,
      "children": [],
      "type_label": "Text(max: 255)",
      "default_value": null,
      "current_value": null,
      "relation_target": null
    }
  ]
}
```

---

### POST `/forge/{schema}/entities` -- Create Entity

Submits a new entity via URL-encoded form data.

**Request body:** `application/x-www-form-urlencoded`
```
name=John+Doe&email=john%40example.com&status=active
```

Form field names match schema field names (snake_case). Special cases:
- **Boolean:** Absent checkbox = `false`. Value `"true"` or `"on"` = `true`.
- **Relation (Many):** Multiple values with same name: `tags=tag:1&tags=tag:2`
- **Composite:** Dot-notation: `address.street=123+Main&address.city=NYC`
- **DateTime:** Accepts `YYYY-MM-DDTHH:MM`, `YYYY-MM-DDTHH:MM:SS`, or RFC3339

**HTML on success (200):** Returns entity detail fragment, replacing the form.
```html
<article data-sf="entity-detail" id="forge-detail-Contact-{id}">
  <!-- entity detail with field values -->
</article>
```

**HTML on validation error (422):** Returns the form again with errors populated.
```html
<div data-sf="entity-form" id="forge-form-Contact">
  <div data-sf="errors">
    <ul><li>Field 'email' is required</li></ul>
  </div>
  <form ...><!-- fields with current_value preserved --></form>
</div>
```

**JSON on success (200):**
```json
{
  "schema": { ... },
  "entity": {
    "id": "Contact:abc123",
    "display_value": "John Doe",
    "field_values": [...],
    "summary": [...]
  }
}
```

**JSON on validation error (422):**
```json
{
  "errors": ["Field 'email' is required"]
}
```

---

### GET `/forge/{schema}/entities/{id}` -- Entity Detail

Returns an entity detail view.

**HTML response:**
```html
<article data-sf="entity-detail" id="forge-detail-Contact-{id}">
  <header>
    <h3>{display_value}</h3>
    <span>ID: {id}</span>
  </header>
  <dl>
    <dt>{field.label}</dt>
    <dd data-field-name="{field.name}">{field value}</dd>
  </dl>
  <footer>
    <time datetime="{created_at}">Created: {created_at}</time>
    <time datetime="{updated_at}">Updated: {updated_at}</time>
  </footer>
  <nav>
    <a href="...">Edit</a>
    <button hx-delete="..." hx-confirm="Delete this entity?">Delete</button>
  </nav>
</article>
```

**JSON response:**
```json
{
  "schema": { ... },
  "entity": {
    "id": "Contact:abc123",
    "display_value": "John Doe",
    "field_values": [
      {
        "name": "status",
        "label": "Status",
        "value": "Active",
        "raw_value": "active",
        "widget_type": "status_badge",
        "field_type": "enum",
        "badge_class": "success",
        "is_empty": false
      }
    ],
    "summary": [...]
  }
}
```

---

### GET `/forge/{schema}/entities/{id}/edit` -- Edit Form

Returns a pre-populated edit form.

**HTML response:** Same structure as create form, but with:
- `hx-put` instead of `hx-post`
- Fields pre-filled with `current_value`
- Submit button says "Update"

**JSON response:**
```json
{
  "schema": { ... },
  "fields": [...],
  "entity_id": "Contact:abc123"
}
```

---

### PUT `/forge/{schema}/entities/{id}` -- Update Entity

Same request/response pattern as create:
- **On success (200):** Returns updated entity detail (HTML or JSON)
- **On validation error (422):** Returns form with errors (HTML) or `{ "errors": [...] }` (JSON)

---

### DELETE `/forge/{schema}/entities/{id}` -- Delete Entity

**Response:** `200 OK` with empty body.

The HTMX `hx-swap="delete"` on the calling button removes the target element from the DOM.

---

### GET `/forge/{schema}/relation-options/{field}` -- Relation Options

Returns options for populating a relation select field.

**Path params:**
| Param | Type | Description |
|-------|------|-------------|
| `schema` | String | Target schema name (the relation target, not the source) |
| `field` | String | Field name (routing context only) |

**HTML response:** Raw `<option>` elements (no wrapper div)
```html
<option value="">-- Select --</option>
<option value="company:abc123">Acme Corp</option>
<option value="company:def456">Widgets Inc</option>
```

**JSON response:**
```json
[
  { "value": "company:abc123", "label": "Acme Corp" },
  { "value": "company:def456", "label": "Widgets Inc" }
]
```

Fetches up to 100 entities. Uses `@display` annotation field for labels, falls back to first Text field, then entity ID.

---

## 3. Content Negotiation

All endpoints support content negotiation via the `Accept` header:

| Accept Header | Response Format |
|---|---|
| `text/html` (default) | HTMX-ready HTML fragments |
| `application/json` | JSON objects |
| (missing/other) | HTML fragments |

```bash
# Get HTML fragment (default)
curl http://localhost:3000/forge/Contact/entities

# Get JSON
curl -H "Accept: application/json" http://localhost:3000/forge/Contact/entities
```

The JSON responses use the same view structs as the HTML templates, so field names and structure are identical. See [Data Shapes](#5-data-shapes) for details.

---

## 4. HTMX Interaction Patterns

### Fragment Lifecycle

```
1. Load list:     GET /forge/{schema}/entities
                  -> <div data-sf="entity-table"> table fragment </div>

2. Paginate:      hx-get="/forge/{schema}/entities/_table?limit=25&offset=25"
                  hx-target="#entity-table"
                  hx-swap="innerHTML"

3. Create:        GET /forge/{schema}/entities/new
                  -> <div data-sf="entity-form"> form </div>

                  POST /forge/{schema}/entities  (form submit)
                  hx-target="closest [data-sf='entity-form']"
                  hx-swap="outerHTML"
                  -> success: detail card replaces form
                  -> error:   form re-renders with errors (422)

4. View detail:   GET /forge/{schema}/entities/{id}
                  -> <article data-sf="entity-detail"> ... </article>

5. Edit:          hx-get="/forge/{schema}/entities/{id}/edit"
                  hx-target="closest [data-sf='entity-detail']"
                  hx-swap="outerHTML"
                  -> detail card replaced by edit form

                  PUT /forge/{schema}/entities/{id}  (form submit)
                  hx-target="closest [data-sf='entity-form']"
                  hx-swap="outerHTML"
                  -> success: detail card replaces form
                  -> error:   form re-renders with errors (422)

6. Delete:        hx-delete="/forge/{schema}/entities/{id}"
                  hx-confirm="Delete this entity?"
                  hx-target="closest tr" (or closest [data-sf='entity-detail'])
                  hx-swap="delete"
                  -> 200 OK, element removed from DOM

7. Relation opts: hx-get="/forge/{target}/relation-options/{field}"
                  hx-trigger="load"
                  hx-target="this"  (the <select> element)
                  hx-swap="innerHTML"
                  -> <option> elements loaded on page render
```

### Relation Select Loading

Relation select fields use `hx-trigger="load"` to fetch options when the form renders:

```html
<select name="company"
        hx-get="/forge/Company/relation-options/company"
        hx-trigger="load"
        hx-target="this"
        hx-swap="innerHTML">
  <option>Loading...</option>
</select>
```

---

## 5. Data Shapes

These are the Rust structs serialized as both template context and JSON responses. Field names become template variables and JSON keys.

### SchemaView

```json
{
  "name": "Contact",
  "display_field": "full_name",
  "version": 1,
  "fields": [ FieldView... ],
  "url_name": "Contact"
}
```

### EntityView

```json
{
  "id": "Contact:abc123",
  "display_value": "John Doe",
  "field_values": [ FieldDisplayView... ],
  "summary": [ FieldDisplayView... ]
}
```

`summary` contains max 3 important fields (widget-annotated fields first).

### FieldView (form inputs)

```json
{
  "name": "email",
  "label": "Email",
  "input_type": "text",
  "attrs": [["maxlength", "255"]],
  "required": true,
  "options": [["active", "Active"], ["inactive", "Inactive"]],
  "multiple": false,
  "children": [],
  "type_label": "Text(max: 255)",
  "default_value": null,
  "current_value": null,
  "relation_target": null
}
```

### FieldDisplayView (read-only display)

```json
{
  "name": "status",
  "label": "Status",
  "value": "Active",
  "raw_value": "active",
  "widget_type": "status_badge",
  "field_type": "enum",
  "badge_class": "success",
  "is_empty": false
}
```

### PaginationView

```json
{
  "current_page": 1,
  "total_pages": 2,
  "total_count": 42,
  "limit": 25,
  "offset": 0,
  "has_previous": false,
  "has_next": true,
  "end_showing": 25,
  "previous_offset": 0,
  "next_offset": 25
}
```

---

## 6. Field Input Types

The `input_type` field on `FieldView` determines which form input is rendered.

| `input_type` | FieldType | HTML element | Notable attributes |
|---|---|---|---|
| `text` | Text | `<input type="text">` | `maxlength` if constraint set |
| `textarea` | RichText | `<textarea>` | `rows="6"` |
| `number` | Integer | `<input type="number">` | `step="1"`, `min`, `max` |
| `number` | Float | `<input type="number">` | `step` from precision (e.g. `"0.01"`) or `"any"` |
| `checkbox` | Boolean | `<input type="checkbox">` | Hidden input `value="false"` + checkbox `value="true"` |
| `datetime-local` | DateTime | `<input type="datetime-local">` | -- |
| `select` | Enum | `<select>` | Static `<option>` from variants |
| `select` | Relation(One) | `<select>` | `relation_target` set, options loaded via HTMX |
| `select` | Relation(Many) | `<select multiple>` | `multiple=true`, `relation_target` set |
| `json` | Json | `<textarea>` | `rows="6"`, `placeholder="Enter JSON..."` |
| `array` | Array | `<textarea>` | `placeholder="One value per line"` |
| `composite` | Composite | `<fieldset>` | Nested child inputs with dot-notation names |

---

## 7. Widget Types (Display)

The `@widget` annotation on a field controls how its value is rendered in read-only views.

| `widget_type` | Rendering | `data-sf` attribute |
|---|---|---|
| `status_badge` | `<span data-sf="badge" data-status="{status}">` | `badge` |
| `progress` | `<progress data-sf="progress" value="..." max="100">` | `progress` |
| `relative_time` | `<time data-sf="relative-time" datetime="...">` | `relative-time` |
| `link` | `<a data-sf="link" href="..." target="_blank">` | `link` |
| `email` | `<a data-sf="email" href="mailto:...">` | `email` |
| `tags` | Split by `", "` into `<span data-sf="tag">` | `tag` |
| `image` | `<img data-sf="image" loading="lazy">` | `image` |
| `code` | `<code data-sf="code">` | `code` |
| (none) | Plain `{{ field.value }}` text | -- |

---

## 8. Badge Status Classification

When `widget_type` is `status_badge`, the `badge_class` is auto-assigned based on the field value:

| Status | Matched values |
|---|---|
| `success` | active, done, completed, closed_won, approved, published, resolved, won, hired, accepted |
| `error` | inactive, terminated, cancelled, closed_lost, rejected, lost, fired, declined, failed |
| `warning` | pending, on_hold, in_review, draft, on_leave, paused, waiting, suspended |
| `info` | in_progress, proposal, negotiation, qualification, todo, prospecting, open, new, interview, review |
| `neutral` | backlog, archived, other, closed, unknown |

Unrecognized values get a deterministic hash to one of the 5 statuses.

In HTML, the status is exposed as `data-status="{status}"` on the badge element. In JSON, it appears as the `badge_class` field.

---

## 9. Annotations That Affect Rendering

| Annotation | Effect |
|---|---|
| `@display` | Sets which field value is used as the entity label |
| `@widget("type")` | Controls field display rendering (see section 7) |
| `@format("hint")` | Formats numeric values: `"currency:$"`, `"currency:EUR"`, `"percent"` |
| `@system` | Excludes schema from navigation listings |
| `@access(read: [...])` | Controls which roles can see the schema and access its data |
| `@field_access(read: [...])` | Controls which roles can see specific field values |

---

## 10. Styling with `data-sf` Attributes

HTML fragments use semantic `data-sf` attributes instead of CSS classes, giving you full control over styling. Here is the complete attribute vocabulary:

### Structural attributes

| Attribute | Element | Description |
|---|---|---|
| `data-sf="entity-table"` | `<div>` | Entity list wrapper |
| `data-sf="entity-list"` | `<table>` | Entity list table |
| `data-sf="entity-detail"` | `<article>` | Entity detail wrapper |
| `data-sf="entity-form"` | `<div>` | Entity form wrapper |
| `data-sf="form-actions"` | `<div>` | Form submit button container |
| `data-sf="pagination"` | `<nav>` | Pagination controls |
| `data-sf="empty-state"` | `<div>` | Empty state placeholder |
| `data-sf="errors"` | `<div>` | Validation error list |
| `data-sf="field"` | `<div>` | Form field wrapper |

### Data attributes on fields

| Attribute | Element | Description |
|---|---|---|
| `data-field-name="{name}"` | `<div>`, `<td>`, `<dd>` | Field name for targeting |
| `data-input-type="{type}"` | `<div data-sf="field">` | Input type (text, select, etc.) |
| `data-entity-id="{id}"` | `<tr>` | Entity record ID |

### Display widget attributes

| Attribute | Element | Description |
|---|---|---|
| `data-sf="badge"` | `<span>` | Status badge |
| `data-status="{status}"` | `<span data-sf="badge">` | Badge status (success, error, warning, info, neutral) |
| `data-sf="progress"` | `<progress>` | Progress bar |
| `data-sf="relative-time"` | `<time>` | Relative time display |
| `data-sf="link"` | `<a>` | External link |
| `data-sf="email"` | `<a>` | Email link |
| `data-sf="tag"` | `<span>` | Tag chip |
| `data-sf="image"` | `<img>` | Image |
| `data-sf="code"` | `<code>` | Code block |

### Example CSS

```css
/* Style the entity list table */
[data-sf="entity-list"] { width: 100%; border-collapse: collapse; }
[data-sf="entity-list"] th { text-align: left; padding: 0.5rem; }
[data-sf="entity-list"] td { padding: 0.5rem; border-top: 1px solid #e5e7eb; }

/* Style badges by status */
[data-sf="badge"] { padding: 0.125rem 0.5rem; border-radius: 9999px; font-size: 0.75rem; }
[data-sf="badge"][data-status="success"] { background: #dcfce7; color: #166534; }
[data-sf="badge"][data-status="error"] { background: #fee2e2; color: #991b1b; }
[data-sf="badge"][data-status="warning"] { background: #fef3c7; color: #92400e; }
[data-sf="badge"][data-status="info"] { background: #dbeafe; color: #1e40af; }
[data-sf="badge"][data-status="neutral"] { background: #f3f4f6; color: #374151; }

/* Style form fields */
[data-sf="field"] { margin-bottom: 1rem; }
[data-sf="field"] label { display: block; font-weight: 500; margin-bottom: 0.25rem; }
[data-sf="field"] input,
[data-sf="field"] select,
[data-sf="field"] textarea { width: 100%; padding: 0.5rem; border: 1px solid #d1d5db; border-radius: 0.375rem; }

/* Style pagination */
[data-sf="pagination"] { display: flex; justify-content: space-between; padding: 0.75rem 0; }
```

---

## 11. Authentication

Widget routes support two authentication methods: **session-based** (for browser users) and **PASETO token-based** (for API consumers). Both share the same access control layer.

### Session-Based Auth (Browser)

Widget routes share the same session layer as the admin UI. When a user logs in at `/admin/login`, the `forge_session` cookie authenticates requests to both `/admin/*` and `/forge/*` routes.

A `session_to_claims` middleware automatically bridges the session into `Claims` request extensions, so the existing `OptionalClaims` extractor and `check_schema_access()` logic work transparently. No additional configuration is needed — if the admin UI is enabled with an `AuthStore`, widget routes are automatically session-aware.

**Flow:**
1. User logs in at `/admin/login` (username + password)
2. Server sets a `forge_session` cookie
3. Subsequent requests to `/forge/*` include the cookie
4. The `session_to_claims` middleware reads the session, constructs `Claims` (user ID, roles), and injects them into request extensions
5. Widget handlers extract `OptionalClaims` and enforce `@access` annotations as usual

### Token-Based Auth (API)

API consumers can authenticate by including a PASETO token in request headers. When a token is present, the upstream token middleware injects `Claims` into request extensions before the session middleware runs. The session middleware defers — it never overrides token-provided `Claims`.

```bash
# Generate a token
schemaforge token generate \
  --key ./keys/paseto.key \
  --sub "user:entity_alice123" \
  --roles "sales,marketing"

# Use it in requests
curl -H "Authorization: Bearer <token>" \
     http://localhost:3000/forge/Contact/entities
```

### Access Control

Schema-level access is controlled by `@access` annotations:
- No `@access` annotation + auth enabled = **deny** (secure-by-default)
- `@access(read: ["public"])` = allow all authenticated users
- `@access(read: ["admin", "manager"])` = allow only those roles

Field-level access is controlled by `@field_access` annotations. Restricted fields are silently omitted from responses.

Without an `AuthStore` configured, no session layer is applied and widget routes fall back to token-only auth.

---

## 12. Building Navigation

SchemaForge does not provide navigation UI. Use the schema list API to build your own:

```bash
# List all schemas (core API)
curl http://localhost:3000/api/v1/forge/schemas
```

Response:
```json
{
  "schemas": [
    { "name": "Contact", "version": 1, "field_count": 5 },
    { "name": "Company", "version": 1, "field_count": 3 }
  ],
  "count": 2
}
```

Schemas with `@system` annotation are excluded from this list. Build your navigation from this response:

```html
<nav id="schema-nav">
  <!-- Fetch schemas and build links -->
  <script>
    fetch('/api/v1/forge/schemas', { headers: { 'Accept': 'application/json' } })
      .then(r => r.json())
      .then(data => {
        const nav = document.getElementById('schema-nav');
        data.schemas.forEach(s => {
          const a = document.createElement('a');
          a.href = `/forge/${s.name}/entities`;
          a.textContent = s.name;
          nav.appendChild(a);
        });
      });
  </script>
</nav>
```

Or with HTMX, embed fragments directly:

```html
<div hx-get="/forge/Contact/entities" hx-trigger="load" hx-target="this">
  Loading contacts...
</div>
```

---

## 13. Template File Inventory

Templates are embedded in the binary at compile time. All templates use MiniJinja syntax and `data-sf` attributes for styling hooks.

### Entry Templates (`forge/`)

| File | Purpose |
|------|---------|
| `forge/entity_list.html` | Entity list wrapper (includes organism) |
| `forge/entity_detail.html` | Entity detail wrapper (includes organism) |
| `forge/entity_form.html` | Create/edit form with field inputs |

### Shared Organisms (`shared/organisms/`)

| File | Purpose |
|------|---------|
| `entity_list.html` | Table with headers, rows, and pagination |
| `entity_detail.html` | Detail card with field display and actions |
| `empty_state.html` | Empty state placeholder |

### Shared Molecules (`shared/molecules/`)

| File | Purpose |
|------|---------|
| `entity_row.html` | Table row with field values and actions |
| `pagination.html` | Previous/Next pagination controls |
| `field_input.html` | Input type dispatcher |

### Shared Atoms (`shared/atoms/`)

| File | Purpose |
|------|---------|
| `field_display.html` | Widget-aware field value renderer |
| `text_input.html` | Text input |
| `textarea.html` | Textarea |
| `number_input.html` | Number input |
| `checkbox.html` | Checkbox |
| `datetime_input.html` | Datetime input |
| `select.html` | Select (with HTMX relation loading) |
| `json_editor.html` | JSON textarea |
| `array_input.html` | Array textarea |
| `composite.html` | Composite fieldset |
| `fallback_input.html` | Fallback text input |

---

## 14. Error Responses

### HTML Errors

Widget endpoints return bare HTML error fragments:

```html
<div data-sf="error" data-status="404">Schema 'Foo' not found</div>
```

### JSON Errors

When `Accept: application/json`, validation errors return:
```json
{ "errors": ["Field 'email' is required"] }
```

Other errors return the appropriate HTTP status code with a plain text body.

### Error Status Codes

| Error | HTTP Status |
|---|---|
| Schema not found | 404 |
| Entity not found | 404 |
| Forbidden | 403 |
| Unauthorized | 401 |
| Validation failed | 422 |
| Backend unavailable | 502 |
| Internal error | 500 |
