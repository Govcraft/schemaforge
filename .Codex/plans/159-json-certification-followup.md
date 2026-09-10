# Issue 159 follow-up: distinguish JSON breadth from decoder limits

## Confirmed problem

The v0.44.1 PostgreSQL storage proof rejects a whole collection when any JSON
document contains at least 100 opening delimiters. That is a bound on breadth
and string contents, not decoder nesting. Read-only aggregate investigation
found 14,558 rejected documents in a 67,650-row collection, while actual
structural-depth and numeric-range checks rejected none. A sample of 100
rejected documents had maximum nesting seven. The earlier synthetic fixture
covered data types but missed broad JSON structures.

## Implementation

Remove JSON-value certification from the authorized-count proof. Both the Cedar
schema generator and resource adapter omit JSON, including required JSON fields.
The existing proof rejects applicable custom Read policies and explicitly
supplied record policies. JSON content therefore cannot change the certified
Read decision. Retain physical JSONB type/schema checks and normal decoding of
selected rows, plus every identity, tenant, and Cedar-representable value check.

Independent authorization review found no visibility or count counterexample.
An undecodable JSON value outside the selected page previously caused an
incidental backend error before authorization. It can now contribute to an exact
authorized count; selecting that row still returns its normal decoding error.
The count describes authorization, not a guarantee of payload decodability.

Add debug diagnostics identifying the failed storage-certification category
and field without logging row identifiers or values. Passing proofs must not
pay for extra diagnostic queries. Keep exact totals, tenant rules, page-only
queries, and all other authorization checks unchanged.

## Validation

Use generic synthetic records only in the public repository. Cover broad
arrays and objects, delimiter-heavy and digit-heavy strings, and undecodable
out-of-page versus selected JSON. Include required/hidden JSON and tenant
exclusion where applicable. Measure the replacement against a broad-JSON
collection at realistic scale, then
check the released v0.44.1 failure and corrected HTTP behavior on that fixture.
Run affected PostgreSQL tests and Clippy, retaining existing safety regressions.
Read-only deployment predicates confirmed that breadth, rather than actual
depth or numeric limits, caused the fallback. Do not export records or mutate
deployment data during diagnosis.

## Measured reproduction

The v0.44.1 release binary returns HTTP 408 after 30.056 seconds for a generic
67,650-row fixture with 33 fields and broad JSON in one fifth of its rows.
The same fixture returns its page with count disabled in 0.09 seconds.

The corrected v0.44.2 candidate returns the exact total of 67,650 in 0.372
seconds. A selected broad JSON payload, filtered POST query, and past-end page
all passed in 0.29 to 0.39 seconds. Seven PostgreSQL regressions and affected
all-target Clippy passed. The published patch must be verified against the
affected endpoint before the issue is closed again.

The parent owns deployment diagnosis, integration, release preparation, and
acceptance. The PostgreSQL worker owns the proof module and its tests.
