//! JMAP seam over the `jmap-client` crate (RFC 8620/8621).
//!
//! This is the ONLY module that imports `jmap_client`. Everything behind it
//! speaks Lyra's plain DTOs (`JmapMailbox`, `JmapEmail`, …) so `sync/store.rs`
//! persistence keeps its shape.
//!
//! Security: `/.well-known/jmap` redirects are pre-resolved with our own
//! same-origin follower; the crate client is then allowed to re-follow only
//! the configured host (the crate's allowlist is host-scoped — our follower
//! already validated the chain origin-scoped). Post-connect, every
//! credential-bearing session URL is pinned to the configured origin.
//!
//! See `docs/superpowers/specs/2026-08-29-lyra-jmap-full-support-design.md`.

#![allow(clippy::doc_markdown)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;

use jmap_client::client::{Client, Credentials};
use jmap_client::core::error::MethodErrorType;
use jmap_client::core::set::SetErrorType;
use jmap_client::email::{Email, EmailAddress, EmailBodyPart};
use jmap_client::mailbox::{Mailbox, Role};
use jmap_client::{Get, URI};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::crypto::{self, EncryptedCredential};

/// Whole-request timeout for JMAP API calls (matches the retired client).
// Used by the seam connect path, wired into callers from Task 2 on.
#[allow(dead_code)]
const JMAP_TIMEOUT: Duration = Duration::from_secs(30);
/// Redirect hops accepted during well-known pre-resolution.
#[allow(dead_code)]
const MAX_DISCOVERY_HOPS: u32 = 5;

// ── Errors ──────────────────────────────────────────────────────────

/// Errors specific to the JMAP seam.
#[derive(Debug, Error)]
pub enum JmapError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("session discovery failed: {0}")]
    SessionDiscovery(String),
    #[error("authentication failed: {0}")]
    Authentication(String),
    /// Legacy wire-level method error (hand-rolled transport; pruned in Task 7).
    #[error("JMAP method error: {code} — {description}")]
    Method { code: String, description: String },
    #[error("crypto error: {0}")]
    Crypto(#[from] crypto::CryptoError),
    #[error("invalid response: {0}")]
    InvalidResponse(String),
    #[error("invalid server URL: {0}")]
    InvalidServerUrl(String),
    #[error("cross-origin URL rejected (credentials stay pinned): {0}")]
    CrossOrigin(String),
    /// Typed error from the `jmap-client` crate.
    #[error("JMAP protocol error: {0}")]
    Client(jmap_client::Error),
}

impl From<jmap_client::Error> for JmapError {
    fn from(err: jmap_client::Error) -> Self {
        // 401 surfaces as ProblemDetails (RFC 7807 body) or a bare status line.
        let is_auth = match &err {
            jmap_client::Error::Problem(p) => p.status() == Some(401),
            jmap_client::Error::Server(s) => s.starts_with("401"),
            _ => false,
        };
        if is_auth {
            Self::Authentication("JMAP server rejected credentials (HTTP 401)".into())
        } else {
            Self::Client(err)
        }
    }
}

impl JmapError {
    /// `Email/queryChanges` / `Email/changes` cannot resume from the stored
    /// token; the caller clears the cursor and full-queries.
    #[must_use]
    pub fn is_stale_query_state(&self) -> bool {
        match self {
            Self::Method { code, .. } => {
                code.eq_ignore_ascii_case("cannotCalculateChanges")
                    || code.eq_ignore_ascii_case("cannotCalculateChangesFrom")
            }
            Self::Client(jmap_client::Error::Method(m)) => {
                m.error() == &MethodErrorType::CannotCalculateChanges
            }
            _ => false,
        }
    }

