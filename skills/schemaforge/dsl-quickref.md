# SchemaForge — DSL Quick Reference

At-a-glance tables. For full grammar see [dsl-reference.md](dsl-reference.md). For lifecycle hooks deep-dive see [hooks-reference.md](hooks-reference.md).

## Field Types

| Type | Syntax | Constraints |
|------|--------|-------------|
| Text | `text` or `text(max: N)` | max character length |
| Rich Text | `richtext` | formatted/HTML content |
| Integer | `integer` or `integer(min: M, max: N)` | min/max bounds |
| Float | `float` or `float(precision: N)` | decimal places |
| Boolean | `boolean` | none |
| DateTime | `datetime` | ISO 8601 timestamps |
| Duration | `duration` | time span; signed nanoseconds (≈±292y); negative rejected on SurrealDB |
| Bytes | `bytes` or `bytes(max: N)` | raw binary; `max` = byte length; `base64.encode/decode` in CEL |
| Enum | `enum("a", "b", "c")` | 1+ variants, no duplicates |
| JSON | `json` | flexible unstructured data |
| Map | `map<text, V>` | open-keyed, homogeneous values; key must be `text` |
| File | `file(bucket: "docs", max_size: "25MB", mime: [...], access: "presigned")` | S3-backed attachment; see [storage-reference.md](storage-reference.md) |
| Relation (one) | `-> TargetSchema` | target must be PascalCase |
| Relation (many) | `-> TargetSchema[]` | Derived inverse view if target has `-> Self` FK back (read-only); else stored array of refs. |
| Array | `text[]`, `integer[]`, etc. | `[]` suffix on primitives |
| Composite | `composite { field: type }` | nested field definitions |

## Modifiers

| Modifier | Syntax | Effect |
|----------|--------|--------|
| Required | `required` | field must have a non-null value |
| Indexed | `indexed` | indexed for fast lookups |
| Unique | `unique` | values must be unique (per-tenant for `@tenant(...)` schemas, table-wide otherwise) |
| Default | `default(value)` | value when field omitted |

**Default value syntax:** `default("text")`, `default(42)`, `default(3.14)`, `default(true)`

**`unique` rules:**
- Allowed on `text`, `integer`, `float`, `datetime`, `enum`. Other types are a parse error (`UniqueOnUnsupportedType`) — `richtext`, `json`, `boolean`, arrays, `composite`, `relation`, `file`.
- For schemas with `@tenant(root)` or `@tenant(parent: "...")` the underlying constraint is composite on `(_tenant, field)`; two tenants can hold the same value.
- Adding `unique` to a column with existing duplicates fails at apply time. The migration step `AddUnique` is classified `RequiresConfirmation`; clean data first or pass `--force`.
- A write that collides returns **HTTP 409** with body `{ "error": "unique_violation", "schema": "...", "field": "...", "message": "..." }`. Generated edit forms route this onto the offending field via `react-hook-form`'s `setError`.

## Schema-Level Annotations (before `schema` keyword)

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
| Export | `@export(formats: [csv\|ndjson\|xlsx\|zip], bundle_files: bool, max_rows: N)` | enable bulk export (fail-closed; distinct Cedar `Export{Entity}` action, not `Read`). See export.md |

## Field-Level Annotations (after modifiers on a field line)

| Annotation | Syntax | Purpose |
|------------|--------|---------|
| Owner | `@owner` | record ownership tracking |
| Widget | `@widget("type")` | UI widget hint (closed 17-token vocabulary) |
| Kanban Column | `@kanban_column` | kanban grouping column |
| Format | `@format("type")` | display format (closed 7-token vocabulary) |
| Field Access | `@field_access(read: [...], write: [...])` | field-level access control |
| Exportable | `@exportable` / `@exportable(flatten: json)` | opt the field into bulk export files (fail-closed; never wider than read — exported set is `@exportable` ∩ readable). See export.md |
| List Hint | `@list(primary\|column\|hidden)` | list-view column curation |
| Enum Colors | `@enum_colors(variant: "color", ...)` | semantic color tokens per enum variant |
| Require | `@require("cel_expr", "message")` | write-time validation (CEL); rejects with `message` (422) unless `true`. Fail-closed. |
| Compute | `@compute("cel_expr")` | server-derived value (CEL); computed at write time and stored |
| Default | `@default("cel_expr")` | computed insert-time default (CEL); distinct from the `default(value)` literal modifier |
| Hidden | `@hidden` | language-level secret guard — field is invisible to every API surface (REST, GraphQL, list, query, get) and rejected in any client-supplied request body; Cedar policy generation skips it so it never surfaces as a resource attribute. Backend code that legitimately needs the value (e.g. `EntityAuthStore` reading `password_hash`) reads the entity directly, bypassing the API layer. |

