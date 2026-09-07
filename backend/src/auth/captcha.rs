//! Login/bootstrap captcha verification.
//!
//! Supported providers: Cloudflare Turnstile, hCaptcha, Google reCAPTCHA.
//! All three expose a siteverify-style endpoint accepting form fields
//! `secret` + `response` and returning a JSON body with a `success` flag.
//!
//! The active provider comes from the Settings page (kv-backed, secret
//! encrypted under the master key); `LYRA_CAPTCHA_*` env vars are the
//! fallback when no setting has been saved.

use serde::{Deserialize, Serialize};

use crate::config::CaptchaConfig;

const TURNSTILE_SITEVERIFY_URL: &str = "https://challenges.cloudflare.com/turnstile/v0/siteverify";
const HCAPTCHA_SITEVERIFY_URL: &str = "https://api.hcaptcha.com/siteverify";
const RECAPTCHA_SITEVERIFY_URL: &str = "https://www.google.com/recaptcha/api/siteverify";

#[cfg(test)]
static TEST_SITEVERIFY_URLS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<&'static str, String>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

#[cfg(test)]
pub fn set_test_siteverify_url(provider: &'static str, url: Option<String>) {
    let mut urls = TEST_SITEVERIFY_URLS.lock().unwrap();
    match url {
        Some(url) => {
            urls.insert(provider, url);
        }
        None => {
            urls.remove(provider);
        }
    }
}

#[cfg_attr(not(test), allow(unused_variables))]
fn siteverify_url(provider: &'static str, default: &str) -> String {
    #[cfg(test)]
    if let Some(url) = TEST_SITEVERIFY_URLS.lock().unwrap().get(provider) {
        return url.clone();
    }
    default.to_string()
}

#[derive(Debug)]
pub enum CaptchaError {
    Invalid,
    Unavailable,
}

#[derive(Debug, Deserialize)]
struct SiteverifyResponse {
    success: bool,
    /// reCAPTCHA v3 only: 1.0 is very likely human, 0.0 very likely a bot.
    #[serde(default)]
    score: Option<f64>,
    #[serde(default)]
    #[allow(dead_code)]
    error_codes: Vec<String>,
}

/// Minimum reCAPTCHA v3 score accepted as human (Google's default guidance).
const RECAPTCHA_V3_MIN_SCORE: f64 = 0.5;

/// Verify a captcha token against the configured provider's siteverify API.
///
/// Never logs the secret or token.
pub async fn verify_captcha(captcha: &CaptchaConfig, token: &str) -> Result<(), CaptchaError> {
    let (provider, default_url, secret) = match captcha {
        CaptchaConfig::None => return Ok(()),
        CaptchaConfig::Turnstile { secret, .. } => ("turnstile", TURNSTILE_SITEVERIFY_URL, secret),
        CaptchaConfig::HCaptcha { secret, .. } => ("hcaptcha", HCAPTCHA_SITEVERIFY_URL, secret),
        CaptchaConfig::Recaptcha { secret, .. } => ("recaptcha", RECAPTCHA_SITEVERIFY_URL, secret),
        CaptchaConfig::RecaptchaV3 { secret, .. } => {
            ("recaptcha-v3", RECAPTCHA_SITEVERIFY_URL, secret)
        }
    };
    let body = verify_at(
        &siteverify_url(provider, default_url),
        provider,
        secret,
        token,
    )
    .await?;
    // v3 is score-based: `success` alone only means the token was valid.
    if matches!(captcha, CaptchaConfig::RecaptchaV3 { .. }) {
        match body.score {
            Some(score) if score >= RECAPTCHA_V3_MIN_SCORE => {}
            score => {
                tracing::warn!(?score, "reCAPTCHA v3 score below threshold");
                return Err(CaptchaError::Invalid);
            }
        }
    }
    Ok(())
}

