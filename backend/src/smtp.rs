//! SMTP send adapter.
//!
//! Sends outgoing email via SMTP using the `lettre` crate.
//! Credentials are decrypted from the account's encrypted store
//! at send time.
//!
//! Spec alignment (RFC 5321 + extensions):
//! - AUTH: prefer PLAIN, then LOGIN; XOAUTH2 listed for CHE-26 (Outlook).
//! - 8BITMIME (RFC 6152) and SMTPUTF8 (RFC 6531): negotiated by lettre from
//!   the EHLO capability set when the envelope/body needs them.
//! - Permanent (5xx) vs transient (4xx) failures are classified for job retry.
//!
//! See `docs/specs/2026-08-20-lyra-sync-and-protocols-spec.md` §8 / §13.2.

#![allow(clippy::doc_markdown)]

use lettre::message::header::ContentType;
use lettre::message::{Mailbox, Message, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::{Credentials, Mechanism};
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::crypto::{self, EncryptedCredential};
use zeroize::Zeroizing;

/// Preferred AUTH mechanisms (order = preference).
///
/// PLAIN / LOGIN cover password and app-password accounts. XOAUTH2 is included
/// so lettre can select it when the server advertises it and credentials are
/// OAuth tokens (CHE-26); password accounts still negotiate PLAIN/LOGIN first.
pub const SMTP_AUTH_MECHANISMS: &[Mechanism] =
    &[Mechanism::Plain, Mechanism::Login, Mechanism::Xoauth2];

/// Errors specific to the SMTP adapter.
#[derive(Debug, Error)]
pub enum SmtpError {
    /// Transient SMTP failure (4xx) — safe to retry with backoff.
    #[error("SMTP transient: {0}")]
    Transient(String),
    /// Permanent SMTP failure (5xx) — do not retry as-is.
    #[error("SMTP permanent: {0}")]
    Permanent(String),
    #[error("SMTP transport error: {0}")]
    Transport(#[from] lettre::transport::smtp::Error),
    #[error("message build error: {0}")]
    MessageBuild(#[from] lettre::error::Error),
    #[error("invalid address: {0}")]
    Address(#[from] lettre::address::AddressError),
    #[error("crypto error: {0}")]
    Crypto(#[from] crypto::CryptoError),
    #[error("invalid credential: {0}")]
    Credential(String),
    #[error("configuration error: {0}")]
    #[allow(dead_code)]
    Config(String),
}

impl SmtpError {
    /// Whether a job worker should reschedule this send.
    #[must_use]
    #[allow(dead_code)] // used by jobs via job_category; kept for callers/tests
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Transient(_))
    }

    /// Fixed category string for `jobs.last_error` (never echoes server text).
    #[must_use]
    pub fn job_category(&self) -> &'static str {
        match self {
            Self::Transient(_) => "SMTP transient",
            Self::Permanent(_) => "SMTP permanent",
            Self::Credential(_) | Self::Config(_) => "SMTP error",
            Self::Transport(_) | Self::MessageBuild(_) | Self::Address(_) | Self::Crypto(_) => {
                "SMTP error"
            }
        }
    }
}

/// Map a lettre transport error to permanent / transient / opaque transport.
pub fn classify_transport_error(err: lettre::transport::smtp::Error) -> SmtpError {
    if err.is_transient() || err.is_timeout() {
        SmtpError::Transient(err.to_string())
    } else if err.is_permanent() {
        SmtpError::Permanent(err.to_string())
    } else {
        SmtpError::Transport(err)
    }
}

/// Security mode for SMTP connections.
///
/// Plaintext (`none`) is intentionally not supported: it sent credentials
/// over an unencrypted, unauthenticated connection.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SmtpSecurity {
    Tls,
    Starttls,
}

/// SMTP connection parameters.
#[derive(Debug, Clone)]
pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    pub security: SmtpSecurity,
    pub username: String,
    pub password: Zeroizing<String>,
}

/// An outbound email message to be sent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboundMessage {
    /// From address (email).
    pub from_email: String,
    /// From display name.
    pub from_name: Option<String>,
    /// To addresses.
    pub to: Vec<(Option<String>, String)>,
    /// Cc addresses.
    pub cc: Vec<(Option<String>, String)>,
    /// Bcc addresses.
    pub bcc: Vec<(Option<String>, String)>,
    /// Subject line.
    pub subject: String,
    /// Plain-text body.
    pub body_text: Option<String>,
    /// HTML body.
    pub body_html: Option<String>,
    /// In-Reply-To header for threading.
    pub in_reply_to: Option<String>,
    /// References header for threading.
    pub references: Option<String>,
}

