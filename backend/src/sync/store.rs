//! Folder/message persistence, cursors, and folder-batch transactions.
//!
//! Statements are built from SeaORM entity column enums so they cannot drift
//! from the schema. Ids bind dialect-aware (TEXT on SQLite / native UUID on
//! Postgres) and JSON/timestamp writes keep the shapes the legacy macro layer
//! produced (see [`id_value`] / [`opt_json_value`] / [`opt_date_value`]).
//!
//! Pool-level statements run straight on `db.orm()`; the `_in_tx` batch core
//! runs inside the legacy sqlx transaction ([`DbPool::begin`]) — every
//! rendered statement is bound parameter-by-parameter onto that open txn so
//! the messages + cursor commit stays atomic.

use uuid::Uuid;

use super::types::{SyncError, SyncResponse};
use crate::db_row::{IdParam, id_param, parse_ts};
use crate::entities::{folder, mail_account, message, sync_cursor};
use crate::imap::ImapMessage;
use crate::protocol::SyncOutcome;
use crate::sanitize::persist_body_html;
use crate::storage::{DbPool, DbTxn};

#[cfg(feature = "postgres")]
use sea_orm::sea_query::PostgresQueryBuilder;
use sea_orm::sea_query::{
    Alias, DeleteStatement, Expr, Func, InsertStatement, OnConflict, Query as Sq, SelectStatement,
    SqliteQueryBuilder, UpdateStatement,
};
use sea_orm::{ColumnTrait, ConnectionTrait, DbBackend, ExprTrait, QueryResult, Value};
use sqlx::Arguments as _;

/// Truncate subject/preview for snippet storage (char-safe for CJK, etc.).
pub(crate) fn truncate_for_snippet(s: &str) -> String {
    if s.chars().count() <= 120 {
        return s.to_string();
    }
    format!("{}...", s.chars().take(117).collect::<String>())
}

pub(crate) struct AccountSyncRow {
    pub(crate) email_address: String,
    pub(crate) auth_type: String,
    pub(crate) credential: String,
    pub(crate) imap_host: Option<String>,
    pub(crate) imap_port: Option<i32>,
    pub(crate) imap_security: Option<String>,
    pub(crate) jmap_base_url: Option<String>,
}

pub(crate) async fn load_account_sync_row(
    db: &DbPool,
    user_id: &str,
    account_id: &str,
) -> Result<AccountSyncRow, SyncError> {
    let mut sel = Sq::select();
    sel.columns([
        mail_account::Column::EmailAddress,
        mail_account::Column::AuthType,
        mail_account::Column::Credential,
        mail_account::Column::ImapHost,
        mail_account::Column::ImapPort,
        mail_account::Column::ImapSecurity,
        mail_account::Column::JmapBaseUrl,
    ])
    .from(mail_account::Entity)
    .and_where(mail_account::Column::Id.eq(id_value(db, account_id)?))
    .and_where(mail_account::Column::UserId.eq(id_value(db, user_id)?));

    let row = db
        .orm()
        .query_one(&sel)
        .await
        .map_err(orm_err)?
        .ok_or(SyncError::AccountNotFound)?;
    Ok(AccountSyncRow {
        email_address: row.try_get("", "email_address").map_err(orm_err)?,
        auth_type: row.try_get("", "auth_type").map_err(orm_err)?,
        credential: row.try_get("", "credential").map_err(orm_err)?,
        imap_host: row.try_get("", "imap_host").map_err(orm_err)?,
        imap_port: row.try_get("", "imap_port").map_err(orm_err)?,
        imap_security: row.try_get("", "imap_security").map_err(orm_err)?,
        jmap_base_url: row.try_get("", "jmap_base_url").map_err(orm_err)?,
    })
}

pub(crate) fn outcome_from_response(result: &SyncResponse) -> SyncOutcome {
    SyncOutcome {
        folders_synced: u32::try_from(result.folders_synced).unwrap_or(u32::MAX),
        messages_synced: u32::try_from(result.messages_synced).unwrap_or(u32::MAX),
    }
}

// ── SeaORM seam (dialect-aware binds + error recovery) ───────────────
//
// Entity PKs are `Uuid`, while rows carry TEXT ids on SQLite (tests use
// non-UUID ids there). Ids therefore bind as strings on SQLite and native
// UUIDs on Postgres, exactly like `db_row::id_param`.

/// Recover the underlying [`sqlx::Error`] from a SeaORM error so
/// `SyncError::Database` keeps reporting the driver failure.
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

/// Bind a UUID-column id: TEXT on SQLite, native `Uuid` on Postgres.
fn id_value(db: &DbPool, id: &str) -> Result<Value, SyncError> {
    Ok(match id_param(db, id)? {
        IdParam::Text(s) => Value::String(Some(s)),
        IdParam::Uuid(u) => Value::Uuid(Some(u)),
    })
}

/// Optional UUID-column id (e.g. folder `parent_id`).
fn opt_id_value(db: &DbPool, id: Option<&str>) -> Result<Value, SyncError> {
    let Some(id) = id else {
        return Ok(match db.backend() {
            DbBackend::Sqlite => Value::String(None),
            _ => Value::Uuid(None),
        });
    };
    id_value(db, id)
}

/// Optional plain-text bind.
fn opt_str_value(raw: Option<&str>) -> Value {
    Value::String(raw.map(str::to_owned))
}

/// JSON column bind mirroring `JsonParam::lenient`: raw text on SQLite,
/// parsed JSONB on Postgres (non-JSON text becomes a JSON string scalar).
fn opt_json_value(db: &DbPool, raw: Option<&str>) -> Value {
    let Some(raw) = raw else {
        return match db.backend() {
            DbBackend::Sqlite => Value::String(None),
            _ => Value::Json(None),
        };
    };
    match db.backend() {
        DbBackend::Sqlite => Value::String(Some(raw.to_owned())),
        _ => Value::Json(Some(Box::new(
            serde_json::from_str(raw).unwrap_or_else(|_| serde_json::Value::String(raw.to_owned())),
        ))),
    }
}

/// Message ingest timestamp mirroring `message_date_param`: parsed UTC stored
/// as `YYYY-MM-DD HH:MM:SS` text on SQLite (sortable against
/// `datetime('now')`), native UTC datetime on Postgres; absent or unparseable
/// input stores NULL rather than the raw string.
fn opt_date_value(db: &DbPool, raw: Option<&str>) -> Value {
    let Some(dt) = raw.and_then(parse_ts) else {
        return match db.backend() {
            DbBackend::Sqlite => Value::String(None),
            _ => Value::ChronoDateTimeUtc(None),
        };
    };
    match db.backend() {
        DbBackend::Sqlite => Value::String(Some(dt.format("%Y-%m-%d %H:%M:%S").to_string())),
        _ => Value::ChronoDateTimeUtc(Some(dt)),
    }
}

/// App-generated UUIDv7 id, stored as text.
fn new_uuid_text() -> String {
    Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string()
}

/// Decode a UUID/TEXT id column: `String` on SQLite, native UUID on Postgres.
fn row_id(row: &QueryResult, col: &str) -> Result<String, SyncError> {
    if let Some(text) = row.try_get::<Option<String>>("", col).ok().flatten() {
        return Ok(text);
    }
    let decoded = row
        .try_get::<Option<Uuid>>("", col)
        .map_err(orm_err)?
        .map(|u| u.to_string());
    decoded.ok_or_else(|| SyncError::Internal(format!("missing column {col}")))
}

// ── Transactional execution seam ─────────────────────────────────────
//
// The `_in_tx` core commits message batches AND sync cursors in ONE legacy
// sqlx transaction (`db.begin()`). SeaORM connections cannot adopt a foreign
// sqlx txn, so entity-built statements are rendered per-backend here and
// executed against the txn connection with parameterized binds.

/// SQL + ordered bind values for the transaction executor.
type RenderedTxSql = (String, Vec<Value>);

/// Statements runnable on the open [`DbTxn`].
trait TxSql {
    fn render_sqlite(&self) -> RenderedTxSql;
    #[cfg(feature = "postgres")]
    fn render_postgres(&self) -> RenderedTxSql;
}

macro_rules! tx_sql_render {
    ($ty:ty) => {
        impl TxSql for $ty {
            fn render_sqlite(&self) -> RenderedTxSql {
                let (sql, values) = <$ty>::build(self, SqliteQueryBuilder);
                (sql, values.0)
            }

            #[cfg(feature = "postgres")]
            fn render_postgres(&self) -> RenderedTxSql {
                let (sql, values) = <$ty>::build(self, PostgresQueryBuilder);
                (sql, values.0)
            }
        }
    };
}

tx_sql_render!(SelectStatement);
tx_sql_render!(InsertStatement);
tx_sql_render!(UpdateStatement);
tx_sql_render!(DeleteStatement);

type SqliteArgs = sqlx::sqlite::SqliteArguments;
#[cfg(feature = "postgres")]
type PgArgs = sqlx::postgres::PgArguments;

/// Add one rendered value to the SQLite argument list. Timestamps encode like
/// the legacy `TsParam`; ids/text stay TEXT (this schema stores TEXT ids).
///
/// Unsupported `Value` carriers are rejected explicitly rather than bound
/// loosely: only shapes this module constructs may reach a txn statement.
fn push_sqlite_arg(args: &mut SqliteArgs, value: Value) -> Result<(), sqlx::Error> {
    let added = match value {
        Value::Bool(v) => args.add(v),
        Value::Int(v) => args.add(v),
        Value::BigInt(v) => args.add(v),
        Value::String(v) => args.add(v),
        // SQLite ids are TEXT; never let sea_orm emit a UUID blob here.
        Value::Uuid(v) => args.add(v.map(|u| u.to_string())),
        Value::ChronoDateTimeUtc(v) => {
            args.add(v.map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string()))
        }
        other => {
            return Err(sqlx::Error::Protocol(format!(
                "unsupported sqlite txn bind: {other:?}"
            )));
        }
    };
    added.map_err(|e| sqlx::Error::Protocol(e.to_string()))
}

