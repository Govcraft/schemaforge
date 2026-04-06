# HTMX Site Guide

SchemaForge generates CRUD APIs from `.schema` files, but production applications
often need a web interface for browsing and managing data. Building one from
scratch means writing templates, wiring up forms, handling authentication, and
styling -- work that duplicates what SchemaForge already knows about your schemas.

The `--with-htmx` flag solves this. It scaffolds a complete, session-authenticated
web frontend at `/site/` powered by HTMX and MiniJinja templates. You get login,
entity listing with pagination, create/edit forms, detail views, and delete
confirmation -- all driven by your schema definitions. Every template and CSS
variable is yours to customize; SchemaForge never overwrites files you have edited.

This guide covers enabling the feature, understanding the scaffolded files,
customizing templates and styles, and extending the HTMX interaction patterns.

---

## Enabling the Site UI

There are two ways to get the HTMX site templates into your project.

**Option 1: Add to an existing project with `serve`**

Pass `--with-htmx` when starting the server. On first run, SchemaForge scaffolds
starter templates into `site/templates/` if that directory does not already exist:

```sh
schemaforge serve --with-htmx
```

The server prints `GET  /site/` in its route listing to confirm the site UI is
active. Open `http://localhost:3000/site/` in a browser to see the home page.

**Option 2: Scaffold with `init`**

The `full` template (the default) includes site templates from the start:

```sh
schemaforge init my-project
# or explicitly:
schemaforge init my-project --template full
```

You still need `--with-htmx` on `serve` to activate the routes.

**Authentication:** The site UI requires a login. Set admin credentials with
`--admin-user` and `--admin-password` (or the `FORGE_ADMIN_USER` /
`FORGE_ADMIN_PASSWORD` environment variables):

```sh
schemaforge serve --with-htmx --admin-password secret
```

---

## Template Structure

The `--with-htmx` flag scaffolds seven MiniJinja templates and serves one CSS
file. The templates follow a standard inheritance pattern: `base.html` defines
the layout, and each page template extends it.

```
site/
  templates/
    base.html            # Shared layout: nav, flash messages, footer
    index.html           # Home page -- schema card grid
    login.html           # Standalone login page (no base.html)
    login_card.html      # Login form fragment (swapped by HTMX on error)
    entities.html         # Entity list with pagination
    entity_detail.html   # Single entity view with edit/delete actions
    entity_form.html     # Create and edit form (shared template)
```

### Template Inheritance

`base.html` provides the document skeleton: the HTMX script tag, the CSS link,
the navigation bar, flash message rendering, and the footer. Page templates
extend it with two blocks:

```html
{% extends "site/base.html" %}

{% block title %}My Page - SchemaForge{% endblock %}

{% block content %}
  <!-- page content here -->
{% endblock %}
```

The `login.html` template is the exception -- it renders a standalone page
without the navigation bar, since unauthenticated users should see only the
login form.

### Page Templates

Each page template receives a context object from its handler. The key variables
available in every protected page are:

| Variable | Type | Description |
|----------|------|-------------|
| `current_user` | object or null | Authenticated user with `display_name` |
| `flash` | list | Flash messages with `message` and `css_class` |
| `schema` | object | Schema metadata with `name`, `url_name`, `fields` |
| `url_prefix` | string | Always `/site/schemas` -- use for building links |

`entity_form.html` serves double duty: when `entity_id` is present, it renders
an edit form with `hx-put`; when absent, it renders a create form with `hx-post`.

---

## Customizing the UI

SchemaForge never overwrites existing templates. Once the `site/templates/`
directory exists, it is entirely yours. Edit any file, add new ones, or replace
the design completely.

### Template Loading Order

When `--with-htmx` is active, the template engine resolves site templates in
this order:

1. **Filesystem** -- `site/templates/` in the project directory (user edits)
2. **Embedded defaults** -- compiled into the SchemaForge binary

This means scaffolded files take priority immediately. If you delete a template
file, the embedded default is used as a fallback. Changes to scaffolded files
take effect on server restart.

### CSS Variables

The site's visual identity is controlled through CSS custom properties in
`site.css`, served at `/site/static/site.css`. Override any of these to change
the look without touching templates:

