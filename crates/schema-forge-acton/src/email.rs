//! Outbound email transport for SchemaForge.
//!
//! SchemaForge has no email needs of its own beyond operational flows that
//! must reach a human out-of-band — today, user invitations (issue #71). The
//! transport is modelled as a trait object ([`EmailSender`]) so handlers stay
//! decoupled from the wire protocol: production wires [`SmtpEmailSender`]
//! (lettre over implicit-TLS SMTPS by default), while tests substitute
//! [`InMemoryEmailSender`] and assert on what would have been sent.
//!
//! TLS uses the workspace's `aws-lc-rs` rustls provider (lettre's `aws-lc-rs`
//! feature), keeping the crypto backend consistent with the rest of the
//! service and aligned with the FIPS build.
//!
//! Credentials are **never** read from a committed file: the
//! [`EmailConfig::password`] field is an `Option<String>` populated by
//! acton-service's config layering from the environment, so the secret enters
//! the process at runtime and stays out of `config.toml` and git.

use async_trait::async_trait;
use lettre::message::header::ContentType;
use lettre::message::Mailbox;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use serde::{Deserialize, Serialize};
use std::sync::Mutex;

/// `[schema_forge.email]` section of `config.toml`.
///
/// SMTP is disabled by default. Link delivery needs only a public base URL;
/// SMTP mode returns [`EmailError::NotConfigured`] when the transport is
/// disabled, allowing the endpoint to return the stored invitation link.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailConfig {
    /// Invitation delivery mode. Link mode bypasses SMTP regardless of enabled.
    #[serde(default)]
    pub delivery: EmailDelivery,

    /// SMTP switch. Link delivery does not construct or use SMTP, regardless
    /// of this value. Disabled SMTP reports a recoverable delivery failure.
    #[serde(default)]
    pub enabled: bool,

    /// SMTP relay hostname, e.g. `"mail.govcraft.ai"`.
    #[serde(default)]
    pub host: Option<String>,

    /// SMTP port. Defaults to 465 (implicit TLS / SMTPS).
    #[serde(default = "default_smtp_port")]
    pub port: u16,

    /// Transport security mode. Defaults to [`EmailTls::Implicit`] (SMTPS).
    #[serde(default)]
    pub tls: EmailTls,

    /// `From` mailbox, e.g. `"SchemaForge <noreply@govcraft.ai>"`.
    #[serde(default)]
    pub from: Option<String>,

    /// SMTP AUTH username. Omit for unauthenticated relays.
    #[serde(default)]
    pub username: Option<String>,

    /// SMTP AUTH password. **Never** place this in a committed `config.toml`;
    /// supply it through the environment so acton-service's config layering
    /// fills it at runtime. Kept `Option` precisely so the on-disk file can
    /// omit it.
    #[serde(default)]
    pub password: Option<String>,

    /// Public base URL of the deployed site, e.g. `"https://app.agency.gov"`.
    /// Used to build absolute links (such as invite-accept URLs) inside
    /// emails, where a request-relative path would be useless to the
    /// recipient.
    #[serde(default)]
    pub public_base_url: Option<String>,
}

impl Default for EmailConfig {
    fn default() -> Self {
        Self {
            delivery: EmailDelivery::default(),
            enabled: false,
            host: None,
            port: default_smtp_port(),
            tls: EmailTls::default(),
            from: None,
            username: None,
            password: None,
            public_base_url: None,
        }
    }
}

impl EmailConfig {
    /// Validate link delivery's shareable public URL. SMTP retains its existing
    /// relative-link fallback when no public URL is configured.
    pub fn validate_link_delivery(&self) -> Result<(), EmailError> {
        if self.delivery != EmailDelivery::Link {
            return Ok(());
        }
        let invalid = || {
            EmailError::InvalidConfig(
            "link delivery requires an absolute http(s) public_base_url without credentials, query, or fragment".into(),
        )
        };
        let raw = self.public_base_url.as_deref().ok_or_else(invalid)?;
        let url = reqwest::Url::parse(raw).map_err(|_| invalid())?;
        let explicit_origin = raw.split_once("://").is_some_and(|(scheme, rest)| {
            scheme.eq_ignore_ascii_case(url.scheme()) && !rest.starts_with('/')
        });
        if !explicit_origin
            || raw.trim() != raw
            || !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(invalid());
        }
        Ok(())
    }
}