async fn verify_at(
    url: &str,
    provider: &str,
    secret: &str,
    token: &str,
) -> Result<SiteverifyResponse, CaptchaError> {
    let client = reqwest::Client::new();
    let resp = client
        .post(url)
        .form(&[("secret", secret), ("response", token)])
        .send()
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, provider, "captcha siteverify request failed");
            CaptchaError::Unavailable
        })?;

    if !resp.status().is_success() {
        tracing::warn!(status = %resp.status(), provider, "captcha siteverify non-success status");
        return Err(CaptchaError::Unavailable);
    }

    let body: SiteverifyResponse = resp.json().await.map_err(|e| {
        tracing::warn!(error = %e, provider, "captcha siteverify response parse failed");
        CaptchaError::Unavailable
    })?;

    if body.success {
        Ok(body)
    } else {
        Err(CaptchaError::Invalid)
    }
}

// ---- Server-wide settings (Settings page; kv-backed, env is the fallback) ----

const SETTINGS_KV_KEY: &str = "server:captcha-settings";

/// One provider's stored site key + encrypted secret.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct StoredPair {
    #[serde(default)]
    site_key: String,
    #[serde(default)]
    secret_encrypted: Option<crate::crypto::EncryptedCredential>,
}

/// Persisted form of the captcha settings. Each provider keeps its own
/// site-key/secret pair so switching providers never discards credentials;
/// `active` picks which pair protects login. Secrets are AES-256-GCM
/// encrypted under the master key before they touch kv.
#[derive(Debug, Default, Serialize, Deserialize)]
struct StoredSettings {
    #[serde(default)]
    active: String,
    #[serde(default)]
    providers: std::collections::BTreeMap<String, StoredPair>,
    // Legacy v1 single-config fields: migrated into `providers` on load,
    // never written back.
    #[serde(default, skip_serializing)]
    provider: Option<String>,
    #[serde(default, skip_serializing)]
    site_key: Option<String>,
    #[serde(default, skip_serializing)]
    secret_encrypted: Option<crate::crypto::EncryptedCredential>,
}

impl StoredSettings {
    /// Fold legacy v1 fields into the per-provider map and default `active`.
    fn normalize(mut self) -> Self {
        if let Some(legacy) = self.provider.take() {
            if self.active.is_empty() {
                self.active.clone_from(&legacy);
            }
            let has_pair = self.site_key.as_deref().is_some_and(|s| !s.is_empty())
                || self.secret_encrypted.is_some();
            if legacy != "none" && has_pair && !self.providers.contains_key(&legacy) {
                self.providers.insert(
                    legacy,
                    StoredPair {
                        site_key: self.site_key.take().unwrap_or_default(),
                        secret_encrypted: self.secret_encrypted.take(),
                    },
                );
            }
        }
        self.site_key = None;
        self.secret_encrypted = None;
        if self.active.is_empty() {
            self.active = "none".to_string();
        }
        self
    }
}

/// Normalize a provider name; `None` for anything unrecognized.
pub fn parse_provider(provider: &str) -> Option<&'static str> {
    match provider.trim().to_ascii_lowercase().as_str() {
        "none" => Some("none"),
        "turnstile" => Some("turnstile"),
        "hcaptcha" => Some("hcaptcha"),
        "recaptcha" => Some("recaptcha"),
        "recaptcha-v3" | "recaptcha_v3" => Some("recaptcha-v3"),
        _ => None,
    }
}

fn config_from_parts(
    provider: &str,
    site_key: String,
    secret: Option<String>,
) -> Result<CaptchaConfig, crate::crypto::CryptoError> {
    match provider {
        "none" => Ok(CaptchaConfig::None),
        "turnstile" | "hcaptcha" | "recaptcha" | "recaptcha-v3" => {
            let secret = secret.ok_or_else(|| {
                crate::crypto::CryptoError::Storage(format!(
                    "captcha settings for {provider} are missing the secret"
                ))
            })?;
            let config = match provider {
                "turnstile" => CaptchaConfig::Turnstile { site_key, secret },
                "hcaptcha" => CaptchaConfig::HCaptcha { site_key, secret },
                "recaptcha" => CaptchaConfig::Recaptcha { site_key, secret },
                _ => CaptchaConfig::RecaptchaV3 { site_key, secret },
            };
            Ok(config)
        }
        other => Err(crate::crypto::CryptoError::Storage(format!(
            "unknown captcha provider in settings: {other}"
        ))),
    }
}

