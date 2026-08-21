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
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use crate::auth::AuthState;
use crate::crypto;
use crate::storage::DbPool;

/// Routes for account management.
pub fn routes() -> Router<AuthState> {
    Router::new()
        .route("/api/accounts", get(list_accounts).post(create_account))
        .route(
            "/api/accounts/{id}",
            get(get_account).put(update_account).delete(delete_account),
        )
        .route("/api/accounts/probe", post(probe_server_config))
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
    #[error("authentication required")]
    Unauthorized,
}

impl IntoResponse for AccountError {
    fn into_response(self) -> axum::response::Response {
        let (status, message) = match self {
            AccountError::NotFound => (StatusCode::NOT_FOUND, self.to_string()),
            AccountError::Database(_) => {
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error".into())
            }
            AccountError::Crypto(_) => {
                (StatusCode::INTERNAL_SERVER_ERROR, "encryption error".into())
            }
            AccountError::InvalidInput(msg) => (StatusCode::BAD_REQUEST, msg),
            AccountError::Unauthorized => (StatusCode::UNAUTHORIZED, self.to_string()),
        };
        (status, Json(serde_json::json!({ "error": message }))).into_response()
    }
}

/// Extract user ID from auth header and session store.
async fn get_user_id(state: &AuthState, headers: &HeaderMap) -> Result<String, AccountError> {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(AccountError::Unauthorized)?;

    state
        .sessions
        .get_session(token)
        .await
        .ok_or(AccountError::Unauthorized)
}

/// Get SQLite pool from DbPool enum.
fn get_sqlite_pool(db: &DbPool) -> &sqlx::SqlitePool {
    match db {
        DbPool::Sqlite(pool) => pool,
        #[cfg(feature = "postgres")]
        DbPool::Postgres(_) => panic!("PostgreSQL not supported yet"),
    }
}