/// How an invitation reaches its recipient.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EmailDelivery {
    /// Send via the configured SMTP transport.
    #[default]
    Smtp,
    /// Return a shareable accept link without sending email.
    Link,
}

fn default_smtp_port() -> u16 {
    465
}

/// SMTP transport-security mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EmailTls {
    /// Implicit TLS (SMTPS) — TLS from connect, conventionally port 465.
    #[default]
    Implicit,
    /// Opportunistic TLS via the STARTTLS command, conventionally port 587.
    StartTls,
}

/// A plain-text email ready to hand to a transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmailMessage {
    /// Recipient mailbox (`"Name <addr>"` or bare `"addr"`).
    pub to: String,
    /// Subject line.
    pub subject: String,
    /// Plain-text body.
    pub body_text: String,
}

/// Errors raised while configuring or using an [`EmailSender`].
#[derive(Debug, thiserror::Error)]
pub enum EmailError {
    /// Email delivery was requested but no usable transport is configured.
    #[error("email is not configured; set [schema_forge.email] enabled = true with host and from")]
    NotConfigured,
    /// The configured values could not produce a transport.
    #[error("invalid email configuration: {0}")]
    InvalidConfig(String),
    /// A `to`/`from` address failed to parse.
    #[error("invalid email address: {0}")]
    InvalidAddress(String),
    /// The transport failed to build or deliver the message.
    #[error("smtp transport error: {0}")]
    Transport(String),
}

/// Sends outbound email. Object-safe (`async_trait`) so it can be held as an
/// `Arc<dyn EmailSender>` extension and swapped for a fake in tests.
#[async_trait]
pub trait EmailSender: Send + Sync {
    /// Deliver `message`, resolving once the relay has accepted it.
    async fn send(&self, message: EmailMessage) -> Result<(), EmailError>;

    /// Public base URL for building absolute links inside message bodies,
    /// if one is configured.
    fn public_base_url(&self) -> Option<&str>;
}

/// Production [`EmailSender`] backed by lettre's async SMTP transport.
pub struct SmtpEmailSender {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
    public_base_url: Option<String>,
}

impl SmtpEmailSender {
    /// Build a transport from `[schema_forge.email]`.
    ///
    /// Returns [`EmailError::NotConfigured`] when `enabled = false`, and
    /// [`EmailError::InvalidConfig`] when a required field (`host`, `from`)
    /// is missing or malformed — surfacing misconfiguration at startup rather
    /// than on the first invite.
    ///
    /// `project_name` is the deployment's display name (from
    /// `[schema_forge] project_name`). When `from` is a bare address with no
    /// display name, it becomes the `From` display-name so recipients see the
    /// application (e.g. `Bob's Dog Scheduling <noreply@…>`) rather than a
    /// naked address. An operator who needs an exact `From` for deliverability
    /// can still embed a display name in `from` directly, which is respected
    /// verbatim.
    pub fn from_config(cfg: &EmailConfig, project_name: &str) -> Result<Self, EmailError> {
        if !cfg.enabled {
            return Err(EmailError::NotConfigured);
        }
        let host = cfg
            .host
            .as_deref()
            .filter(|h| !h.is_empty())
            .ok_or_else(|| EmailError::InvalidConfig("host is required".to_string()))?;
        let from_raw = cfg
            .from
            .as_deref()
            .filter(|f| !f.is_empty())
            .ok_or_else(|| EmailError::InvalidConfig("from is required".to_string()))?;
        let parsed: Mailbox = from_raw
            .parse()
            .map_err(|e| EmailError::InvalidAddress(format!("from '{from_raw}': {e}")))?;
        // Brand a bare address with the project name; respect an explicit
        // display name the operator set in `from`.
        let from = match (parsed.name.is_none(), project_name.trim().is_empty()) {
            (true, false) => Mailbox::new(Some(project_name.to_string()), parsed.email),
            _ => parsed,
        };

        let builder = match cfg.tls {
            EmailTls::Implicit => AsyncSmtpTransport::<Tokio1Executor>::relay(host),
            EmailTls::StartTls => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(host),
        }
        .map_err(|e| EmailError::InvalidConfig(e.to_string()))?
        .port(cfg.port);

        let builder = match (cfg.username.as_deref(), cfg.password.as_deref()) {
            (Some(user), Some(pass)) if !user.is_empty() => {
                builder.credentials(Credentials::new(user.to_string(), pass.to_string()))
            }
            _ => builder,
        };

        Ok(Self {
            transport: builder.build(),
            from,
            public_base_url: cfg.public_base_url.clone(),
        })
    }
}

