use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use hmac::{Hmac, KeyInit, Mac};
use schema_forge_backend::Entity;
use schema_forge_core::query::{FieldPath, Filter, Query};
use schema_forge_core::types::{Annotation, DynamicValue, SchemaDefinition};
use serde::Serialize;
use sha2::Sha256;
use tracing::{debug, error, warn};

use crate::state::{DynForgeBackend, SchemaRegistry};

type HmacSha256 = Hmac<Sha256>;

/// Global webhook dispatcher, lazily initialized from config.
static DISPATCHER: std::sync::OnceLock<WebhookDispatcher> = std::sync::OnceLock::new();

/// Get (or create) the global webhook dispatcher from config.
///
/// Returns `None` if webhooks are disabled.
pub fn get_dispatcher(config: &WebhookConfig) -> Option<&'static WebhookDispatcher> {
    if !config.enabled {
        return None;
    }
    Some(DISPATCHER.get_or_init(|| WebhookDispatcher::new(config.clone())))
}

/// Valid webhook event types.
pub const VALID_EVENTS: &[&str] = &["created", "updated", "deleted"];

// ---------------------------------------------------------------------------
// WebhookEvent
// ---------------------------------------------------------------------------

/// The JSON payload delivered to webhook subscribers.
#[derive(Debug, Clone, Serialize)]
pub struct WebhookEvent {
    /// Wire payload format version (plain JSON fields).
    pub payload_version: u8,
    /// Unique delivery ID (UUID v4).
    pub event_id: String,
    /// Event type: `entity.created`, `entity.updated`, or `entity.deleted`.
    pub event_type: String,
    /// Schema name the entity belongs to.
    pub schema: String,
    /// Entity ID.
    pub entity_id: String,
    /// ISO 8601 timestamp.
    pub timestamp: String,
    /// User who triggered the event.
    pub actor: Option<String>,
    /// Entity fields (present for create/update, absent for delete).
    pub payload: Option<serde_json::Value>,
}

impl WebhookEvent {
    /// Build an event from a create operation.
    pub fn from_create(schema: &SchemaDefinition, entity: &Entity, actor: Option<&str>) -> Self {
        Self {
            payload_version: 2,
            event_id: uuid::Uuid::new_v4().to_string(),
            event_type: "entity.created".to_string(),
            schema: schema.name.to_string(),
            entity_id: entity.id.as_str().to_string(),
            timestamp: now_iso8601(),
            actor: actor.map(String::from),
            payload: Some(entity_fields_to_json(entity, schema)),
        }
    }

    /// Build an event from an update operation.
    pub fn from_update(schema: &SchemaDefinition, entity: &Entity, actor: Option<&str>) -> Self {
        Self {
            payload_version: 2,
            event_id: uuid::Uuid::new_v4().to_string(),
            event_type: "entity.updated".to_string(),
            schema: schema.name.to_string(),
            entity_id: entity.id.as_str().to_string(),
            timestamp: now_iso8601(),
            actor: actor.map(String::from),
            payload: Some(entity_fields_to_json(entity, schema)),
        }
    }

    /// Build an event from a delete operation (no payload — entity is gone).
    pub fn from_delete(schema: &str, entity_id: &str, actor: Option<&str>) -> Self {
        Self {
            payload_version: 2,
            event_id: uuid::Uuid::new_v4().to_string(),
            event_type: "entity.deleted".to_string(),
            schema: schema.to_string(),
            entity_id: entity_id.to_string(),
            timestamp: now_iso8601(),
            actor: actor.map(String::from),
            payload: None,
        }
    }
}

/// Convert entity fields to a JSON value.
fn entity_fields_to_json(entity: &Entity, schema: &SchemaDefinition) -> serde_json::Value {
    let mut fields = crate::conversions::entity_to_response(entity, schema).fields;
    // Subscribers have no caller claims. Use a fixed conservative field policy.
    fields.retain(|name, _| {
        schema
            .field(name)
            .is_none_or(|field| field.field_access().is_none())
    });
    serde_json::Value::Object(fields)
}

/// Get current UTC time as RFC 3339 string.
fn now_iso8601() -> String {
    humantime::format_rfc3339_millis(std::time::SystemTime::now()).to_string()
}

