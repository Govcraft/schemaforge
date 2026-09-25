# Migration safety (#167, #174, #175, #176)

1. Centralize validation of tenant transitions and rename hints in DiffEngine. Reject tenant annotation transitions before any DDL or metadata write, requiring a manual migration/backfill and matching metadata. Preserve backend integrity until automatic backfill is available.
2. Add typed FieldAnnotation::RenamedFrom with DSL parsing and display, retained in signed serialized schemas. Collect active hints centrally, validate missing/ambiguous/conflicting sources, and retain no-op hints on repeat apply.
3. Preflight serve migrations before executing user schema changes; require explicit allow-destructive-migrations opt-in. Gate runtime schema PUT with explicit body opt-in and preserve existing schema annotations.
4. Reconcile PostgreSQL relation foreign keys consistently at schema persistence/startup checkpoints, after tables exist, using explicit restrictive delete behavior and idempotent checks. Validate existing data and report failures. Translate 23503 into typed conflict errors.
5. Reject empty PostgreSQL UPDATE fields before emitting SQL.

Tests: DSL roundtrip and invalid rename hints, repeat apply, tenant add/remove/root-parent refusal, startup/runtime destructive refusals, PostgreSQL fresh/add/backfill/cyclic FK behavior and deletion/write error mapping; nextest and clippy, cross-backend compilation. No dependencies required. Public annotation and error variants warrant minor release; root owns version bump.

## Atomic runtime schema persistence followup

Add apply_schema_change(name, steps, optional definition) to SchemaBackend and dynamic adapter with unsupported default. Each supported backend executes schema DDL, required validation, metadata upsert/delete, and integrity reconciliation in a single transaction. Extract existing statement/connection helpers to preserve rename behavior. PostgreSQL uses transaction-local metadata and strict FK finalization; cache invalidates only after commit. Runtime DELETE keeps its existing registry-only semantics (parent decision). Add focused rollback contracts exercising failure after DDL and metadata-only/deletion paths, run focused nextest and Clippy only; CI owns broad gates.

## SurrealDB transactional rename visibility

Measured SELECT snapshots show REMOVE FIELD followed by UPDATE in the same transaction discards unrelated FLEXIBLE object data. Individual statements in separate transactions do not. Preserve one transaction: copy to fully defined destination, temporarily relax source required/default constraints, unset old values while source definition still exists, defer only rename-source definition removals until all data writes finish. Regression must retain unrelated nested data and verify a later write still works, besides renamed nested data and metadata rollback.

## SQL Server sparse update parity

CI exposed that SQL Server replaces its entire JSON payload on EntityStore::update, unlike PostgreSQL/SurrealDB and the runtime's field-filtered partial update path. Implement one bound JSON_MODIFY UPDATE with OUTPUT inserted row, preserving omitted values and explicit tagged nulls without a read/modify/write race. Clarify trait semantics, retain rename regression, and add SQL Server direct null/empty-update checks. Run focused SQL Server integration tests only.

## Supported SurrealDB engine correction

Reproduced simultaneous successful attachment clears on the 2.6 Mem engine even with explicit transactions. SurrealKV0.9.3 advances commit oracle before index publication; snapshots can mix old data with the newer conflict watermark. Latest2.6.4 uses identical engine; no compatible SurrealKV0.9 patch exists. Upgrade the backend SDK to stable3.3.0, whose Mem engine uses SurrealMX0.27 completed-commit watermark. Adapt native value/error APIs without changing entity semantics, add multi-thread competing clear/replace regression, run focused CAS and migration tests. Root approved supported upgrade over local mutex or vendored engine fork.
