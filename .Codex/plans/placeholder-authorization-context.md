# Explicit placeholder authorization context (issue #155)

## Existing behavior

Application action checks without a concrete entity construct a Cedar resource with UID `<Schema>::"_any"`. Required representable attributes receive type defaults so strict entity validation succeeds. Optional attributes are absent. Custom policies currently cannot use an explicit request-context marker, and denial logs omit the synthetic nature of the resource. This affects Read preflights for collection and point reads, as well as other schema-level checks. The generic create route currently authorizes schema access, not a concrete proposed entity.

## Approved design

1. Declare required `context.resource_is_placeholder: Bool` on generated application actions and field actions. Set it to true only for `authorize(..., None)` and false for concrete resources and field checks. Keep `_any` and required defaults unchanged. Do not rewrite policies, remove forbids, or change generated owner rules.
2. Add explicit placeholder marker and actual Cedar resource UID to decision logs, and include matched policy IDs on denials. Test captured structured log fields for both placeholder and concrete denials.
3. Document guarded Read permits and forbids, including optional attribute guards and separate schema preflight admission. Explain that conditional permits cannot narrow other permits. Clearly state that guarding Create to exclude placeholders does not enforce proposed field values in the current generic create route.
4. Test strict validation, required versus optional fields, real records containing default-like values, unchanged unguarded forbids, field-context false, and HTTP list/point reads. Manual Cedar callers must provide the newly required context; omission must fail request validation.
5. Run focused and full nextest, all-target clippy with warnings denied, and independent PostgreSQL acceptance. Commit with signed Conventional Commits, open a PR, and wait for parent review before merge.

## Boundaries and compatibility

Own this worktree only. No #151 owner-policy edits, new dependencies, version bumps, release tags, publication, or deployment. The required Cedar context changes the generated request contract for callers that construct requests manually; the next release needs explicit compatibility/semver review. Framework entry-point signatures and HTTP response contracts stay unchanged. No static policy reachability analysis is attempted.

## Files

- `src/cedar/schema_gen.rs`: application/field action context declarations.
- `src/authz/engine.rs`: pure context construction and explicit decision tracing.
- `src/authz/adapters.rs`: accurate placeholder contract documentation.
- Authorization tests: policy evaluation, HTTP behavior, strict caller contract, structured log capture.
- Public documentation: custom policy context and safe Read examples, linked from existing policy guidance.

## Independent acceptance

A PostgreSQL CLI fixture with a required Boolean field passed all eleven GET/POST pagination, count, and filter cases under the guarded Read forbid; visible point reads returned 200 and hidden reads 403. Restoring the unguarded forbid preserved collection and visible-point denials. Captured JSON logs distinguished placeholder true with the `_any` UID from concrete false and included matched policy IDs. Cedar annotation labels are not stable PolicySet IDs; diagnostics use the IDs assigned to the compiled bundle.