// ---------------------------------------------------------------------------
// WebhookConfig
// ---------------------------------------------------------------------------

/// Global webhook configuration.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct WebhookConfig {
    /// Enable webhook system globally (default: false).
    #[serde(default)]
    pub enabled: bool,

    /// Default retry count for failed deliveries (default: 3).
    #[serde(default = "default_retry_count")]
    pub default_retry_count: u32,

    /// Default timeout per delivery attempt in seconds (default: 10).
    #[serde(default = "default_timeout_seconds")]
    pub default_timeout_seconds: u32,

    /// Maximum concurrent webhook deliveries (default: 100).
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent_deliveries: usize,

    /// Global HMAC signing secret (fallback when subscription has no per-subscription secret).
    #[serde(default)]
    pub signing_secret: Option<String>,

    /// Allowed URL schemes (default: `["https"]`).
    #[serde(default = "default_allowed_schemes")]
    pub allowed_url_schemes: Vec<String>,
}

fn default_retry_count() -> u32 {
    3
}
fn default_timeout_seconds() -> u32 {
    10
}
fn default_max_concurrent() -> usize {
    100
}
fn default_allowed_schemes() -> Vec<String> {
    vec!["https".to_string()]
}

impl Default for WebhookConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            default_retry_count: default_retry_count(),
            default_timeout_seconds: default_timeout_seconds(),
            max_concurrent_deliveries: default_max_concurrent(),
            signing_secret: None,
            allowed_url_schemes: default_allowed_schemes(),
        }
    }
}

// ---------------------------------------------------------------------------
// ResolvedSubscription
// ---------------------------------------------------------------------------

/// A unified webhook subscription resolved from either DSL annotations or
/// runtime `WebhookSubscription` entities.
#[derive(Debug, Clone)]
pub struct ResolvedSubscription {
    /// Target URL to POST to.
    pub url: String,
    /// HMAC signing secret (per-subscription override).
    pub secret: Option<String>,
    /// Retry count override (falls back to global config).
    pub retry_count: Option<u32>,
    /// Timeout override in seconds (falls back to global config).
    pub timeout_seconds: Option<u32>,
}

// ---------------------------------------------------------------------------
// WebhookDispatcher
// ---------------------------------------------------------------------------

/// Non-blocking webhook delivery engine.
///
/// Spawns background `tokio` tasks for each delivery, with retry and
/// exponential backoff. Never blocks the calling HTTP handler.
#[derive(Clone)]
pub struct WebhookDispatcher {
    config: WebhookConfig,
    semaphore: Arc<tokio::sync::Semaphore>,
}

impl WebhookDispatcher {
    /// Create a new dispatcher with the given configuration.
    pub fn new(config: WebhookConfig) -> Self {
        let semaphore = Arc::new(tokio::sync::Semaphore::new(
            config.max_concurrent_deliveries,
        ));
        Self { config, semaphore }
    }

    /// Fire-and-forget: spawn a background delivery task for each subscription.
    ///
    /// Returns immediately — webhook delivery never blocks the API response.
    pub fn dispatch(&self, event: WebhookEvent, subscriptions: Vec<ResolvedSubscription>) {
        for sub in subscriptions {
            let event = event.clone();
            let config = self.config.clone();
            let semaphore = self.semaphore.clone();
            tokio::spawn(async move {
                let _permit = match semaphore.acquire().await {
                    Ok(p) => p,
                    Err(_) => {
                        error!(url = %sub.url, "webhook semaphore closed");
                        return;
                    }
                };
                deliver_with_retry(&event, &sub, &config).await;
            });
        }
    }

    /// Resolve all active subscriptions for a schema + event type.
    ///
    /// Merges inline DSL subscriptions (from `@webhook(url: "...")`) with
    /// runtime `WebhookSubscription` entities from the database.
    pub async fn resolve_subscriptions(
        &self,
        schema_def: &SchemaDefinition,
        event_type: &str,
        backend: &dyn DynForgeBackend,
        registry: &SchemaRegistry,
    ) -> Vec<ResolvedSubscription> {
        let mut subs = Vec::new();

        // 1. Inline DSL subscription (from @webhook annotation)
        if let Some(Annotation::Webhook {
            url: Some(url),
            secret,
            ..
        }) = schema_def.webhook_annotation()
        {
            subs.push(ResolvedSubscription {
                url: url.clone(),
                secret: secret.clone(),
                retry_count: None,
                timeout_seconds: None,
            });
        }

        // 2. Runtime subscriptions from WebhookSubscription entities
        let schema_name = schema_def.name.as_str();
        match query_webhook_subscriptions(backend, registry, schema_name, event_type).await {
            Ok(runtime_subs) => subs.extend(runtime_subs),
            Err(e) => {
                warn!(
                    schema = schema_name,
                    error = %e,
                    "failed to query runtime webhook subscriptions"
                );
            }
        }

        subs
    }
}

