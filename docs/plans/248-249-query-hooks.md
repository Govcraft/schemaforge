# Close issues 248 and 249 and release 0.52.0

## Scope and evidence

The open issue inventory at `9340d19` contains two reports. List and query
handlers validate filter types but execute filters and sorts before projecting
restricted fields. Update handlers calculate changed field names for events but
discard that information when constructing detached hook invocations.

## Query authorization

Authorize each filter leaf and sort path before querying storage. Hidden fields
must always fail. Root field permissions must allow reading the queried field;
relation traversal must fail unless target record and field readability can be
established safely. Account for record-dependent custom policies, which a
placeholder authorization alone cannot prove safe. Preserve pagination and
query limits without introducing an unbounded authorization scan.

Regression tests cover REST equality and prefix probes, sorting, JSON query
filters and sorts, nested boolean filters, hidden fields, allowed role reads,
write-only restrictions, and custom field policies. Rejected requests must not
reach the storage query. Apply the same boundary to GraphQL lists and to
both synchronous and asynchronous filtered exports, which share the leak.

## Detached change hooks

Add `changed_fields` and `previous` to hook invocations. Compute update and patch
metadata from persisted pre-write and post-write entities, including hook
mutations. Previous values contain only changed fields. Create and delete
invocations carry empty metadata. Failed writes emit no detached invocation.

Extend generated AfterChange protobuf requests with unused system field tags,
preserving existing field numbers. Older descriptors remain supported and
receive their existing request shape. Regenerated services receive metadata.
Tests cover unchanged writes, nulls, additions/removals, direct and batch update
paths, descriptor encoding, and older descriptors.

## Validation and release

Use an isolated worktree. Run formatting, meaningful regression tests with
nextest, and Clippy with warnings denied. Validate supported backend feature
graphs independently through required CI and the existing full release
workflow. Fix failures before merging or publishing.

Release CLI 0.52.0 and runtime crate 0.50.0: hook metadata adds functionality and
new public Rust struct fields require downstream literal updates. Document
query authorization tightening and hook regeneration. Use signed Conventional
Commits, merge through the protected branch workflow, tag `v0.52.0`, and verify
the published archives and signed checksum manifest. Close both issues with
their implementation and validation evidence.