impl OutboundMessage {
    /// True when any envelope address needs SMTPUTF8 (RFC 6531).
    #[must_use]
    #[allow(dead_code)] // capability gate for probes / future preflight
    pub fn needs_smtputf8(&self) -> bool {
        let mut addrs = std::iter::once(self.from_email.as_str())
            .chain(self.to.iter().map(|(_, e)| e.as_str()))
            .chain(self.cc.iter().map(|(_, e)| e.as_str()))
            .chain(self.bcc.iter().map(|(_, e)| e.as_str()));
        addrs.any(|a| !a.is_ascii())
    }

    /// True when body/subject content is not 7-bit ASCII (needs 8BITMIME).
    #[must_use]
    #[allow(dead_code)] // capability gate for probes / future preflight
    pub fn needs_8bitmime(&self) -> bool {
        !self.subject.is_ascii()
            || self.body_text.as_deref().is_some_and(|t| !t.is_ascii())
            || self.body_html.as_deref().is_some_and(|h| !h.is_ascii())
    }
}

/// Parsed EHLO capability flags (subset Lyra cares about).
#[allow(clippy::struct_excessive_bools, dead_code)]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EhloCapabilities {
    pub eight_bit_mime: bool,
    pub smtp_utf8: bool,
    pub starttls: bool,
    pub auth_plain: bool,
    pub auth_login: bool,
    pub auth_xoauth2: bool,
}

#[allow(dead_code)]
impl EhloCapabilities {
    /// Parse capability keywords from EHLO response lines (after the greeting).
    ///
    /// Mirrors lettre's `ServerInfo::from_response` feature set for the
    /// extensions Lyra depends on — used for unit tests and future probes.
    #[must_use]
    pub fn from_ehlo_lines<'a>(lines: impl IntoIterator<Item = &'a str>) -> Self {
        let mut caps = Self::default();
        for line in lines {
            let mut split = line.split_whitespace();
            match split.next().unwrap_or("") {
                "8BITMIME" => caps.eight_bit_mime = true,
                "SMTPUTF8" => caps.smtp_utf8 = true,
                "STARTTLS" => caps.starttls = true,
                "AUTH" => {
                    for mech in split {
                        match mech {
                            "PLAIN" => caps.auth_plain = true,
                            "LOGIN" => caps.auth_login = true,
                            "XOAUTH2" => caps.auth_xoauth2 = true,
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        caps
    }

    /// Whether this message can be sent given the server's advertised features.
    pub fn allows_message(&self, msg: &OutboundMessage) -> Result<(), &'static str> {
        if msg.needs_smtputf8() && !self.smtp_utf8 {
            return Err("envelope needs SMTPUTF8 but server did not advertise it");
        }
        if msg.needs_8bitmime() && !self.eight_bit_mime {
            return Err("message needs 8BITMIME but server did not advertise it");
        }
        if !(self.auth_plain || self.auth_login || self.auth_xoauth2) {
            return Err("server advertises no supported AUTH mechanism");
        }
        Ok(())
    }
}

/// SMTP send adapter.
///
/// Wraps `lettre`'s async SMTP transport.
pub struct SmtpAdapter {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    #[allow(dead_code)]
    config: SmtpConfig,
}

impl SmtpAdapter {
    /// Connect to an SMTP server.
    ///
    /// Handles TLS (port 465) and STARTTLS (port 587) connections.
    /// AUTH mechanisms: PLAIN → LOGIN → XOAUTH2 (server must advertise).
    pub fn connect(config: &SmtpConfig) -> Result<Self, SmtpError> {
        let creds = Credentials::new(config.username.clone(), (*config.password).clone());
        let mechanisms = SMTP_AUTH_MECHANISMS.to_vec();

        let transport = match config.security {
            SmtpSecurity::Tls => AsyncSmtpTransport::<Tokio1Executor>::relay(&config.host)?
                .credentials(creds)
                .authentication(mechanisms)
                .port(config.port)
                .build(),
            SmtpSecurity::Starttls => {
                AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&config.host)?
                    .credentials(creds)
                    .authentication(mechanisms)
                    .port(config.port)
                    .build()
            }
        };

        Ok(Self {
            transport,
            config: config.clone(),
        })
    }

    /// Send an outbound email message.
    pub async fn send(&self, msg: &OutboundMessage) -> Result<String, SmtpError> {
        let message = build_message(msg)?;

        self.transport
            .send(message)
            .await
            .map_err(classify_transport_error)?;

        // Return a placeholder message-id (lettre doesn't expose the actual one)
        Ok(format!(
            "sent-{}",
            uuid::Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext))
        ))
    }
}

