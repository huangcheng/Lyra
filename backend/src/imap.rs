//! IMAP protocol adapter.
//!
//! Wraps `async-imap` to provide connect/login/list/select/fetch operations.
//! The adapter hides all protocol details from the sync engine.
//!
//! See `docs/specs/2026-08-20-lyra-sync-and-protocols-spec.md`.

#![allow(clippy::doc_markdown)]

use async_imap::Session;
use futures_util::TryStreamExt;
use imap_proto::types::Address;
use serde::Deserialize;
use thiserror::Error;
use tokio::net::TcpStream;
use tokio_native_tls::TlsStream;

use crate::crypto::{self, EncryptedCredential};
use crate::sanitize::sanitize_email_html;
use zeroize::Zeroizing;

/// Errors specific to the IMAP adapter.
#[derive(Debug, Error)]
pub enum ImapError {
    #[error("connection failed: {0}")]
    Connection(String),
    #[error("TLS error: {0}")]
    Tls(String),
    #[error("login failed: {0}")]
    Login(String),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("crypto error: {0}")]
    Crypto(#[from] crypto::CryptoError),
    #[error("imap error: {0}")]
    Imap(#[from] async_imap::error::Error),
}

/// Security mode for IMAP connections.
///
/// Plaintext (`none`) is intentionally not supported: it sent credentials
/// over an unencrypted, unauthenticated connection.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImapSecurity {
    Tls,
    Starttls,
}

/// Connection parameters for an IMAP server.
#[derive(Debug, Clone)]
pub struct ImapConfig {
    pub host: String,
    pub port: u16,
    pub security: ImapSecurity,
    pub username: String,
    pub password: Zeroizing<String>,
}

/// Metadata for a single IMAP folder (mailbox).
#[derive(Debug, Clone)]
pub struct ImapFolder {
    /// Server-assigned folder name (e.g. "INBOX", "Sent").
    pub name: String,
    /// IMAP folder delimiter (usually "/" or ".").
    pub delimiter: Option<String>,
    /// Folder attributes from LIST response.
    #[allow(dead_code)]
    pub attributes: Vec<String>,
}

/// Parsed attachment extracted from a MIME part.
#[derive(Debug, Clone)]
pub struct ExtractedAttachment {
    pub filename: String,
    pub content_type: String,
    pub data: Vec<u8>,
    pub content_id: Option<String>,
    pub is_inline: bool,
}

/// Parsed envelope for a fetched message.
#[derive(Debug, Clone)]
pub struct ImapMessage {
    /// IMAP UID (unique within the folder + `uid_validity`).
    pub uid: u32,
    /// RFC 5322 `Message-ID` header.
    pub message_id: Option<String>,
    /// Subject header.
    pub subject: Option<String>,
    /// From header (raw).
    pub from: Option<String>,
    /// To header (raw).
    pub to: Option<String>,
    /// Cc header (raw).
    pub cc: Option<String>,
    /// Date header.
    pub date: Option<String>,
    /// In-Reply-To header.
    pub in_reply_to: Option<String>,
    /// References header.
    pub references: Option<String>,
    /// IMAP flags (`\Seen`, `\Flagged`, etc.).
    pub flags: Vec<String>,
    /// RFC822.SIZE if available.
    pub size: Option<u32>,
    /// Full raw body (when fetched).
    #[allow(dead_code)]
    pub body: Option<Vec<u8>>,
    /// Plain-text body extracted from raw MIME.
    pub body_text: Option<String>,
    /// HTML body extracted from raw MIME.
    pub body_html: Option<String>,
    /// Whether message has attachments (heuristic from MIME structure).
    pub has_attachments: bool,
    /// Extracted attachment parts (populated when bodies are fetched).
    pub attachments: Vec<ExtractedAttachment>,
}

/// An authenticated IMAP session.
///
/// Wraps `async_imap::Session<TlsStream<TcpStream>>` and provides
/// high-level operations for the sync engine.
pub struct ImapClient {
    session: Session<TlsStream<TcpStream>>,
}

