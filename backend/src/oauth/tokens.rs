//! Encrypted OAuth token blob + access-secret resolution for sync/send.

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use super::microsoft::{MsOAuthConfig, MsOAuthError, refresh_access_token};
use crate::crypto::{self, EncryptedCredential};
use crate::db_row::id_param;
use crate::storage::DbPool;

/// Refresh when the access token expires within this window.
const REFRESH_SKEW_SECS: i64 = 120;

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
    let encrypted: EncryptedCredential = serde_json::from_str(credential_json)
        .map_err(|e| MsOAuthError::Internal(format!("invalid credential blob: {e}")))?;
    let bytes =
        crypto::decrypt(dek, &encrypted).map_err(|e| MsOAuthError::Internal(e.to_string()))?;
    let json = String::from_utf8(bytes)
        .map_err(|e| MsOAuthError::Internal(format!("token utf-8: {e}")))?;
    serde_json::from_str(&json).map_err(|e| MsOAuthError::Internal(format!("token json: {e}")))
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

/// Decrypt account credential; refresh OAuth access token when near expiry.
pub async fn resolve_mail_access_secret(
    db: &DbPool,
    account_id: &str,
    auth_type: &str,
    credential_json: &str,
    dek: &[u8],
    ms: Option<&MsOAuthConfig>,
) -> Result<MailAccessSecret, MsOAuthError> {
    if auth_type == "oauth2" {
        let mut tokens = decrypt_oauth_tokens(credential_json, dek)?;
        let now = chrono::Utc::now().timestamp();
        if tokens.expires_at - now <= REFRESH_SKEW_SECS {
            let Some(cfg) = ms else {
                return Err(MsOAuthError::NotConfigured);
            };
            let refreshed = refresh_access_token(cfg, &tokens.refresh_token).await?;
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