    /// Transient failure worth retrying with backoff: transport/timeout,
    /// 5xx/429, `serverUnavailable`/`serverPartialFail`/`tooManyChanges`
    /// method errors, `rateLimit`/`overQuota` set errors.
    // Consumed by the sync loop's retry/backoff wiring (Task 2).
    #[allow(dead_code)]
    #[must_use]
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Http(_) => true,
            Self::Client(err) => match err {
                jmap_client::Error::Transport(_) => true,
                jmap_client::Error::Server(status) => {
                    status.starts_with("429") || status.starts_with('5')
                }
                jmap_client::Error::Problem(p) => p.status().is_some_and(|s| s == 429 || s >= 500),
                jmap_client::Error::Method(m) => matches!(
                    m.error(),
                    MethodErrorType::ServerUnavailable
                        | MethodErrorType::ServerPartialFail
                        | MethodErrorType::TooManyChanges
                ),
                jmap_client::Error::Set(s) => {
                    matches!(s.error(), SetErrorType::RateLimit | SetErrorType::OverQuota)
                }
                _ => false,
            },
            _ => false,
        }
    }

    /// Authentication/authorization failure; callers evict the cached session.
    // Consumed by the sync loop's session-eviction wiring (Task 2).
    #[allow(dead_code)]
    #[must_use]
    pub fn is_auth(&self) -> bool {
        match self {
            Self::Authentication(_) => true,
            Self::Client(jmap_client::Error::Problem(p)) => {
                matches!(p.status(), Some(401 | 403))
            }
            Self::Client(jmap_client::Error::Server(s)) => {
                s.starts_with("401") || s.starts_with("403")
            }
            _ => false,
        }
    }
}

// ── Lyra DTOs (persistence boundary; moved from jmap.rs) ────────────

/// A JMAP Mailbox object.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JmapMailbox {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub total_emails: Option<u64>,
    #[serde(default)]
    pub unread_emails: Option<u64>,
    #[serde(default)]
    pub sort_order: Option<u32>,
}

/// A JMAP Email object (partial, only the fields we need).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JmapEmail {
    pub id: String,
    #[serde(default)]
    pub blob_id: Option<String>,
    #[serde(default)]
    pub thread_id: Option<String>,
    #[serde(default)]
    pub mailbox_ids: Option<serde_json::Value>,
    #[serde(default)]
    pub keywords: Option<serde_json::Value>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub received_at: Option<String>,
    #[serde(default)]
    pub message_id: Option<Vec<String>>,
    #[serde(default)]
    pub in_reply_to: Option<Vec<String>>,
    #[serde(default)]
    pub references: Option<Vec<String>>,
    #[serde(default)]
    pub sender: Option<Vec<JmapEmailAddress>>,
    #[serde(default)]
    pub from: Option<Vec<JmapEmailAddress>>,
    #[serde(default)]
    pub to: Option<Vec<JmapEmailAddress>>,
    #[serde(default)]
    pub cc: Option<Vec<JmapEmailAddress>>,
    #[serde(default)]
    pub bcc: Option<Vec<JmapEmailAddress>>,
    #[serde(default)]
    pub reply_to: Option<Vec<JmapEmailAddress>>,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub body_structure: Option<serde_json::Value>,
    #[serde(default)]
    pub body_values: Option<serde_json::Map<String, serde_json::Value>>,
    #[serde(default)]
    pub text_body: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    pub html_body: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    pub has_attachment: Option<bool>,
    #[serde(default)]
    pub attachments: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    pub preview: Option<String>,
}

/// A JMAP email address.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct JmapEmailAddress {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
}

impl JmapEmail {
    /// Extract the plain-text body from bodyValues.
    pub fn body_text(&self) -> Option<String> {
        extract_body_part(self, "text/plain")
    }

    /// Extract the HTML body from bodyValues (unsanitized; persist via `persist_body_html`).
    pub fn body_html(&self) -> Option<String> {
        extract_body_part(self, "text/html")
    }

    /// Get the `from` address as a formatted string.
    pub fn format_from(&self) -> Option<String> {
        self.from
            .as_ref()
            .and_then(|addrs| addrs.first())
            .map(|a| match (&a.name, &a.email) {
                (Some(name), Some(email)) => format!("{name} <{email}>"),
                (None, Some(email)) => email.clone(),
                (Some(name), None) => name.clone(),
                _ => String::new(),
            })
    }

    /// Get the `to` addresses as a formatted string.
    pub fn to_string_list(&self) -> Option<String> {
        self.to.as_ref().map(|addrs| {
            addrs
                .iter()
                .map(|a| match (&a.name, &a.email) {
                    (Some(name), Some(email)) => format!("{name} <{email}>"),
                    (None, Some(email)) => email.clone(),
                    (Some(name), None) => name.clone(),
                    _ => String::new(),
                })
                .collect::<Vec<_>>()
                .join(", ")
        })
    }

    /// Get the first Message-ID header value.
    pub fn message_id_header(&self) -> Option<String> {
        self.message_id
            .as_ref()
            .and_then(|ids| ids.first().cloned())
    }

