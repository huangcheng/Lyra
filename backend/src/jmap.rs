//! JMAP protocol adapter.
//!
//! Implements a JMAP HTTP client for session discovery, mailbox sync,
//! Email/query + Email/get, and EmailSubmission send when the session
//! advertises `urn:ietf:params:jmap:submission`.
//!
//! See `docs/specs/2026-08-20-lyra-sync-and-protocols-spec.md` §6.

#![allow(clippy::doc_markdown)]

use reqwest::Client;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::crypto::{self, EncryptedCredential};
use crate::smtp::OutboundMessage;

/// Errors specific to the JMAP adapter.
#[derive(Debug, Error)]
pub enum JmapError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("session discovery failed: {0}")]
    SessionDiscovery(String),
    #[error("authentication failed: {0}")]
    #[allow(dead_code)]
    Authentication(String),
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
}

impl JmapError {
    /// `Email/queryChanges` cannot resume from the stored token; caller should full-query.
    #[must_use]
    pub fn is_stale_query_state(&self) -> bool {
        match self {
            Self::Method { code, .. } => {
                code.eq_ignore_ascii_case("cannotCalculateChanges")
                    || code.eq_ignore_ascii_case("cannotCalculateChangesFrom")
            }
            _ => false,
        }
    }
}

// ── JMAP Session Resource ───────────────────────────────────────────

/// JMAP session resource returned by `/.well-known/jmap`.
///
/// Only the fields we need are deserialized; the full spec has more.
#[derive(Debug, Clone, Deserialize)]
pub struct JmapSession {
    /// Primary account ID for the authenticated user.
    #[serde(rename = "primaryAccounts")]
    pub primary_accounts: PrimaryAccounts,
    /// Map of capability URIs to their objects.
    pub capabilities: serde_json::Value,
    /// Map of account ID → account object.
    #[allow(dead_code)]
    pub accounts: serde_json::Value,
    /// URL to use as the API endpoint.
    #[serde(rename = "apiUrl")]
    pub api_url: String,
    /// URL for event source (push).
    #[serde(rename = "eventSourceUrl")]
    pub event_source_url: Option<String>,
    /// URL for uploading blobs.
    #[serde(rename = "uploadUrl")]
    #[allow(dead_code)]
    pub upload_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PrimaryAccounts {
    /// The account ID for `urn:ietf:params:jmap:mail`.
    #[serde(rename = "urn:ietf:params:jmap:mail")]
    pub mail: Option<String>,
}

// ── JMAP Request / Response ─────────────────────────────────────────

/// A JMAP API request body.
#[derive(Debug, Serialize)]
pub struct JmapRequest {
    /// Client-used request identifier for correlating responses.
    #[serde(rename = "using")]
    pub using: Vec<String>,
    /// Method calls to invoke.
    #[serde(rename = "methodCalls")]
    pub method_calls: Vec<MethodCall>,
}

/// A single JMAP method call.
pub type MethodCall = (String, serde_json::Value, String);

/// Top-level JMAP response body.
#[derive(Debug, Deserialize)]
pub struct JmapResponse {
    /// Responses to the method calls, in order.
    #[serde(rename = "methodResponses")]
    pub method_responses: Vec<MethodResponse>,
    /// Session state for the response.
    #[serde(rename = "sessionState")]
    #[allow(dead_code)]
    pub session_state: Option<String>,
}

/// A single method response: `(name, arguments, call_id)`.
pub type MethodResponse = (String, serde_json::Value, String);

// ── Sync cursor ─────────────────────────────────────────────────────

/// Stored JMAP sync state for a mailbox.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct JmapSyncState {
    /// JMAP `queryState` from `Email/query`.
    pub query_state: Option<String>,
    /// Highest `receivedAt` seen (for ordering).
    pub highest_received_at: Option<String>,
}

// ── JMAP client ─────────────────────────────────────────────────────

/// Authenticated JMAP HTTP session.
///
/// Wraps the `reqwest` HTTP client and provides high-level operations
/// for the sync engine: list mailboxes, query/fetch emails.
///
/// Credentials are pinned to the origin of the configured base URL: the
/// session's `apiUrl`/`uploadUrl` must share that origin or discovery
/// fails, so a malicious JMAP server cannot exfiltrate the account
/// password by pointing those URLs at another host.
pub struct JmapClient {
    client: Client,
    session: JmapSession,
    account_id: String,
    auth_token: String,
    origin: String,
}

