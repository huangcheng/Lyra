//! IMAP protocol adapter.
//!
//! Wraps `async-imap` to provide connect/login/list/select/fetch operations.
//! The adapter hides all protocol details from the sync engine.
//!
//! See `docs/specs/2026-08-20-lyra-sync-and-protocols-spec.md`.

#![allow(clippy::doc_markdown)]

use async_imap::Session;
use async_imap::types::Capabilities;
use futures_util::TryStreamExt;
use imap_proto::types::Address;
use serde::Deserialize;
use std::future::Future;
use std::time::Duration;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_native_tls::TlsStream;

use crate::crypto::{self, EncryptedCredential};
use zeroize::Zeroizing;

/// Bound for TCP connect + TLS + login + initial CAPABILITY (CHE-129).
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Bound for a single IMAP command / fetch collect (CHE-129).
pub const COMMAND_TIMEOUT: Duration = Duration::from_mins(1);

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
    #[error("IMAP operation timed out")]
    Timeout,
    #[error("crypto error: {0}")]
    Crypto(#[from] crypto::CryptoError),
    #[error("imap error: {0}")]
    Imap(#[from] async_imap::error::Error),
}

/// Await `fut` or fail with [`ImapError::Timeout`] when `limit` elapses.
pub(crate) async fn timed<T>(
    limit: Duration,
    fut: impl Future<Output = Result<T, ImapError>>,
) -> Result<T, ImapError> {
    match tokio::time::timeout(limit, fut).await {
        Ok(result) => result,
        Err(_) => Err(ImapError::Timeout),
    }
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
    /// Password or OAuth access token (see `xoauth2`).
    pub password: Zeroizing<String>,
    /// When true, authenticate with SASL XOAUTH2 (password field = access token).
    pub xoauth2: bool,
}

/// SASL XOAUTH2 authenticator (RFC 7628 / Google+Microsoft IMAP).
struct Xoauth2Auth {
    user: String,
    access_token: String,
}

impl async_imap::Authenticator for &Xoauth2Auth {
    type Response = String;
    fn process(&mut self, _challenge: &[u8]) -> Self::Response {
        format!(
            "user={}\x01auth=Bearer {}\x01\x01",
            self.user, self.access_token
        )
    }
}

/// Read the RFC 3501 untagged `* OK` greeting after TLS connect.
///
/// Required before [`async_imap::Client::authenticate`] (XOAUTH2). Password
/// [`async_imap::Client::login`] tolerates an unread greeting via its tagged
/// response loop, but SASL handshake mis-parses the greeting as auth continuation
/// and hangs until timeout.
async fn consume_server_greeting<T>(client: &mut async_imap::Client<T>) -> Result<(), ImapError>
where
    T: AsyncRead + AsyncWrite + Unpin + Send + std::fmt::Debug + 'static,
{
    match client.read_response().await {
        Some(Ok(_)) => Ok(()),
        Some(Err(e)) => Err(ImapError::Imap(async_imap::error::Error::Io(e))),
        None => Err(ImapError::Connection(
            "IMAP server closed before greeting".into(),
        )),
    }
}

async fn authenticate_client<T>(
    client: async_imap::Client<T>,
    config: &ImapConfig,
) -> Result<Session<T>, ImapError>
where
    T: AsyncRead + AsyncWrite + Unpin + Send + std::fmt::Debug + 'static,
{
    if config.xoauth2 {
        let auth = Xoauth2Auth {
            user: config.username.clone(),
            access_token: (*config.password).clone(),
        };
        client
            .authenticate("XOAUTH2", &auth)
            .await
            .map_err(|(e, _)| ImapError::Login(e.to_string()))
    } else {
        client
            .login(&config.username, &config.password)
            .await
            .map_err(|(e, _)| ImapError::Login(e.to_string()))
    }
}

/// Decode an IMAP mailbox name (Modified UTF-7, RFC 3501 §5.1.3) for display.
///
/// Wire names from LIST/SELECT stay encoded; only UI-facing `folder.name` uses this.
pub fn decode_imap_mailbox_name(encoded: &str) -> String {
    if !encoded.contains('&') {
        return encoded.to_string();
    }
    utf7_imap::decode_utf7_imap(encoded.to_string())
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

/// Result of SELECT / EXAMINE (RFC 3501 + RFC 7162 CONDSTORE).
#[derive(Debug, Clone, Copy)]
pub struct ImapSelectResult {
    pub uid_validity: u32,
    /// Present when the server advertises CONDSTORE (RFC 7162).
    pub highest_modseq: Option<u64>,
}

/// An authenticated IMAP session.
///
/// Wraps `async_imap::Session<TlsStream<TcpStream>>` and provides
/// high-level operations for the sync engine.
pub struct ImapClient {
    session: Session<TlsStream<TcpStream>>,
    capabilities: Capabilities,
}

impl ImapClient {
    /// Connect to an IMAP server and authenticate.
    ///
    /// Handles TLS and STARTTLS connections. Bounded by [`CONNECT_TIMEOUT`].
    pub async fn connect(config: &ImapConfig) -> Result<Self, ImapError> {
        Self::connect_within(config, CONNECT_TIMEOUT).await
    }

    /// Connect with an explicit timeout (tests inject a short bound).
    pub(crate) async fn connect_within(
        config: &ImapConfig,
        connect_timeout: Duration,
    ) -> Result<Self, ImapError> {
        timed(connect_timeout, Self::connect_inner(config)).await
    }

    async fn connect_inner(config: &ImapConfig) -> Result<Self, ImapError> {
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

                let mut client = async_imap::Client::new(tls_stream);
                consume_server_greeting(&mut client).await?;
                let mut session = authenticate_client(client, config).await?;

                let capabilities = session.capabilities().await.map_err(ImapError::Imap)?;

                Ok(Self {
                    session,
                    capabilities,
                })
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
                let mut session = authenticate_client(tls_client, config).await?;

                let capabilities = session.capabilities().await.map_err(ImapError::Imap)?;

                Ok(Self {
                    session,
                    capabilities,
                })
            }
        }
    }

    /// Whether the server advertises RFC 7162 CONDSTORE (or QRESYNC).
    pub fn supports_condstore(&self) -> bool {
        self.capabilities.has_str("CONDSTORE") || self.capabilities.has_str("QRESYNC")
    }

    /// Whether the server advertises RFC 6851 MOVE.
    pub fn supports_move(&self) -> bool {
        self.capabilities.has_str("MOVE")
    }

    /// Whether the server advertises RFC 2177 IDLE.
    pub fn supports_idle(&self) -> bool {
        self.capabilities.has_str("IDLE")
    }

    /// Whether the server advertises RFC 5464 METADATA.
    pub fn supports_metadata(&self) -> bool {
        self.capabilities.has_str("METADATA")
    }

    /// Best-effort `SETMETADATA … (/private/specialuse …)` (RFC 5464 + 6154).
    ///
    /// No-op when METADATA is not advertised. Errors (including `NO [USEATTR]`)
    /// are swallowed so local role overrides still succeed everywhere.
    pub async fn set_private_specialuse(
        &mut self,
        mailbox_wire: &str,
        special_use: Option<&str>,
    ) -> Result<(), ImapError> {
        if !self.supports_metadata() {
            return Ok(());
        }
        let mbx = imap_quoted(mailbox_wire);
        let value = match special_use {
            Some(flag) => imap_quoted(flag),
            None => "NIL".to_string(),
        };
        let cmd = format!("SETMETADATA {mbx} (/private/specialuse {value})");
        timed(COMMAND_TIMEOUT, async {
            match self.session.run_command_and_check_ok(&cmd).await {
                Ok(()) => Ok(()),
                Err(e) => {
                    tracing::debug!(error = %e, mailbox = %mailbox_wire, "SETMETADATA skipped");
                    Ok(())
                }
            }
        })
        .await
    }

    /// List all folders (mailboxes) on the server.
    pub async fn list_folders(&mut self) -> Result<Vec<ImapFolder>, ImapError> {
        timed(COMMAND_TIMEOUT, async {
            let listing = self
                .session
                .list(Some(""), Some("*"))
                .await
                .map_err(ImapError::Imap)?;

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
        })
        .await
    }

    /// Select a folder (mailbox) for subsequent operations.
    pub async fn select(&mut self, folder_name: &str) -> Result<ImapSelectResult, ImapError> {
        timed(COMMAND_TIMEOUT, async {
            let mailbox = self
                .session
                .select(folder_name)
                .await
                .map_err(ImapError::Imap)?;

            Ok(ImapSelectResult {
                uid_validity: mailbox.uid_validity.unwrap_or(0),
                highest_modseq: mailbox.highest_modseq,
            })
        })
        .await
    }

    /// Fetch message UIDs optionally after a given UID.
    ///
    /// Uses UID SEARCH to find messages.
    pub async fn search_uids(&mut self, after_uid: Option<u32>) -> Result<Vec<u32>, ImapError> {
        timed(COMMAND_TIMEOUT, async {
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
        })
        .await
    }

    /// Fetch message metadata (envelope + flags + size) for the given UIDs.
    ///
    /// Does **not** fetch message bodies — use `fetch_bodies` for that.
    pub async fn fetch_metadata(&mut self, uids: &[u32]) -> Result<Vec<ImapMessage>, ImapError> {
        if uids.is_empty() {
            return Ok(Vec::new());
        }

        timed(COMMAND_TIMEOUT, async {
            let uid_set = format_uid_set(uids);
            let fetch_items = parenthesize_fetch_atts("UID FLAGS RFC822.SIZE ENVELOPE");

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
        })
        .await
    }

    /// Fetch messages whose mod-sequence changed since `modseq` (RFC 7162 CHANGEDSINCE).
    ///
    /// No-op when `modseq` is 0 or CONDSTORE is unavailable.
    pub async fn fetch_changed_since(
        &mut self,
        modseq: u64,
    ) -> Result<Vec<ImapMessage>, ImapError> {
        if modseq == 0 || !self.supports_condstore() {
            return Ok(Vec::new());
        }

        timed(COMMAND_TIMEOUT, async {
            let fetch_items = parenthesize_fetch_atts("UID FLAGS RFC822.SIZE ENVELOPE");
            let query = format!("{fetch_items} (CHANGEDSINCE {modseq})");

            let stream = self
                .session
                .uid_fetch("1:*", query)
                .await
                .map_err(ImapError::Imap)?;

            let fetches: Vec<_> = stream.try_collect().await.map_err(ImapError::Imap)?;

            Ok(fetches
                .iter()
                .map(|msg| parse_fetch_to_message(msg, false))
                .collect())
        })
        .await
    }

    /// Fetch full message bodies for the given UIDs.
    ///
    /// Uses `BODY.PEEK[]` to retrieve the complete RFC 822 message without
    /// implicitly setting `\Seen`.
    pub async fn fetch_bodies(&mut self, uids: &[u32]) -> Result<Vec<ImapMessage>, ImapError> {
        if uids.is_empty() {
            return Ok(Vec::new());
        }

        timed(COMMAND_TIMEOUT, async {
            let uid_set = format_uid_set(uids);
            let fetch_items = parenthesize_fetch_atts("UID FLAGS RFC822.SIZE ENVELOPE BODY.PEEK[]");

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
        })
        .await
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
        timed(COMMAND_TIMEOUT, async {
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
        })
        .await
    }

    /// Move a message UID into another mailbox (RFC 6851 UID MOVE).
    ///
    /// Falls back to UID COPY + STORE \\Deleted + EXPUNGE when MOVE is not advertised.
    pub async fn move_uid(&mut self, uid: u32, destination: &str) -> Result<(), ImapError> {
        timed(COMMAND_TIMEOUT, async {
            let uid_str = uid.to_string();
            if self.supports_move() {
                self.session
                    .uid_mv(&uid_str, destination)
                    .await
                    .map_err(ImapError::Imap)?;
                return Ok(());
            }

            // Documented exception: COPY+EXPUNGE when MOVE unavailable (RFC 6851 §3.3).
            self.session
                .uid_copy(&uid_str, destination)
                .await
                .map_err(ImapError::Imap)?;
            let query = "+FLAGS (\\Deleted)".to_string();
            let stream = self
                .session
                .uid_store(uid_str.clone(), query)
                .await
                .map_err(ImapError::Imap)?;
            let _: Vec<_> = stream.try_collect().await.map_err(ImapError::Imap)?;
            if self.capabilities.has_str("UIDPLUS") {
                let stream = self
                    .session
                    .uid_expunge(&uid_str)
                    .await
                    .map_err(ImapError::Imap)?;
                let _: Vec<_> = stream.try_collect().await.map_err(ImapError::Imap)?;
            } else {
                let stream = self.session.expunge().await.map_err(ImapError::Imap)?;
                let _: Vec<_> = stream.try_collect().await.map_err(ImapError::Imap)?;
            }
            Ok(())
        })
        .await
    }

    /// Log out and close the connection.
    #[allow(dead_code)]
    pub async fn logout(mut self) -> Result<(), ImapError> {
        timed(COMMAND_TIMEOUT, async {
            self.session.logout().await.map_err(ImapError::Imap)?;
            Ok(())
        })
        .await
    }

    /// Enter IDLE on the currently selected mailbox until a server notify or renew.
    ///
    /// Consumes the client (IDLE owns the session). Returns after `NewData`
    /// (typically EXISTS/EXPUNGE/FETCH). Renews IDLE on inactivity timeout
    /// (RFC 2177: re-issue before ~29 minutes).
    ///
    /// Caller must have [`Self::select`]ed a mailbox first and checked
    /// [`Self::supports_idle`]. IDLE wait itself is not bounded by
    /// [`COMMAND_TIMEOUT`] (intentional long-poll); init/done are.
    pub async fn into_idle_watch(self) -> Result<IdleWatchOutcome, ImapError> {
        if !self.supports_idle() {
            return Ok(IdleWatchOutcome::Unsupported);
        }

        let renew = std::time::Duration::from_mins(25);
        let mut handle = self.session.idle();
        timed(COMMAND_TIMEOUT, async {
            handle.init().await.map_err(ImapError::Imap)
        })
        .await?;

        loop {
            let (wait_fut, _interrupt) = handle.wait_with_timeout(renew);
            match wait_fut.await.map_err(ImapError::Imap)? {
                async_imap::extensions::idle::IdleResponse::NewData(_) => {
                    // Drop DONE + session; next sync opens a fresh connection.
                    let _ = timed(COMMAND_TIMEOUT, async {
                        handle.done().await.map_err(ImapError::Imap)
                    })
                    .await;
                    return Ok(IdleWatchOutcome::Notified);
                }
                async_imap::extensions::idle::IdleResponse::Timeout => {
                    let session = timed(COMMAND_TIMEOUT, async {
                        handle.done().await.map_err(ImapError::Imap)
                    })
                    .await?;
                    handle = session.idle();
                    timed(COMMAND_TIMEOUT, async {
                        handle.init().await.map_err(ImapError::Imap)
                    })
                    .await?;
                }
                async_imap::extensions::idle::IdleResponse::ManualInterrupt => {
                    let _ = timed(COMMAND_TIMEOUT, async {
                        handle.done().await.map_err(ImapError::Imap)
                    })
                    .await;
                    return Ok(IdleWatchOutcome::Interrupted);
                }
            }
        }
    }
}

