# Audit browsing and verification implementation plan

Expose the existing acton-service audit store through three authenticated HTTP routes under `/api/v1/forge/audit`: status, events, and verification. Reuse the upstream storage handle and sequence-based query contract. Never create a second store or call the unbounded verifier from an HTTP handler.

Access is deployment-wide, reserved for `platform_admin` by a dedicated audit gate; tenant-scoped administrators and anonymous requests are denied before storage access. Publish audit-read and audit-verify permission flags. Schema Cedar grants cannot expand this deployment-level privilege.

Status describes configuration, active logger/storage availability, retained sequence bounds and retention settings, plus collection/persistence limitations. Event DTOs explicitly project identifiers, sequence, timestamp, kind, severity, service and chain hashes, excluding metadata and request/source details. Pagination accepts only an exclusive sequence cursor, fixed inclusive snapshot upper bound and bounded limit, in ascending sequence order. Unknown filters are rejected. Missing snapshot data produces a conflict rather than silently skipping retained-away events.

Verification accepts an inclusive nonzero sequence range capped at 1,000 requested events plus one predecessor. Reuse the upstream bounded verifier and represent valid, broken, incomplete and unavailable results explicitly with observation time, requested/checked range and stored-anchor information. All storage work has a five-second request deadline. No result claims independently trusted history or acknowledged persistence for application writes.

Keep request validation and DTO projection pure. New audit-specific enums and request types model the contract; existing upstream event IDs and sequence numbers retain their defined meanings. Use existing ForgeError for HTTP errors. Add disposable synthetic-storage HTTP tests for access, concurrent appends, retention, gaps, invalid limits, secret projection, valid full/suffix ranges, corruption and unavailable storage. Test actual backend adapters upstream.

Bump SchemaForge CLI to 0.42.0 for additive API functionality and the integration crate to 0.41.0. Upgrade acton-service through cargo add after its required additive API release. Run formatting, nextest, all supported backend Clippy configurations and required CI. Sign Conventional Commits and tag using established vX.Y.Z convention. Verify all six released archives and signed manifests, then smoke-test downloaded Linux PostgreSQL and SurrealDB binaries.

Upstream 0.42.0 replaces audit UUIDs with typed `mti` IDs. Assert that the HTTP projection emits canonical `audit_` IDs with UUIDv7 payloads for new events. Historical UUID conversion preserves original bits and hashes; upstream regressions cover legacy serialization and storage.
