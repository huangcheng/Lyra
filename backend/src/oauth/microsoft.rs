//! Microsoft identity platform (v2) authorization-code + refresh for mail XOAUTH2.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Azure / Entra app registration settings (optional at boot).
#[derive(Debug, Clone)]
pub struct MsOAuthConfig {
    pub client_id: String,
    /// Confidential clients set a secret; public + PKCE may leave this empty.
    pub client_secret: Option<String>,
    pub redirect_uri: String,
    /// Tenant id or `common` / `organizations` / `consumers`.
    pub tenant: String,
}

#[derive(Debug, Error)]
pub enum MsOAuthError {
    #[error("Microsoft OAuth is not configured on this server")]
    NotConfigured,
    #[error("OAuth provider not supported: {0}")]
    UnknownProvider(String),
    #[error("OAuth provider is not configured on this server: {0}")]
    ProviderNotConfigured(String),
    #[error("invalid OAuth state")]
    InvalidState,
    #[error("token exchange failed: {0}")]
    TokenExchange(String),
    #[error("mailbox email is required to start OAuth")]
    MissingEmailParam,
    #[error("missing email in OAuth profile")]
    MissingEmail,
    #[error("stored mail credential could not be decrypted")]
    CredentialDecrypt,
    #[error("{0}")]
    Internal(String),
}

impl MsOAuthError {
    /// True when the account's stored credential itself is unreadable — the
    /// only failure class that justifies deactivating the account. Token
    /// endpoint outages, network errors, and missing server config are
    /// transient from the account's perspective and must stay retryable.
    pub fn is_credential_decrypt(&self) -> bool {
        matches!(self, Self::CredentialDecrypt)
    }
}

impl MsOAuthConfig {
    /// Load Microsoft OAuth client credentials from the environment.
    ///
    /// `redirect_uri` is left empty — sufficient for token refresh in sync/send
    /// workers. HTTP handlers should call [`Self::with_redirect`].
    pub fn from_client_env() -> Option<Self> {
        let client_id = std::env::var("LYRA_MS_OAUTH_CLIENT_ID").ok()?;
        if client_id.trim().is_empty() {
            return None;
        }
        let client_secret = std::env::var("LYRA_MS_OAUTH_CLIENT_SECRET")
            .ok()
            .filter(|s| !s.trim().is_empty());
        let tenant = std::env::var("LYRA_MS_OAUTH_TENANT")
            .unwrap_or_else(|_| "common".into())
            .trim()
            .to_string();
        Some(Self {
            client_id: client_id.trim().into(),
            client_secret,
            redirect_uri: String::new(),
            tenant: if tenant.is_empty() {
                "common".into()
            } else {
                tenant
            },
        })
    }

    fn authorize_url_base(&self) -> String {
        format!(
            "https://login.microsoftonline.com/{}/oauth2/v2.0/authorize",
            self.tenant
        )
    }

    fn token_url(&self) -> String {
        format!(
            "https://login.microsoftonline.com/{}/oauth2/v2.0/token",
            self.tenant
        )
    }
}

/// Scopes for IMAP + SMTP XOAUTH2 (Exchange Online).
pub const MS_MAIL_SCOPES: &str = "openid offline_access email profile \
https://outlook.office.com/IMAP.AccessAsUser.All \
https://outlook.office.com/SMTP.Send";

#[derive(Debug, Clone)]
pub struct PkcePair {
    pub verifier: String,
    pub challenge: String,
}

pub fn generate_pkce() -> PkcePair {
    let mut raw = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut raw);
    let verifier = URL_SAFE_NO_PAD.encode(raw);
    let digest = Sha256::digest(verifier.as_bytes());
    let challenge = URL_SAFE_NO_PAD.encode(digest);
    PkcePair {
        verifier,
        challenge,
    }
}

pub fn generate_state() -> String {
    let mut raw = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut raw);
    URL_SAFE_NO_PAD.encode(raw)
}

/// Build the browser authorize URL (authorization code + PKCE).
pub fn build_authorize_url(cfg: &MsOAuthConfig, state: &str, pkce: &PkcePair) -> String {
    let mut url = format!(
        "{}?client_id={}&response_type=code&redirect_uri={}&response_mode=query&scope={}&state={}&code_challenge={}&code_challenge_method=S256",
        cfg.authorize_url_base(),
        urlencoding(&cfg.client_id),
        urlencoding(&cfg.redirect_uri),
        urlencoding(MS_MAIL_SCOPES),
        urlencoding(state),
        urlencoding(&pkce.challenge),
    );
    if cfg.client_secret.is_none() {
        // Public client hint (optional).
        url.push_str("&prompt=select_account");
    }
    url
}