/// Result of an IDLE watch session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdleWatchOutcome {
    /// Server pushed mailbox state (EXISTS / EXPUNGE / etc.).
    Notified,
    /// Server does not advertise IDLE — caller should poll only.
    Unsupported,
    /// Local interrupt (stop token); reconnect later.
    Interrupted,
}

// ── Parsing helpers ─────────────────────────────────────────────────

/// Decode RFC 2047 encoded-words in an unstructured header (Subject, display name).
///
/// IMAP ENVELOPE often returns subjects still encoded as `=?UTF-8?Q?...?=` /
/// `=?utf-8?B?...?=`. Plain ASCII is returned unchanged.
pub fn decode_mime_header(raw: &str) -> String {
    if !raw.contains("=?") {
        return raw.to_string();
    }
    let mut bytes = Vec::with_capacity(raw.len() + 16);
    bytes.extend_from_slice(b"Subject: ");
    bytes.extend_from_slice(raw.as_bytes());
    if !raw.ends_with('\n') {
        bytes.extend_from_slice(b"\r\n\r\n");
    }
    mail_parser::MessageParser::default()
        .parse(&bytes)
        .and_then(|msg| msg.subject().map(str::to_owned))
        .filter(|decoded| !decoded.is_empty())
        .unwrap_or_else(|| raw.to_string())
}

