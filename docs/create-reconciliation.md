# Create reconciliation v1

This opt-in PostgreSQL protocol binds one submitted create request to a server-reserved intent. The entity, its initial revision, and the committed receipt share one database transaction. Ordinary entity creation remains available without an intent.

Only authenticated requests to hook-free schemas are supported initially. Configured schema hooks or enabled global webhooks refuse reservation and pending commitment. Receipt recovery remains available when hooks are subsequently configured. This protocol does not guarantee delivery or exactly-once execution of external effects.

## Requests and responses

All paths below are relative to the existing SchemaForge API mount. Use the same authentication and selected `X-Active-Tenant` as ordinary entity requests. The server binds the intent to the normalized principal, selected tenant, and registered schema identity. Changing any part of this scope cannot recover or commit the intent. Current authorization is evaluated on every request.

1. `POST /schemas/{schema}/create-intents` with the ordinary `{ "fields": { ... } }` entity body reserves an intent without inserting an entity. A successful response is HTTP 201 with a receipt:

   ```json
   {
     "id": "createintent_<opaque identifier>",
     "state": "pending",
     "expires_at": "2026-01-01T00:15:00Z",
     "recover_until": "2026-01-02T00:00:00Z",
     "entity_id": null
   }
   ```

2. `POST /schemas/{schema}/entities` with the same body and exactly one `Create-Intent` header commits or reconciles the intent. Treat the identifier as opaque: send the exact returned value, without quotes, whitespace, lists, or additional header instances. Malformed identifiers are HTTP 400. Successful first commitment is HTTP 201; successful reconciliation is HTTP 200. Both return the ordinary entity response and `Create-Intent` response header. The returned entity is its currently authorized representation, including subsequent changes, not a historical response. `Entity-Revision` is present when revision reads have been prepared for the schema.

3. `GET /schemas/{schema}/create-intents/{id}` returns HTTP 200 and the same receipt structure. States are `pending` with a null entity ID, `committed` with the original currently readable entity ID, or `committed_unavailable` with a null entity ID when the committed record has been deleted. The latter is an outcome, not a successful usable entity. Fetch a committed entity normally to obtain current data and its revision.

The `fields` fingerprint ignores object-key order recursively. Array order, omitted fields versus explicit null, and JSON number representations remain significant. Server defaults, computed fields, owner injection, and tenant injection are not part of the submitted fingerprint. Unknown or hidden input fields are rejected. Changed submitted content requires a separate explicit create decision; it cannot reuse an intent.

## Deadlines and uncertain responses

V1 reserves a 900-second admission window and an 86,400-second recovery window, both measured from server reservation time. The returned timestamps are authoritative. Initial commitment must be admitted while holding the receipt lock strictly before `expires_at`. Lock waits are followed by a fresh server-time check. A transaction admitted before expiry can complete after it. A committed receipt can be recovered strictly before `recover_until`. Expired pending intents, expired recovery windows, unknown IDs, pruned receipts, and differently scoped IDs are refused. Absence never authorizes insertion.

A pending receipt is a snapshot; an in-flight commit can finish after the read. Retry the same body with the same intent rather than reserving another. A connection failure, timeout, or HTTP 503 can occur after commitment, so it never proves that no record was created. Failure of the current result read can likewise follow a successful commitment. Reconcile the same intent. If the recovery window expires without a known outcome, stop automatic retries and require an explicit operator or user decision; never silently reserve a replacement.

Validation and business uniqueness failures do not commit an entity and leave the intent pending until its admission deadline. They are not terminal cached outcomes. A later identical request can succeed if the relevant external state changes. Altering the body produces a content conflict. Schema-definition changes refuse an uncommitted intent. Completed receipts retain the original record identity through renames and deletion and never recreate a deleted record.

## Errors and authorization

Protocol conflicts use the existing HTTP 409 envelope with `error: "conflict"` and a `reason`:

| Reason | Meaning |
| --- | --- |
| `create_intent_unsupported` | Backend or configured side-effect path does not support this protocol. |
| `create_intent_unavailable` | Unknown, expired, pruned, or differently scoped intent. No replacement create is attempted. |
| `create_intent_content_conflict` | Submitted fields differ from the reserved input. |
| `create_intent_schema_changed` | An uncommitted reservation's schema definition is no longer current. |
| `create_result_unavailable` | Commitment exists, but POST reconciliation cannot return the deleted result. It was not recreated. |

Normal validation errors, authentication errors, authorization denials, and business `unique_violation` errors retain their existing envelopes and statuses. HTTP 400 malformed intent errors use `error: "invalid_query"`. HTTP 503 uses the existing backend-unavailable envelope. Do not classify an outcome from human-readable messages.

Reservation, commitment, and receipt lookup require current create permission. Returning a committed result additionally uses ordinary current record and field read authorization. Denied reads return their authorization error, never `committed` or `committed_unavailable` success. A former owner or tenant member cannot replay historical authorization. Receipt storage contains a content fingerprint and schema snapshot, not an entity response or credentials. Live-name collision rules remain schema/business constraints independent of receipt identity.

## Operational limits

Receipts are stored in a reserved internal PostgreSQL table. Expired receipts are removed in bounded batches on reservation. This contract assumes SchemaForge transactional writers and does not cover direct SQL changes to internal tables, restoring partial database state, or external hook effects. Revisions use the existing record-revision infrastructure. Capability metadata advertises `create_reconciliation.protocol = "create-intent-v1"`, PostgreSQL availability, the request header, and the two v1 windows; individual schema support still depends on its hooks and the deployment configuration.