impl ImapClient {
    /// Connect to an IMAP server and authenticate.
    ///
    /// Handles TLS and STARTTLS connections.
    pub async fn connect(config: &ImapConfig) -> Result<Self, ImapError> {
        let addr = format!("{}:{}", config.host, config.port);

        match config.security {
            ImapSecurity::Tls => {
                let tls = tokio_native_tls::TlsConnector::from(
                    native_tls::TlsConnector::builder()
                        .build()
                        .map_err(|e| ImapError::Tls(e.to_string()))?,
                );

                let tcp = TcpStream::connect(&addr)
                    .await
                    .map_err(|e| ImapError::Connection(e.to_string()))?;

                let tls_stream = tls
                    .connect(&config.host, tcp)
                    .await
                    .map_err(|e| ImapError::Tls(e.to_string()))?;

                let client = async_imap::Client::new(tls_stream);
                let session = client
                    .login(&config.username, &config.password)
                    .await
                    .map_err(|(e, _)| ImapError::Login(e.to_string()))?;

                Ok(Self { session })
            }
            ImapSecurity::Starttls => {
                // Connect in plain text, then upgrade via STARTTLS
                let tcp = TcpStream::connect(&addr)
                    .await
                    .map_err(|e| ImapError::Connection(e.to_string()))?;

                let tls_connector = native_tls::TlsConnector::builder()
                    .build()
                    .map_err(|e| ImapError::Tls(e.to_string()))?;

                let mut client = async_imap::Client::new(tcp);

                // Run STARTTLS command
                client
                    .run_command_and_check_ok("STARTTLS", None)
                    .await
                    .map_err(|e| ImapError::Tls(format!("STARTTLS failed: {e}")))?;

                // Upgrade the connection to TLS
                let inner = client.into_inner();
                let tokio_tls = tokio_native_tls::TlsConnector::from(tls_connector);
                let tls_stream = tokio_tls
                    .connect(&config.host, inner)
                    .await
                    .map_err(|e| ImapError::Tls(e.to_string()))?;

                let tls_client = async_imap::Client::new(tls_stream);
                let session = tls_client
                    .login(&config.username, &config.password)
                    .await
                    .map_err(|(e, _)| ImapError::Login(e.to_string()))?;

                Ok(Self { session })
            }
        }
    }

    /// List all folders (mailboxes) on the server.
    pub async fn list_folders(&mut self) -> Result<Vec<ImapFolder>, ImapError> {
        let listing = self
            .session
            .list(Some(""), Some("*"))
            .await
            .map_err(ImapError::Imap)?;

        // Collect the stream into a Vec
        let items: Vec<_> = listing.try_collect().await.map_err(ImapError::Imap)?;

        let mut folders = Vec::new();
        for item in &items {
            folders.push(ImapFolder {
                name: item.name().to_string(),
                delimiter: item.delimiter().map(String::from),
                attributes: item.attributes().iter().map(|a| format!("{a:?}")).collect(),
            });
        }

        Ok(folders)
    }

    /// Select a folder (mailbox) for subsequent operations.
    ///
    /// Returns `(uid_validity, uid_next, exists)`.
    pub async fn select(&mut self, folder_name: &str) -> Result<(u32, u32, u32), ImapError> {
        let mailbox = self
            .session
            .select(folder_name)
            .await
            .map_err(ImapError::Imap)?;

        let uid_validity = mailbox.uid_validity.unwrap_or(0);
        let uid_next = mailbox.uid_next.unwrap_or(0);
        let exists = mailbox.exists;

        Ok((uid_validity, uid_next, exists))
    }

    /// Fetch message UIDs optionally after a given UID.
    ///
    /// Uses UID SEARCH to find messages.
    pub async fn search_uids(&mut self, after_uid: Option<u32>) -> Result<Vec<u32>, ImapError> {
        let query = match after_uid {
            Some(uid) => format!("{}:*", uid + 1),
            None => "1:*".to_string(),
        };

        let uids = self
            .session
            .uid_search(&query)
            .await
            .map_err(ImapError::Imap)?;

        Ok(uids.into_iter().collect())
    }

    /// Fetch message metadata (envelope + flags + size) for the given UIDs.
    ///
    /// Does **not** fetch message bodies — use `fetch_bodies` for that.
    pub async fn fetch_metadata(&mut self, uids: &[u32]) -> Result<Vec<ImapMessage>, ImapError> {
        if uids.is_empty() {
            return Ok(Vec::new());
        }

        let uid_set = format_uid_set(uids);
        let fetch_items = "UID FLAGS RFC822.SIZE ENVELOPE";

        let stream = self
            .session
            .uid_fetch(&uid_set, fetch_items)
            .await
            .map_err(ImapError::Imap)?;

        let fetches: Vec<_> = stream.try_collect().await.map_err(ImapError::Imap)?;

        let mut result = Vec::new();
        for msg in &fetches {
            result.push(parse_fetch_to_message(msg, false));
        }

        Ok(result)
    }

