# Authenticated entity events

Compile the CLI or integration crate with `--features sse` and enable the route:

```toml
[schema_forge.events]
enabled = true
keep_alive_secs = 15
channel_capacity = 256
max_connections_per_user = 8
retry_secs = 3
```

Released binaries include SSE support. The feature and configuration default to
 disabled for library builds, and configuration defaults to disabled for binaries.
Enabling events in a binary without SSE support refuses startup. Bounds must be
positive: keep-alive/retry up to 3600 seconds, capacity up to 65536 messages, and
up to 1024 connections per subject.

Connect to `GET /api/v1/forge/schemas/Note/events` with a PASETO bearer header.
Use `X-Active-Tenant: Organization:<entity_id>` just as on entity GET routes.
Native `EventSource` cannot supply these headers; use a streaming fetch reader.
Tokens in query strings are rejected. Unknown schemas return 404, anonymous
requests 401, denied reads/membership 403, and exhausted connection limits 429.
Disabled events return 404.

```javascript
async function readChanges(token, activeTenant, onEvent, signal) {
  const response = await fetch('/api/v1/forge/schemas/Note/events?category=books', {
    headers: {
      Authorization: `Bearer ${token}`,
      ...(activeTenant ? { 'X-Active-Tenant': activeTenant } : {}),
    },
    signal,
  });
  if (!response.ok) throw new Error(`Stream refused: ${response.status}`);
  const reader = response.body.pipeThrough(new TextDecoderStream()).getReader();
  let pending = '';
  try {
    for (;;) {
      const { value, done } = await reader.read();
      if (done) break;
      pending += value.replace(/\r\n/g, '\n');
      let end;
      while ((end = pending.indexOf('\n\n')) !== -1) {
        const frame = pending.slice(0, end);
        pending = pending.slice(end + 2);
        const lines = frame.split('\n');
        const kind = lines.find(line => line.startsWith('event:'))?.slice(6).trim();
        const data = lines.filter(line => line.startsWith('data:'))
          .map(line => line.slice(5).trimStart()).join('\n');
        if (!data) continue; // retry and keep-alive frames
        onEvent(kind, JSON.parse(data));
        if (kind === 'closed') return;
      }
    }
  } finally {
    await reader.cancel();
  }
}
```

Every change has `event_id`, `event_type`, `schema`, `entity_id`, `timestamp`, and
`actor`. The SSE `id:` matches `event_id`; events are `entity.created`,
`entity.updated`, and `entity.deleted`. Create/update include `entity`, with the
same shape as `GET /entities/{id}?resolve=false`, including permissions. Deletes
carry metadata and the ID, without the deleted entity. Webhooks reuse commit
metadata when events are enabled.

Authorization evaluates the full committed record, or the full pre-delete record,
for each subscriber. Read hooks, record visibility, field restrictions, hidden
fields, relation IDs, and derived collections use the entity GET projection.
Denied records produce no event. Filters accept `field=value` or
`field__eq=value`, using the normal query parser's type coercion. Fields must be
known, readable, and visible in the projected result; hidden or unauthorized
fields cannot become a filtering oracle. Sorting, pagination, nested paths, and
non-equality operators are rejected.

A single framework broadcaster serves this process. Actor-owned event writes
serialize the backend commit and nonblocking publication before replying, so
commits on an entity remain ordered even if its HTTP request is cancelled. Slow
consumers receive a final `closed` event with `reason: slow_consumer` and disconnect.
Their bounded queues never hold up or fail writers. Dropping a stream releases
its connection reservation. Other reasons include authorization, schema, or
stream changes; clients should refetch and reconnect only after resolving them.

Live account status, roles, tenant membership and the effective tenant hierarchy
are checked on connection, before each queued change, and at most every five
seconds while idle. Platform administrators bypass tenant scope, so only their
account status and roles are rechecked. A changed active scope ends the stream
with a final generic `closed` event; no membership record payload is exposed.
Policy changes apply to subsequent projections. Tokens expiring also end their
streams.

There is no replay buffer, cross-process delivery, or durable outbox. Only
mutations through the shared REST/GraphQL entity handlers and create-intent commit
path publish events; external database writes and custom direct backend calls do
not. Process failure can lose delivery after a commit. `Last-Event-ID` is ignored.
Refetch the current authorized state after every reconnect, switch of tenant, or
`closed` event. Connect before refetching and buffer intervening events if the
consumer needs to avoid a gap between its snapshot and live updates.

Embedded services initialize `EventsRuntime` with their auth/entity stores and
send `ConfigureEvents` after `InitForge`, before accepting connections. These
services share the same router and commit envelopes as the CLI.
