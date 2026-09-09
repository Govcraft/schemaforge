# Data correctness issues 140 through 142

Preserve enum data during widening and reorder migrations with Identity; represent removed variants explicitly in a new non-exhaustive ValueTransform variant and null only those values before new constraints are installed. Widening is safe, narrowing requires confirmation. Exercise planner and PostgreSQL/SurrealDB generated migrations plus storage behavior where available.

Define null equality as absent or explicit null; inequality is its complement, including non-null comparisons. Compile PostgreSQL null predicates without binds and use IS DISTINCT FROM for non-null inequality; align SurrealDB and MSSQL missing fields. Test filtering and counts together using reusable backend test cases.

Make entity query ordering total by appending id ascending unless explicitly sorted by id. Preserve descending id requests, accept direction as the HTTP alias for order, and validate reserved id filter paths. Fix MSSQL id extraction and SurrealDB record-id literals. Test stable pages, duplicate sort values, descending order and id ranges.

Keep existing typed IDs/errors and pure query/codegen helpers. No dependencies required. Additive transform IR suggests core minor version; parent coordinates all version changes. Run focused nextest and deny-warning Clippy, then parent integration gates for all supported backends.

# Attachment clear concurrency support (issue 120 review)

Add default-unsupported `EntityStore::update_field_if_matches(schema, id, FieldName, expected DynamicValue, value DynamicValue) -> Result<bool, BackendError>` and dynamic bridge. Backend implementations atomically compare the current field and update just it, returning false for a missing/stale attachment. PostgreSQL UPDATE RETURNING participates in the existing revision transaction; SurrealDB uses a conditional UPDATE; SQL Server uses JSON_MODIFY with a bound exact JSON comparison. Shared tests verify stale snapshots do not detach replacements, concurrent clears have one winner, unrelated updates survive and null/missing results are safe. Parent owns actor message, HTTP route and audit handling.
