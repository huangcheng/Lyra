//! Mail account management.
//!
//! Provides CRUD operations for multiple mail accounts per user,
//! encrypted credential storage, and automatic server configuration
//! probing (Thunderbird/Apple Mail style).
//!
//! See `docs/specs/2026-08-20-lyra-sync-and-protocols-spec.md`.

#![allow(clippy::doc_markdown)]

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use crate::api_error::ApiErrorBody;
use crate::auth::{AuthState, AuthUser};
use crate::crypto;
use crate::db_row::{InvalidIdError, id_from_row, id_param, opt_ts_from_row, ts_from_row};

/// Routes for account management.
pub fn routes() -> Router<AuthState> {
    Router::new()
        .route("/api/v1/accounts", get(list_accounts).post(create_account))
        .route(
            "/api/v1/accounts/{id}",
            get(get_account).put(update_account).delete(delete_account),
        )
        .route("/api/v1/accounts/probe", post(probe_server_config))
}

/// A mail account as returned by the API.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub id: String,
    pub display_name: String,
    pub email_address: String,
    pub protocol: String,
    pub imap_host: Option<String>,
    pub imap_port: Option<i32>,
    pub imap_security: Option<String>,
    pub smtp_host: Option<String>,
    pub smtp_port: Option<i32>,
    pub smtp_security: Option<String>,
    pub carddav_url: Option<String>,
    pub caldav_url: Option<String>,
    pub is_active: bool,
    pub sync_enabled: bool,
    pub last_sync_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Request to create a new mail account.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateAccountRequest {
    pub display_name: String,
    pub email_address: String,
    pub protocol: Option<String>,
    pub auth_type: Option<String>,
    pub password: String,
    pub imap_host: Option<String>,
    pub imap_port: Option<i32>,
    pub imap_security: Option<String>,
    pub smtp_host: Option<String>,
    pub smtp_port: Option<i32>,
    pub smtp_security: Option<String>,
    /// Base URL for JMAP discovery (`https://host` or `…/.well-known/jmap`).
    /// When omitted and `protocol` is `jmap`, derived as `https://{email-domain}`.
    pub jmap_base_url: Option<String>,
}

/// Request to update an existing mail account.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAccountRequest {
    pub display_name: Option<String>,
    pub email_address: Option<String>,
    pub password: Option<String>,
    pub imap_host: Option<String>,
    pub imap_port: Option<i32>,
    pub imap_security: Option<String>,
    pub smtp_host: Option<String>,
    pub smtp_port: Option<i32>,
    pub smtp_security: Option<String>,
    pub carddav_url: Option<String>,
    pub caldav_url: Option<String>,
    pub is_active: Option<bool>,
    pub sync_enabled: Option<bool>,
}

/// Request to probe server configuration.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeRequest {
    pub email_address: String,
    pub domain: Option<String>,
}

/// Server configuration probe result.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeResult {
    pub found: bool,
    pub source: Option<String>,
    pub protocol: String,
    /// Suggested auth: `"oauth2"` for Microsoft Outlook/365, otherwise omitted
    /// (password form). Frontend uses this to switch away from app-password UX.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_method: Option<String>,
    pub imap_host: Option<String>,
    pub imap_port: Option<i32>,
    pub imap_security: Option<String>,
    pub smtp_host: Option<String>,
    pub smtp_port: Option<i32>,
    pub smtp_security: Option<String>,
}

/// Error type for account operations.
#[derive(Debug, thiserror::Error)]
pub enum AccountError {
    #[error("account not found")]
    NotFound,
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("encryption error: {0}")]
    Crypto(#[from] crypto::CryptoError),
    #[error("invalid input: {0}")]
    InvalidInput(String),
}

impl From<InvalidIdError> for AccountError {
    fn from(_: InvalidIdError) -> Self {
        Self::NotFound
    }
}

impl IntoResponse for AccountError {
    fn into_response(self) -> axum::response::Response {
        let (status, message, code) = match self {
            AccountError::NotFound => (StatusCode::NOT_FOUND, self.to_string(), Some("not_found")),
            AccountError::Database(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal error".into(),
                Some("internal_error"),
            ),
            AccountError::Crypto(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "encryption error".into(),
                Some("internal_error"),
            ),
            AccountError::InvalidInput(msg) => (StatusCode::BAD_REQUEST, msg, Some("bad_request")),
        };
        (status, Json(ApiErrorBody::new(message, code))).into_response()
    }
}

