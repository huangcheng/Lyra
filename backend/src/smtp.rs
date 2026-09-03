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
use lettre::message::{Attachment, Mailbox, Message, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::{Credentials, Mechanism};
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::crypto;
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
    #[allow(dead_code)] // reserved; mail secrets resolve via oauth::resolve_mail_access_secret
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
    /// Prefer SASL XOAUTH2 (password field holds the access token).
    pub xoauth2: bool,
}

/// An outgoing attachment carried on [`OutboundMessage`].
///
/// `data_base64` keeps the plugin/job JSON wire self-contained; SMTP builds
/// decode it back to bytes before MIME assembly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboundAttachment {
    pub filename: String,
    pub content_type: String,
    pub data_base64: String,
    /// RFC 2045 Content-ID (without angle brackets); `Some` = inline part
    /// referenced as `cid:` from the HTML body (RFC 2392).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_id: Option<String>,
}

impl OutboundAttachment {
    #[must_use]
    pub fn from_bytes(filename: &str, content_type: &str, data: &[u8]) -> Self {
        use base64::Engine as _;
        Self {
            filename: filename.to_owned(),
            content_type: content_type.to_owned(),
            data_base64: base64::engine::general_purpose::STANDARD.encode(data),
            content_id: None,
        }
    }

    /// Inline part constructor: bytes + the Content-ID the HTML references.
    #[must_use]
    pub fn from_bytes_inline(
        filename: &str,
        content_type: &str,
        data: &[u8],
        content_id: &str,
    ) -> Self {
        let mut att = Self::from_bytes(filename, content_type, data);
        att.content_id = Some(content_id.to_owned());
        att
    }