/// Add one rendered value to the Postgres argument list.
#[cfg(feature = "postgres")]
fn push_postgres_arg(args: &mut PgArgs, value: Value) -> Result<(), sqlx::Error> {
    let added = match value {
        Value::Bool(v) => args.add(v),
        Value::Int(v) => args.add(v),
        Value::BigInt(v) => args.add(v),
        Value::String(v) => args.add(v),
        Value::Uuid(v) => args.add(v),
        Value::ChronoDateTimeUtc(v) => args.add(v),
        Value::Json(v) => args.add(v.map(|boxed| *boxed)),
        other => {
            return Err(sqlx::Error::Protocol(format!(
                "unsupported postgres txn bind: {other:?}"
            )));
        }
    };
    added.map_err(|e| sqlx::Error::Protocol(e.to_string()))
}

/// Collect rendered values into driver arguments; every user value reaches the
/// database as a bind parameter, matching the `db_sql` macro audit rules.
fn sqlite_args(values: Vec<Value>) -> Result<SqliteArgs, sqlx::Error> {
    let mut args = SqliteArgs::default();
    for value in values {
        push_sqlite_arg(&mut args, value)?;
    }
    Ok(args)
}

#[cfg(feature = "postgres")]
fn postgres_args(values: Vec<Value>) -> Result<PgArgs, sqlx::Error> {
    let mut args = PgArgs::default();
    for value in values {
        push_postgres_arg(&mut args, value)?;
    }
    Ok(args)
}

/// Execute an entity-built statement on the open sqlx transaction.
///
/// The statement text is produced solely by sea_query from entity column
/// enums (constant identifiers/patterns); every variable rides through
/// [`sqlite_args`] / [`postgres_args`] — hence `AssertSqlSafe`.
async fn tx_execute<S: TxSql>(tx: &mut DbTxn, stmt: &S) -> Result<(), SyncError> {
    match tx {
        DbTxn::Sqlite(t) => {
            let (sql, values) = stmt.render_sqlite();
            let query = sqlx::query_with(sqlx::AssertSqlSafe(sql), sqlite_args(values)?);
            query.execute(&mut **t).await?;
        }
        #[cfg(feature = "postgres")]
        DbTxn::Postgres(t) => {
            let (sql, values) = stmt.render_postgres();
            let query = sqlx::query_with(sqlx::AssertSqlSafe(sql), postgres_args(values)?);
            query.execute(&mut **t).await?;
        }
    }
    Ok(())
}

/// Fetch a single UUID/TEXT `id` column on the open sqlx transaction.
async fn tx_fetch_id<S: TxSql>(tx: &mut DbTxn, stmt: &S) -> Result<Option<String>, SyncError> {
    Ok(match tx {
        DbTxn::Sqlite(t) => {
            let (sql, values) = stmt.render_sqlite();
            sqlx::query_scalar_with::<_, String, _>(sqlx::AssertSqlSafe(sql), sqlite_args(values)?)
                .fetch_optional(&mut **t)
                .await?
        }
        #[cfg(feature = "postgres")]
        DbTxn::Postgres(t) => {
            let (sql, values) = stmt.render_postgres();
            sqlx::query_scalar_with::<_, Uuid, _>(sqlx::AssertSqlSafe(sql), postgres_args(values)?)
                .fetch_optional(&mut **t)
                .await?
                .map(|u| u.to_string())
        }
    })
}

/// Fetch a single aggregate integer on the open sqlx transaction.
async fn tx_fetch_count<S: TxSql>(tx: &mut DbTxn, stmt: &S) -> Result<i64, SyncError> {
    match tx {
        DbTxn::Sqlite(t) => {
            let (sql, values) = stmt.render_sqlite();
            Ok(
                sqlx::query_scalar_with::<_, i64, _>(
                    sqlx::AssertSqlSafe(sql),
                    sqlite_args(values)?,
                )
                .fetch_one(&mut **t)
                .await?,
            )
        }
        #[cfg(feature = "postgres")]
        DbTxn::Postgres(t) => {
            let (sql, values) = stmt.render_postgres();
            Ok(sqlx::query_scalar_with::<_, i64, _>(
                sqlx::AssertSqlSafe(sql),
                postgres_args(values)?,
            )
            .fetch_one(&mut **t)
            .await?)
        }
    }
}

// ── Database helpers ────────────────────────────────────────────────

/// Split an IMAP wire mailbox path into `(parent_wire, leaf_wire)`.
pub(crate) fn split_imap_folder_path<'a>(
    wire_name: &'a str,
    delimiter: Option<&str>,
) -> (Option<&'a str>, &'a str) {
    let Some(delim) = delimiter.filter(|d| !d.is_empty()) else {
        return (None, wire_name);
    };
    wire_name
        .rsplit_once(delim)
        .map_or((None, wire_name), |(parent, leaf)| {
            if parent.is_empty() {
                (None, leaf)
            } else {
                (Some(parent), leaf)
            }
        })
}

/// Leaf display name for an IMAP mailbox (Modified UTF-7 decoded).
pub(crate) fn imap_folder_display_name(wire_name: &str, delimiter: Option<&str>) -> String {
    let (_, leaf) = split_imap_folder_path(wire_name, delimiter);
    crate::imap::decode_imap_mailbox_name(leaf)
}

/// Depth of an IMAP mailbox path (0 = top-level). Used to upsert parents first.
pub(crate) fn imap_folder_depth(wire_name: &str, delimiter: Option<&str>) -> usize {
    delimiter
        .filter(|d| !d.is_empty())
        .map_or(0, |d| wire_name.matches(d).count())
}

/// Local folder id lookup by `(account_id, external_id)` — shared by the
/// parent resolution, upsert existence checks, and id wiring.
async fn find_folder_id_by_external_id(
    db: &DbPool,
    account_id: &str,
    external_id: &str,
) -> Result<Option<String>, SyncError> {
    let mut sel = Sq::select();
    sel.column(folder::Column::Id)
        .from(folder::Entity)
        .and_where(folder::Column::AccountId.eq(id_value(db, account_id)?))
        .and_where(folder::Column::ExternalId.eq(external_id));
    let row = db.orm().query_one(&sel).await.map_err(orm_err)?;
    row.map(|r| row_id(&r, "id")).transpose()
}

/// Upsert an IMAP folder by `(account_id, wire_name)`.
///
/// `wire_name` is the encoded IMAP mailbox name (Modified UTF-7 on the wire).
/// It is stored in `external_id` for SELECT/LIST; `name` is the decoded leaf
/// segment; `parent_id` links to the parent mailbox when a delimiter is present.
pub(crate) async fn upsert_folder(
    db: &DbPool,
    account_id: &str,
    wire_name: &str,
    delimiter: Option<&str>,
    attributes: &[String],
) -> Result<(), SyncError> {
    let display_name = imap_folder_display_name(wire_name, delimiter);
    let role = special_use_role(attributes).or_else(|| infer_folder_role(&display_name));
    let external_id = wire_name;
    let account_bind = id_value(db, account_id)?;
    let (parent_wire, _) = split_imap_folder_path(wire_name, delimiter);
    let parent_id = if let Some(parent_wire) = parent_wire {
        find_folder_id_by_external_id(db, account_id, parent_wire).await?
    } else {
        None
    };
    let parent_bind = opt_id_value(db, parent_id.as_deref())?;

    // Try to find existing folder
    let existing = find_folder_id_by_external_id(db, account_id, external_id).await?;

    if let Some(id) = existing {
        // Preserve role_override: only refresh detected role / name / parent.
        let mut upd = Sq::update();
        upd.table(folder::Entity)
            .value(folder::Column::Name, Expr::val(display_name))
            .value(folder::Column::Role, Expr::val(role))
            .value(folder::Column::ParentId, Expr::val(parent_bind))
            .value(folder::Column::UpdatedAt, Expr::current_timestamp())
            .and_where(folder::Column::Id.eq(id_value(db, &id)?));
        db.orm().execute(&upd).await.map_err(orm_err)?;
    } else {
        let id = new_uuid_text();
        let mut ins = Sq::insert();
        ins.into_table(folder::Entity)
            .columns([
                folder::Column::Id,
                folder::Column::AccountId,
                folder::Column::ExternalId,
                folder::Column::Name,
                folder::Column::Role,
                folder::Column::ParentId,
                folder::Column::SortOrder,
            ])
            .values_panic([
                Expr::val(id_value(db, &id)?),
                Expr::val(account_bind),
                Expr::val(external_id),
                Expr::val(display_name),
                Expr::val(role),
                Expr::val(parent_bind),
                Expr::val(0_i32),
            ]);
        db.orm().execute(&ins).await.map_err(orm_err)?;
    }

    Ok(())
}

/// Effective folder role: local override wins over SPECIAL-USE / name inference.
#[must_use]
pub(crate) fn effective_folder_role(
    role: Option<&str>,
    role_override: Option<&str>,
) -> Option<String> {
    role_override
        .filter(|s| !s.is_empty())
        .or(role)
        .map(str::to_owned)
}