// ---------------------------------------------------------------------------
// Delivery internals
// ---------------------------------------------------------------------------

/// Deliver a webhook event with exponential backoff retry.
async fn deliver_with_retry(
    event: &WebhookEvent,
    subscription: &ResolvedSubscription,
    config: &WebhookConfig,
) {
    let max_retries = subscription
        .retry_count
        .unwrap_or(config.default_retry_count);
    let timeout = Duration::from_secs(
        subscription
            .timeout_seconds
            .unwrap_or(config.default_timeout_seconds) as u64,
    );

    let body = match serde_json::to_vec(event) {
        Ok(b) => b,
        Err(e) => {
            error!(error = %e, "failed to serialize webhook event");
            return;
        }
    };

    let signature = compute_signature(subscription, config, &body);

    for attempt in 0..=max_retries {
        if attempt > 0 {
            let backoff = Duration::from_millis(
                500_u64
                    .saturating_mul(2_u64.saturating_pow(attempt - 1))
                    .min(60_000),
            );
            tokio::time::sleep(backoff).await;
        }

        let client = match checked_client(&subscription.url, config, timeout).await {
            Ok(client) => client,
            Err(error) => {
                warn!(%error, event_id = %event.event_id, "webhook destination refused");
                if matches!(error, WebhookUrlError::Resolution) {
                    continue;
                }
                return;
            }
        };
        let mut request = client
            .post(&subscription.url)
            .header("Content-Type", "application/json")
            .header("X-SchemaForge-Event", &event.event_type)
            .header("X-SchemaForge-Delivery", &event.event_id)
            .timeout(timeout)
            .body(body.clone());

        if let Some(ref sig) = signature {
            request = request.header("X-SchemaForge-Signature", sig.as_str());
        }

        match request.send().await {
            Ok(resp) if resp.status().is_success() => {
                debug!(
                    url = %subscription.url,
                    event_id = %event.event_id,
                    attempt,
                    "webhook delivered"
                );
                return;
            }
            Ok(resp) if resp.status().is_client_error() => {
                // 4xx = misconfigured subscription, don't retry
                warn!(
                    url = %subscription.url,
                    status = %resp.status(),
                    event_id = %event.event_id,
                    "webhook rejected with client error, not retrying"
                );
                return;
            }
            Ok(resp) => {
                warn!(
                    url = %subscription.url,
                    status = %resp.status(),
                    attempt,
                    "webhook delivery failed with server error"
                );
            }
            Err(e) => {
                warn!(
                    url = %subscription.url,
                    error = %e,
                    attempt,
                    "webhook delivery failed"
                );
            }
        }
    }

    error!(
        url = %subscription.url,
        event_id = %event.event_id,
        max_retries,
        "webhook delivery exhausted all retries"
    );
}

/// Compute HMAC-SHA256 signature for the request body.
fn compute_signature(
    subscription: &ResolvedSubscription,
    config: &WebhookConfig,
    body: &[u8],
) -> Option<String> {
    let secret = subscription
        .secret
        .as_deref()
        .or(config.signing_secret.as_deref())?;

    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC can accept any key size");
    mac.update(body);
    let result = mac.finalize();
    Some(format!("sha256={}", hex::encode(result.into_bytes())))
}

// ---------------------------------------------------------------------------
// Runtime subscription queries
// ---------------------------------------------------------------------------

