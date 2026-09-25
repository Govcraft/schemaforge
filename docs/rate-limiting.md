# HTTP rate limiting and proxy deployment

`schemaforge serve` enables acton-service's governor limiter by default. Its
configuration lives at the top-level `[rate_limit]`, separate from
`[schema_forge.export.rate_limit]`. Defaults are `auto_apply = true`,
`per_user_rpm = 200`, `per_client_rpm = 1000`, `window_secs = 60`, and
`trust_forwarded_headers = false`. Governor replenishes per-minute quotas
continuously and uses a global burst of 10% of the applicable quota (20 for
anonymous/user requests). `window_secs` does not change governor's per-minute
quota calculation.

Authenticated requests use the user/client identity when claims are available.
Anonymous requests, including login, health, readiness and metadata requests,
fall back to the direct TCP peer address and share its user-limit bucket.
Behind a reverse proxy, that address is usually the proxy itself.

For a server reachable only through a trusted proxy, set
`trust_forwarded_headers = true` and configure the proxy to overwrite incoming
`X-Forwarded-For` and `X-Real-IP`. Governor reads the first forwarded address.
Prevent direct access to the application port, since otherwise callers could
choose their own bucket by spoofing headers. If those conditions cannot be
met, keep this option false and size the shared quota for aggregate traffic,
or enforce rate limits at the proxy and set `auto_apply = false`.

```toml
[rate_limit]
per_user_rpm = 200
per_client_rpm = 1000
trust_forwarded_headers = true

[rate_limit.routes."POST /api/v1/forge/auth/login"]
requests_per_minute = 30
burst_size = 10
per_user = true

[rate_limit.routes."GET /health"]
requests_per_minute = 6000
burst_size = 1000
per_user = false

[rate_limit.routes."GET /ready"]
requests_per_minute = 6000
burst_size = 1000
per_user = false
```

Route overrides use full incoming paths, optionally prefixed by the HTTP
method, and replace the global quota for matching requests. `per_user = true`
uses an identity or anonymous IP bucket; `false` uses one bucket shared by all
callers to that route. The example gives probes separate generously sized
buckets so login traffic cannot consume their allowance. Adjust paths to match
your deployed probe endpoints. Governor has no per-route exemption switch;
to guarantee unthrottled probes, disable automatic governor application and
configure the proxy limiter to exclude probe routes. A quota of zero is not an
exemption.

## Entity CLI retries

Entity JSON API requests and export requests retry explicit HTTP 429 responses
up to `--max-retries` times (default 3, maximum 10). `--max-retries 0` disables
retries. The client honors `Retry-After` in seconds or HTTP-date form; without a
valid header it waits 1, 2, 4 seconds and so on, capped at 30 seconds per delay.
The retry count bounds attempts; a server-supplied delay can be longer than the
fallback cap. A past date permits an immediate retry. The final 429 becomes the
normal CLI HTTP error. Transport failures and other status codes are not
retried because a write may already have committed. Direct object-storage file
transfers do not use these API retries.

```sh
schemaforge entity create Note --set title=example --max-retries 5
```

## Listener precedence

Explicit `serve -H` and `-p` flags override `ACTON_SERVICE_BIND` and
`ACTON_SERVICE_PORT`, which override `[service] bind` and `port` in the selected
configuration. With no configured listener, SchemaForge binds to
`127.0.0.1:3000`. An explicit `0.0.0.0` remains supported. The startup banner
shows the effective address.
