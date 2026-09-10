# Authorized entity pagination

Starting with SchemaForge v0.43.1, GET `/api/v1/forge/schemas/{schema}/entities` and POST `/api/v1/forge/schemas/{schema}/entities/query` apply record authorization before response pagination and total counting.

`count` is the number of entities in the returned page. For authenticated callers requesting a total, `total_count` is the number of readable records matching the tenant scope and query filters, before the caller's offset and limit. Offsets skip readable records. Denied records neither contribute to the total nor leave gaps in a page. A filter that matches only denied records returns an empty page with a zero total. An offset beyond the readable end returns an empty page with the readable total unchanged.

Authorization still applies the configured operator record policy and Cedar checks to complete rows. Field projection, field access filtering, and relation enrichment occur after record authorization and page selection. This change does not alter owner semantics or any authorization rule. No count of withheld records is exposed. Anonymous responses continue to omit `total_count`.

Exact authorized totals require evaluating every matching candidate because the backend API cannot translate arbitrary record policies into storage predicates. The route reads candidates in batches of 256 and retains the requested readable page. For large collections, `count=false` on GET, or `"count": false` in a POST query, omits the total and stops scanning after filling the readable page. Deep offsets still require evaluating the preceding candidates. An omitted limit can still return all readable matches.

PostgreSQL and SurrealDB execute bounded, filtered, sorted candidate queries. The current SQL Server backend reads and filters the table internally for each query, so route batching does not bound its internal memory and repeated batches increase scan work. Exact authorized counts can be substantially more expensive than a storage `COUNT(*)`.

Queries use deterministic ordering with an ID tie-breaker. Candidate batches do not share a database snapshot, so concurrent inserts, deletes, or changes to sort/filter fields can affect a scan or later pages. The response is an observation, not a retained snapshot. Backend failures abort the request instead of returning a partial page with a fabricated total.
