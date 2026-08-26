//! Encrypted OAuth token blob + access-secret resolution for sync/send.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex as AsyncMutex;
use zeroize::Zeroizing;

use super::microsoft::{MsOAuthConfig, MsOAuthError, refresh_access_token as refresh_microsoft};
use super::yandex::{YandexOAuthConfig, refresh_access_token as refresh_yandex};
use crate::accounts::{is_microsoft_mail_host, is_yandex_mail_host};
use crate::crypto::{self, EncryptedCredential};
use crate::db_row::id_param;
use crate::storage::DbPool;

/// Refresh when the access token expires within this window.
const REFRESH_SKEW_SECS: i64 = 120;

/// Per-account refresh single-flight locks.
///
/// The sync loop, IDLE watcher, SMTP send, and HTTP mutations can all resolve
/// the same account concurrently. Providers rotate refresh tokens on use, so
/// two racing refreshes can invalidate each other and lock the mailbox out
/// until re-consent. Serializing per account (plus the post-lock re-read in
/// [`resolve_mail_access_secret`]) makes the loser reuse the winner's tokens.
static REFRESH_LOCKS: LazyLock<Mutex<HashMap<String, Arc<AsyncMutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn refresh_lock_for(account_id: &str) -> Arc<AsyncMutex<()>> {
    REFRESH_LOCKS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry(account_id.to_string())
        .or_default()
        .clone()
}

/// Runtime OAuth client configs for token refresh in sync workers.
#[derive(Debug, Clone, Default)]
pub struct OAuthRefreshConfigs {
    pub microsoft: Option<MsOAuthConfig>,
    pub yandex: Option<YandexOAuthConfig>,
}

impl OAuthRefreshConfigs {
    pub fn from_env() -> Self {
        Self {
            microsoft: MsOAuthConfig::from_client_env(),
            yandex: YandexOAuthConfig::from_client_env(),
        }
    }
}

/// Plaintext JSON stored inside the DEK-encrypted `mail_account.credential` blob
/// when `auth_type = oauth2`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthTokenSet {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix epoch seconds when `access_token` expires.
    pub expires_at: i64,
    #[serde(default)]
    pub token_type: String,
    #[serde(default)]
    pub scope: String,
}

/// Secret used for IMAP/SMTP auth after decrypt (+ optional refresh).
#[derive(Debug)]
pub enum MailAccessSecret {
    /// Password or app-password (PLAIN/LOGIN).
    Password(Zeroizing<String>),
    /// Bearer access token for XOAUTH2.
    AccessToken(Zeroizing<String>),
}

impl MailAccessSecret {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Password(p) | Self::AccessToken(p) => p.as_str(),
        }
    }

    pub fn is_xoauth2(&self) -> bool {
        matches!(self, Self::AccessToken(_))
    }
}

/// Encrypt token JSON under the user DEK (same seam as passwords).
pub fn encrypt_oauth_tokens(dek: &[u8], tokens: &OAuthTokenSet) -> Result<String, MsOAuthError> {
    let json = serde_json::to_string(tokens)
        .map_err(|e| MsOAuthError::Internal(format!("serialize tokens: {e}")))?;
    let encrypted =
        crypto::encrypt(dek, json.as_bytes()).map_err(|e| MsOAuthError::Internal(e.to_string()))?;
    serde_json::to_string(&encrypted)
        .map_err(|e| MsOAuthError::Internal(format!("credential blob: {e}")))
}

fn decrypt_oauth_tokens(credential_json: &str, dek: &[u8]) -> Result<OAuthTokenSet, MsOAuthError> {
    let encrypted: EncryptedCredential =
        serde_json::from_str(credential_json).map_err(|_| MsOAuthError::CredentialDecrypt)?;
    let bytes = crypto::decrypt(dek, &encrypted).map_err(|_| MsOAuthError::CredentialDecrypt)?;
    let json = String::from_utf8(bytes).map_err(|_| MsOAuthError::CredentialDecrypt)?;
    serde_json::from_str(&json).map_err(|_| MsOAuthError::CredentialDecrypt)
}

fn decrypt_password(credential_json: &str, dek: &[u8]) -> Result<Zeroizing<String>, MsOAuthError> {
    let encrypted: EncryptedCredential = serde_json::from_str(credential_json)
        .map_err(|e| MsOAuthError::Internal(format!("invalid credential blob: {e}")))?;
    let bytes =
        crypto::decrypt(dek, &encrypted).map_err(|e| MsOAuthError::Internal(e.to_string()))?;
    let s = String::from_utf8(bytes)
        .map_err(|e| MsOAuthError::Internal(format!("password utf-8: {e}")))?;
    Ok(Zeroizing::new(s))
}

/// Persist an updated (refreshed) credential blob.
pub async fn update_account_credential(
    db: &DbPool,
    account_id: &str,
    credential_json: &str,
) -> Result<(), MsOAuthError> {
    let id = id_param(db, account_id)
        .map_err(|_| MsOAuthError::Internal("invalid account id".into()))?;
    db_execute!(
        db,
        "UPDATE mail_account SET credential = ?, updated_at = datetime('now') WHERE id = ?",
        credential_json,
        &id
    )
    .map_err(|e| MsOAuthError::Internal(format!("update credential: {e}")))?;
    Ok(())
}

