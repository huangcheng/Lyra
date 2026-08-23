//! SMTP send helpers and the compose HTTP handler.

use axum::{extract::State, Json};
use serde::Serialize;
use uuid::Uuid;

use super::types::SyncError;
use crate::auth::{AuthState, AuthUser};
use crate::db_row::{id_from_row, id_param};
use crate::kernel::App;
use crate::protocol::SendHandle;
use crate::smtp::{OutboundMessage, SmtpAdapter, SmtpConfig, SmtpSecurity};
use crate::storage::DbPool;
use sqlx::Row;

/// Look up a send plugin by `send_protocol`. Unknown ids map to HTTP 400.
pub(crate) fn resolve_send_plugin(app: &App, send_protocol: &str) -> Result<SendHandle, SyncError> {
    app.send(send_protocol)
        .map_err(|e| SyncError::InvalidInput(e.to_string()))
}

/// Request to send a message.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendMessageRequest {
    pub account_id: String,
    pub to: Vec<serde_json::Value>,
    pub subject: String,
    pub body_text: Option<String>,
    pub body_html: Option<String>,
    pub cc: Option<Vec<serde_json::Value>>,
    pub bcc: Option<Vec<serde_json::Value>>,
    pub in_reply_to: Option<String>,
    pub references: Option<String>,
}

/// Response for send operation.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SendMessageResponse {
    pub status: String,
    pub message_id: String,
}


/// Send a message through the account's `SendPlugin` (SMTP today).
///
/// Mailbox sync is queued for workers; single-message SMTP stays request-scoped
/// so compose can await success without a 202/`jobId` handshake.
pub(crate) async fn send_message(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Json(body): Json<SendMessageRequest>,
) -> Result<Json<SendMessageResponse>, SyncError> {
    let db = state.db();
    let account_bind = id_param(db, &body.account_id)?;
    let user_bind = id_param(db, &user_id)?;

    let row = db_fetch_optional!(
        db,
        r"
        SELECT email_address, send_protocol, is_active
        FROM mail_account
        WHERE id = ? AND user_id = ?
        ",
        |row| {
            let is_active: bool = row.get("is_active");
            let send_protocol: String = row.get("send_protocol");
            let email_address: String = row.get("email_address");
            (is_active, send_protocol, email_address)
        },
        &account_bind,
        &user_bind
    )?
    .ok_or(SyncError::AccountNotFound)?;

    let (is_active, send_protocol, email_address) = row;
    if !is_active {
        return Err(SyncError::AccountDisabled);
    }

    let plugin = resolve_send_plugin(state.app.as_ref(), &send_protocol)?;

    let to = parse_address_list(&body.to);
    let cc = parse_address_list(&body.cc.unwrap_or_default());
    let bcc = parse_address_list(&body.bcc.unwrap_or_default());

    if to.is_empty() {
        return Err(SyncError::InvalidInput("No recipients specified".into()));
    }

    let outbound = OutboundMessage {
        from_email: email_address,
        from_name: None,
        to,
        cc,
        bcc,
        subject: body.subject,
        body_text: body.body_text,
        body_html: body.body_html,
        in_reply_to: body.in_reply_to,
        references: body.references,
    };

    let raw = serde_json::to_string(&outbound)
        .map_err(|e| SyncError::InvalidInput(format!("cannot encode outbound: {e}")))?;

    plugin
        .send(&body.account_id, &raw)
        .await
        .map_err(SyncError::Protocol)?;

    let message_id = format!(
        "sent-{}",
        Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext))
    );

    Ok(Json(SendMessageResponse {
        status: "sent".into(),
        message_id,
    }))
}

pub(crate) fn parse_address_list(values: &[serde_json::Value]) -> Vec<(Option<String>, String)> {
    values
        .iter()
        .filter_map(|v| {
            if let Some(s) = v.as_str() {
                Some((None, s.to_string()))
            } else {
                v.get("email").and_then(|e| e.as_str()).map(|email| {
                    let name = v.get("name").and_then(|n| n.as_str()).map(String::from);
                    (name, email.to_string())
                })
            }
        })
        .collect()
}

/// Deliver an outbound message through the SMTP adapter.
pub(crate) async fn deliver_smtp(
    config: SmtpConfig,
    outbound: OutboundMessage,
) -> Result<String, SyncError> {
    let adapter = SmtpAdapter::connect(&config)?;
    Ok(adapter.send(&outbound).await?)
}