macro_rules! account_from_row {
    ($row:expr) => {{
        Account {
            id: id_from_row(&$row, "id"),
            display_name: $row
                .get::<Option<String>, _>("display_name")
                .unwrap_or_default(),
            email_address: $row.get("email_address"),
            protocol: $row.get("protocol"),
            imap_host: $row.get("imap_host"),
            imap_port: $row.get("imap_port"),
            imap_security: $row.get("imap_security"),
            smtp_host: $row.get("smtp_host"),
            smtp_port: $row.get("smtp_port"),
            smtp_security: $row.get("smtp_security"),
            carddav_url: $row.get("carddav_url"),
            caldav_url: $row.get("caldav_url"),
            is_active: $row.get::<bool, _>("is_active"),
            sync_enabled: $row.get::<bool, _>("sync_enabled"),
            last_sync_at: opt_ts_from_row(&$row, "last_sync_at"),
            created_at: ts_from_row(&$row, "created_at"),
            updated_at: ts_from_row(&$row, "updated_at"),
        }
    }};
}

const ACCOUNT_SELECT: &str = r"
        SELECT id, display_name, email_address, protocol,
               imap_host, imap_port, imap_security,
               smtp_host, smtp_port, smtp_security,
               carddav_url, caldav_url,
               is_active, sync_enabled, last_sync_at,
               created_at, updated_at
        FROM mail_account
";

