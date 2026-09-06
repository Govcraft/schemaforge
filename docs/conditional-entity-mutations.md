# Conditional entity mutations

SchemaForge can reject an edit or deletion when the record has changed since the caller read it. The first implementation supports PostgreSQL. SurrealDB, MSSQL and third-party adapters retain ordinary CRUD and explicitly reject conditional mutations until they implement the optional backend contract.

This is record concurrency control. It does not create a revision history, merge competing edits, deduplicate creation, or make schema and authorization rollouts transactional with in-flight requests.

## Enable a PostgreSQL schema

Upgrade every writing SchemaForge instance first. Then explicitly prepare the selected schemas:

```sh
schemaforge apply --prepare-record-revisions ./schemas/
```

The command applies normal schema changes and backfills internal record revisions, including when the supplied schema is unchanged. The operation takes a table write lock while preparing each table. Schedule preparation appropriately for large or busy datasets. `--dry-run` reports the preparation without performing the backfill. Unsupported backends reject the flag before applying schema changes. Repeated preparation preserves existing revisions.

Revisions live in reserved internal tables, outside the entity field map. They do not become schema fields, exports, list columns or GraphQL properties. GET never creates or repairs a missing revision. A schema becomes ready only after preparation succeeds.

## Read, edit and delete

An authorized ordinary detail GET on a ready schema returns:

```http
Entity-Revision: revision_01k00000000000000000000000
Access-Control-Expose-Headers: Entity-Revision
```

The illustrated token is an opaque format example, not a usable edit condition. Keep the exact value returned by the server with that record's edit baseline. Request a fresh read without caching when beginning an edit.

Supply the token when sending PUT, PATCH or DELETE to the same entity:

```http
If-Entity-Revision: <exact Entity-Revision value from GET>
```

The server first checks the current request's schema action, record ownership/tenant boundary and supplied-field permissions. It validates the condition against that authorized record snapshot, then compares the same revision atomically when writing. A competing record change between authorization and persistence causes rejection.

Successful conditional PUT and PATCH return the ordinary entity response plus a fresh `Entity-Revision`. Accepted equal-value PATCH also advances the revision. Conditional DELETE retains the ordinary successful delete response. List responses and ordinary mutation responses do not return record revision headers; GET the detail when a new baseline is needed.

Requests without a condition retain ordinary CRUD behavior. Every actual backend write advances the marker, including unconditional writes, so an old conditional editor cannot overwrite a later unconditional edit. A conditional request never falls back to an unconditional mutation.

## Handle responses

| Response | Meaning and client action |
| --- | --- |
| Detail GET with `Entity-Revision` | This authorized record has a usable edit baseline. |
| Detail GET without the header | Conditional mutation is unavailable for that schema/backend. Do not claim stale-edit protection. |
| 409, `reason: "unique_violation"` | Another record already uses a unique value. Revise the input; the rejected write leaves the record and revision unchanged. Constraint and hidden field details are omitted. |
| 409, `reason: "revision_conflict"` | The baseline changed. Keep the user's draft and reload for deliberate reconciliation. |
| 409, `reason: "conditional_mutation_unsupported"` | The adapter or schema is not ready. Do not retry unconditionally. |
| 400 | Malformed/duplicate revision header, or unsupported `If-Match`. Correct the request. |
| 401/403/404 | Normal authentication, authorization or initially missing-record behavior. Do not substitute a conflict diagnosis. |
| Transport failure or timeout | Outcome may be uncertain. Inspect current state before deciding what to do; do not automatically replay a mutation. |

Conflict responses have the existing envelope:

```json
{"error":"conflict","reason":"revision_conflict","message":"The record changed. Reload it before trying again."}
```

They contain no current token, current field values or internal database details. A record deleted after successful baseline authorization produces the same generic conflict. Deleting and recreating the same ID does not make an earlier token valid again.

`If-Match` is intentionally unsupported for entity mutations. Record revisions are not strong HTTP representation validators: caller permissions, filtered fields, read hooks, relation expansions and derived collections can change response bytes without changing the underlying record.

Cross-origin deployments must allow `If-Entity-Revision` in their existing CORS policy. Revision responses expose `Entity-Revision`; this does not allow a new origin. The development permissive CORS setting is unchanged.

## Discover adapter support

The public meta response includes:

```json
{
  "capabilities": {
    "conditional_entity_mutations": {
      "protocol": "record-revision-v1",
      "backend_supported": true,
      "schema_readiness": "entity-revision-response-header",
      "request_header": "If-Entity-Revision",
      "response_header": "Entity-Revision"
    }
  }
}
```

`backend_supported` describes the bundled adapter, not readiness of every schema. The authorized detail response header is the per-record readiness signal. Custom hosts/adapters must advertise their own support accurately.

## Consistency boundary

PostgreSQL stores the entity and marker in one transaction. Versioned reads use a consistent snapshot; conditional mutations lock the entity row, compare its marker, and commit the row and fresh marker together. The same lock order applies to ordinary writers. Schema migrations that change stored data invalidate affected record markers in their transaction. Dynamic entity queries avoid retaining a prepared result shape across schema changes.

Request-time metadata, authorization and rules remain authoritative. A metadata-only change before a request may permit or reject that request under the new rules without changing its record revision. A concurrent policy/schema rollout after request snapshot resolution is not an atomic cutover. Related-record rule reads are not protected by a same-record comparison. Deployments requiring stronger cutovers must coordinate or quiesce writers separately.

Before hooks may run before another writer wins the race; their external side effects are not rolled back by a rejected database mutation. Successful after-change/delete hooks and mutation notifications are dispatched only after the conditional database operation succeeds.

Direct SQL writers and older SchemaForge binaries do not participate in this marker contract. Do not mix them with a deployment claiming conditional mutation protection. This feature does not make administrative SQL changes or mixed-version fleets safe automatically.

## Verification

The PostgreSQL crate includes an ignored live test that creates and drops its own temporary database namespace. Set `SCHEMAFORGE_TEST_POSTGRES_URL` to a test database whose role can create schemas, then run:

```sh
cargo nextest run -p schema-forge-postgres --test conditional --run-ignored all
```

It covers competing updates, update/delete races, duplicate deletion, unconditional-write invalidation, equal-value guarded writes, deletion/recreation, explicit readiness and schema migration. The HTTP integration suite also has a PostgreSQL test requiring a separately provisioned disposable namespace and `SCHEMAFORGE_TEST_POSTGRES_DISPOSABLE=1`; its caller must clean up that namespace. Ordinary unsupported-adapter and authorization-order tests run without an external database.
