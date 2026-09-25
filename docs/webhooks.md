# Webhook delivery contract

Enable webhooks with `[schema_forge.webhooks] enabled = true`, mark a schema with
`@webhook`, and configure an inline URL or a `WebhookSubscription`.

Delivery is **best effort**. Entity persistence completes before subscription
lookup and background delivery start. A process failure between those steps can
lose an event. Pending deliveries and retries exist only in memory, so a restart
loses them. Exhausted retries are logged and discarded. There is no durable
outbox, delivery history, dead-letter queue, replay endpoint, or gap-detection
sequence. A successful entity API response does not mean its webhook arrived.

The default is one attempt plus three retries with exponential backoff. HTTP
4xx responses stop retries; other failures retry up to the configured limit.
The `X-SchemaForge-Delivery` ID stays the same across retries of one event.
Subscribers should deduplicate that ID: a timeout can occur after the subscriber
has already processed a request. The ID does not identify missing events.

Integrations that need current state should periodically reconcile through the
entity list/query APIs, using their normal authorization and tenant scope.
Polling can recover current visible records but cannot reconstruct intermediate
updates or deleted records. Applications requiring a complete change history
must maintain a separate durable change log; webhooks do not provide one.

## Payload format

Events include `payload_version: 2`. Create and update payloads are JSON objects
using the REST API's value encoding, including JSON nulls and RFC 3339 datetimes.
This replaces the earlier tagged `DynamicValue` encoding. Delete events have a
null payload. Consumers of the old format must update their decoders.

`@hidden` fields are always omitted. Fields with `@field_access` annotations are
also always omitted because subscriptions have no authenticated field-read
identity. This conservative field policy applies regardless of the triggering
caller's role. Other fields can still be limited by the triggering request's
read projection. Events contain the entity ID, schema, actor, and timestamp.

## Destination policy

Only public HTTP(S) destinations are supported. `allowed_url_schemes` defaults
to `["https"]`; permitting `http` is an explicit operator choice. Subscription
writes and inline schema application validate destinations, including DNS
resolution. Delivery repeats the check for existing records and DNS changes,
rejects an answer containing any non-public address, and connects only to the
checked addresses. Redirects and environment HTTP proxies are disabled. URL
credentials and fragments are rejected. Private destinations are unsupported.
