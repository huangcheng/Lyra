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

use futures_util::StreamExt;
use jmap_client::client::{Client, Credentials};
use jmap_client::core::error::MethodErrorType;
use jmap_client::core::get::GetRequest;
use jmap_client::core::query::{QueryRequest, QueryResponse};
use jmap_client::core::query_changes::QueryChangesResponse;
use jmap_client::core::response::{EmailChangesResponse, EmailGetResponse, MailboxGetResponse};
use jmap_client::core::set::SetErrorType;
use jmap_client::email::{self, Email, EmailAddress, EmailBodyPart, Property};
use jmap_client::event_source::PushNotification;
use jmap_client::identity::Identity;
use jmap_client::mailbox::{Mailbox, Role};
use jmap_client::{DataType, Get, Set, URI};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::crypto::{self, EncryptedCredential};
use crate::smtp::OutboundMessage;

/// Whole-request timeout for JMAP API calls (matches the legacy client).
const JMAP_TIMEOUT: Duration = Duration::from_secs(30);
/// Redirect hops accepted during well-known pre-resolution.
const MAX_DISCOVERY_HOPS: u32 = 5;
/// `Email/changes` page size and page bound (a page is ≤ this many ids).
const CHANGES_PAGE_SIZE: usize = 500;
const CHANGES_MAX_PAGES: usize = 8;
/// Cap on returned body values (mirrors the IMAP lazy-fetch body cap).
const MAX_BODY_VALUE_BYTES: usize = 25 * 1024 * 1024;

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
        // The string sniff on `Error::Server` depends on jmap-client 0.4.2's
        // `StatusCode` Display format ("401 Unauthorized"); the crate is
        // version-pinned — revisit these matches on any upgrade.
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
    // Consumed by the send-path error classification in `plugins/jmap_send.rs`
    // ("JMAP transient" feeds the capped-backoff reschedule in jobs.rs). The
    // `Error::Server` status sniff depends on jmap-client 0.4.2's `StatusCode`
    // Display format (version-pinned; revisit on crate upgrade).
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
    // The `Error::Server` status sniff depends on jmap-client 0.4.2's
    // `StatusCode` Display format (version-pinned; revisit on crate upgrade).
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
    /// Typed attachment locators for blob download (built by `map_attachments`).
    #[serde(default)]
    pub attachments_meta: Vec<JmapAttachmentMeta>,
}

/// A JMAP email address.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct JmapEmailAddress {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
}

/// Attachment locator for blob download (RFC 8621 EmailBodyPart subset).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct JmapAttachmentMeta {
    pub blob_id: String,
    pub filename: String,
    pub content_type: String,
    pub size: u64,
    pub content_id: Option<String>,
    pub is_inline: bool,
}

/// Incremental result from `Email/queryChanges` (moved from jmap.rs).
#[derive(Debug, Clone)]
pub(crate) struct EmailQueryChanges {
    pub(crate) added_ids: Vec<String>,
    pub(crate) removed_ids: Vec<String>,
    pub(crate) new_query_state: Option<String>,
}

/// One page of a (batched) `Email/query` + `Email/get`.
#[derive(Debug)]
pub(crate) struct EmailPage {
    /// Ids from the query (paging is driven by these, not the fetched list).
    pub(crate) ids: Vec<String>,
    pub(crate) emails: Vec<JmapEmail>,
    /// Folder cursor (`queryState`), committed only with the last page.
    pub(crate) query_state: Option<String>,
    /// Account-level `Email` state from the get response (`Email/changes` input).
    pub(crate) email_state: Option<String>,
}

/// Account-level `Email/changes` outcome.
#[derive(Debug)]
pub(crate) struct JmapEmailChanges {
    pub(crate) updated_ids: Vec<String>,
    pub(crate) destroyed_ids: Vec<String>,
    pub(crate) new_state: Option<String>,
}

/// Outcome of waiting on a JMAP EventSource stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventSourceOutcome {
    StateChanged,
    Unsupported,
    StreamEnded,
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
fn credentials_for(auth_type: &str, email: &str, secret: &str) -> Credentials {
    if auth_type.eq_ignore_ascii_case("bearer") {
        Credentials::bearer(secret)
    } else {
        Credentials::basic(email, secret)
    }
}

