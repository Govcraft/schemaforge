# Clearing file attachments

`DELETE /api/v1/forge/schemas/{schema}/entities/{id}/fields/{field}` clears an optional file attachment. A concurrent attachment replacement returns `409` with reason `attachment_changed`, leaving the replacement intact. The comparison and field clear are atomic and preserve unrelated edits. A successful request returns `204 No Content`; clearing an already empty field also returns 204. Required file fields return 422 and remain unchanged.

The caller must authenticate and satisfy schema update, record, selected-tenant, and field write authorization. Clearing supports every attachment state, including scanning and quarantined files. It changes only the attachment field and retains the object bytes. Storage retention and bucket lifecycle rules determine how long those bytes remain. Previously issued presigned URLs remain usable until expiry or object removal.

The runtime emits `forge.file.detached` through its configured durable audit logger after a successful detachment. Metadata includes the caller, schema, entity, field, bucket alias, object key, previous status, and `object_disposition: retained`. An already empty field does not emit a second detachment event. No new hook event is introduced; the audit event is the lifecycle notification for this operation.

Use the CLI with the same connection and credential options as other entity commands:

```sh
schemaforge entity file clear Document document_… attachment --dry-run
schemaforge entity file clear Document document_… attachment --yes
```

Interactive use prompts for confirmation. Scripts must supply `--yes`. `--format json` returns the cleared target and retention disposition. Use this route rather than a generic entity patch so that the object key remains traceable in the file lifecycle audit trail.

Custom backend implementations must implement `EntityStore::update_field_if_matches`; its default rejects the operation rather than falling back to an unsafe read-modify-write.

Upload confirmation and scan completion use the same comparison, preventing an in-flight update based on an old attachment from restoring it after a clear. Clearing an empty field does not cancel an upload that has not yet been attached.

In Cedar resource attributes, a populated file field is represented by its object key string, matching the generated Cedar schema. Required file attachments therefore remain valid authorization resources before a clear is rejected.