/// Re-read the stored credential blob (single-flight re-check).
async fn load_credential_blob(
    db: &DbPool,
    account_id: &str,
) -> Result<Option<String>, MsOAuthError> {
    let id = id_param(db, account_id)
        .map_err(|_| MsOAuthError::Internal("invalid account id".into()))?;
    let row = db_scalar_optional!(
        db,
        String,
        "SELECT credential FROM mail_account WHERE id = ?",
        &id
    )
    .map_err(|e| MsOAuthError::Internal(format!("load credential: {e}")))?;
    Ok(row)
}

/// Decrypt account credential; refresh OAuth access token when near expiry.
///
/// Refreshes are single-flight per account: after acquiring the lock the
/// credential is re-read from the DB, so a task that waited behind a
/// concurrent refresh reuses the freshly persisted tokens instead of
/// redeeming (and burning) the previous refresh token again.
pub async fn resolve_mail_access_secret(
    db: &DbPool,
    account_id: &str,
    auth_type: &str,
    credential_json: &str,
    dek: &[u8],
    imap_host: Option<&str>,
    configs: &OAuthRefreshConfigs,
) -> Result<MailAccessSecret, MsOAuthError> {
    if auth_type == "oauth2" {
        let mut tokens = decrypt_oauth_tokens(credential_json, dek)?;
        let now = chrono::Utc::now().timestamp();
        if tokens.expires_at - now <= REFRESH_SKEW_SECS {
            let refresh_lock = refresh_lock_for(account_id);
            let _guard = refresh_lock.lock().await;
            // Double-check: another caller may have refreshed while we waited.
            let reloaded = match load_credential_blob(db, account_id).await {
                Ok(Some(blob)) => decrypt_oauth_tokens(&blob, dek).ok(),
                _ => None,
            };
            if let Some(fresh) = reloaded {
                if fresh.expires_at - chrono::Utc::now().timestamp() > REFRESH_SKEW_SECS {
                    return Ok(MailAccessSecret::AccessToken(Zeroizing::new(
                        fresh.access_token,
                    )));
                }
                // Newest refresh_token wins even when still inside the skew.
                tokens = fresh;
            }
            let refreshed = match refresh_provider(imap_host, configs)? {
                OAuthRefreshProvider::Microsoft(cfg) => {
                    refresh_microsoft(cfg, &tokens.refresh_token).await?
                }
                OAuthRefreshProvider::Yandex(cfg) => {
                    refresh_yandex(cfg, &tokens.refresh_token).await?
                }
            };
            tokens.access_token = refreshed.access_token;
            if let Some(rt) = refreshed.refresh_token {
                tokens.refresh_token = rt;
            }
            tokens.expires_at = refreshed.expires_at;
            if let Some(scope) = refreshed.scope {
                tokens.scope = scope;
            }
            let blob = encrypt_oauth_tokens(dek, &tokens)?;
            update_account_credential(db, account_id, &blob).await?;
        }
        return Ok(MailAccessSecret::AccessToken(Zeroizing::new(
            tokens.access_token,
        )));
    }

    Ok(MailAccessSecret::Password(decrypt_password(
        credential_json,
        dek,
    )?))
}

enum OAuthRefreshProvider<'a> {
    Microsoft(&'a MsOAuthConfig),
    Yandex(&'a YandexOAuthConfig),
}

fn refresh_provider<'a>(
    imap_host: Option<&str>,
    configs: &'a OAuthRefreshConfigs,
) -> Result<OAuthRefreshProvider<'a>, MsOAuthError> {
    if imap_host.is_some_and(is_yandex_mail_host) {
        return configs
            .yandex
            .as_ref()
            .map(OAuthRefreshProvider::Yandex)
            .ok_or(MsOAuthError::NotConfigured);
    }
    if imap_host.is_some_and(is_microsoft_mail_host) {
        return configs
            .microsoft
            .as_ref()
            .map(OAuthRefreshProvider::Microsoft)
            .ok_or(MsOAuthError::NotConfigured);
    }
    // Unknown or missing host: fail closed instead of guessing a provider —
    // redeeming a refresh token against the wrong client invalidates it.
    Err(MsOAuthError::NotConfigured)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::generate_key;

    #[test]
    fn oauth_token_roundtrip_under_dek() {
        let dek = generate_key();
        let tokens = OAuthTokenSet {
            access_token: "at".into(),
            refresh_token: "rt".into(),
            expires_at: 1_700_000_000,
            token_type: "Bearer".into(),
            scope: "imap".into(),
        };
        let blob = encrypt_oauth_tokens(&dek, &tokens).unwrap();
        let again = decrypt_oauth_tokens(&blob, &dek).unwrap();
        assert_eq!(again.access_token, "at");
        assert_eq!(again.refresh_token, "rt");
        assert_eq!(again.expires_at, 1_700_000_000);
    }
}
