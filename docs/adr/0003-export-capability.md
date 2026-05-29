# 0003 — Export capability: bulk export as an authorized, fail-closed query

## Status

Accepted

## Context

SchemaForge needs a first-class bulk-export capability: a caller asks for many
rows of an entity, optionally filtered, and receives a file (CSV, NDJSON, XLSX,
or a multi-entity ZIP bundle). The naive implementation — "run the existing
query with no page limit and serialize the result" — is mechanically correct but
a **security regression** for a US-government production target on an ATO
(Authority to Operate) track.

Two distinct risks force the design:

- **Bulk export is an exfiltration surface, not just a read.** A policy author
  may legitimately want to grant a caller "read one record at a time" (e.g. a
  caseworker looking up a single subject) while **denying** the ability to pull
  the whole table to a file. If export reuses the `Read` authorization decision,
  that split is impossible: anyone who can read one row can drain the table.
  Export must be separately authorizable from read.

- **Field-level read access is not the same as field-level export consent.** A
  field can be perfectly readable in a single-record API response yet still be
  inappropriate to ship in a downloadable file (an SSN that is fine to display
  inline behind a screen but must never sit in a CSV on someone's laptop). Read
  access and export eligibility are independent axes; export eligibility must
  never be assumed from read access.

The existing query path already enforces tenant scoping and field stripping that
export must not be allowed to bypass:

- The query/filter IR lives in `crates/schema-forge-core/src/query.rs` (`Query`,
  `Filter`, `FieldPath`). Export reuses it **unchanged** — an export *is* a query
  with no page limit.
- Tenant injection and per-field read filtering are enforced by
  `inject_tenant_scope` / `inject_tenant_on_create` and `filter_entity_fields`
  in `crates/schema-forge-acton/src/access.rs`. Export must run the **same**
  injection and stripping; it must not become a side door around them.
- Schema-level authorization flows through the `AccessAction` enum
  (`crates/schema-forge-acton/src/access.rs`) into the Cedar engine
  (`crates/schema-forge-acton/src/authz/`, `cedar/`), which maps each verb to
  action UIDs. Per-field access is driven by the `@field_access` annotation and
  emitted as per-field Cedar actions (`cedar/schema_gen.rs`, `cedar/policy_gen.rs`).

A file backend already exists (`crates/schema-forge-acton/src/.../storage`, S3 /
S3-compatible) and is the natural place to land generated artifacts and serve
time-limited presigned downloads. The acton-reactive runtime owns async work via
supervised actors; spawning detached tasks (`tokio::spawn`) inside a handler is a
project defect because it removes the work from supervision.

## Decision

Model export as **a query with no page limit, materialized to a file**, gated by
**two new fail-closed controls layered on top of the unchanged query path**: a
distinct Cedar action and a per-field opt-in. The query IR, tenant scoping, and
`filter_entity_fields` are reused verbatim.

### DSL surface (two annotations)

- **Entity-level — `@export`** enables export for an entity and is **fail-closed**:
  an entity with no `@export` annotation cannot be exported at all.

  ```
  @export(formats: [csv, ndjson, xlsx], bundle_files: false, max_rows: 100000)
  ```

  `formats` bounds the deliverable shapes, `max_rows` caps a single export, and
  `bundle_files` controls whether file-field blobs are pulled into a ZIP bundle.

- **Field-level — `@exportable`** opts a single field into bulk files and is
  **fail-closed**: a field with no `@exportable` cannot leave in a bulk file even
  if the caller can read it.

  ```
  @exportable
  @exportable(flatten: json)
  ```

### Security model

1. **Read vs. export are distinct Cedar actions.** A new `AccessAction::Export`
   variant is added to `crates/schema-forge-acton/src/access.rs` and wired
   through `authz/namespace.rs` and the Cedar schema/policy generators, mapping to
   its own action UID (`Export{Entity}`). It is **not** an alias for `Read`. A
   policy can therefore `permit` read-one and `forbid` export-many on the same
   principal/entity. Strict-mode policy validation is extended to cover the new
   action so a policy referencing export is validated, not silently ignored.

2. **Two-level fail-closed opt-in.** Export of an entity requires `@export` on
   the entity (level 1). Each column requires `@exportable` on the field
   (level 2). The absence of either annotation denies, never permits.

3. **Exported columns are an intersection, never a union.** The column set
   shipped to a caller is exactly:

   ```
   exported_columns = { f : @exportable(f) } ∩ { f : field_access permits Read for caller }
   ```

   `@exportable` is **independent** of read access — it is a separate consent —
   but it is **never wider** than read access. A field that is `@exportable` but
   not readable by the caller is stripped, using the same `filter_entity_fields`
   pass the single-record read path uses. Export can only ever be a *subset* of
   what the query path already authorizes.

4. **Same tenant injection and field stripping as the query path.** Export runs
   `inject_tenant_scope` and `filter_entity_fields` identically to a normal
   query. Export is never a path around the authz the query path enforces.

5. **AU-2 exfiltration audit trail.** Every export emits audit events via
   `state.audit_logger().log_custom`:
   `forge.export.initiated`, `forge.export.completed`, and
   `forge.export.denied`. Each event records subject, schema, filter, requested
   fields, resolved row count, and format — the bulk-disclosure trail an ATO
   reviewer expects under AU-2.

6. **Row cap and rate limit.** The configurable `max_rows` cap is enforced
   server-side (it bounds the materialized result regardless of what the caller
   requests), and bulk export is rate-limited so a caller cannot drain data by
   issuing many small exports back-to-back.

### Pure serialization core and flatten policy

A pure, unit-testable serialization core maps `DynamicValue` to a CSV cell,
NDJSON value, or XLSX cell under **one documented flatten policy**. CSV is the
rectangular projection (lossy where it must be); NDJSON is the lossless dump.

| `DynamicValue` shape | CSV / XLSX cell | NDJSON value |
| --- | --- | --- |
| scalar (text/int/float/bool/datetime/duration/enum) | value as-is | value as-is |
| relation (one) | resolved display value | `{ id, display }` object |
| relation (many) / array / map / composite | JSON-encoded in the cell | native, lossless |
| bytes | omitted by default; base64 only on opt-in | base64 string |
| file | URL / metadata only | URL / metadata only; raw blob only inside a ZIP bundle |

XLSX rides the async/buffered path because `rust_xlsxwriter` cannot truly stream.
ZIP bundles carry multi-entity exports and, when `bundle_files = true`, the raw
blobs of `file` fields.

### Delivery: sync stream vs. async job

- **`POST /schemas/{schema}/entities/export`** with body
  `{ filter?, fields?, format, async? }`.
  - If the result is **under `max_rows` AND the format is streamable** (CSV or
    NDJSON), the file is **streamed inline** in the response.
  - Otherwise — XLSX, ZIP, over-cap, or `async: true` — a **job is created** and
    its id returned.
- **`GET /schemas/{schema}/exports/{job_id}`** returns job status and, when
  complete, a **time-limited presigned GET URL** to the artifact in the S3
  storage backend.

The async job is an **acton-reactive actor**, not a `tokio::spawn`: the runtime
owns and supervises the task. Job states are
`queued → running → complete | failed`. The actor generates the artifact to the
existing S3 backend and the status endpoint returns a TTL-bounded presigned URL.

## Consequences

- **Read-one and export-many are independently grantable.** Because `Export` is
  its own Cedar action, a policy can permit single-record reads while forbidding
  bulk export — the central protection a federal data owner needs.
- **Default-deny at two levels.** Adding the capability does not expose any
  existing entity or field: nothing is exportable until an author adds `@export`
  and marks fields `@exportable`. A schema that predates this feature exports
  nothing, which is the correct fail-closed posture.
- **Export can never exceed read access.** The intersection with
  `filter_entity_fields` guarantees the file is a subset of what the caller could
  already read one row at a time; `@exportable` can only narrow, never widen.
- **No authz bypass.** Reusing the query IR, tenant injection, and field
  stripping unchanged means export inherits every guarantee the query path
  already proves, rather than reimplementing (and potentially weakening) them.
- **Auditable exfiltration trail.** The `forge.export.*` events give reviewers a
  complete record of who pulled what, with which filter, and how many rows —
  satisfying AU-2 for bulk disclosure.
- **Bounded blast radius.** The row cap and rate limit bound both a single
  export and the aggregate rate, so the capability cannot be turned into an
  unmetered table drain.
- **Pure core is testable without external services.** The `DynamicValue →
  cell/value` mapping is pure and unit-tested directly; the storage backend is
  abstracted so tests use in-process surreal (`mem://`) and a mock store, with no
  live MinIO/S3 dependency, consistent with the existing acton test suite.
- **Maintenance cost.** Two new annotations, a new Cedar action, an actor-based
  job lifecycle, and a serializer with a documented flatten contract are added
  surface to maintain and keep covered by strict-mode validation and tests. This
  is the accepted cost of an audited, fail-closed export rather than an
  unauthorized "query with no limit."

## Alternatives considered

- **Reuse `AccessAction::Read` for export.** Rejected. It makes read-one and
  export-many indistinguishable, so any principal who can read a single record
  can drain the whole table to a file. The read-vs-export split is the core
  security requirement; collapsing it defeats the feature's purpose for a
  federal data owner.

- **Export every readable field (no `@exportable`).** Rejected. It conflates
  "fine to display inline" with "fine to ship in a downloadable file" and would
  silently leak sensitive-but-readable fields (e.g. an SSN) into CSVs. Export
  eligibility is a separate, fail-closed consent that must be opted into per
  field.

- **Opt-out fields instead of opt-in.** Rejected. Opt-out is fail-open: a newly
  added field would be exportable by default until someone remembered to exclude
  it. Fail-closed opt-in is mandatory for an ATO target.

- **Always stream synchronously.** Rejected. XLSX cannot truly stream
  (`rust_xlsxwriter` buffers), large/over-cap exports would hold a request open
  and risk timeouts, and ZIP bundles need staging. The sync path is reserved for
  small, streamable formats; everything else takes the supervised async-job path.

- **`tokio::spawn` the export work inside the handler.** Hard no. It detaches the
  work from the acton-reactive runtime's supervision, contrary to project policy.
  The async job is a supervised actor instead.

- **Stream large files through the API process instead of presigned URLs.**
  Rejected. It ties up the API process for the life of a large download and
  duplicates the storage backend's strength. A TTL-bounded presigned GET URL
  hands the transfer to the storage layer and bounds the download window.

## References

- Issue (epic) — Export capability (epic to be filed; this ADR precedes it)
- ADR-0001 — `uint`: distinct DSL type vs. unsigned constraint
- ADR-0002 — CEL expression substrate: own evaluator over `DynamicValue`
- `crates/schema-forge-core/src/query.rs` — `Query` / `Filter` / `FieldPath` IR,
  reused unchanged (export = query with no page limit)
- `crates/schema-forge-acton/src/access.rs` — `AccessAction` (new `Export`
  variant), `filter_entity_fields`, tenant injection
- `crates/schema-forge-acton/src/authz/namespace.rs` — action verb → Cedar UID
  wiring (new `Export` action)
- `crates/schema-forge-acton/src/cedar/schema_gen.rs`,
  `crates/schema-forge-acton/src/cedar/policy_gen.rs` — Cedar schema/policy
  generation, including per-field `@field_access` actions
- `crates/schema-forge-acton/src/routes/` — `POST .../entities/export`,
  `GET .../exports/{job_id}` endpoints
- `crates/schema-forge-acton/src/.../storage` (`s3.rs`) — artifact storage and
  presigned download URLs
- `state.audit_logger().log_custom` — `forge.export.initiated` /
  `.completed` / `.denied` AU-2 events