/// List all mail accounts for the authenticated user.
async fn list_accounts(
    State(state): State<AuthState>,
    headers: HeaderMap,
) -> Result<Json<Vec<Account>>, AccountError> {
    let user_id = get_user_id(&state, &headers).await?;
    let pool = get_sqlite_pool(state.db());

    let rows = sqlx::query(
        r"
        SELECT id, display_name, email_address, protocol,
               imap_host, imap_port, imap_security,
               smtp_host, smtp_port, smtp_security,
               is_active, sync_enabled, last_sync_at,
               created_at, updated_at
        FROM mail_account
        WHERE user_id = ?
        ORDER BY created_at DESC
        ",
    )
    .bind(&user_id)
    .fetch_all(pool)
    .await?;

    let accounts: Vec<Account> = rows
        .iter()
        .map(|row| Account {
            id: row.get("id"),
            display_name: row
                .get::<Option<String>, _>("display_name")
                .unwrap_or_default(),
            email_address: row.get("email_address"),
            protocol: row.get("protocol"),
            imap_host: row.get("imap_host"),
            imap_port: row.get("imap_port"),
            imap_security: row.get("imap_security"),
            smtp_host: row.get("smtp_host"),
            smtp_port: row.get("smtp_port"),
            smtp_security: row.get("smtp_security"),
            carddav_url: row.get("carddav_url"),
            caldav_url: row.get("caldav_url"),
            is_active: row.get::<bool, _>("is_active"),
            sync_enabled: row.get::<bool, _>("sync_enabled"),
            last_sync_at: row.get("last_sync_at"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        })
        .collect();

    Ok(Json(accounts))
}

/// Get a specific mail account by ID.
async fn get_account(
    State(state): State<AuthState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Account>, AccountError> {
    let user_id = get_user_id(&state, &headers).await?;
    let pool = get_sqlite_pool(state.db());

    let row = sqlx::query(
        r"
        SELECT id, display_name, email_address, protocol,
               imap_host, imap_port, imap_security,
               smtp_host, smtp_port, smtp_security,
               is_active, sync_enabled, last_sync_at,
               created_at, updated_at
        FROM mail_account
        WHERE id = ? AND user_id = ?
        ",
    )
    .bind(&id)
    .bind(&user_id)
    .fetch_optional(pool)
    .await?
    .ok_or(AccountError::NotFound)?;

    Ok(Json(Account {
        id: row.get("id"),
        display_name: row
            .get::<Option<String>, _>("display_name")
            .unwrap_or_default(),
        email_address: row.get("email_address"),
        protocol: row.get("protocol"),
        imap_host: row.get("imap_host"),
        imap_port: row.get("imap_port"),
        imap_security: row.get("imap_security"),
        smtp_host: row.get("smtp_host"),
        smtp_port: row.get("smtp_port"),
        smtp_security: row.get("smtp_security"),
            carddav_url: row.get("carddav_url"),
            caldav_url: row.get("caldav_url"),
        is_active: row.get::<bool, _>("is_active"),
        sync_enabled: row.get::<bool, _>("sync_enabled"),
        last_sync_at: row.get("last_sync_at"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }))
}

/// Create a new mail account.
async fn create_account(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Json(body): Json<CreateAccountRequest>,
) -> Result<(StatusCode, Json<Account>), AccountError> {
    let user_id = get_user_id(&state, &headers).await?;
    let pool = get_sqlite_pool(state.db());

    // Validate email format (basic check)
    if !body.email_address.contains('@') {
        return Err(AccountError::InvalidInput("invalid email address".into()));
    }

    // Get user's DEK for encryption
    let dek = AuthState::get_user_dek()?;

    // Encrypt the password
    let encrypted = crypto::encrypt(&dek, body.password.as_bytes())?;
    let credential_json = serde_json::to_string(&encrypted)
        .map_err(|e| AccountError::InvalidInput(format!("credential serialization failed: {e}")))?;

    let id = Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();
    let protocol = body.protocol.unwrap_or_else(|| "imap".into());
    let auth_type = body.auth_type.unwrap_or_else(|| "password".into());

    sqlx::query(
        r"
        INSERT INTO mail_account (
            id, user_id, display_name, email_address, protocol, auth_type,
            credential, imap_host, imap_port, imap_security,
            smtp_host, smtp_port, smtp_security, is_active, sync_enabled
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, 1)
        ",
    )
    .bind(&id)
    .bind(&user_id)
    .bind(&body.display_name)
    .bind(&body.email_address)
    .bind(&protocol)
    .bind(&auth_type)
    .bind(&credential_json)
    .bind(&body.imap_host)
    .bind(body.imap_port)
    .bind(&body.imap_security)
    .bind(&body.smtp_host)
    .bind(body.smtp_port)
    .bind(&body.smtp_security)
    .execute(pool)
    .await?;

    let now = chrono::Utc::now().to_rfc3339();
    Ok((
        StatusCode::CREATED,
        Json(Account {
            id,
            display_name: body.display_name,
            email_address: body.email_address,
            protocol,
            imap_host: body.imap_host,
            imap_port: body.imap_port,
            imap_security: body.imap_security,
            smtp_host: body.smtp_host,
            smtp_port: body.smtp_port,
            smtp_security: body.smtp_security,
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
    headers: HeaderMap,
    Json(body): Json<UpdateAccountRequest>,
) -> Result<Json<Account>, AccountError> {
    let user_id = get_user_id(&state, &headers).await?;
    let pool = get_sqlite_pool(state.db());

    // Verify account exists and belongs to user
    let _existing = sqlx::query("SELECT id FROM mail_account WHERE id = ? AND user_id = ?")
        .bind(&id)
        .bind(&user_id)
        .fetch_optional(pool)
        .await?
        .ok_or(AccountError::NotFound)?;

    // Build dynamic update
    let mut updates = Vec::new();
    let mut params: Vec<String> = Vec::new();

    if let Some(display_name) = &body.display_name {
        updates.push("display_name = ?");
        params.push(display_name.clone());
    }
    if let Some(email) = &body.email_address {
        if !email.contains('@') {
            return Err(AccountError::InvalidInput("invalid email address".into()));
        }
        updates.push("email_address = ?");
        params.push(email.clone());
    }
    if let Some(is_active) = body.is_active {
        updates.push("is_active = ?");
        params.push(if is_active { "1" } else { "0" }.into());
    }
    if let Some(sync_enabled) = body.sync_enabled {
        updates.push("sync_enabled = ?");
        params.push(if sync_enabled { "1" } else { "0" }.into());
    }

    // Update password if provided
    if let Some(password) = &body.password {
        let dek = AuthState::get_user_dek()?;
        let encrypted = crypto::encrypt(&dek, password.as_bytes())?;
        let credential_json = serde_json::to_string(&encrypted).map_err(|e| {
            AccountError::InvalidInput(format!("credential serialization failed: {e}"))
        })?;
        updates.push("credential = ?");
        params.push(credential_json);
    }

    // Update IMAP settings
    if body.imap_host.is_some() || body.imap_port.is_some() || body.imap_security.is_some() {
        if let Some(host) = &body.imap_host {
            updates.push("imap_host = ?");
            params.push(host.clone());
        }
        if let Some(port) = body.imap_port {
            updates.push("imap_port = ?");
            params.push(port.to_string());
        }
        if let Some(security) = &body.imap_security {
            updates.push("imap_security = ?");
            params.push(security.clone());
        }
    }

    if let Some(url) = &body.carddav_url {
        updates.push("carddav_url = ?");
        params.push(url.clone());
    }
    if let Some(url) = &body.caldav_url {
        updates.push("caldav_url = ?");
        params.push(url.clone());
    }

    // Update SMTP settings
    if body.smtp_host.is_some() || body.smtp_port.is_some() || body.smtp_security.is_some() {
        if let Some(host) = &body.smtp_host {
            updates.push("smtp_host = ?");
            params.push(host.clone());
        }
        if let Some(port) = body.smtp_port {
            updates.push("smtp_port = ?");
            params.push(port.to_string());
        }
        if let Some(security) = &body.smtp_security {
            updates.push("smtp_security = ?");
            params.push(security.clone());
        }
    }

    if updates.is_empty() {
        return Err(AccountError::InvalidInput("no fields to update".into()));
    }

    updates.push("updated_at = datetime('now')");
    params.push(id.clone());
    params.push(user_id.clone());

    let sql = format!(
        "UPDATE mail_account SET {} WHERE id = ? AND user_id = ?",
        updates.join(", ")
    );

    // Execute with dynamic params
    let mut query = sqlx::query(&sql);
    for param in &params {
        query = query.bind(param);
    }
    query.execute(pool).await?;

    // Return updated account
    let row = sqlx::query(
        r"
        SELECT id, display_name, email_address, protocol,
               imap_host, imap_port, imap_security,
               smtp_host, smtp_port, smtp_security,
               is_active, sync_enabled, last_sync_at,
               created_at, updated_at
        FROM mail_account
        WHERE id = ? AND user_id = ?
        ",
    )
    .bind(&id)
    .bind(&user_id)
    .fetch_one(pool)
    .await?;

    Ok(Json(Account {
        id: row.get("id"),
        display_name: row
            .get::<Option<String>, _>("display_name")
            .unwrap_or_default(),
        email_address: row.get("email_address"),
        protocol: row.get("protocol"),
        imap_host: row.get("imap_host"),
        imap_port: row.get("imap_port"),
        imap_security: row.get("imap_security"),
        smtp_host: row.get("smtp_host"),
        smtp_port: row.get("smtp_port"),
        smtp_security: row.get("smtp_security"),
            carddav_url: row.get("carddav_url"),
            caldav_url: row.get("caldav_url"),
        is_active: row.get::<bool, _>("is_active"),
        sync_enabled: row.get::<bool, _>("sync_enabled"),
        last_sync_at: row.get("last_sync_at"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }))
}

/// Delete a mail account.
async fn delete_account(
    State(state): State<AuthState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, AccountError> {
    let user_id = get_user_id(&state, &headers).await?;
    let pool = get_sqlite_pool(state.db());

    let result = sqlx::query("DELETE FROM mail_account WHERE id = ? AND user_id = ?")
        .bind(&id)
        .bind(&user_id)
        .execute(pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AccountError::NotFound);
    }

    Ok(StatusCode::NO_CONTENT)
}

/// Probe for server configuration (Thunderbird/Apple Mail style).
///
/// Tries multiple methods in order:
/// 1. Mozilla ISPDB (Thunderbird autoconfig)
/// 2. SRV records
/// 3. Common server name patterns
async fn probe_server_config(
    State(_state): State<AuthState>,
    Json(body): Json<ProbeRequest>,
) -> Result<Json<ProbeResult>, AccountError> {
    let domain = body
        .domain
        .clone()
        .unwrap_or_else(|| extract_domain(&body.email_address));

    if domain.is_empty() {
        return Err(AccountError::InvalidInput(
            "could not extract domain from email".into(),
        ));
    }

    // Try Mozilla ISPDB first
    if let Some(config) = probe_mozilla_ispdb(&domain).await {
        return Ok(Json(config));
    }

    // Try SRV records
    if let Some(config) = probe_srv_records(&domain) {
        return Ok(Json(config));
    }

    // Try common patterns
    if let Some(config) = probe_common_patterns(&domain).await {
        return Ok(Json(config));
    }

    Ok(Json(ProbeResult {
        found: false,
        source: None,
        protocol: "imap".into(),
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
fn normalize_security(security: Option<&str>) -> Option<String> {
    match security {
        Some("SSL" | "TLS") => Some("tls".into()),
        Some("STARTTLS") => Some("starttls".into()),
        Some("plain" | "none") => Some("none".into()),
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

/// Try to connect to a TCP port.
async fn try_tcp_connect(host: &str, port: u16) -> bool {
    tokio::net::TcpStream::connect(format!("{host}:{port}"))
        .await
        .is_ok()
}