/// Load SMTP settings for `account_id` and build an outbound message from raw source.
///
/// `raw` may be JSON [`OutboundMessage`] (HTTP compose) or minimal RFC822-ish text.
pub(crate) async fn prepare_smtp_send(
    db: &DbPool,
    account_id: &str,
    raw: &str,
) -> Result<(SmtpConfig, OutboundMessage), SyncError> {
    let row = db_fetch_optional!(
        db,
        r"
        SELECT email_address, credential, user_id,
               smtp_host, smtp_port, smtp_security, is_active
        FROM mail_account
        WHERE id = ?
        ",
        |row| {
            let is_active: bool = row.get("is_active");
            let smtp_host: Option<String> = row.get("smtp_host");
            let smtp_port: Option<i32> = row.get("smtp_port");
            let smtp_security: Option<String> = row.get("smtp_security");
            let credential_json: String = row.get("credential");
            let email_address: String = row.get("email_address");
            let user_id = id_from_row(&row, "user_id");
            (
                is_active,
                smtp_host,
                smtp_port,
                smtp_security,
                credential_json,
                email_address,
                user_id,
            )
        },
        &id_param(db, account_id)?
    )?
    .ok_or(SyncError::AccountNotFound)?;

    let (is_active, smtp_host, smtp_port, smtp_security, credential_json, email_address, user_id) =
        row;
    if !is_active {
        return Err(SyncError::AccountDisabled);
    }

    let host =
        smtp_host.ok_or_else(|| SyncError::InvalidInput("SMTP host not configured".into()))?;
    let port = u16::try_from(smtp_port.unwrap_or(587)).unwrap_or(587);
    let security = match smtp_security.as_deref() {
        Some(s) => {
            match crate::netsec::normalize_security_mode(s).map_err(SyncError::InvalidInput)? {
                "tls" => SmtpSecurity::Tls,
                _ => SmtpSecurity::Starttls,
            }
        }
        None => SmtpSecurity::Starttls,
    };

    let dek = crate::auth::AuthState::get_user_dek(db, &user_id)
        .await
        .map_err(|e| SyncError::Crypto(e.to_string()))?;
    let password = crate::smtp::decrypt_account_password(&credential_json, &dek)?;

    let config = SmtpConfig {
        host,
        port,
        security,
        username: email_address.clone(),
        password: zeroize::Zeroizing::new(password),
    };

    let outbound = outbound_from_raw(email_address, raw)?;
    Ok((config, outbound))
}

pub(crate) fn outbound_from_raw(from_email: String, raw: &str) -> Result<OutboundMessage, SyncError> {
    let trimmed = raw.trim_start();
    if trimmed.starts_with('{') {
        let mut outbound: OutboundMessage = serde_json::from_str(trimmed)
            .map_err(|e| SyncError::InvalidInput(format!("invalid outbound JSON: {e}")))?;
        outbound.from_email = from_email;
        if outbound.to.is_empty() {
            return Err(SyncError::InvalidInput("No recipients specified".into()));
        }
        return Ok(outbound);
    }

    let to = recipients_from_raw(raw);
    if to.is_empty() {
        return Err(SyncError::InvalidInput("No recipients specified".into()));
    }

    Ok(OutboundMessage {
        from_email,
        from_name: None,
        to,
        cc: vec![],
        bcc: vec![],
        subject: subject_from_raw(raw).unwrap_or_default(),
        body_text: Some(body_from_raw(raw)),
        body_html: None,
        in_reply_to: None,
        references: None,
    })
}

pub(crate) fn body_from_raw(raw: &str) -> String {
    if let Some(idx) = raw.find("\n\n") {
        raw[idx + 2..].to_string()
    } else if let Some(idx) = raw.find("\r\n\r\n") {
        raw[idx + 4..].to_string()
    } else {
        raw.to_string()
    }
}

pub(crate) fn recipients_from_raw(raw: &str) -> Vec<(Option<String>, String)> {
    for line in raw.lines() {
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("to")
        {
            return value
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| (None, s.to_string()))
                .collect();
        }
    }
    Vec::new()
}

pub(crate) fn subject_from_raw(raw: &str) -> Option<String> {
    for line in raw.lines() {
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("subject")
        {
            return Some(value.trim().to_string());
        }
    }
    None
}