    /// Fetch full message bodies for the given UIDs.
    ///
    /// Uses `BODY[]` to retrieve the complete RFC 822 message.
    pub async fn fetch_bodies(&mut self, uids: &[u32]) -> Result<Vec<ImapMessage>, ImapError> {
        if uids.is_empty() {
            return Ok(Vec::new());
        }

        let uid_set = format_uid_set(uids);
        let fetch_items = "UID FLAGS RFC822.SIZE ENVELOPE BODY[]";

        let stream = self
            .session
            .uid_fetch(&uid_set, fetch_items)
            .await
            .map_err(ImapError::Imap)?;

        let fetches: Vec<_> = stream.try_collect().await.map_err(ImapError::Imap)?;

        let mut result = Vec::new();
        for msg in &fetches {
            result.push(parse_fetch_to_message(msg, true));
        }

        Ok(result)
    }

    /// Add IMAP flags on a UID (e.g. `\\Seen`, `\\Flagged`).
    pub async fn add_flags(&mut self, uid: u32, flags: &[&str]) -> Result<(), ImapError> {
        self.uid_store_flags(uid, "+", flags).await
    }

    /// Remove IMAP flags on a UID.
    pub async fn remove_flags(&mut self, uid: u32, flags: &[&str]) -> Result<(), ImapError> {
        self.uid_store_flags(uid, "-", flags).await
    }

    async fn uid_store_flags(
        &mut self,
        uid: u32,
        op: &str,
        flags: &[&str],
    ) -> Result<(), ImapError> {
        if flags.is_empty() {
            return Ok(());
        }
        let flag_list = flags.join(" ");
        let query = format!("{op}FLAGS ({flag_list})");
        let stream = self
            .session
            .uid_store(uid.to_string(), query)
            .await
            .map_err(ImapError::Imap)?;
        // Drain FETCH responses from STORE
        let _: Vec<_> = stream.try_collect().await.map_err(ImapError::Imap)?;
        Ok(())
    }

    /// Move a message UID into another mailbox (RFC 6851 UID MOVE).
    pub async fn move_uid(&mut self, uid: u32, destination: &str) -> Result<(), ImapError> {
        self.session
            .uid_mv(uid.to_string(), destination)
            .await
            .map_err(ImapError::Imap)
    }

    /// Log out and close the connection.
    #[allow(dead_code)]
    pub async fn logout(mut self) -> Result<(), ImapError> {
        self.session.logout().await.map_err(ImapError::Imap)?;
        Ok(())
    }
}

// ── Parsing helpers ─────────────────────────────────────────────────

/// Parse an `async_imap::types::Fetch` into our `ImapMessage`.
fn parse_fetch_to_message(fetch: &async_imap::types::Fetch, include_body: bool) -> ImapMessage {
    let uid = fetch.uid.unwrap_or(0);
    let flags: Vec<String> = fetch.flags().map(|f| format!("{f:?}")).collect();
    let size = fetch.size;

    // Parse envelope if present
    let (message_id, subject, from, to, cc, date, in_reply_to, references) =
        if let Some(envelope) = fetch.envelope() {
            (
                envelope
                    .message_id
                    .as_ref()
                    .map(|n| String::from_utf8_lossy(n).into_owned()),
                envelope
                    .subject
                    .as_ref()
                    .map(|n| String::from_utf8_lossy(n).into_owned()),
                envelope.from.as_ref().map(|a| format_address_list(a)),
                envelope.to.as_ref().map(|a| format_address_list(a)),
                envelope.cc.as_ref().map(|a| format_address_list(a)),
                envelope
                    .date
                    .as_ref()
                    .map(|n| String::from_utf8_lossy(n).into_owned()),
                envelope
                    .in_reply_to
                    .as_ref()
                    .map(|n| String::from_utf8_lossy(n).into_owned()),
                None, // References not in Envelope; would need to parse headers
            )
        } else {
            (None, None, None, None, None, None, None, None)
        };

    let (body_bytes, body_text, body_html, has_attachments, attachments) = if include_body {
        if let Some(raw) = fetch.body() {
            let (text, html, atts) = extract_mime_parts(raw);
            let has = !atts.is_empty();
            (Some(raw.to_vec()), text, html, has, atts)
        } else {
            (None, None, None, false, Vec::new())
        }
    } else {
        (None, None, None, false, Vec::new())
    };

    ImapMessage {
        uid,
        message_id,
        subject,
        from,
        to,
        cc,
        date,
        in_reply_to,
        references,
        flags,
        size,
        body: body_bytes,
        body_text,
        body_html,
        has_attachments,
        attachments,
    }
}

