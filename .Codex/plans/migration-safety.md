# Migration safety (#167, #174, #175, #176)

1. Centralize validation of tenant transitions and rename hints in DiffEngine. Reject tenant annotation transitions before any DDL or metadata write, requiring a manual migration/backfill and matching metadata. Preserve backend integrity until automatic backfill is available.
2. Add typed FieldAnnotation::RenamedFrom with DSL parsing and display, retained in signed serialized schemas. Collect active hints centrally, validate missing/ambiguous/conflicting sources, and retain no-op hints on repeat apply.
3. Preflight serve migrations before executing user schema changes; require explicit allow-destructive-migrations opt-in. Gate runtime schema PUT with explicit body opt-in and preserve existing schema annotations.
4. Reconcile PostgreSQL relation foreign keys consistently at schema persistence/startup checkpoints, after tables exist, using explicit restrictive delete behavior and idempotent checks. Validate existing data and report failures. Translate 23503 into typed conflict errors.
5. Reject empty PostgreSQL UPDATE fields before emitting SQL.

Tests: DSL roundtrip and invalid rename hints, repeat apply, tenant add/remove/root-parent refusal, startup/runtime destructive refusals, PostgreSQL fresh/add/backfill/cyclic FK behavior and deletion/write error mapping; nextest and clippy, cross-backend compilation. No dependencies required. Public annotation and error variants warrant minor release; root owns version bump.