impl JmapClient {
    /// Discover and authenticate a JMAP session.
    ///
    /// Steps:
    /// 1. `GET https://<host>/.well-known/jmap` (or use `base_url` if provided)
    /// 2. Authenticate with Basic auth (email:password)
    /// 3. Parse the session resource
    /// 4. Extract the primary mail account ID
    pub async fn discover(base_url: &str, email: &str, password: &str) -> Result<Self, JmapError> {
        crate::netsec::validate_server_url(base_url).map_err(JmapError::InvalidServerUrl)?;

        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            // Never follow redirects: a redirect could carry the request to
            // a different host, and we must never replay credentials
            // cross-origin. Redirect responses surface as errors instead.
            .redirect(reqwest::redirect::Policy::none())
            .build()?;

        // Step 1: Discover session URL
        let well_known = if base_url.ends_with("/.well-known/jmap") {
            base_url.to_string()
        } else {
            let trimmed = base_url.trim_end_matches('/');
            format!("{trimmed}/.well-known/jmap")
        };

        let origin = crate::netsec::origin_of(&well_known).map_err(JmapError::InvalidServerUrl)?;

        let resp = client
            .get(&well_known)
            .basic_auth(email, Some(password))
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(JmapError::SessionDiscovery(format!(
                "HTTP {} from {well_known}",
                resp.status()
            )));
        }

        let session: JmapSession = resp.json().await?;

        // The session document is server-controlled: pin every URL we will
        // send credentials to against the configured origin.
        check_session_urls(&origin, &session)?;

        // Step 2: Extract primary mail account
        let account_id = session
            .primary_accounts
            .mail
            .clone()
            .or_else(|| {
                // Fallback: try first key in accounts map
                session
                    .accounts
                    .as_object()
                    .and_then(|m| m.keys().next().cloned())
            })
            .ok_or_else(|| JmapError::SessionDiscovery("no mail account in JMAP session".into()))?;