// ── Helpers ──────────────────────────────────────────────────────────

/// Format a list of UIDs into an IMAP UID set string (e.g. "1,3,5:10").
fn format_uid_set(uids: &[u32]) -> String {
    let mut sorted = uids.to_vec();
    sorted.sort_unstable();
    sorted.dedup();

    let mut parts = Vec::new();
    let mut start = sorted[0];
    let mut end = sorted[0];

    for &uid in &sorted[1..] {
        if uid == end + 1 {
            end = uid;
        } else {
            if start == end {
                parts.push(start.to_string());
            } else {
                parts.push(format!("{start}:{end}"));
            }
            start = uid;
            end = uid;
        }
    }
    if start == end {
        parts.push(start.to_string());
    } else {
        parts.push(format!("{start}:{end}"));
    }

    parts.join(",")
}

/// Format an IMAP address list into a readable string.
fn format_address_list(addrs: &[Address]) -> String {
    let parts: Vec<String> = addrs
        .iter()
        .map(|addr| {
            let mailbox = addr
                .mailbox
                .as_ref()
                .map(|m| String::from_utf8_lossy(m).into_owned())
                .unwrap_or_default();
            let host = addr
                .host
                .as_ref()
                .map(|h| String::from_utf8_lossy(h).into_owned())
                .unwrap_or_default();
            let name = addr
                .name
                .as_ref()
                .map(|n| String::from_utf8_lossy(n).into_owned());

            if let Some(name) = name {
                format!("{name} <{mailbox}@{host}>")
            } else {
                format!("{mailbox}@{host}")
            }
        })
        .collect();

    parts.join(", ")
}

#[allow(clippy::too_many_lines)]
/// Extract plain-text, HTML, and attachment parts from a raw RFC 822 message.
///
/// Returns `(body_text, body_html, attachments)`.
fn extract_mime_parts(raw: &[u8]) -> (Option<String>, Option<String>, Vec<ExtractedAttachment>) {
    let Ok(body_str) = std::str::from_utf8(raw) else {
        return (None, None, Vec::new());
    };

    let mut body_text = None;
    let mut body_html = None;
    let mut attachments = Vec::new();

    let body_start = body_str
        .find("\r\n\r\n")
        .map(|i| i + 4)
        .or_else(|| body_str.find("\n\n").map(|i| i + 2))
        .unwrap_or(0);

    let headers_section = &body_str[..body_start];
    let body_section = &body_str[body_start..];

    let boundary =
        extract_header_value(headers_section, "content-type").and_then(|ct| extract_boundary(&ct));

    if let Some(boundary) = boundary {
        let parts: Vec<&str> = body_section.split(&format!("--{boundary}")).collect();
        for part in &parts[1..] {
            if part.starts_with("--") {
                continue;
            }

            let part_body_start = part
                .find("\r\n\r\n")
                .map(|i| i + 4)
                .or_else(|| part.find("\n\n").map(|i| i + 2))
                .unwrap_or(0);

            if part_body_start >= part.len() {
                continue;
            }

            let part_headers = &part[..part_body_start.min(part.len())];
            let part_body = &part[part_body_start..];

            let content_type = extract_header_value(part_headers, "content-type")
                .unwrap_or_else(|| "text/plain".to_string());
            let encoding = extract_header_value(part_headers, "content-transfer-encoding");
            let disposition = extract_header_value(part_headers, "content-disposition");
            let content_id = extract_header_value(part_headers, "content-id")
                .map(|s| s.trim_matches(|c| c == '<' || c == '>').to_string());

            let content = part_body.trim_end_matches(['\r', '\n', '-']);
            let is_attachment = disposition
                .as_deref()
                .is_some_and(|d| d.to_lowercase().starts_with("attachment"))
                || content_type.contains("application/")
                || (content_type.contains("image/")
                    && disposition
                        .as_deref()
                        .is_some_and(|d| !d.to_lowercase().contains("inline")));

            if content_type.contains("multipart/") {
                let nested = extract_mime_parts(content.as_bytes());
                if body_text.is_none() {
                    body_text = nested.0;
                }
                if body_html.is_none() {
                    body_html = nested.1;
                }
                attachments.extend(nested.2);
            } else if is_attachment
                || content_type.contains("application/")
                || content_type.contains("image/")
                || content_type.contains("audio/")
                || content_type.contains("video/")
            {
                let filename = disposition
                    .as_deref()
                    .and_then(extract_filename)
                    .or_else(|| {
                        content_type.split(';').find_map(|p| {
                            let p = p.trim();
                            if p.to_lowercase().starts_with("name=") {
                                Some(p.split_once('=')?.1.trim_matches('"').to_string())
                            } else {
                                None
                            }
                        })
                    })
                    .unwrap_or_else(|| "attachment.bin".to_string());
                let is_inline = disposition
                    .as_deref()
                    .is_some_and(|d| d.to_lowercase().contains("inline"));
                if let Some(data) = decode_content_bytes(content, encoding.as_deref()) {
                    attachments.push(ExtractedAttachment {
                        filename,
                        content_type: content_type
                            .split(';')
                            .next()
                            .unwrap_or("application/octet-stream")
                            .trim()
                            .to_string(),
                        data,
                        content_id,
                        is_inline,
                    });
                }
            } else if content_type.contains("text/plain") && body_text.is_none() {
                body_text = decode_content(content, encoding.as_deref());
            } else if content_type.contains("text/html") && body_html.is_none() {
                body_html =
                    decode_content(content, encoding.as_deref()).map(|h| sanitize_email_html(&h));
            }
        }
    } else {
        let content_type = extract_header_value(headers_section, "content-type")
            .unwrap_or_else(|| "text/plain".to_string());
        let encoding = extract_header_value(headers_section, "content-transfer-encoding");
        let content = body_section.trim_end_matches(['\r', '\n']);

        if content_type.contains("text/plain") {
            body_text = decode_content(content, encoding.as_deref());
        } else if content_type.contains("text/html") {
            body_html =
                decode_content(content, encoding.as_deref()).map(|h| sanitize_email_html(&h));
        }
    }

    (body_text, body_html, attachments)
}