/// Upsert a JMAP mailbox. Uses mailbox `id` as `external_id`.
pub(crate) async fn upsert_jmap_folder(
    db: &DbPool,
    account_id: &str,
    mailbox: &crate::sync::jmap_client::JmapMailbox,
) -> Result<(), SyncError> {
    let role = mailbox
        .role
        .as_deref()
        .or_else(|| infer_folder_role(&mailbox.name));
    let external_id = mailbox.id.as_str();

    let existing = find_folder_id_by_external_id(db, account_id, external_id).await?;

    if let Some(id) = existing {
        let mut upd = Sq::update();
        upd.table(folder::Entity)
            .value(folder::Column::Name, Expr::val(mailbox.name.as_str()))
            .value(folder::Column::Role, Expr::val(role))
            .value(folder::Column::UpdatedAt, Expr::current_timestamp())
            .and_where(folder::Column::Id.eq(id_value(db, &id)?));
        db.orm().execute(&upd).await.map_err(orm_err)?;
    } else {
        let id = new_uuid_text();
        let mut ins = Sq::insert();
        ins.into_table(folder::Entity)
            .columns([
                folder::Column::Id,
                folder::Column::AccountId,
                folder::Column::ExternalId,
                folder::Column::Name,
                folder::Column::Role,
                folder::Column::SortOrder,
            ])
            .values_panic([
                Expr::val(id_value(db, &id)?),
                Expr::val(id_value(db, account_id)?),
                Expr::val(external_id),
                Expr::val(mailbox.name.as_str()),
                Expr::val(role),
                Expr::val(0_i32),
            ]);
        db.orm().execute(&ins).await.map_err(orm_err)?;
    }

    Ok(())
}

/// Wire JMAP `parentId` after all mailboxes exist locally.
pub(crate) async fn link_jmap_folder_parent(
    db: &DbPool,
    account_id: &str,
    child_external_id: &str,
    parent_external_id: &str,
) -> Result<(), SyncError> {
    let child_id = get_folder_id(db, account_id, child_external_id).await?;
    let parent_id = get_folder_id(db, account_id, parent_external_id).await?;
    let mut upd = Sq::update();
    upd.table(folder::Entity)
        .value(
            folder::Column::ParentId,
            Expr::val(id_value(db, &parent_id)?),
        )
        .value(folder::Column::UpdatedAt, Expr::current_timestamp())
        .and_where(folder::Column::Id.eq(id_value(db, &child_id)?));
    db.orm().execute(&upd).await.map_err(orm_err)?;
    Ok(())
}

/// Get the local folder ID by `external_id`.
pub(crate) async fn get_folder_id(
    db: &DbPool,
    account_id: &str,
    name: &str,
) -> Result<String, SyncError> {
    find_folder_id_by_external_id(db, account_id, name)
        .await?
        .ok_or_else(|| SyncError::Database(sqlx::Error::RowNotFound))
}

/// RFC 6154 SPECIAL-USE attribute → folder role. Takes precedence over name
/// matching: servers that send `\Archive` / `\Junk` / etc. know better than
/// our name guesses. Attributes arrive as async-imap `Attribute` Debug strings
/// (`Archive`, `Custom("\\Junk")`), so match on the trailing alphanumeric
/// token. `\All` folds into `archive` (Gmail "All Mail"); `\Flagged` has no
/// folder role here (flagged is a smart view).
pub(crate) fn special_use_role(attributes: &[String]) -> Option<&'static str> {
    for attr in attributes {
        let token: String = attr
            .chars()
            .filter(|c| c.is_alphanumeric())
            .collect::<String>()
            .to_lowercase();
        for (suffix, role) in [
            ("archive", "archive"),
            ("all", "archive"),
            ("drafts", "drafts"),
            ("junk", "spam"),
            ("sent", "sent"),
            ("trash", "trash"),
        ] {
            if token.ends_with(suffix) {
                return Some(role);
            }
        }
    }
    None
}

/// Infer folder role from name.
pub(crate) fn infer_folder_role(name: &str) -> Option<&'static str> {
    let lower = name.to_lowercase();
    if lower == "inbox" {
        Some("inbox")
    } else if lower == "sent" || lower == "sent mail" || lower == "sent messages" {
        Some("sent")
    } else if lower == "drafts" || lower == "draft" {
        Some("drafts")
    } else if lower == "trash" || lower == "deleted" || lower == "deleted messages" {
        Some("trash")
    } else if lower == "spam" || lower == "junk" || lower == "bulk mail" {
        Some("spam")
    } else if lower == "archive" || lower == "all mail" || lower == "all messages" {
        Some("archive")
    } else {
        None
    }
}

// ── Sync cursor ─────────────────────────────────────────────────────

/// Stored sync cursor for a folder.
pub(crate) struct SyncCursorInfo {
    pub(crate) uid_validity: u32,
    pub(crate) last_uid: u32,
    /// RFC 7162 HIGHESTMODSEQ at last successful sync (0 when unknown / no CONDSTORE).
    pub(crate) last_modseq: u64,
}

/// Load a raw cursor string for `(account, folder, cursor_type)`.
///
/// Cursor strings round-trip verbatim (`{uidvalidity}:{uid}` plus optional
/// `:{modseq}`, or opaque JMAP `queryState` tokens).
async fn load_cursor_value(
    db: &DbPool,
    account_id: &str,
    folder_id: &str,
    cursor_type: &str,
) -> Result<Option<String>, SyncError> {
    let mut sel = Sq::select();
    sel.column(sync_cursor::Column::CursorValue)
        .from(sync_cursor::Entity)
        .and_where(sync_cursor::Column::AccountId.eq(id_value(db, account_id)?))
        .and_where(sync_cursor::Column::FolderId.eq(id_value(db, folder_id)?))
        .and_where(sync_cursor::Column::CursorType.eq(cursor_type));
    let row = db.orm().query_one(&sel).await.map_err(orm_err)?;
    row.map(|r| r.try_get::<String>("", "cursor_value").map_err(orm_err))
        .transpose()
}

/// Load the sync cursor for a folder.
pub(crate) async fn load_cursor(
    db: &DbPool,
    account_id: &str,
    folder_id: &str,
) -> Result<Option<SyncCursorInfo>, SyncError> {
    let value = load_cursor_value(db, account_id, folder_id, "uidvalidity_uid").await?;
    Ok(value.as_deref().map(parse_cursor_value))
}

