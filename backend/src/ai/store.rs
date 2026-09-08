//! `ai_settings` storage — sea-query based, mirroring the spam store's
//! dialect-agnostic id binding (TEXT on SQLite, native UUID on Postgres).

use sea_orm::sea_query::{Alias, Expr, Query as Sq};
use sea_orm::{ConnectionTrait, ExprTrait};

use crate::db_row::{IdParam, id_param};
use crate::storage::DbPool;

use super::settings::{AiDialect, AiFeatures, AiSettings, AiSettingsError};

fn orm_err(err: sea_orm::DbErr) -> AiSettingsError {
    use sea_orm::RuntimeErr;
    let sqlx_err = match err {
        sea_orm::DbErr::Exec(RuntimeErr::SqlxError(e))
        | sea_orm::DbErr::Query(RuntimeErr::SqlxError(e))
        | sea_orm::DbErr::Conn(RuntimeErr::SqlxError(e)) => {
            std::sync::Arc::try_unwrap(e).unwrap_or_else(|s| sqlx::Error::Protocol(s.to_string()))
        }
        other => sqlx::Error::Protocol(other.to_string()),
    };
    AiSettingsError::Database(sqlx_err)
}

fn user_value(db: &DbPool, user_id: &str) -> Result<sea_orm::Value, AiSettingsError> {
    Ok(
        match id_param(db, user_id).map_err(|e| AiSettingsError::Invalid(e.to_string()))? {
            IdParam::Text(s) => sea_orm::Value::String(Some(s)),
            IdParam::Uuid(u) => sea_orm::Value::Uuid(Some(u)),
        },
    )
}

/// Load the user's AI settings; a missing row reads as empty defaults.
pub async fn load_settings(db: &DbPool, user_id: &str) -> Result<AiSettings, AiSettingsError> {
    let user = user_value(db, user_id)?;
    let mut sel = Sq::select();
    sel.columns([
        Alias::new("enabled"),
        Alias::new("dialect"),
        Alias::new("base_url"),
        Alias::new("model"),
        Alias::new("api_key"),
        Alias::new("features"),
    ])
    .from(Alias::new("ai_settings"))
    .and_where(Expr::cust("user_id").eq(Expr::val(user)));
    let row = db.orm().query_one(&sel).await.map_err(orm_err)?;
    let Some(row) = row else {
        return Ok(AiSettings::empty());
    };
    let dialect = row
        .try_get::<String>("", "dialect")
        .ok()
        .and_then(|d| AiDialect::parse(&d))
        .unwrap_or(AiDialect::OpenAiChat);
    let features_raw = row.try_get::<String>("", "features").ok();
    let features = features_raw
        .as_deref()
        .and_then(|raw| serde_json::from_str::<AiFeatures>(raw).ok())
        .unwrap_or_default();
    Ok(AiSettings::from_columns(
        row.try_get("", "enabled").unwrap_or(false),
        dialect,
        row.try_get("", "base_url")
            .ok()
            .flatten()
            .unwrap_or_default(),
        row.try_get("", "model").ok().flatten().unwrap_or_default(),
        row.try_get("", "api_key")
            .ok()
            .flatten()
            .unwrap_or_default(),
        features,
    ))
}

/// Upsert; `api_key_blob` is the already-encrypted JSON envelope (or empty).
pub async fn save_settings(
    db: &DbPool,
    user_id: &str,
    s: &AiSettings,
) -> Result<(), AiSettingsError> {
    let user = user_value(db, user_id)?;
    let features = serde_json::to_string(&s.features)
        .map_err(|e| AiSettingsError::Invalid(format!("features not serializable: {e}")))?;
    let mut ins = Sq::insert();
    ins.into_table(Alias::new("ai_settings"))
        .columns([
            Alias::new("user_id"),
            Alias::new("enabled"),
            Alias::new("dialect"),
            Alias::new("base_url"),
            Alias::new("model"),
            Alias::new("api_key"),
            Alias::new("features"),
        ])
        .values_panic(vec![
            Expr::val(user),
            Expr::val(s.enabled),
            Expr::val(s.dialect.as_str()),
            Expr::val(s.base_url.trim()),
            Expr::val(s.model.trim()),
            Expr::val(s.key_blob()),
            Expr::val(features),
        ])
        .on_conflict(
            sea_orm::sea_query::OnConflict::column(Alias::new("user_id"))
                .update_columns([
                    Alias::new("enabled"),
                    Alias::new("dialect"),
                    Alias::new("base_url"),
                    Alias::new("model"),
                    Alias::new("api_key"),
                    Alias::new("features"),
                ])
                .to_owned(),
        );
    db.orm().execute(&ins).await.map_err(orm_err)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn pool() -> DbPool {
        let storage = crate::storage::Storage::new("sqlite::memory:")
            .await
            .unwrap();
        storage.run_migrations().await.unwrap();
        let db = storage.pool().clone();
        let DbPool::Sqlite(p) = &db else {
            panic!("sqlite");
        };
        sqlx::query(
            "INSERT INTO lyra_user (id, username, password_hash, encrypted_dek) \
             VALUES ('u1', 'aitest', 'hash', '[]')",
        )
        .execute(p)
        .await
        .unwrap();
        db
    }

    #[tokio::test]
    async fn missing_row_reads_as_empty_defaults() {
        let db = pool().await;
        let s = load_settings(&db, "u1").await.unwrap();
        assert_eq!(s, AiSettings::empty());
    }

    #[tokio::test]
    async fn settings_roundtrip_and_upsert() {
        let db = pool().await;
        let on = AiSettings::from_columns(
            true,
            AiDialect::Anthropic,
            "https://api.anthropic.com".into(),
            "claude-sonnet-4-5".into(),
            r#"{"ciphertext":"abc","nonce":"n"}"#.into(),
            AiFeatures {
                draft_reply: true,
                assistant: true,
            },
        );
        save_settings(&db, "u1", &on).await.unwrap();
        assert_eq!(load_settings(&db, "u1").await.unwrap(), on);
        assert!(load_settings(&db, "u1").await.unwrap().has_key());

        // Upsert overwrites, not duplicates; unknown dialect falls back.
        let off = AiSettings::empty();
        save_settings(&db, "u1", &off).await.unwrap();
        assert_eq!(load_settings(&db, "u1").await.unwrap(), AiSettings::empty());
    }
}