/// Query `WebhookSubscription` entities from the database.
async fn query_webhook_subscriptions(
    backend: &dyn DynForgeBackend,
    registry: &SchemaRegistry,
    target_schema: &str,
    event_type: &str,
) -> Result<Vec<ResolvedSubscription>, schema_forge_backend::error::BackendError> {
    // Look up the WebhookSubscription schema to get its ID
    let ws_def = match registry.get("WebhookSubscription").await {
        Some(def) => def,
        None => {
            // WebhookSubscription schema not registered — no runtime subscriptions
            return Ok(Vec::new());
        }
    };

    let target_path =
        FieldPath::parse("target_schema").expect("target_schema is a valid field path");
    let active_path = FieldPath::parse("active").expect("active is a valid field path");

    let query = Query::new(ws_def.id.clone()).with_filter(Filter::and(vec![
        Filter::eq(target_path, DynamicValue::Text(target_schema.to_string())),
        Filter::eq(active_path, DynamicValue::Boolean(true)),
    ]));

    let result = backend.query(&query).await?;

    let subs = result
        .entities
        .iter()
        .filter(|entity| {
            // Filter by event type: empty events list = match all
            match entity.fields.get("events") {
                Some(DynamicValue::Array(events)) => {
                    events.is_empty()
                        || events
                            .iter()
                            .any(|e| matches!(e, DynamicValue::Text(t) if t == event_type))
                }
                _ => true,
            }
        })
        .filter_map(|entity| {
            let url = match entity.fields.get("url") {
                Some(DynamicValue::Text(u)) => u.clone(),
                _ => return None,
            };
            let secret = match entity.fields.get("secret") {
                Some(DynamicValue::Text(s)) if !s.is_empty() => Some(s.clone()),
                _ => None,
            };
            let retry_count = match entity.fields.get("retry_count") {
                Some(DynamicValue::Integer(n)) => Some(*n as u32),
                _ => None,
            };
            let timeout_seconds = match entity.fields.get("timeout_seconds") {
                Some(DynamicValue::Integer(n)) => Some(*n as u32),
                _ => None,
            };
            Some(ResolvedSubscription {
                url,
                secret,
                retry_count,
                timeout_seconds,
            })
        })
        .collect();

    Ok(subs)
}

// ---------------------------------------------------------------------------
// URL validation (SSRF protection)
// ---------------------------------------------------------------------------

/// Validate a webhook URL for safety.
///
/// Rejects private/loopback IPs and enforces allowed URL schemes.
pub fn validate_webhook_url(url: &str, allowed_schemes: &[String]) -> Result<(), WebhookUrlError> {
    let parsed = reqwest::Url::parse(url).map_err(|_| WebhookUrlError::InvalidUrl)?;
    if !matches!(parsed.scheme(), "http" | "https")
        || !allowed_schemes.iter().any(|s| s == parsed.scheme())
    {
        return Err(WebhookUrlError::DisallowedScheme(parsed.scheme().into()));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() || parsed.fragment().is_some() {
        return Err(WebhookUrlError::InvalidUrl);
    }
    let host = parsed.host_str().ok_or(WebhookUrlError::InvalidUrl)?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.trim_end_matches('.').eq_ignore_ascii_case("localhost")
        || host.parse::<IpAddr>().is_ok_and(|ip| is_private_ip(&ip))
    {
        return Err(WebhookUrlError::PrivateIp);
    }
    Ok(())
}

/// Conservative globally routable address policy, including mapped IPv4.
fn is_private_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, c, _] = v4.octets();
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || a == 0
                || a >= 224
                || (a == 100 && (64..=127).contains(&b))
                || (a == 192 && b == 0 && (c == 0 || c == 2))
                || (a == 192 && b == 88 && c == 99)
                || (a == 198 && (b == 18 || b == 19 || (b == 51 && c == 100)))
                || (a == 203 && b == 0 && c == 113)
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_private_ip(&IpAddr::V4(v4));
            }
            let s = v6.segments();
            // Permit ordinary global unicast only, excluding special allocations,
            // documentation ranges and transition mechanisms.
            s[0] & 0xe000 != 0x2000
                || (s[0] == 0x2001 && s[1] < 0x200)
                || (s[0] == 0x2001 && s[1] == 0xdb8)
                || s[0] == 0x2002
                || (s[0] == 0x3fff && s[1] < 0x1000)
        }
    }
}