/// List all mail accounts for the authenticated user.
async fn list_accounts(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<Vec<Account>>, AccountError> {
    let db = state.db();
    let user_id = id_param(db, &user_id)?;
    let sql = format!(
        "{ACCOUNT_SELECT}        WHERE user_id = ?\n        ORDER BY created_at DESC\n        "
    );
    let accounts = db_fetch_all!(db, &sql, |row| account_from_row!(row), &user_id)?;
    Ok(Json(accounts))
}

/// Get a specific mail account by ID.
async fn get_account(
    State(state): State<AuthState>,
    Path(id): Path<String>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<Account>, AccountError> {
    let db = state.db();
    let id = id_param(db, &id)?;
    let user_id = id_param(db, &user_id)?;
    let sql = format!("{ACCOUNT_SELECT}        WHERE id = ? AND user_id = ?\n        ");
    let account = db_fetch_optional!(db, &sql, |row| account_from_row!(&row), &id, &user_id)?
        .ok_or(AccountError::NotFound)?;
    Ok(Json(account))
}

/// Create a new mail account.
#[allow(clippy::too_many_lines)]
async fn create_account(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Json(body): Json<CreateAccountRequest>,
) -> Result<(StatusCode, Json<Account>), AccountError> {
    let db = state.db();

    // Validate email format (basic check)
    if !body.email_address.contains('@') {
        return Err(AccountError::InvalidInput("invalid email address".into()));
    }

    // Security modes are allowlisted: only tls / starttls (plaintext removed).
    let imap_security = body
        .imap_security
        .as_deref()
        .map(|s| crate::netsec::normalize_security_mode(s).map(str::to_string))
        .transpose()
        .map_err(AccountError::InvalidInput)?;
    let smtp_security = body
        .smtp_security
        .as_deref()
        .map(|s| crate::netsec::normalize_security_mode(s).map(str::to_string))
        .transpose()
        .map_err(AccountError::InvalidInput)?;

    // Get user's DEK for encryption
    let dek = AuthState::get_user_dek(state.db(), &user_id).await?;

    // Encrypt the password
    let encrypted = crypto::encrypt(&dek, body.password.as_bytes())?;
    let credential_json = serde_json::to_string(&encrypted)
        .map_err(|e| AccountError::InvalidInput(format!("credential serialization failed: {e}")))?;

    let id = Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();
    let protocol = body.protocol.unwrap_or_else(|| "imap".into());
    let receive_protocol = if protocol == "jmap" {
        "jmap".to_string()
    } else {
        "imap".to_string()
    };
    let jmap_base_url = resolve_jmap_base_url(
        &protocol,
        body.jmap_base_url.as_deref(),
        &body.email_address,
    );
    let send_protocol = choose_send_protocol(
        &protocol,
        jmap_base_url.as_deref(),
        &body.email_address,
        &body.password,
    )
    .await;
    let auth_type = body.auth_type.unwrap_or_else(|| "password".into());
    let id_bind = id_param(db, &id)?;
    let user_bind = id_param(db, &user_id)?;

    db_execute!(
        db,
        r"
        INSERT INTO mail_account (
            id, user_id, display_name, email_address, protocol, auth_type,
            credential, imap_host, imap_port, imap_security,
            smtp_host, smtp_port, smtp_security, jmap_base_url,
            is_active, sync_enabled, receive_protocol, send_protocol
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ",
        &id_bind,
        &user_bind,
        &body.display_name,
        &body.email_address,
        &protocol,
        &auth_type,
        &credential_json,
        &body.imap_host,
        body.imap_port,
        &imap_security,
        &body.smtp_host,
        body.smtp_port,
        &smtp_security,
        &jmap_base_url,
        true,
        true,
        &receive_protocol,
        &send_protocol
    )?;

    let now = chrono::Utc::now().to_rfc3339();
    if let Err(error) = crate::jobs::enqueue(
        db,
        &crate::jobs::JobPayload::SyncAccount {
            account_id: id.clone(),
            user_id: user_id.clone(),
        },
        &now,
    )
    .await
    {
        tracing::warn!(%error, account_id = %id, "failed to enqueue sync after create");
    }

    Ok((
        StatusCode::CREATED,
        Json(Account {
            id,
            display_name: body.display_name,
            email_address: body.email_address,
            protocol,
            imap_host: body.imap_host,
            imap_port: body.imap_port,
            imap_security,
            smtp_host: body.smtp_host,
            smtp_port: body.smtp_port,
            smtp_security,
            carddav_url: None,
            caldav_url: None,
            is_active: true,
            sync_enabled: true,
            last_sync_at: None,
            created_at: now.clone(),
            updated_at: now,
        }),
    ))
}

/// Update an existing mail account.
#[allow(clippy::too_many_lines)]
async fn update_account(
    State(state): State<AuthState>,
    Path(id): Path<String>,
    AuthUser(user_id): AuthUser,
    Json(body): Json<UpdateAccountRequest>,
) -> Result<Json<Account>, AccountError> {
    let db = state.db();

    let id_bind = id_param(db, &id)?;
    let user_bind = id_param(db, &user_id)?;

    // Verify account exists and belongs to user
    let existing: Option<String> = db_id_optional!(
        db,
        "SELECT id FROM mail_account WHERE id = ? AND user_id = ?",
        &id_bind,
        &user_bind
    )?;
    if existing.is_none() {
        return Err(AccountError::NotFound);
    }

    if let Some(email) = &body.email_address
        && !email.contains('@')
    {
        return Err(AccountError::InvalidInput("invalid email address".into()));
    }
    let imap_security = body
        .imap_security
        .as_deref()
        .map(|s| crate::netsec::normalize_security_mode(s).map(str::to_string))
        .transpose()
        .map_err(AccountError::InvalidInput)?;
    let smtp_security = body
        .smtp_security
        .as_deref()
        .map(|s| crate::netsec::normalize_security_mode(s).map(str::to_string))
        .transpose()
        .map_err(AccountError::InvalidInput)?;
    if let Some(url) = &body.carddav_url {
        crate::netsec::validate_server_url(url).map_err(AccountError::InvalidInput)?;
    }
    if let Some(url) = &body.caldav_url {
        crate::netsec::validate_server_url(url).map_err(AccountError::InvalidInput)?;
    }

    let has_update = body.display_name.is_some()
        || body.email_address.is_some()
        || body.is_active.is_some()
        || body.sync_enabled.is_some()
        || body.password.is_some()
        || body.imap_host.is_some()
        || body.imap_port.is_some()
        || body.imap_security.is_some()
        || body.carddav_url.is_some()
        || body.caldav_url.is_some()
        || body.smtp_host.is_some()
        || body.smtp_port.is_some()
        || body.smtp_security.is_some();
    if !has_update {
        return Err(AccountError::InvalidInput("no fields to update".into()));
    }

    let credential_json = if let Some(password) = &body.password {
        let dek = AuthState::get_user_dek(state.db(), &user_id).await?;
        let encrypted = crypto::encrypt(&dek, password.as_bytes())?;
        Some(serde_json::to_string(&encrypted).map_err(|e| {
            AccountError::InvalidInput(format!("credential serialization failed: {e}"))
        })?)
    } else {
        None
    };

    db_execute!(
        db,
        r"
        UPDATE mail_account SET
            display_name = IFNULL(?, display_name),
            email_address = IFNULL(?, email_address),
            is_active = IFNULL(?, is_active),
            sync_enabled = IFNULL(?, sync_enabled),
            credential = IFNULL(?, credential),
            imap_host = IFNULL(?, imap_host),
            imap_port = IFNULL(?, imap_port),
            imap_security = IFNULL(?, imap_security),
            carddav_url = IFNULL(?, carddav_url),
            caldav_url = IFNULL(?, caldav_url),
            smtp_host = IFNULL(?, smtp_host),
            smtp_port = IFNULL(?, smtp_port),
            smtp_security = IFNULL(?, smtp_security),
            updated_at = datetime('now')
        WHERE id = ? AND user_id = ?
        ",
        body.display_name.as_ref(),
        body.email_address.as_ref(),
        body.is_active,
        body.sync_enabled,
        credential_json.as_ref(),
        body.imap_host.as_ref(),
        body.imap_port,
        imap_security.as_ref(),
        body.carddav_url.as_ref(),
        body.caldav_url.as_ref(),
        body.smtp_host.as_ref(),
        body.smtp_port,
        smtp_security.as_ref(),
        &id_bind,
        &user_bind
    )?;

    let select_sql = format!("{ACCOUNT_SELECT}        WHERE id = ? AND user_id = ?\n        ");
    let account = db_fetch_one!(
        db,
        &select_sql,
        |row| account_from_row!(&row),
        &id_bind,
        &user_bind
    )?;
    Ok(Json(account))
}

/// Delete a mail account.
async fn delete_account(
    State(state): State<AuthState>,
    Path(id): Path<String>,
    AuthUser(user_id): AuthUser,
) -> Result<StatusCode, AccountError> {
    let db = state.db();

    let id = id_param(db, &id)?;
    let user_id = id_param(db, &user_id)?;
    let affected = db_execute!(
        db,
        "DELETE FROM mail_account WHERE id = ? AND user_id = ?",
        &id,
        &user_id
    )?;

    if affected == 0 {
        return Err(AccountError::NotFound);
    }

    Ok(StatusCode::NO_CONTENT)
}

/// Resolve JMAP discovery base URL for a new account.
fn resolve_jmap_base_url(
    protocol: &str,
    explicit: Option<&str>,
    email_address: &str,
) -> Option<String> {
    if protocol != "jmap" {
        return None;
    }
    if let Some(url) = explicit.map(str::trim).filter(|s| !s.is_empty()) {
        return Some(url.to_owned());
    }
    let domain = extract_domain(email_address);
    if domain.is_empty() {
        return None;
    }
    Some(format!("https://{domain}"))
}

/// Prefer JMAP EmailSubmission when the session advertises it; otherwise SMTP.
async fn choose_send_protocol(
    protocol: &str,
    jmap_base_url: Option<&str>,
    email: &str,
    password: &str,
) -> String {
    if protocol != "jmap" {
        return "smtp".into();
    }
    let Some(base) = jmap_base_url else {
        return "smtp".into();
    };
    match crate::jmap::JmapClient::discover(base, email, password).await {
        Ok(client) if client.supports_submission() => {
            tracing::info!(%email, "JMAP submission capability present; send_protocol=jmap");
            "jmap".into()
        }
        Ok(_) => {
            tracing::info!(%email, "JMAP session lacks submission; send_protocol=smtp");
            "smtp".into()
        }
        Err(err) => {
            tracing::warn!(%email, error = %err, "JMAP probe failed; send_protocol=smtp");
            "smtp".into()
        }
    }
}

/// Probe for server configuration (Thunderbird/Apple Mail style).
///
/// Tries multiple methods in order:
/// 1. Mozilla ISPDB (Thunderbird autoconfig)
/// 2. SRV records
/// 3. Common server name patterns
///
/// Authenticated only: the common-pattern probe makes outbound TCP
/// connections, so an open probe endpoint would be an internal port-scan
/// oracle. The domain is validated and resolved addresses are filtered
/// against private/loopback ranges before connecting.
async fn probe_server_config(
    AuthUser(_user_id): AuthUser,
    Json(body): Json<ProbeRequest>,
) -> Result<Json<ProbeResult>, AccountError> {
    let domain = body
        .domain
        .clone()
        .unwrap_or_else(|| extract_domain(&body.email_address));

    let domain = crate::netsec::validate_domain(&domain).map_err(AccountError::InvalidInput)?;

    // Microsoft consumer / 365 domains should use OAuth2 (XOAUTH2), not a
    // password. ISPDB still returns outlook.office365.com + password auth,
    // which fails for modern Outlook accounts without an app password.
    if is_microsoft_mail_domain(&domain) {
        return Ok(Json(microsoft_oauth_probe_result("microsoft_domain")));
    }
    if is_yandex_mail_domain(&domain) {
        return Ok(Json(yandex_oauth_probe_result("yandex_domain")));
    }

    // Try Mozilla ISPDB first
    if let Some(config) = probe_mozilla_ispdb(&domain).await {
        return Ok(Json(annotate_oauth_probe(config)));
    }

    // Try SRV records
    if let Some(config) = probe_srv_records(&domain) {
        return Ok(Json(annotate_oauth_probe(config)));
    }

    // Try common patterns
    if let Some(config) = probe_common_patterns(&domain).await {
        return Ok(Json(annotate_oauth_probe(config)));
    }

    Ok(Json(ProbeResult {
        found: false,
        source: None,
        protocol: "imap".into(),
        auth_method: None,
        imap_host: None,
        imap_port: None,
        imap_security: None,
        smtp_host: None,
        smtp_port: None,
        smtp_security: None,
    }))
}

/// Extract domain from email address.
fn extract_domain(email: &str) -> String {
    email.split('@').nth(1).unwrap_or("").trim().to_lowercase()
}

/// Probe Mozilla ISPDB for autoconfig.
async fn probe_mozilla_ispdb(domain: &str) -> Option<ProbeResult> {
    // `domain` is validated by `validate_domain` before we get here, so it
    // contains only ASCII DNS labels and is safe to interpolate into the path.
    let url = format!("https://autoconfig.thunderbird.net/v1.1/{domain}");

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .ok()?;

    let resp = client.get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }

    let body = resp.text().await.ok()?;
    parse_mozilla_autoconfig(&body)
}

/// Parse Mozilla autoconfig XML.
fn parse_mozilla_autoconfig(xml: &str) -> Option<ProbeResult> {
    let mut imap_host = None;
    let mut imap_port = None;
    let mut imap_security = None;
    let mut smtp_host = None;
    let mut smtp_port = None;
    let mut smtp_security = None;

    // Extract IMAP settings
    if let Some(start) = xml.find("<incomingServer type=\"imap\">") {
        let section = &xml[start..];
        imap_host = extract_xml_value(section, "hostname");
        imap_port = extract_xml_value(section, "port").and_then(|p| p.parse().ok());
        imap_security = extract_xml_value(section, "socketType");
    }

    // Extract SMTP settings
    if let Some(start) = xml.find("<outgoingServer type=\"smtp\">") {
        let section = &xml[start..];
        smtp_host = extract_xml_value(section, "hostname");
        smtp_port = extract_xml_value(section, "port").and_then(|p| p.parse().ok());
        smtp_security = extract_xml_value(section, "socketType");
    }

    if imap_host.is_some() || smtp_host.is_some() {
        Some(ProbeResult {
            found: true,
            source: Some("mozilla_ispdb".into()),
            protocol: "imap".into(),
            auth_method: None,
            imap_host,
            imap_port,
            imap_security: normalize_security(imap_security.as_deref()),
            smtp_host,
            smtp_port,
            smtp_security: normalize_security(smtp_security.as_deref()),
        })
    } else {
        None
    }
}

/// Extract value from XML tag.
fn extract_xml_value(xml: &str, tag: &str) -> Option<String> {
    let start_tag = format!("<{tag}>");
    let end_tag = format!("</{tag}>");

    let start = xml.find(&start_tag)? + start_tag.len();
    let end = xml.find(&end_tag)?;
    Some(xml[start..end].trim().to_string())
}

/// Normalize security type to our format.
///
/// `plain`/`none` socket types map to `None`: plaintext transport is not
/// supported, so the probe result omits the mode and the UI keeps its
/// (encrypted) default instead of suggesting an insecure configuration.
fn normalize_security(security: Option<&str>) -> Option<String> {
    match security {
        Some("SSL" | "TLS") => Some("tls".into()),
        Some("STARTTLS") => Some("starttls".into()),
        Some("plain" | "none") => None,
        _ => Some("tls".into()),
    }
}

/// Probe SRV records for server configuration.
fn probe_srv_records(_domain: &str) -> Option<ProbeResult> {
    // SRV record lookup would go here
    // For now, return None to fall through to common patterns
    None
}

/// Probe common server name patterns.
async fn probe_common_patterns(domain: &str) -> Option<ProbeResult> {
    let common_hosts = vec![
        (format!("imap.{domain}"), 993u16, "tls"),
        (format!("mail.{domain}"), 993, "tls"),
        (format!("imap.{domain}"), 143, "starttls"),
        (format!("mail.{domain}"), 143, "starttls"),
    ];

    let smtp_hosts = vec![
        (format!("smtp.{domain}"), 587, "starttls"),
        (format!("smtp.{domain}"), 465, "tls"),
        (format!("mail.{domain}"), 587, "starttls"),
        (format!("mail.{domain}"), 465, "tls"),
    ];

    // Try to connect to common IMAP ports
    let mut imap_config = None;
    for (host, port, security) in &common_hosts {
        if try_tcp_connect(host, *port).await {
            imap_config = Some((host.clone(), i32::from(*port), security.to_string()));
            break;
        }
    }

    // Try to connect to common SMTP ports
    let mut smtp_config = None;
    for (host, port, security) in &smtp_hosts {
        if try_tcp_connect(host, *port).await {
            smtp_config = Some((host.clone(), i32::from(*port), security.to_string()));
            break;
        }
    }

    if imap_config.is_some() || smtp_config.is_some() {
        Some(ProbeResult {
            found: true,
            source: Some("common_patterns".into()),
            protocol: "imap".into(),
            auth_method: None,
            imap_host: imap_config.as_ref().map(|c| c.0.clone()),
            imap_port: imap_config.as_ref().map(|c| c.1),
            imap_security: imap_config.as_ref().map(|c| c.2.clone()),
            smtp_host: smtp_config.as_ref().map(|c| c.0.clone()),
            smtp_port: smtp_config.as_ref().map(|c| c.1),
            smtp_security: smtp_config.as_ref().map(|c| c.2.clone()),
        })
    } else {
        None
    }
}

/// True for Microsoft consumer / 365 mailbox domains that should use OAuth2.
pub(crate) fn is_microsoft_mail_domain(domain: &str) -> bool {
    const EXACT: &[&str] = &[
        "outlook.com",
        "hotmail.com",
        "live.com",
        "live.in",
        "msn.com",
        "passport.com",
        "office365.com",
    ];
    let d = domain.trim().to_ascii_lowercase();
    if EXACT.iter().any(|e| *e == d) {
        return true;
    }
    d.ends_with(".outlook.com")
        || d.ends_with(".hotmail.com")
        || d.ends_with(".live.com")
        || d.ends_with(".msn.com")
        || d.ends_with(".onmicrosoft.com")
}

/// True for Yandex mailbox domains that should use OAuth2.
pub(crate) fn is_yandex_mail_domain(domain: &str) -> bool {
    const EXACT: &[&str] = &[
        "yandex.ru",
        "yandex.com",
        "ya.ru",
        "yandex.by",
        "yandex.kz",
        "yandex.ua",
        "yandex.com.tr",
        "yandex.az",
        "yandex.co.il",
        "yandex.lv",
        "yandex.ee",
        "yandex.lt",
        "yandex.md",
        "yandex.tj",
        "yandex.tm",
        "narod.ru",
    ];
    let d = domain.trim().to_ascii_lowercase();
    if EXACT.iter().any(|e| *e == d) {
        return true;
    }
    d.ends_with(".yandex.ru") || d.ends_with(".yandex.com")
}

pub(crate) fn is_yandex_mail_host(host: &str) -> bool {
    let h = host.trim().to_ascii_lowercase();
    h == "imap.yandex.com" || h == "smtp.yandex.com" || h.ends_with(".yandex.com")
}

pub(crate) fn is_microsoft_mail_host(host: &str) -> bool {
    let h = host.trim().to_ascii_lowercase();
    h == "outlook.office365.com"
        || h == "outlook.office.com"
        || h == "smtp-mail.outlook.com"
        || h.ends_with(".office365.com")
        || h.ends_with(".outlook.com")
}

fn microsoft_oauth_probe_result(source: &str) -> ProbeResult {
    ProbeResult {
        found: true,
        source: Some(source.into()),
        protocol: "imap".into(),
        auth_method: Some("oauth2".into()),
        imap_host: Some("outlook.office365.com".into()),
        imap_port: Some(993),
        imap_security: Some("tls".into()),
        smtp_host: Some("smtp-mail.outlook.com".into()),
        smtp_port: Some(587),
        smtp_security: Some("starttls".into()),
    }
}

fn yandex_oauth_probe_result(source: &str) -> ProbeResult {
    ProbeResult {
        found: true,
        source: Some(source.into()),
        protocol: "imap".into(),
        auth_method: Some("oauth2".into()),
        imap_host: Some("imap.yandex.com".into()),
        imap_port: Some(993),
        imap_security: Some("tls".into()),
        smtp_host: Some("smtp.yandex.com".into()),
        smtp_port: Some(465),
        smtp_security: Some("tls".into()),
    }
}

/// If ISPDB/SRV pointed at OAuth-capable mail hosts, prefer OAuth2 over password.
fn annotate_oauth_probe(mut result: ProbeResult) -> ProbeResult {
    let oauth_host = result
        .imap_host
        .as_deref()
        .is_some_and(|h| is_microsoft_mail_host(h) || is_yandex_mail_host(h))
        || result
            .smtp_host
            .as_deref()
            .is_some_and(|h| is_microsoft_mail_host(h) || is_yandex_mail_host(h));
    if oauth_host {
        result.auth_method = Some("oauth2".into());
    }
    result
}

/// Try to connect to a TCP port.
///
/// Resolves the hostname first and refuses to connect when every resolved
/// address is loopback/private/link-local/reserved (SSRF guard — wildcard
/// DNS names like `10.0.0.5.nip.io` must not reach internal hosts). When
/// resolution yields a mix, only public addresses are tried.
async fn try_tcp_connect(host: &str, port: u16) -> bool {
    let Ok(addrs) = tokio::net::lookup_host((host, port)).await else {
        return false;
    };
    let public = crate::netsec::filter_public_addrs(addrs.map(|a| a.ip()));
    for ip in public {
        if tokio::net::TcpStream::connect(std::net::SocketAddr::new(ip, port))
            .await
            .is_ok()
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_security_allowlist_only() {
        assert_eq!(normalize_security(Some("SSL")), Some("tls".into()));
        assert_eq!(normalize_security(Some("TLS")), Some("tls".into()));
        assert_eq!(
            normalize_security(Some("STARTTLS")),
            Some("starttls".into())
        );
        // Plaintext is unsupported — never suggest it.
        assert_eq!(normalize_security(Some("plain")), None);
        assert_eq!(normalize_security(Some("none")), None);
        // Unknown → default to TLS (secure default).
        assert_eq!(normalize_security(Some("weird")), Some("tls".into()));
        assert_eq!(normalize_security(None), Some("tls".into()));
    }

    #[test]
    fn microsoft_domains_prefer_oauth2() {
        assert!(is_microsoft_mail_domain("live.in"));
        assert!(is_microsoft_mail_domain("outlook.com"));
        assert!(is_microsoft_mail_domain("Hotmail.Com"));
        assert!(is_microsoft_mail_domain("contoso.onmicrosoft.com"));
        assert!(!is_microsoft_mail_domain("fastmail.com"));
        assert!(!is_microsoft_mail_domain("example.com"));
    }

    #[test]
    fn microsoft_hosts_annotate_oauth2() {
        assert!(is_microsoft_mail_host("outlook.office365.com"));
        assert!(is_microsoft_mail_host("smtp-mail.outlook.com"));
        assert!(!is_microsoft_mail_host("imap.fastmail.com"));

        let annotated = annotate_oauth_probe(ProbeResult {
            found: true,
            source: Some("mozilla_ispdb".into()),
            protocol: "imap".into(),
            auth_method: None,
            imap_host: Some("outlook.office365.com".into()),
            imap_port: Some(993),
            imap_security: Some("tls".into()),
            smtp_host: Some("smtp-mail.outlook.com".into()),
            smtp_port: Some(587),
            smtp_security: Some("starttls".into()),
        });
        assert_eq!(annotated.auth_method.as_deref(), Some("oauth2"));
    }
}
