# Issue 159: preserve exact authorized totals without redundant scans

## Contract and design

Default list totals remain exact and available. Read access, page offsets, filters,
and field projection retain their existing meanings. No approximate or partial
count response is introduced.

Capture one immutable Cedar policy snapshot per query. Prepare principal and
action state once, and evaluate concrete rows against that snapshot. Track the
actor's explicitly supplied record policy separately from its default Cedar
adapter, so list queries run supplied policies and Cedar once each.

For generated record-independent Read policies, compare actual applicable policy
ASTs with the generated policy ASTs. Reject custom applicable predicates, action
hierarchies, and incompatible schema shapes. Ask storage for a certified exact
page/count using `query_cedar_compatible(expected_schema, query, scope)`. The
capability defaults to unsupported for external and non-PostgreSQL backends.
PostgreSQL certifies physical columns, decoded values and schema identity under a
transaction before applying equivalent tenant membership predicates. A failed
proof falls back to the authorized stream; I/O failures remain errors.

The scan keeps bounded candidate batches and exact totals. Arbitrary custom
record policies can still require O(matching rows) work. No cross-batch database
snapshot guarantee is added to the fallback path.

## Ownership

The PostgreSQL worker owns backend auth/trait capability, PostgreSQL implementation,
tests and package patch versions. This agent owns acton engine, policy proof,
actor/message/state integration, routes, tests, CLI/acton versions and public docs.
The parent independently measures realistic PostgreSQL fixtures and reviews the PR.

## Validation and release

Focused nextest regressions cover generated policies, custom UID/context/tenant
predicates, opaque operator policies, schema compatibility, sparse authorized
pages, projection and captured snapshots. PostgreSQL tests prove physical shape
and malformed-row rejection. Run affected all-target clippy with warnings denied.
Independent acceptance measures large ordinary and custom-filtered collections.

Patch CLI 0.44.1 and acton 0.43.1 fix a performance regression without changing
response semantics. No dependency changes are planned. Commit signed Conventional
Commit and open one PR including release preparation. Parent owns merge/release.

## Validation results

- Focused acton proof/context/pagination run: 17 passed.
- Additional real PostgreSQL HTTP regression passed with exact totals and field
  projection, including a malformed required value outside the requested page.
- Strengthened policy-AST multiset regression passed with a duplicate permit
  replacing the tenant guard.
- Affected acton/CLI all-target clippy passed with warnings denied. The added
  HTTP test target separately passed clippy after its addition.
- Default PostgreSQL count certification initially exceeded the actor reply
  deadline on a rich 202,628-row fixture. Measurement isolated identifier and
  JSON regular expressions; equivalent conservative SQL checks reduce the
  combined proof to approximately 0.7 seconds. Final HTTP acceptance passed all
  nine cases: simple GET/POST exact totals in 0.58 to 0.60 seconds, rich default
  totals in 0.82 seconds, and filtered/deep/past-end/projected pages in 0.82 to
  0.91 seconds. Restricted fields stayed hidden and relation displays remained
  correct. Page-only rich records returned in 0.12 seconds. Both collections
  contained 202,628 synthetic records. The v0.44.0 release binary baseline
  returned HTTP 408 after 30.02 seconds on the simple default count.
- Custom predicate scans retain exact semantics: a 20,000-row debug-build query
  returned exactly 20 readable rows in its total, while a full 202,628-row
  custom scan reached the 30-second request deadline. No approximation or
  timeout extension is introduced. Release-build performance is not inferred
  from these debug measurements.