fn validate_addresses(addresses: &[SocketAddr]) -> Result<(), WebhookUrlError> {
    if addresses.is_empty() {
        return Err(WebhookUrlError::Resolution);
    }
    if addresses.iter().any(|address| is_private_ip(&address.ip())) {
        return Err(WebhookUrlError::PrivateIp);
    }
    Ok(())
}

async fn resolved_destination(
    url: &str,
    config: &WebhookConfig,
    timeout: Duration,
) -> Result<(reqwest::Url, Vec<SocketAddr>), WebhookUrlError> {
    validate_webhook_url(url, &config.allowed_url_schemes)?;
    let parsed = reqwest::Url::parse(url).map_err(|_| WebhookUrlError::InvalidUrl)?;
    let host = parsed
        .host_str()
        .ok_or(WebhookUrlError::InvalidUrl)?
        .trim_start_matches('[')
        .trim_end_matches(']');
    let port = parsed
        .port_or_known_default()
        .ok_or(WebhookUrlError::InvalidUrl)?;
    let addresses = tokio::time::timeout(timeout, tokio::net::lookup_host((host, port)))
        .await
        .map_err(|_| WebhookUrlError::Resolution)?
        .map_err(|_| WebhookUrlError::Resolution)?
        .collect::<Vec<_>>();
    validate_addresses(&addresses)?;
    Ok((parsed, addresses))
}

async fn checked_client(
    url: &str,
    config: &WebhookConfig,
    timeout: Duration,
) -> Result<reqwest::Client, WebhookUrlError> {
    let (parsed, addresses) = resolved_destination(url, config, timeout).await?;
    pinned_client(&parsed, &addresses)
}

fn pinned_client(
    parsed: &reqwest::Url,
    addresses: &[SocketAddr],
) -> Result<reqwest::Client, WebhookUrlError> {
    let host = parsed.host_str().ok_or(WebhookUrlError::InvalidUrl)?;
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .resolve_to_addrs(host, addresses)
        .build()
        .map_err(|_| WebhookUrlError::Transport)
}

/// Validate the effective fields before persisting a webhook subscription.
pub async fn validate_subscription_fields(
    schema: &SchemaDefinition,
    fields: &std::collections::BTreeMap<String, DynamicValue>,
    config: &WebhookConfig,
) -> Result<(), WebhookUrlError> {
    if schema.name.as_str() == "WebhookSubscription" {
        let Some(DynamicValue::Text(url)) = fields.get("url") else {
            return Err(WebhookUrlError::InvalidUrl);
        };
        resolved_destination(url, config, Duration::from_secs(10)).await?;
    }
    Ok(())
}

/// Validate an inline webhook destination before applying a schema.
pub async fn validate_schema_webhooks(
    schema: &SchemaDefinition,
    config: &WebhookConfig,
) -> Result<(), WebhookUrlError> {
    if let Some(Annotation::Webhook { url: Some(url), .. }) = schema.webhook_annotation() {
        resolved_destination(url, config, Duration::from_secs(10)).await?;
    }
    Ok(())
}

/// Errors from webhook URL validation.
#[derive(Debug)]
pub enum WebhookUrlError {
    InvalidUrl,
    DisallowedScheme(String),
    PrivateIp,
    Resolution,
    Transport,
}

impl std::fmt::Display for WebhookUrlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Resolution => write!(f, "webhook destination DNS resolution failed"),
            Self::Transport => write!(f, "webhook HTTP client initialization failed"),
            Self::InvalidUrl => write!(f, "invalid URL"),
            Self::DisallowedScheme(s) => write!(f, "disallowed URL scheme: {s}"),
            Self::PrivateIp => write!(f, "private or loopback IP addresses are not allowed"),
        }
    }
}