/// Build a `lettre::Message` from an `OutboundMessage`.
fn build_message(msg: &OutboundMessage) -> Result<Message, SmtpError> {
    let from_mailbox = Mailbox::new(msg.from_name.clone(), msg.from_email.parse()?);

    let mut builder = Message::builder()
        .from(from_mailbox)
        .subject(msg.subject.clone());

    // Add In-Reply-To and References for threading
    if let Some(ref irt) = msg.in_reply_to {
        builder = builder.in_reply_to(irt.clone());
    }
    if let Some(ref refs) = msg.references {
        builder = builder.references(refs.clone());
    }

    // Add To recipients
    for (name, email) in &msg.to {
        let mailbox = Mailbox::new(name.clone(), email.parse()?);
        builder = builder.to(mailbox);
    }

    // Add Cc recipients
    for (name, email) in &msg.cc {
        let mailbox = Mailbox::new(name.clone(), email.parse()?);
        builder = builder.cc(mailbox);
    }

    // Add Bcc recipients
    for (name, email) in &msg.bcc {
        let mailbox = Mailbox::new(name.clone(), email.parse()?);
        builder = builder.bcc(mailbox);
    }

    // Build body: prefer multipart (text + html), fallback to single
    let message = match (&msg.body_text, &msg.body_html) {
        (Some(text), Some(html)) => {
            let text_part = SinglePart::builder()
                .header(ContentType::TEXT_PLAIN)
                .body(text.clone());

            let html_part = SinglePart::builder()
                .header(ContentType::TEXT_HTML)
                .body(html.clone());

            builder.multipart(
                MultiPart::alternative()
                    .singlepart(text_part)
                    .singlepart(html_part),
            )?
        }
        (Some(text), None) => builder.header(ContentType::TEXT_PLAIN).body(text.clone())?,
        (None, Some(html)) => builder.header(ContentType::TEXT_HTML).body(html.clone())?,
        (None, None) => builder
            .header(ContentType::TEXT_PLAIN)
            .body(String::new())?,
    };

    Ok(message)
}

