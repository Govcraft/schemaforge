# Export Reference — Bulk Export as an Authorized, Fail-Closed Query

SchemaForge's bulk-export capability lets a caller pull many rows of an entity —
optionally filtered — into a file: CSV, NDJSON, XLSX, or a multi-entity ZIP
bundle. One mental model carries the whole feature:

> **An export is a query with no page limit, materialized to a file.**

The query/filter IR, tenant scoping, and per-field read stripping are reused
**unchanged** from the normal query path. Two new fail-closed gates layer on top:
a distinct Cedar action and a per-field opt-in. Export is therefore never a
backdoor around the authorization the query path already enforces — the file is,
by construction, a subset of what the caller could already read one record at a
time.

This is a US-government production target on an ATO track, so the design treats
bulk export as an **exfiltration surface**, not merely a read. See
[ADR-0003](../../docs/adr/0003-export-capability.md) for the full security
rationale.

## Why export is not just "read with no limit"

Two risks force the two-gate design:

- **Bulk export is separately authorizable from read.** A policy author may
  legitimately grant "read one record" (a caseworker looking up a single
  subject) while **denying** "pull the whole table to a file". If export reused
  the `Read` decision, anyone who can read one row could drain the table. Export
  is gated by a **distinct Cedar action** instead.
- **Read access ≠ export consent.** A field can be fine to display inline behind
  a screen yet inappropriate to ship in a CSV on someone's laptop (an SSN, say).
  Export eligibility is a separate, **fail-closed** consent declared per field
  with `@exportable`.

## DSL: the two annotations

**Entity-level — `@export`** (fail-closed: no annotation ⇒ no export at all):

```
@export(formats: [csv, ndjson, xlsx], bundle_files: false, max_rows: 100000)
@display("full_name")
schema Contact { ... }
```

- `formats` — deliverable shapes from `csv | ndjson | xlsx | zip`; non-empty. A
  request for a format outside this list is rejected (400).
- `bundle_files` — when `true`, `file`-field blobs are pulled into the ZIP
  bundle (otherwise files surface as URL/metadata only). Default `false`.
- `max_rows` — caps a single export; an over-cap result is **refused** (413),
  not truncated. Intersected (min) with the server-wide ceiling.

**Field-level — `@exportable`** (fail-closed: no annotation ⇒ never in a file):

```
first_name: text @exportable
tags:       text[] @exportable(flatten: json)
ssn:        text(max: 4) @field_access(read: ["hr"])   // readable, never exportable
```

- `flatten: json` — force JSON-in-cell rendering for the rectangular formats
  (CSV/XLSX); never affects NDJSON, which is always lossless.
- `@exportable` on a field of a schema without `@export` is a **parse-time
  error**.

## The intersection rule (the security invariant)

The exported column set is **always** the intersection — never a union:

```
exported_columns = { f : @exportable(f) }  ∩  { f : field_access permits Read for caller }
```

`@exportable` is **independent** of read access (a separate consent) but **never
wider** than it. A field that is `@exportable` but not readable by the caller is
still stripped by the same `filter_entity_fields` the query path runs. A
caller-supplied `fields` list is intersected into this set too — it can only
narrow, never widen, what leaves. If every requested field is non-exportable,
the request is denied (`forge.export.denied`, reason `no_exportable_fields_requested`).

Tenant injection (`inject_tenant_scope`) runs identically to the query path, so
an export can never reach across tenants.

## Authorization: a distinct Cedar action

Export is gated by `AccessAction::Export`, which maps to its own Cedar action UID
`Export{Entity}` — **not** an alias for `Read`. A policy can therefore:

```
permit (principal in Role::"caseworker", action == Action::"ReadContact", resource);
forbid (principal in Role::"caseworker", action == Action::"ExportContact", resource);
```

Read-one is permitted; export-many is denied on the same principal/entity.
Strict-mode policy validation (`schemaforge policies validate`) covers the new
action, so a policy that references export is validated, not silently ignored.

## Endpoints

| Method | Path | Purpose |
|--------|------|---------|
| POST | `/api/v1/forge/schemas/:schema/entities/export` | Export to a file. Body `{ filter?, fields?, format, async? }`. |
| GET | `/api/v1/forge/schemas/:schema/exports/:job_id` | Poll an async job; presigned download URL once complete. |

**Sync (streaming) path.** A `csv`/`ndjson` export within the row cap and not
flagged `async` streams the file **inline**: `200 OK`, format `Content-Type`,
`Content-Disposition: attachment; filename="<schema>.<ext>"`. CSV is the
rectangular projection; NDJSON is the lossless dump.

**Async (job) path.** `xlsx`, `zip`, an explicit `async: true`, or an over-cap
result returns `202 Accepted` with `{ job_id, status: "queued", schema, format }`.
XLSX is async-only because `rust_xlsxwriter` buffers the whole workbook; ZIP is
async-only because the multi-member archive (and any bundled file blobs) is
staged before upload. The job is a **supervised acton-reactive actor** — never a
detached `tokio::spawn` — moving through `queued → running → complete | failed`.