fn urlencoding(s: &str) -> String {
    super::urlencode_component(s)
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    #[serde(default)]
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: i64,
    #[serde(default)]
    #[allow(dead_code)]
    token_type: String,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

use super::exchange::ExchangedTokens;

async fn post_token_form(
    client: &reqwest::Client,
    url: String,
    form: &[(&str, &str)],
) -> Result<TokenResponse, MsOAuthError> {
    let res = client
        .post(url)
        .form(form)
        .send()
        .await
        .map_err(|e| MsOAuthError::TokenExchange(e.to_string()))?;
    let status = res.status();
    let text = res
        .text()
        .await
        .map_err(|e| MsOAuthError::TokenExchange(e.to_string()))?;
    serde_json::from_str(&text)
        .map_err(|e| MsOAuthError::TokenExchange(format!("decode response (HTTP {status}): {e}")))
}

/// Exchange authorization `code` for tokens.
pub async fn exchange_code(
    cfg: &MsOAuthConfig,
    code: &str,
    pkce_verifier: &str,
) -> Result<ExchangedTokens, MsOAuthError> {
    let client = super::oauth_http_client();
    let mut form = vec![
        ("client_id", cfg.client_id.as_str()),
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", cfg.redirect_uri.as_str()),
        ("code_verifier", pkce_verifier),
        ("scope", MS_MAIL_SCOPES),
    ];
    if let Some(secret) = cfg.client_secret.as_deref() {
        form.push(("client_secret", secret));
    }

    let body = post_token_form(&client, cfg.token_url(), &form).await?;
    if let Some(err) = body.error {
        return Err(MsOAuthError::TokenExchange(format!(
            "{err}: {}",
            body.error_description.unwrap_or_default()
        )));
    }
    let refresh = body
        .refresh_token
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            MsOAuthError::TokenExchange("no refresh_token; ensure offline_access scope".into())
        })?;
    let email = body
        .id_token
        .as_deref()
        .and_then(email_from_id_token)
        .or_else(|| {
            body.id_token
                .as_deref()
                .and_then(preferred_username_from_id_token)
        });
    Ok(ExchangedTokens {
        access_token: body.access_token,
        refresh_token: Some(refresh),
        expires_at: chrono::Utc::now().timestamp() + body.expires_in.max(60),
        scope: body.scope.or(Some(MS_MAIL_SCOPES.into())),
        email,
    })
}

/// Refresh the access token; may rotate refresh_token.
pub async fn refresh_access_token(
    cfg: &MsOAuthConfig,
    refresh_token: &str,
) -> Result<ExchangedTokens, MsOAuthError> {
    let client = super::oauth_http_client();
    let mut form = vec![
        ("client_id", cfg.client_id.as_str()),
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("scope", MS_MAIL_SCOPES),
    ];
    if let Some(secret) = cfg.client_secret.as_deref() {
        form.push(("client_secret", secret));
    }

    let body = post_token_form(&client, cfg.token_url(), &form).await?;
    if let Some(err) = body.error {
        return Err(MsOAuthError::TokenExchange(format!(
            "{err}: {}",
            body.error_description.unwrap_or_default()
        )));
    }
    Ok(ExchangedTokens {
        access_token: body.access_token,
        refresh_token: body.refresh_token.filter(|s| !s.is_empty()),
        expires_at: chrono::Utc::now().timestamp() + body.expires_in.max(60),
        scope: body.scope,
        email: None,
    })
}

fn email_from_id_token(id_token: &str) -> Option<String> {
    claim_from_jwt(id_token, "email")
}

fn preferred_username_from_id_token(id_token: &str) -> Option<String> {
    claim_from_jwt(id_token, "preferred_username").or_else(|| claim_from_jwt(id_token, "upn"))
}

/// Decode JWT payload without signature verification (token already from MS HTTPS).
fn claim_from_jwt(jwt: &str, claim: &str) -> Option<String> {
    let payload = jwt.split('.').nth(1)?;
    let padded = match payload.len() % 4 {
        2 => format!("{payload}=="),
        3 => format!("{payload}="),
        _ => payload.to_string(),
    };
    let bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(&padded))
        .or_else(|_| base64::engine::general_purpose::STANDARD.decode(&padded))
        .ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    v.get(claim)?.as_str().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_verifier_is_url_safe() {
        let p = generate_pkce();
        assert!(!p.verifier.is_empty());
        assert!(!p.challenge.is_empty());
        assert_ne!(p.verifier, p.challenge);
    }

    #[test]
    fn authorize_url_contains_pkce() {
        let cfg = MsOAuthConfig {
            client_id: "cid".into(),
            client_secret: Some("sec".into()),
            redirect_uri: "http://localhost:3000/api/v1/oauth/callback".into(),
            tenant: "common".into(),
        };
        let pkce = generate_pkce();
        let url = build_authorize_url(&cfg, "st", &pkce);
        assert!(url.contains("code_challenge="));
        assert!(url.contains("login.microsoftonline.com/common"));
        assert!(url.contains("IMAP.AccessAsUser.All"));
    }

    #[test]
    fn claim_from_fake_jwt() {
        // {"email":"a@example.com"} base64url
        let payload = URL_SAFE_NO_PAD.encode(br#"{"email":"a@example.com"}"#);
        let jwt = format!("hdr.{payload}.sig");
        assert_eq!(email_from_id_token(&jwt).as_deref(), Some("a@example.com"));
    }
}
