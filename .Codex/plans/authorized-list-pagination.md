# Authorized entity list pagination (issue #152)

## Diagnosis and contract

The shared GET list and POST query route asks storage for counts and a paged result before applying the operator record policy and Cedar row authorization. This exposes counts of unreadable rows and can produce empty intermediate pages. Preserve both authorization checks and tenant/filter/sort rules; apply them before caller offset, page limit, and total count.

## Implementation plan

1. Scan complete candidate entities in bounded batches using the existing backend query API and deterministic ID tie-breaker. Disable storage counts.
2. Run the unchanged operator policy and Cedar checks on each batch, accumulate the readable total, and retain only the requested readable page. Stop after filling the page when count is disabled; preserve anonymous total omission. Do not project fields until after authorization.
3. Propagate backend failures without returning partial pages or totals. Preserve zero-limit and beyond-end behavior without overflow.
4. Add HTTP tests for mixed and all-denied rows, filters, projections, GET and POST queries, readable offsets, beyond-end pages, and a denied prefix crossing the batch boundary. Instrument an operator policy to check bounded batches and count=false early stopping.
5. Document the authorized-total contract and costs. Exact totals require evaluating all matching rows; the backend interface has no authorization predicate translation or shared snapshot. PostgreSQL and SurrealDB push bounded queries to storage. SQL Server currently reads the whole table per backend query, so route batching does not bound that backend's internal memory and increases repeated scan work.
6. Run focused regression tests, full PostgreSQL integration tests, CLI tests, clippy with warnings denied, and independent PostgreSQL HTTP acceptance before the next release.

## Coordination

The parent approved this plan. Release remains held until this fix and the request-context patch are both validated. No owner semantics, hidden-row counts, or public API fields are added.