        // Encode credentials for later use
        let credentials = format!("{email}:{password}");
        let auth_token = format!(
            "Basic {}",
            base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                credentials.as_bytes()
            )
        );

        Ok(Self {
            client,
            session,
            account_id,
            auth_token,
            origin,
        })
    }

    /// Create a `JmapClient` directly from an existing session resource
    /// (avoids re-discovery).
    ///
    /// Pins credentials to the origin of the session's `apiUrl`; prefer
    /// [`JmapClient::discover`], which pins to the configured base URL.
    #[allow(dead_code)]
    pub fn from_session(
        session: JmapSession,
        account_id: String,
        email: &str,
        password: &str,
    ) -> Self {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("reqwest client");

        let credentials = format!("{email}:{password}");
        let auth_token = format!(
            "Basic {}",
            base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                credentials.as_bytes()
            )
        );

        let origin = crate::netsec::origin_of(&session.api_url).unwrap_or_default();

        Self {
            client,
            session,
            account_id,
            auth_token,
            origin,
        }
    }

    /// The primary mail account ID.
    #[allow(dead_code)]
    pub fn account_id(&self) -> &str {
        &self.account_id
    }

    /// The JMAP API URL.
    #[allow(dead_code)]
    pub fn api_url(&self) -> &str {
        &self.session.api_url
    }

    /// Expanded EventSource URL when the session provides a template, else `None`.
    ///
    /// Substitutes RFC 8620 placeholders: `types=*`, `closeafter=no`, `ping=30`.
    #[must_use]
    pub fn event_source_url_expanded(&self) -> Option<String> {
        self.session.event_source_url.as_ref().map(|t| expand_event_source_url(t))
    }

    /// Open the session EventSource and wait until a `state` (or similar) push event.
    ///
    /// Returns [`EventSourceOutcome::Unsupported`] when `eventSourceUrl` is absent.
    pub async fn wait_event_source_state(&self) -> Result<EventSourceOutcome, JmapError> {
        let Some(url) = self.event_source_url_expanded() else {
            return Ok(EventSourceOutcome::Unsupported);
        };

        let target = crate::netsec::origin_of(&url).map_err(JmapError::InvalidServerUrl)?;
        if target != self.origin {
            return Err(JmapError::CrossOrigin(url));
        }

        let mut resp = self
            .client
            .get(&url)
            .header("Authorization", &self.auth_token)
            .header("Accept", "text/event-stream")
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(JmapError::Method {
                code: resp.status().to_string(),
                description: format!("EventSource HTTP {}", resp.status()),
            });
        }

        let mut buffer = String::new();
        loop {
            let Some(chunk) = resp.chunk().await.map_err(JmapError::Http)? else {
                break;
            };
            buffer.push_str(&String::from_utf8_lossy(&chunk).replace("\r\n", "\n"));
            while let Some(idx) = buffer.find("\n\n") {
                let frame = buffer[..idx].to_string();
                buffer = buffer[idx + 2..].to_string();
                if sse_frame_is_state_push(&frame) {
                    return Ok(EventSourceOutcome::StateChanged);
                }
            }
        }

        Ok(EventSourceOutcome::StreamEnded)
    }

    /// True when the session resource advertises `capability` (top-level key).
    #[must_use]
    pub fn has_capability(&self, capability: &str) -> bool {
        self.session
            .capabilities
            .as_object()
            .is_some_and(|m| m.contains_key(capability))
    }

    /// True when `urn:ietf:params:jmap:submission` is advertised (RFC 8621).
    #[must_use]
    pub fn supports_submission(&self) -> bool {
        self.has_capability("urn:ietf:params:jmap:submission")
    }

    // ── Mailbox operations ──────────────────────────────────────

    /// List all mailboxes (folders) for this account.
    pub async fn list_mailboxes(&self) -> Result<Vec<JmapMailbox>, JmapError> {
        let req = JmapRequest {
            using: vec!["urn:ietf:params:jmap:mail".into()],
            method_calls: vec![(
                "Mailbox/get".into(),
                serde_json::json!({
                    "accountId": self.account_id,
                    "ids": null
                }),
                "mb0".into(),
            )],
        };

        let resp = self.send_request(&req).await?;
        let args = take_ok_args(&resp, "Mailbox/get")?;

        let list = args
            .get("list")
            .and_then(|v| v.as_array())
            .ok_or_else(|| JmapError::InvalidResponse("missing list in Mailbox/get".into()))?;

        let mut mailboxes = Vec::new();
        for item in list {
            let mb: JmapMailbox = serde_json::from_value(item.clone())
                .map_err(|e| JmapError::InvalidResponse(format!("Mailbox parse: {e}")))?;
            mailboxes.push(mb);
        }

        Ok(mailboxes)
    }

    // ── Email operations ────────────────────────────────────────

    /// Query emails in a mailbox (full query, not incremental).
    ///
    /// Returns the list of email IDs and a new query state. Incremental
    /// updates use [`Self::query_email_changes`].
    pub async fn query_emails(
        &self,
        mailbox_id: &str,
        limit: Option<u32>,
    ) -> Result<EmailQueryResult, JmapError> {
        let filter = serde_json::json!({
            "inMailbox": mailbox_id
        });

        let args = serde_json::json!({
            "accountId": self.account_id,
            "filter": filter,
            "sort": [{ "property": "receivedAt", "isAscending": false }],
            "limit": limit.unwrap_or(100),
            "calculateTotal": true
        });

        let req = JmapRequest {
            using: vec!["urn:ietf:params:jmap:mail".into()],
            method_calls: vec![("Email/query".into(), args, "eq0".into())],
        };

        let resp = self.send_request(&req).await?;
        let args = take_ok_args(&resp, "Email/query")?;

        let ids: Vec<String> = args
            .get("ids")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let query_state = args
            .get("queryState")
            .and_then(|v| v.as_str())
            .map(String::from);

        let total = args.get("total").and_then(serde_json::Value::as_u64);

        Ok(EmailQueryResult {
            ids,
            query_state,
            total,
        })
    }

    /// Incremental mailbox changes since a stored `queryState` (RFC 8621 `Email/queryChanges`).
    pub async fn query_email_changes(
        &self,
        mailbox_id: &str,
        since_query_state: &str,
    ) -> Result<EmailQueryChanges, JmapError> {
        let args = serde_json::json!({
            "accountId": self.account_id,
            "filter": { "inMailbox": mailbox_id },
            "sort": [{ "property": "receivedAt", "isAscending": false }],
            "sinceQueryState": since_query_state,
            "maxChanges": 100
        });

        let req = JmapRequest {
            using: vec!["urn:ietf:params:jmap:mail".into()],
            method_calls: vec![("Email/queryChanges".into(), args, "eqc0".into())],
        };

        let resp = self.send_request(&req).await?;
        let args = take_ok_args(&resp, "Email/queryChanges")?;

        let added_ids: Vec<String> = args
            .get("added")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| item.get("id").and_then(|v| v.as_str()).map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();

        let removed_ids: Vec<String> = args
            .get("removed")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();

        let new_query_state = args
            .get("newQueryState")
            .and_then(|v| v.as_str())
            .map(str::to_owned);

        Ok(EmailQueryChanges {
            added_ids,
            removed_ids,
            new_query_state,
        })
    }

    /// Fetch email objects by ID with the properties we need.
    pub async fn get_emails(&self, ids: &[String]) -> Result<Vec<JmapEmail>, JmapError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }

        let properties: Vec<String> = vec![
            "id".into(),
            "blobId".into(),
            "threadId".into(),
            "mailboxIds".into(),
            "keywords".into(),
            "size".into(),
            "receivedAt".into(),
            "messageId".into(),
            "inReplyTo".into(),
            "references".into(),
            "sender".into(),
            "from".into(),
            "to".into(),
            "cc".into(),
            "bcc".into(),
            "replyTo".into(),
            "subject".into(),
            "bodyStructure".into(),
            "bodyValues".into(),
            "textBody".into(),
            "htmlBody".into(),
            "hasAttachment".into(),
            "attachments".into(),
            "preview".into(),
        ];

        let req = JmapRequest {
            using: vec!["urn:ietf:params:jmap:mail".into()],
            method_calls: vec![(
                "Email/get".into(),
                serde_json::json!({
                    "accountId": self.account_id,
                    "ids": ids,
                    "properties": properties,
                    "fetchTextBodyValues": true,
                    "fetchHTMLBodyValues": true,
                }),
                "eg0".into(),
            )],
        };

        let resp = self.send_request(&req).await?;
        let args = take_ok_args(&resp, "Email/get")?;

        let list = args
            .get("list")
            .and_then(|v| v.as_array())
            .ok_or_else(|| JmapError::InvalidResponse("missing list in Email/get".into()))?;

        let mut emails = Vec::new();
        for item in list {
            let email: JmapEmail = serde_json::from_value(item.clone())
                .map_err(|e| JmapError::InvalidResponse(format!("Email parse: {e}")))?;
            emails.push(email);
        }

        Ok(emails)
    }

    // ── Submission (RFC 8621 EmailSubmission) ─────────────────────

    /// List identities for the mail account (`Identity/get`).
    pub async fn list_identities(&self) -> Result<Vec<JmapIdentity>, JmapError> {
        let req = JmapRequest {
            using: vec![
                "urn:ietf:params:jmap:core".into(),
                "urn:ietf:params:jmap:submission".into(),
            ],
            method_calls: vec![(
                "Identity/get".into(),
                serde_json::json!({
                    "accountId": self.account_id,
                    "ids": null
                }),
                "id0".into(),
            )],
        };

        let resp = self.send_request(&req).await?;
        let args = take_ok_args(&resp, "Identity/get")?;
        let list = args
            .get("list")
            .and_then(|v| v.as_array())
            .ok_or_else(|| JmapError::InvalidResponse("missing list in Identity/get".into()))?;

        let mut identities = Vec::new();
        for item in list {
            let identity: JmapIdentity = serde_json::from_value(item.clone())
                .map_err(|e| JmapError::InvalidResponse(format!("Identity parse: {e}")))?;
            identities.push(identity);
        }
        Ok(identities)
    }

    /// Create a draft via `Email/set` and submit it with `EmailSubmission/set`.
    ///
    /// Requires [`Self::supports_submission`]. Moves the message to Sent on
    /// success when a `sent` mailbox exists.
    pub async fn submit_email(&self, outbound: &OutboundMessage) -> Result<String, JmapError> {
        if !self.supports_submission() {
            return Err(JmapError::SessionDiscovery(
                "JMAP session does not advertise urn:ietf:params:jmap:submission".into(),
            ));
        }

        let identities = self.list_identities().await?;
        let identity = pick_identity(&identities, &outbound.from_email).ok_or_else(|| {
            JmapError::InvalidResponse(format!(
                "no JMAP identity for {}",
                outbound.from_email
            ))
        })?;

        let mailboxes = self.list_mailboxes().await?;
        let drafts_id = mailbox_id_for_role(&mailboxes, "drafts");
        let sent_id = mailbox_id_for_role(&mailboxes, "sent");

        let email_create = build_email_create(outbound, drafts_id.as_deref())?;

        let mut on_success_update = serde_json::Map::new();
        if let Some(ref drafts) = drafts_id {
            on_success_update.insert(format!("mailboxIds/{drafts}"), serde_json::Value::Null);
        }
        on_success_update.insert("keywords/$draft".into(), serde_json::Value::Null);
        if let Some(ref sent) = sent_id {
            on_success_update.insert(format!("mailboxIds/{sent}"), serde_json::json!(true));
        }

        let req = JmapRequest {
            using: vec![
                "urn:ietf:params:jmap:core".into(),
                "urn:ietf:params:jmap:mail".into(),
                "urn:ietf:params:jmap:submission".into(),
            ],
            method_calls: vec![
                (
                    "Email/set".into(),
                    serde_json::json!({
                        "accountId": self.account_id,
                        "create": {
                            "draft": email_create
                        }
                    }),
                    "es0".into(),
                ),
                (
                    "EmailSubmission/set".into(),
                    serde_json::json!({
                        "accountId": self.account_id,
                        "create": {
                            "sub": {
                                "emailId": "#draft",
                                "identityId": identity.id
                            }
                        },
                        "onSuccessUpdateEmail": {
                            "#sub": on_success_update
                        }
                    }),
                    "es1".into(),
                ),
            ],
        };

        let resp = self.send_request(&req).await?;
        let email_args = take_ok_args_ref(&resp, "Email/set")?;
        if let Some(not_created) = email_args.get("notCreated").and_then(|v| v.as_object())
            && let Some(err) = not_created.get("draft")
        {
            return Err(jmap_set_error("Email/set", err));
        }

        let sub_args = take_ok_args(&resp, "EmailSubmission/set")?;
        if let Some(not_created) = sub_args.get("notCreated").and_then(|v| v.as_object())
            && let Some(err) = not_created.get("sub")
        {
            return Err(jmap_set_error("EmailSubmission/set", err));
        }

        let submission_id = sub_args
            .pointer("/created/sub/id")
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .ok_or_else(|| {
                JmapError::InvalidResponse("EmailSubmission/set missing created.sub.id".into())
            })?;

        Ok(submission_id)
    }

    // ── Internal helpers ────────────────────────────────────────

    /// Send a JMAP request to the API endpoint.
    async fn send_request(&self, request: &JmapRequest) -> Result<JmapResponse, JmapError> {
        // Defensive re-check: discovery already pinned session URLs, but
        // never attach Authorization without verifying the target origin.
        let target =
            crate::netsec::origin_of(&self.session.api_url).map_err(JmapError::InvalidServerUrl)?;
        if target != self.origin {
            tracing::warn!(
                target_origin = %target,
                expected_origin = %self.origin,
                "JMAP: refusing cross-origin request (credentials stay pinned)"
            );
            return Err(JmapError::CrossOrigin(self.session.api_url.clone()));
        }

        let resp = self
            .client
            .post(&self.session.api_url)
            .header("Authorization", &self.auth_token)
            .header("Content-Type", "application/json")
            .json(request)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(JmapError::Method {
                code: status.to_string(),
                description: body,
            });
        }

        let jmap_resp: JmapResponse = resp.json().await?;
        Ok(jmap_resp)
    }
}

