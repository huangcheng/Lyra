//! SMTP send adapter.
//!
//! Sends outgoing email via SMTP using the `lettre` crate.
//! Credentials are decrypted from the account's encrypted store
//! at send time.
//!
//! See `docs/specs/2026-08-20-lyra-sync-and-protocols-spec.md` §8.

#![allow(clippy::doc_markdown)]

use lettre::message::header::ContentType;
use lettre::message::{Mailbox, Message, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::crypto::{self, EncryptedCredential};

/// Errors specific to the SMTP adapter.
#[derive(Debug, Error)]
pub enum SmtpError {
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
    pub password: String,
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
    pub fn connect(config: &SmtpConfig) -> Result<Self, SmtpError> {
        let creds = Credentials::new(config.username.clone(), config.password.clone());

        let transport = match config.security {
            SmtpSecurity::Tls => AsyncSmtpTransport::<Tokio1Executor>::relay(&config.host)?
                .credentials(creds)
                .port(config.port)
                .build(),
            SmtpSecurity::Starttls => {
                AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&config.host)?
                    .credentials(creds)
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
            .map_err(SmtpError::Transport)?;

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
    fn decrypt_roundtrip() {
        let key = crypto::generate_key();
        let password = "smtp-test-password";
        let encrypted = crypto::encrypt(&key, password.as_bytes()).unwrap();
        let json = serde_json::to_string(&encrypted).unwrap();
        let decrypted = decrypt_account_password(&json, &key).unwrap();
        assert_eq!(decrypted, password);
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
        // Successfully built — lettre validates addresses
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
        // Successfully built with threading headers
        assert_eq!(message.envelope().to().len(), 1);
    }
}