/// Authorization header value for our own pre-resolution GETs. `Credentials`
/// stores the Basic value pre-encoded (jmap-client `src/client.rs`).
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
pub(crate) struct JmapSeam {
    client: Client,
    origin: String,
}

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
    #[allow(dead_code)] // account-probe wiring lands in a later task
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

    /// `maxCallsInRequest` capability; conservative default when the session's
    /// core capabilities fail to type-check (untagged `Capabilities` is
    /// all-or-nothing per required field set).
    pub(crate) fn max_calls_in_request(&self) -> usize {
        match self.client.session().core_capabilities() {
            Some(c) => c.max_calls_in_request(),
            None => 8,
        }
    }

    /// List all mailboxes (folders) for this account (`Mailbox/get`, ids omitted = all).
    pub(crate) async fn list_mailboxes(&self) -> Result<Vec<JmapMailbox>, JmapError> {
        let mut request = self.client.build();
        request.get_mailbox();
        let mut resp = request.send_single::<MailboxGetResponse>().await?;
        Ok(resp.take_list().iter().filter_map(map_mailbox).collect())
    }

    /// One `Email/query` page with the matching `Email/get` batched into the
    /// same request via a `/ids` result reference (one round trip per page).
    /// Splits into two requests when `maxCallsInRequest < 2`.
    pub(crate) async fn query_emails_page(
        &self,
        mailbox_id: &str,
        position: usize,
        limit: usize,
    ) -> Result<EmailPage, JmapError> {
        let position = i32::try_from(position).unwrap_or(i32::MAX);
        if self.max_calls_in_request() < 2 {
            let mut query_req = self.client.build();
            fill_email_query(query_req.query_email(), mailbox_id, position, limit);
            let mut query_resp = query_req.send_single::<QueryResponse>().await?;
            let ids = query_resp.take_ids();
            let query_state = query_resp.take_query_state();
            let (emails, email_state) = self.get_emails(&ids).await?;
            return Ok(EmailPage {
                ids,
                emails,
                query_state: Some(query_state),
                email_state,
            });
        }

        let mut request = self.client.build();
        let ids_ref = {
            let q = request.query_email();
            fill_email_query(q, mailbox_id, position, limit);
            q.result_reference()
        };
        {
            let g = request.get_email();
            g.ids_ref(ids_ref);
            fill_email_get(g);
        }
        let mut responses = request.send().await?.unwrap_method_responses();
        if responses.len() != 2 {
            return Err(JmapError::InvalidResponse(format!(
                "expected Email/query + Email/get responses, got {}",
                responses.len()
            )));
        }
        // Unwrap the query FIRST: when the query fails, the get's `/ids`
        // result reference fails too, and unwrapping the get first would
        // surface only its `invalidResultReference`, masking the root error.
        let mut query_resp = responses.remove(0).unwrap_query_email()?;
        let mut get_resp = responses.remove(0).unwrap_get_email()?;
        let email_state = get_resp.take_state();
        let emails = get_resp.take_list().iter().filter_map(map_email).collect();
        Ok(EmailPage {
            ids: query_resp.take_ids(),
            emails,
            query_state: Some(query_resp.take_query_state()),
            email_state: Some(email_state),
        })
    }

    /// Incremental mailbox changes since a stored `queryState` (RFC 8621 `Email/queryChanges`).
    pub(crate) async fn query_email_changes(
        &self,
        mailbox_id: &str,
        since_query_state: &str,
    ) -> Result<EmailQueryChanges, JmapError> {
        let mut request = self.client.build();
        {
            let q = request.query_email_changes(since_query_state);
            q.filter(email::query::Filter::in_mailbox(mailbox_id));
            q.sort([email::query::Comparator::received_at().descending()]);
            q.max_changes(CHANGES_PAGE_SIZE);
        }
        let resp = request.send_single::<QueryChangesResponse>().await?;
        Ok(EmailQueryChanges {
            added_ids: resp.added().iter().map(|a| a.id().to_owned()).collect(),
            removed_ids: resp.removed().to_vec(),
            new_query_state: Some(resp.new_query_state().to_owned()),
        })
    }

    /// Account-level `Email/changes`: keyword/mailbox updates and destroys that
    /// per-folder queryChanges cannot see (no membership change). Pages until
    /// `hasMoreChanges` clears or the bound is hit.
    pub(crate) async fn email_changes(
        &self,
        since_state: &str,
    ) -> Result<JmapEmailChanges, JmapError> {
        let mut updated_ids = Vec::new();
        let mut destroyed_ids = Vec::new();
        // `since` always ends at the latest page's state (the loop runs at
        // least once), so `new_state` is just its final value.
        let mut since = since_state.to_owned();
        let mut pages = 0usize;
        loop {
            let mut request = self.client.build();
            request
                .changes_email(since.clone())
                .max_changes(CHANGES_PAGE_SIZE);
            let mut resp = request.send_single::<EmailChangesResponse>().await?;
            updated_ids.extend(resp.take_updated());
            destroyed_ids.extend(resp.take_destroyed());
            since = resp.take_new_state();
            if !resp.has_more_changes() {
                break;
            }
            pages += 1;
            if pages >= CHANGES_MAX_PAGES {
                // The state still advances page by page, so the remaining
                // changes flow in on the next sync — this is a backlog
                // warning, not data loss.
                tracing::warn!(
                    "Email/changes still hasMoreChanges after {CHANGES_MAX_PAGES} pages; \
                     resuming from the latest state next sync"
                );
                break;
            }
        }
        Ok(JmapEmailChanges {
            updated_ids,
            destroyed_ids,
            new_state: Some(since),
        })
    }

    /// Fetch email objects by id with the sync property set.
    /// Returns the emails plus the account-level `Email` state.
    pub(crate) async fn get_emails(
        &self,
        ids: &[String],
    ) -> Result<(Vec<JmapEmail>, Option<String>), JmapError> {
        if ids.is_empty() {
            return Ok((Vec::new(), None));
        }
        let mut request = self.client.build();
        {
            let get = request.get_email();
            get.ids(ids.iter().cloned());
            fill_email_get(get);
        }
        let mut resp = request.send_single::<EmailGetResponse>().await?;
        let state = resp.take_state();
        let emails = resp.take_list().iter().filter_map(map_email).collect();
        Ok((emails, Some(state)))
    }

    /// Download a blob via the session `downloadUrl` (RFC 8620 §6.2; the URL
    /// template was origin-pinned at connect).
    pub(crate) async fn download_blob(&self, blob_id: &str) -> Result<Vec<u8>, JmapError> {
        Ok(self.client.download(blob_id).await?)
    }

    /// Submit an outbound message. Attachments upload to `uploadUrl` first
    /// (blob upload is not a JMAP method call), then ONE batched request:
    /// `Email/set` create `#draft` + `EmailSubmission/set` with `#`
    /// back-references and an on-success patch (move to Sent, clear `$draft`).
    ///
    /// OpenGPG MIME-wrapped outbound (`mime_body` set) goes through
    /// `Email/import` of the uploaded RFC822 blob so the wrapper survives —
    /// an `Email/set` create would rebuild (and destroy) the MIME structure.
    pub(crate) async fn submit_outbound(
        &self,
        outbound: &OutboundMessage,
    ) -> Result<String, JmapError> {
        if !self.supports_submission() {
            return Err(JmapError::SessionDiscovery(
                "JMAP session does not advertise urn:ietf:params:jmap:submission".into(),
            ));
        }

        // Request A: identities + mailboxes (their results shape request B).
        let mut request_a = self.client.build();
        request_a.get_identity();
        request_a.get_mailbox();
        let mut responses = request_a.send().await?.unwrap_method_responses();
        if responses.len() != 2 {
            return Err(JmapError::InvalidResponse(format!(
                "expected Identity/get + Mailbox/get responses, got {}",
                responses.len()
            )));
        }
        let mailboxes = responses.remove(1).unwrap_get_mailbox()?.take_list();
        let identities = responses.remove(0).unwrap_get_identity()?.take_list();

        let identity_id = pick_identity(&identities, &outbound.from_email).ok_or_else(|| {
            JmapError::InvalidResponse(format!("no JMAP identity for {}", outbound.from_email))
        })?;
        let drafts_id = mailbox_id_for_role(&mailboxes, &Role::Drafts);
        let sent_id = mailbox_id_for_role(&mailboxes, &Role::Sent);

        if let Some(mime_body) = &outbound.mime_body {
            return self
                .submit_mime_wrapped(mime_body, &identity_id, drafts_id, sent_id, &mailboxes)
                .await;
        }

        let uploaded = self.upload_attachments(outbound).await?;

        // Request B: create + submit with creation-id references.
        let mut request = self.client.build();
        {
            let email_set = request.set_email();
            let create_boxes: Vec<String> = drafts_id.iter().cloned().collect();
            fill_outbound_email(
                email_set.create_with_id("draft"),
                outbound,
                &create_boxes,
                &uploaded,
            );
        }
        {
            let sub_set = request.set_email_submission();
            sub_set
                .create_with_id("sub")
                .email_id("#draft")
                .identity_id(identity_id.as_str());
            fill_on_success_patch(
                sub_set.arguments().on_success_update_email("sub"),
                sent_id.as_deref(),
            );
        }
        let mut responses = request.send().await?.unwrap_method_responses();
        if responses.len() != 2 {
            return Err(JmapError::InvalidResponse(format!(
                "expected Email/set + EmailSubmission/set responses, got {}",
                responses.len()
            )));
        }
        // Unwrap Email/set FIRST (symmetry with query_emails_page): when it
        // fails, the submission's `#draft` reference fails too, and unwrapping
        // the submission first would mask the root error.
        let mut email_resp = responses.remove(0).unwrap_set_email()?;
        let mut sub_resp = responses.remove(0).unwrap_set_email_submission()?;
        // notCreated surfaces here as Error::Set (→ JmapError::Client).
        email_resp.created("draft")?;
        let mut submission = sub_resp.created("sub")?;
        Ok(submission.take_id())
    }

    /// OpenGPG path: upload the RFC822 MIME blob, `Email/import` into Drafts,
    /// submit — the signed/encrypted MIME wrapper survives.
    async fn submit_mime_wrapped(
        &self,
        mime_body: &str,
        identity_id: &str,
        drafts_id: Option<String>,
        sent_id: Option<String>,
        mailboxes: &[Mailbox<Get>],
    ) -> Result<String, JmapError> {
        let import_mailbox = drafts_id
            .as_deref()
            .or(sent_id.as_deref())
            .or_else(|| mailboxes.iter().find_map(|m| m.id()))
            .ok_or_else(|| JmapError::InvalidResponse("no mailbox to import into".into()))?
            .to_owned();

        let blob_id = self
            .client
            .upload(None, mime_body.as_bytes().to_vec(), Some("message/rfc822"))
            .await?
            .take_blob_id();

        let mut request = self.client.build();
        let import_create_id = {
            let import_req = request.import_email();
            let import = import_req.email(blob_id);
            import.mailbox_ids([import_mailbox.as_str()]);
            import.keywords(["$draft"]);
            import.create_id()
        };
        {
            let sub_set = request.set_email_submission();
            sub_set
                .create_with_id("sub")
                .email_id(format!("#{import_create_id}"))
                .identity_id(identity_id);
            fill_on_success_patch(
                sub_set.arguments().on_success_update_email("sub"),
                sent_id.as_deref(),
            );
        }
        let mut responses = request.send().await?.unwrap_method_responses();
        if responses.len() != 2 {
            return Err(JmapError::InvalidResponse(format!(
                "expected Email/import + EmailSubmission/set responses, got {}",
                responses.len()
            )));
        }
        // Unwrap Email/import FIRST: the submission's `#{create_id}` reference
        // fails with it and would otherwise mask the root error.
        let mut import_resp = responses.remove(0).unwrap_import_email()?;
        let mut sub_resp = responses.remove(0).unwrap_set_email_submission()?;
        import_resp.created(&import_create_id)?;
        let mut submission = sub_resp.created("sub")?;
        Ok(submission.take_id())
    }

    /// Upload each outbound attachment to the session `uploadUrl`
    /// (RFC 8620 §6.1), returning blob ids for the `Email/set` create.
    async fn upload_attachments(
        &self,
        outbound: &OutboundMessage,
    ) -> Result<Vec<UploadedAttachment>, JmapError> {
        let mut uploaded = Vec::new();
        for att in &outbound.attachments {
            let bytes = att.decode().map_err(|e| {
                JmapError::InvalidResponse(format!("attachment {}: {}", att.filename, e))
            })?;
            let blob_id = self
                .client
                .upload(None, bytes, Some(att.content_type.as_str()))
                .await
                // Name the file like the decode error above; the enum has no
                // transient string variant, and the legacy send path used
                // InvalidResponse for upload failures too.
                .map_err(|e| {
                    JmapError::InvalidResponse(format!("attachment {}: {e}", att.filename))
                })?
                .take_blob_id();
            uploaded.push(UploadedAttachment {
                blob_id,
                content_type: att.content_type.clone(),
                name: att.filename.clone(),
            });
        }
        Ok(uploaded)
    }

    /// True when `urn:ietf:params:jmap:submission` is advertised (RFC 8621).
    pub(crate) fn supports_submission(&self) -> bool {
        self.client
            .session()
            .has_capability(URI::Submission.as_ref())
    }

    /// Open the session EventSource (`types=*`, `closeafter=no`, `ping=30`)
    /// and wait for the first mail-relevant state change. The stream itself
    /// has no read timeout — the crate times only the connect, which fixes
    /// the old bug where the 30s request timeout killed the stream cyclically.
    pub(crate) async fn wait_for_state_change(&self) -> Result<EventSourceOutcome, JmapError> {
        if self.client.session().event_source_url().is_empty() {
            return Ok(EventSourceOutcome::Unsupported);
        }
        let mut stream = self
            .client
            .event_source(None::<Vec<DataType>>, false, Some(30), None)
            .await?;
        while let Some(item) = stream.next().await {
            match item {
                Ok(notification) => {
                    if push_implies_sync(&notification) {
                        return Ok(EventSourceOutcome::StateChanged);
                    }
                }
                Err(err) => return Err(JmapError::from(err)),
            }
        }
        Ok(EventSourceOutcome::StreamEnded)
    }
}

