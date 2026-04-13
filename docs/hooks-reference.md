# Hooks Reference

SchemaForge generates full CRUD endpoints from `.schema` files, but real
applications need more than raw create/read/update/delete. Hooks let you
inject custom business logic — validation, enrichment, notifications,
moderation — at specific entity lifecycle points without touching
SchemaForge itself.

A hook is declared in the DSL with a one-line annotation and implemented
as an independently-deployed gRPC service. SchemaForge calls into your
service at the lifecycle event; your service may modify the entity,
abort the operation, or just observe. The scaffold command generates a
complete `acton-service` project from your annotated schemas, so the
only thing you write by hand is the business logic inside each stub.

This reference walks you through the full loop: declare a hook,
generate the service, implement the handlers, configure dispatch, and
run the resulting system. If you already have a running SchemaForge
deployment and are hitting the limits of pure CRUD, start at
[Section 2](#2-declaring-hooks). If you just want to see a worked
example before reading anything else, jump to
[Section 3.1](#31-generate-the-scaffold).

---

## Table of Contents

1. [Overview](#1-overview)
2. [Declaring Hooks](#2-declaring-hooks)
   - 2.1 [The `@hook` annotation](#21-the-hook-annotation)
   - 2.2 [Lifecycle events](#22-lifecycle-events)
   - 2.3 [Dispatch lifecycle](#23-dispatch-lifecycle)
3. [Building the Hook Service](#3-building-the-hook-service)
   - 3.1 [Generate the scaffold](#31-generate-the-scaffold)
   - 3.2 [Project layout](#32-project-layout)
   - 3.3 [Implement the stubs](#33-implement-the-stubs)
   - 3.4 [Wire format contract](#34-wire-format-contract)
4. [Running SchemaForge with Hooks](#4-running-schemaforge-with-hooks)
   - 4.1 [Configuration](#41-configuration)
   - 4.2 [Observing dispatch](#42-observing-dispatch)
   - 4.3 [Failure modes](#43-failure-modes)
5. [Evolving Hooks Over Time](#5-evolving-hooks-over-time)
   - 5.1 [Schema migration diffs](#51-schema-migration-diffs)
   - 5.2 [`hooks list` and `hooks diff`](#52-hooks-list-and-hooks-diff)

---

## 1. Overview

Hooks extend SchemaForge's CRUD pipeline at well-defined lifecycle
points. The runtime calls out to your gRPC service at the moment of
interest; your service responds with a typed message that tells
SchemaForge whether to proceed, modify the payload, or abort.

Three properties keep hooks cheap to adopt and cheap to operate:

- **Declared in the schema, not in code.** Adding `@hook(before_change)`
  to a schema is the *only* change you make inside SchemaForge. The
  implementation lives elsewhere.
- **Typed per-schema wire format.** The scaffold command generates a
  proto file whose request/response messages match your schema's
  fields exactly. There is no untyped JSON envelope to parse.
- **Zero cost when unused.** Schemas without hook annotations pay no
  per-request dispatcher overhead. Read-side hooks additionally
  early-exit on a per-event check, so declaring `@hook(before_change)`
  on one schema does not slow down reads of that schema or any other.

A hook service is a normal `acton-service` project. It ships with the
same observability, resilience, and auth primitives as any other
`acton-service`, and it runs in your own infrastructure under your own
supervision — SchemaForge never owns the process.

---

## 2. Declaring Hooks

Hooks live on schemas, not on individual CRUD calls. Annotating a
schema with `@hook(event)` turns on dispatch for that schema at that
lifecycle point; SchemaForge does the rest.

### 2.1 The `@hook` annotation

The annotation takes two arguments: a lifecycle **event** and a
triple-quoted **intent** string that documents what the hook is
supposed to do.

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

The intent string is **not** executed code — it is a natural-language
description that the scaffold generator bakes into each stub's
docstring and `.prompt.md` file. If you use an AI coding assistant to
fill in the stubs, the intent string is the prompt.

A single schema may declare multiple `@hook` annotations, one per
lifecycle event. Declaring the same event twice is a parse error.

Hook events are **opt-in per event** — SchemaForge only dispatches
for events that appear in the schema. A schema with only
`@hook(before_change)` never triggers `after_change`, `before_delete`,
or any other event.

### 2.2 Lifecycle events

SchemaForge supports seven lifecycle events. Five are wired into the
current runtime; two are reserved for future use.

| Event | DSL keyword | Fires on | Blocking? | May abort? | May modify? | System operation values |
|---|---|---|---|---|---|---|
| Before change | `before_change` | POST/PUT | yes | yes | yes | `create`, `update` |
| After change | `after_change` | POST/PUT | no (fire-and-forget) | no | no | `create`, `update` |
| Before delete | `before_delete` | DELETE | yes | yes | n/a (no payload) | `delete` |
| After delete | `after_delete` | DELETE | no (fire-and-forget) | no | n/a | `delete` |
| Before read | `before_read` | GET one, GET list, POST query | yes | yes | n/a (no payload) | `read`, `list`, `query` |
| After read | `after_read` | GET one | yes | yes | yes | `read` |
| Before validate | `before_validate` | *(reserved)* | — | — | — | — |

A few semantic notes:

- **Blocking events** hold the HTTP request until the hook returns.
  SchemaForge only persists (or responds) after the hook accepts the
  operation. If you need something asynchronous, use the corresponding
  `after_*` event instead.
- **Fire-and-forget events** (`after_change`, `after_delete`) are
  dispatched on a background task. Errors are logged; they never
  reach the HTTP client. The persisted entity is already committed
  when these fire.
- **Read-side hooks** fire on the list and query endpoints for
  `before_read` only, with `operation` set to `list` or `query`
  respectively. `after_read` is per-entity and currently fires only
  on single-entity GETs.
- **`before_validate`** is reserved; the variant exists in the DSL
  today but is not yet wired into the runtime. Use `before_change`
  for pre-persistence logic.

### 2.3 Dispatch lifecycle

The flow below shows what happens on a `POST /schemas/Translation/entities`
request against a schema with both `@hook(before_change)` and
`@hook(after_change)` declared. Blocking events are drawn with solid
arrows; fire-and-forget with dashed.

```
  HTTP request
       │
       ▼
  validate fields
       │
       ▼
  access checks
       │
       ▼
  ┌──────────────────────┐
  │  before_change hook  │────▶ abort_reason? ──yes──▶  422 Unprocessable
  │  (blocking gRPC)     │                             (hook_aborted)
  └──────────────────────┘
       │
       │ modified_fields merged into entity payload
       ▼
  persist to backend
       │
       ├───────────────────────────▶  200/201 response
       │                              (synchronous path ends here)
       ╎
       ╎ (background task)
       ▼
  ┌──────────────────────┐
  │  after_change hook   │
  │  (fire-and-forget)   │
  └──────────────────────┘
```

On failure, the path the client sees depends on two things: whether
the hook is declared **required** in the config, and which failure
mode occurred. See [Section 4.3](#43-failure-modes) for the full
matrix.

---

## 3. Building the Hook Service

With a schema annotated, the next step is to produce the gRPC service
that SchemaForge will call. `schema-forge hooks generate` does this
from the schema directory — you never hand-write protobufs.

### 3.1 Generate the scaffold

From a directory containing the `Translation` schema:

```console
$ schema-forge hooks generate --all \
    --schema-dir schemas \
    --out-dir hooks-service
Scanning schemas in schemas...
  found 1 schema(s) with hooks
Hook service scaffold written to hooks-service
  Next steps:
    cd hooks-service && cargo check
    Implement each TODO in src/hooks/<schema>.rs
    Read the .prompt.md files for AI-assist prompts
```

Two flags select scope:

- `--all` — one combined project containing every schema with
  `@hook(...)` annotations. This is the recommended deployment
  topology: a single hook service binary per SchemaForge deployment,
  talking to all hooked schemas.
- `--schema Translation` — only the named schema. Useful when you
  want to deploy hook services per bounded context and maintain them
  independently.

Pass `--force` to overwrite the per-schema implementation files
(`src/hooks/<schema>.rs`). Without `--force`, re-running the command
leaves your edits intact — only the proto files, `main.rs`, and
prompt files are regenerated.

### 3.2 Project layout

```
hooks-service/
├── Cargo.toml
├── build.rs
├── proto/
│   └── translation_hooks.proto         # one per annotated schema
└── src/
    ├── main.rs                          # regenerated each run
    └── hooks/
        ├── mod.rs                       # regenerated each run
        ├── translation.rs               # preserved without --force
        └── translation/
            ├── before_change.prompt.md  # regenerated each run
            └── after_change.prompt.md   # regenerated each run
```

The proto file is the source of truth for the wire format. The
`build.rs` compiles it into Rust code and emits a `FileDescriptorSet`
binary that SchemaForge loads at startup — this is how SchemaForge
learns the exact request/response shape at runtime.

The `.prompt.md` files describe each stub in enough detail that an AI
assistant can fill the body in one shot: they include the intent, the
request/response field tables, and a "Done when" checklist.

### 3.3 Implement the stubs

Each stub arrives as a no-op that returns the default response. Open
`src/hooks/translation.rs`:

```rust
use crate::pb::translation::translation_hooks_server::TranslationHooks;
use crate::pb::translation::*;
use tonic::{Request, Response, Status};

#[derive(Default)]
pub struct Service;

#[tonic::async_trait]
impl TranslationHooks for Service {
    /// Normalize source_text and call the external translation API
    async fn before_change(
        &self,
        request: Request<TranslationBeforeChangeRequest>,
    ) -> Result<Response<TranslationBeforeChangeResponse>, Status> {
        let _req = request.into_inner();
        // TODO: implement before_change for `Translation` — see
        //       src/hooks/translation/before_change.prompt.md
        Ok(Response::new(TranslationBeforeChangeResponse::default()))
    }

    /// Publish a translation.completed event to NATS
    async fn after_change(
        &self,
        request: Request<TranslationAfterChangeRequest>,
    ) -> Result<Response<TranslationAfterChangeResponse>, Status> {
        let _req = request.into_inner();
        // TODO: ...
        Ok(Response::new(TranslationAfterChangeResponse::default()))
    }
}
```

A realistic `before_change` body looks like this:

```rust
async fn before_change(
    &self,
    request: Request<TranslationBeforeChangeRequest>,
) -> Result<Response<TranslationBeforeChangeResponse>, Status> {
    let req = request.into_inner();

    // Abort if profanity detected in source_text.
    if contains_profanity(&req.source_text) {
        return Ok(Response::new(TranslationBeforeChangeResponse {
            abort_reason: Some("profanity detected".to_string()),
            ..Default::default()
        }));
    }

    // Call the external translation API and patch translated_text.
    let translated = external_translate(&req.source_text, "es").await?;
    Ok(Response::new(TranslationBeforeChangeResponse {
        abort_reason: None,
        translated_text: Some(translated),
        ..Default::default()
    }))
}
```

Two patterns to note:

- **Return `abort_reason: Some(...)`** to reject the operation.
  SchemaForge surfaces the message to the HTTP client as a 422.
- **Set any optional response field** to overwrite that field in the
  entity before persistence. Fields you leave at `None` are left
  untouched; fields you set win over whatever the client submitted.

Compile and run the hook service on its own port:

```console
$ cd hooks-service
$ cargo run
   Compiling hooks-service v0.1.0
    Finished dev [unoptimized + debuginfo] target(s) in 12.3s
     Running `target/debug/hooks-service`
hook service listening on 0.0.0.0:9090
```

### 3.4 Wire format contract

SchemaForge and your hook service agree on a small, predictable
protobuf contract. Understanding it matters when you want to evolve
either side independently.

**Service and method naming.** For a schema named `Translation`:

- Service: `TranslationHooks` (inside package `schema_forge_hooks.translation`)
- Method per event: PascalCase form of the event, e.g. `BeforeChange`,
  `AfterChange`, `BeforeDelete`

**Request message.** Named `{Schema}{Event}Request`. It contains:

| Field | Type | Tag | Source |
|---|---|---|---|
| `operation` | `string` | 1 | System — current operation name |
| `user_id` | `optional string` | 2 | System — authenticated user's subject claim |
| `entity_id` | `optional string` | 3 | System — entity id (absent on create) |
| *schema field* | *mapped type* | 100+ | One per declared schema field |

Schema fields start at tag 100 so system fields stay stable as your
schema grows. Required schema fields are plain protobuf fields;
optional ones use `optional`.

Scalar field-type mapping:

| DSL type | Proto type |
|---|---|
| `text` | `string` |
| `integer` | `int64` |
| `float` | `double` |
| `boolean` | `bool` |
| `datetime` | `string` (RFC3339) |
| `enum` | `string` |
| `relation` | `string` (entity id) |

**Response message.** Named `{Schema}{Event}Response`. For blocking
events it contains:

| Field | Type | Tag | Meaning |
|---|---|---|---|
| `abort_reason` | `optional string` | 1 | Set to reject the operation |
| *schema field* | `optional` *mapped type* | 100+ | Set to overwrite that field in the entity |

Every schema field appears as **optional** in the response, regardless
of whether it is required in the schema. Setting a response field
replaces the value SchemaForge would otherwise persist; leaving it
unset means "no change from the incoming payload."

**Type coercion of response fields.** Because the wire format encodes
`datetime`, `enum`, and `relation` as protobuf `string`, the
dispatcher coerces each response field against the schema's declared
type before merging it into the pending payload. In particular,
`datetime` response fields are parsed from RFC3339 strings into typed
timestamps so they bind cleanly against `timestamp with time zone`
columns. A response value that cannot be coerced — for example, a
`datetime` field set to `"not-a-date"` — causes the hook call to fail
with HTTP 422 `hook_aborted` and a message identifying the offending
field.

For fire-and-forget events (`after_change`, `after_delete`), the
response message is empty — the transport round-trip still happens,
but its contents are ignored.

---

## 4. Running SchemaForge with Hooks

SchemaForge needs two things to dispatch to a hook service: the
`hooks.enabled` flag in config, and a binding per `(schema, event)`
pair that points to the endpoint and the proto descriptor file the
scaffold emitted.

### 4.1 Configuration

Hooks live under `[schema_forge.hooks]` in your `config.toml`:

```toml
[schema_forge.hooks]
enabled = true
default_timeout_ms = 5000
max_concurrent_async = 100

[[schema_forge.hooks.bindings]]
schema = "Translation"
event = "BeforeChange"
endpoint = "http://hooks-service:9090"
required = true
descriptor_path = "/var/lib/schemaforge/hooks_descriptor.bin"

[[schema_forge.hooks.bindings]]
schema = "Translation"
event = "AfterChange"
endpoint = "http://hooks-service:9090"
required = false
descriptor_path = "/var/lib/schemaforge/hooks_descriptor.bin"
```

Top-level fields:

| Field | Default | Meaning |
|---|---|---|
| `enabled` | `false` | Global kill-switch. When `false`, all hook annotations are ignored at runtime. Set this to `false` in local dev to run without hook services. |
| `default_timeout_ms` | `5000` | Per-call timeout applied to any binding that does not set its own. |
| `max_concurrent_async` | `100` | Upper bound on background after-hook dispatches. |
| `bindings` | `[]` | List of per-(schema, event) bindings. |

Per-binding fields:

| Field | Required | Meaning |
|---|---|---|
| `schema` | yes | Schema name, PascalCase, matching the DSL. |
| `event` | yes | PascalCase form of the event: `BeforeChange`, `AfterChange`, `BeforeRead`, `AfterRead`, `BeforeDelete`, `AfterDelete`. Note: config uses PascalCase here while the DSL uses `snake_case` (`before_change`). |
| `endpoint` | yes | gRPC endpoint URL, e.g. `http://translation-hooks:9090`. |
| `timeout_ms` | no | Per-binding override for `default_timeout_ms`. |
| `required` | no (`false`) | If `true`, SchemaForge fails the CRUD request when the hook is unreachable or times out. If `false`, such failures are logged and the operation proceeds. Explicit aborts from the hook always propagate, regardless of `required`. |
| `descriptor_path` | yes | Path to the compiled `FileDescriptorSet` binary that the hook scaffold's `build.rs` emits. SchemaForge loads this at startup to learn the typed request/response shape. |

**Descriptor path in practice.** The scaffold's `build.rs` writes its
`FileDescriptorSet` to the Cargo `OUT_DIR` and exposes the location
via the `HOOKS_DESCRIPTOR_PATH` build-env variable. When you deploy,
copy that `.bin` to a stable path (e.g. under `/var/lib/schemaforge/`)
and point `descriptor_path` at it. SchemaForge validates every binding
at startup and fails fast if a descriptor is missing, unreadable, or
does not contain the expected `{Schema}Hooks` service.

### 4.2 Observing dispatch

Hook dispatch runs inside the `ForgeActor`'s supervision tree, so
spans and audit events flow through the same observability pipeline
as the rest of SchemaForge. A successful `before_change` dispatch
looks like this in the logs (`RUST_LOG=debug`):

```
DEBUG schema_forge_acton::hooks: dispatching before hook
  schema=Translation event=BeforeChange
  endpoint=http://hooks-service:9090 required=true
DEBUG schema_forge_acton::hooks::tonic_dispatcher: tonic dispatch (before)
  schema=Translation event=BeforeChange endpoint=http://hooks-service:9090
```

After-hook failures log at `ERROR` and never propagate to the client:

```
ERROR schema_forge_acton::hooks: after hook dispatch failed
  schema=Translation event=AfterChange
  endpoint=http://hooks-service:9090
  error=hook at http://hooks-service:9090 unavailable: connection refused
```

Startup emits a single line confirming the dispatcher is online:

```
  Hook dispatcher initialized with 2 binding(s).
```

If this line is missing from the startup output despite bindings in
config, either `hooks.enabled = false` or descriptor validation
failed — check the error above the startup banner.

### 4.3 Failure modes

Five distinct outcomes are possible when a blocking hook runs. The
table below shows how each maps to the HTTP response, and how the
`required` flag changes the behavior.

| Outcome | `required = true` | `required = false` |
|---|---|---|
| Hook returns `abort_reason` | 422 `hook_aborted` with the reason | 422 `hook_aborted` with the reason |
| Hook returns modified fields | 2xx with fields applied | 2xx with fields applied |
| Hook returns empty response | 2xx, no changes | 2xx, no changes |
| Hook times out | 503 `hook_timeout` | 2xx, failure logged, operation proceeds |
| Endpoint unreachable | 503 `hook_unavailable` | 2xx, failure logged, operation proceeds |

Two rules govern the matrix:

- **Explicit aborts always propagate.** An `abort_reason` is a
  deliberate business decision — it bypasses the `required` policy
  and always becomes a 422.
- **`required` only affects transport failures.** Use `required = true`
  for hooks whose logic is load-bearing (e.g. compliance checks that
  must block a write). Use `required = false` for hooks whose
  unavailability should degrade gracefully (e.g. a non-critical
  enrichment step).

Fire-and-forget events (`after_change`, `after_delete`) never affect
the HTTP response. All failures are logged at `ERROR` and the client
sees the operation as successful.

---

## 5. Evolving Hooks Over Time

Schemas drift. Fields get added, events get rewritten, intents change
as the business learns. Hooks participate in SchemaForge's schema
evolution story the same way fields do.

### 5.1 Schema migration diffs

When you apply a schema change that touches hook annotations,
SchemaForge's diff engine emits one of three new migration steps:

| Step | Fires when | Migration safety |
|---|---|---|
| `AddHook` | A new `@hook(event)` appears | Safe |
| `RemoveHook` | An existing `@hook(event)` is removed | Safe |
| `ChangeHookIntent` | The intent string of an existing hook changes | Safe |

Hook migrations are **metadata-only** — no on-disk migration runs,
because the schema's fields haven't changed. They show up in
`schema-forge migrate plan` output alongside field-level steps:

```console
$ schema-forge migrate plan schemas/translation.schema
MigrationPlan for Translation:
  ADD HOOK before_change "Normalize source_text and call the external translation API"
  ADD HOOK after_change "Publish a translation.completed event to NATS"
Safety: Safe
```

The operator action is **not** a database migration — it's
regenerating and redeploying the hook service so its proto interface
matches the new schema shape. A schema change that adds a field will
change the request message for every hook on that schema; run
`schema-forge hooks generate` again and redeploy the hook service
before rolling the schema update forward.

### 5.2 `hooks list` and `hooks diff`

Two CLI commands help you see hook state at a glance without loading
the full schemas.

`schema-forge hooks list` enumerates every hook annotation across a
schema directory:

```console
$ schema-forge hooks list --schema-dir schemas
schema Translation
  before_change — Normalize source_text and call the external translation API
  after_change — Publish a translation.completed event to NATS
2 hook(s) total
```

`schema-forge hooks diff` compares two schema directories and reports
hook-level changes:

```console
$ schema-forge hooks diff schemas/old schemas/new
+ Translation.before_change — Normalize source_text and call the external translation API
- Translation.deprecated_notify
~ Translation.after_change (intent changed)
```

The three markers mirror the migration steps: `+` for added, `-` for
removed, `~` for an intent change. Use `hooks diff` in CI to gate
schema PRs on whether downstream hook services need to be
regenerated.

---

## See Also

- [Query API Reference](query-api-reference.md) — filtering, sorting,
  and pagination on the endpoints that hook events fire against.
- [Widget UI Reference](widget-ui-reference.md) — how hook-modified
  fields flow through the generated admin UI.
