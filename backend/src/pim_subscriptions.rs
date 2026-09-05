//! HTTP handlers for ICS / webcal calendar subscriptions.

#![allow(clippy::doc_markdown)]

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, patch, post},
};
use chrono::Utc;
use sea_orm::sea_query::{Expr, Query as Sq};
use sea_orm::{ColumnTrait, ConnectionTrait, QueryResult, Value};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::{AuthState, AuthUser};
use crate::db_row::id_param;
use crate::entities::{calendar_subscription, subscription_event};
use crate::pim::PimError;
use crate::storage::DbPool;

pub fn routes() -> Router<AuthState> {
    Router::new()
        .route(
            "/api/v1/calendar-subscriptions",
            get(list_subscriptions).post(create_subscription),
        )
        .route(
            "/api/v1/calendar-subscriptions/{id}",
            patch(update_subscription).delete(delete_subscription),
        )
        .route(
            "/api/v1/calendar-subscriptions/{id}/refresh",
            post(refresh_one),
        )
        .route(
            "/api/v1/calendar-subscriptions/{id}/events",
            get(list_subscription_events),
        )
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Subscription {
    pub id: String,
    pub url: String,
    pub name: String,
    pub color: Option<String>,
    pub is_active: bool,
    pub last_fetched_at: Option<String>,
    pub last_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionEvent {
    pub id: String,
    pub subscription_id: String,
    pub summary: Option<String>,
    pub description: Option<String>,
    pub dtstart: Option<String>,
    pub dtend: Option<String>,
    pub location: Option<String>,
    pub is_all_day: bool,
    pub recurrence_rule: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSubscriptionRequest {
    pub url: String,
    pub name: Option<String>,
    pub color: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateSubscriptionRequest {
    pub name: Option<String>,
    pub color: Option<String>,
    pub is_active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct ListSubEventsQuery {
    pub start: Option<String>,
    pub end: Option<String>,
}

fn orm_err(err: sea_orm::DbErr) -> PimError {
    PimError::Database(match err {
        sea_orm::DbErr::Exec(sea_orm::RuntimeErr::SqlxError(e))
        | sea_orm::DbErr::Query(sea_orm::RuntimeErr::SqlxError(e))
        | sea_orm::DbErr::Conn(sea_orm::RuntimeErr::SqlxError(e)) => std::sync::Arc::try_unwrap(e)
            .unwrap_or_else(|shared| sqlx::Error::Protocol(shared.to_string())),
        other => sqlx::Error::Protocol(other.to_string()),
    })
}

fn id_value(db: &DbPool, id: &str) -> Result<Value, PimError> {
    Ok(match id_param(db, id).map_err(|_| PimError::NotFound)? {
        crate::db_row::IdParam::Text(s) => Value::String(Some(s)),
        crate::db_row::IdParam::Uuid(u) => Value::Uuid(Some(u)),
    })
}

fn now_value(db: &DbPool) -> Value {
    match db {
        DbPool::Sqlite(_) => {
            Value::String(Some(Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()))
        }
        #[cfg(feature = "postgres")]
        DbPool::Postgres(_) => Value::ChronoDateTimeUtc(Some(Utc::now())),
    }
}

fn row_id(row: &QueryResult, col: &str) -> Result<String, PimError> {
    if let Ok(s) = row.try_get::<String>("", col) {
        return Ok(s);
    }
    row.try_get::<Uuid>("", col)
        .map(|u| u.to_string())
        .map_err(orm_err)
}

fn row_opt_ts(row: &QueryResult, col: &str) -> Result<Option<String>, PimError> {
    if let Ok(text) = row.try_get::<Option<String>>("", col) {
        return Ok(text.map(crate::db_row::normalize_ts_text));
    }
    row.try_get::<Option<chrono::DateTime<Utc>>>("", col)
        .map(|opt| opt.map(|t| t.to_rfc3339()))
        .map_err(orm_err)
}

fn row_ts(row: &QueryResult, col: &str) -> Result<String, PimError> {
    row_opt_ts(row, col)?.ok_or(PimError::NotFound)
}

fn subscription_from_row(row: &QueryResult) -> Result<Subscription, PimError> {
    Ok(Subscription {
        id: row_id(row, "id")?,
        url: row.try_get("", "url").map_err(orm_err)?,
        name: row.try_get("", "name").map_err(orm_err)?,
        color: row.try_get("", "color").ok().flatten(),
        is_active: row.try_get("", "is_active").map_err(orm_err)?,
        last_fetched_at: row_opt_ts(row, "last_fetched_at")?,
        last_error: row.try_get("", "last_error").ok().flatten(),
        created_at: row_ts(row, "created_at")?,
        updated_at: row_ts(row, "updated_at")?,
    })
}

fn event_from_row(row: &QueryResult) -> Result<SubscriptionEvent, PimError> {
    Ok(SubscriptionEvent {
        id: row_id(row, "id")?,
        subscription_id: row_id(row, "subscription_id")?,
        summary: row.try_get("", "summary").ok().flatten(),
        description: row.try_get("", "description").ok().flatten(),
        dtstart: row_opt_ts(row, "dtstart")?,
        dtend: row_opt_ts(row, "dtend")?,
        location: row.try_get("", "location").ok().flatten(),
        is_all_day: row.try_get("", "is_all_day").unwrap_or(false),
        recurrence_rule: row.try_get("", "recurrence_rule").ok().flatten(),
        status: row.try_get("", "status").ok().flatten(),
    })
}

const DEFAULT_COLORS: &[&str] = &[
    "#c08532", "#9fc9a2", "#9fbbe0", "#c0a8dd", "#dfa88f", "#7eb8a8",
];

async fn list_subscriptions(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<Vec<Subscription>>, PimError> {
    let db = state.db();
    let user = id_value(db, &user_id)?;
    let mut q = Sq::select();
    q.columns([
        calendar_subscription::Column::Id,
        calendar_subscription::Column::Url,
        calendar_subscription::Column::Name,
        calendar_subscription::Column::Color,
        calendar_subscription::Column::IsActive,
        calendar_subscription::Column::LastFetchedAt,
        calendar_subscription::Column::LastError,
        calendar_subscription::Column::CreatedAt,
        calendar_subscription::Column::UpdatedAt,
    ])
    .from(calendar_subscription::Entity)
    .and_where(calendar_subscription::Column::UserId.eq(user))
    .order_by(calendar_subscription::Column::Name, sea_orm::Order::Asc);
    let rows = db.orm().query_all(&q).await.map_err(orm_err)?;
    rows.iter().map(subscription_from_row).collect::<Result<Vec<_>, _>>().map(Json)
}

async fn create_subscription(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Json(body): Json<CreateSubscriptionRequest>,
) -> Result<(StatusCode, Json<Subscription>), PimError> {
    let db = state.db();
    let url = crate::ics::normalize_ics_url(&body.url).map_err(PimError::InvalidInput)?;
    let user = id_value(db, &user_id)?;
    let id_str = Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string();
    let id = id_value(db, &id_str)?;
    let name = body
        .name
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| "Subscribed calendar".into());
    let color = body.color.unwrap_or_else(|| {
        let n = id_str.bytes().map(u32::from).sum::<u32>() as usize;
        DEFAULT_COLORS[n % DEFAULT_COLORS.len()].to_string()
    });

    let mut insert = Sq::insert();
    insert
        .into_table(calendar_subscription::Entity)
        .columns([
            calendar_subscription::Column::Id,
            calendar_subscription::Column::UserId,
            calendar_subscription::Column::Url,
            calendar_subscription::Column::Name,
            calendar_subscription::Column::Color,
            calendar_subscription::Column::IsActive,
        ])
        .values_panic([
            Expr::val(id.clone()),
            Expr::val(user),
            Expr::val(url),
            Expr::val(name),
            Expr::val(color),
            Expr::val(true),
        ]);
    db.orm().execute(&insert).await.map_err(orm_err)?;

    // Best-effort immediate refresh
    let _ = crate::ics::refresh_subscription(db, &id_str).await;

    let mut q = Sq::select();
    q.columns([
        calendar_subscription::Column::Id,
        calendar_subscription::Column::Url,
        calendar_subscription::Column::Name,
        calendar_subscription::Column::Color,
        calendar_subscription::Column::IsActive,
        calendar_subscription::Column::LastFetchedAt,
        calendar_subscription::Column::LastError,
        calendar_subscription::Column::CreatedAt,
        calendar_subscription::Column::UpdatedAt,
    ])
    .from(calendar_subscription::Entity)
    .and_where(calendar_subscription::Column::Id.eq(id));
    let row = db
        .orm()
        .query_one(&q)
        .await
        .map_err(orm_err)?
        .ok_or(PimError::NotFound)?;
    Ok((StatusCode::CREATED, Json(subscription_from_row(&row)?)))
}

async fn update_subscription(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Path(id): Path<String>,
    Json(body): Json<UpdateSubscriptionRequest>,
) -> Result<Json<Subscription>, PimError> {
    let db = state.db();
    let user = id_value(db, &user_id)?;
    let sid = id_value(db, &id)?;
    let mut upd = Sq::update();
    upd.table(calendar_subscription::Entity)
        .value(calendar_subscription::Column::UpdatedAt, now_value(db))
        .and_where(calendar_subscription::Column::Id.eq(sid.clone()))
        .and_where(calendar_subscription::Column::UserId.eq(user.clone()));
    if let Some(name) = body.name {
        upd.value(calendar_subscription::Column::Name, name);
    }
    if let Some(color) = body.color {
        upd.value(calendar_subscription::Column::Color, color);
    }
    if let Some(active) = body.is_active {
        upd.value(calendar_subscription::Column::IsActive, active);
    }
    let res = db.orm().execute(&upd).await.map_err(orm_err)?;
    if res.rows_affected() == 0 {
        return Err(PimError::NotFound);
    }
    let mut q = Sq::select();
    q.columns([
        calendar_subscription::Column::Id,
        calendar_subscription::Column::Url,
        calendar_subscription::Column::Name,
        calendar_subscription::Column::Color,
        calendar_subscription::Column::IsActive,
        calendar_subscription::Column::LastFetchedAt,
        calendar_subscription::Column::LastError,
        calendar_subscription::Column::CreatedAt,
        calendar_subscription::Column::UpdatedAt,
    ])
    .from(calendar_subscription::Entity)
    .and_where(calendar_subscription::Column::Id.eq(sid))
    .and_where(calendar_subscription::Column::UserId.eq(user));
    let row = db
        .orm()
        .query_one(&q)
        .await
        .map_err(orm_err)?
        .ok_or(PimError::NotFound)?;
    Ok(Json(subscription_from_row(&row)?))
}

async fn delete_subscription(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Path(id): Path<String>,
) -> Result<StatusCode, PimError> {
    let db = state.db();
    let user = id_value(db, &user_id)?;
    let sid = id_value(db, &id)?;
    let mut del = Sq::delete();
    del.from_table(calendar_subscription::Entity)
        .and_where(calendar_subscription::Column::Id.eq(sid))
        .and_where(calendar_subscription::Column::UserId.eq(user));
    let res = db.orm().execute(&del).await.map_err(orm_err)?;
    if res.rows_affected() == 0 {
        return Err(PimError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn refresh_one(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, PimError> {
    let db = state.db();
    let user = id_value(db, &user_id)?;
    let sid = id_value(db, &id)?;
    let mut q = Sq::select();
    q.column(calendar_subscription::Column::Id)
        .from(calendar_subscription::Entity)
        .and_where(calendar_subscription::Column::Id.eq(sid))
        .and_where(calendar_subscription::Column::UserId.eq(user));
    if db.orm().query_one(&q).await.map_err(orm_err)?.is_none() {
        return Err(PimError::NotFound);
    }
    let n = crate::ics::refresh_subscription(db, &id)
        .await
        .map_err(|e| PimError::SyncError(e.to_string()))?;
    Ok(Json(serde_json::json!({ "status": "ok", "synced": n })))
}

async fn list_subscription_events(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Path(id): Path<String>,
    Query(query): Query<ListSubEventsQuery>,
) -> Result<Json<Vec<SubscriptionEvent>>, PimError> {
    let db = state.db();
    let user = id_value(db, &user_id)?;
    let sid = id_value(db, &id)?;
    let mut own = Sq::select();
    own.column(calendar_subscription::Column::Id)
        .from(calendar_subscription::Entity)
        .and_where(calendar_subscription::Column::Id.eq(sid.clone()))
        .and_where(calendar_subscription::Column::UserId.eq(user));
    if db.orm().query_one(&own).await.map_err(orm_err)?.is_none() {
        return Err(PimError::NotFound);
    }

    let mut q = Sq::select();
    q.columns([
        subscription_event::Column::Id,
        subscription_event::Column::SubscriptionId,
        subscription_event::Column::Summary,
        subscription_event::Column::Description,
        subscription_event::Column::Dtstart,
        subscription_event::Column::Dtend,
        subscription_event::Column::Location,
        subscription_event::Column::IsAllDay,
        subscription_event::Column::RecurrenceRule,
        subscription_event::Column::Status,
    ])
    .from(subscription_event::Entity)
    .and_where(subscription_event::Column::SubscriptionId.eq(sid));
    if let Some(start) = &query.start {
        q.and_where(subscription_event::Column::Dtstart.gte(start.as_str()));
    }
    if let Some(end) = &query.end {
        q.and_where(subscription_event::Column::Dtstart.lt(end.as_str()));
    }
    let rows = db.orm().query_all(&q).await.map_err(orm_err)?;
    rows.iter()
        .map(event_from_row)
        .collect::<Result<Vec<_>, _>>()
        .map(Json)
}