**`@list(hint)` resolution ladder:** explicit hint wins → the `@display("...")` field auto-promotes to `primary` when no explicit primary is declared → `rich_text`, `composite`, `array`, `relation_one`, `relation_many`, and `json` fields default to `hidden` → everything else defaults to `column`. At most one `@list(primary)` per schema (parse error otherwise). `@list(column)` on a relation field opts it back in to list display and the generator renders the resolved `<field>__display` label as a linked cell.

**`@enum_colors(...)` color vocabulary:** `neutral`, `gray`, `red`, `amber`, `green`, `blue`, `purple`, `violet`, `teal`, `rose`. Only allowed on enum fields; every key must match an existing variant (parse error otherwise). Drives the generated `EnumBadge` component in `list.tsx` with Tailwind classes per token.

## Write-Time Rules — Quick Reference

CEL expressions evaluated in-process (no gRPC), fail-closed, *before* any hook. Syntax-validated at parse, type-checked at apply.

| Annotation | Fires | Effect |
|---|---|---|
| `@require("expr", "msg")` | create + update | Reject (422 `msg`) unless `expr` is exactly `true`; non-boolean/eval-error → 500 |
| `@compute("expr")` | create + update | Compute and store, overwriting client input; computes chain in field order |
| `@default("expr")` | create only | Fill an absent/null field before `@compute` (≠ the `default(value)` literal modifier) |

**Bindings:** every field by name · `now` (request-time `timestamp` variable, *not* a `now()` call) · `principal` (`.sub`, `.email`, `.username`, `.roles`, `.perms`).

**Cross-entity reads (`@require` only):** `related.<F>.<col>` dereferences a single-valued relation field `F` to its committed, tenant-scoped related row. Single hop only; fail-closed if absent/null/tenant-hidden; multi-hop and to-many are rejected. Example: `@require("status != 'closed' || related.approval.state == 'granted'", "...")`.

**Order:** `@default → @compute → @require → before_validate / before_change → PERSIST → after_*/webhook`.

**Rules vs. hooks:** use rules when the logic depends only on the record, `now`, the caller, and one related row; use a `@hook` for external I/O. See [dsl-reference.md](dsl-reference.md).

## Lifecycle Hooks — Quick Reference

Hooks let schemas call out to an external gRPC service at well-defined lifecycle events. The implementation lives in a separate `acton-service` project — SchemaForge itself only dispatches.

### Declaration

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
| `before_validate` | POST/PUT | yes | yes | yes |

`before_validate` is dispatched after the write-time rule phases and before `before_change`. For simple record-local validation prefer a `@require` rule (in-process, no gRPC); use a hook when the check needs external I/O. For async work use the corresponding `after_*` event — fire-and-forget failures are logged, never reach the client, and the entity is already committed when they fire. For file-field scanners, use `after_upload` to run AV/OCR against the presigned `download_url` in the request, then post the verdict back via the `/scan-complete` endpoint (which in turn fires `on_scan_complete`).

### Workflow

1. Annotate schemas with `@hook(event) """intent"""`.
2. `schema-forge hooks generate --all --schema-dir schemas --out-dir hooks-service`.
3. Implement each stub in `src/hooks/<schema>.rs`. Return `abort_reason: Some(...)` to reject (becomes a 422 `hook_aborted`); set optional response fields to overwrite the entity before persistence.
4. `cargo run` the hook service on its own port.
5. Configure `[schema_forge.hooks]` with `enabled = true` and a `[[schema_forge.hooks.bindings]]` entry per `(schema, event)` pair.
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