/// Unwrap a method response matching `expected`, mapping JMAP `"error"` methods
/// to [`JmapError::Method`]. Prefers an exact name match when several responses
/// are present (e.g. Email/set + EmailSubmission/set).
fn take_ok_args(resp: &JmapResponse, expected: &str) -> Result<serde_json::Value, JmapError> {
    take_ok_args_ref(resp, expected).cloned()
}

fn take_ok_args_ref<'a>(
    resp: &'a JmapResponse,
    expected: &str,
) -> Result<&'a serde_json::Value, JmapError> {
    let mut first_error: Option<JmapError> = None;
    for (name, args, _cid) in &resp.method_responses {
        if name == "error" {
            if first_error.is_none() {
                let code = args
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_owned();
                let description = args
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_owned();
                first_error = Some(JmapError::Method { code, description });
            }
            continue;
        }
        if name == expected {
            return Ok(args);
        }
    }
    if let Some(err) = first_error {
        return Err(err);
    }
    Err(JmapError::InvalidResponse(format!(
        "expected {expected}, got {:?}",
        resp.method_responses
            .iter()
            .map(|(n, _, _)| n.as_str())
            .collect::<Vec<_>>()
    )))
}

fn jmap_set_error(method: &str, err: &serde_json::Value) -> JmapError {
    let code = err
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_owned();
    let description = err
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();
    JmapError::Method {
        code: format!("{method}: {code}"),
        description,
    }
}