fn extract_filename(disposition: &str) -> Option<String> {
    for part in disposition.split(';') {
        let part = part.trim();
        let lower = part.to_lowercase();
        if lower.starts_with("filename*=") {
            // RFC 5987 — ignore charset for v1; take after ''
            let value = part.split_once('=')?.1.trim_matches('"');
            if let Some((_charset_lang, rest)) = value.split_once("''") {
                return Some(rest.to_string());
            }
            return Some(value.to_string());
        }
        if lower.starts_with("filename=") {
            return Some(part.split_once('=')?.1.trim_matches('"').to_string());
        }
    }
    None
}

/// Extract a header value from raw headers (case-insensitive).
fn extract_header_value(headers: &str, name: &str) -> Option<String> {
    let name_lower = name.to_lowercase();
    for line in headers.lines() {
        if let Some((key, value)) = line.split_once(':')
            && key.trim().to_lowercase() == name_lower
        {
            return Some(value.trim().to_string());
        }
    }
    None
}

/// Extract the `boundary` parameter from a `Content-Type` header value.
fn extract_boundary(content_type: &str) -> Option<String> {
    for part in content_type.split(';') {
        let part = part.trim();
        if part.to_lowercase().starts_with("boundary=") {
            let value = part.split_once('=')?.1;
            return Some(value.trim_matches('"').to_string());
        }
    }
    None
}

/// Decode content based on transfer encoding.
fn decode_content(content: &str, encoding: Option<&str>) -> Option<String> {
    decode_content_bytes(content, encoding).and_then(|bytes| String::from_utf8(bytes).ok())
}

fn decode_content_bytes(content: &str, encoding: Option<&str>) -> Option<Vec<u8>> {
    match encoding.map(str::to_lowercase).as_deref() {
        Some("base64") => {
            let cleaned: String = content.chars().filter(|c| !c.is_whitespace()).collect();
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &cleaned).ok()
        }
        Some("quoted-printable") => Some(decode_quoted_printable(content).into_bytes()),
        _ => Some(content.as_bytes().to_vec()),
    }
}

/// Simple quoted-printable decoder.
fn decode_quoted_printable(input: &str) -> String {
    let mut output = Vec::new();
    let bytes = input.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'='
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_digit(bytes[i + 1]), hex_digit(bytes[i + 2]))
        {
            output.push(hi * 16 + lo);
            i += 3;
            continue;
        }
        if bytes[i] == b'='
            && i + 1 < bytes.len()
            && (bytes[i + 1] == b'\r' || bytes[i + 1] == b'\n')
        {
            // Soft line break
            i += 2;
            if i < bytes.len() && bytes[i] == b'\n' {
                i += 1;
            }
            continue;
        }
        output.push(bytes[i]);
        i += 1;
    }

    String::from_utf8_lossy(&output).into_owned()
}

