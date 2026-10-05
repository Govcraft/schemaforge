# HTTP rate limiting and proxy deployment

`schemaforge serve` enables acton-service's governor limiter by default. Its
configuration lives at the top-level `[rate_limit]`, separate from
`[schema_forge.export.rate_limit]`. Defaults are `auto_apply = true`,
`per_user_rpm = 200`, `per_client_rpm = 1000`, `window_secs = 60`, and
`trust_forwarded_headers = false`. Governor replenishes per-minute quotas
continuously and uses a global burst of 10% of the applicable quota (20 for
user requests and anonymous requests with default settings). `window_secs` does
not change governor's per-minute quota calculation.

Authenticated requests use the user/client identity when claims are available.
Anonymous requests, including login and metadata requests, fall back to the
direct TCP peer address. Their bucket uses `anonymous_rpm` and `anonymous_burst`
when configured; otherwise it uses `per_user_rpm` and a burst of 10% of that
quota, with a minimum of 1. Behind a reverse proxy, the peer address is usually
the proxy itself.

`/health` and `/ready` are exempt by default. `exempt_paths` matches the full
path exactly, ignores the HTTP method and query string, and distinguishes a
trailing slash. Setting the list replaces the defaults: include the probe paths
when adding other exemptions, or set `exempt_paths = []` to count probes.

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
exempt_paths = ["/health", "/ready"]
anonymous_rpm = 200
anonymous_burst = 20

[rate_limit.routes."POST /api/v1/forge/auth/login"]
requests_per_minute = 30
burst_size = 10
per_user = true
```

Route overrides use full incoming paths, optionally prefixed by the HTTP
method, and replace the global quota for matching requests. `per_user = true`
uses an identity or anonymous IP bucket; `false` uses one bucket shared by all
callers to that route. Exempt paths bypass the limiter before route overrides
are considered. A quota of zero is not an exemption.

Governor reports the current bucket capacity in `X-RateLimit-Remaining`.
Rate-limited responses include `Retry-After`; governor no longer emits
`X-RateLimit-Reset`.

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

Explicit `serve -H` and `-p` flags override `ACTON_SERVICE__BIND` and
`ACTON_SERVICE__PORT`, which override `[service] bind` and `port` in the selected
configuration. With no configured listener, SchemaForge binds to
`127.0.0.1:3000`. An explicit `0.0.0.0` remains supported. The startup banner
shows the effective address.

The unambiguous legacy names `ACTON_SERVICE_BIND` and `ACTON_SERVICE_PORT`
remain supported. New configuration should use the double-underscore names.