/// Pick the identity whose email matches `from`, else the first identity.
fn pick_identity<'a>(identities: &'a [JmapIdentity], from: &str) -> Option<&'a JmapIdentity> {
    let from_lower = from.to_ascii_lowercase();
    identities
        .iter()
        .find(|i| i.email.eq_ignore_ascii_case(&from_lower))
        .or_else(|| identities.first())
}

fn mailbox_id_for_role(mailboxes: &[JmapMailbox], role: &str) -> Option<String> {
    mailboxes
        .iter()
        .find(|m| m.role.as_deref() == Some(role))
        .map(|m| m.id.clone())
}

/// Build the Email object for `Email/set` create (RFC 8621 §4.7).
fn build_email_create(
    outbound: &OutboundMessage,
    drafts_id: Option<&str>,
) -> Result<serde_json::Value, JmapError> {
    let mut mailbox_ids = serde_json::Map::new();
    if let Some(id) = drafts_id {
        mailbox_ids.insert(id.to_owned(), serde_json::json!(true));
    }

    let text = outbound
        .body_text
        .clone()
        .or_else(|| outbound.body_html.clone())
        .unwrap_or_default();

    let mut email = serde_json::json!({
        "from": [jmap_address(outbound.from_name.as_deref(), &outbound.from_email)],
        "to": outbound.to.iter().map(|(n, e)| jmap_address(n.as_deref(), e)).collect::<Vec<_>>(),
        "cc": outbound.cc.iter().map(|(n, e)| jmap_address(n.as_deref(), e)).collect::<Vec<_>>(),
        "bcc": outbound.bcc.iter().map(|(n, e)| jmap_address(n.as_deref(), e)).collect::<Vec<_>>(),
        "subject": outbound.subject,
        "keywords": { "$draft": true },
        "mailboxIds": mailbox_ids,
        "bodyValues": {
            "bd1": {
                "value": text,
                "charset": "utf-8"
            }
        },
        "textBody": [{ "partId": "bd1", "type": "text/plain" }]
    });

    if let Some(html) = &outbound.body_html
        && outbound.body_text.is_some()
    {
        email["bodyValues"]["bd2"] = serde_json::json!({
            "value": html,
            "charset": "utf-8"
        });
        email["htmlBody"] = serde_json::json!([{ "partId": "bd2", "type": "text/html" }]);
    } else if outbound.body_html.is_some() && outbound.body_text.is_none() {
        email["textBody"] = serde_json::Value::Array(vec![]);
        email["htmlBody"] = serde_json::json!([{ "partId": "bd1", "type": "text/html" }]);
    }

    if let Some(ref irt) = outbound.in_reply_to {
        email["inReplyTo"] = serde_json::json!([irt]);
    }
    if let Some(ref refs) = outbound.references {
        let list: Vec<&str> = refs.split_whitespace().collect();
        email["references"] = serde_json::json!(list);
    }

    Ok(email)
}