fn decode_opt_mime_header(raw: Option<String>) -> Option<String> {
    raw.map(|s| decode_mime_header(&s))
}

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
                decode_opt_mime_header(
                    envelope
                        .subject
                        .as_ref()
                        .map(|n| String::from_utf8_lossy(n).into_owned()),
                ),
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

/// Wrap FETCH data items in parentheses when the caller passed a bare list.
///
/// RFC 3501 `fetch` is `sequence-set SP (ALL / FULL / FAST / fetch-att /
/// "(" fetch-att *(SP fetch-att) ")")`. `async-imap` interpolates the query
/// as-is, so `UID FLAGS ENVELOPE` is parsed as a single `fetch-att` (`UID`)
/// and the rest is ignored. Parenthesized lists fetch every item.
fn parenthesize_fetch_atts(atts: &str) -> String {
    let trimmed = atts.trim();
    if trimmed.starts_with('(') && trimmed.ends_with(')') {
        trimmed.to_string()
    } else {
        format!("({trimmed})")
    }
}

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
                .map(|n| decode_mime_header(&String::from_utf8_lossy(n)));

            if let Some(name) = name {
                format!("{name} <{mailbox}@{host}>")
            } else {
                format!("{mailbox}@{host}")
            }
        })
        .collect();

    parts.join(", ")
}

