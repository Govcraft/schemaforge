# Issue 252: certify custom field policies per request

## Root cause and reproduction
The field query proof requires exact equality with generated policy ASTs, using only action/resource heads to select policies. A blanket custom forbid therefore blocks every protected query field before evaluating whether its principal or condition can match. Reproduce both reported policies through HTTP regressions on the unmodified implementation before changing production code.

## Design
Keep generated Read storage-scope certification unchanged. Replace the private field-read proof with a per-request conservative proof, using the same policy snapshot and principal adapters as normal Cedar authorization. Ignore a policy only when its principal head provably cannot match, including exact equality, group membership, type, and type-plus-membership. Use Cedar's Entities membership semantics, not string comparisons of role names.

For policies that can match principal/action/schema, accept only record-invariant resource heads (Any or the current schema type), and conditions expressible with a conservative EST whitelist of principal/action variables, literals, Boolean/comparison/arithmetic/set operations, and direct principal attribute reads. Reject resource/context variables anywhere in nested conditions; reject direct entity attribute/tag access, dynamic/nested attribute access, slots, unsupported operators/extensions, and record-specific resource equality/membership heads. Unknown forms fail closed. Evaluate the entire original policy set with Cedar afterward; independence never implies permission, and matched constant forbids and evaluation errors continue to deny.

Reuse existing domain types and error types; no new dependencies or public API. Retain concrete Cedar resource-shape compatibility proof and inherited-action rejection. REST, GraphQL, and exports already use the shared field-query boundary and need no route changes.

## Files and functions
- authz/read_scope.rs: field_read_is_record_independent(snapshot, schema, field, claims), principal head matching helper, pure EST condition proof. Preserve generated Read exact-AST proof.
- authz/engine.rs: pass actual claims to the private field proof and retain same-snapshot decision.
- tests/query_field_authorization.rs: reproduce both policies, positive/negative principal cases, body filter/sort, query-path denial, and export execution behavior.
- tests/graphql_query_field_authorization.rs: shared-boundary positive/negative policy regressions.
- docs/custom-policy-context.md: state the actual supported proof and remaining intentional fail-closed cases.

## Tests and validation
First run new HTTP regression against existing production code and record expected 403 instead of 200. Then implement and run focused HTTP/GraphQL/unit nextest suites, followed by package clippy with warnings denied. Include nested resource/context dependencies, entity-literal attribute access, matching principal forbids, actual principal claims, custom principal-only permits, unrelated policies, and stale concrete shape regression. Root owns broader database validation and release.

## Semver
Patch: restore documented safe custom-policy behavior through private helpers; preserve public contracts and record confidentiality.