impl std::error::Error for WebhookUrlError {}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_projection_omits_nonpublic_fields_and_uses_plain_json() {
        let schema = schema_forge_dsl::parse(
            r#"schema Note {
            title: text
            secret: text @hidden
            salary: int @field_access(read: ["hr"], write: ["hr"])
        }"#,
        )
        .unwrap()
        .remove(0);
        let entity = Entity::new(
            schema.name.clone(),
            std::collections::BTreeMap::from([
                ("title".into(), DynamicValue::Text("hello".into())),
                ("secret".into(), DynamicValue::Text("private".into())),
                ("salary".into(), DynamicValue::Integer(100)),
            ]),
        );
        for event in [
            WebhookEvent::from_create(&schema, &entity, None),
            WebhookEvent::from_update(&schema, &entity, None),
        ] {
            assert_eq!(event.payload_version, 2);
            assert_eq!(event.payload, Some(serde_json::json!({"title": "hello"})));
        }
    }

    #[test]
    fn destination_policy_covers_special_address_ranges() {
        for host in [
            "127.1",
            "2130706433",
            "0.0.0.0",
            "100.64.1.2",
            "192.0.0.8",
            "198.18.0.1",
            "224.0.0.1",
            "255.255.255.255",
            "[::1]",
            "[::]",
            "[fc00::1]",
            "[fe80::1]",
            "[::ffff:127.0.0.1]",
            "[2002:7f00:1::]",
            "[2001:db8::1]",
            "[64:ff9b::7f00:1]",
            "localhost.",
        ] {
            assert!(
                validate_webhook_url(&format!("https://{host}/hook"), &["https".into()]).is_err(),
                "{host}"
            );
        }
        for host in ["8.8.8.8", "[2606:4700:4700::1111]", "example.com"] {
            assert!(
                validate_webhook_url(&format!("https://{host}/hook"), &["https".into()]).is_ok(),
                "{host}"
            );
        }
        for url in [
            "https://user:pass@example.com/",
            "https://example.com/#fragment",
            "file:///tmp/test",
        ] {
            assert!(validate_webhook_url(url, &["https".into(), "file".into()]).is_err());
        }
    }

    #[test]
    fn mixed_public_private_dns_results_are_refused() {
        let public = "8.8.8.8:443".parse().unwrap();
        let private = "10.0.0.1:443".parse().unwrap();
        assert!(validate_addresses(&[public]).is_ok());
        assert!(validate_addresses(&[public, private]).is_err());
        assert!(validate_addresses(&[]).is_err());
    }

    #[tokio::test]
    async fn pinned_transport_uses_checked_address_and_does_not_follow_redirects() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let initial = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let destination = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let checked_address = initial.local_addr().unwrap();
        let location = format!("http://{}/private", destination.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut connection, _) = initial.accept().await.unwrap();
            let mut request = vec![0; 4096];
            let count = connection.read(&mut request).await.unwrap();
            assert!(String::from_utf8_lossy(&request[..count]).contains("POST /hook"));
            connection.write_all(format!("HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
        });
        // Inject a fixture address at the transport boundary, after the policy
        // boundary tested separately. This hostname cannot resolve via DNS.
        let url = reqwest::Url::parse(&format!(
            "http://webhook.invalid:{}/hook",
            checked_address.port()
        ))
        .unwrap();
        let client = pinned_client(&url, &[checked_address]).unwrap();
        let response = client
            .post(url)
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::FOUND);
        server.await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), destination.accept())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn write_validation_rejects_inline_and_subscription_destinations() {
        let inline = schema_forge_dsl::parse(
            r#"@webhook(url: "https://[::1]/hook") schema Note { title: text }"#,
        )
        .unwrap()
        .remove(0);
        let config = WebhookConfig::default();
        assert!(validate_schema_webhooks(&inline, &config).await.is_err());
        let subscription = schema_forge_dsl::parse("schema WebhookSubscription { url: text }")
            .unwrap()
            .remove(0);
        for url in ["http://example.com/hook", "https://127.0.0.1/hook"] {
            let fields =
                std::collections::BTreeMap::from([("url".into(), DynamicValue::Text(url.into()))]);
            assert!(
                validate_subscription_fields(&subscription, &fields, &config)
                    .await
                    .is_err()
            );
        }
        assert!(
            validate_subscription_fields(&subscription, &Default::default(), &config)
                .await
                .is_err()
        );
        assert!(
            validate_subscription_fields(&inline, &Default::default(), &config)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn delivery_rejects_unsafe_stored_subscription_without_connecting() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let subscription = ResolvedSubscription {
            url: format!("http://{}/hook", listener.local_addr().unwrap()),
            secret: None,
            retry_count: Some(0),
            timeout_seconds: Some(1),
        };
        let config = WebhookConfig {
            allowed_url_schemes: vec!["http".into()],
            ..Default::default()
        };
        let event = WebhookEvent::from_delete("Note", "note_test", None);
        deliver_with_retry(&event, &subscription, &config).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(50), listener.accept())
                .await
                .is_err()
        );
    }

    #[test]
    fn compute_signature_with_secret() {
        let sub = ResolvedSubscription {
            url: "https://example.com/hook".to_string(),
            secret: Some("test-secret".to_string()),
            retry_count: None,
            timeout_seconds: None,
        };
        let config = WebhookConfig::default();
        let body = b"test body";

        let sig = compute_signature(&sub, &config, body);
        assert!(sig.is_some());
        assert!(sig.unwrap().starts_with("sha256="));
    }

    #[test]
    fn compute_signature_no_secret() {
        let sub = ResolvedSubscription {
            url: "https://example.com/hook".to_string(),
            secret: None,
            retry_count: None,
            timeout_seconds: None,
        };
        let config = WebhookConfig::default();
        let body = b"test body";

        let sig = compute_signature(&sub, &config, body);
        assert!(sig.is_none());
    }

    #[test]
    fn compute_signature_falls_back_to_global() {
        let sub = ResolvedSubscription {
            url: "https://example.com/hook".to_string(),
            secret: None,
            retry_count: None,
            timeout_seconds: None,
        };
        let config = WebhookConfig {
            signing_secret: Some("global-secret".to_string()),
            ..Default::default()
        };
        let body = b"test body";

        let sig = compute_signature(&sub, &config, body);
        assert!(sig.is_some());
    }

    #[test]
    fn validate_url_rejects_http() {
        let result = validate_webhook_url("http://example.com/hook", &["https".to_string()]);
        assert!(matches!(result, Err(WebhookUrlError::DisallowedScheme(_))));
    }

    #[test]
    fn validate_url_accepts_https() {
        let result = validate_webhook_url("https://example.com/hook", &["https".to_string()]);
        assert!(result.is_ok());
    }

    #[test]
    fn validate_url_rejects_localhost() {
        let result = validate_webhook_url("https://localhost/hook", &["https".to_string()]);
        assert!(matches!(result, Err(WebhookUrlError::PrivateIp)));
    }

    #[test]
    fn validate_url_rejects_loopback() {
        let result = validate_webhook_url("https://127.0.0.1/hook", &["https".to_string()]);
        assert!(matches!(result, Err(WebhookUrlError::PrivateIp)));
    }

    #[test]
    fn validate_url_rejects_private_ip() {
        let result = validate_webhook_url("https://10.0.0.1/hook", &["https".to_string()]);
        assert!(matches!(result, Err(WebhookUrlError::PrivateIp)));
    }

    #[test]
    fn validate_url_rejects_link_local() {
        let result = validate_webhook_url("https://169.254.1.1/hook", &["https".to_string()]);
        assert!(matches!(result, Err(WebhookUrlError::PrivateIp)));
    }

    #[test]
    fn validate_url_rejects_invalid() {
        let result = validate_webhook_url("not-a-url", &["https".to_string()]);
        assert!(matches!(result, Err(WebhookUrlError::InvalidUrl)));
    }

    #[test]
    fn webhook_config_defaults() {
        let config = WebhookConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.default_retry_count, 3);
        assert_eq!(config.default_timeout_seconds, 10);
        assert_eq!(config.max_concurrent_deliveries, 100);
        assert!(config.signing_secret.is_none());
        assert_eq!(config.allowed_url_schemes, vec!["https"]);
    }

    #[test]
    fn webhook_config_serde_roundtrip() {
        let config = WebhookConfig {
            enabled: true,
            default_retry_count: 5,
            default_timeout_seconds: 15,
            max_concurrent_deliveries: 50,
            signing_secret: Some("secret".to_string()),
            allowed_url_schemes: vec!["https".to_string(), "http".to_string()],
        };
        let json = serde_json::to_string(&config).unwrap();
        let back: WebhookConfig = serde_json::from_str(&json).unwrap();
        assert!(back.enabled);
        assert_eq!(back.default_retry_count, 5);
        assert_eq!(back.default_timeout_seconds, 15);
        assert_eq!(back.max_concurrent_deliveries, 50);
        assert_eq!(back.signing_secret.as_deref(), Some("secret"));
    }
}
