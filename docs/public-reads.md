# Public entity reads

An explicit public-read policy lets callers load configuration before signing in:

```text
@access(read: ["public"], write: ["admin"], delete: ["admin"])
schema Branding {
    title: text
}
```

The official server accepts requests without an Authorization header on entity list and single-record GET/HEAD routes. Cedar decides whether the requested schema is public. Missing credentials on protected schemas return 401. A supplied invalid, expired, malformed, or empty credential still returns 401 on these entity routes. Valid credentials retain their roles, tenant context, and normal audit and rate-limit behavior. Other routes, including mutations and POST queries, keep their authentication requirements.

An empty role list means authenticated users, not public access. Generated schema and field permits verify that the principal has authenticated identity attributes; the anonymous principal has none. Existing custom policies using only `principal is Forge::Principal` also match the anonymous principal type. Add `when { principal has id }` when such a custom permit is intended only for authenticated callers.

Public schema access does not override record ownership, tenant isolation, explicit Cedar forbids, or field restrictions. Related display values and derived child IDs also require authorization for the target data. Anonymous list responses omit the `total_count` field, because that total is calculated before record filtering; `count` describes only the visible page.

Custom `RecordAccessPolicy` implementations deny anonymous reads by default. To support public reads, override `filter_visible_optional` and explicitly handle absent claims. Authenticated requests continue through the existing `filter_visible` implementation. The built-in Cedar policy supports both callers without inventing authenticated claims.

Embedding applications must opt selected GET/HEAD entity routes into acton-service's optional token authentication and retain schema authorization. Marking an entity prefix as `public_paths` skips token verification and is not an equivalent configuration.
