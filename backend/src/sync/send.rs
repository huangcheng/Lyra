//! SMTP send helpers and the compose HTTP handler.

use axum::{Json, extract::State};
use sea_orm::sea_query::Query as Sq;
use sea_orm::{ColumnTrait, ConnectionTrait, QueryResult, Value};
use serde::Serialize;
use uuid::Uuid;

use super::types::SyncError;
use crate::auth::{AuthSession, AuthState};
use crate::entities::mail_account;
use crate::kernel::App;
use crate::opengpg::keys::OpengpgError;
use crate::opengpg::send::{
    OpengpgSendOptions, OutboundDraft, collect_recipient_emails, wrap_outbound_opengpg,
};
use crate::protocol::SendHandle;
use crate::smtp::{OutboundMessage, SmtpAdapter, SmtpConfig, SmtpSecurity};
use crate::storage::DbPool;

// ── SeaORM plumbing (entity queries on `db.orm()`) ──────────────────
//
// Account ids are TEXT on SQLite and native UUID on Postgres; `IdParam`
// keeps the parse semantics the macro layer used.

/// Unwrap the driver error SeaORM wraps so [`SyncError::Database`] keeps
/// reporting the underlying `sqlx::Error`; non-driver SeaORM errors become
/// `sqlx::Error::Protocol` with the original message.
fn orm_err(err: sea_orm::DbErr) -> SyncError {
    use sea_orm::RuntimeErr;
    let sqlx_err = match err {
        sea_orm::DbErr::Exec(RuntimeErr::SqlxError(e))
        | sea_orm::DbErr::Query(RuntimeErr::SqlxError(e))
        | sea_orm::DbErr::Conn(RuntimeErr::SqlxError(e)) => std::sync::Arc::try_unwrap(e)
            .unwrap_or_else(|shared| sqlx::Error::Protocol(shared.to_string())),
        other => sqlx::Error::Protocol(other.to_string()),
    };
    SyncError::Database(sqlx_err)
}

/// Dialect-aware bind for a UUID-column id.
fn id_value(db: &DbPool, id: &str) -> Result<Value, SyncError> {
    Ok(
        match crate::db_row::id_param(db, id)
            .map_err(|e| SyncError::Database(sqlx::Error::from(e)))?
        {
            crate::db_row::IdParam::Text(s) => Value::String(Some(s)),
            crate::db_row::IdParam::Uuid(u) => Value::Uuid(Some(u)),
        },
    )
}

/// Decode an id column from either engine (TEXT on SQLite, UUID on Postgres).
fn row_id(row: &QueryResult, col: &str) -> Result<String, SyncError> {
    if let Some(text) = row.try_get::<Option<String>>("", col).map_err(orm_err)? {
        return Ok(text);
    }
    row.try_get::<Option<uuid::Uuid>>("", col)
        .map_err(orm_err)?
        .map(|u| u.to_string())
        .ok_or_else(|| orm_err(sea_orm::DbErr::RecordNotFound("id column was NULL".into())))
}

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
    #[serde(default)]
    pub opengpg: Option<OpengpgSendOptions>,
}

/// Response for send operation.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SendMessageResponse {
    pub status: String,
    pub message_id: String,
}

/// All unique recipient emails of the request (to + cc + bcc).
fn opengpg_recipient_emails(body: &SendMessageRequest) -> Vec<String> {
    let mut recipients = collect_recipient_emails(&body.to);
    recipients.extend(collect_recipient_emails(
        &body.cc.clone().unwrap_or_default(),
    ));
    recipients.extend(collect_recipient_emails(
        &body.bcc.clone().unwrap_or_default(),
    ));
    recipients
}