/// Settings encryption key: HKDF-derived from the master key, same pattern as
/// per-user KEKs (`crypto::derive_user_kek`).
fn settings_key() -> Result<[u8; 32], crate::crypto::CryptoError> {
    Ok(crate::crypto::derive_user_kek(
        super::dek::master_key()?,
        "server:captcha-settings",
    ))
}

fn decrypt_pair(pair: &StoredPair) -> Result<(String, Option<String>), String> {
    let secret = match &pair.secret_encrypted {
        None => None,
        Some(enc) => {
            let bytes = crate::crypto::decrypt(&settings_key().map_err(|e| e.to_string())?, enc)
                .map_err(|e| e.to_string())?;
            Some(String::from_utf8(bytes).map_err(|e| e.to_string())?)
        }
    };
    Ok((pair.site_key.clone(), secret))
}

async fn read_blob(
    kv: &std::sync::Arc<dyn crate::kv::KvStore>,
) -> Result<Option<StoredSettings>, String> {
    let raw = kv.get(SETTINGS_KV_KEY).await.map_err(|e| e.to_string())?;
    match raw {
        None => Ok(None),
        Some(json) => {
            let stored: StoredSettings = serde_json::from_str(&json)
                .map_err(|e| format!("captcha settings corrupt: {e}"))?;
            Ok(Some(stored.normalize()))
        }
    }
}

/// The active captcha config saved on the Settings page, if any. The active
/// provider's pair must be complete (site key + secret) to count.
pub async fn load_stored(
    kv: &std::sync::Arc<dyn crate::kv::KvStore>,
) -> Result<Option<CaptchaConfig>, String> {
    let Some(stored) = read_blob(kv).await? else {
        return Ok(None);
    };
    if stored.active == "none" {
        return Ok(Some(CaptchaConfig::None));
    }
    let Some(pair) = stored.providers.get(&stored.active) else {
        return Ok(Some(CaptchaConfig::None));
    };
    let (site_key, secret) = decrypt_pair(pair)?;
    if site_key.is_empty() || secret.is_none() {
        tracing::warn!(
            provider = %stored.active,
            "active captcha provider has an incomplete key pair; captcha disabled"
        );
        return Ok(Some(CaptchaConfig::None));
    }
    config_from_parts(&stored.active, site_key, secret)
        .map(Some)
        .map_err(|e| e.to_string())
}

/// Public view of one provider's saved pair (never includes the secret).
#[derive(Debug, Clone)]
pub struct PairView {
    pub site_key: String,
    pub has_secret: bool,
}

/// Settings-page view: the active provider plus every known pair. When a
/// setting was saved (`source == "settings"`), the env pair is merged in for
/// providers the setting does not cover, so the UI can switch back to it.
pub struct SettingsView {
    pub active: String,
    pub providers: Vec<(String, PairView)>,
    pub source: &'static str,
}

fn env_pair(env_config: &CaptchaConfig) -> Option<(String, PairView)> {
    let (site_key, secret) = env_config.credentials();
    Some((
        env_config.provider_name().to_string(),
        PairView {
            site_key: site_key?.to_string(),
            has_secret: secret.is_some(),
        },
    ))
}

pub async fn load_view(
    kv: &std::sync::Arc<dyn crate::kv::KvStore>,
    env_config: &CaptchaConfig,
) -> SettingsView {
    match read_blob(kv).await {
        Ok(Some(stored)) => {
            let mut providers: std::collections::BTreeMap<String, PairView> =
                std::collections::BTreeMap::new();
            for (name, pair) in &stored.providers {
                match decrypt_pair(pair) {
                    Ok((site_key, secret)) => {
                        providers.insert(
                            name.clone(),
                            PairView {
                                site_key,
                                has_secret: secret.is_some(),
                            },
                        );
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, provider = %name, "captcha pair unreadable");
                    }
                }
            }
            if let Some((name, view)) = env_pair(env_config) {
                providers.entry(name).or_insert(view);
            }
            SettingsView {
                active: stored.active,
                providers: providers.into_iter().collect(),
                source: "settings",
            }
        }
        Ok(None) => SettingsView {
            active: env_config.provider_name().to_string(),
            providers: env_pair(env_config).into_iter().collect(),
            source: "env",
        },
        Err(e) => {
            tracing::warn!(error = %e, "captcha settings unreadable; falling back to env config");
            SettingsView {
                active: env_config.provider_name().to_string(),
                providers: env_pair(env_config).into_iter().collect(),
                source: "env",
            }
        }
    }
}

