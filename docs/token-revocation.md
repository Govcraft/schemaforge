# Revoke bearer tokens

`schemaforge serve` stores token revocation in its entity database. It uses the
existing SurrealDB, PostgreSQL, or SQL Server connection and initializes the
revocation table before accepting requests. No separate cache server is required.

A platform administrator can revoke an exact subject's older tokens:

```sh
curl -X POST "$FORGE_URL/api/v1/forge/auth/revocations" \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"subject":"user:alice@example.gov","not_before":1791400000}'
```

`not_before` is an inclusive Unix timestamp in seconds. Tokens issued at or
before that time are refused, including tokens without an issuance timestamp.
Tokens issued afterward remain usable. Use the token's exact `sub` value;
login subjects have the `user:` prefix, and service token subjects need not.

The response contains `subject` and the effective `not_before`. Cutoffs only
increase, so repeating a request with an older timestamp cannot restore access.
Cutoffs survive process restarts and apply to every instance sharing the database
and revocation namespace. They remain stored after individual tokens expire.

HTTP and gRPC token authentication check revocation. An existing event stream
closes at its next identity check, using the configured events recheck cadence.
Database failures refuse authenticated requests and close existing streams.
Changing a user's `active` flag does not itself revoke their bearer tokens; use
this route to revoke existing tokens as part of account deactivation.

The route requires authentication and the `platform_admin` role. Invalid input
returns 422, insufficient privileges return 403, and unavailable revocation
storage returns 502. Successful changes emit `forge.token.subject_revoked` audit
events with the actor, subject, and effective cutoff.

The default revocation namespace is `schemaforge`. An optional acton-service
section can select a deployment-specific namespace. Its backend must match the
entity database:

```toml
[revocation]
backend = "surrealdb" # or "postgres" or "mssql"
namespace = "my-deployment"
```

Use the same namespace on every instance of a deployment. Changing it selects
an independent set of cutoffs.