Poll `GET .../exports/:job_id`:

```json
{ "job_id": "…", "schema": "Contact", "format": "xlsx",
  "status": "complete", "row_count": 4213,
  "download_url": "https://s3…/exports/…?X-Amz-Expires=…" }
```

The `download_url` is minted **on demand** as a TTL-bounded presigned GET URL;
fetch it **without** a Bearer token (it is self-authorizing and points at the
object store, not the API). Job reads are scoped to the **initiating subject** —
another caller's `job_id` returns `404` (existence never leaks), defending
against IDOR / cross-subject exfiltration.

## Serialization flatten policy (one documented contract)

The `DynamicValue → cell/value` mapping is a **pure, unit-tested** core
(`schema-forge-core/src/export`). One policy governs every format:

| `DynamicValue` shape | CSV / XLSX cell | NDJSON value |
| --- | --- | --- |
| scalar (text / int / float / bool / datetime / duration / enum) | value as-is | value as-is |
| relation (one) | resolved `@display` value | `{ id, display }` object |
| relation (many) / array / map / composite | JSON-encoded in the cell | native, lossless |
| bytes | omitted by default; base64 only on opt-in | base64 string |
| file | URL / metadata only | URL / metadata only |

XLSX shares the CSV cell rendering (it is also rectangular). Raw `file` blobs are
materialized **only** inside a ZIP bundle when `@export(bundle_files: true)` —
never in a CSV/XLSX cell. A relation whose target row is missing, unreadable, or
has no `@display` value resolves to an empty cell (CSV/XLSX) or a `null` display
(NDJSON).

## Hardening bounds (configurable, fail-closed)

`[schema_forge.export]` in `config.toml`:

```toml
[schema_forge.export]
default_max_rows = 100000        # server-wide ceiling; @export(max_rows) is min'd with it

[schema_forge.export.rate_limit]
max_requests = 30                # export initiations per subject per window; 0 = kill switch
window_secs  = 60
```

- **Row ceiling.** The effective cap is `min(@export(max_rows), default_max_rows)`
  — a schema can narrow it but never widen it above the operator's bound.
- **Per-subject rate limit.** A supervised `ExportRateLimiter` actor over a pure
  fixed-window core, keyed by subject (anonymous callers share one bucket).
  Exceeding the window returns `429`. `max_requests = 0` disables export
  entirely.

## Audit trail (AU-2)

Every request emits one of `forge.export.initiated` / `forge.export.completed` /
`forge.export.denied` via `state.audit_logger().log_custom`, carrying subject,
schema, filter, requested fields, row count, and format — a complete exfiltration
record of who pulled what, with which filter, and how much. Denials carry a
reason (`rate_limited`, `no_exportable_fields_requested`, authz/cap/format).

## CLI

```bash
# small CSV streams inline to a file
schemaforge entity export Contact --eq status=active --fields first_name,last_name -o contacts.csv

# lossless NDJSON to stdout
schemaforge entity export Contact --format ndjson | jq '.'

# xlsx/zip (and --async, or an over-cap result) run as a job; --async polls + downloads
schemaforge entity export Contact --format xlsx --async -o ./exports/
```

When `-o`/`--out` names a directory, the CLI joins a **sanitized basename** of the
server-suggested filename onto it: a name containing `..`, a path separator, or an
absolute path is rejected and falls back to a safe default, so a malicious or
compromised server can never steer the artifact outside your chosen directory
(path traversal).

See [cli-reference.md](cli-reference.md) for every flag and
[rest-api-reference.md](rest-api-reference.md) for the wire contract and error
paths (403 export-denied, 413 over-cap, 422 deferred, 429 rate-limited).

## Error paths at a glance

| Status | `error` kind | When |
|--------|--------------|------|
| 400 | `invalid_query` | `format` outside `csv\|ndjson\|xlsx\|zip` |
| 403 | `forbidden` | no `@export`; Cedar `Export{Entity}` denied; all requested fields non-exportable |
| 413 | `export_too_large` | result exceeds the resolved `max_rows` cap |
| 422 | `export_deferred` | non-streamable / over-cap / `async` deferred to the job endpoint |
| 429 | `rate_limited` | per-subject export rate limit exceeded |

## See also

- [ADR-0003](../../docs/adr/0003-export-capability.md) — the security model and alternatives considered
- [dsl-reference.md](dsl-reference.md) — `@export` / `@exportable` annotation syntax and the flatten table
- [rest-api-reference.md](rest-api-reference.md) — endpoint contract
- [cli-reference.md](cli-reference.md) — `entity export` subcommand
- [config-reference.md](config-reference.md) — `[schema_forge.export]` bounds
- [query-api-reference.md](query-api-reference.md) — the filter grammar an export reuses unchanged