/// One provider-pair update from the Settings page: `None` removes the pair.
/// Within an update, an omitted field keeps the stored value.
#[derive(Debug, Default)]
pub struct PairUpdate {
    pub site_key: Option<String>,
    pub secret: Option<String>,
}

/// Why a save was rejected (storage errors are strings for logging).
#[derive(Debug)]
pub enum SaveSettingsError {
    /// The active provider has no complete pair (kv or env fallback).
    IncompleteActive,
    Store(String),
}

/// Persist a Settings-page change: set `active`, then merge `updates` into
/// the stored pairs. Validates that the resulting active provider resolves
/// to a complete pair (from the settings or the env fallback) before writing.
pub async fn save_settings(
    kv: &std::sync::Arc<dyn crate::kv::KvStore>,
    env_config: &CaptchaConfig,
    active: &str,
    updates: Vec<(String, Option<PairUpdate>)>,
) -> Result<(), SaveSettingsError> {
    let mut stored = read_blob(kv)
        .await
        .map_err(SaveSettingsError::Store)?
        .unwrap_or_default();
    stored.active = active.to_string();

    for (provider, update) in updates {
        match update {
            None => {
                stored.providers.remove(&provider);
            }
            Some(update) => {
                let pair = stored.providers.entry(provider).or_default();
                if let Some(site_key) = update.site_key {
                    pair.site_key = site_key.trim().to_string();
                }
                if let Some(secret) = update.secret.map(|s| s.trim().to_string())
                    && !secret.is_empty()
                {
                    pair.secret_encrypted = Some(
                        crate::crypto::encrypt(
                            &settings_key().map_err(|e| SaveSettingsError::Store(e.to_string()))?,
                            secret.as_bytes(),
                        )
                        .map_err(|e| SaveSettingsError::Store(e.to_string()))?,
                    );
                }
            }
        }
    }

    // The active provider must resolve to a complete pair, either from the
    // settings themselves or from the env config for the same provider.
    if active != "none" {
        let complete_in_settings = match stored.providers.get(active) {
            Some(pair) => {
                let (site_key, secret) = decrypt_pair(pair).map_err(SaveSettingsError::Store)?;
                !site_key.is_empty() && secret.is_some()
            }
            None => false,
        };
        let complete_in_env = env_config.provider_name() == active
            && matches!(env_config.credentials(), (Some(_), Some(_)));
        if !complete_in_settings && !complete_in_env {
            return Err(SaveSettingsError::IncompleteActive);
        }
    }

    let json =
        serde_json::to_string(&stored).map_err(|e| SaveSettingsError::Store(e.to_string()))?;
    kv.set(SETTINGS_KV_KEY, &json, None)
        .await
        .map_err(|e| SaveSettingsError::Store(e.to_string()))
}

/// Effective captcha config: the Settings-page value when saved, else the
/// `LYRA_CAPTCHA_*` env config. Returns the config and where it came from
/// (`"settings"` or `"env"`). kv failures fall back to env with a warning.
/// When the setting is saved but its active pair is incomplete, the env
/// config for the same provider fills the gap.
pub async fn load_effective(
    kv: &std::sync::Arc<dyn crate::kv::KvStore>,
    env_config: &CaptchaConfig,
) -> (CaptchaConfig, &'static str) {
    match load_stored(kv).await {
        Ok(Some(config)) => {
            // Saved but incomplete active pair → env may still cover it.
            if matches!(config, CaptchaConfig::None)
                && let Ok(Some(stored)) = read_blob(kv).await
                && stored.active != "none"
                && env_config.provider_name() == stored.active
                && matches!(env_config.credentials(), (Some(_), Some(_)))
            {
                return (env_config.clone(), "settings");
            }
            (config, "settings")
        }
        Ok(None) => (env_config.clone(), "env"),
        Err(e) => {
            tracing::warn!(error = %e, "captcha settings unreadable; falling back to env config");
            (env_config.clone(), "env")
        }
    }
}