/// Extract plain-text, HTML, and attachment parts from a raw RFC 822 message.
///
/// Returns `(body_text, body_html, attachments)`. HTML is **not** sanitized
/// here — persist via [`crate::sanitize::persist_body_html`].
fn extract_mime_parts(raw: &[u8]) -> (Option<String>, Option<String>, Vec<ExtractedAttachment>) {
    let Some(message) = mail_parser::MessageParser::default().parse(raw) else {
        return (None, None, Vec::new());
    };

    let body_text = message.body_text(0).map(std::borrow::Cow::into_owned);
    let body_html = message.body_html(0).map(std::borrow::Cow::into_owned);

    let attachments = message
        .attachments()
        .filter(|part| !part.is_multipart() && part.message().is_none())
        .map(|part| {
            use mail_parser::MimeHeaders;
            let filename = part
                .attachment_name()
                .unwrap_or("attachment.bin")
                .to_owned();
            let content_type = part.content_type().map_or_else(
                || "application/octet-stream".into(),
                |ct| match ct.subtype() {
                    Some(sub) => format!("{}/{sub}", ct.ctype()),
                    None => ct.ctype().to_owned(),
                },
            );
            ExtractedAttachment {
                filename,
                content_type,
                data: part.contents().to_vec(),
                content_id: part.content_id().map(str::to_owned),
                is_inline: part
                    .content_disposition()
                    .is_some_and(mail_parser::ContentType::is_inline),
            }
        })
        .collect();

    (body_text, body_html, attachments)
}

