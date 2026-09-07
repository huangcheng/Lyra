//! Cloudflare Turnstile captcha verification.

use serde::Deserialize;

const SITEVERIFY_URL: &str = "https://challenges.cloudflare.com/turnstile/v0/siteverify";

#[cfg(test)]
static TEST_SITEVERIFY_URL: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

#[cfg(test)]
pub fn set_test_siteverify_url(url: Option<String>) {
    *TEST_SITEVERIFY_URL.lock().unwrap() = url;
}

fn siteverify_url() -> String {
    #[cfg(test)]
    if let Some(url) = TEST_SITEVERIFY_URL.lock().unwrap().clone() {
        return url;
    }
    SITEVERIFY_URL.to_string()
}

#[derive(Debug)]
pub enum TurnstileError {
    Invalid,
    Unavailable,
}

#[derive(Debug, Deserialize)]
struct SiteverifyResponse {
    success: bool,
    #[serde(default)]
    #[allow(dead_code)]
    error_codes: Vec<String>,
}

/// Verify a Turnstile token against Cloudflare's siteverify API.
///
/// Never logs the secret or token.
pub async fn verify_turnstile(secret: &str, token: &str) -> Result<(), TurnstileError> {
    verify_turnstile_at(&siteverify_url(), secret, token).await
}

async fn verify_turnstile_at(url: &str, secret: &str, token: &str) -> Result<(), TurnstileError> {
    let client = reqwest::Client::new();
    let resp = client
        .post(url)
        .form(&[("secret", secret), ("response", token)])
        .send()
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "turnstile siteverify request failed");
            TurnstileError::Unavailable
        })?;

    if !resp.status().is_success() {
        tracing::warn!(status = %resp.status(), "turnstile siteverify non-success status");
        return Err(TurnstileError::Unavailable);
    }

    let body: SiteverifyResponse = resp.json().await.map_err(|e| {
        tracing::warn!(error = %e, "turnstile siteverify response parse failed");
        TurnstileError::Unavailable
    })?;

    if body.success {
        Ok(())
    } else {
        Err(TurnstileError::Invalid)
    }
}