static CLIENT_CACHE: OnceLock<Mutex<HashMap<String, Arc<JmapSeam>>>> = OnceLock::new();

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
///
/// Returns `None` for id-less emails: collapsing them onto `external_id = ""`
/// would make distinct server rows upsert onto each other.
fn map_email(email: &Email<Get>) -> Option<JmapEmail> {
    let id = email.id()?;
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
    Some(JmapEmail {
        id: id.to_owned(),
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
        attachments: None,
        preview: email.preview().map(str::to_owned),
        attachments_meta: map_attachments(email),
    })
}

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
fn map_body_refs(parts: Option<&[EmailBodyPart]>) -> Option<Vec<serde_json::Value>> {
    parts.map(|ps| {
        ps.iter()
            .map(|p| serde_json::json!({ "partId": p.part_id(), "type": p.content_type() }))
            .collect()
    })
}

/// Collect downloadable attachment parts (blobId required) into typed meta.
fn map_attachments(email: &Email<Get>) -> Vec<JmapAttachmentMeta> {
    email
        .attachments()
        .map(|parts| {
            parts
                .iter()
                .filter_map(|p| {
                    let blob_id = p.blob_id()?.to_owned();
                    Some(JmapAttachmentMeta {
                        blob_id,
                        filename: p.name().unwrap_or("attachment").to_owned(),
                        content_type: p
                            .content_type()
                            .unwrap_or("application/octet-stream")
                            .to_owned(),
                        size: p.size() as u64,
                        content_id: p.content_id().map(str::to_owned),
                        is_inline: p.content_disposition() == Some("inline"),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// An attachment already uploaded to the server, referenced by blob id.
#[derive(Debug, Clone)]
struct UploadedAttachment {
    blob_id: String,
    content_type: String,
    name: String,
}

/// `EmailAddress` for a create (name+email tuple, or bare email).
fn crate_address(name: Option<&str>, email: &str) -> EmailAddress {
    match name {
        Some(n) if !n.is_empty() => EmailAddress::from((n.to_owned(), email.to_owned())),
        _ => EmailAddress::from(email.to_owned()),
    }
}

/// Fill the `Email/set` create object for a draft/submission (RFC 8621 §4.7).
/// Body parts: `bd1` text, `bd2` html when both exist; html-only uses `bd1`
/// as the html part.
fn fill_outbound_email(
    email: &mut Email<Set>,
    outbound: &OutboundMessage,
    mailbox_ids: &[String],
    uploaded: &[UploadedAttachment],
) {
    if !mailbox_ids.is_empty() {
        email.mailbox_ids(mailbox_ids.iter().map(String::as_str));
    }
    email.keywords(["$draft"]);
    email.from([crate_address(
        outbound.from_name.as_deref(),
        &outbound.from_email,
    )]);
    email.to(outbound
        .to
        .iter()
        .map(|(n, e)| crate_address(n.as_deref(), e))
        .collect::<Vec<_>>());
    email.cc(outbound
        .cc
        .iter()
        .map(|(n, e)| crate_address(n.as_deref(), e))
        .collect::<Vec<_>>());
    email.bcc(
        outbound
            .bcc
            .iter()
            .map(|(n, e)| crate_address(n.as_deref(), e))
            .collect::<Vec<_>>(),
    );
    email.subject(outbound.subject.as_str());

    let text = outbound
        .body_text
        .clone()
        .or_else(|| outbound.body_html.clone())
        .unwrap_or_default();
    email.body_value("bd1".to_owned(), text.as_str());
    match (&outbound.body_text, &outbound.body_html) {
        (Some(_), Some(html)) => {
            email.text_body(
                EmailBodyPart::new()
                    .part_id("bd1")
                    .content_type("text/plain"),
            );
            email.body_value("bd2".to_owned(), html.as_str());
            email.html_body(
                EmailBodyPart::new()
                    .part_id("bd2")
                    .content_type("text/html"),
            );
        }
        (Some(_) | None, None) => {
            email.text_body(
                EmailBodyPart::new()
                    .part_id("bd1")
                    .content_type("text/plain"),
            );
        }
        (None, Some(_)) => {
            email.html_body(
                EmailBodyPart::new()
                    .part_id("bd1")
                    .content_type("text/html"),
            );
        }
    }

    if let Some(irt) = &outbound.in_reply_to {
        email.in_reply_to([irt.clone()]);
    }
    if let Some(refs) = &outbound.references {
        email.references(
            refs.split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>(),
        );
    }

    for att in uploaded {
        email.attachment(
            EmailBodyPart::new()
                .blob_id(att.blob_id.clone())
                .name(att.name.clone())
                .content_type(att.content_type.clone()),
        );
    }
}

/// Post-submit patch as full-value replacement (RFC 8621 §7.5.1): the email
/// was created in this same request with exactly `$draft`, so replacing
/// `keywords` with the empty set is exactly "clear $draft", and replacing
/// `mailboxIds` is exactly the move to Sent (or mailbox-less when the account
/// has no Sent mailbox — the old client's behavior).
fn fill_on_success_patch(patch: &mut Email<Set>, sent_id: Option<&str>) {
    patch.keywords(Vec::<String>::new());
    patch.mailbox_ids(sent_id.into_iter().map(str::to_owned));
}

/// Pick the identity whose email matches `from` (case-insensitive), else the first.
fn pick_identity(identities: &[Identity], from: &str) -> Option<String> {
    identities
        .iter()
        .find(|i| {
            i.email
                .as_deref()
                .is_some_and(|e| e.eq_ignore_ascii_case(from))
        })
        .or_else(|| identities.first())
        .and_then(|i| i.id.clone())
}

/// First mailbox with `role` (server id).
fn mailbox_id_for_role(mailboxes: &[Mailbox<Get>], role: &Role) -> Option<String> {
    mailboxes
        .iter()
        .find(|m| m.role() == *role)
        .and_then(|m| m.id().map(str::to_owned))
}

/// Whether a push notification carries a mail-relevant state change
/// (ping frames are already filtered by the crate's SSE parser).
pub(crate) fn push_implies_sync(notification: &PushNotification) -> bool {
    match notification {
        PushNotification::StateChange(changes) => [
            DataType::Email,
            DataType::Mailbox,
            DataType::Thread,
            DataType::EmailSubmission,
        ]
        .iter()
        .any(|t| changes.has_type(t.clone())),
        PushNotification::CalendarAlert(_) => false,
    }
}

/// Map a crate `Mailbox<Get>`; `None` when the server row has no id.
/// `Role::Junk` normalizes to Lyra's `"spam"` vocabulary.
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

/// Properties fetched for every synced message (RFC 8621 §4.3 property list).
fn email_get_properties() -> Vec<Property> {
    use jmap_client::email::Property as P;
    vec![
        P::Id,
        P::BlobId,
        P::ThreadId,
        P::MailboxIds,
        P::Keywords,
        P::Size,
        P::ReceivedAt,
        P::MessageId,
        P::InReplyTo,
        P::References,
        P::Sender,
        P::From,
        P::To,
        P::Cc,
        P::Bcc,
        P::ReplyTo,
        P::Subject,
        P::BodyStructure,
        P::BodyValues,
        P::TextBody,
        P::HtmlBody,
        P::Attachments,
        P::HasAttachment,
        P::Preview,
    ]
}

/// `Email/query` args for one folder page (receivedAt desc, paged by position).
fn fill_email_query(
    q: &mut QueryRequest<Email<Set>>,
    mailbox_id: &str,
    position: i32,
    limit: usize,
) {
    q.filter(email::query::Filter::in_mailbox(mailbox_id));
    q.sort([email::query::Comparator::received_at().descending()]);
    q.position(position);
    q.limit(limit);
    q.calculate_total(true);
}

/// `Email/get` args for sync: full property set + text/HTML body values,
/// capped at `MAX_BODY_VALUE_BYTES` per value.
fn fill_email_get(get: &mut GetRequest<Email<Set>>) {
    get.properties(email_get_properties());
    get.arguments().fetch_text_body_values(true);
    get.arguments().fetch_html_body_values(true);
    get.arguments().max_body_value_bytes(MAX_BODY_VALUE_BYTES);
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

        let mapped = map_email(&crate_email).expect("email with id maps");
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
        let mapped = map_email(&crate_email).expect("email with id maps");
        assert!(!mapped.is_seen());
        assert!(mapped.keywords.is_none());
    }

    #[test]
    fn map_email_skips_idless_emails() {
        // An id-less row must not collapse onto external_id = "" and upsert
        // over unrelated messages.
        let idless: Email<Get> =
            serde_json::from_value(serde_json::json!({ "subject": "no id" })).unwrap();
        assert!(map_email(&idless).is_none());
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

    // ── request wire shapes (Task 2) ────────────────────────────────

    #[test]
    fn email_query_serializes_rfc_shape() {
        use jmap_client::Method;
        use jmap_client::core::RequestParams;

        let mut q =
            QueryRequest::<Email<Set>>::new(RequestParams::new("acc", Method::QueryEmail, 0));
        fill_email_query(&mut q, "mb1", 0, 100);
        let json = serde_json::to_value(&q).unwrap();
        assert_eq!(json["accountId"], "acc");
        assert_eq!(json["filter"]["inMailbox"], "mb1");
        assert_eq!(json["sort"][0]["property"], "receivedAt");
        assert_eq!(json["sort"][0]["isAscending"], false);
        assert_eq!(json["position"], 0);
        assert_eq!(json["limit"], 100);
        assert_eq!(json["calculateTotal"], true);
    }

    #[test]
    fn email_get_serializes_properties_and_body_flags() {
        use jmap_client::Method;
        use jmap_client::core::RequestParams;

        let mut g = GetRequest::<Email<Set>>::new(RequestParams::new("acc", Method::GetEmail, 0));
        g.ids(["em1"]);
        fill_email_get(&mut g);
        let json = serde_json::to_value(&g).unwrap();
        assert_eq!(json["fetchTextBodyValues"], true);
        assert_eq!(json["fetchHTMLBodyValues"], true);
        let props = json["properties"].as_array().unwrap();
        assert!(props.contains(&serde_json::json!("threadId")));
        assert!(props.contains(&serde_json::json!("mailboxIds")));
        assert!(props.contains(&serde_json::json!("keywords")));
        assert!(props.contains(&serde_json::json!("attachments")));
    }

    // ── attachment meta mapping (Task 3) ────────────────────────────

    #[test]
    fn map_attachments_collects_downloadable_parts() {
        let email: Email<Get> = serde_json::from_value(serde_json::json!({
            "id": "em1",
            "attachments": [
                { "blobId": "b1", "name": "invoice.pdf", "type": "application/pdf", "size": 1234, "disposition": "attachment" },
                { "name": "no-blob.txt", "type": "text/plain" },
                { "blobId": "b2", "type": "image/png", "size": 10, "cid": "cid1", "disposition": "inline" }
            ]
        }))
        .unwrap();
        let meta = map_attachments(&email);
        assert_eq!(meta.len(), 2, "parts without blobId are skipped");
        assert_eq!(meta[0].blob_id, "b1");
        assert_eq!(meta[0].filename, "invoice.pdf");
        assert_eq!(meta[0].content_type, "application/pdf");
        assert_eq!(meta[0].size, 1234);
        assert!(!meta[0].is_inline);
        assert_eq!(meta[1].blob_id, "b2");
        assert!(meta[1].is_inline);
        assert_eq!(meta[1].content_id.as_deref(), Some("cid1"));
        assert_eq!(meta[1].filename, "attachment", "fallback filename");
    }

    #[test]
    fn map_attachments_empty_without_attachments() {
        let email: Email<Get> = serde_json::from_value(serde_json::json!({ "id": "em2" })).unwrap();
        assert!(map_attachments(&email).is_empty());
    }

    // ── send path wire shapes (Task 4) ──────────────────────────────

    use jmap_client::Method;
    use jmap_client::core::RequestParams;
    use jmap_client::core::set::SetRequest;
    use jmap_client::email::import::EmailImportRequest;
    use jmap_client::email_submission::EmailSubmission;
    use jmap_client::identity::Identity;

    fn sample_outbound() -> crate::smtp::OutboundMessage {
        crate::smtp::OutboundMessage {
            from_email: "me@example.com".into(),
            from_name: Some("Me".into()),
            to: vec![(Some("You".into()), "you@example.com".into())],
            cc: vec![],
            bcc: vec![],
            subject: "Hi".into(),
            body_text: Some("Hello".into()),
            body_html: None,
            in_reply_to: None,
            references: None,
            mime_content_type: None,
            mime_body: None,
            attachments: Vec::new(),
            message_id: None,
        }
    }

    #[test]
    fn draft_email_serializes_jmap_create_shape() {
        let mut req = SetRequest::<Email<Set>>::new(RequestParams::new("acc", Method::SetEmail, 0));
        fill_outbound_email(
            req.create_with_id("draft"),
            &sample_outbound(),
            &["mb-drafts".to_owned()],
            &[],
        );
        let json = serde_json::to_value(&req).unwrap();
        let draft = &json["create"]["draft"];
        assert_eq!(draft["subject"], "Hi");
        assert_eq!(draft["keywords"]["$draft"], true);
        assert_eq!(draft["mailboxIds"]["mb-drafts"], true);
        assert_eq!(draft["from"][0]["name"], "Me");
        assert_eq!(draft["from"][0]["email"], "me@example.com");
        assert_eq!(draft["to"][0]["email"], "you@example.com");
        assert_eq!(draft["bodyValues"]["bd1"]["value"], "Hello");
        assert_eq!(draft["textBody"][0]["partId"], "bd1");
        assert_eq!(draft["textBody"][0]["type"], "text/plain");
    }

    #[test]
    fn draft_email_dual_body_and_threading_headers() {
        let mut outbound = sample_outbound();
        outbound.body_html = Some("<p>Hello</p>".into());
        outbound.in_reply_to = Some("<parent@example.com>".into());
        outbound.references = Some("<a@example.com> <b@example.com>".into());
        let mut req = SetRequest::<Email<Set>>::new(RequestParams::new("acc", Method::SetEmail, 0));
        fill_outbound_email(req.create_with_id("draft"), &outbound, &[], &[]);
        let json = serde_json::to_value(&req).unwrap();
        let draft = &json["create"]["draft"];
        assert_eq!(draft["bodyValues"]["bd2"]["value"], "<p>Hello</p>");
        assert_eq!(draft["htmlBody"][0]["partId"], "bd2");
        assert_eq!(draft["inReplyTo"][0], "<parent@example.com>");
        assert_eq!(draft["references"][1], "<b@example.com>");
    }

    #[test]
    fn draft_email_references_uploaded_attachment_blobs() {
        let uploaded = vec![UploadedAttachment {
            blob_id: "blob-1".into(),
            content_type: "application/pdf".into(),
            name: "invoice.pdf".into(),
        }];
        let mut req = SetRequest::<Email<Set>>::new(RequestParams::new("acc", Method::SetEmail, 0));
        fill_outbound_email(
            req.create_with_id("draft"),
            &sample_outbound(),
            &[],
            &uploaded,
        );
        let json = serde_json::to_value(&req).unwrap();
        let draft = &json["create"]["draft"];
        assert_eq!(draft["attachments"][0]["blobId"], "blob-1");
        assert_eq!(draft["attachments"][0]["name"], "invoice.pdf");
        assert_eq!(draft["attachments"][0]["type"], "application/pdf");
    }

    #[test]
    fn on_success_patch_uses_full_value_replacement() {
        let mut req = SetRequest::<EmailSubmission<Set>>::new(RequestParams::new(
            "acc",
            Method::SetEmailSubmission,
            0,
        ));
        req.create_with_id("sub")
            .email_id("#draft")
            .identity_id("i1");
        fill_on_success_patch(
            req.arguments().on_success_update_email("sub"),
            Some("mb-sent"),
        );
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["create"]["sub"]["emailId"], "#draft");
        assert_eq!(json["create"]["sub"]["identityId"], "i1");
        // Full-value replacement (RFC 8621 §7.5.1 semantics without patch-null):
        // we created the email in this same request with exactly `$draft`.
        assert_eq!(
            json["onSuccessUpdateEmail"]["#sub"]["keywords"],
            serde_json::json!({})
        );
        assert_eq!(
            json["onSuccessUpdateEmail"]["#sub"]["mailboxIds"]["mb-sent"],
            true
        );
    }

    #[test]
    fn import_request_serializes_rfc_shape() {
        let mut req = EmailImportRequest::new(RequestParams::new("acc", Method::ImportEmail, 0));
        let import = req.email("blob-mime");
        import.mailbox_ids(["mb-drafts"]);
        import.keywords(["$draft"]);
        let create_id = import.create_id();
        assert_eq!(create_id, "i0");
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["emails"]["i0"]["blobId"], "blob-mime");
        assert_eq!(json["emails"]["i0"]["mailboxIds"]["mb-drafts"], true);
        assert_eq!(json["emails"]["i0"]["keywords"]["$draft"], true);
    }

    #[test]
    fn pick_identity_prefers_matching_email() {
        let identities: Vec<Identity> = serde_json::from_value(serde_json::json!([
            { "id": "i1", "name": "Other", "email": "other@example.com" },
            { "id": "i2", "name": "Me", "email": "me@example.com" }
        ]))
        .unwrap();
        assert_eq!(
            pick_identity(&identities, "ME@example.com").as_deref(),
            Some("i2")
        );
        // No match → first identity.
        assert_eq!(
            pick_identity(&identities, "nobody@example.com").as_deref(),
            Some("i1")
        );
        assert!(pick_identity(&[], "me@example.com").is_none());
    }

    // ── push classification (Task 5) ────────────────────────────────

    #[test]
    fn push_state_change_implies_sync() {
        use jmap_client::event_source::Changes;

        let changes: Changes = serde_json::from_value(serde_json::json!({
            "id": null,
            "changes": { "a1": { "Email": "s1", "Mailbox": "m2" } }
        }))
        .unwrap();
        assert!(push_implies_sync(&PushNotification::StateChange(changes)));

        let empty: Changes = serde_json::from_value(serde_json::json!({
            "id": null,
            "changes": { "a1": {} }
        }))
        .unwrap();
        assert!(!push_implies_sync(&PushNotification::StateChange(empty)));

        let unrelated: Changes = serde_json::from_value(serde_json::json!({
            "id": null,
            "changes": { "a1": { "Quota": "q1" } }
        }))
        .unwrap();
        assert!(!push_implies_sync(&PushNotification::StateChange(
            unrelated
        )));
    }
}