/// Map a Lyra folder role to an RFC 6154 SPECIAL-USE flag string (`\Sent`, …).
///
/// `inbox` has no SPECIAL-USE attribute; returns `None`.
#[must_use]
pub fn role_to_specialuse(role: &str) -> Option<&'static str> {
    match role {
        "sent" => Some("\\Sent"),
        "drafts" => Some("\\Drafts"),
        "trash" => Some("\\Trash"),
        "spam" => Some("\\Junk"),
        "archive" => Some("\\Archive"),
        _ => None,
    }
}

/// Quote an IMAP string (RFC 3501 quoted-string).
#[must_use]
pub(crate) fn imap_quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if c == '\\' || c == '"' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
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
    fn role_to_specialuse_maps_rfc6154() {
        assert_eq!(role_to_specialuse("sent"), Some("\\Sent"));
        assert_eq!(role_to_specialuse("spam"), Some("\\Junk"));
        assert_eq!(role_to_specialuse("archive"), Some("\\Archive"));
        assert_eq!(role_to_specialuse("inbox"), None);
        assert_eq!(role_to_specialuse("unknown"), None);
    }

    #[test]
    fn imap_quoted_escapes_backslash_and_quote() {
        assert_eq!(imap_quoted("INBOX"), "\"INBOX\"");
        assert_eq!(imap_quoted("\\Sent"), "\"\\\\Sent\"");
        assert_eq!(imap_quoted("a\"b"), "\"a\\\"b\"");
    }

    #[tokio::test]
    async fn timed_pending_future_errors_within_bound() {
        let started = std::time::Instant::now();
        let err = timed(Duration::from_millis(50), async {
            std::future::pending::<Result<(), ImapError>>().await
        })
        .await
        .unwrap_err();
        assert!(matches!(err, ImapError::Timeout));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn connect_to_blackhole_listener_times_out() {
        // Accept TCP but never speak IMAP — login hangs until CONNECT timeout.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let Ok((sock, _)) = listener.accept().await else {
                return;
            };
            // Keep the socket open without writing a greeting.
            let _ = sock;
            std::future::pending::<()>().await;
        });

        let cfg = ImapConfig {
            host: addr.ip().to_string(),
            port: addr.port(),
            security: ImapSecurity::Tls,
            username: "user".into(),
            password: Zeroizing::new("pass".into()),
            xoauth2: false,
        };
        // Plain TLS will fail handshake quickly on a raw TCP blackhole; use Starttls
        // so the hang is on the IMAP greeting / STARTTLS command instead.
        let cfg = ImapConfig {
            security: ImapSecurity::Starttls,
            ..cfg
        };

        let started = std::time::Instant::now();
        let result = ImapClient::connect_within(&cfg, Duration::from_millis(200)).await;
        let Err(err) = result else {
            panic!("expected Timeout from blackhole listener");
        };
        assert!(
            matches!(err, ImapError::Timeout),
            "expected Timeout, got {err:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn fetch_atts_must_be_parenthesized() {
        // RFC 3501: a fetch-att list is one atom or a parenthesized list.
        // Without parens, servers treat the first atom (`UID`) as the only item
        // and drop ENVELOPE / BODY[] — blank subjects and empty reading panes.
        assert_eq!(
            parenthesize_fetch_atts("UID FLAGS RFC822.SIZE ENVELOPE"),
            "(UID FLAGS RFC822.SIZE ENVELOPE)"
        );
        assert_eq!(
            parenthesize_fetch_atts("UID FLAGS RFC822.SIZE ENVELOPE BODY.PEEK[]"),
            "(UID FLAGS RFC822.SIZE ENVELOPE BODY.PEEK[])"
        );
        assert_eq!(parenthesize_fetch_atts("(UID FLAGS)"), "(UID FLAGS)");
    }

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
    fn extract_mime_parts_html_is_sanitized_at_persist() {
        let raw = b"Content-Type: multipart/alternative; boundary=\"b\"\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nplain\r\n--b\r\nContent-Type: text/html\r\n\r\n<p><b>hi</b></p><script>alert(1)</script><img src=\"https://x/y.png\" onerror=\"alert(2)\">\r\n--b--\r\n";
        let (text, html, _) = extract_mime_parts(raw);
        assert_eq!(text.as_deref().map(str::trim), Some("plain"));
        let html = crate::sanitize::persist_body_html(html.as_deref()).expect("html part");
        assert!(html.contains("<b>hi</b>"), "got: {html}");
        assert!(!html.contains("<script"), "got: {html}");
        assert!(!html.to_lowercase().contains("onerror"), "got: {html}");
        assert!(!html.contains("alert("), "got: {html}");
        assert!(html.contains("src=\"https://x/y.png\""), "got: {html}");
    }

    #[test]
    fn extract_mime_parts_nested_alternative_inside_mixed() {
        let raw = b"Content-Type: multipart/mixed; boundary=\"mixed\"\r\n\r\n\
--mixed\r\nContent-Type: multipart/alternative; boundary=\"alt\"\r\n\r\n\
--alt\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nplain body\r\n\
--alt\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<p>html body</p>\r\n\
--alt--\r\n\
--mixed\r\nContent-Type: application/pdf\r\nContent-Disposition: attachment; filename=\"note.pdf\"\r\nContent-Transfer-Encoding: base64\r\n\r\nQUJD\r\n\
--mixed--\r\n";
        let (text, html, atts) = extract_mime_parts(raw);
        assert_eq!(text.as_deref().map(str::trim), Some("plain body"));
        assert_eq!(html.as_deref().map(str::trim), Some("<p>html body</p>"));
        assert_eq!(atts.len(), 1, "atts={atts:?}");
        assert_eq!(atts[0].filename, "note.pdf");
        assert_eq!(atts[0].content_type.to_ascii_lowercase(), "application/pdf");
        assert_eq!(atts[0].data, b"ABC");
    }

    #[test]
    fn extract_mime_parts_decodes_legacy_charset() {
        let mut raw = b"Content-Type: text/plain; charset=iso-8859-1\r\nContent-Transfer-Encoding: 8bit\r\n\r\nCaf"
            .to_vec();
        raw.push(0xE9);
        let (text, html, atts) = extract_mime_parts(&raw);
        assert_eq!(text.as_deref(), Some("Café"));
        assert!(atts.is_empty());
        let _ = html;
    }

    #[test]
    fn decode_mime_header_ascii_passthrough() {
        assert_eq!(decode_mime_header("Hello"), "Hello");
        assert_eq!(decode_mime_header("Re: AccuWeather"), "Re: AccuWeather");
    }

    #[test]
    fn decode_mime_header_base64_utf8() {
        // "Re: 关于自动续费" (utf-8 B)
        let encoded = "=?utf-8?B?UmU6IOWFs+S6juiHquWKqOe7reiOuQ==?=";
        let decoded = decode_mime_header(encoded);
        assert!(!decoded.contains("=?"), "got: {decoded}");
        assert!(decoded.contains("Re:"), "got: {decoded}");
        assert!(
            decoded.contains('关') || decoded.contains('续'),
            "got: {decoded}"
        );
    }

    #[test]
    fn decode_mime_header_quoted_printable_utf8() {
        let encoded = "=?UTF-8?Q?Re:_=E5=85=B3=E4=BA=8E=E8=87=AA=E5=8A=A8=E7=BB=AD=E8=B4=B9?=";
        let decoded = decode_mime_header(encoded);
        assert!(!decoded.contains("=?"), "got: {decoded}");
        assert!(decoded.starts_with("Re:"), "got: {decoded}");
        assert!(
            decoded.contains('关') || decoded.contains('费'),
            "got: {decoded}"
        );
    }

    #[test]
    fn decode_mime_header_mixed_plain_and_encoded() {
        let encoded = "AS AdGuard Support =?UTF-8?Q?Re:_=E7=BD=91=E7=AB=99?= startpage.ws";
        let decoded = decode_mime_header(encoded);
        assert!(!decoded.contains("=?"), "got: {decoded}");
        assert!(decoded.contains("AdGuard"), "got: {decoded}");
    }

    #[test]
    fn decode_imap_mailbox_name_ascii_passthrough() {
        assert_eq!(decode_imap_mailbox_name("INBOX"), "INBOX");
        assert_eq!(decode_imap_mailbox_name("Sent"), "Sent");
        assert_eq!(decode_imap_mailbox_name("Archive"), "Archive");
    }

    #[test]
    fn decode_imap_mailbox_name_modified_utf7() {
        // Fastmail-style Chinese folder under Archive (Modified UTF-7 on the wire).
        let wire = "Archive/&Xi5SqWUvYwE-";
        let decoded = decode_imap_mailbox_name(wire);
        assert!(
            decoded.contains('档') || decoded.contains("Archive"),
            "expected decoded Chinese folder name, got: {decoded}"
        );
        assert!(
            !decoded.contains("&Xi5"),
            "wire encoding should be decoded: {decoded}"
        );
    }

    #[test]
    fn imap_config_password_is_zeroizing() {
        let cfg = ImapConfig {
            host: "imap.example.com".into(),
            port: 993,
            security: ImapSecurity::Tls,
            username: "user".into(),
            password: Zeroizing::new("secret".into()),
            xoauth2: false,
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