fn jmap_address(name: Option<&str>, email: &str) -> serde_json::Value {
    match name {
        Some(n) if !n.is_empty() => serde_json::json!({ "name": n, "email": email }),
        _ => serde_json::json!({ "email": email }),
    }
}

fn expand_event_source_url(template: &str) -> String {
    template
        .replace("{types}", "*")
        .replace("{closeafter}", "no")
        .replace("{ping}", "30")
}

/// True when an SSE frame is a JMAP push that should trigger sync.
fn sse_frame_is_state_push(frame: &str) -> bool {
    let mut event_name = "message";
    let mut data = String::new();
    for line in frame.lines() {
        if let Some(rest) = line.strip_prefix("event:") {
            event_name = rest.trim();
        } else if let Some(rest) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(rest.trim_start());
        }
    }
    // RFC 8620 §7.3: `state` events carry changed account states.
    // Some servers also emit `ping`; ignore those.
    if event_name.eq_ignore_ascii_case("ping") {
        return false;
    }
    if event_name.eq_ignore_ascii_case("state") {
        return true;
    }
    // Default event type with JSON body containing "changed" (Fastmail-style).
    data.contains("\"changed\"") || data.contains("\"State\"")
}

/// Outcome of waiting on a JMAP EventSource stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventSourceOutcome {
    StateChanged,
    Unsupported,
    StreamEnded,
}

// ── JMAP data types ─────────────────────────────────────────────────

/// A JMAP Identity object (submission capability).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JmapIdentity {
    pub id: String,
    pub name: String,
    pub email: String,
}

/// Verify every credential-bearing URL in the session document shares the
/// configured origin (`apiUrl`, and `uploadUrl` when present).
///
/// Add new session URLs here when they start being consumed
/// (e.g. `eventSourceUrl`, `downloadUrl`).
fn check_session_urls(origin: &str, session: &JmapSession) -> Result<(), JmapError> {
    let mut owned: Vec<String> = Vec::new();
    let mut urls: Vec<&str> = vec![session.api_url.as_str()];
    if let Some(upload) = &session.upload_url {
        urls.push(upload.as_str());
    }
    if let Some(es) = &session.event_source_url {
        // Template may contain `{types}` etc.; pin the expanded URL origin.
        owned.push(
            es.replace("{types}", "*")
                .replace("{closeafter}", "no")
                .replace("{ping}", "30"),
        );
        urls.push(owned.last().expect("just pushed").as_str());
    }
    for url in urls {
        let target = crate::netsec::origin_of(url).map_err(JmapError::InvalidServerUrl)?;
        if target != origin {
            tracing::warn!(
                target_origin = %target,
                expected_origin = %origin,
                "JMAP: session URL points at a different origin; refusing to send credentials"
            );
            return Err(JmapError::CrossOrigin(url.to_string()));
        }
    }
    Ok(())
}

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

/// Result of an `Email/query` call.
#[derive(Debug)]
pub struct EmailQueryResult {
    pub ids: Vec<String>,
    pub query_state: Option<String>,
    #[allow(dead_code)]
    pub total: Option<u64>,
}