```css
:root {
    --bg: #1a1a2e;           /* Page background */
    --bg-surface: #16213e;   /* Nav and card surfaces */
    --bg-card: #1e2a47;      /* Card backgrounds */
    --text: #e0e0e0;         /* Primary text */
    --text-muted: #8892a4;   /* Secondary text */
    --accent: #6366f1;       /* Links and primary buttons */
    --accent-hover: #818cf8; /* Hover state for accent */
    --border: #2a3a5c;       /* Borders and dividers */
    --error: #ef4444;        /* Error text and danger buttons */
    --radius: 8px;           /* Border radius for cards and inputs */
    --font: system-ui, -apple-system, "Segoe UI", Roboto, sans-serif;
}
```

For example, switching to a light theme requires changing only the color
variables -- every component references them.

### Modifying Templates

Templates use MiniJinja syntax (similar to Jinja2). A few patterns you will
encounter:

- **`{% extends %}`** and **`{% block %}`** for layout inheritance
- **`{% include %}`** for reusable partials (shared components)
- **`{{ variable }}`** for value interpolation
- **`{% for item in list %}`** for iteration
- **`{% if condition %}`** for conditionals

To add a sidebar to every page, edit `base.html`. To change how a single page
renders, edit that page's template. To change how entity fields display, edit the
shared atoms (see [Shared UI Components](#shared-ui-components) below).

---

## HTMX Patterns

The site UI uses HTMX to submit forms, delete records, and navigate after
mutations -- all without custom JavaScript. HTMX attributes on HTML elements
issue HTTP requests and swap content based on the response. Five patterns appear
throughout the templates.

### Form Submissions (hx-post, hx-put)

The `entity_form.html` template uses `hx-post` for creating entities and
`hx-put` for updating them. The form targets `body` and enables URL push so the
browser history updates:

```html
<form
    hx-post="/site/schemas/Contact/entities"
    hx-target="body"
    hx-push-url="true"
>
```

For edits, the same template switches to `hx-put`:

```html
<form
    hx-put="/site/schemas/Contact/entities/abc123"
    hx-target="body"
    hx-push-url="true"
>
```

On validation failure, the server returns the form HTML with a 422 status and
error messages. HTMX swaps the body content, showing errors inline without a
full page reload.

### Inline Delete (hx-delete)

Delete buttons appear in two contexts with different swap strategies.

**In the entity list** (table rows), the delete button removes just the row:

```html
<button
    hx-delete="/site/schemas/Contact/entities/abc123"
    hx-confirm="Delete this entity?"
    hx-target="closest tr"
    hx-swap="delete"
>Delete</button>
```

`hx-confirm` shows a browser confirmation dialog. On success, the server returns
an `HX-Redirect` header and HTMX navigates to the entity list with a flash
message confirming the deletion.

**On the detail page**, the delete button targets the content area:

```html
<button
    hx-delete="/site/schemas/Contact/entities/abc123"
    hx-confirm="Delete this entity?"
    hx-target="#entity-content"
    hx-swap="innerHTML"
>Delete</button>
```

### Server-Side Redirects (HxRedirect)

After successful create, update, or delete operations, handlers return an
`HX-Redirect` response header instead of HTML content. HTMX intercepts this
header and performs a full page navigation to the specified URL:

| Operation | Redirect Target |
|-----------|----------------|
| Create | Detail page for the new entity |
| Update | Detail page for the updated entity |
| Delete | Entity list for the schema |
| Login | Home page (`/site/`) |
| Logout | Login page (`/site/login`) |

This pattern ensures the browser URL, page title, and navigation state all
update correctly after mutations.

### Flash Messages

Flash messages provide one-time feedback after redirects. Handlers push messages
to the session before redirecting:

- "Contact created successfully" after a create
- "Contact updated successfully" after an update
- "Contact deleted" after a delete
- "Welcome back, Admin!" after login

The `base.html` template renders any pending flash messages at the top of the
content area:

```html
{% if flash %}
<div class="flash-container">
    {% for msg in flash %}
    <div class="flash {{ msg.css_class }}">{{ msg.message }}</div>
    {% endfor %}
</div>
{% endif %}
```

Four CSS classes control flash styling: `flash-success`, `flash-error`,
`flash-warning`, and `flash-info`.

Flash messages are distinct from validation errors. When a form submission fails
validation, the server returns the form HTML with a 422 status and the errors
rendered inline -- no flash message is pushed. Flash messages only appear after
successful redirects (create, update, delete, login).

### Login Flow

The login page demonstrates HTMX partial replacement. The login form posts to
`/site/login` and targets the `.login-card` container:

```html
<form hx-post="/site/login" hx-target="closest .login-card" hx-swap="outerHTML">
```

- **On failure:** The server returns only the `login_card.html` fragment with an
  error message. HTMX swaps just the card, keeping the page intact and showing
  the error inline.
- **On success:** The server returns an `HX-Redirect` to `/site/`. HTMX
  performs a full navigation, loading the authenticated home page with its
  navigation bar and flash welcome message.

This two-outcome pattern -- partial swap on error, full redirect on success --
avoids full page reloads for validation feedback while ensuring authentication
state changes propagate to the entire page.

---

## Shared UI Components

The site templates reuse SchemaForge's shared component library, organized
following atomic design principles. These components are also used by the admin
and widget UIs, so customizing them affects all three interfaces.

```
shared/
  atoms/          # Smallest display units
    field_display.html    # Renders a field value (badges, links, dates, etc.)
    text_input.html       # Text input with label
    checkbox.html         # Boolean checkbox
    select.html           # Dropdown select
    number_input.html     # Numeric input
    datetime_input.html   # Date/time picker
    textarea.html         # Multi-line text
    json_editor.html      # JSON field editor
    array_input.html      # Array field input
    composite.html        # Composite/nested field
    fallback_input.html   # Catch-all input type
  molecules/      # Combinations of atoms
    entity_row.html       # Single table row with field values and actions
    pagination.html       # Previous/Next navigation with offset counts
    empty_state.html      # "No entities yet" placeholder
  organisms/      # Full functional units
    entity_list.html      # Complete table: header, rows, pagination, empty state
```

The entity list page (`entities.html`) includes the organism directly:

```html
<div id="entity-table">
    {% include "shared/organisms/entity_list.html" %}
</div>
```

The entity form uses `admin/fragments/field_input.html` to dispatch each field
to the correct atom input (text, checkbox, select, etc.):

```html
{% for field in fields %}
{% include "admin/fragments/field_input.html" %}
{% endfor %}
```

The organism assembles molecules (rows, pagination) which assemble atoms (field
displays). To change how a boolean field renders across the entire application,
edit `shared/atoms/field_display.html`. To change the table layout, edit
`shared/organisms/entity_list.html`.

The `field_display.html` atom supports rich widget types based on schema field
metadata: `status_badge`, `progress`, `relative_time`, `link`, `email`, `phone`,
`color`, `tags`, `image`, `code`, and `markdown`. These render automatically
when your schema fields use the corresponding types.

---

## Route Reference

All site routes are mounted under `/site/`. Protected routes redirect to
`/site/login` for unauthenticated requests.

| Method | Path | Description |
|--------|------|-------------|
| GET | `/site/` | Home page with schema cards |
| GET | `/site/login` | Login page |
| POST | `/site/login` | Submit login credentials |
| POST | `/site/logout` | Destroy session and redirect |
| GET | `/site/static/site.css` | Site stylesheet |
| GET | `/site/schemas/{name}/entities` | Entity list with pagination |
| GET | `/site/schemas/{name}/entities/_table` | Pagination fragment (HTMX) |
| GET | `/site/schemas/{name}/entities/new` | Create entity form |
| POST | `/site/schemas/{name}/entities` | Submit new entity |
| GET | `/site/schemas/{name}/entities/{id}` | Entity detail page |
| GET | `/site/schemas/{name}/entities/{id}/edit` | Edit entity form |
| PUT | `/site/schemas/{name}/entities/{id}` | Submit entity update |
| DELETE | `/site/schemas/{name}/entities/{id}` | Delete entity |

Pagination uses `limit` and `offset` query parameters on the entity list routes.
The default page size is 25 entities.
