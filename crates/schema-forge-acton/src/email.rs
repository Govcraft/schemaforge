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
/// Disabled by default: a deployment that never sends mail carries no SMTP
/// configuration and the invite endpoints that require delivery fail closed
/// with [`EmailError::NotConfigured`] rather than silently dropping messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailConfig {
    /// Master switch. When `false`, no transport is constructed and any flow
    /// that needs to send mail is refused with a clear error.
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
    pub fn from_config(cfg: &EmailConfig) -> Result<Self, EmailError> {
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
        let from: Mailbox = from_raw
            .parse()
            .map_err(|e| EmailError::InvalidAddress(format!("from '{from_raw}': {e}")))?;

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
        self.sent.lock().expect("email recorder mutex poisoned").clone()
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(email.public_base_url.as_deref(), Some("https://app.agency.gov"));
    }

    #[test]
    fn smtp_sender_refuses_when_disabled() {
        let cfg = EmailConfig::default();
        assert!(matches!(
            SmtpEmailSender::from_config(&cfg),
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
            SmtpEmailSender::from_config(&cfg),
            Err(EmailError::InvalidConfig(_))
        ));
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
}