    /// Decode the payload; an invalid base64 string is a programming error on
    /// the producer side, so it maps to a permanent SMTP error.
    pub fn decode(&self) -> Result<Vec<u8>, SmtpError> {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD
            .decode(self.data_base64.as_bytes())
            .map_err(|e| SmtpError::Permanent(format!("attachment {}: {e}", self.filename)))
    }
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
    /// RFC 3156 / OpenGPG MIME wrapper (replaces body_text/body_html when set).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_content_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_body: Option<String>,
    /// Attachments; non-empty forces a `multipart/mixed` wrapper around the
    /// body part (RFC 2046).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<OutboundAttachment>,
    /// RFC 5322 Message-ID to stamp (drafts set this so a later sync can
    /// locate the appended copy).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
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
        let mechanisms = if config.xoauth2 {
            vec![Mechanism::Xoauth2]
        } else {
            SMTP_AUTH_MECHANISMS.to_vec()
        };

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

/// The message body as a MIME part: single part or `multipart/alternative`.
enum BodyPart {
    Single(SinglePart),
    Alternative(MultiPart),
}

/// Any MIME level: a single leaf part or a multipart container.
enum Part {
    Single(SinglePart),
    Multi(MultiPart),
}

/// Build the body part: OpenGPG MIME wrapper, multipart text+html, or a
/// single text/html part.
fn build_body_part(msg: &OutboundMessage) -> Result<BodyPart, SmtpError> {
    if let (Some(ct), Some(body)) = (&msg.mime_content_type, &msg.mime_body) {
        let content_type = ContentType::parse(ct)
            .map_err(|e| SmtpError::Permanent(format!("invalid Content-Type: {e}")))?;
        return Ok(BodyPart::Single(
            SinglePart::builder()
                .header(content_type)
                .body(body.clone()),
        ));
    }
    Ok(match (&msg.body_text, &msg.body_html) {
        (Some(text), Some(html)) => {
            let text_part = SinglePart::builder()
                .header(ContentType::TEXT_PLAIN)
                .body(text.clone());

            let html_part = SinglePart::builder()
                .header(ContentType::TEXT_HTML)
                .body(html.clone());

            BodyPart::Alternative(
                MultiPart::alternative()
                    .singlepart(text_part)
                    .singlepart(html_part),
            )
        }
        (Some(text), None) => BodyPart::Single(
            SinglePart::builder()
                .header(ContentType::TEXT_PLAIN)
                .body(text.clone()),
        ),
        (None, Some(html)) => BodyPart::Single(
            SinglePart::builder()
                .header(ContentType::TEXT_HTML)
                .body(html.clone()),
        ),
        (None, None) => BodyPart::Single(
            SinglePart::builder()
                .header(ContentType::TEXT_PLAIN)
                .body(String::new()),
        ),
    })
}

/// Content-IDs become a header value: printable ASCII, no whitespace, no
/// angle brackets (build_message adds them). Anything else is rejected.
pub(crate) fn validate_content_id(cid: &str) -> Result<(), SmtpError> {
    let ok = !cid.is_empty()
        && cid
            .chars()
            .all(|c| c.is_ascii_graphic() && c != '<' && c != '>');
    if ok {
        Ok(())
    } else {
        Err(SmtpError::Permanent(format!("invalid content-id: {cid:?}")))
    }
}

/// Attachment → lettre part (base64 body, `attachment` disposition).
fn attachment_part(att: &OutboundAttachment) -> Result<SinglePart, SmtpError> {
    let bytes = att.decode()?;
    let content_type = ContentType::parse(&att.content_type).unwrap_or_else(|_| {
        ContentType::parse("application/octet-stream").expect("static MIME type parses")
    });
    Ok(Attachment::new(att.filename.clone()).body(bytes, content_type))
}

/// Inline attachment → lettre part: `inline` disposition (RFC 2183) +
/// `Content-ID` (RFC 2045) so the HTML body's `cid:` refs (RFC 2392) resolve.
fn inline_part(att: &OutboundAttachment) -> Result<SinglePart, SmtpError> {
    let cid = att.content_id.as_deref().ok_or_else(|| {
        SmtpError::Permanent(format!(
            "inline attachment {}: missing content_id",
            att.filename
        ))
    })?;
    validate_content_id(cid)?;
    let bytes = att.decode()?;
    let content_type = ContentType::parse(&att.content_type).unwrap_or_else(|_| {
        ContentType::parse("application/octet-stream").expect("static MIME type parses")
    });
    Ok(
        Attachment::new_inline_with_name(cid.to_owned(), att.filename.clone())
            .body(bytes, content_type),
    )
}

/// Build a `lettre::Message` from an `OutboundMessage`.
pub(crate) fn build_message(msg: &OutboundMessage) -> Result<Message, SmtpError> {
    let from_mailbox = Mailbox::new(msg.from_name.clone(), msg.from_email.parse()?);

    let mut builder = Message::builder()
        .from(from_mailbox)
        .subject(msg.subject.clone());

    if let Some(ref mid) = msg.message_id {
        builder = builder.message_id(Some(mid.clone()));
    }

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

    let body_part = build_body_part(msg)?;
    // OpenGPG replaces the body with a crypto wrapper (mime_body); inline
    // parts would leak outside the envelope, so they degrade to attachments.
    let inline_allowed = msg.mime_body.is_none();
    let (inline, regular): (Vec<&OutboundAttachment>, Vec<&OutboundAttachment>) = msg
        .attachments
        .iter()
        .partition(|a| inline_allowed && a.content_id.is_some());

    // RFC 2387 multipart/related: body + inline parts, only when present.
    let body_level: Part = if inline.is_empty() {
        match body_part {
            BodyPart::Single(sp) => Part::Single(sp),
            BodyPart::Alternative(mp) => Part::Multi(mp),
        }
    } else {
        let mut related = match body_part {
            BodyPart::Single(sp) => MultiPart::related().singlepart(sp),
            BodyPart::Alternative(mp) => MultiPart::related().multipart(mp),
        };
        for att in inline {
            related = related.singlepart(inline_part(att)?);
        }
        Part::Multi(related)
    };

    // RFC 2046 multipart/mixed: body level first, then regular attachments.
    let message = match (body_level, regular.is_empty()) {
        (Part::Single(sp), true) => builder.singlepart(sp)?,
        (Part::Multi(mp), true) => builder.multipart(mp)?,
        (level, false) => {
            let mut mixed = match level {
                Part::Single(sp) => MultiPart::mixed().singlepart(sp),
                Part::Multi(mp) => MultiPart::mixed().multipart(mp),
            };
            for att in regular {
                mixed = mixed.singlepart(attachment_part(att)?);
            }
            builder.multipart(mixed)?
        }
    };

    Ok(message)
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
            xoauth2: false,
        };
        assert_eq!(&*cfg.password, "secret");
    }

    #[test]
    fn decrypt_roundtrip() {
        let key = crypto::generate_key();
        let password = "smtp-test-password";
        let encrypted = crypto::encrypt(&key, password.as_bytes()).unwrap();
        let plaintext = crypto::decrypt(&key, &encrypted).unwrap();
        assert_eq!(String::from_utf8(plaintext).unwrap(), password);
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
            mime_content_type: None,
            mime_body: None,
            attachments: Vec::new(),
            message_id: None,
        };
        assert!(caps.allows_message(&msg).is_ok());
    }

    #[test]
    fn ehlo_rejects_utf8_envelope_without_smtputf8() {
        let caps =
            EhloCapabilities::from_ehlo_lines(["smtp.example.com", "AUTH PLAIN", "8BITMIME"]);
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
            mime_content_type: None,
            mime_body: None,
            attachments: Vec::new(),
            message_id: None,
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
            mime_content_type: None,
            mime_body: None,
            attachments: Vec::new(),
            message_id: None,
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
            mime_content_type: None,
            mime_body: None,
            attachments: Vec::new(),
            message_id: None,
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
            mime_content_type: None,
            mime_body: None,
            attachments: Vec::new(),
            message_id: None,
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
            mime_content_type: None,
            mime_body: None,
            attachments: Vec::new(),
            message_id: None,
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
            mime_content_type: None,
            mime_body: None,
            attachments: Vec::new(),
            message_id: None,
        };

        let message = build_message(&msg).unwrap();
        assert_eq!(message.envelope().to().len(), 1);
    }

    #[test]
    fn build_message_with_attachments_is_multipart_mixed() {
        let att = OutboundAttachment::from_bytes("notes.txt", "text/plain", b"hello attachment");
        let msg = OutboundMessage {
            from_email: "sender@example.com".into(),
            from_name: None,
            to: vec![(None, "recipient@example.com".into())],
            cc: vec![],
            bcc: vec![],
            subject: "With attachment".into(),
            body_text: Some("Plain".into()),
            body_html: Some("<p>HTML</p>".into()),
            in_reply_to: None,
            references: None,
            mime_content_type: None,
            mime_body: None,
            attachments: vec![att],
            message_id: None,
        };

        let message = build_message(&msg).unwrap();
        let raw = String::from_utf8_lossy(&message.formatted()).into_owned();
        assert!(raw.contains("multipart/mixed"), "outer wrapper missing");
        assert!(
            raw.contains("multipart/alternative"),
            "inner body alternative missing"
        );
        assert!(raw.contains("filename=\"notes.txt\""));
        // lettre picks the transfer encoding (QP for ASCII text, base64 for
        // binary); assert the payload itself made it into the part.
        assert!(raw.contains("hello attachment"));
    }

    #[test]
    fn build_message_openpgpg_mime_with_attachments_keeps_wrapper() {
        let att =
            OutboundAttachment::from_bytes("data.bin", "application/octet-stream", &[1, 2, 3]);
        let msg = OutboundMessage {
            from_email: "sender@example.com".into(),
            from_name: None,
            to: vec![(None, "recipient@example.com".into())],
            cc: vec![],
            bcc: vec![],
            subject: "Signed + attachment".into(),
            body_text: None,
            body_html: None,
            in_reply_to: None,
            references: None,
            mime_content_type: Some(
                "multipart/signed; protocol=\"application/pgp-signature\"".into(),
            ),
            mime_body: Some("signed payload".into()),
            attachments: vec![att],
            message_id: None,
        };

        let message = build_message(&msg).unwrap();
        let raw = String::from_utf8_lossy(&message.formatted()).into_owned();
        assert!(raw.contains("multipart/mixed"));
        assert!(raw.contains("application/pgp-signature"));
        assert!(raw.contains("filename=\"data.bin\""));
    }

    #[test]
    fn outbound_attachment_base64_roundtrip() {
        let att = OutboundAttachment::from_bytes("f", "application/pdf", b"%PDF-1.7");
        assert_eq!(att.decode().unwrap(), b"%PDF-1.7");
    }

    fn inline_att(name: &str, cid: &str) -> OutboundAttachment {
        OutboundAttachment::from_bytes_inline(name, "image/png", b"\x89PNG", cid)
    }

    fn base_msg() -> OutboundMessage {
        OutboundMessage {
            from_email: "a@example.com".into(),
            from_name: None,
            to: vec![(None, "b@example.com".into())],
            cc: vec![],
            bcc: vec![],
            subject: "s".into(),
            body_text: Some("hi".into()),
            body_html: Some("<p>hi</p><img src=\"cid:img1@lyra\">".into()),
            in_reply_to: None,
            references: None,
            mime_content_type: None,
            mime_body: None,
            attachments: vec![],
            message_id: None,
        }
    }

    fn formatted(msg: &OutboundMessage) -> String {
        String::from_utf8(build_message(msg).unwrap().formatted()).unwrap()
    }

    #[test]
    fn inline_attachment_produces_multipart_related() {
        let mut msg = base_msg();
        msg.attachments.push(inline_att("a.png", "img1@lyra"));
        let raw = formatted(&msg);
        assert!(raw.contains("multipart/related"), "{raw}");
        assert!(raw.contains("Content-ID: <img1@lyra>"), "{raw}");
        assert!(raw.contains("Content-Disposition: inline"), "{raw}");
        assert!(!raw.contains("multipart/mixed"), "{raw}");
        // related wraps the alternative body: html appears before the image part
        let html_pos = raw.find("text/html").unwrap();
        let img_pos = raw.find("Content-ID: <img1@lyra>").unwrap();
        assert!(html_pos < img_pos, "{raw}");
    }

    #[test]
    fn inline_plus_file_produces_mixed_wrapping_related() {
        let mut msg = base_msg();
        msg.attachments.push(inline_att("a.png", "img1@lyra"));
        msg.attachments.push(OutboundAttachment::from_bytes(
            "doc.pdf",
            "application/pdf",
            b"PDF",
        ));
        let raw = formatted(&msg);
        assert!(raw.contains("multipart/mixed"), "{raw}");
        assert!(raw.contains("multipart/related"), "{raw}");
        assert!(raw.contains("Content-Disposition: inline"), "{raw}");
        assert!(raw.contains("filename=\"doc.pdf\""), "{raw}");
        let mixed_pos = raw.find("multipart/mixed").unwrap();
        let related_pos = raw.find("multipart/related").unwrap();
        assert!(mixed_pos < related_pos, "{raw}");
    }

    #[test]
    fn regular_attachments_only_keep_todays_structure() {
        let mut msg = base_msg();
        msg.attachments.push(OutboundAttachment::from_bytes(
            "doc.pdf",
            "application/pdf",
            b"PDF",
        ));
        let raw = formatted(&msg);
        assert!(raw.contains("multipart/mixed"), "{raw}");
        assert!(!raw.contains("multipart/related"), "{raw}");
        assert!(!raw.contains("Content-Disposition: inline"), "{raw}");
    }

    #[test]
    fn no_attachments_no_inline_markers() {
        let raw = formatted(&base_msg());
        assert!(!raw.contains("multipart/related"), "{raw}");
        assert!(!raw.contains("multipart/mixed"), "{raw}");
        assert!(raw.contains("multipart/alternative"), "{raw}");
    }

    #[test]
    fn invalid_content_id_is_rejected() {
        let mut msg = base_msg();
        msg.attachments
            .push(inline_att("a.png", "bad>\nbcc:evil@example.com"));
        assert!(build_message(&msg).is_err());
    }

    #[test]
    fn opengpg_wrapper_downgrades_inline_to_regular_attachment() {
        let mut msg = base_msg();
        msg.mime_content_type =
            Some("multipart/encrypted; protocol=\"application/pgp-encrypted\"".into());
        msg.mime_body = Some("wrapped".into());
        msg.body_text = None;
        msg.body_html = None;
        msg.attachments.push(inline_att("a.png", "img1@lyra"));
        let raw = formatted(&msg);
        assert!(!raw.contains("multipart/related"), "{raw}");
        assert!(!raw.contains("Content-Disposition: inline"), "{raw}");
        assert!(raw.contains("multipart/encrypted"), "{raw}");
    }
}