/// Convert a hex ASCII digit to its numeric value.
fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Decrypt the stored credential for an account.
///
/// Takes the encrypted JSON blob from the `credential` column and
/// the user's DEK, and returns the plaintext password.
pub fn decrypt_account_password(credential_json: &str, dek: &[u8]) -> Result<String, ImapError> {
    let encrypted: EncryptedCredential = serde_json::from_str(credential_json)
        .map_err(|e| ImapError::Protocol(format!("invalid credential blob: {e}")))?;

    let plaintext = crypto::decrypt(dek, &encrypted)?;

    String::from_utf8(plaintext)
        .map_err(|e| ImapError::Protocol(format!("credential not valid UTF-8: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_uid_set_contiguous() {
        assert_eq!(format_uid_set(&[1, 2, 3, 4, 5]), "1:5");
    }

    #[test]
    fn format_uid_set_scattered() {
        assert_eq!(format_uid_set(&[1, 3, 5, 7]), "1,3,5,7");
    }

    #[test]
    fn format_uid_set_mixed() {
        assert_eq!(format_uid_set(&[1, 2, 3, 7, 8, 10]), "1:3,7:8,10");
    }

    #[test]
    fn format_uid_set_single() {
        assert_eq!(format_uid_set(&[42]), "42");
    }

    #[test]
    fn format_uid_set_unsorted_with_dupes() {
        assert_eq!(format_uid_set(&[5, 3, 1, 3, 5]), "1,3,5");
    }

    #[test]
    fn extract_mime_parts_sanitizes_html_part() {
        let raw = b"Content-Type: multipart/alternative; boundary=\"b\"\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nplain\r\n--b\r\nContent-Type: text/html\r\n\r\n<p><b>hi</b></p><script>alert(1)</script><img src=\"https://x/y.png\" onerror=\"alert(2)\">\r\n--b--\r\n";
        let (text, html, _) = extract_mime_parts(raw);
        assert_eq!(text.as_deref(), Some("plain"));
        let html = html.expect("html part");
        assert!(html.contains("<b>hi</b>"), "got: {html}");
        assert!(!html.contains("<script"), "got: {html}");
        assert!(!html.to_lowercase().contains("onerror"), "got: {html}");
        assert!(!html.contains("alert("), "got: {html}");
        assert!(html.contains("src=\"https://x/y.png\""), "got: {html}");
    }

    #[test]
    fn extract_boundary_simple() {
        let ct = "multipart/mixed; boundary=\"abc123\"";
        assert_eq!(extract_boundary(ct), Some("abc123".into()));
    }

    #[test]
    fn extract_boundary_no_quotes() {
        let ct = "multipart/mixed; boundary=abc123";
        assert_eq!(extract_boundary(ct), Some("abc123".into()));
    }

    #[test]
    fn extract_boundary_none() {
        let ct = "text/plain";
        assert_eq!(extract_boundary(ct), None);
    }

    #[test]
    fn extract_header_value_basic() {
        let headers = "Content-Type: text/plain\r\nSubject: Hello\r\n";
        assert_eq!(
            extract_header_value(headers, "Content-Type"),
            Some("text/plain".into())
        );
        assert_eq!(
            extract_header_value(headers, "subject"),
            Some("Hello".into())
        );
        assert_eq!(extract_header_value(headers, "Missing"), None);
    }

    #[test]
    fn decode_quoted_printable_basic() {
        let input = "Hello=20World=21";
        assert_eq!(decode_quoted_printable(input), "Hello World!");
    }

    #[test]
    fn decode_quoted_printable_soft_break() {
        let input = "long line=\r\ncontinued";
        assert_eq!(decode_quoted_printable(input), "long linecontinued");
    }

    #[test]
    fn imap_config_password_is_zeroizing() {
        let cfg = ImapConfig {
            host: "imap.example.com".into(),
            port: 993,
            security: ImapSecurity::Tls,
            username: "user".into(),
            password: Zeroizing::new("secret".into()),
        };
        assert_eq!(&*cfg.password, "secret");
    }

    #[test]
    fn decrypt_roundtrip() {
        let key = crypto::generate_key();
        let password = "test-password-123";
        let encrypted = crypto::encrypt(&key, password.as_bytes()).unwrap();
        let json = serde_json::to_string(&encrypted).unwrap();
        let decrypted = decrypt_account_password(&json, &key).unwrap();
        assert_eq!(decrypted, password);
    }
}