    /// Whether the email has the `$seen` keyword (read).
    pub fn is_seen(&self) -> bool {
        self.keywords
            .as_ref()
            .and_then(|k| k.get("$seen"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    }

    /// Whether the email has the `$flagged` keyword (starred).
    pub fn is_flagged(&self) -> bool {
        self.keywords
            .as_ref()
            .and_then(|k| k.get("$flagged"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    }
}

/// Extract body text from `bodyValues` using the part type.
fn extract_body_part(email: &JmapEmail, content_type: &str) -> Option<String> {
    let body_parts = if content_type == "text/plain" {
        email.text_body.as_ref()?
    } else {
        email.html_body.as_ref()?
    };

    let part_id = body_parts.first()?.get("partId")?.as_str()?;
    let body_values = email.body_values.as_ref()?;
    let value = body_values.get(part_id)?;
    value.get("value")?.as_str().map(String::from)
}

/// Decrypt the stored credential for a JMAP account (password or Bearer token).
pub fn decrypt_account_password(credential_json: &str, dek: &[u8]) -> Result<String, JmapError> {
    let encrypted: EncryptedCredential = serde_json::from_str(credential_json)
        .map_err(|e| JmapError::InvalidResponse(format!("invalid credential blob: {e}")))?;

    let plaintext = crypto::decrypt(dek, &encrypted)?;

    String::from_utf8(plaintext)
        .map_err(|e| JmapError::InvalidResponse(format!("credential not valid UTF-8: {e}")))
}

// ── Discovery security ──────────────────────────────────────────────

/// Resolve one discovery redirect hop: `location` (possibly relative) against
/// the URL that produced it, rejecting any hop that leaves `origin`.
///
/// Pure decision function so the credential-pinning policy is unit-testable
/// without a network.
pub(crate) fn resolve_discovery_redirect(
    current_url: &str,
    location: &str,
    origin: &str,
) -> Result<String, JmapError> {
    let base = reqwest::Url::parse(current_url).map_err(|e| {
        JmapError::SessionDiscovery(format!("invalid current URL '{current_url}': {e}"))
    })?;
    let resolved = base.join(location).map_err(|e| {
        JmapError::SessionDiscovery(format!("invalid redirect target '{location}': {e}"))
    })?;
    let resolved = resolved.as_str().to_string();
    let target_origin = crate::netsec::origin_of(&resolved).map_err(JmapError::InvalidServerUrl)?;
    if target_origin != origin {
        tracing::warn!(
            target_origin = %target_origin,
            expected_origin = %origin,
            "JMAP: discovery redirect leaves the configured origin; refusing to follow"
        );
        return Err(JmapError::CrossOrigin(resolved));
    }
    Ok(resolved)
}

/// Pin every credential-bearing session URL to the configured origin.
///
/// `urls` are the session's `apiUrl` / `uploadUrl` / `downloadUrl` /
/// `eventSourceUrl`; empty entries are skipped defensively.
// Called from `JmapSeam::connect`/`refresh_if_stale` (Task 2 wiring).
#[allow(dead_code)]
fn pin_session_urls(origin: &str, urls: &[&str]) -> Result<(), JmapError> {
    for url in urls.iter().filter(|u| !u.is_empty()) {
        let target = crate::netsec::origin_of(url).map_err(JmapError::InvalidServerUrl)?;
        if target != origin {
            tracing::warn!(
                target_origin = %target,
                expected_origin = %origin,
                "JMAP: session URL points at a different origin; refusing to send credentials"
            );
            return Err(JmapError::CrossOrigin((*url).to_owned()));
        }
    }
    Ok(())
}

/// `auth_type = "bearer"` (e.g. Fastmail API tokens) selects Bearer; anything
/// else is Basic with the account password.
#[allow(dead_code)] // called from `JmapSeam::connect` (Task 2 wiring)
fn credentials_for(auth_type: &str, email: &str, secret: &str) -> Credentials {
    if auth_type.eq_ignore_ascii_case("bearer") {
        Credentials::bearer(secret)
    } else {
        Credentials::basic(email, secret)
    }
}

/// Authorization header value for our own pre-resolution GETs. `Credentials`
/// stores the Basic value pre-encoded (jmap-client `src/client.rs`).
#[allow(dead_code)] // called from `preflight_discovery` (Task 2 wiring)
fn authorization_header(credentials: &Credentials) -> String {
    match credentials {
        Credentials::Basic(encoded) => format!("Basic {encoded}"),
        Credentials::Bearer(token) => format!("Bearer {token}"),
    }
}

/// Pre-validate the `/.well-known/jmap` redirect chain with our own
/// same-origin follower (the crate's `connect()` re-fetches the session
/// itself afterwards). Returns whether any redirect hop occurred.
///
/// Non-2xx final statuses are *not* an error here: `connect()` produces the
/// typed 401/problem error for them. Only the chain's shape matters.
#[allow(dead_code)] // called from `JmapSeam::connect` (Task 2 wiring)
async fn preflight_discovery(
    base: &str,
    auth_header: &str,
    origin: &str,
) -> Result<bool, JmapError> {
    let http = reqwest::Client::builder()
        .timeout(JMAP_TIMEOUT)
        // Automatic following stays disabled: every hop is checked below.
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let mut url = format!("{base}/.well-known/jmap");
    let mut redirected = false;
    for _hop in 0..MAX_DISCOVERY_HOPS {
        let resp = http
            .get(&url)
            .header(reqwest::header::AUTHORIZATION, auth_header)
            .send()
            .await?;
        if !resp.status().is_redirection() {
            return Ok(redirected);
        }
        redirected = true;
        let location = resp
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| {
                JmapError::SessionDiscovery(format!("redirect from {url} has no Location"))
            })?;
        url = resolve_discovery_redirect(&url, location, origin)?;
    }
    Err(JmapError::SessionDiscovery(format!(
        "too many redirects from {base}/.well-known/jmap"
    )))
}

// ── The seam client ─────────────────────────────────────────────────

/// A connected JMAP session pinned to its configured origin.
///
/// Consumed by the sync loop, send path, and push wiring from Task 2 on.
#[allow(dead_code)]
pub(crate) struct JmapSeam {
    client: Client,
    origin: String,
}

#[allow(dead_code)] // seam surface is wired into callers over Tasks 2–7
impl JmapSeam {
    /// Discover + connect (no caching). Pre-resolves well-known redirects
    /// with the same-origin follower, then pins the session URLs.
    async fn connect(
        base_url: &str,
        email: &str,
        secret: &str,
        auth_type: &str,
    ) -> Result<Self, JmapError> {
        crate::netsec::validate_server_url(base_url).map_err(JmapError::InvalidServerUrl)?;
        let trimmed = base_url.trim_end_matches('/');
        let base = trimmed.strip_suffix("/.well-known/jmap").unwrap_or(trimmed);
        let origin = crate::netsec::origin_of(base).map_err(JmapError::InvalidServerUrl)?;
        let host = reqwest::Url::parse(base)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned))
            .ok_or_else(|| JmapError::InvalidServerUrl(format!("no host in '{base}'")))?;

        let credentials = credentials_for(auth_type, email, secret);
        let redirected =
            preflight_discovery(base, &authorization_header(&credentials), &origin).await?;

        let mut builder = Client::new().credentials(credentials).timeout(JMAP_TIMEOUT);
        if redirected {
            // The crate's redirect policy is host-scoped and denies all hosts
            // by default (`Client::redirect_policy`). Our follower already
            // validated this exact chain origin-scoped; allow precisely the
            // configured host so connect() can re-follow it. The allowlist
            // stays empty for non-redirecting servers.
            builder = builder.follow_redirects([host]);
        }
        let mut client = builder.connect(base).await?;

        let session = client.session();
        pin_session_urls(
            &origin,
            &[
                session.api_url(),
                session.upload_url(),
                session.download_url(),
                session.event_source_url(),
            ],
        )?;
        // `connect()` defaults to an arbitrary first primary account (hash-map
        // order); pin the *mail* primary account instead.
        let mail_account = session
            .primary_accounts()
            .find(|(uri, _)| uri.as_str() == URI::Mail.as_ref())
            .map(|(_, id)| id.clone())
            .or_else(|| session.accounts().next().cloned())
            .ok_or_else(|| JmapError::SessionDiscovery("no mail account in JMAP session".into()))?;
        client.set_default_account_id(mail_account);

        Ok(Self { client, origin })
    }

    /// Cached connect for a Lyra account: one session per account per process.
    pub(crate) async fn connect_for_account(
        account_id: &str,
        base_url: &str,
        email: &str,
        secret: &str,
        auth_type: &str,
    ) -> Result<Arc<Self>, JmapError> {
        if let Some(seam) = lock_cache().get(account_id) {
            return Ok(Arc::clone(seam));
        }
        let seam = Arc::new(Self::connect(base_url, email, secret, auth_type).await?);
        lock_cache().insert(account_id.to_owned(), Arc::clone(&seam));
        Ok(seam)
    }

    /// Uncached connect (account probe before the account row exists).
    pub(crate) async fn connect_ephemeral(
        base_url: &str,
        email: &str,
        secret: &str,
        auth_type: &str,
    ) -> Result<Self, JmapError> {
        Self::connect(base_url, email, secret, auth_type).await
    }

    /// Drop the cached session (auth failure, credential change, delete).
    pub(crate) fn evict(account_id: &str) {
        lock_cache().remove(account_id);
    }

    /// Refresh the session when a response reported a newer `sessionState`;
    /// the refreshed URLs are pinned again.
    pub(crate) async fn refresh_if_stale(&self) -> Result<(), JmapError> {
        if !self.client.is_session_updated() {
            self.client.refresh_session().await?;
            let session = self.client.session();
            pin_session_urls(
                &self.origin,
                &[
                    session.api_url(),
                    session.upload_url(),
                    session.download_url(),
                    session.event_source_url(),
                ],
            )?;
        }
        Ok(())
    }

    /// True when `urn:ietf:params:jmap:submission` is advertised (RFC 8621).
    pub(crate) fn supports_submission(&self) -> bool {
        self.client
            .session()
            .has_capability(URI::Submission.as_ref())
    }
}

#[allow(dead_code)] // session cache is exercised via `JmapSeam` (Task 2 wiring)
static CLIENT_CACHE: OnceLock<Mutex<HashMap<String, Arc<JmapSeam>>>> = OnceLock::new();

#[allow(dead_code)]
fn lock_cache() -> MutexGuard<'static, HashMap<String, Arc<JmapSeam>>> {
    CLIENT_CACHE
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

// ── Crate → DTO mapping ─────────────────────────────────────────────

/// Map a crate `Email<Get>` onto the Lyra DTO that `sync/store.rs` persists.
///
/// `keywords`/`mailbox_ids` keep the old DTO shape (JSON object with `true`
/// values); the crate exposes only set keys, which is the same information.
#[allow(dead_code)] // consumed by the sync loop rewrite (Task 2)
fn map_email(email: &Email<Get>) -> JmapEmail {
    let keywords = email.keywords();
    let keywords = if keywords.is_empty() {
        None
    } else {
        Some(serde_json::Value::Object(
            keywords
                .into_iter()
                .map(|k| (k.to_owned(), serde_json::Value::Bool(true)))
                .collect(),
        ))
    };
    let mailbox_ids = email.mailbox_ids();
    let mailbox_ids = if mailbox_ids.is_empty() {
        None
    } else {
        Some(serde_json::Value::Object(
            mailbox_ids
                .into_iter()
                .map(|m| (m.to_owned(), serde_json::Value::Bool(true)))
                .collect(),
        ))
    };
    JmapEmail {
        id: email.id().unwrap_or_default().to_owned(),
        blob_id: email.blob_id().map(str::to_owned),
        thread_id: email.thread_id().map(str::to_owned),
        mailbox_ids,
        keywords,
        size: Some(email.size() as u64),
        received_at: email
            .received_at()
            .and_then(|ts| chrono::DateTime::from_timestamp(ts, 0))
            .map(|dt| dt.to_rfc3339()),
        message_id: email.message_id().map(<[String]>::to_vec),
        in_reply_to: email.in_reply_to().map(<[String]>::to_vec),
        references: email.references().map(<[String]>::to_vec),
        sender: email.sender().map(map_addresses),
        from: email.from().map(map_addresses),
        to: email.to().map(map_addresses),
        cc: email.cc().map(map_addresses),
        bcc: email.bcc().map(map_addresses),
        reply_to: email.reply_to().map(map_addresses),
        subject: email.subject().map(str::to_owned),
        body_structure: None, // never read by store.rs
        body_values: map_body_values(email),
        text_body: map_body_refs(email.text_body()),
        html_body: map_body_refs(email.html_body()),
        has_attachment: Some(email.has_attachment()),
        attachments: None, // superseded by typed attachment meta (Task 3)
        preview: email.preview().map(str::to_owned),
    }
}

#[allow(dead_code)] // helper of `map_email` (Task 2)
fn map_addresses(addrs: &[EmailAddress]) -> Vec<JmapEmailAddress> {
    addrs
        .iter()
        .map(|a| JmapEmailAddress {
            name: a.name().map(str::to_owned),
            email: Some(a.email().to_owned()),
        })
        .collect()
}

/// Rebuild the `bodyValues` JSON map (`partId → {value, isTruncated}`) that
/// `extract_body_part` reads, from the crate's keyed accessor.
#[allow(dead_code)] // helper of `map_email` (Task 2)
fn map_body_values(email: &Email<Get>) -> Option<serde_json::Map<String, serde_json::Value>> {
    let mut map = serde_json::Map::new();
    for part in email
        .text_body()
        .into_iter()
        .flatten()
        .chain(email.html_body().into_iter().flatten())
    {
        if let Some(part_id) = part.part_id()
            && let Some(value) = email.body_value(part_id)
        {
            map.insert(
                part_id.to_owned(),
                serde_json::json!({ "value": value.value(), "isTruncated": value.is_truncated() }),
            );
        }
    }
    if map.is_empty() { None } else { Some(map) }
}

/// `textBody`/`htmlBody` as `[{partId, type}]` JSON (the DTO's wire shape).
#[allow(dead_code)] // helper of `map_email` (Task 2)
fn map_body_refs(parts: Option<&[EmailBodyPart]>) -> Option<Vec<serde_json::Value>> {
    parts.map(|ps| {
        ps.iter()
            .map(|p| serde_json::json!({ "partId": p.part_id(), "type": p.content_type() }))
            .collect()
    })
}

/// Map a crate `Mailbox<Get>`; `None` when the server row has no id.
/// `Role::Junk` normalizes to Lyra's `"spam"` vocabulary.
#[allow(dead_code)] // consumed by the sync loop rewrite (Task 2)
fn map_mailbox(mb: &Mailbox<Get>) -> Option<JmapMailbox> {
    let id = mb.id()?.to_owned();
    let role = match mb.role() {
        Role::Inbox => Some("inbox".into()),
        Role::Sent => Some("sent".into()),
        Role::Trash => Some("trash".into()),
        Role::Drafts => Some("drafts".into()),
        Role::Junk => Some("spam".into()),
        Role::Archive => Some("archive".into()),
        Role::Important => Some("important".into()),
        Role::Other(other) => Some(other.clone()),
        Role::None => None,
    };
    Some(JmapMailbox {
        id,
        name: mb.name().unwrap_or_default().to_owned(),
        role,
        parent_id: mb.parent_id().map(str::to_owned),
        total_emails: Some(mb.total_emails() as u64),
        unread_emails: Some(mb.unread_emails() as u64),
        sort_order: Some(mb.sort_order()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use jmap_client::core::error::{MethodError, MethodErrorType, ProblemDetails, ProblemType};

    // ── redirect pre-resolution (moved from jmap.rs) ────────────────

    #[test]
    fn discovery_redirect_relative_same_origin_accepted() {
        let resolved = resolve_discovery_redirect(
            "https://api.fastmail.com/.well-known/jmap",
            "/jmap/session",
            "https://api.fastmail.com:443",
        )
        .expect("same-origin relative redirect must be followed");
        assert_eq!(resolved, "https://api.fastmail.com/jmap/session");
    }

    #[test]
    fn discovery_redirect_absolute_same_origin_accepted() {
        let resolved = resolve_discovery_redirect(
            "https://jmap.example.com/.well-known/jmap",
            "https://jmap.example.com/jmap/session",
            "https://jmap.example.com:443",
        )
        .expect("same-origin absolute redirect must be followed");
        assert_eq!(resolved, "https://jmap.example.com/jmap/session");
    }

    #[test]
    fn discovery_redirect_cross_origin_rejected() {
        let err = resolve_discovery_redirect(
            "https://jmap.example.com/.well-known/jmap",
            "https://evil.example/session",
            "https://jmap.example.com:443",
        )
        .unwrap_err();
        assert!(matches!(err, JmapError::CrossOrigin(_)), "got: {err}");
    }

    #[test]
    fn discovery_redirect_scheme_downgrade_rejected() {
        let err = resolve_discovery_redirect(
            "https://jmap.example.com/.well-known/jmap",
            "http://jmap.example.com/jmap/session",
            "https://jmap.example.com:443",
        )
        .unwrap_err();
        assert!(matches!(err, JmapError::CrossOrigin(_)), "got: {err}");
    }

    #[test]
    fn discovery_redirect_garbage_location_rejected() {
        let err = resolve_discovery_redirect(
            "https://jmap.example.com/.well-known/jmap",
            "http://[::1",
            "https://jmap.example.com:443",
        )
        .unwrap_err();
        assert!(matches!(err, JmapError::SessionDiscovery(_)), "got: {err}");
    }

    // ── session URL origin pinning ──────────────────────────────────

    #[test]
    fn session_url_pinning_accepts_same_origin() {
        pin_session_urls(
            "https://jmap.example.com:443",
            &[
                "https://jmap.example.com/api/",
                "https://jmap.example.com/upload/{accountId}",
                "https://jmap.example.com/download/{accountId}/{blobId}/{name}/{type}",
                "https://jmap.example.com/events/?types={types}&closeafter={closeafter}&ping={ping}",
            ],
        )
        .unwrap();
    }

    #[test]
    fn session_url_pinning_rejects_cross_origin_and_garbage() {
        // A malicious JMAP server pointing uploadUrl elsewhere must never
        // receive our Authorization header.
        let err = pin_session_urls(
            "https://jmap.example.com:443",
            &[
                "https://jmap.example.com/api/",
                "https://evil.example/upload/",
            ],
        )
        .unwrap_err();
        assert!(matches!(err, JmapError::CrossOrigin(_)), "got: {err}");
        // https → http on the same host is a different origin.
        assert!(
            pin_session_urls(
                "https://jmap.example.com:443",
                &["http://jmap.example.com/api/"]
            )
            .is_err()
        );
        // Unparseable URL.
        assert!(pin_session_urls("https://jmap.example.com:443", &["not a url"]).is_err());
    }

    // ── credentials ─────────────────────────────────────────────────

    #[test]
    fn bearer_auth_type_selects_bearer_credential() {
        let creds = credentials_for("bearer", "u@example.com", "api-token");
        assert_eq!(authorization_header(&creds), "Bearer api-token");
        // case-insensitive
        let creds = credentials_for("Bearer", "u@example.com", "api-token");
        assert_eq!(authorization_header(&creds), "Bearer api-token");
    }

    #[test]
    fn password_auth_type_selects_basic_credential() {
        use base64::Engine as _;
        let creds = credentials_for("password", "u@example.com", "pw");
        let expected = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode("u@example.com:pw")
        );
        assert_eq!(authorization_header(&creds), expected);
    }

    // ── error classification ────────────────────────────────────────

    #[test]
    fn stale_query_state_detects_rfc_code() {
        let err = JmapError::from(jmap_client::Error::Method(MethodError {
            p_type: MethodErrorType::CannotCalculateChanges,
        }));
        assert!(err.is_stale_query_state());
        // Legacy string-matched arm (hand-rolled transport until Task 7).
        let legacy = JmapError::Method {
            code: "cannotCalculateChanges".into(),
            description: String::new(),
        };
        assert!(legacy.is_stale_query_state());
        assert!(!JmapError::InvalidResponse("nope".into()).is_stale_query_state());
    }

    #[test]
    fn transient_classification_matrix() {
        let unavailable = JmapError::from(jmap_client::Error::Method(MethodError {
            p_type: MethodErrorType::ServerUnavailable,
        }));
        assert!(unavailable.is_transient());

        let server_500 = JmapError::from(jmap_client::Error::Server(
            "500 Internal Server Error".into(),
        ));
        assert!(server_500.is_transient());

        let problem_429 =
            JmapError::from(jmap_client::Error::Problem(Box::new(ProblemDetails::new(
                ProblemType::Other("slowDown".into()),
                Some(429),
                None,
                None,
                None,
                None,
            ))));
        assert!(problem_429.is_transient());

        let rate_limited: jmap_client::Error = serde_json::from_value::<
            jmap_client::core::set::SetError<String>,
        >(serde_json::json!({"type": "rateLimit"}))
        .unwrap()
        .into();
        assert!(JmapError::from(rate_limited).is_transient());

        // Permanent classifications.
        let invalid = JmapError::from(jmap_client::Error::Method(MethodError {
            p_type: MethodErrorType::InvalidArguments,
        }));
        assert!(!invalid.is_transient());
        assert!(!JmapError::InvalidResponse("nope".into()).is_transient());
        assert!(!JmapError::SessionDiscovery("nope".into()).is_transient());
    }

    #[test]
    fn auth_classification_maps_401_to_authentication() {
        let problem_401 =
            JmapError::from(jmap_client::Error::Problem(Box::new(ProblemDetails::new(
                ProblemType::Other("unauthorized".into()),
                Some(401),
                None,
                None,
                None,
                None,
            ))));
        assert!(
            matches!(problem_401, JmapError::Authentication(_)),
            "401 ProblemDetails must map to Authentication, got: {problem_401:?}"
        );
        assert!(problem_401.is_auth());

        let server_401 = JmapError::from(jmap_client::Error::Server("401 Unauthorized".into()));
        assert!(matches!(server_401, JmapError::Authentication(_)));
        assert!(server_401.is_auth());

        assert!(!JmapError::SessionDiscovery("x".into()).is_auth());
    }

    // ── credential decrypt (moved from jmap.rs) ─────────────────────

    #[test]
    fn decrypt_roundtrip() {
        let key = crypto::generate_key();
        let password = "jmap-test-password";
        let encrypted = crypto::encrypt(&key, password.as_bytes()).unwrap();
        let json = serde_json::to_string(&encrypted).unwrap();
        let decrypted = decrypt_account_password(&json, &key).unwrap();
        assert_eq!(decrypted, password);
    }

    // ── crate → DTO mapping ─────────────────────────────────────────

    #[test]
    fn map_email_maps_keywords_thread_body_and_addresses() {
        let crate_email: Email<Get> = serde_json::from_value(serde_json::json!({
            "id": "em1",
            "threadId": "th1",
            "mailboxIds": { "mb1": true },
            "keywords": { "$seen": true },
            "size": 12345,
            "receivedAt": "2025-01-15T10:00:00Z",
            "messageId": ["<msg1@example.com>"],
            "from": [{ "name": "Alice", "email": "alice@example.com" }],
            "to": [{ "name": "Bob", "email": "bob@example.com" }],
            "subject": "Hello!",
            "preview": "Hi Bob, ...",
            "hasAttachment": false,
            "bodyValues": { "p1": { "value": "Hello world" } },
            "textBody": [{ "partId": "p1", "type": "text/plain" }],
            "htmlBody": []
        }))
        .unwrap();

        let mapped = map_email(&crate_email);
        assert_eq!(mapped.id, "em1");
        assert_eq!(mapped.thread_id.as_deref(), Some("th1"));
        assert!(mapped.is_seen());
        assert!(!mapped.is_flagged());
        assert_eq!(
            mapped.format_from().as_deref(),
            Some("Alice <alice@example.com>")
        );
        assert_eq!(
            mapped.to_string_list().as_deref(),
            Some("Bob <bob@example.com>")
        );
        assert_eq!(mapped.subject.as_deref(), Some("Hello!"));
        assert_eq!(mapped.body_text().as_deref(), Some("Hello world"));
        assert_eq!(mapped.body_html(), None);
        assert_eq!(
            mapped.received_at.as_deref(),
            Some("2025-01-15T10:00:00+00:00")
        );
        assert_eq!(
            mapped.message_id_header().as_deref(),
            Some("<msg1@example.com>")
        );
        assert_eq!(mapped.size, Some(12345));
    }

    #[test]
    fn map_email_empty_keywords_stay_absent() {
        let crate_email: Email<Get> =
            serde_json::from_value(serde_json::json!({ "id": "em2", "keywords": {} })).unwrap();
        let mapped = map_email(&crate_email);
        assert!(!mapped.is_seen());
        assert!(mapped.keywords.is_none());
    }

    #[test]
    fn map_mailbox_normalizes_junk_to_lyra_spam_role() {
        // The crate's `Role` deserializer borrows &str, so it needs a
        // borrowing deserializer (`from_str`), not the owning `from_value`.
        let junk: Mailbox<Get> = serde_json::from_str(
            r#"{"id": "mb-junk", "name": "Junk", "role": "junk", "totalEmails": 3}"#,
        )
        .unwrap();
        let mapped = map_mailbox(&junk).unwrap();
        assert_eq!(mapped.id, "mb-junk");
        // Lyra's role vocabulary is spam (move_message_to_role queries "spam").
        assert_eq!(mapped.role.as_deref(), Some("spam"));
        assert_eq!(mapped.total_emails, Some(3));
    }

    #[test]
    fn map_mailbox_handles_missing_role_and_parent() {
        let mb: Mailbox<Get> = serde_json::from_value(serde_json::json!({
            "id": "mb2",
            "name": "Projects",
            "parentId": "mb1"
        }))
        .unwrap();
        let mapped = map_mailbox(&mb).unwrap();
        assert_eq!(mapped.role, None);
        assert_eq!(mapped.parent_id.as_deref(), Some("mb1"));
        // A mailbox without a server id is skipped, not persisted.
        let no_id: Mailbox<Get> =
            serde_json::from_value(serde_json::json!({ "name": "Ghost" })).unwrap();
        assert!(map_mailbox(&no_id).is_none());
    }
}