/// Save the sync cursor for a folder.
///
/// Cursor format: `{uid_validity}:{last_uid}` for the `uidvalidity_uid` type.
#[cfg(test)]
pub(crate) async fn save_cursor(
    db: &DbPool,
    account_id: &str,
    folder_id: &str,
    protocol: &str,
    uid_validity: u32,
    last_uid: u32,
    last_modseq: u64,
) -> Result<(), SyncError> {
    let mut tx = db.begin().await?;
    save_cursor_in_tx(
        &mut tx,
        db,
        account_id,
        folder_id,
        protocol,
        uid_validity,
        last_uid,
        last_modseq,
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Parse a cursor value string `{uid_validity}:{last_uid}` or `{uid_validity}:{last_uid}:{modseq}`.
pub(crate) fn parse_cursor_value(value: &str) -> SyncCursorInfo {
    let parts: Vec<&str> = value.split(':').collect();
    match parts.as_slice() {
        [uv, lu, ms] => SyncCursorInfo {
            uid_validity: uv.parse().unwrap_or(0),
            last_uid: lu.parse().unwrap_or(0),
            last_modseq: ms.parse().unwrap_or(0),
        },
        [uv, lu] => SyncCursorInfo {
            uid_validity: uv.parse().unwrap_or(0),
            last_uid: lu.parse().unwrap_or(0),
            last_modseq: 0,
        },
        _ => SyncCursorInfo {
            uid_validity: 0,
            last_uid: 0,
            last_modseq: 0,
        },
    }
}

pub(crate) fn format_cursor_value(uid_validity: u32, last_uid: u32, last_modseq: u64) -> String {
    if last_modseq > 0 {
        format!("{uid_validity}:{last_uid}:{last_modseq}")
    } else {
        format!("{uid_validity}:{last_uid}")
    }
}

/// Load the stored JMAP `queryState` token for a folder.
///
/// Returns the raw token to be sent verbatim as `sinceQueryState` on the next sync.
pub(crate) async fn load_jmap_cursor(
    db: &DbPool,
    account_id: &str,
    folder_id: &str,
) -> Result<Option<String>, SyncError> {
    load_cursor_value(db, account_id, folder_id, "state_token").await
}

/// Save the JMAP `queryState` token for a folder.
///
/// The token is an opaque server string and is stored verbatim — never hashed —
/// so it can be sent back as `sinceQueryState` on the next sync.
#[cfg(test)]
pub(crate) async fn save_jmap_cursor(
    db: &DbPool,
    account_id: &str,
    folder_id: &str,
    query_state: &str,
) -> Result<(), SyncError> {
    let mut tx = db.begin().await?;
    save_jmap_cursor_in_tx(&mut tx, db, account_id, folder_id, query_state).await?;
    tx.commit().await?;
    Ok(())
}

pub(crate) async fn clear_jmap_cursor(
    db: &DbPool,
    account_id: &str,
    folder_id: &str,
) -> Result<(), SyncError> {
    clear_state_cursor(db, account_id, folder_id, "state_token").await
}

async fn clear_state_cursor(
    db: &DbPool,
    account_id: &str,
    folder_id: &str,
    cursor_type: &str,
) -> Result<(), SyncError> {
    let mut del = Sq::delete();
    del.from_table(sync_cursor::Entity)
        .and_where(sync_cursor::Column::AccountId.eq(id_value(db, account_id)?))
        .and_where(sync_cursor::Column::FolderId.eq(id_value(db, folder_id)?))
        .and_where(sync_cursor::Column::CursorType.eq(cursor_type));
    db.orm().execute(&del).await.map_err(orm_err)?;
    Ok(())
}

/// Load the account-level `Email/changes` state.
///
/// `Email/changes` state is account-scoped, but `sync_cursor.folder_id` is
/// NOT NULL with a folder FK — the row anchors on the account's inbox folder.
pub(crate) async fn load_jmap_email_state(
    db: &DbPool,
    account_id: &str,
    anchor_folder_id: &str,
) -> Result<Option<String>, SyncError> {
    load_cursor_value(db, account_id, anchor_folder_id, "email_state").await
}

pub(crate) async fn save_jmap_email_state(
    db: &DbPool,
    account_id: &str,
    anchor_folder_id: &str,
    state: &str,
) -> Result<(), SyncError> {
    let mut tx = db.begin().await?;
    save_state_cursor_in_tx(
        &mut tx,
        db,
        account_id,
        anchor_folder_id,
        "email_state",
        state,
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

pub(crate) async fn clear_jmap_email_state(
    db: &DbPool,
    account_id: &str,
    anchor_folder_id: &str,
) -> Result<(), SyncError> {
    clear_state_cursor(db, account_id, anchor_folder_id, "email_state").await
}

/// Find the local folder id for a JMAP mailbox id (`external_id`), if synced.
pub(crate) async fn find_folder_id(
    db: &DbPool,
    account_id: &str,
    external_id: &str,
) -> Result<Option<String>, SyncError> {
    find_folder_id_by_external_id(db, account_id, external_id).await
}

/// The account's folder with `role` (local `role_override` wins), if any.
pub(crate) async fn folder_id_for_role(
    db: &DbPool,
    account_id: &str,
    role: &str,
) -> Result<Option<String>, SyncError> {
    let mut sel = Sq::select();
    sel.column(folder::Column::Id)
        .from(folder::Entity)
        .and_where(folder::Column::AccountId.eq(id_value(db, account_id)?))
        .and_where(Expr::cust("COALESCE(role_override, role)").eq(Expr::val(role)))
        .order_by_expr(
            Expr::col(folder::Column::SortOrder),
            sea_orm::sea_query::Order::Asc,
        )
        .limit(1);
    let row = db.orm().query_one(&sel).await.map_err(orm_err)?;
    row.map(|r| row_id(&r, "id")).transpose()
}

/// The account's first folder by display order — fallback anchor for the
/// account-level JMAP `email_state` cursor when no inbox-role folder exists
/// (the `sync_cursor.folder_id` FK just needs *some* folder of the account).
pub(crate) async fn first_folder_id(
    db: &DbPool,
    account_id: &str,
) -> Result<Option<String>, SyncError> {
    let mut sel = Sq::select();
    sel.column(folder::Column::Id)
        .from(folder::Entity)
        .and_where(folder::Column::AccountId.eq(id_value(db, account_id)?))
        .order_by_expr(
            Expr::col(folder::Column::SortOrder),
            sea_orm::sea_query::Order::Asc,
        )
        .order_by_expr(
            Expr::col(folder::Column::CreatedAt),
            sea_orm::sea_query::Order::Asc,
        )
        .limit(1);
    let row = db.orm().query_one(&sel).await.map_err(orm_err)?;
    row.map(|r| row_id(&r, "id")).transpose()
}

// ── Message upsert ──────────────────────────────────────────────────
//
// Full-text index rows are maintained by migration 0009 triggers on `message`
// (FTS5 on SQLite, `search_vector` on PostgreSQL). Upserts and deletes here
// automatically refresh the search index — no separate hook required.

/// Upsert a message from IMAP metadata.
///
/// Returns `true` if the message was newly inserted, `false` if updated.
#[cfg(test)]
pub(crate) async fn upsert_message(
    db: &DbPool,
    account_id: &str,
    folder_id: &str,
    msg: &ImapMessage,
) -> Result<bool, SyncError> {
    let mut tx = db.begin().await?;
    let was_new = upsert_message_in_tx(&mut tx, db, account_id, folder_id, msg).await?;
    tx.commit().await?;
    Ok(was_new)
}

/// Delete all messages in a folder (used when UIDVALIDITY changes).
#[cfg(test)]
pub(crate) async fn clear_folder_messages(db: &DbPool, folder_id: &str) -> Result<(), SyncError> {
    let mut tx = db.begin().await?;
    clear_folder_messages_in_tx(&mut tx, db, folder_id).await?;
    tx.commit().await?;
    Ok(())
}

/// Update folder message counts from the message table.
pub(crate) async fn update_folder_counts(db: &DbPool, folder_id: &str) -> Result<(), SyncError> {
    let mut tx = db.begin().await?;
    update_folder_counts_in_tx(&mut tx, db, folder_id).await?;
    tx.commit().await?;
    Ok(())
}

/// Persist one IMAP folder page: optional wipe, upserts, cursor, counts — one transaction.
///
/// Returns `(messages_new, messages_updated)`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn persist_imap_folder_batch(
    db: &DbPool,
    account_id: &str,
    folder_id: &str,
    messages: &[ImapMessage],
    uid_validity: u32,
    last_uid: u32,
    last_modseq: u64,
    clear_first: bool,
) -> Result<(usize, usize), SyncError> {
    let mut tx = db.begin().await?;
    if clear_first {
        clear_folder_messages_in_tx(&mut tx, db, folder_id).await?;
    }
    let mut new = 0usize;
    let mut updated = 0usize;
    for msg in messages {
        if upsert_message_in_tx(&mut tx, db, account_id, folder_id, msg).await? {
            new += 1;
        } else {
            updated += 1;
        }
    }
    save_cursor_in_tx(
        &mut tx,
        db,
        account_id,
        folder_id,
        "imap",
        uid_validity,
        last_uid,
        last_modseq,
    )
    .await?;
    update_folder_counts_in_tx(&mut tx, db, folder_id).await?;
    tx.commit().await?;
    Ok((new, updated))
}

/// Text-cast projection of a message column — address columns are JSONB on
/// PostgreSQL and cannot feed text operators (`LIKE`, `=`) directly.
fn as_text_col(col: message::Column) -> Expr {
    // Table-qualified so the expression also embeds cleanly in
    // `ON CONFLICT DO UPDATE` bodies, where bare refs are ambiguous on
    // PostgreSQL.
    Expr::col((message::Entity, col)).cast_as(Alias::new("text"))
}

/// IMAP UIDs of up to `limit` messages in a folder whose stored subject,
/// snippet, addresses, or body still carries U+FFFD mojibake — rows synced
/// before full legacy-charset decoding (mail-parser `full_encoding`) was
/// enabled. The sync-loop self-heal pass re-fetches these envelopes.
pub(crate) async fn mojibake_message_uids(
    db: &DbPool,
    folder_id: &str,
    limit: u64,
) -> Result<Vec<u32>, SyncError> {
    let mut sel = Sq::select();
    sel.column(message::Column::ExternalId)
        .from(message::Entity)
        .and_where(message::Column::FolderId.eq(id_value(db, folder_id)?))
        .and_where(
            as_text_col(message::Column::Subject)
                .like("%\u{FFFD}%")
                .or(as_text_col(message::Column::Snippet).like("%\u{FFFD}%"))
                .or(as_text_col(message::Column::FromAddress).like("%\u{FFFD}%"))
                .or(as_text_col(message::Column::ToAddresses).like("%\u{FFFD}%"))
                .or(as_text_col(message::Column::BodyText).like("%\u{FFFD}%"))
                .or(as_text_col(message::Column::BodyHtml).like("%\u{FFFD}%")),
        )
        .limit(limit);
    let rows = db.orm().query_all(&sel).await.map_err(orm_err)?;
    let mut uids = Vec::with_capacity(rows.len());
    for row in &rows {
        let external_id: Option<String> = row.try_get("", "external_id").map_err(orm_err)?;
        if let Ok(uid) = parse_imap_uid(external_id.as_deref()) {
            uids.push(uid);
        }
    }
    Ok(uids)
}

/// Upsert re-fetched envelopes for self-heal — no cursor movement — and clear
/// any remaining mojibake bodies in the folder so the next open lazily
/// re-fetches and re-parses them with full charset decoding.
///
/// Subject/snippet repair rides the normal upsert conflict path: `stale_text`
/// treats U+FFFD text as stale, so the freshly decoded envelope replaces it.
pub(crate) async fn repair_imap_messages(
    db: &DbPool,
    account_id: &str,
    folder_id: &str,
    messages: &[ImapMessage],
) -> Result<usize, SyncError> {
    let mut tx = db.begin().await?;
    let mut repaired = 0usize;
    for msg in messages {
        upsert_message_in_tx(&mut tx, db, account_id, folder_id, msg).await?;
        repaired += 1;
    }
    let mut clear = Sq::update();
    clear
        .table(message::Entity)
        .value(message::Column::BodyText, opt_str_value(None))
        .value(message::Column::BodyHtml, opt_str_value(None))
        .and_where(message::Column::FolderId.eq(id_value(db, folder_id)?))
        .and_where(
            message::Column::BodyText
                .like("%\u{FFFD}%")
                .or(message::Column::BodyHtml.like("%\u{FFFD}%")),
        );
    tx_execute(&mut tx, &clear).await?;
    update_folder_counts_in_tx(&mut tx, db, folder_id).await?;
    tx.commit().await?;
    Ok(repaired)
}

/// Local message rows in a folder whose IMAP UID no longer exists on the
/// server (moved or deleted between syncs) — the "ghost rows" that render as
/// permanently empty bodies in the reader. Rows with a NULL or unparseable
/// `external_id` (e.g. local drafts not yet uploaded) are never reported.
pub(crate) fn stale_imap_row_ids(
    local_rows: &[(String, Option<String>)],
    server_uids: &std::collections::HashSet<u32>,
) -> Vec<String> {
    local_rows
        .iter()
        .filter_map(|(id, external_id)| {
            let uid = parse_imap_uid(external_id.as_deref()).ok()?;
            (!server_uids.contains(&uid)).then(|| id.clone())
        })
        .collect()
}

/// Soft-delete local rows whose IMAP UIDs vanished server-side, scoped to one
/// folder. The caller passes a *complete* current UID listing (`UID SEARCH
/// 1:*`); a failed listing must never reach this function, or live mail would
/// be wiped. Returns the number of rows soft-deleted.
pub(crate) async fn reconcile_folder_deletions(
    db: &DbPool,
    account_id: &str,
    folder_id: &str,
    server_uids: &std::collections::HashSet<u32>,
) -> Result<usize, SyncError> {
    let mut sel = Sq::select();
    sel.expr_as(
        // Read the id as text: the column is UUID on PostgreSQL, where
        // decoding it as String (like the bind fix, 19b9141) type-errors.
        Expr::col((message::Entity, message::Column::Id)).cast_as(Alias::new("text")),
        Alias::new("id"),
    )
    .column(message::Column::ExternalId)
    .from(message::Entity)
    .and_where(message::Column::AccountId.eq(id_value(db, account_id)?))
    .and_where(message::Column::FolderId.eq(id_value(db, folder_id)?))
    .and_where(message::Column::IsDeleted.eq(false));
    let rows = db.orm().query_all(&sel).await.map_err(orm_err)?;
    let mut local_rows = Vec::with_capacity(rows.len());
    for row in &rows {
        let id: String = row.try_get("", "id").map_err(orm_err)?;
        let external_id: Option<String> = row.try_get("", "external_id").map_err(orm_err)?;
        local_rows.push((id, external_id));
    }

    let stale = stale_imap_row_ids(&local_rows, server_uids);
    if stale.is_empty() {
        return Ok(0);
    }

    let id_values = stale
        .iter()
        .map(|id| id_value(db, id))
        .collect::<Result<Vec<_>, _>>()?;
    let mut tx = db.begin().await?;
    let mut upd = Sq::update();
    upd.table(message::Entity)
        .value(message::Column::IsDeleted, true)
        .and_where(message::Column::Id.is_in(id_values));
    tx_execute(&mut tx, &upd).await?;
    update_folder_counts_in_tx(&mut tx, db, folder_id).await?;
    tx.commit().await?;
    Ok(stale.len())
}

/// One JMAP message persisted this batch (same order as the input slice).
#[derive(Debug)]
pub(crate) struct JmapPersistedMessage {
    pub(crate) local_id: String,
    pub(crate) was_new: bool,
}

/// Persist one JMAP mailbox page: upserts, cursor, counts — one transaction.
/// Returns `(new, updated, per-message results in input order)`.
pub(crate) async fn persist_jmap_folder_batch(
    db: &DbPool,
    account_id: &str,
    folder_id: &str,
    emails: &[crate::sync::jmap_client::JmapEmail],
    query_state: Option<&str>,
) -> Result<(usize, usize, Vec<JmapPersistedMessage>), SyncError> {
    let mut tx = db.begin().await?;
    let mut new = 0usize;
    let mut updated = 0usize;
    let mut persisted = Vec::with_capacity(emails.len());
    for email in emails {
        let (was_new, local_id) =
            upsert_jmap_message_in_tx(&mut tx, db, account_id, folder_id, email).await?;
        if was_new {
            new += 1;
        } else {
            updated += 1;
        }
        persisted.push(JmapPersistedMessage { local_id, was_new });
    }
    if let Some(qs) = query_state {
        save_jmap_cursor_in_tx(&mut tx, db, account_id, folder_id, qs).await?;
    }
    update_folder_counts_in_tx(&mut tx, db, folder_id).await?;
    tx.commit().await?;
    Ok((new, updated, persisted))
}

/// Hard-delete JMAP messages by server ids (`external_id`), scoped to the
/// account. Server-gone and moved-out messages both land here; the caller
/// has already subtracted anything re-fetched this run. Attachment rows and
/// FTS entries cascade. Folder counts of affected folders are refreshed.
pub(crate) async fn delete_jmap_messages_by_external_ids(
    db: &DbPool,
    account_id: &str,
    external_ids: &[String],
) -> Result<usize, SyncError> {
    // Chunk the IN (...) binds so huge removal sets stay under SQLite's
    // variable cap (999 on older builds, 32766 on modern ones).
    const IDS_CHUNK: usize = 1000;

    if external_ids.is_empty() {
        return Ok(0);
    }
    let account_bind = id_value(db, account_id)?;

    // Folders whose counts change (collected before the delete; the delete +
    // count refresh are deliberately non-transactional, as persist_attachments).
    let mut folder_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut deleted = 0usize;
    for chunk in external_ids.chunks(IDS_CHUNK) {
        let mut sel = Sq::select();
        sel.distinct()
            .column(message::Column::FolderId)
            .from(message::Entity)
            .and_where(message::Column::AccountId.eq(account_bind.clone()))
            .and_where(message::Column::ExternalId.is_in(chunk.iter().cloned()));
        let rows = db.orm().query_all(&sel).await.map_err(orm_err)?;
        for row in &rows {
            folder_ids.insert(row_id(row, "folder_id")?);
        }

        let mut del = Sq::delete();
        del.from_table(message::Entity)
            .and_where(message::Column::AccountId.eq(account_bind.clone()))
            .and_where(message::Column::ExternalId.is_in(chunk.iter().cloned()));
        let res = db.orm().execute(&del).await.map_err(orm_err)?;
        deleted =
            deleted.saturating_add(usize::try_from(res.rows_affected()).unwrap_or(usize::MAX));
    }

    for folder_id in &folder_ids {
        update_folder_counts(db, folder_id).await?;
    }
    Ok(deleted)
}

/// Wire-format for IMAP message identity.
///
/// RFC 3501 UIDs are unique only within a mailbox (paired with UIDVALIDITY),
/// so the folder must scope the stored id — bare UIDs collide across folders
/// and silently absorb each other on the `UNIQUE(account_id, external_id)`
/// upsert. The folder's internal id (not its wire name) is used so renames
/// don't re-key history.
pub(crate) fn imap_message_external_id(folder_id: &str, uid: u32) -> String {
    format!("{folder_id}:{uid}")
}

/// Recover the IMAP UID from a stored message `external_id`.
///
/// Accepts bare numeric ids as a fallback for rows written before ids became
/// folder-scoped.
pub(crate) fn parse_imap_uid(external_id: Option<&str>) -> Result<u32, SyncError> {
    let raw =
        external_id.ok_or_else(|| SyncError::InvalidInput("message has no IMAP UID".into()))?;
    let uid_part = raw.rsplit(':').next().unwrap_or(raw);
    uid_part
        .parse::<u32>()
        .map_err(|_| SyncError::InvalidInput("message has no IMAP UID".into()))
}

/// Local row within `ON CONFLICT … DO UPDATE SET`. Column refs there MUST be
/// table-qualified on PostgreSQL — a bare `"col"` is ambiguous against
/// `excluded."col"` — while SQLite resolves the qualified form to the current
/// row just as well.
fn cur_row_col(col: message::Column) -> Expr {
    Expr::col((message::Entity, col))
}

/// Incoming candidate row within `DO UPDATE SET` (`excluded."col"`).
fn excluded_col(col: message::Column) -> Expr {
    Expr::col((Alias::new("excluded"), col))
}

/// Refresh-only-if-stale guard used for subject/snippet fill-in: replace the
/// stored text when it is NULL, empty, still RFC-2047-encoded, or carries
/// U+FFFD mojibake baked in before full legacy-charset decoding landed.
///
/// The column is compared as text: address columns are JSONB on PostgreSQL,
/// where `jsonb LIKE text` and `jsonb = text` have no operator. `CAST(x AS
/// text)` is a no-op for the TEXT columns and for every SQLite type.
fn stale_text(col: message::Column) -> Expr {
    let cast = || as_text_col(col);
    cast()
        .is_null()
        .or(cast().eq(""))
        .or(cast().like("%=?%?=%"))
        .or(cast().like("%\u{FFFD}%"))
}

/// Existing-message id lookup by `(account_id, external_id)` inside the txn.
async fn find_message_id_in_tx(
    tx: &mut DbTxn,
    db: &DbPool,
    account_id: &str,
    external_id: &str,
) -> Result<Option<String>, SyncError> {
    let mut sel = Sq::select();
    sel.column(message::Column::Id)
        .from(message::Entity)
        .and_where(message::Column::AccountId.eq(id_value(db, account_id)?))
        .and_where(message::Column::ExternalId.eq(external_id));
    tx_fetch_id(tx, &sel).await
}

/// One message row ready for the shared envelope insert.
struct MessageInsert<'a> {
    id_bind: Value,
    account_bind: Value,
    folder_bind: Value,
    external_id: &'a str,
    message_id_header: Option<&'a str>,
    subject: Option<&'a str>,
    from_json: Option<&'a str>,
    to_json: Option<&'a str>,
    cc_json: Option<&'a str>,
    date: Option<&'a str>,
    is_read: bool,
    is_starred: bool,
    flags_json: &'a str,
    size_bytes: Option<i32>,
    in_reply_to: Option<&'a str>,
    references_headers: Option<&'a str>,
    snippet: Option<&'a str>,
    has_attachments: bool,
    body_text: Option<&'a str>,
    body_html: Option<&'a str>,
    jmap_thread_id: Option<&'a str>,
}

/// Insert one message with the canonical envelope column set.
///
/// Shared body of the IMAP/JMAP upserts (`snoozed_until` stays schema-default
/// NULL exactly like the legacy `VALUES (…, NULL)` spelling).
fn message_insert(db: &DbPool, m: MessageInsert<'_>) -> InsertStatement {
    let mut ins = Sq::insert();
    ins.into_table(message::Entity)
        .columns([
            message::Column::Id,
            message::Column::AccountId,
            message::Column::FolderId,
            message::Column::ExternalId,
            message::Column::MessageIdHeader,
            message::Column::Subject,
            message::Column::FromAddress,
            message::Column::ToAddresses,
            message::Column::CcAddresses,
            message::Column::Date,
            message::Column::IsRead,
            message::Column::IsStarred,
            message::Column::Flags,
            message::Column::SizeBytes,
            message::Column::InReplyTo,
            message::Column::ReferencesHeaders,
            message::Column::Snippet,
            message::Column::HasAttachments,
            message::Column::BodyText,
            message::Column::BodyHtml,
            message::Column::JmapThreadId,
        ])
        .values_panic(vec![
            Expr::val(m.id_bind),
            Expr::val(m.account_bind),
            Expr::val(m.folder_bind),
            Expr::val(m.external_id),
            Expr::val(opt_str_value(m.message_id_header)),
            Expr::val(opt_str_value(m.subject)),
            Expr::val(opt_json_value(db, m.from_json)),
            Expr::val(opt_json_value(db, m.to_json)),
            Expr::val(opt_json_value(db, m.cc_json)),
            Expr::val(opt_date_value(db, m.date)),
            Expr::val(m.is_read),
            Expr::val(m.is_starred),
            Expr::val(opt_json_value(db, Some(m.flags_json))),
            Expr::val(m.size_bytes),
            Expr::val(opt_str_value(m.in_reply_to)),
            Expr::val(opt_str_value(m.references_headers)),
            Expr::val(opt_str_value(m.snippet)),
            Expr::val(m.has_attachments),
            Expr::val(opt_str_value(m.body_text)),
            Expr::val(opt_str_value(m.body_html)),
            Expr::val(opt_str_value(m.jmap_thread_id)),
        ]);
    ins
}

/// Refresh read/star/flags state plus fill-in semantics for a matched message.
///
/// `flags = excluded.flags`, while subject/snippet/address columns only move
/// off RFC-2047-encoded placeholders or U+FFFD mojibake and date/header
/// columns only fill previously-absent values — the legacy ON CONFLICT
/// clauses, extended so charset-mangled sender names heal on re-sync.
fn apply_fill_in_on_conflict(mut insert: InsertStatement) -> InsertStatement {
    let conflict = OnConflict::columns([message::Column::AccountId, message::Column::ExternalId])
        .update_columns([message::Column::IsRead, message::Column::IsStarred])
        .value(message::Column::Flags, excluded_col(message::Column::Flags))
        .value(
            message::Column::Subject,
            Expr::case(
                stale_text(message::Column::Subject),
                excluded_col(message::Column::Subject),
            )
            .finally(cur_row_col(message::Column::Subject)),
        )
        .value(
            message::Column::FromAddress,
            Expr::case(
                stale_text(message::Column::FromAddress),
                excluded_col(message::Column::FromAddress),
            )
            .finally(cur_row_col(message::Column::FromAddress)),
        )
        .value(
            message::Column::ToAddresses,
            Expr::case(
                stale_text(message::Column::ToAddresses),
                excluded_col(message::Column::ToAddresses),
            )
            .finally(cur_row_col(message::Column::ToAddresses)),
        )
        .value(
            message::Column::CcAddresses,
            Expr::case(
                stale_text(message::Column::CcAddresses),
                excluded_col(message::Column::CcAddresses),
            )
            .finally(cur_row_col(message::Column::CcAddresses)),
        )
        .value(
            message::Column::Date,
            Func::coalesce([
                cur_row_col(message::Column::Date),
                excluded_col(message::Column::Date),
            ]),
        )
        .value(
            message::Column::Snippet,
            Expr::case(
                stale_text(message::Column::Snippet),
                excluded_col(message::Column::Snippet),
            )
            .finally(cur_row_col(message::Column::Snippet)),
        )
        .value(
            message::Column::MessageIdHeader,
            Func::coalesce([
                cur_row_col(message::Column::MessageIdHeader),
                excluded_col(message::Column::MessageIdHeader),
            ]),
        )
        .value(message::Column::UpdatedAt, Expr::current_timestamp())
        .to_owned();
    insert.on_conflict(conflict);
    insert
}

pub(crate) async fn upsert_message_in_tx(
    tx: &mut DbTxn,
    db: &DbPool,
    account_id: &str,
    folder_id: &str,
    msg: &ImapMessage,
) -> Result<bool, SyncError> {
    let external_id = imap_message_external_id(folder_id, msg.uid);

    let existing = find_message_id_in_tx(tx, db, account_id, &external_id).await?;

    let flags_json = serde_json::to_string(&msg.flags).unwrap_or_else(|_| "{}".into());
    let is_read = msg
        .flags
        .iter()
        .any(|f| f.contains("Seen") || f.contains("\\Seen"));
    let is_starred = msg
        .flags
        .iter()
        .any(|f| f.contains("Flagged") || f.contains("\\Flagged"));
    let snippet = msg.subject.as_deref().map(truncate_for_snippet);

    let from_json = msg
        .from
        .as_ref()
        .map(|f| serde_json::json!({ "raw": f }).to_string());
    let to_json = msg
        .to
        .as_ref()
        .map(|t| serde_json::json!(vec![t]).to_string());

    let was_new = existing.is_none();

    let body_html = persist_body_html(msg.body_html.as_deref());
    let insert = apply_fill_in_on_conflict(message_insert(
        db,
        MessageInsert {
            id_bind: id_value(db, &new_uuid_text())?,
            account_bind: id_value(db, account_id)?,
            folder_bind: id_value(db, folder_id)?,
            external_id: &external_id,
            message_id_header: msg.message_id.as_deref(),
            subject: msg.subject.as_deref(),
            from_json: from_json.as_deref(),
            to_json: to_json.as_deref(),
            cc_json: msg.cc.as_deref(),
            date: msg.date.as_deref(),
            is_read,
            is_starred,
            flags_json: &flags_json,
            size_bytes: msg.size.and_then(|s| i32::try_from(s).ok()),
            in_reply_to: msg.in_reply_to.as_deref(),
            references_headers: msg.references.as_deref(),
            snippet: snippet.as_deref(),
            has_attachments: msg.has_attachments,
            body_text: msg.body_text.as_deref(),
            body_html: body_html.as_deref(),
            jmap_thread_id: None,
        },
    ));
    tx_execute(tx, &insert).await?;

    Ok(was_new)
}

pub(crate) async fn upsert_jmap_message_in_tx(
    tx: &mut DbTxn,
    db: &DbPool,
    account_id: &str,
    folder_id: &str,
    email: &crate::sync::jmap_client::JmapEmail,
) -> Result<(bool, String), SyncError> {
    let external_id = &email.id;

    let existing = find_message_id_in_tx(tx, db, account_id, external_id).await?;

    let is_read = email.is_seen();
    let is_starred = email.is_flagged();
    let snippet = email
        .preview
        .clone()
        .or_else(|| email.subject.as_ref().map(|s| truncate_for_snippet(s)));

    let from_json = email
        .format_from()
        .map(|f| serde_json::json!({ "raw": f }).to_string());
    let to_json = email
        .to_string_list()
        .map(|t| serde_json::json!(vec![t]).to_string());
    let cc_json = email.cc.as_ref().map(|addrs| {
        let formatted: Vec<String> = addrs
            .iter()
            .map(|a| match (&a.name, &a.email) {
                (Some(name), Some(email)) => format!("{name} <{email}>"),
                (None, Some(email)) => email.clone(),
                _ => String::new(),
            })
            .collect();
        serde_json::json!(formatted).to_string()
    });

    let flags_json = serde_json::to_string(&email.keywords).unwrap_or_else(|_| "{}".into());

    if let Some(id) = existing {
        let mut upd = Sq::update();
        upd.table(message::Entity)
            .value(message::Column::IsRead, Expr::val(is_read))
            .value(message::Column::IsStarred, Expr::val(is_starred))
            .value(
                message::Column::Flags,
                Expr::val(opt_json_value(db, Some(flags_json.as_str()))),
            )
            // Server-side moves re-home the row; the JMAP threadId is persisted.
            .value(
                message::Column::FolderId,
                Expr::val(id_value(db, folder_id)?),
            )
            .value(
                message::Column::JmapThreadId,
                Expr::val(opt_str_value(email.thread_id.as_deref())),
            )
            .value(message::Column::UpdatedAt, Expr::current_timestamp())
            .and_where(message::Column::Id.eq(id_value(db, &id)?));
        tx_execute(tx, &upd).await?;
        Ok((false, id))
    } else {
        let in_reply_to = email
            .in_reply_to
            .as_ref()
            .and_then(|ids| ids.first())
            .cloned();
        let references = email.references.as_ref().map(|refs| refs.join(" "));
        let message_id_header = email.message_id_header();
        let body_text = email.body_text();
        let body_html = persist_body_html(email.body_html().as_deref());
        let insert = message_insert(
            db,
            MessageInsert {
                id_bind: id_value(db, &new_uuid_text())?,
                account_bind: id_value(db, account_id)?,
                folder_bind: id_value(db, folder_id)?,
                external_id,
                message_id_header: message_id_header.as_deref(),
                subject: email.subject.as_deref(),
                from_json: from_json.as_deref(),
                to_json: to_json.as_deref(),
                cc_json: cc_json.as_deref(),
                date: email.received_at.as_deref(),
                is_read,
                is_starred,
                flags_json: &flags_json,
                size_bytes: email.size.map(|s| i32::try_from(s).unwrap_or(i32::MAX)),
                in_reply_to: in_reply_to.as_deref(),
                references_headers: references.as_deref(),
                snippet: snippet.as_deref(),
                has_attachments: email.has_attachment.unwrap_or(false),
                body_text: body_text.as_deref(),
                body_html: body_html.as_deref(),
                jmap_thread_id: email.thread_id.as_deref(),
            },
        );
        tx_execute(tx, &insert).await?;
        let id = find_message_id_in_tx(tx, db, account_id, external_id)
            .await?
            .ok_or_else(|| SyncError::Internal("JMAP message insert lost its row".into()))?;
        Ok((true, id))
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn save_cursor_in_tx(
    tx: &mut DbTxn,
    db: &DbPool,
    account_id: &str,
    folder_id: &str,
    protocol: &str,
    uid_validity: u32,
    last_uid: u32,
    last_modseq: u64,
) -> Result<(), SyncError> {
    let cursor_value = format_cursor_value(uid_validity, last_uid, last_modseq);
    let mut ins = Sq::insert();
    ins.into_table(sync_cursor::Entity)
        .columns([
            sync_cursor::Column::Id,
            sync_cursor::Column::AccountId,
            sync_cursor::Column::FolderId,
            sync_cursor::Column::Protocol,
            sync_cursor::Column::CursorType,
            sync_cursor::Column::CursorValue,
            sync_cursor::Column::UpdatedAt,
        ])
        .values_panic([
            Expr::val(id_value(db, &new_uuid_text())?),
            Expr::val(id_value(db, account_id)?),
            Expr::val(id_value(db, folder_id)?),
            Expr::val(protocol),
            Expr::val("uidvalidity_uid"),
            Expr::val(cursor_value),
            Expr::current_timestamp(),
        ])
        .on_conflict(
            OnConflict::columns([
                sync_cursor::Column::AccountId,
                sync_cursor::Column::FolderId,
                sync_cursor::Column::CursorType,
            ])
            .update_columns([
                sync_cursor::Column::CursorValue,
                sync_cursor::Column::UpdatedAt,
            ])
            .to_owned(),
        );
    tx_execute(tx, &ins).await?;
    Ok(())
}

pub(crate) async fn save_jmap_cursor_in_tx(
    tx: &mut DbTxn,
    db: &DbPool,
    account_id: &str,
    folder_id: &str,
    query_state: &str,
) -> Result<(), SyncError> {
    save_state_cursor_in_tx(tx, db, account_id, folder_id, "state_token", query_state).await
}

/// Upsert a JMAP state cursor of `cursor_type` (`state_token` per folder,
/// `email_state` account-level). Opaque server tokens, stored verbatim.
async fn save_state_cursor_in_tx(
    tx: &mut DbTxn,
    db: &DbPool,
    account_id: &str,
    folder_id: &str,
    cursor_type: &str,
    cursor_value: &str,
) -> Result<(), SyncError> {
    let mut ins = Sq::insert();
    ins.into_table(sync_cursor::Entity)
        .columns([
            sync_cursor::Column::Id,
            sync_cursor::Column::AccountId,
            sync_cursor::Column::FolderId,
            sync_cursor::Column::Protocol,
            sync_cursor::Column::CursorType,
            sync_cursor::Column::CursorValue,
            sync_cursor::Column::UpdatedAt,
        ])
        .values_panic([
            Expr::val(id_value(db, &new_uuid_text())?),
            Expr::val(id_value(db, account_id)?),
            Expr::val(id_value(db, folder_id)?),
            Expr::val("jmap"),
            Expr::val(cursor_type),
            Expr::val(cursor_value),
            Expr::current_timestamp(),
        ])
        .on_conflict(
            OnConflict::columns([
                sync_cursor::Column::AccountId,
                sync_cursor::Column::FolderId,
                sync_cursor::Column::CursorType,
            ])
            .update_columns([
                sync_cursor::Column::CursorValue,
                sync_cursor::Column::UpdatedAt,
            ])
            .to_owned(),
        );
    tx_execute(tx, &ins).await?;
    Ok(())
}

pub(crate) async fn clear_folder_messages_in_tx(
    tx: &mut DbTxn,
    db: &DbPool,
    folder_id: &str,
) -> Result<(), SyncError> {
    let folder_bind = id_value(db, folder_id)?;

    let mut del_messages = Sq::delete();
    del_messages
        .from_table(message::Entity)
        .and_where(message::Column::FolderId.eq(folder_bind.clone()));
    tx_execute(tx, &del_messages).await?;

    let mut del_cursors = Sq::delete();
    del_cursors
        .from_table(sync_cursor::Entity)
        .and_where(sync_cursor::Column::FolderId.eq(folder_bind))
        // This is an IMAP-path wipe (UIDVALIDITY change): folder-scoped
        // cursors must die with the wiped rows — a deltas-only resume could
        // never refetch them. Account-level JMAP `email_state` rows merely
        // ANCHOR on the folder and must survive, or the Email/changes
        // safety net silently goes dark on an IMAP-fallback wipe.
        .and_where(sync_cursor::Column::CursorType.ne("email_state"));
    tx_execute(tx, &del_cursors).await?;
    Ok(())
}

/// Folder message count filtered to live (and optionally unread) rows.
fn folder_count_stmt(folder_bind: Value, unread_only: bool) -> SelectStatement {
    let mut sel = Sq::select();
    sel.expr(Func::count(Expr::col(message::Column::Id)))
        .from(message::Entity)
        .and_where(message::Column::FolderId.eq(folder_bind))
        .and_where(message::Column::IsDeleted.eq(false));
    if unread_only {
        sel.and_where(message::Column::IsRead.eq(false));
    }
    sel
}

pub(crate) async fn update_folder_counts_in_tx(
    tx: &mut DbTxn,
    db: &DbPool,
    folder_id: &str,
) -> Result<(), SyncError> {
    let folder_bind = id_value(db, folder_id)?;
    let total = tx_fetch_count(tx, &folder_count_stmt(folder_bind.clone(), false)).await?;
    let unread = tx_fetch_count(tx, &folder_count_stmt(folder_bind.clone(), true)).await?;

    let mut upd = Sq::update();
    upd.table(folder::Entity)
        .value(
            folder::Column::TotalMessages,
            Expr::val(i32::try_from(total).unwrap_or(i32::MAX)),
        )
        .value(
            folder::Column::UnreadMessages,
            Expr::val(i32::try_from(unread).unwrap_or(i32::MAX)),
        )
        .value(folder::Column::UpdatedAt, Expr::current_timestamp())
        .and_where(folder::Column::Id.eq(folder_bind));
    tx_execute(tx, &upd).await?;
    Ok(())
}

#[cfg(test)]
mod effective_role_tests {
    use super::effective_folder_role;

    #[test]
    fn override_wins_over_detected() {
        assert_eq!(
            effective_folder_role(Some("inbox"), Some("archive")).as_deref(),
            Some("archive")
        );
        assert_eq!(
            effective_folder_role(Some("sent"), None).as_deref(),
            Some("sent")
        );
        assert_eq!(
            effective_folder_role(None, Some("drafts")).as_deref(),
            Some("drafts")
        );
        assert_eq!(
            effective_folder_role(Some("spam"), Some("")).as_deref(),
            Some("spam")
        );
    }
}

#[cfg(test)]
mod jsonb_text_cast_tests {
    use super::{as_text_col, stale_text};
    use crate::entities::message;
    use sea_orm::sea_query::Query as Sq;
    use sea_orm::{ExprTrait, Iden};

    fn where_sql(expr: sea_orm::sea_query::Expr) -> String {
        let mut sel = Sq::select();
        sel.column(message::Column::Id)
            .from(message::Entity)
            .and_where(expr);
        sel.build(super::PostgresQueryBuilder).0
    }

    /// Address columns are JSONB on PostgreSQL; a bare `LIKE`/`=` against them
    /// is a prepare-time `operator does not exist: jsonb ~~ text` failure.
    /// Inside `ON CONFLICT DO UPDATE`, current-row refs must additionally be
    /// table-qualified or Postgres rejects them as ambiguous vs `excluded`.
    #[test]
    fn stale_text_casts_and_qualifies_for_postgres() {
        for col in [
            message::Column::FromAddress,
            message::Column::ToAddresses,
            message::Column::CcAddresses,
        ] {
            let sql = where_sql(stale_text(col));
            let name = col.to_string().to_lowercase();
            assert!(
                sql.contains(&format!(r#"CAST("message"."{name}" AS text) LIKE"#)),
                "missing qualified text cast for {name}: {sql}"
            );
            assert!(
                !sql.contains(&format!(r#""{name}" LIKE"#)),
                "bare jsonb LIKE survived for {name}: {sql}"
            );
        }
    }

    #[test]
    fn mojibake_scan_casts_address_columns() {
        let cond = as_text_col(message::Column::FromAddress)
            .like("%\u{FFFD}%")
            .or(as_text_col(message::Column::ToAddresses).like("%\u{FFFD}%"));
        let sql = where_sql(cond);
        assert!(
            sql.contains(r#"CAST("message"."from_address" AS text) LIKE"#),
            "{sql}"
        );
        assert!(
            sql.contains(r#"CAST("message"."to_addresses" AS text) LIKE"#),
            "{sql}"
        );
    }
}

#[cfg(test)]
mod stale_imap_row_tests {
    use super::stale_imap_row_ids;
    fn rows(entries: &[(&str, Option<&str>)]) -> Vec<(String, Option<String>)> {
        entries
            .iter()
            .map(|(id, ext)| ((*id).to_string(), ext.map(str::to_string)))
            .collect()
    }

    #[test]
    fn deletes_only_rows_whose_uid_vanished() {
        let server: std::collections::HashSet<u32> = [10, 11].into_iter().collect();
        let local = rows(&[
            ("keep-1", Some("folder-a:10")),
            ("keep-2", Some("folder-a:11")),
            ("ghost", Some("folder-a:12")),
            ("draft-no-ext", None),
            ("draft-weird-ext", Some("local-draft-1")),
            ("legacy-bare-uid", Some("99")),
        ]);
        assert_eq!(
            stale_imap_row_ids(&local, &server),
            vec!["ghost".to_string(), "legacy-bare-uid".to_string()]
        );
    }

    #[test]
    fn empty_server_listing_marks_everything_parseable_stale() {
        // The caller must never pass a failed/partial listing; given a
        // complete-but-empty one (folder truly empty), every synced row is stale.
        let server = std::collections::HashSet::new();
        let local = rows(&[("a", Some("f:1")), ("b", None)]);
        assert_eq!(stale_imap_row_ids(&local, &server), vec!["a".to_string()]);
    }
}

/// Live-PostgreSQL roundtrips for the IMAP sync seam.
///
/// Every dialect bug that ever reached production (`jsonb ~~ text`,
/// ambiguous `ON CONFLICT` refs, UUID-id decodes) lived in this module and
/// passed the SQLite suite, because SQLite's loose typing forgives all of
/// them. These tests run the real seam functions against a migrated
/// PostgreSQL so the SQLite suite can never green-light a statement
/// PostgreSQL refuses to prepare.
///
/// Set `LYRA_TEST_DATABASE_URL=postgres://…` and run
/// `cargo test --features postgres -- --ignored sync::store::postgres_live`.
/// Each test seeds rows under fresh UUIDs; the throwaway test database is
/// expected to be ephemeral (CI service container).
#[cfg(test)]
#[cfg(feature = "postgres")]
mod postgres_live {
    use super::{
        get_folder_id, mojibake_message_uids, new_uuid_text, reconcile_folder_deletions,
        upsert_folder, upsert_message,
    };
    use crate::imap::ImapMessage;
    use crate::storage::{DbPool, Storage};

    /// One pool + one migration pass shared by every test in this module:
    /// libtest runs them in parallel, and concurrent migrations race on
    /// catalog objects (`pg_type_typname_nsp_index` duplicates).
    async fn pg() -> DbPool {
        static PG: tokio::sync::OnceCell<DbPool> = tokio::sync::OnceCell::const_new();
        PG.get_or_init(|| async {
            let url = std::env::var("LYRA_TEST_DATABASE_URL")
                .expect("LYRA_TEST_DATABASE_URL=postgres://…");
            let storage = Storage::new(&url).await.expect("connect postgres");
            storage.run_migrations().await.expect("run migrations");
            storage.pool().clone()
        })
        .await
        .clone()
    }

    /// `(account_id, folder_id)` for a fresh IMAP account with an INBOX.
    async fn seed_account_with_inbox(db: &DbPool) -> (String, String) {
        let user_id = new_uuid_text();
        let account_id = new_uuid_text();
        let DbPool::Postgres(pool) = db else {
            panic!("expected postgres pool");
        };
        // Ids are UUID columns; sqlx binds strings as text, so cast.
        sqlx::query(
            "INSERT INTO lyra_user (id, username, password_hash, encrypted_dek) \
             VALUES ($1::uuid, $2, 'hash', '[]')",
        )
        .bind(&user_id)
        .bind(format!("pg-live-{user_id}"))
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO mail_account (\
                 id, user_id, display_name, email_address, protocol, auth_type, credential, \
                 imap_host, imap_port, imap_security, is_active, sync_enabled\
             ) VALUES ($1::uuid, $2::uuid, 'PG Live', 'pg-live@example.com', 'imap', \
                       'password', '{}', 'imap.example.com', 993, 'tls', true, true)",
        )
        .bind(&account_id)
        .bind(&user_id)
        .execute(pool)
        .await
        .unwrap();
        upsert_folder(db, &account_id, "INBOX", None, &[])
            .await
            .unwrap();
        let folder_id = get_folder_id(db, &account_id, "INBOX").await.unwrap();
        (account_id, folder_id)
    }

    fn message(uid: u32, subject: &str, from: &str) -> ImapMessage {
        ImapMessage {
            uid,
            message_id: Some(format!("<{uid}@pg-live.example.com>")),
            subject: Some(subject.into()),
            from: Some(from.into()),
            to: Some("to@example.com".into()),
            cc: None,
            date: None,
            in_reply_to: None,
            references: None,
            flags: vec!["\\Seen".into()],
            size: Some(1024),
            body: None,
            body_text: None,
            body_html: None,
            has_attachments: false,
            attachments: vec![],
        }
    }

    /// The ON CONFLICT fill-in path: insert, then re-upsert the same
    /// external_id. Exercises the jsonb text-casts and table-qualified
    /// current-row refs that SQLite accepts unqualified.
    #[tokio::test]
    #[ignore = "needs postgres"]
    async fn upsert_fill_in_roundtrip() {
        let db = pg().await;
        let (account_id, folder_id) = seed_account_with_inbox(&db).await;
        let msg = message(42, "Subject A", "sender@example.com");

        assert!(
            upsert_message(&db, &account_id, &folder_id, &msg)
                .await
                .unwrap()
        );

        let mut again = msg.clone();
        again.flags = vec!["\\Seen".into(), "\\Flagged".into()];
        assert!(
            !upsert_message(&db, &account_id, &folder_id, &again)
                .await
                .unwrap()
        );

        let DbPool::Postgres(pool) = &db else {
            panic!()
        };
        let (count, is_starred, subject): (i64, bool, String) = sqlx::query_as(
            "SELECT COUNT(*), bool_or(is_starred), MIN(subject) \
             FROM message WHERE account_id = $1::uuid AND external_id = $2",
        )
        .bind(&account_id)
        .bind(super::imap_message_external_id(&folder_id, 42))
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(count, 1, "upsert must not duplicate the row");
        assert!(is_starred, "re-upsert refreshes flags");
        assert_eq!(subject, "Subject A", "non-stale subject is not clobbered");
    }

    /// The mojibake self-heal scan: a U+FFFD inside the JSONB from_address
    /// must still be found (`jsonb LIKE text` has no operator on Postgres).
    #[tokio::test]
    #[ignore = "needs postgres"]
    async fn mojibake_scan_finds_address_mojibake() {
        let db = pg().await;
        let (account_id, folder_id) = seed_account_with_inbox(&db).await;
        upsert_message(
            &db,
            &account_id,
            &folder_id,
            &message(7, "Clean", "S\u{FFFD}nder@example.com"),
        )
        .await
        .unwrap();
        upsert_message(
            &db,
            &account_id,
            &folder_id,
            &message(8, "Also clean", "ok@example.com"),
        )
        .await
        .unwrap();

        let uids = mojibake_message_uids(&db, &folder_id, 10).await.unwrap();
        assert_eq!(uids, vec![7], "only the mojibake row is flagged");
    }

    /// Deletion reconcile: reading UUID ids as text, then soft-deleting
    /// vanished UIDs.
    #[tokio::test]
    #[ignore = "needs postgres"]
    async fn deletion_reconcile_soft_deletes_vanished_uids() {
        let db = pg().await;
        let (account_id, folder_id) = seed_account_with_inbox(&db).await;
        upsert_message(
            &db,
            &account_id,
            &folder_id,
            &message(1, "Keep", "a@example.com"),
        )
        .await
        .unwrap();
        upsert_message(
            &db,
            &account_id,
            &folder_id,
            &message(2, "Vanish", "b@example.com"),
        )
        .await
        .unwrap();

        let server_uids: std::collections::HashSet<u32> = [1u32].into_iter().collect();
        let deleted = reconcile_folder_deletions(&db, &account_id, &folder_id, &server_uids)
            .await
            .unwrap();
        assert_eq!(deleted, 1);

        let DbPool::Postgres(pool) = &db else {
            panic!()
        };
        let soft_deleted: Vec<bool> = sqlx::query_scalar(
            "SELECT is_deleted FROM message \
             WHERE account_id = $1::uuid AND folder_id = $2::uuid ORDER BY external_id",
        )
        .bind(&account_id)
        .bind(&folder_id)
        .fetch_all(pool)
        .await
        .unwrap();
        assert_eq!(soft_deleted, vec![false, true], "vanished UID soft-deleted");
    }
}
