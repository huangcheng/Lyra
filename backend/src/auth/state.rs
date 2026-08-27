//! Shared auth state.

use std::sync::Arc;

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QuerySelect, Value};

use crate::crypto::{self, CryptoError};
use crate::db_row::id_param;
use crate::entities::mail_account as account_entity;
use crate::kernel::App;
use crate::kv::KvStore;
use crate::storage::DbPool;

use super::dek::{
    fetch_encrypted_dek, install_master_key, master_key, provision_and_rotate_legacy_dek,
};
use super::session::SessionStore;

#[derive(Clone)]
pub struct AuthState {
    pub db: DbPool,
    pub sessions: SessionStore,
    pub min_password_length: usize,
    pub data_dir: std::path::PathBuf,
    pub app: Arc<App>,
    /// Per-auth-session OpenGPG unlock cache (passphrases only; never persisted).
    pub opengpg_unlock: Arc<crate::opengpg::UnlockRing>,
    /// Optional Microsoft mail OAuth app settings (None when not configured).
    pub ms_oauth: Option<crate::oauth::MsOAuthConfig>,
    /// Optional Yandex mail OAuth app settings (None when not configured).
    pub yandex_oauth: Option<crate::oauth::YandexOAuthConfig>,
}

impl AuthState {
    pub fn new(
        db: DbPool,
        config: &crate::config::Config,
        app: Arc<App>,
        kv: Arc<dyn KvStore>,
    ) -> Result<Self, anyhow::Error> {
        std::fs::create_dir_all(&config.data_dir)?;
        install_master_key(&config.master_key);
        Ok(Self {
            db: db.clone(),
            sessions: SessionStore::new(db, kv),
            min_password_length: config.min_password_length,
            data_dir: std::path::PathBuf::from(&config.data_dir),
            app,
            opengpg_unlock: Arc::new(crate::opengpg::UnlockRing::new()),
            ms_oauth: config.ms_oauth.clone(),
            yandex_oauth: config.yandex_oauth.clone(),
        })
    }

    /// Get the database pool.
    pub fn db(&self) -> &DbPool {
        &self.db
    }

    /// Kv store for sessions and user settings (privacy allow-list, etc.).
    pub fn kv(&self) -> &Arc<dyn KvStore> {
        self.sessions.kv()
    }

    /// Get the user's data encryption key (DEK) for credential encryption.
    ///
    /// The DEK is a random 256-bit key generated at bootstrap, wrapped with
    /// the per-user KEK (HKDF-SHA256 from the master key, bound to the user
    /// id) and stored in `lyra_user.encrypted_dek`. This unwraps it on demand.
    ///
    /// Users created before the DEK hierarchy have a NULL `encrypted_dek`.
    /// The first lookup mints a DEK and re-encrypts account passwords that
    /// still use the old padded-`LYRA_MASTER_KEY` scheme.
    ///
    /// # Errors
    /// Fails with a typed [`CryptoError`] if the master key was never
    /// installed, the user row is missing, or unwrapping fails (e.g. the
    /// DEK was wrapped under a different master key — re-add accounts or
    /// reset the database).
    pub async fn get_user_dek(db: &DbPool, user_id: &str) -> Result<Vec<u8>, CryptoError> {
        let kek = crypto::derive_user_kek(master_key()?, user_id);
        match fetch_encrypted_dek(db, user_id).await {
            Ok(wrapped) => crypto::unwrap_dek(&kek, &wrapped),
            Err(CryptoError::MissingDek) => {
                provision_and_rotate_legacy_dek(db, user_id, &kek).await
            }
            Err(e) => Err(e),
        }
    }

    /// Unwrap the user DEK (minting/rotating legacy credentials if needed),
    /// then reload the account password blob so callers never decrypt a
    /// pre-rotation snapshot.
    pub async fn get_user_dek_and_credential(
        db: &DbPool,
        user_id: &str,
        account_id: &str,
    ) -> Result<(Vec<u8>, String), CryptoError> {
        let dek = Self::get_user_dek(db, user_id).await?;
        let account = id_param(db, account_id).map_err(|e| CryptoError::Storage(e.to_string()))?;
        let user = id_param(db, user_id).map_err(|e| CryptoError::Storage(e.to_string()))?;
        let id_value = match account {
            crate::db_row::IdParam::Text(s) => Value::String(Some(s)),
            crate::db_row::IdParam::Uuid(u) => Value::Uuid(Some(u)),
        };
        let user_value = match user {
            crate::db_row::IdParam::Text(s) => Value::String(Some(s)),
            crate::db_row::IdParam::Uuid(u) => Value::Uuid(Some(u)),
        };
        let credential: Option<String> = account_entity::Entity::find()
            .select_only()
            .column(account_entity::Column::Credential)
            .filter(
                sea_orm::sea_query::Condition::all()
                    .add(account_entity::Column::Id.eq(id_value))
                    .add(account_entity::Column::UserId.eq(user_value)),
            )
            .into_tuple::<Option<String>>()
            .one(&db.orm())
            .await
            .map_err(|e| CryptoError::Storage(unwrap_sqlx_err(e).to_string()))?
            .flatten();
        let credential = credential.ok_or_else(|| {
            CryptoError::Storage("mail account not found while loading credentials".into())
        })?;
        Ok((dek, credential))
    }
}

// ── Route handlers ──────────────────────────────────────────────────

/// Unwrap sea_orm's Arc-wrapped driver error (sqlx 0.9 errors are not Clone).
fn unwrap_sqlx_err(err: sea_orm::DbErr) -> sqlx::Error {
    use sea_orm::RuntimeErr;
    match err {
        sea_orm::DbErr::Exec(RuntimeErr::SqlxError(e))
        | sea_orm::DbErr::Query(RuntimeErr::SqlxError(e))
        | sea_orm::DbErr::Conn(RuntimeErr::SqlxError(e)) => std::sync::Arc::try_unwrap(e)
            .unwrap_or_else(|shared| sqlx::Error::Protocol(shared.to_string())),
        other => sqlx::Error::Protocol(other.to_string()),
    }
}