/// Send a message through the account's `SendPlugin` (SMTP today).
///
/// Mailbox sync is queued for workers; single-message SMTP stays request-scoped
/// so compose can await success without a 202/`jobId` handshake.
pub(crate) async fn send_message(
    State(state): State<AuthState>,
    session: AuthSession,
    Json(body): Json<SendMessageRequest>,
) -> Result<Json<SendMessageResponse>, SyncError> {
    let db = state.db();
    let user_id = session.user_id;
    let account_bind = id_value(db, &body.account_id)?;
    let user_bind = id_value(db, &user_id)?;

    let mut probe = Sq::select();
    probe
        .columns([
            mail_account::Column::EmailAddress,
            mail_account::Column::SendProtocol,
            mail_account::Column::IsActive,
        ])
        .from(mail_account::Entity)
        .and_where(mail_account::Column::Id.eq(account_bind))
        .and_where(mail_account::Column::UserId.eq(user_bind));
    let row = db
        .orm()
        .query_one(&probe)
        .await
        .map_err(orm_err)?
        .ok_or(SyncError::AccountNotFound)?;

    let (is_active, send_protocol, email_address) = (
        row.try_get::<bool>("", "is_active").map_err(orm_err)?,
        row.try_get::<String>("", "send_protocol")
            .map_err(orm_err)?,
        row.try_get::<String>("", "email_address")
            .map_err(orm_err)?,
    );
    if !is_active {
        return Err(SyncError::AccountDisabled);
    }

    let plugin = resolve_send_plugin(state.app.as_ref(), &send_protocol)?;

    let to = parse_address_list(&body.to);
    let cc_values = body.cc.clone().unwrap_or_default();
    let bcc_values = body.bcc.clone().unwrap_or_default();
    let cc = parse_address_list(&cc_values);
    let bcc = parse_address_list(&bcc_values);

    if to.is_empty() {
        return Err(SyncError::InvalidInput("No recipients specified".into()));
    }

    // Snapshot OpenGPG inputs before consuming body fields (owned clones, no
    // borrows of `body` cross the partial moves below).
    let opengpg = body
        .opengpg
        .as_ref()
        .map(|opts| (opts.clone(), opengpg_recipient_emails(&body)));

    let mut body_text = body.body_text;
    let mut body_html = body.body_html;
    let mut mime_content_type = None;
    let mut mime_body = None;

    if let Some((opts, recipients)) = &opengpg
        && let Some(wrapped) = wrap_outbound_opengpg(
            &state,
            &user_id,
            &session.token,
            &body.account_id,
            opts,
            OutboundDraft {
                body_text: body_text.as_deref(),
                body_html: body_html.as_deref(),
                recipient_emails: recipients,
            },
        )
        .await
        .map_err(map_opengpg_send_error)?
    {
        mime_content_type = Some(wrapped.content_type);
        mime_body = Some(wrapped.body);
        body_text = None;
        body_html = None;
    }

    let outbound = OutboundMessage {
        from_email: email_address,
        from_name: None,
        to,
        cc,
        bcc,
        subject: body.subject,
        body_text,
        body_html,
        in_reply_to: body.in_reply_to,
        references: body.references,
        mime_content_type,
        mime_body,
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

fn map_opengpg_send_error(err: OpengpgError) -> SyncError {
    match err {
        OpengpgError::InvalidInput(msg) => SyncError::InvalidInput(msg),
        OpengpgError::NotFound => SyncError::InvalidInput("OpenGPG key not found".into()),
        other => SyncError::Crypto(other.to_string()),
    }
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

/// Discover a JMAP session and submit via EmailSubmission.
pub(crate) async fn deliver_jmap(
    jmap_base_url: &str,
    email: &str,
    password: &str,
    outbound: OutboundMessage,
) -> Result<String, SyncError> {
    let client = crate::jmap::JmapClient::discover(jmap_base_url, email, password).await?;
    Ok(client.submit_email(&outbound).await?)
}

/// Load JMAP settings for `account_id` and build an outbound message from raw source.
pub(crate) async fn prepare_jmap_send(
    db: &DbPool,
    account_id: &str,
    raw: &str,
) -> Result<(String, String, String, OutboundMessage), SyncError> {
    let mut probe = Sq::select();
    probe
        .columns([
            mail_account::Column::EmailAddress,
            mail_account::Column::UserId,
            mail_account::Column::JmapBaseUrl,
            mail_account::Column::IsActive,
        ])
        .from(mail_account::Entity)
        .and_where(mail_account::Column::Id.eq(id_value(db, account_id)?));
    let row = db
        .orm()
        .query_one(&probe)
        .await
        .map_err(orm_err)?
        .ok_or(SyncError::AccountNotFound)?;

    let (is_active, jmap_base_url, email_address, user_id) = (
        row.try_get::<bool>("", "is_active").map_err(orm_err)?,
        row.try_get::<Option<String>>("", "jmap_base_url")
            .map_err(orm_err)?,
        row.try_get::<String>("", "email_address")
            .map_err(orm_err)?,
        row_id(&row, "user_id")?,
    );
    if !is_active {
        return Err(SyncError::AccountDisabled);
    }

    let base_url = jmap_base_url
        .ok_or_else(|| SyncError::InvalidInput("JMAP base URL not configured".into()))?;

    let (dek, credential_json) =
        crate::auth::AuthState::get_user_dek_and_credential(db, &user_id, account_id)
            .await
            .map_err(|e| SyncError::Crypto(e.to_string()))?;
    let password = crate::jmap::decrypt_account_password(&credential_json, &dek)?;

    let outbound = outbound_from_raw(email_address.clone(), raw)?;
    Ok((base_url, email_address, password, outbound))
}

/// Load SMTP settings for `account_id` and build an outbound message from raw source.
///
/// `raw` may be JSON [`OutboundMessage`] (HTTP compose) or minimal RFC822-ish text.
pub(crate) async fn prepare_smtp_send(
    db: &DbPool,
    account_id: &str,
    raw: &str,
) -> Result<(SmtpConfig, OutboundMessage), SyncError> {
    let mut probe = Sq::select();
    probe
        .columns([
            mail_account::Column::EmailAddress,
            mail_account::Column::UserId,
            mail_account::Column::AuthType,
            mail_account::Column::SmtpHost,
            mail_account::Column::SmtpPort,
            mail_account::Column::SmtpSecurity,
            mail_account::Column::IsActive,
        ])
        .from(mail_account::Entity)
        .and_where(mail_account::Column::Id.eq(id_value(db, account_id)?));
    let row = db
        .orm()
        .query_one(&probe)
        .await
        .map_err(orm_err)?
        .ok_or(SyncError::AccountNotFound)?;

    let (is_active, smtp_host, smtp_port, smtp_security, email_address, auth_type, user_id) = (
        row.try_get::<bool>("", "is_active").map_err(orm_err)?,
        row.try_get::<Option<String>>("", "smtp_host")
            .map_err(orm_err)?,
        row.try_get::<Option<i32>>("", "smtp_port")
            .map_err(orm_err)?,
        row.try_get::<Option<String>>("", "smtp_security")
            .map_err(orm_err)?,
        row.try_get::<String>("", "email_address")
            .map_err(orm_err)?,
        row.try_get::<String>("", "auth_type").map_err(orm_err)?,
        row_id(&row, "user_id")?,
    );
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

    let (dek, credential_json) =
        crate::auth::AuthState::get_user_dek_and_credential(db, &user_id, account_id)
            .await
            .map_err(|e| SyncError::Crypto(e.to_string()))?;
    let oauth = crate::oauth::OAuthRegistry::refresh_configs();
    let secret = crate::oauth::resolve_mail_access_secret(
        db,
        account_id,
        &auth_type,
        &credential_json,
        &dek,
        Some(&host),
        &oauth,
    )
    .await
    .map_err(|e| {
        if e.is_credential_decrypt() {
            SyncError::Crypto(e.to_string())
        } else {
            SyncError::Protocol(e.to_string())
        }
    })?;

    let config = SmtpConfig {
        host,
        port,
        security,
        username: email_address.clone(),
        password: zeroize::Zeroizing::new(secret.as_str().to_string()),
        xoauth2: secret.is_xoauth2(),
    };

    let outbound = outbound_from_raw(email_address, raw)?;
    Ok((config, outbound))
}

pub(crate) fn outbound_from_raw(
    from_email: String,
    raw: &str,
) -> Result<OutboundMessage, SyncError> {
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
        mime_content_type: None,
        mime_body: None,
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
            return Some(crate::imap::decode_mime_header(value.trim()));
        }
    }
    None
}
