# Storage Reference — `file` Fields and S3-Compatible Storage

SchemaForge's `file` field type handles binary artifacts — contracts, evidence,
images, uploaded proposals — as a first-class DSL concept. One declaration
gives you a typed attachment column, REST endpoints for upload and download,
a lifecycle state machine, and three hook events where scanner / AV / OCR
services can intervene. The runtime never handles bytes on the upload path.

This reference walks through the full loop: declare the field, configure a
storage backend, use the three-endpoint upload flow from a client, plug in a
scanner via hooks, and operate the feature in production. If you already have
a running SchemaForge deployment, start at [Section 2](#2-declaring-a-file-field).

---

## Table of Contents

1. [Overview](#1-overview)
2. [Declaring a file field](#2-declaring-a-file-field)
   - 2.1 [DSL syntax](#21-dsl-syntax)
   - 2.2 [Size literals and MIME patterns](#22-size-literals-and-mime-patterns)
   - 2.3 [Access modes: presigned vs proxied](#23-access-modes-presigned-vs-proxied)
3. [Configuration](#3-configuration)
   - 3.1 [Named backends](#31-named-backends)
   - 3.2 [MinIO, AWS S3, R2, Wasabi](#32-minio-aws-s3-r2-wasabi)
4. [The three-endpoint upload flow](#4-the-three-endpoint-upload-flow)
   - 4.1 [POST upload-url](#41-post-upload-url)
   - 4.2 [PUT to the presigned URL](#42-put-to-the-presigned-url)
   - 4.3 [POST confirm-upload](#43-post-confirm-upload)
   - 4.4 [GET download](#44-get-download)
5. [State machine and scanning](#5-state-machine-and-scanning)
   - 5.1 [States and transitions](#51-states-and-transitions)
   - 5.2 [Scan integration via hooks](#52-scan-integration-via-hooks)
   - 5.3 [scan-complete callback](#53-scan-complete-callback)
6. [File-specific hook events](#6-file-specific-hook-events)
7. [Database storage and migrations](#7-database-storage-and-migrations)
8. [Operations](#8-operations)
   - 8.1 [Startup validation](#81-startup-validation)
   - 8.2 [Bucket layout and object keys](#82-bucket-layout-and-object-keys)
   - 8.3 [Failure modes](#83-failure-modes)
9. [Limitations and v1 scope](#9-limitations-and-v1-scope)

---

## 1. Overview

A `file` field stores an attachment's **metadata** on the entity (JSON object:
`{ key, size, mime, status, uploaded_at, checksum }`) while the **bytes** live
in an S3-compatible bucket. Clients upload directly to storage via a presigned
PUT URL; the runtime mints the URL, verifies the result, and gates downloads
on the lifecycle state.

Four properties keep the design honest:

- **The runtime never proxies upload bytes.** Uploads always go client → S3.
  This is a hard architectural invariant — `mint_upload_url` is the only way
  to place an object, and it returns a short-TTL presigned PUT URL signed
  against the configured bucket.
- **Downloads are configurable per field.** `access: "presigned"` (default)
  redirects the client to S3 for the byte fetch. `access: "proxied"` streams
  bytes through the runtime so authz is re-checked per request and every
  byte-fetch is auditable. Sensitive document fields should use `proxied`.
- **A state machine gates visibility.** An attachment moves through
  `pending → uploaded → scanning → available | quarantined | rejected`, and
  downloads are refused unless the state is `available`. A scanner (yours,
  plugged in via a hook) is responsible for the final transition.
- **S3-compatible only.** No local-FS backend, no per-backend abstraction
  trait. MinIO in development, AWS S3 / R2 / Wasabi / Ceph in production.
  Same code path.

---

## 2. Declaring a File Field

### 2.1 DSL syntax

```
field contract: file(
    bucket: "documents",
    max_size: "25MB",
    mime: ["application/pdf", "image/*"],
    access: "presigned"
)
```

Positioning follows the usual field form — `name: <type> <modifiers>
<annotations>`. The `file(...)` type takes four named parameters:

| Parameter | Required | Meaning |
|---|---|---|
| `bucket` | yes | Name of a configured backend. Must resolve to a `[schema_forge.storage.backends.<name>]` entry at startup, or the server refuses to boot. |
| `max_size` | yes | Maximum accepted size. Plain integer (bytes) or quoted string with a suffix (`"25MB"`, `"500KB"`, `"1GiB"`). |
| `mime` | yes | Allowlist of MIME patterns. Exact types (`"application/pdf"`) or families (`"image/*"`). At least one entry. |
| `access` | no (`"presigned"`) | `"presigned"` to 302-redirect downloads to a signed GET URL, or `"proxied"` to stream bytes through the runtime. |

The `required` modifier and `@field_access(...)` annotation both work on
file fields and mean what they usually mean: `required` rejects writes that
leave the attachment null, and `@field_access(...)` gates visibility of the
attachment metadata itself. File endpoints additionally enforce the schema's
`@access(...)` — `Write` for mint/confirm, `Read` for download.

### 2.2 Size literals and MIME patterns

**Size literals** use base-1024 (KiB-style) because object-storage tooling
consistently uses that convention for size thresholds. Accepted forms:

| Written | Interpreted as |
|---|---|
| `1024` | 1,024 bytes (plain integer) |
| `"25MB"` / `"25M"` / `"25MiB"` / `"25mb"` | 26,214,400 bytes |
| `"1KB"` / `"1K"` / `"1KiB"` | 1,024 bytes |
| `"2TB"` / `"2T"` / `"2TiB"` | 2 × 1024⁴ bytes |

Invalid forms (`"25XB"`, `"MB"`, `"-25MB"`, empty, suffix-only) raise a
`InvalidSizeLiteral` parse error.

**MIME patterns** come in two shapes:

- **Exact:** `"application/pdf"` matches only that type.
- **Family:** `"image/*"` matches any type whose prefix is `image`
  (case-insensitive).

The allowlist is enforced at two points: at mint time (the client's claimed
`mime` must pass) and at confirm time (the observed `Content-Type` on the
stored object must also pass, if the backend returns one on `HEAD`).

MIME is a **declaration**, not a guarantee — clients can lie about
`Content-Type`. The runtime adds two defenses on top:

- Presigned POST policies bind the exact `Content-Type` header at signing
  time, so a client uploading with a different type gets rejected by S3
  before any bytes land.
- `HeadObject` at confirm time re-reads the Content-Type the backend
  recorded. A mismatch fails the confirm with a 422.

Magic-byte verification inside the runtime is v1.1 scope; v1 relies on
client-asserted MIME plus the two checks above, or delegates deeper
inspection to an `after_upload` hook.

### 2.3 Access modes: presigned vs proxied

`access: "presigned"` (default) is the right choice for most fields:

- The runtime mints a short-TTL presigned GET (default 300s) and 302s the
  client to S3.
- Bytes go S3 → client direct — no runtime bandwidth cost, no streaming.
- Range requests, caching, resume-on-failure are all handled by S3.
- The caveat: a presigned URL is a bearer token until expiry. Revoking
  access after mint requires waiting out the TTL.

`access: "proxied"` is the right choice for classified or PII-heavy documents:

- The runtime opens a `GetObject` stream from S3 and pipes bytes through to
  the client.
- Authz is re-checked on every request, so permission revocations take
  effect instantly.
- Every byte-fetch can be logged for audit.
- Range requests are forwarded. Content-Length is set.
- The cost: runtime bandwidth, no S3 caching benefit, higher per-request
  latency.

Mix and match per field inside the same schema — a `Document` entity can
have `cover_image: file(..., access: "presigned")` and
`classified_pdf: file(..., access: "proxied")`.

---

## 3. Configuration

### 3.1 Named backends

File fields reference backends by name. Declare each backend under
`[schema_forge.storage.backends.<name>]`:

```toml
[schema_forge.storage]
# TTL applied to presigned URLs when a backend does not override it.
default_presign_ttl_secs = 300

[schema_forge.storage.backends.documents]
endpoint = "http://localhost:9100"       # MinIO dev; omit for AWS
region = "us-east-1"
bucket = "forge-documents"
access_key_id = "${S3_ACCESS_KEY}"
secret_access_key = "${S3_SECRET_KEY}"
force_path_style = true                   # required for MinIO
presign_ttl_secs = 300                    # optional per-backend override

[schema_forge.storage.backends.evidence]
region = "us-east-1"
bucket = "forge-evidence"
# No explicit keys — uses the IAM role on EC2 / ECS / EKS.
```

Per-backend fields:

| Field | Required | Meaning |
|---|---|---|
| `endpoint` | no | Override endpoint URL. Required for MinIO and Wasabi; omit for AWS S3 so the SDK picks the regional endpoint. |
| `region` | yes | AWS region (e.g. `"us-east-1"`, `"eu-west-1"`). MinIO ignores region but still requires a non-empty string. |
| `bucket` | yes | Bucket name within the backend. |
| `access_key_id` | no | Static access key. Omit to use the default AWS credentials chain (env vars, IAM role, SSO, etc.). |
| `secret_access_key` | no | Static secret paired with `access_key_id`. |
| `session_token` | no | STS session token, when using temporary credentials. |
| `force_path_style` | no (`false`) | `true` forces `http://host/bucket/key` addressing. Required for MinIO; leave unset for AWS. |
| `presign_ttl_secs` | no | Override `default_presign_ttl_secs` for this backend. |

### 3.2 MinIO, AWS S3, R2, Wasabi

Every backend that speaks the AWS S3 API works:

**MinIO (dev):**
```toml
[schema_forge.storage.backends.local]
endpoint = "http://127.0.0.1:9100"
region = "us-east-1"
bucket = "forge-dev"
access_key_id = "minioadmin"
secret_access_key = "minioadmin"
force_path_style = true
```

**AWS S3 with IAM role:**
```toml
[schema_forge.storage.backends.documents]
region = "us-east-1"
bucket = "forge-documents"
# No keys — reads AWS_WEB_IDENTITY_TOKEN_FILE / IMDS.
```

**Cloudflare R2:**
```toml
[schema_forge.storage.backends.assets]
endpoint = "https://<account-id>.r2.cloudflarestorage.com"
region = "auto"
bucket = "forge-assets"
access_key_id = "${R2_ACCESS_KEY}"
secret_access_key = "${R2_SECRET}"
```

**Wasabi:**
```toml
[schema_forge.storage.backends.archive]
endpoint = "https://s3.us-east-1.wasabisys.com"
region = "us-east-1"
bucket = "forge-archive"
access_key_id = "${WASABI_ACCESS_KEY}"
secret_access_key = "${WASABI_SECRET}"
```

The runtime pins to `aws-sdk-s3` 1.122 with SigV4, so anything that
authenticates AWS clients authenticates SchemaForge.

---

## 4. The Three-Endpoint Upload Flow

Every file upload is exactly three HTTP calls. All paths below are scoped
under `/api/v1/forge/schemas/{schema}/entities/{id}/fields/{field}`.

```
  Client                         Runtime                         S3
    │                               │                             │
    │ POST /upload-url              │                             │
    │ {filename, mime, size} ─────▶ │                             │
    │                               │  check_schema_access(Write) │
    │                               │  validate size & mime       │
    │                               │  fire before_upload (block) │
    │                               │  mint presigned PUT URL     │
    │◀──── {upload_url, key, ...} ──│                             │
    │                               │                             │
    │ PUT bytes ────────────────────┼────────────────────────────▶│
    │◀────────────────────── 200 ETag ────────────────────────── │
    │                               │                             │
    │ POST /confirm-upload          │                             │
    │ {key, checksum?} ───────────▶ │                             │
    │                               │  HeadObject(key) ──────────▶│
    │                               │◀── metadata ─────────────── │
    │                               │  persist FileAttachment     │
    │                               │  fire after_upload (detach) │
    │◀─── {status, attachment} ────│                             │
    │                               │                             │
    │                      (scanner hook eventually calls         │
    │                       /scan-complete — see §5)              │
    │                               │                             │
    │ GET /fields/{field}           │                             │
    │ (presigned) ────────────────▶ │  302 to signed GET ────────▶│
    │ (proxied)   ────────────────▶ │  GetObject stream through ──│
```

### 4.1 POST upload-url

`POST /api/v1/forge/schemas/{schema}/entities/{id}/fields/{field}/upload-url`

Mints a presigned PUT URL. Requires `Write` access on the schema.

Request:
```json
{
  "filename": "proposal.pdf",
  "mime": "application/pdf",
  "size": 1048576
}
```

Response:
```json
{
  "upload_url": "https://bucket.s3.amazonaws.com/key?X-Amz-Signature=...",
  "key": "tenant_abc/Deal/entity_01J.../contract/019200.../proposal.pdf",
  "headers": { "Content-Type": "application/pdf" },
  "expires_at": "2026-04-16T17:05:00Z"
}
```

The client MUST send the values in `headers` verbatim as request headers on
the subsequent PUT, or the signature will not match.

Errors:

| Status | Reason |
|---|---|
| 400 | Invalid schema name or entity id. |
| 401 | No / expired bearer token. |
| 403 | Schema denies `Write` for this user. |
| 404 | Schema not found. |
| 422 | `size` exceeds `max_size`, `mime` not in allowlist, filename empty, or the field is not a `file` type. |
| 422 `hook_aborted` | `before_upload` hook returned an abort. |
| 500 | Backend bucket not configured, presign failed. |

### 4.2 PUT to the presigned URL

```
PUT <upload_url>
Content-Type: application/pdf
<bytes>
```

The response comes straight from the object store; SchemaForge is not
involved. A successful PUT returns 200 with an ETag header. The client should
retain the key returned in step 4.1 and pass it to confirm — it is not
derivable client-side.

### 4.3 POST confirm-upload

`POST /api/v1/forge/schemas/{schema}/entities/{id}/fields/{field}/confirm-upload`

Verifies bytes landed and writes the `FileAttachment` to the entity field.
Requires `Write` access on the schema.

Request:
```json
{
  "key": "tenant_abc/Deal/entity_01J.../contract/019200.../proposal.pdf",
  "checksum_sha256": "abc123..."
}
```

Response:
```json
{
  "status": "scanning",
  "attachment": {
    "key": "tenant_abc/Deal/entity_01J.../contract/019200.../proposal.pdf",
    "size": 1048576,
    "mime": "application/pdf",
    "status": "scanning",
    "uploaded_at": "2026-04-16T17:01:24Z",
    "created_at": "2026-04-16T17:01:24Z",
    "checksum": "abc123..."
  }
}
```

The runtime:
1. Rejects keys that don't match the expected prefix for
   `(tenant, schema, entity_id, field)` — prevents cross-entity key injection.
2. Calls `HeadObject(key)` against the configured bucket.
3. Verifies the observed size ≤ `max_size` and the content-type (if reported)
   matches the MIME allowlist.
4. Writes a `FileAttachment` with `status = scanning` if the schema declares
   an `@hook(on_scan_complete)`, or `status = available` otherwise.
5. Fires `after_upload` (detached) with a freshly minted presigned GET URL
   for the scanner to read bytes.

Errors:

| Status | Reason |
|---|---|
| 422 | Key prefix mismatch, no object at the key, size/mime re-check failed. |
| 502 | `HeadObject` failed (bucket unreachable, credentials wrong). |

### 4.4 GET download

`GET /api/v1/forge/schemas/{schema}/entities/{id}/fields/{field}`

Serves the file. Requires `Read` access on the schema. Refuses with 422
unless `attachment.status == "available"`.

**Presigned mode** (`access: "presigned"`):

- `GET ...?redirect=true` (default) — returns `302 Found` to a short-TTL
  presigned S3 GET URL. The browser follows it transparently.
- `GET ...?redirect=false` — returns `200 OK` with JSON
  `{ "url": "...", "key": "..." }`. Used by SPAs that want to render an
  `<a href>` without following it.

**Proxied mode** (`access: "proxied"`):

- Opens a `GetObject` stream.
- Forwards `Content-Type` and `Content-Length` headers from the backend.
- Pipes bytes through to the response body.
- Supports `Range` header passthrough.
- Re-checks authz on every request (no cached decisions, unlike the
  presigned TTL window).

---

## 5. State Machine and Scanning

### 5.1 States and transitions

Every attachment lives in one of six states:

```
         ┌─────────┐     confirm-upload      ┌──────────┐
         │ pending │ ─────────────────────▶  │ uploaded │
         └─────────┘                         └──────────┘
                                                   │
                              (if scan hook exists)│ (else)
                                                   ▼
                                           ┌──────────┐
                                           │ scanning │
                                           └──────────┘
                                                 │
                     ┌───────────────────────────┼───────────────────────────┐
                     │                           │                           │
                     ▼                           ▼                           ▼
               ┌───────────┐             ┌─────────────┐             ┌──────────┐
               │ available │             │ quarantined │             │ rejected │
               └───────────┘             └─────────────┘             └──────────┘
```

| State | Meaning | Downloadable? |
|---|---|---|
| `pending` | Upload URL has been minted; bytes not yet confirmed. | No |
| `uploaded` | Confirm succeeded; scan dispatch pending. Transient. | No |
| `scanning` | In scanner's queue; awaiting `on_scan_complete`. | No |
| `available` | Scan cleared (or no scanner configured). | **Yes** |
| `quarantined` | Scanner rejected the file; bytes retained for forensics. | No |
| `rejected` | Validation / scan reported a terminal failure before bytes landed. | No |

`available` is the only terminal state from which downloads succeed. The
runtime always returns `409 Conflict` for a download request against any
other state (with a JSON body identifying the current state).

### 5.2 Scan integration via hooks

The runtime does not ship with a scanner. Scanning is your responsibility,
delegated through the hook system:

1. Declare `@hook(on_scan_complete)` on the schema that owns the file field.
2. Implement the hook handler in your gRPC service. The runtime passes
   `object_key`, `mime_type`, `file_size`, and a short-TTL `download_url`
   signed for the exact key — the handler uses the URL to stream bytes
   directly from S3, runs whatever inspection (ClamAV, VirusTotal, govcloud
   AV, OCR, magic-byte check) is appropriate for your deployment, and decides
   the terminal state.
3. Post the verdict back to the runtime via `POST /scan-complete`
   (Section 5.3).

Because hooks are gRPC services you already own, you choose the scanner.
SchemaForge just provides the lifecycle hooks to plug it in.

If **no** `on_scan_complete` hook is declared on the schema, the runtime
transitions `scanning → available` automatically at confirm time. This is
appropriate for dev / low-risk deployments; for any environment that needs
virus scanning, always declare the hook.

### 5.3 scan-complete callback

`POST /api/v1/forge/schemas/{schema}/entities/{id}/fields/{field}/scan-complete`

The scanner service calls this when it has a verdict. **Requires the
`platform_admin` role** — in practice the scanner runs under a service
account whose token carries `platform_admin`. The role is intentionally
distinct from any in-app `"admin"` you might use in `@access(...)` so
applications can carve out an "admin" tier without granting the scanner
service platform-wide privileges.

Request:
```json
{
  "status": "available",
  "reason": null
}
```
or
```json
{
  "status": "quarantined",
  "reason": "clamav.eicar-test-signature"
}
```

Response: the full `AttachmentResponse` with the new status.

The runtime:
1. Validates the caller has `platform_admin` role (403 otherwise).
2. Validates the new status is either `"available"` or `"quarantined"`.
3. Validates the current attachment is in state `scanning` (409 otherwise).
4. Persists the transition.
5. Fires `on_scan_complete` hook.

---

## 6. File-Specific Hook Events

Three new events complement the entity-level ones. Adding a `@hook(...)`
annotation for any of these opts the schema in:

| Event (DSL) | Fires when | Blocking? | May abort? |
|---|---|---|---|
| `before_upload` | `POST /upload-url` before the presigned URL is minted | yes | yes (returns 422) |
| `after_upload` | `POST /confirm-upload` after `HeadObject` succeeds | no (detached) | no |
| `on_scan_complete` | Synchronously during `POST /scan-complete` | no (detached) | no |

The proto generator (`schema-forge hooks generate`) emits per-event request
messages with file-specific fields instead of the entity-scalar fields it
uses for regular CRUD hooks:

**`{Entity}BeforeUploadRequest`**:
| Field | Type | Meaning |
|---|---|---|
| `operation` | `string` | Always `"mint_upload_url"`. |
| `user_id` | `optional string` | Authenticated subject. |
| `entity_id` | `optional string` | Entity receiving the attachment. |
| `field_name` | `string` (tag 100) | Name of the file field. |
| `file_name` | `string` (tag 101) | Client-declared filename. |
| `mime_type` | `string` (tag 102) | Client-declared MIME. |
| `file_size` | `int64` (tag 103) | Client-declared size in bytes. |

**`{Entity}AfterUploadRequest`** / **`{Entity}OnScanCompleteRequest`**:
| Field | Type | Meaning |
|---|---|---|
| `operation` | `string` | `"confirm_upload"` or `"scan_complete"`. |
| `user_id` | `optional string` | Authenticated subject. |
| `entity_id` | `optional string` | Entity the attachment belongs to. |
| `field_name` | `string` | Name of the file field. |
| `object_key` | `string` | Bucket-relative object key. |
| `mime_type` | `string` | Observed Content-Type (or DSL fallback). |
| `file_size` | `int64` | Observed bytes from `HeadObject`. |
| `status` | `string` | New lifecycle state. |
| `download_url` | `optional string` | Short-TTL presigned GET (AfterUpload / OnScanComplete). |

**Response messages** carry only `abort_reason` (string, tag 1) and an
optional `advisory_status` (string, tag 100) for logging. File events do not
modify the attachment via their response — quarantining is done through the
`scan-complete` REST callback, not via `on_scan_complete`'s response body.

Use `before_upload` to:
- Enforce a tighter policy than the DSL expresses (per-tenant quota, per-user
  file-count limit, per-department MIME restriction).
- Tag the upload with additional metadata your pipeline needs.
- Reject uploads during maintenance windows.

Use `after_upload` to:
- Run your scanner against the `download_url`.
- Generate thumbnails / extract OCR text / compute PII classification.
- Push a notification to NATS / SQS / Pub-Sub.

Use `on_scan_complete` to:
- Record terminal scan outcomes in audit logs.
- Notify stakeholders when a file is quarantined.
- Trigger downstream workflows (approval, publish, archive).

---

## 7. Database Storage and Migrations

A `file` field is stored as a single column:

| Backend | Column type | Null policy | Structural check |
|---|---|---|---|
| PostgreSQL | `JSONB` | nullable unless `required` | `jsonb_typeof(col) = 'object' AND col ? 'status' AND col ? 'key'` |
| SurrealDB | `object` (FLEXIBLE) | nullable unless `required` | schema-level FLEXIBLE |

The JSON shape matches the `FileAttachment` struct:

```json
{
  "key": "tenant_abc/Deal/entity_01J.../contract/.../proposal.pdf",
  "size": 1048576,
  "mime": "application/pdf",
  "status": "available",
  "created_at": "2026-04-16T17:01:00Z",
  "uploaded_at": "2026-04-16T17:01:24Z",
  "checksum": "abc123..."
}
```

Runtime enforces MIME / size / access; the database only enforces structural
validity via the CHECK constraint (Postgres) or FLEXIBLE schema (Surreal).
**Changing `max_size`, `mime`, or `access` in the DSL does not require a
migration** — those are runtime policy, not storage shape.

Adding a file field to an existing schema generates the standard
`MigrationStep::AddField` plan:

```
ALTER TABLE "Deal"
  ADD COLUMN IF NOT EXISTS "contract" JSONB
  CONSTRAINT "chk_Deal_contract_file"
    CHECK ("contract" IS NULL OR (jsonb_typeof("contract") = 'object'
      AND "contract" ? 'status' AND "contract" ? 'key'));
```

GraphQL mapping: file fields serialize as the `JSON` scalar in the generated
GraphQL schema, so clients read `{ key, size, mime, status, uploaded_at }`
via the standard entity query. Writes still go through the REST upload flow
— GraphQL mutations do not carry file bytes.

Query API: `file` fields are not filterable or sortable in v1. They are
eligible for field projection via the `fields` parameter (include the file
field name to get the attachment metadata back in list responses).

---

## 8. Operations

### 8.1 Startup validation

At boot, `SchemaForgeExtension::build_init` validates every schema's file
fields against the configured backends. A mismatch fails startup with a
clear error:

```
file field(s) reference undeclared storage backends:
[Deal::contract -> bucket "documents", Evidence::scan -> bucket "evidence"].
Add matching [schema_forge.storage.backends.<name>] entries.
```

This is deliberate — a misconfigured deployment that starts and then fails
the first upload is much worse than one that refuses to start at all. Fix
the config and retry.

A running server logs the backend count at info level:

```
INFO backends=2 storage registry initialized
```

### 8.2 Bucket layout and object keys

The runtime generates keys deterministically:

```
{tenant}/{schema}/{entity_id}/{field}/{uuid_v7}/{sanitized_filename}
```

- `tenant` — value of the entity's `_tenant` field if the schema is
  multi-tenant; `_shared` otherwise.
- `schema` — PascalCase schema name.
- `entity_id` — TypeID of the entity (e.g. `entity_01JXABC...`).
- `field` — snake_case field name.
- `uuid_v7` — monotonic UUID v7 to keep keys sortable by creation time and
  collision-free across concurrent uploads to the same field.
- `sanitized_filename` — the client-provided filename with unsafe characters
  replaced by `_`. Length capped at 255.

Design this layout into your bucket policies:

- **Retention policies** can target `{tenant}/{schema}/*` prefixes for
  per-tenant cleanup on offboarding.
- **Lifecycle rules** can move older objects to cheaper storage tiers
  based on the UUID v7 timestamp embedded in the path segment.
- **S3 inventory** scans naturally group by tenant then schema.

Quarantined objects are not moved by the runtime in v1 — they remain under
their original key, but the attachment's `status = "quarantined"` keeps them
undownloadable. For cold-path forensic access, apply an S3 lifecycle rule
that moves objects matching `quarantine/**` (if your scanner copies them
there) to a Glacier tier.

### 8.3 Failure modes

| Failure | Result |
|---|---|
| Bucket unreachable at mint | 500 `presign failed`. Check credentials and endpoint. |
| Client sends wrong `Content-Type` on PUT | S3 rejects with 403 (SignatureDoesNotMatch). Client must use the `headers` from the mint response verbatim. |
| Client never calls confirm | Object stays in bucket under `pending`-phase key. No entity row updated. **No runtime sweep in v1** — apply an S3 lifecycle rule to auto-expire unreferenced uploads after 24h. |
| `HeadObject` returns 404 at confirm | 422 `no object at key — upload did not complete`. |
| Observed size > `max_size` at confirm | 422 `uploaded size N exceeds max_size M`. The object stays in the bucket (no automatic cleanup); apply a lifecycle rule or write an admin script. |
| Observed content-type not in allowlist | 422 `uploaded content-type X not in allowlist`. |
| Presigned URL expires before client PUTs | S3 rejects with 403. Client mints a fresh URL. |
| Scanner never calls back | Attachment stays in `scanning` forever. Downloads blocked. Write an operational alert on `status='scanning' AND now() - uploaded_at > 1h`. |
| Runtime crashes mid-confirm | Object exists, no entity row. Same recovery as "never called confirm" — client retries or lifecycle rule cleans up. |
| Schema `bucket:` name typo | Startup validation fails with a clear message before the server accepts requests. |

---

## 9. Limitations and v1 Scope

**Deferred to v1.1:**

- `file[]` multi-file fields. Today a file field is one-file-per-row. Storing
  multiple attachments requires either a sidecar schema (e.g. `Document`
  entities with `owner: -> Deal`) or waiting for v1.1 which will add a
  dedicated `file_attachments` sidecar table.
- **Magic-byte MIME verification** inside the runtime. v1 trusts
  client-asserted MIME at mint time + S3's `Content-Type` condition + hook
  callback. Magic-byte sniffing is delegated to `after_upload` hooks.
- **Background reconciler** to sweep stale `pending` attachments. Use S3
  lifecycle rules for orphaned-object cleanup today.
- **S3-event-driven confirmation** (SNS/SQS callback). v1 is strictly
  client-driven: the client must call `confirm-upload` after the PUT.
- **Multipart / resumable uploads** for objects > 100 MB. v1 is single-part.
- **Filterable / sortable file fields** in queries. Today they're projected
  only.

**By design:**

- **S3-compatible only.** No local-filesystem backend. This keeps the code
  path simple and production-realistic; run MinIO in dev for parity.
- **Uploads always go direct.** The runtime will never proxy upload bytes —
  this is a load-bearing invariant for FedRAMP boundary design.
- **State machine is centralized.** Attachments transition through well-known
  states and downloads gate on `available`. Alternative schemes (e.g.
  transient access tokens) are out of scope.
- **Scanning is plug-in, not built-in.** The runtime never ships with an
  AV engine — enterprise deployments have strict opinions about which
  scanner to use and how it is operated. Hooks keep that decision external.
