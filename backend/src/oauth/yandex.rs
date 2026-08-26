//! Yandex OAuth authorization-code + refresh for mail XOAUTH2.

use serde::Deserialize;

use super::exchange::ExchangedTokens;
use super::microsoft::{MsOAuthError, PkcePair};
use super::urlencode_component as urlencoding;

/// Yandex OAuth app registration settings (optional at boot).
#[derive(Debug, Clone)]
pub struct YandexOAuthConfig {
    pub client_id: String,
    pub client_secret: Option<String>,
    pub redirect_uri: String,
}

impl YandexOAuthConfig {
    pub fn from_client_env() -> Option<Self> {
        let client_id = std::env::var("LYRA_YANDEX_OAUTH_CLIENT_ID").ok()?;
        if client_id.trim().is_empty() {
            return None;
        }
        let client_secret = std::env::var("LYRA_YANDEX_OAUTH_CLIENT_SECRET")
            .ok()
            .filter(|s| !s.trim().is_empty());
        Some(Self {
            client_id: client_id.trim().into(),
            client_secret,
            redirect_uri: String::new(),
        })
    }
}

/// Scopes registered in the Yandex OAuth app (IMAP + SMTP + email).
pub const YANDEX_MAIL_SCOPES: &str = "login:email mail:imap_full mail:smtp";

/// Build the browser authorize URL (authorization code + PKCE S256).
///
/// PKCE makes the code unredeemable without the verifier even when the
/// deployment runs without a `client_secret` (public client).
pub fn build_authorize_url(
    cfg: &YandexOAuthConfig,
    state: &str,
    pkce: &PkcePair,
    login_hint: Option<&str>,
) -> String {
    let mut url = format!(
        "https://oauth.yandex.com/authorize?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&code_challenge={}&code_challenge_method=S256",
        urlencoding(&cfg.client_id),
        urlencoding(&cfg.redirect_uri),
        urlencoding(YANDEX_MAIL_SCOPES),
        urlencoding(state),
        urlencoding(&pkce.challenge),
    );
    if let Some(hint) = login_hint.filter(|e| e.contains('@')) {
        url.push_str("&login_hint=");
        url.push_str(&urlencoding(hint));
    }
    url
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    expires_in: i64,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

/// Exchange authorization `code` for tokens (PKCE verifier required).
pub async fn exchange_code(
    cfg: &YandexOAuthConfig,
    code: &str,
    pkce_verifier: &str,
) -> Result<ExchangedTokens, MsOAuthError> {
    let client = super::oauth_http_client();
    let mut form = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("client_id", cfg.client_id.as_str()),
        ("redirect_uri", cfg.redirect_uri.as_str()),
        ("code_verifier", pkce_verifier),
    ];
    if let Some(secret) = cfg.client_secret.as_deref() {
        form.push(("client_secret", secret));
    }

    let res = client
        .post("https://oauth.yandex.com/token")
        .form(&form)
        .send()
        .await
        .map_err(|e| MsOAuthError::TokenExchange(e.to_string()))?;
    let body: TokenResponse = res
        .json()
        .await
        .map_err(|e| MsOAuthError::TokenExchange(e.to_string()))?;
    if let Some(err) = body.error {
        return Err(MsOAuthError::TokenExchange(format!(
            "{err}: {}",
            body.error_description.unwrap_or_default()
        )));
    }
    let refresh = body
        .refresh_token
        .filter(|s| !s.is_empty())
        .ok_or_else(|| MsOAuthError::TokenExchange("no refresh_token".into()))?;
    let email = fetch_profile_email(&body.access_token).await.ok();
    Ok(ExchangedTokens {
        access_token: body.access_token,
        refresh_token: Some(refresh),
        expires_at: chrono::Utc::now().timestamp() + body.expires_in.max(60),
        scope: body.scope.or(Some(YANDEX_MAIL_SCOPES.into())),
        email,
    })
}

/// Refresh the access token; may rotate refresh_token.
pub async fn refresh_access_token(
    cfg: &YandexOAuthConfig,
    refresh_token: &str,
) -> Result<ExchangedTokens, MsOAuthError> {
    let client = super::oauth_http_client();
    let mut form = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", cfg.client_id.as_str()),
    ];
    if let Some(secret) = cfg.client_secret.as_deref() {
        form.push(("client_secret", secret));
    }

    let res = client
        .post("https://oauth.yandex.com/token")
        .form(&form)
        .send()
        .await
        .map_err(|e| MsOAuthError::TokenExchange(e.to_string()))?;
    let body: TokenResponse = res
        .json()
        .await
        .map_err(|e| MsOAuthError::TokenExchange(e.to_string()))?;
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

#[derive(Debug, Deserialize)]
struct ProfileResponse {
    #[serde(default)]
    default_email: Option<String>,
    #[serde(default)]
    login: Option<String>,
}

async fn fetch_profile_email(access_token: &str) -> Result<String, MsOAuthError> {
    let client = super::oauth_http_client();
    let res = client
        .get("https://login.yandex.ru/info?format=json")
        .header("Authorization", format!("OAuth {access_token}"))
        .send()
        .await
        .map_err(|e| MsOAuthError::TokenExchange(e.to_string()))?;
    let profile: ProfileResponse = res
        .json()
        .await
        .map_err(|e| MsOAuthError::TokenExchange(e.to_string()))?;
    profile
        .default_email
        .or(profile.login.map(|l| format!("{l}@yandex.ru")))
        .filter(|e| e.contains('@'))
        .ok_or_else(|| MsOAuthError::TokenExchange("missing email in profile".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oauth::microsoft::generate_pkce;

    #[test]
    fn authorize_url_contains_scopes_pkce_and_login_hint() {
        let cfg = YandexOAuthConfig {
            client_id: "cid".into(),
            client_secret: Some("sec".into()),
            redirect_uri: "http://localhost:3000/api/v1/oauth/callback".into(),
        };
        let pkce = generate_pkce();
        let url = build_authorize_url(&cfg, "st", &pkce, Some("user@yandex.ru"));
        assert!(url.contains("oauth.yandex.com/authorize"));
        assert!(url.contains("mail%3Aimap_full"));
        assert!(url.contains(&format!("code_challenge={}", urlencoding(&pkce.challenge))));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("login_hint=user%40yandex.ru"));
    }
}