#[async_trait]
impl EmailSender for SmtpEmailSender {
    async fn send(&self, message: EmailMessage) -> Result<(), EmailError> {
        let to: Mailbox = message
            .to
            .parse()
            .map_err(|e| EmailError::InvalidAddress(format!("to '{}': {e}", message.to)))?;
        let email = Message::builder()
            .from(self.from.clone())
            .to(to)
            .subject(message.subject)
            .header(ContentType::TEXT_PLAIN)
            .body(message.body_text)
            .map_err(|e| EmailError::Transport(e.to_string()))?;
        self.transport
            .send(email)
            .await
            .map_err(|e| EmailError::Transport(e.to_string()))?;
        Ok(())
    }

    fn public_base_url(&self) -> Option<&str> {
        self.public_base_url.as_deref()
    }
}

/// In-memory [`EmailSender`] that records messages instead of sending them.
///
/// Used by tests to assert on delivered content (e.g. that an invite-accept
/// link was produced). It never fails, so it must not stand in for a real
/// transport in production — the wiring selects [`SmtpEmailSender`] there.
#[derive(Debug, Default)]
pub struct InMemoryEmailSender {
    sent: Mutex<Vec<EmailMessage>>,
    public_base_url: Option<String>,
}

impl InMemoryEmailSender {
    /// Create a recorder with an optional base URL for link construction.
    pub fn new(public_base_url: Option<String>) -> Self {
        Self {
            sent: Mutex::new(Vec::new()),
            public_base_url,
        }
    }

    /// Snapshot of every message handed to [`EmailSender::send`] so far.
    pub fn sent(&self) -> Vec<EmailMessage> {
        self.sent
            .lock()
            .expect("email recorder mutex poisoned")
            .clone()
    }
}

#[async_trait]
impl EmailSender for InMemoryEmailSender {
    async fn send(&self, message: EmailMessage) -> Result<(), EmailError> {
        self.sent
            .lock()
            .expect("email recorder mutex poisoned")
            .push(message);
        Ok(())
    }

    fn public_base_url(&self) -> Option<&str> {
        self.public_base_url.as_deref()
    }
}

/// An [`EmailSender`] that always refuses delivery with
/// [`EmailError::NotConfigured`].
///
/// Wired when `[schema_forge.email] enabled = false` so flows that need to
/// send mail get a clear "email not configured" error from `send` rather than
/// a confusing 500 on a missing `Extension<Arc<dyn EmailSender>>`. The
/// configured `public_base_url` (if any) is still surfaced so link-building
/// code paths behave identically regardless of whether SMTP is wired.
pub struct DisabledEmailSender {
    public_base_url: Option<String>,
}

impl DisabledEmailSender {
    /// Create a disabled sender carrying an optional base URL.
    pub fn new(public_base_url: Option<String>) -> Self {
        Self { public_base_url }
    }
}

#[async_trait]
impl EmailSender for DisabledEmailSender {
    async fn send(&self, _message: EmailMessage) -> Result<(), EmailError> {
        Err(EmailError::NotConfigured)
    }

    fn public_base_url(&self) -> Option<&str> {
        self.public_base_url.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_delivery_requires_shareable_absolute_url() {
        for base in [
            None,
            Some("/local"),
            Some("https:example.gov"),
            Some("https:///example.gov"),
            Some(" https://example.gov"),
            Some("ftp://example.gov"),
            Some("https://u:p@example.gov"),
            Some("https://example.gov?query=1"),
            Some("https://example.gov#fragment"),
        ] {
            let config = EmailConfig {
                delivery: EmailDelivery::Link,
                public_base_url: base.map(str::to_string),
                ..Default::default()
            };
            assert!(config.validate_link_delivery().is_err(), "{base:?}");
        }
        let config = EmailConfig {
            delivery: EmailDelivery::Link,
            public_base_url: Some("https://example.gov/app/".into()),
            ..Default::default()
        };
        assert!(!config.enabled);
        assert!(config.validate_link_delivery().is_ok());
        assert!(EmailConfig::default().validate_link_delivery().is_ok());
    }

    #[test]
    fn delivery_deserialization_is_explicit_and_defaults_to_smtp() {
        let default: EmailConfig = toml::from_str("").unwrap();
        assert_eq!(default.delivery, EmailDelivery::Smtp);
        let link: EmailConfig = toml::from_str("delivery = 'link'").unwrap();
        assert_eq!(link.delivery, EmailDelivery::Link);
        assert!(toml::from_str::<EmailConfig>("delivery = 'unknown'").is_err());
    }

    #[test]
    fn config_defaults_to_disabled_smtps() {
        let cfg = EmailConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.port, 465);
        assert_eq!(cfg.tls, EmailTls::Implicit);
        assert!(cfg.host.is_none());
    }

