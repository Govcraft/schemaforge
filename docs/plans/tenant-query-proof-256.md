# Issue 256: prove field predicates within enforced tenant scopes

## Confirmed flow and baseline
REST list/JSON query, GraphQL, and exports authorize caller filter/sort fields before injecting tenant filters. Injection admits the active tenant and every ancestor in the rewritten chain, filtering child `_tenant` or root `id`; administrators, disabled tenancy, empty claims/chains, and shared schemas remain unscoped. The concrete Cedar adapter derives a root's `_tenant` from its ID and gives every concrete resource empty parents. Existing custom field proof rejects any matching resource dependency, including the reported guarded tenant forbid.

Add real-server HTTP regressions for the exact two-schema example and the reported policy, with administrator and non-shared-tenant callers. Run them against unchanged production code and record the expected 403 failures before implementing.

## Design approved by root
Preserve the public `inject_tenant_scope` signature. Factor its logic into an internal trusted `QueryTenantScope`, representing Unscoped or the complete finite list of candidate tenant IDs and their root/child storage field. Use the same scope object for preflight proof and filter injection. Obtain the object from the actual configured enforcement boundary, never from claims alone. REST injects the captured scope before shared execution; GraphQL and export pipelines do likewise. Async export acceptance and execution each perform the proof using their resolved configuration and current policy snapshot.

For every finite candidate tenant, certify custom conditions and evaluate the original complete Cedar policy set on a typed tenant-bound witness with empty parents. Every candidate must Allow without diagnostic errors. Require canonical tenant identities to avoid Cedar parsing translation diverging from raw backend filter values. Root scopes bind `_tenant` from exactly the ID set enforced by storage.

Extend the conservative EST proof with ordered Boolean reasoning. Evaluate principal/action-only Boolean subexpressions using public `Policy::from_json` and `Authorizer` APIs. Only support resource tenant presence and direct tenant equality/inequality with entity literals. A finite scope proves presence; an unscoped resource needs a positive `has _tenant` fact before reading its optional tenant. Guarded comparisons can be total unknown Booleans, allowing a following known false conjunct to prove the entire condition false for an administrator. Preserve Cedar evaluation order: an unsafe/error-producing left operand cannot be discarded because a right operand is false. Short-circuited right branches need not be evaluated. Reject unsupported residual dependencies and any actual diagnostic errors.

Keep exact generated-policy AST integrity, action inheritance restrictions, principal membership semantics, concrete schema-shape checks, arbitrary entity alias restrictions, and record-specific resource head denials. Add no resource parents, dependencies, public types, or lint suppressions.

## Files and validation
Modify access scope injection, private authz proof/engine, shared REST query helpers/callers, GraphQL resolver, export acceptance/materialization, and `docs/custom-policy-context.md`. Add focused unit and real-server regressions covering allowed/shared tenants, ancestor scope containing a forbidden tenant, roots, admin bypass, disabled/unscoped tenancy, true record fences, missing attributes/errors and Boolean order, exact generated rules, exports including execution recheck, and GraphQL.

Run locked Nextest through the disposable SurrealDB runner, relevant Clippy with warnings denied, formatting/diff checks, and existing query authorization regressions. Root owns independent security review, broader backend validation, versions/lockfile/changelog, signed commits, PR, and release. Public APIs remain unchanged; semver recommendation is patch.