/// Decrypt the stored credential for SMTP authentication.
pub fn decrypt_account_password(credential_json: &str, dek: &[u8]) -> Result<String, SmtpError> {
    let encrypted: EncryptedCredential = serde_json::from_str(credential_json)
        .map_err(|e| SmtpError::Credential(format!("invalid credential blob: {e}")))?;

    let plaintext = crypto::decrypt(dek, &encrypted)?;

    String::from_utf8(plaintext)
        .map_err(|e| SmtpError::Credential(format!("credential not valid UTF-8: {e}")))
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smtp_config_password_is_zeroizing() {
        let cfg = SmtpConfig {
            host: "smtp.example.com".into(),
            port: 465,
            security: SmtpSecurity::Tls,
            username: "user".into(),
            password: Zeroizing::new("secret".into()),
        };
        assert_eq!(&*cfg.password, "secret");
    }

    #[test]
    fn decrypt_roundtrip() {
        let key = crypto::generate_key();
        let password = "smtp-test-password";
        let encrypted = crypto::encrypt(&key, password.as_bytes()).unwrap();
        let json = serde_json::to_string(&encrypted).unwrap();
        let decrypted = decrypt_account_password(&json, &key).unwrap();
        assert_eq!(decrypted, password);
    }

    #[test]
    fn auth_mechanisms_prefer_plain_then_login_then_xoauth2() {
        assert_eq!(
            SMTP_AUTH_MECHANISMS,
            &[Mechanism::Plain, Mechanism::Login, Mechanism::Xoauth2]
        );
    }

    #[test]
    fn ehlo_parses_full_capability_matrix() {
        let caps = EhloCapabilities::from_ehlo_lines([
            "mail.example.com",
            "8BITMIME",
            "SMTPUTF8",
            "STARTTLS",
            "AUTH PLAIN LOGIN XOAUTH2",
            "SIZE 52428800",
        ]);
        assert!(caps.eight_bit_mime);
        assert!(caps.smtp_utf8);
        assert!(caps.starttls);
        assert!(caps.auth_plain);
        assert!(caps.auth_login);
        assert!(caps.auth_xoauth2);
    }

    #[test]
    fn ehlo_minimal_auth_plain_only() {
        let caps = EhloCapabilities::from_ehlo_lines(["smtp.example.com", "AUTH PLAIN"]);
        assert!(!caps.eight_bit_mime);
        assert!(!caps.smtp_utf8);
        assert!(caps.auth_plain);
        assert!(!caps.auth_login);
        assert!(!caps.auth_xoauth2);
    }

    #[test]
    fn ehlo_allows_ascii_message_without_8bitmime() {
        let caps = EhloCapabilities::from_ehlo_lines(["smtp.example.com", "AUTH PLAIN"]);
        let msg = OutboundMessage {
            from_email: "a@example.com".into(),
            from_name: None,
            to: vec![(None, "b@example.com".into())],
            cc: vec![],
            bcc: vec![],
            subject: "Hello".into(),
            body_text: Some("plain".into()),
            body_html: None,
            in_reply_to: None,
            references: None,
        };
        assert!(caps.allows_message(&msg).is_ok());
    }

    #[test]
    fn ehlo_rejects_utf8_envelope_without_smtputf8() {
        let caps = EhloCapabilities::from_ehlo_lines(["smtp.example.com", "AUTH PLAIN", "8BITMIME"]);
        let msg = OutboundMessage {
            from_email: "用户@例子.测试".into(),
            from_name: None,
            to: vec![(None, "b@example.com".into())],
            cc: vec![],
            bcc: vec![],
            subject: "Hi".into(),
            body_text: Some("plain".into()),
            body_html: None,
            in_reply_to: None,
            references: None,
        };
        assert!(msg.needs_smtputf8());
        assert!(caps.allows_message(&msg).is_err());
    }

    #[test]
    fn ehlo_rejects_8bit_body_without_8bitmime() {
        let caps = EhloCapabilities::from_ehlo_lines(["smtp.example.com", "AUTH LOGIN"]);
        let msg = OutboundMessage {
            from_email: "a@example.com".into(),
            from_name: None,
            to: vec![(None, "b@example.com".into())],
            cc: vec![],
            bcc: vec![],
            subject: "你好".into(),
            body_text: Some("世界".into()),
            body_html: None,
            in_reply_to: None,
            references: None,
        };
        assert!(msg.needs_8bitmime());
        assert!(caps.allows_message(&msg).is_err());
    }

    #[test]
    fn smtp_error_categories_are_safe_for_jobs() {
        let t = SmtpError::Transient("421 4.7.0 Try again later token=secret".into());
        let p = SmtpError::Permanent("550 5.1.1 User unknown token=secret".into());
        assert!(t.is_retryable());
        assert!(!p.is_retryable());
        assert_eq!(t.job_category(), "SMTP transient");
        assert_eq!(p.job_category(), "SMTP permanent");
        // Display may contain server text; job category must not.
        assert!(!t.job_category().contains("secret"));
        assert!(!p.job_category().contains("secret"));
    }

    #[test]
    fn build_message_text_only() {
        let msg = OutboundMessage {
            from_email: "sender@example.com".into(),
            from_name: Some("Sender".into()),
            to: vec![(Some("Recipient".into()), "recipient@example.com".into())],
            cc: vec![],
            bcc: vec![],
            subject: "Test Subject".into(),
            body_text: Some("Hello world".into()),
            body_html: None,
            in_reply_to: None,
            references: None,
        };

        let message = build_message(&msg).unwrap();
        assert_eq!(message.envelope().to().len(), 1);
    }

    #[test]
    fn build_message_html_only() {
        let msg = OutboundMessage {
            from_email: "sender@example.com".into(),
            from_name: None,
            to: vec![(None, "recipient@example.com".into())],
            cc: vec![],
            bcc: vec![],
            subject: "HTML Test".into(),
            body_text: None,
            body_html: Some("<p>Hello</p>".into()),
            in_reply_to: None,
            references: None,
        };

        let message = build_message(&msg).unwrap();
        assert_eq!(message.envelope().to().len(), 1);
    }

    #[test]
    fn build_message_multipart() {
        let msg = OutboundMessage {
            from_email: "sender@example.com".into(),
            from_name: Some("Sender".into()),
            to: vec![
                (Some("Alice".into()), "alice@example.com".into()),
                (None, "bob@example.com".into()),
            ],
            cc: vec![(None, "cc@example.com".into())],
            bcc: vec![],
            subject: "Multipart Test".into(),
            body_text: Some("Plain text".into()),
            body_html: Some("<p>HTML</p>".into()),
            in_reply_to: Some("<original@example.com>".into()),
            references: Some("<original@example.com>".into()),
        };

        let message = build_message(&msg).unwrap();
        assert_eq!(message.envelope().to().len(), 3); // 2 to + 1 cc
    }

    #[test]
    fn build_message_with_threading() {
        let msg = OutboundMessage {
            from_email: "sender@example.com".into(),
            from_name: None,
            to: vec![(None, "recipient@example.com".into())],
            cc: vec![],
            bcc: vec![],
            subject: "Re: Original".into(),
            body_text: Some("Reply".into()),
            body_html: None,
            in_reply_to: Some("<original-msg-id@mail.example.com>".into()),
            references: Some(
                "<thread-root@mail.example.com> <original-msg-id@mail.example.com>".into(),
            ),
        };

        let message = build_message(&msg).unwrap();
        assert_eq!(message.envelope().to().len(), 1);
    }
}