/// Incremental result from `Email/queryChanges`.
#[derive(Debug, Clone)]
pub struct EmailQueryChanges {
    pub added_ids: Vec<String>,
    #[allow(dead_code)]
    pub removed_ids: Vec<String>,
    pub new_query_state: Option<String>,
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

/// Decrypt the stored credential for a JMAP account.
pub fn decrypt_account_password(credential_json: &str, dek: &[u8]) -> Result<String, JmapError> {
    let encrypted: EncryptedCredential = serde_json::from_str(credential_json)
        .map_err(|e| JmapError::InvalidResponse(format!("invalid credential blob: {e}")))?;

    let plaintext = crypto::decrypt(dek, &encrypted)?;

    String::from_utf8(plaintext)
        .map_err(|e| JmapError::InvalidResponse(format!("credential not valid UTF-8: {e}")))
}

// ── Probe ───────────────────────────────────────────────────────────

/// Probe a domain for JMAP support.
///
/// Returns `true` if `/.well-known/jmap` responds with HTTP 200 or 401
/// (meaning the server exists but requires auth).
#[allow(dead_code)]
pub async fn probe_jmap(domain: &str) -> Option<String> {
    let url = format!("https://{domain}/.well-known/jmap");

    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .ok()?;

    let resp = client.get(&url).send().await.ok()?;

    // 200 = session resource; 401 = server exists but needs auth
    if resp.status().is_success() || resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        Some(url)
    } else {
        None
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_urls_same_origin_accepted() {
        let session = JmapSession {
            primary_accounts: PrimaryAccounts {
                mail: Some("a1".into()),
            },
            capabilities: serde_json::json!({}),
            accounts: serde_json::json!({}),
            api_url: "https://jmap.example.com/api/".into(),
            event_source_url: None,
            upload_url: Some("https://jmap.example.com/upload/".into()),
        };
        assert!(check_session_urls("https://jmap.example.com:443", &session).is_ok());
    }

    #[test]
    fn session_urls_cross_origin_rejected() {
        // A malicious JMAP server pointing apiUrl at another host must not
        // receive our Authorization header.
        let session = JmapSession {
            primary_accounts: PrimaryAccounts {
                mail: Some("a1".into()),
            },
            capabilities: serde_json::json!({}),
            accounts: serde_json::json!({}),
            api_url: "https://evil.example/api/".into(),
            event_source_url: None,
            upload_url: None,
        };
        let err = check_session_urls("https://jmap.example.com:443", &session).unwrap_err();
        assert!(matches!(err, JmapError::CrossOrigin(_)), "got: {err}");

        let session = JmapSession {
            api_url: "https://jmap.example.com/api/".into(),
            upload_url: Some("https://evil.example/upload/".into()),
            ..session
        };
        assert!(check_session_urls("https://jmap.example.com:443", &session).is_err());
    }

    #[test]
    fn session_urls_unparseable_rejected() {
        let session = JmapSession {
            primary_accounts: PrimaryAccounts {
                mail: Some("a1".into()),
            },
            capabilities: serde_json::json!({}),
            accounts: serde_json::json!({}),
            api_url: "not a url".into(),
            event_source_url: None,
            upload_url: None,
        };
        assert!(check_session_urls("https://jmap.example.com:443", &session).is_err());
    }

    #[test]
    fn parse_jmap_mailbox() {
        let json = serde_json::json!({
            "id": "mb1",
            "name": "Inbox",
            "role": "inbox",
            "totalEmails": 42,
            "unreadEmails": 7
        });

        let mb: JmapMailbox = serde_json::from_value(json).unwrap();
        assert_eq!(mb.id, "mb1");
        assert_eq!(mb.name, "Inbox");
        assert_eq!(mb.role.as_deref(), Some("inbox"));
        assert_eq!(mb.total_emails, Some(42));
        assert_eq!(mb.unread_emails, Some(7));
    }

    #[test]
    fn parse_jmap_email() {
        let json = serde_json::json!({
            "id": "em1",
            "threadId": "th1",
            "mailboxIds": { "mb1": true },
            "keywords": { "$seen": true, "$flagged": false },
            "size": 12345,
            "receivedAt": "2025-01-15T10:00:00Z",
            "messageId": ["<msg1@example.com>"],
            "from": [{ "name": "Alice", "email": "alice@example.com" }],
            "to": [{ "name": "Bob", "email": "bob@example.com" }],
            "subject": "Hello!",
            "preview": "Hi Bob, ...",
            "hasAttachment": false
        });

        let email: JmapEmail = serde_json::from_value(json).unwrap();
        assert_eq!(email.id, "em1");
        assert!(email.is_seen());
        assert!(!email.is_flagged());
        assert_eq!(
            email.format_from(),
            Some("Alice <alice@example.com>".into())
        );
        assert_eq!(email.subject.as_deref(), Some("Hello!"));
    }

    #[test]
    fn jmap_email_body_extraction() {
        let json = serde_json::json!({
            "id": "em2",
            "subject": "Test",
            "bodyStructure": {
                "type": "text/plain",
                "partId": "p1"
            },
            "bodyValues": {
                "p1": {
                    "value": "Hello world",
                    "encoding": "UTF-8"
                }
            },
            "textBody": [{ "partId": "p1", "type": "text/plain" }],
            "htmlBody": []
        });

        let email: JmapEmail = serde_json::from_value(json).unwrap();
        assert_eq!(email.body_text(), Some("Hello world".into()));
        assert_eq!(email.body_html(), None);
    }

    #[test]
    fn jmap_email_seen_flagged() {
        let json = serde_json::json!({
            "id": "em3",
            "keywords": {}
        });
        let email: JmapEmail = serde_json::from_value(json).unwrap();
        assert!(!email.is_seen());
        assert!(!email.is_flagged());
    }

    #[test]
    fn parse_email_query_result() {
        let json = serde_json::json!({
            "ids": ["em1", "em2", "em3"],
            "queryState": "abc123",
            "total": 100
        });

        let result: EmailQueryResult = EmailQueryResult {
            ids: json["ids"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect(),
            query_state: json["queryState"].as_str().map(String::from),
            total: json["total"].as_u64(),
        };

        assert_eq!(result.ids.len(), 3);
        assert_eq!(result.query_state.as_deref(), Some("abc123"));
        assert_eq!(result.total, Some(100));
    }

    #[test]
    fn decrypt_roundtrip() {
        let key = crypto::generate_key();
        let password = "jmap-test-password";
        let encrypted = crypto::encrypt(&key, password.as_bytes()).unwrap();
        let json = serde_json::to_string(&encrypted).unwrap();
        let decrypted = decrypt_account_password(&json, &key).unwrap();
        assert_eq!(decrypted, password);
    }

    #[test]
    fn parse_mailbox_with_null_role() {
        let json = serde_json::json!({
            "id": "mb2",
            "name": "Projects"
        });

        let mb: JmapMailbox = serde_json::from_value(json).unwrap();
        assert_eq!(mb.role, None);
        assert_eq!(mb.total_emails, None);
    }

    #[test]
    fn take_ok_args_maps_jmap_error_method() {
        let resp = JmapResponse {
            method_responses: vec![(
                "error".into(),
                serde_json::json!({
                    "type": "cannotCalculateChanges",
                    "description": "state too old"
                }),
                "eqc0".into(),
            )],
            session_state: None,
        };
        let err = take_ok_args(&resp, "Email/queryChanges").unwrap_err();
        assert!(err.is_stale_query_state(), "got: {err}");
    }

    #[test]
    fn take_ok_args_returns_matching_method() {
        let resp = JmapResponse {
            method_responses: vec![(
                "Email/query".into(),
                serde_json::json!({ "ids": ["em1"], "queryState": "s1" }),
                "eq0".into(),
            )],
            session_state: None,
        };
        let args = take_ok_args(&resp, "Email/query").expect("ok");
        assert_eq!(args["queryState"], "s1");
    }

    #[test]
    fn take_ok_args_picks_named_method_among_several() {
        let resp = JmapResponse {
            method_responses: vec![
                (
                    "Email/set".into(),
                    serde_json::json!({ "created": { "draft": { "id": "e1" } } }),
                    "es0".into(),
                ),
                (
                    "EmailSubmission/set".into(),
                    serde_json::json!({ "created": { "sub": { "id": "s1" } } }),
                    "es1".into(),
                ),
            ],
            session_state: None,
        };
        let args = take_ok_args(&resp, "EmailSubmission/set").expect("ok");
        assert_eq!(args["created"]["sub"]["id"], "s1");
    }

    #[test]
    fn session_supports_submission_capability() {
        let session = JmapSession {
            primary_accounts: PrimaryAccounts {
                mail: Some("a1".into()),
            },
            capabilities: serde_json::json!({
                "urn:ietf:params:jmap:core": {},
                "urn:ietf:params:jmap:mail": {},
                "urn:ietf:params:jmap:submission": {}
            }),
            accounts: serde_json::json!({}),
            api_url: "https://jmap.example.com/api/".into(),
            event_source_url: None,
            upload_url: None,
        };
        let client = JmapClient::from_session(session, "a1".into(), "u@example.com", "pw");
        assert!(client.supports_submission());
        assert!(!client.has_capability("urn:ietf:params:jmap:calendars"));
    }

    #[test]
    fn pick_identity_prefers_matching_email() {
        let identities = vec![
            JmapIdentity {
                id: "i1".into(),
                name: "Other".into(),
                email: "other@example.com".into(),
            },
            JmapIdentity {
                id: "i2".into(),
                name: "Me".into(),
                email: "me@example.com".into(),
            },
        ];
        let picked = pick_identity(&identities, "ME@example.com").unwrap();
        assert_eq!(picked.id, "i2");
    }

    #[test]
    fn build_email_create_sets_draft_keywords_and_recipients() {
        let outbound = OutboundMessage {
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
        };
        let email = build_email_create(&outbound, Some("mb-drafts")).unwrap();
        assert_eq!(email["subject"], "Hi");
        assert_eq!(email["keywords"]["$draft"], true);
        assert_eq!(email["mailboxIds"]["mb-drafts"], true);
        assert_eq!(email["to"][0]["email"], "you@example.com");
        assert_eq!(email["bodyValues"]["bd1"]["value"], "Hello");
    }

    #[test]
    fn expand_event_source_url_fills_rfc_placeholders() {
        let t = "https://jmap.example.com/events/{types}/{closeafter}/{ping}";
        assert_eq!(
            expand_event_source_url(t),
            "https://jmap.example.com/events/*/no/30"
        );
    }

    #[test]
    fn sse_frame_detects_state_event() {
        assert!(sse_frame_is_state_push(
            "event: state\ndata: {\"changed\":{\"a1\":{\"Email\":\"s1\"}}}\n"
        ));
        assert!(!sse_frame_is_state_push("event: ping\ndata: 30\n"));
        assert!(sse_frame_is_state_push(
            "data: {\"changed\":{\"a1\":{\"Mailbox\":\"m1\"}}}\n"
        ));
    }

    #[test]
    fn stale_query_state_detects_rfc_code() {
        let err = JmapError::Method {
            code: "cannotCalculateChanges".into(),
            description: String::new(),
        };
        assert!(err.is_stale_query_state());
        let err = JmapError::InvalidResponse("nope".into());
        assert!(!err.is_stale_query_state());
    }
}
