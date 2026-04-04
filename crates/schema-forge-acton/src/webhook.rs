use std::net::IpAddr;
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
    pub fn from_create(schema: &str, entity: &Entity, actor: Option<&str>) -> Self {
        Self {
            event_id: uuid::Uuid::new_v4().to_string(),
            event_type: "entity.created".to_string(),
            schema: schema.to_string(),
            entity_id: entity.id.as_str().to_string(),
            timestamp: now_iso8601(),
            actor: actor.map(String::from),
            payload: Some(entity_fields_to_json(entity)),
        }
    }

    /// Build an event from an update operation.
    pub fn from_update(schema: &str, entity: &Entity, actor: Option<&str>) -> Self {
        Self {
            event_id: uuid::Uuid::new_v4().to_string(),
            event_type: "entity.updated".to_string(),
            schema: schema.to_string(),
            entity_id: entity.id.as_str().to_string(),
            timestamp: now_iso8601(),
            actor: actor.map(String::from),
            payload: Some(entity_fields_to_json(entity)),
        }
    }

    /// Build an event from a delete operation (no payload — entity is gone).
    pub fn from_delete(schema: &str, entity_id: &str, actor: Option<&str>) -> Self {
        Self {
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
fn entity_fields_to_json(entity: &Entity) -> serde_json::Value {
    serde_json::to_value(&entity.fields).unwrap_or(serde_json::Value::Null)
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
    client: reqwest::Client,
    config: WebhookConfig,
    semaphore: Arc<tokio::sync::Semaphore>,
}

impl WebhookDispatcher {
    /// Create a new dispatcher with the given configuration.
    pub fn new(config: WebhookConfig) -> Self {
        let semaphore = Arc::new(tokio::sync::Semaphore::new(config.max_concurrent_deliveries));
        Self {
            client: reqwest::Client::new(),
            config,
            semaphore,
        }
    }

    /// Fire-and-forget: spawn a background delivery task for each subscription.
    ///
    /// Returns immediately — webhook delivery never blocks the API response.
    pub fn dispatch(&self, event: WebhookEvent, subscriptions: Vec<ResolvedSubscription>) {
        for sub in subscriptions {
            let client = self.client.clone();
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
                deliver_with_retry(&client, &event, &sub, &config).await;
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
    client: &reqwest::Client,
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
            let backoff = Duration::from_millis(500 * 2u64.pow(attempt - 1));
            tokio::time::sleep(backoff).await;
        }

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
    // Basic URL parsing without the `url` crate
    let (scheme, rest) = url
        .split_once("://")
        .ok_or(WebhookUrlError::InvalidUrl)?;

    if !allowed_schemes.iter().any(|s| s == scheme) {
        return Err(WebhookUrlError::DisallowedScheme(scheme.to_string()));
    }

    // Extract host (before any port or path)
    let host_part = rest.split('/').next().unwrap_or(rest);
    let host = host_part.split(':').next().unwrap_or(host_part);

    if host.is_empty() {
        return Err(WebhookUrlError::InvalidUrl);
    }

    if host == "localhost" {
        return Err(WebhookUrlError::PrivateIp);
    }

    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_private_ip(&ip) {
            return Err(WebhookUrlError::PrivateIp);
        }
    }

    Ok(())
}

/// Check whether an IP address is private/loopback.
fn is_private_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => v6.is_loopback(),
    }
}

/// Errors from webhook URL validation.
#[derive(Debug)]
pub enum WebhookUrlError {
    InvalidUrl,
    DisallowedScheme(String),
    PrivateIp,
}

impl std::fmt::Display for WebhookUrlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
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