    #[test]
    fn config_deserialises_without_password() {
        // Committed config omits the secret; the loader supplies it from env.
        let toml = r#"
            [schema_forge.email]
            enabled = true
            host = "mail.govcraft.ai"
            port = 465
            tls = "implicit"
            from = "SchemaForge <noreply@govcraft.ai>"
            username = "noreply@govcraft.ai"
            public_base_url = "https://app.agency.gov"
        "#;
        #[derive(Deserialize)]
        struct Wrapper {
            schema_forge: Inner,
        }
        #[derive(Deserialize)]
        struct Inner {
            email: EmailConfig,
        }
        let w: Wrapper = toml::from_str(toml).unwrap();
        let email = w.schema_forge.email;
        assert!(email.enabled);
        assert_eq!(email.host.as_deref(), Some("mail.govcraft.ai"));
        assert_eq!(email.tls, EmailTls::Implicit);
        assert!(email.password.is_none());
        assert_eq!(
            email.public_base_url.as_deref(),
            Some("https://app.agency.gov")
        );
    }

    #[test]
    fn smtp_sender_refuses_when_disabled() {
        let cfg = EmailConfig::default();
        assert!(matches!(
            SmtpEmailSender::from_config(&cfg, "SchemaForge"),
            Err(EmailError::NotConfigured)
        ));
    }

    #[test]
    fn smtp_sender_requires_host_and_from() {
        let cfg = EmailConfig {
            enabled: true,
            ..EmailConfig::default()
        };
        assert!(matches!(
            SmtpEmailSender::from_config(&cfg, "SchemaForge"),
            Err(EmailError::InvalidConfig(_))
        ));
    }

    // These build a real `AsyncSmtpTransport`, whose connection-pool `Drop`
    // requires a Tokio runtime — run them as async tests so the runtime
    // outlives the sender.
    #[tokio::test]
    async fn bare_from_address_is_branded_with_project_name() {
        let cfg = EmailConfig {
            enabled: true,
            host: Some("mail.example.gov".to_string()),
            from: Some("noreply@example.gov".to_string()),
            ..EmailConfig::default()
        };
        let sender = SmtpEmailSender::from_config(&cfg, "Bob's Dog Scheduling").unwrap();
        assert_eq!(sender.from.name.as_deref(), Some("Bob's Dog Scheduling"));
        assert_eq!(sender.from.email.to_string(), "noreply@example.gov");
    }

    #[tokio::test]
    async fn explicit_from_display_name_is_respected() {
        let cfg = EmailConfig {
            enabled: true,
            host: Some("mail.example.gov".to_string()),
            from: Some("Agency Mailer <noreply@example.gov>".to_string()),
            ..EmailConfig::default()
        };
        // Operator's explicit display name wins over the project name.
        let sender = SmtpEmailSender::from_config(&cfg, "Bob's Dog Scheduling").unwrap();
        assert_eq!(sender.from.name.as_deref(), Some("Agency Mailer"));
    }

    #[tokio::test]
    async fn in_memory_sender_records_messages() {
        let sender = InMemoryEmailSender::new(Some("https://app.agency.gov".to_string()));
        sender
            .send(EmailMessage {
                to: "user@example.gov".to_string(),
                subject: "Welcome".to_string(),
                body_text: "hello".to_string(),
            })
            .await
            .unwrap();
        let sent = sender.sent();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].to, "user@example.gov");
        assert_eq!(sender.public_base_url(), Some("https://app.agency.gov"));
    }

    #[tokio::test]
    async fn disabled_sender_fails_closed_but_keeps_base_url() {
        let sender = DisabledEmailSender::new(Some("https://app.agency.gov".to_string()));
        let res = sender
            .send(EmailMessage {
                to: "user@example.gov".to_string(),
                subject: "Welcome".to_string(),
                body_text: "hello".to_string(),
            })
            .await;
        assert!(matches!(res, Err(EmailError::NotConfigured)));
        assert_eq!(sender.public_base_url(), Some("https://app.agency.gov"));
    }
}
