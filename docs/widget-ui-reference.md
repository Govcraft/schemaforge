# SchemaForge Widget UI Reference

This document describes every HTTP endpoint, HTMX interaction, template file, and data shape in the Widget UI system. Use it to build custom frontends that integrate with SchemaForge.

The Widget UI has two tiers:

- **Widget routes** (`/forge/...`) -- bare HTMX fragments with no layout, embeddable into any page
- **Site routes** (`/app/...`) -- full-page application with sidebar nav, session auth, and dashboard

Both are gated behind the `widget-ui` Cargo feature on `schema-forge-acton`.

---

## Table of Contents

1. [Widget Endpoints (Embeddable Fragments)](#1-widget-endpoints-embeddable-fragments)
2. [Site Endpoints (Full-Page Application)](#2-site-endpoints-full-page-application)
3. [Auth Endpoints](#3-auth-endpoints)
4. [HTMX Interaction Patterns](#4-htmx-interaction-patterns)
5. [Template File Inventory](#5-template-file-inventory)
6. [Data Shapes](#6-data-shapes)
7. [Field Input Types](#7-field-input-types)
8. [Widget Types (Display)](#8-widget-types-display)
9. [Badge Color Classification](#9-badge-color-classification)
10. [Annotations That Affect Rendering](#10-annotations-that-affect-rendering)

---

## 1. Widget Endpoints (Embeddable Fragments)

All widget routes live under `/forge` and return **bare HTML fragments** (no `<html>`, no layout). They are designed to be swapped into any HTMX-powered page.

Auth is applied externally via the `register_widget_routes()` method, which wraps all routes with API auth middleware.

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

**Response:** HTML fragment wrapping `shared/organisms/entity_list_table.html`

```html
<div class="forge-entity-table" id="forge-table-Contact">
  <table>
    <thead><tr><!-- field headers + Actions --></tr></thead>
    <tbody>
      <tr id="entity-{id}"><!-- field values + View/Edit/Delete --></tr>
      ...
    </tbody>
  </table>
  <!-- pagination: Previous / Next -->
</div>
```

**Template:** `forge/entity_list_table.html`

**Context:**
```
schema:     SchemaView
entities:   Vec<EntityView>
pagination: PaginationView
url_prefix: "/forge"
```

---

### GET `/forge/{schema}/entities/_table` -- Pagination Fragment

Identical to the entity list endpoint. Exists as a dedicated HTMX target for pagination links.

---

### GET `/forge/{schema}/entities/new` -- Create Form

Returns an empty entity form.

**Response:** HTML form fragment

```html
<div class="forge-entity-form" id="forge-form-Contact">
  <form hx-post="/forge/Contact/entities"
        hx-target="closest .forge-entity-form"
        hx-swap="outerHTML">
    <!-- field inputs -->
    <button type="submit">Create</button>
  </form>
</div>
```

**Template:** `forge/entity_form.html`

**Context:**
```
schema:     SchemaView
fields:     Vec<FieldView>
entity_id:  None
errors:     []
url_prefix: "/forge"
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

**On success (200):** Returns entity detail fragment, replacing the form.

```html
<div class="forge-detail" id="forge-detail-Contact-{id}">
  <!-- entity detail card with Edit/Delete buttons -->
</div>
```

**Template:** `organisms/entity_detail_full.html`

**On validation error (422):** Returns the form again with errors populated.

```html
<div class="forge-entity-form" id="forge-form-Contact">
  <div class="forge-errors">
    <ul><li>Field 'email' is required</li></ul>
  </div>
  <form ...><!-- fields with current_value preserved --></form>
</div>
```

---

### GET `/forge/{schema}/entities/{id}` -- Entity Detail

Returns an entity detail card.

**Response:**
```html
<div class="forge-detail" id="forge-detail-Contact-{id}">
  <div class="forge-detail-header">
    <h3>{display_value}</h3>
    <span>ID: {id}</span>
  </div>
  <div class="forge-detail-body">
    <!-- field label/value pairs -->
  </div>
  <div class="forge-detail-footer">
    <a hx-get="/forge/Contact/entities/{id}/edit"
       hx-target="closest [id^='forge-detail-']"
       hx-swap="outerHTML">Edit</a>
    <button hx-delete="/forge/Contact/entities/{id}"
            hx-confirm="Delete this entity?"
            hx-target="closest [id^='forge-detail-']"
            hx-swap="outerHTML">Delete</button>
  </div>
</div>
```

**Template:** `organisms/entity_detail_full.html`

**Context:**
```
schema:     SchemaView
entity:     EntityView  (with resolved relation display values)
url_prefix: "/forge"
```

---

### GET `/forge/{schema}/entities/{id}/edit` -- Edit Form

Returns a pre-populated edit form.

**Response:** Same structure as create form, but with:
- `hx-put` instead of `hx-post`
- Fields pre-filled with `current_value`
- Submit button says "Update"

```html
<div class="forge-entity-form" id="forge-form-Contact">
  <form hx-put="/forge/Contact/entities/{id}"
        hx-target="closest .forge-entity-form"
        hx-swap="outerHTML">
    <!-- fields with current_value set -->
    <button type="submit">Update</button>
  </form>
</div>
```

**Template:** `forge/entity_form.html`

**Context:**
```
schema:     SchemaView
fields:     Vec<FieldView>  (with current_value populated)
entity_id:  Some("{id}")
errors:     []
url_prefix: "/forge"
```

---

### PUT `/forge/{schema}/entities/{id}` -- Update Entity

Same request/response pattern as create:
- **On success (200):** Returns updated entity detail fragment
- **On validation error (422):** Returns form with errors

---

### DELETE `/forge/{schema}/entities/{id}` -- Delete Entity

**Response:** `200 OK` with empty body.

The HTMX `hx-swap="delete"` on the calling button removes the target element from the DOM.

---

### GET `/forge/{schema}/relation-options/{field}` -- Relation Options

Returns `<option>` elements for populating a relation select field.

**Path params:**
| Param | Type | Description |
|-------|------|-------------|
| `schema` | String | Target schema name (the relation target, not the source) |
| `field` | String | Field name (routing context only) |

**Response:** Raw HTML (no wrapper div)
```html
<option value="">-- Select --</option>
<option value="company:abc123">Acme Corp</option>
<option value="company:def456">Widgets Inc</option>
```

Fetches up to 100 entities. Uses `@display` annotation field for labels, falls back to first Text field, then entity ID.

---

## 2. Site Endpoints (Full-Page Application)

Site routes live under `/app/` and return complete HTML pages extending `cloud/base.html` with sidebar navigation. Protected routes require session authentication (cookie: `forge_site`).

### GET `/app/` -- Dashboard

Full-page dashboard with aggregate statistics for all accessible schemas.

**Template:** `cloud/dashboard.html`

**Context:**
```
nav_schemas:     Vec<NavSchemaEntry>   -- sidebar entries
active_nav:      "dashboard"
schema_cards:    Vec<DashboardCard>    -- aggregate widget cards
current_user:    Option<SiteUserView>
heading_actions: []
breadcrumbs:     []
stats:           Vec<StatItem>         -- derived from schema_cards
```

**Aggregate widgets** are controlled by the `@dashboard(widgets: [...])` annotation:
- `"count"` -- total entity count
- `"sum:field_name"` -- sum of a numeric field
- `"avg:field_name"` -- average of a numeric field

Default: `["count"]` if no annotation.

Values are formatted using `@format` annotations (e.g., `@format("currency:$")` renders `$1,234.00`).

---

### GET `/app/{schema}/entities` -- Entity List

Returns either a **table view** or a **kanban board** depending on schema annotations.

#### Table View (default)

**Template:** `cloud/entity_list.html`

**Context:**
```
nav_schemas:     Vec<NavSchemaEntry>
active_nav:      "{schema}"
schema:          SchemaView
entities:        Vec<EntityView>
pagination:      PaginationView
list_style:      "table"
filter_fields:   Vec<FilterField>     -- enum fields become filter pills
current_user:    Option<SiteUserView>
heading_actions: [HeadingAction("Create New", "/app/{schema}/entities/new")]
breadcrumbs:     [("Dashboard", "/app/"), ("{schema}", None)]
```

**Query params:**
| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `limit` | usize | 25 | Page size |
| `offset` | usize | 0 | Starting offset |
| `filter_{field}` | String | -- | Filter by enum field value |

**HTMX interactions in the response:**
- Filter pills: `hx-get="/app/{schema}/entities/_table?filter_{field}={value}"`, `hx-target="#sf-entity-table"`, `hx-swap="innerHTML"`, `hx-push-url="true"`
- Pagination: `hx-get="/app/{schema}/entities/_table?limit=...&offset=..."`, `hx-target="#sf-entity-table"`, `hx-swap="innerHTML"`
- Delete: `hx-delete="/app/{schema}/entities/{id}"`, `hx-confirm="..."`, `hx-target="#sf-entity-{id}"`, `hx-swap="delete"`

#### Kanban View

Activated when the schema has `@dashboard(layout: "kanban")` AND a kanban-eligible field (an enum field with `@kanban_column` annotation or referenced by `@dashboard(group_by: "field")`).

**Template:** `cloud/entity_list_kanban.html`

**Context:**
```
nav_schemas:     Vec<NavSchemaEntry>
active_nav:      "{schema}"
schema:          SchemaView
columns:         Vec<KanbanColumn>
kanban_field:    String                -- field name used for grouping
current_user:    Option<SiteUserView>
heading_actions: [HeadingAction("Create New", ...)]
breadcrumbs:     [("Dashboard", "/app/"), ("{schema}", None)]
```

**Drag-and-drop:** Native HTML5 drag events with JavaScript that calls:
```javascript
htmx.ajax('PATCH', '/app/{schema}/entities/{id}/move',
  { values: { field: '{kanban_field}', value: '{target_variant}' }, swap: 'none' });
```
Optimistic DOM update with rollback on error.

---

### GET `/app/{schema}/entities/_table` -- Table Fragment

HTMX pagination/filter target. Returns just the table body, not the full page.

**Template:** `cloud/fragments/entity_list_body.html`

**Context:**
```
schema:        SchemaView
entities:      Vec<EntityView>
pagination:    PaginationView
list_style:    "table"
filter_fields: Vec<FilterField>
```

---

### GET `/app/{schema}/entities/new` -- Create Form

Full-page create form.

**Template:** `cloud/entity_form.html`

**Context:**
```
nav_schemas, active_nav, current_user  -- standard nav
schema:          SchemaView
fields:          Vec<FieldView>
entity_id:       None
errors:          []
heading_actions: []
breadcrumbs:     [("Dashboard", "/app/"), ("{schema}", "/app/{schema}/entities"), ("New", None)]
```

**Form HTMX:**
```html
<form method="post" action="/app/{schema}/entities"
      hx-post="/app/{schema}/entities"
      hx-target="closest .sf-detail"
      hx-swap="outerHTML">
```

---

### POST `/app/{schema}/entities` -- Create Entity

- **On success:** `302` redirect to `/app/{schema}/entities/{id}`
- **On validation error (422):** Re-renders `cloud/entity_form.html` with errors

---

### GET `/app/{schema}/entities/{id}` -- Entity Detail

Full-page entity detail.

**Template:** `cloud/entity_detail.html`

**Context:**
```
nav_schemas, active_nav, current_user  -- standard nav
schema:          SchemaView
entity:          EntityView
detail_style:    "full"
heading_actions: [HeadingAction("Edit", "/app/{schema}/entities/{id}/edit")]
breadcrumbs:     [("Dashboard", "/app/"), ("{schema}", "/app/{schema}/entities"), ("{display_value}", None)]
```

---

### GET `/app/{schema}/entities/{id}/edit` -- Edit Form

Full-page edit form with pre-populated fields.

**Template:** `cloud/entity_form.html`

**Context:**
```
schema:          SchemaView
fields:          Vec<FieldView>  (with current_value populated)
entity_id:       Some("{id}")
errors:          []
breadcrumbs:     [..., ("Edit {id}", None)]
```

**Form HTMX:**
```html
<form method="post" action="/app/{schema}/entities/{id}"
      hx-put="/app/{schema}/entities/{id}"
      hx-target="closest .sf-detail"
      hx-swap="outerHTML">
```

---

### PUT `/app/{schema}/entities/{id}/edit` -- Update Entity

- **On success:** `302` redirect to `/app/{schema}/entities/{id}`
- **On validation error (422):** Re-renders form with errors

---

### DELETE `/app/{schema}/entities/{id}` -- Delete Entity

Returns `200 OK` with empty body.

---

### PATCH `/app/{schema}/entities/{id}/move` -- Kanban Move

Moves a kanban card to a different column.

**Request body:** `application/x-www-form-urlencoded`
```
field=status&value=in_progress
```

| Param | Description |
|-------|-------------|
| `field` | The enum field name used for kanban grouping |
| `value` | The new enum variant value |

**Response:** `200 OK` with empty body.

---

### GET `/app/{schema}/relation-options/{field}` -- Relation Options

Same as the widget version. Returns `<option>` HTML elements.

---

## 3. Auth Endpoints

### GET `/app/login` -- Login Page

Standalone HTML page (no base.html, no sidebar). If already authenticated, redirects to `/app/`.

**Template:** `cloud/login.html`

**Context:**
```
error: Option<String>   -- error message from failed login attempt
```

**HTML structure:**
```html
<!DOCTYPE html>
<html>
<body>
  <!-- error alert (if error is set) -->
  <form method="post" action="/app/login">
    <input name="username" type="text" required>
    <input name="password" type="password" required>
    <button type="submit">Sign in</button>
  </form>
  <!-- demo accounts table (usernames/passwords/roles) -->
</body>
</html>
```

No HTMX on this page -- standard form POST.

---

### POST `/app/login` -- Login Submit

**Request body:** `application/x-www-form-urlencoded`
```
username=admin&password=admin
```

| Field | Type | Required |
|-------|------|----------|
| `username` | String | Yes |
| `password` | String | Yes |

Validates credentials against the `_forge_users` SurrealDB table using `crypto::argon2::compare()`.

- **On success:** Sets session cookie (`forge_site`), redirects to `/app/`
- **On failure:** Re-renders login page with `error: "Invalid username or password"`

---

### POST `/app/logout` -- Logout

Destroys the session and redirects to `/app/login`.

No request body required.

---

### Auth Middleware Behavior

The `require_site_auth` middleware protects all `/app/` routes except login/logout.

When a request is not authenticated:
- **HTMX requests** (detected via `HX-Request` header): Returns `HX-Redirect: /app/login` header with empty body. HTMX will perform a client-side redirect.
- **Normal requests:** Returns `302` redirect to `/app/login`.

---

## 4. HTMX Interaction Patterns

### Pattern 1: Widget Fragment Lifecycle

```
1. Load list:     GET /forge/{schema}/entities
                  -> <div id="forge-table-{schema}"> table fragment </div>

2. Paginate:      hx-get="/forge/{schema}/entities/_table?limit=25&offset=25"
                  hx-target="#entity-table"
                  hx-swap="innerHTML"

3. Create:        GET /forge/{schema}/entities/new
                  -> <div class="forge-entity-form"> form </div>

                  POST /forge/{schema}/entities  (form submit)
                  hx-target="closest .forge-entity-form"
                  hx-swap="outerHTML"
                  -> success: detail card replaces form
                  -> error:   form re-renders with errors (422)

4. View detail:   GET /forge/{schema}/entities/{id}
                  -> <div class="forge-detail" id="forge-detail-{schema}-{id}"> ... </div>

5. Edit:          hx-get="/forge/{schema}/entities/{id}/edit"
                  hx-target="closest [id^='forge-detail-']"
                  hx-swap="outerHTML"
                  -> detail card replaced by edit form

                  PUT /forge/{schema}/entities/{id}  (form submit)
                  hx-target="closest .forge-entity-form"
                  hx-swap="outerHTML"
                  -> success: detail card replaces form
                  -> error:   form re-renders with errors (422)

6. Delete:        hx-delete="/forge/{schema}/entities/{id}"
                  hx-confirm="Delete this entity?"
                  hx-target="#entity-{id}"  (or closest [id^='forge-detail-'])
                  hx-swap="delete"
                  -> 200 OK, element removed from DOM

7. Relation opts: hx-get="/forge/{target}/relation-options/{field}"
                  hx-trigger="load"
                  hx-target="this"  (the <select> element)
                  hx-swap="innerHTML"
                  -> <option> elements loaded on page render
```

### Pattern 2: Site Page Lifecycle

```
1. Dashboard:     GET /app/
                  -> full page with aggregate cards + stats

2. List (table):  GET /app/{schema}/entities
                  -> full page with table inside <div id="sf-entity-table">

3. Filter:        hx-get="/app/{schema}/entities/_table?filter_status=active"
                  hx-target="#sf-entity-table"
                  hx-swap="innerHTML"
                  hx-push-url="true"
                  -> table body fragment replaces contents, URL updates

4. Paginate:      hx-get="/app/{schema}/entities/_table?limit=25&offset=25"
                  hx-target="#sf-entity-table"
                  hx-swap="innerHTML"
                  -> table body fragment

5. Create:        GET /app/{schema}/entities/new  -> full page form
                  POST /app/{schema}/entities     -> 302 to detail page

6. Detail:        GET /app/{schema}/entities/{id} -> full page detail

7. Edit:          GET /app/{schema}/entities/{id}/edit   -> full page form
                  PUT /app/{schema}/entities/{id}/edit   -> 302 to detail page

8. Delete:        hx-delete="/app/{schema}/entities/{id}"
                  hx-confirm="..."
                  hx-target="#sf-entity-{id}"
                  hx-swap="delete"
                  -> 200 OK, row removed

9. Kanban move:   htmx.ajax('PATCH', '/app/{schema}/entities/{id}/move',
                    { values: { field: 'status', value: 'done' }, swap: 'none' })
                  -> 200 OK (DOM already updated optimistically)
```

### Pattern 3: Relation Select Loading

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

After load, the select is populated with:
```html
<option value="">-- Select --</option>
<option value="Company:abc">Acme Corp</option>
```

---

## 5. Template File Inventory

All templates live under `crates/schema-forge-acton/templates/`.

### Widget Fragment Templates (`forge/`)

| File | Purpose | Wraps |
|------|---------|-------|
| `forge/entity_list_table.html` | Table list (default) | `shared/organisms/entity_list_table.html` |
| `forge/entity_list_cards.html` | Card grid list | `shared/organisms/entity_list_cards.html` |
| `forge/entity_list_compact.html` | Compact row list | `shared/organisms/entity_list_compact.html` |
| `forge/entity_detail.html` | Entity detail | `shared/organisms/entity_detail_full.html` |
| `forge/entity_form.html` | Create/edit form | Self-contained with field inputs |
| `forge/entity_table.html` | Legacy table (compat) | `admin/fragments/entity_table_body.html` |

### Site Page Templates (`cloud/`)

| File | Purpose | Extends |
|------|---------|---------|
| `cloud/base.html` | Base layout (sidebar + content) | -- |
| `cloud/login.html` | Login page (standalone) | -- |
| `cloud/dashboard.html` | Dashboard | `cloud/base.html` |
| `cloud/entity_list.html` | Entity list (table) | `cloud/base.html` |
| `cloud/entity_list_kanban.html` | Entity list (kanban) | `cloud/base.html` |
| `cloud/entity_detail.html` | Entity detail | `cloud/base.html` |
| `cloud/entity_form.html` | Create/edit form | `cloud/base.html` |
| `cloud/fragments/entity_list_body.html` | Table body (HTMX fragment) | -- |

### Shell Layout (`cloud/shells/`)

| File | Purpose |
|------|---------|
| `cloud/shells/sidebar.html` | Sidebar layout with mobile dialog |

### Cloud Atoms (`cloud/atoms/`)

| File | Purpose |
|------|---------|
| `cloud/atoms/field_display.html` | Widget-aware field value renderer |
| `cloud/atoms/field_input.html` | Form input dispatcher (all field types) |
| `cloud/atoms/composite.html` | Composite field fieldset |
| `cloud/atoms/sidebar_macros.html` | Jinja macros for sidebar nav items |

### Cloud Molecules (`cloud/molecules/`)

| File | Purpose |
|------|---------|
| `cloud/molecules/shell_sidebar_nav.html` | Sidebar navigation panel |
| `cloud/molecules/shell_sidebar_icon_nav.html` | Compact icon-only sidebar |
| `cloud/molecules/shell_sidebar_mobile_bar.html` | Mobile top bar |
| `cloud/molecules/shell_header_user_controls.html` | User dropdown menu |
| `cloud/molecules/shell_logo.html` | Logo/title display |
| `cloud/molecules/shell_mobile_disclosure.html` | Mobile nav disclosure |
| `cloud/molecules/shell_stacked_nav_inner.html` | Horizontal nav (stacked) |
| `cloud/molecules/shell_stacked_tab_nav_inner.html` | Tab-style horizontal nav |
| `cloud/molecules/shell_stacked_page_header.html` | Page header (stacked) |
| `cloud/molecules/shell_multicolumn_header.html` | Multi-column header |
| `cloud/molecules/dashboard_card.html` | Dashboard aggregate card |
| `cloud/molecules/stat_item_simple.html` | Simple stat display |
| `cloud/molecules/stat_item_card.html` | Card stat display |
| `cloud/molecules/stat_item_icon.html` | Stat with icon + trend |
| `cloud/molecules/stat_item_comparison.html` | Stat with comparison |
| `cloud/molecules/stat_item_trending.html` | Stat with trend badge |

### Shared Components (`shared/`)

#### Atoms (`shared/atoms/`)

| File | Purpose |
|------|---------|
| `field_display.html` | Widget-aware field display |
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
| `avatar.html` | Avatar image/initials |
| `heading_title.html` | Heading title + subtitle |
| `meta_item.html` | Meta info item |
| `stat_cell.html` | Stat value cell |
| `button.html` | Action button |
| `action_divider.html` | Button group divider |
| `initial_badge.html` | Letter initial badge |
| `stat_trend_badge.html` | Trend indicator badge |
| `stat_trend_inline.html` | Inline trend text |
| `stat_trend_text.html` | Trend text block |
| `stat_icon_box.html` | Stat icon container |
| `grid_action_arrow.html` | Grid card arrow SVG |

#### Molecules (`shared/molecules/`)

| File | Purpose |
|------|---------|
| `entity_row.html` | Table row (with HTMX delete) |
| `pagination.html` | Pagination controls (with HTMX) |
| `page_header.html` | Page header with actions |
| `empty_state.html` | Empty state placeholder |
| `alert.html` | Alert/notification |
| `breadcrumbs.html` | Breadcrumb trail |
| `heading_breadcrumbs.html` | Heading with breadcrumbs |
| `button_group.html` | Button group |
| `meta_row.html` | Metadata row |
| `filter_tabs.html` | Filter tab pills |
| `stat_grid.html` | Stat grid layout |
| `mobile_dropdown.html` | Mobile action dropdown |
| `dashboard_card.html` | Dashboard card |
| `grid_item_badge.html` | Grid: badge variant |
| `grid_item_profile.html` | Grid: profile variant |
| `grid_item_directory.html` | Grid: directory variant |
| `grid_item_link.html` | Grid: link variant |
| `grid_item_gallery.html` | Grid: gallery variant |
| `grid_item_detail.html` | Grid: detail variant |
| `grid_item_action.html` | Grid: action variant |
| `grid_card_actions.html` | Grid card footer actions |
| `grid_card_inline_actions.html` | Compact inline actions |
| `grid_entity_dl.html` | Entity definition list |

#### Organisms (`shared/organisms/`)

| File | Purpose |
|------|---------|
| `entity_list_table.html` | Table list with pagination |
| `entity_list_cards.html` | Card grid with pagination |
| `entity_list_compact.html` | Compact list with pagination |
| `entity_list_grid_badge.html` | Badge grid layout |
| `entity_list_grid_profile.html` | Profile grid layout |
| `entity_list_grid_directory.html` | Directory grid layout |
| `entity_list_grid_link.html` | Link grid layout |
| `entity_list_grid_gallery.html` | Gallery grid layout |
| `entity_list_grid_detail.html` | Detail grid layout |
| `entity_list_grid_actions.html` | Actions grid layout |
| `entity_detail_full.html` | Full detail card |
| `entity_detail_split.html` | Split two-column detail |
| `entity_detail_tabbed.html` | Tabbed detail (CSS tabs) |
| `heading_with_actions.html` | Heading variant |
| `heading_with_actions_and_breadcrumbs.html` | Heading variant |
| `heading_card_with_avatar_and_stats.html` | Heading variant |
| `heading_with_avatar_and_actions.html` | Heading variant |
| `heading_with_banner_image.html` | Heading variant |
| `heading_with_filters_and_actions.html` | Heading variant |
| `heading_with_logo_meta_and_actions.html` | Heading variant |
| `heading_with_meta_actions_and_breadcrumbs.html` | Heading variant |
| `heading_with_meta_and_actions.html` | Heading variant |
| `stats_section.html` | Stats dispatcher |
| `stats_cards.html` | Stats variant |
| `stats_with_icons.html` | Stats variant |
| `stats_shared_borders.html` | Stats variant |
| `stats_trending.html` | Stats variant |
| `stats_simple.html` | Stats variant |
| Container variants (5) | Layout containers |
| Card variants (10) | Structural card templates |

---

## 6. Data Shapes

These are the Rust structs serialized as template context. Field names become template variables.

### SchemaView

```
{
  name:          String         // "Contact"
  display_field: Option<String> // "full_name" (from @display annotation)
  version:       Option<u32>    // 1
  fields:        [FieldView]    // all schema fields
  url_name:      String         // "Contact" (used in URL paths)
}
```

### EntityView

```
{
  id:            String              // "Contact:abc123"
  display_value: String              // "John Doe" (from @display field)
  field_values:  [FieldDisplayView]  // all field values
  summary:       [FieldDisplayView]  // max 3 important fields (widget-annotated first)
}
```

### FieldView (form inputs)

```
{
  name:            String              // "email"
  label:           String              // "Email"
  input_type:      String              // "text", "select", "number", etc.
  attrs:           [(String, String)]  // [("maxlength", "255")]
  required:        bool
  options:         [(String, String)]  // [("active", "Active"), ("inactive", "Inactive")]
  multiple:        bool                // true for Relation with Many cardinality
  children:        [FieldView]         // for Composite/Array types
  type_label:      String              // "Text(max: 255)"
  default_value:   Option<String>
  current_value:   Option<String>      // populated for edit forms
  relation_target: Option<String>      // "Company" for Relation fields
}
```

### FieldDisplayView (read-only display)

```
{
  name:        String         // "status"
  label:       String         // "Status"
  value:       String         // "Active" (formatted, with @format applied)
  raw_value:   String         // "active" (unformatted original)
  widget_type: Option<String> // "status_badge"
  field_type:  String         // "enum"
  badge_class: Option<String> // "sf-badge-success"
  is_empty:    bool
}
```

### PaginationView

```
{
  current_page:    usize  // 1-based
  total_pages:     usize
  total_count:     usize
  limit:           usize  // page size (default 25)
  offset:          usize
  has_previous:    bool
  has_next:        bool
  end_showing:     usize  // min(offset + limit, total_count)
  previous_offset: usize
  next_offset:     usize
}
```

### KanbanColumn

```
{
  variant:   String        // "todo"
  label:     String        // "Todo"
  entities:  [EntityView]
  count:     usize
}
```

### FilterField

```
{
  name:         String        // "status"
  label:        String        // "Status"
  pills:        [FilterPill]  // one per enum variant
  all_active:   bool          // true when no filter selected
  active_value: String        // currently selected variant (empty = none)
}
```

### FilterPill

```
{
  value:     String  // "active"
  is_active: bool
}
```

### NavSchemaEntry

```
{
  url_name:     String         // "Contact"
  label:        String         // "Contact"
  entity_count: Option<String> // "42"
}
```

### SiteUserView

```
{
  username:     String         // "admin"
  display_name: String         // "Admin User"
  roles:        [String]       // ["admin"]
  is_admin:     bool
  avatar_url:   Option<String> // always None currently
}
```

### HeadingAction

```
{
  url:   String  // "/app/Contact/entities/new"
  label: String  // "Create New"
  class: String  // "sf-btn-primary"
}
```

### BreadcrumbItem

```
{
  label: String         // "Dashboard"
  url:   Option<String> // "/app/" (None for current page)
}
```

### StatItem

```
{
  label:           String         // "Contact Count"
  value:           String         // "42"
  unit:            Option<String> // "$"
  trend_value:     Option<String> // "+12%"
  trend_direction: Option<String> // "up" or "down"
  previous_value:  Option<String>
  icon_svg:        Option<String> // raw SVG string
  link_url:        Option<String> // "/app/Contact/entities"
  link_label:      Option<String> // "View all Contact"
}
```

### DashboardCard

```
{
  url_name:      String  // "Contact"
  label:         String  // "Contact"
  widget_label:  String  // "Count", "Total Revenue"
  display_value: String  // "42", "$1,234.00"
}
```

---

## 7. Field Input Types

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
| `json` | Json | `<textarea>` | `rows="6"`, monospace font, `placeholder="Enter JSON..."` |
| `array` | Array | `<textarea>` | `placeholder="One value per line"` |
| `composite` | Composite | `<fieldset>` | Nested child inputs with dot-notation names |

---

## 8. Widget Types (Display)

The `@widget` annotation on a field controls how its value is rendered in read-only views.

| `widget_type` | Rendering | CSS class |
|---|---|---|
| `status_badge` | `<span class="sf-badge {badge_class}">` | Auto-classified (see section 9) |
| `progress` | Progress bar div with percentage width | `sf-progress-bar` |
| `relative_time` | `<time>` with human-relative text ("5 minutes ago") | -- |
| `count_badge` | `<span class="sf-count-badge">` | -- |
| `link` | `<a href="..." target="_blank">` | -- |
| `email` | `<a href="mailto:...">` | -- |
| `phone` | `<a href="tel:...">` | -- |
| `color` | Color swatch span + text | `sf-color-swatch` |
| `tags` | Split by `", "` into `<span class="sf-tag">` | -- |
| `image` | `<img>` with lazy loading | -- |
| `code` | `<code>` block | -- |
| `markdown` | `<div class="sf-markdown">` | -- |
| (none) | Plain `{{ field.value }}` text | -- |

---

## 9. Badge Color Classification

When `widget_type` is `status_badge`, the `badge_class` is auto-assigned based on the field value:

| CSS Class | Matched values |
|---|---|
| `sf-badge-success` | active, done, completed, closed_won, approved, published, resolved, won, hired, accepted |
| `sf-badge-error` | inactive, terminated, cancelled, closed_lost, rejected, lost, fired, declined, failed |
| `sf-badge-warning` | pending, on_hold, in_review, draft, on_leave, paused, waiting, suspended |
| `sf-badge-info` | in_progress, proposal, negotiation, qualification, todo, prospecting, open, new, interview, review |
| `sf-badge-neutral` | backlog, archived, other, closed, unknown |

Unrecognized values get a deterministic hash to one of the 5 classes.

---

## 10. Annotations That Affect Rendering

| Annotation | Effect |
|---|---|
| `@display` | Sets which field value is used as the entity label |
| `@widget("type")` | Controls field display rendering (see section 8) |
| `@format("hint")` | Formats numeric values: `"currency:$"`, `"currency:EUR"`, `"percent"` |
| `@dashboard(widgets: [...])` | Controls dashboard aggregate widgets: `"count"`, `"sum:field"`, `"avg:field"` |
| `@dashboard(layout: "kanban")` | Enables kanban board view for entity list |
| `@dashboard(group_by: "field")` | Specifies the enum field for kanban column grouping |
| `@kanban_column` | Marks a field as the kanban grouping field (takes precedence over `group_by`) |
| `@system` | Excludes schema from sidebar navigation |
| `@access(read: [...])` | Controls which roles can see the schema in nav and access its data |
| `@field_access(read: [...])` | Controls which roles can see specific field values |

---

## Error Responses

### Widget Errors

Widget endpoints return bare HTML error fragments:

```html
<div class="forge-error" data-status="404">Schema 'Foo' not found</div>
```

| Error | HTTP Status |
|---|---|
| Schema not found | 404 |
| Entity not found | 404 |
| Forbidden | 403 |
| Unauthorized | 401 |
| Validation failed | 422 |
| Backend unavailable | 502 |
| Internal error | 500 |

### Site Errors

| Error | Response |
|---|---|
| Not found | 404 plain text |
| Forbidden | 403 full HTML page with "Back to Dashboard" link |
| Internal | 500 plain text |
