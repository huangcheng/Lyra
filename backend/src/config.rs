//! Environment-based configuration.
//!
//! Most values come from environment variables with sensible defaults.
//! `LYRA_MASTER_KEY` is required — boot fails closed without it.
//! No secrets are stored in the tree; credentials are loaded at runtime.

use std::env;

/// Application configuration loaded from environment variables.
#[derive(Debug, Clone)]
pub struct Config {
    /// Address to listen on (e.g. "0.0.0.0:3000").
    pub listen_addr: String,
    /// Database connection URL (`sqlite:///path` or `postgres://...`).
    #[allow(dead_code)]
    pub database_url: String,
    /// Directory for storing message blobs and attachments.
    #[allow(dead_code)]
    pub data_dir: String,
    /// Minimum password length (default: 8).
    pub min_password_length: usize,
    /// Max concurrent mailbox syncs (`SYNC_MAX_CONCURRENT`, default 3).
    pub sync_max_concurrent: usize,
    /// Seconds between active-account poll ticks (`SYNC_POLL_SECS`, default 300).
    pub sync_poll_secs: u64,
    /// Per-outgoing-attachment byte cap (`LYRA_MAX_ATTACHMENT_BYTES`, default 25 MiB).
    pub max_attachment_bytes: u64,
    /// Redis URL for session/kv store (`REDIS_URL`). When unset, boot uses in-memory kv.
    pub redis_url: Option<String>,
    /// Master key for the per-user DEK hierarchy (`LYRA_MASTER_KEY`, 32+ bytes).
    /// Required: the backend refuses to start without it. Never logged.
    pub master_key: Vec<u8>,
    /// Optional Microsoft mail OAuth (Outlook / M365 XOAUTH2).
    pub ms_oauth: Option<crate::oauth::MsOAuthConfig>,
    /// Optional Yandex mail OAuth (IMAP/SMTP XOAUTH2).
    pub yandex_oauth: Option<crate::oauth::YandexOAuthConfig>,
    /// Optional login/bootstrap captcha (off by default).
    pub captcha: CaptchaConfig,
    /// VAPID `sub` contact for Web Push (RFC 8292). Defaults to
    /// `mailto:admin@<LYRA_PUBLIC_URL host>`.
    pub vapid_subject: String,
}

/// Captcha provider configuration (`LYRA_CAPTCHA_*` env vars).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptchaConfig {
    None,
    Turnstile {
        site_key: String,
        secret: String,
    },
    HCaptcha {
        site_key: String,
        secret: String,
    },
    Recaptcha {
        site_key: String,
        secret: String,
    },
    /// reCAPTCHA v3: invisible, score-based (no checkbox challenge).
    RecaptchaV3 {
        site_key: String,
        secret: String,
    },
}

impl CaptchaConfig {
    /// Public site-facing slice (never includes the secret).
    pub fn public(&self) -> Option<CaptchaPublicConfig> {
        let (provider, site_key) = match self {
            Self::None => return None,
            Self::Turnstile { site_key, .. } => ("turnstile", site_key),
            Self::HCaptcha { site_key, .. } => ("hcaptcha", site_key),
            Self::Recaptcha { site_key, .. } => ("recaptcha", site_key),
            Self::RecaptchaV3 { site_key, .. } => ("recaptcha-v3", site_key),
        };
        Some(CaptchaPublicConfig {
            provider: provider.to_string(),
            site_key: site_key.clone(),
        })
    }

    /// Provider slug: `"none"`, `"turnstile"`, `"hcaptcha"`, `"recaptcha"`,
    /// or `"recaptcha-v3"`.
    pub fn provider_name(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Turnstile { .. } => "turnstile",
            Self::HCaptcha { .. } => "hcaptcha",
            Self::Recaptcha { .. } => "recaptcha",
            Self::RecaptchaV3 { .. } => "recaptcha-v3",
        }
    }

    /// `(site_key, secret)` for provider variants; both `None` when disabled.
    pub fn credentials(&self) -> (Option<&str>, Option<&str>) {
        match self {
            Self::None => (None, None),
            Self::Turnstile { site_key, secret }
            | Self::HCaptcha { site_key, secret }
            | Self::Recaptcha { site_key, secret }
            | Self::RecaptchaV3 { site_key, secret } => {
                (Some(site_key.as_str()), Some(secret.as_str()))
            }
        }
    }
}

/// Captcha settings exposed on `GET /api/v1/auth/status`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptchaPublicConfig {
    pub provider: String,
    pub site_key: String,
}

/// Configuration error; boot fails closed on any variant.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error(
        "LYRA_MASTER_KEY is not set; refusing to start. \
         Generate one with `openssl rand -base64 32` and set it in the environment or .env \
         (see .env.example)."
    )]
    MasterKeyMissing,
    #[error(
        "LYRA_MASTER_KEY is too short ({0} bytes; need at least 32). \
         Generate one with `openssl rand -base64 32`."
    )]
    MasterKeyTooShort(usize),
    #[error(
        "LYRA_PUBLIC_URL is not set; refusing to start. \
         Set the URL users type in the browser, e.g. http://localhost:3000 or https://mail.example.com \
         (see .env.example)."
    )]
    PublicUrlMissing,
    #[error("LYRA_PUBLIC_URL must start with http:// or https:// (got {0:?})")]
    PublicUrlInvalid(String),
    #[error("mail OAuth provider config is invalid: {0}")]
    OauthConfig(String),
    #[error(
        "LYRA_CAPTCHA_PROVIDER must be \"none\", \"turnstile\", \"hcaptcha\", \"recaptcha\", or \"recaptcha-v3\" (got {0:?})"
    )]
    CaptchaProviderUnknown(String),
    #[error("LYRA_CAPTCHA_PROVIDER={0} requires LYRA_CAPTCHA_SITE_KEY and LYRA_CAPTCHA_SECRET")]
    CaptchaIncomplete(String),
    #[error("unused captcha env vars detected ({0}); configure captcha via LYRA_CAPTCHA_*")]
    CaptchaMisconfigured(String),
}

fn normalize_public_url(raw: &str) -> Result<String, ConfigError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ConfigError::PublicUrlMissing);
    }
    if !trimmed.starts_with("http://") && !trimmed.starts_with("https://") {
        return Err(ConfigError::PublicUrlInvalid(trimmed.to_string()));
    }
    Ok(trimmed.trim_end_matches('/').to_string())
}

fn captcha_from_env() -> Result<CaptchaConfig, ConfigError> {
    for prefix in ["LYRA_HCAPTCHA_", "LYRA_RECAPTCHA_"] {
        if env::vars().any(|(k, v)| k.starts_with(prefix) && !v.is_empty()) {
            return Err(ConfigError::CaptchaMisconfigured(
                prefix.trim_end_matches('_').to_string(),
            ));
        }
    }

    let raw = env::var("LYRA_CAPTCHA_PROVIDER").unwrap_or_default();
    let provider = raw.trim();
    if provider.is_empty() || provider.eq_ignore_ascii_case("none") {
        return Ok(CaptchaConfig::None);
    }

    let keys = || -> Result<(String, String), ConfigError> {
        let site_key = env::var("LYRA_CAPTCHA_SITE_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| ConfigError::CaptchaIncomplete(provider.to_string()))?;
        let secret = env::var("LYRA_CAPTCHA_SECRET")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| ConfigError::CaptchaIncomplete(provider.to_string()))?;
        Ok((site_key.trim().to_string(), secret.trim().to_string()))
    };

    match provider.to_ascii_lowercase().as_str() {
        "turnstile" => {
            let (site_key, secret) = keys()?;
            Ok(CaptchaConfig::Turnstile { site_key, secret })
        }
        "hcaptcha" => {
            let (site_key, secret) = keys()?;
            Ok(CaptchaConfig::HCaptcha { site_key, secret })
        }
        "recaptcha" => {
            let (site_key, secret) = keys()?;
            Ok(CaptchaConfig::Recaptcha { site_key, secret })
        }
        "recaptcha-v3" | "recaptcha_v3" => {
            let (site_key, secret) = keys()?;
            Ok(CaptchaConfig::RecaptchaV3 { site_key, secret })
        }
        _ => Err(ConfigError::CaptchaProviderUnknown(provider.to_string())),
    }
}

/// Load and validate the master key from `LYRA_MASTER_KEY`.
fn master_key_from_env() -> Result<Vec<u8>, ConfigError> {
    let raw = env::var("LYRA_MASTER_KEY").map_err(|_| ConfigError::MasterKeyMissing)?;
    // Used as raw bytes (no hex/base64 decoding) — encoding-agnostic by design.
    let bytes = raw.into_bytes();
    if bytes.len() < 32 {
        return Err(ConfigError::MasterKeyTooShort(bytes.len()));
    }
    Ok(bytes)
}

impl Config {
    /// Load configuration from environment variables.
    ///
    /// Required:
    ///   - `LYRA_MASTER_KEY` — master key for the per-user DEK hierarchy (32+ bytes)
    ///
    /// Optional (with defaults):
    ///   - `LISTEN_ADDR` — default `0.0.0.0:3000`
    ///   - `DATABASE_URL` — default `sqlite:./data/lyra.db`
    ///   - `DATA_DIR`    — default `./data`
    ///   - `SYNC_MAX_CONCURRENT` — default `3`
    ///   - `SYNC_POLL_SECS` — default `300`
    ///   - `REDIS_URL` — if set, Redis kv (fail boot on connect error); else memory
    ///   - `LYRA_PUBLIC_URL` — required; public base URL (no trailing slash)
    ///   - `LYRA_VAPID_SUBJECT` — optional; VAPID contact for Web Push
    ///
    /// # Errors
    /// Returns `ConfigError` if `LYRA_MASTER_KEY` or `LYRA_PUBLIC_URL` is invalid.
    pub fn from_env() -> Result<Self, ConfigError> {
        let listen_addr = env::var("LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:3000".to_string());

        let database_url = env::var("DATABASE_URL").unwrap_or_else(|_| {
            tracing::warn!("DATABASE_URL not set; defaulting to sqlite:./data/lyra.db");
            "sqlite:./data/lyra.db".to_string()
        });

        let data_dir = env::var("DATA_DIR").unwrap_or_else(|_| "./data".to_string());

        let min_password_length: usize = env::var("MIN_PASSWORD_LENGTH")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(8);

        let sync_max_concurrent: usize = env::var("SYNC_MAX_CONCURRENT")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|&n| n > 0)
            .unwrap_or(3);

        let sync_poll_secs: u64 = env::var("SYNC_POLL_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|&n| n > 0)
            .unwrap_or(300);

        let max_attachment_bytes: u64 = env::var("LYRA_MAX_ATTACHMENT_BYTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|&n| n >= 1024)
            .unwrap_or(25 * 1024 * 1024);

        let redis_url = env::var("REDIS_URL").ok().filter(|s| !s.is_empty());

        let master_key = master_key_from_env()?;
        let public_url = normalize_public_url(
            &env::var("LYRA_PUBLIC_URL").map_err(|_| ConfigError::PublicUrlMissing)?,
        )?;
        let vapid_subject = env::var("LYRA_VAPID_SUBJECT")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                let host = public_url
                    .trim_start_matches("https://")
                    .trim_start_matches("http://")
                    .split('/')
                    .next()
                    .unwrap_or("localhost");
                format!("mailto:admin@{host}")
            });
        // Fail closed: a present-but-invalid provider matrix refuses boot
        // instead of silently disabling mail OAuth. (A missing file is fine —
        // `load` returns an empty registry then.)
        let oauth_registry =
            crate::oauth::OAuthRegistry::load(&public_url, &data_dir).map_err(|e| {
                tracing::error!(error = %e, "mail OAuth provider config invalid; refusing to start");
                ConfigError::OauthConfig(e.to_string())
            })?;
        let ms_oauth = oauth_registry.microsoft().cloned();
        let yandex_oauth = oauth_registry.yandex().cloned();
        let captcha = captcha_from_env()?;

        Ok(Self {
            listen_addr,
            database_url,
            data_dir,
            min_password_length,
            sync_max_concurrent,
            sync_poll_secs,
            max_attachment_bytes,
            redis_url,
            master_key,
            ms_oauth,
            yandex_oauth,
            captcha,
            vapid_subject,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Env vars are process-global; serialise tests that mutate them.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn defaults_are_sensible() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: These vars are only touched by config tests, which are
        // serialised via ENV_LOCK. This is the standard pattern for
        // testing env-based config in Rust.
        unsafe {
            env::remove_var("LISTEN_ADDR");
            env::remove_var("DATABASE_URL");
            env::remove_var("DATA_DIR");
            env::remove_var("MIN_PASSWORD_LENGTH");
            env::remove_var("SYNC_MAX_CONCURRENT");
            env::remove_var("SYNC_POLL_SECS");
            env::remove_var("REDIS_URL");
            env::remove_var("LYRA_PUBLIC_URL");
            env::remove_var("LYRA_VAPID_SUBJECT");
            env::remove_var("LYRA_CAPTCHA_PROVIDER");
            env::remove_var("LYRA_CAPTCHA_SITE_KEY");
            env::remove_var("LYRA_CAPTCHA_SECRET");
            env::set_var("LYRA_MASTER_KEY", "test-master-key-with-32-bytes-minimum!!");
            env::set_var("LYRA_PUBLIC_URL", "http://localhost:3000");
        }

        let cfg = Config::from_env().unwrap();
        assert_eq!(cfg.captcha, CaptchaConfig::None);
        assert_eq!(cfg.listen_addr, "0.0.0.0:3000");
        assert!(cfg.database_url.contains("sqlite"));
        assert_eq!(cfg.data_dir, "./data");
        assert_eq!(cfg.min_password_length, 8);
        assert_eq!(cfg.sync_max_concurrent, 3);
        assert_eq!(cfg.sync_poll_secs, 300);
        assert!(cfg.redis_url.is_none());
        assert_eq!(cfg.master_key, b"test-master-key-with-32-bytes-minimum!!");
        assert_eq!(cfg.vapid_subject, "mailto:admin@localhost:3000");

        unsafe {
            env::remove_var("LYRA_MASTER_KEY");
            env::remove_var("LYRA_PUBLIC_URL");
        }
    }

    #[test]
    fn missing_master_key_fails_closed() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: see `defaults_are_sensible`.
        unsafe {
            env::remove_var("LYRA_MASTER_KEY");
            env::remove_var("LYRA_PUBLIC_URL");
        }

        let err = Config::from_env().unwrap_err();
        assert!(matches!(err, ConfigError::MasterKeyMissing));
        assert!(err.to_string().contains("openssl rand -base64 32"));
    }

    #[test]
    fn short_master_key_fails_closed() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: see `defaults_are_sensible`.
        unsafe {
            env::set_var("LYRA_MASTER_KEY", "too-short");
            env::set_var("LYRA_PUBLIC_URL", "http://localhost:3000");
        }

        let err = Config::from_env().unwrap_err();
        assert!(matches!(err, ConfigError::MasterKeyTooShort(9)));
        assert!(err.to_string().contains("openssl rand -base64 32"));

        unsafe {
            env::remove_var("LYRA_MASTER_KEY");
            env::remove_var("LYRA_PUBLIC_URL");
        }
    }

    #[test]
    fn missing_public_url_fails_closed() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            env::set_var("LYRA_MASTER_KEY", "test-master-key-with-32-bytes-minimum!!");
            env::remove_var("LYRA_PUBLIC_URL");
        }

        let err = Config::from_env().unwrap_err();
        assert!(matches!(err, ConfigError::PublicUrlMissing));

        unsafe {
            env::remove_var("LYRA_MASTER_KEY");
        }
    }

    #[test]
    fn invalid_public_url_scheme_fails_closed() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            env::set_var("LYRA_MASTER_KEY", "test-master-key-with-32-bytes-minimum!!");
            env::set_var("LYRA_PUBLIC_URL", "ftp://mail.example.com");
        }

        let err = Config::from_env().unwrap_err();
        assert!(matches!(err, ConfigError::PublicUrlInvalid(_)));

        unsafe {
            env::remove_var("LYRA_MASTER_KEY");
            env::remove_var("LYRA_PUBLIC_URL");
            env::remove_var("LYRA_CAPTCHA_PROVIDER");
        }
    }

    #[test]
    fn captcha_defaults_to_none() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            env::set_var("LYRA_MASTER_KEY", "test-master-key-with-32-bytes-minimum!!");
            env::set_var("LYRA_PUBLIC_URL", "http://localhost:3000");
            env::remove_var("LYRA_CAPTCHA_PROVIDER");
            env::remove_var("LYRA_CAPTCHA_SITE_KEY");
            env::remove_var("LYRA_CAPTCHA_SECRET");
        }
        let cfg = Config::from_env().unwrap();
        assert_eq!(cfg.captcha, CaptchaConfig::None);
        unsafe {
            env::remove_var("LYRA_MASTER_KEY");
            env::remove_var("LYRA_PUBLIC_URL");
        }
    }

    #[test]
    fn captcha_turnstile_requires_keys() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            env::set_var("LYRA_MASTER_KEY", "test-master-key-with-32-bytes-minimum!!");
            env::set_var("LYRA_PUBLIC_URL", "http://localhost:3000");
            env::set_var("LYRA_CAPTCHA_PROVIDER", "turnstile");
            env::remove_var("LYRA_CAPTCHA_SITE_KEY");
            env::remove_var("LYRA_CAPTCHA_SECRET");
        }
        let err = Config::from_env().unwrap_err();
        assert!(matches!(err, ConfigError::CaptchaIncomplete(p) if p == "turnstile"));
        unsafe {
            env::remove_var("LYRA_MASTER_KEY");
            env::remove_var("LYRA_PUBLIC_URL");
            env::remove_var("LYRA_CAPTCHA_PROVIDER");
        }
    }

    #[test]
    fn captcha_turnstile_loads_when_complete() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            env::set_var("LYRA_MASTER_KEY", "test-master-key-with-32-bytes-minimum!!");
            env::set_var("LYRA_PUBLIC_URL", "http://localhost:3000");
            env::set_var("LYRA_CAPTCHA_PROVIDER", "turnstile");
            env::set_var("LYRA_CAPTCHA_SITE_KEY", "site-key-test");
            env::set_var("LYRA_CAPTCHA_SECRET", "secret-test");
        }
        let cfg = Config::from_env().unwrap();
        assert_eq!(
            cfg.captcha,
            CaptchaConfig::Turnstile {
                site_key: "site-key-test".into(),
                secret: "secret-test".into(),
            }
        );
        unsafe {
            env::remove_var("LYRA_MASTER_KEY");
            env::remove_var("LYRA_PUBLIC_URL");
            env::remove_var("LYRA_CAPTCHA_PROVIDER");
            env::remove_var("LYRA_CAPTCHA_SITE_KEY");
            env::remove_var("LYRA_CAPTCHA_SECRET");
        }
    }

    #[test]
    fn captcha_rejects_unknown_provider() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            env::set_var("LYRA_MASTER_KEY", "test-master-key-with-32-bytes-minimum!!");
            env::set_var("LYRA_PUBLIC_URL", "http://localhost:3000");
            env::set_var("LYRA_CAPTCHA_PROVIDER", "securimage");
        }
        let err = Config::from_env().unwrap_err();
        assert!(matches!(err, ConfigError::CaptchaProviderUnknown(_)));
        unsafe {
            env::remove_var("LYRA_MASTER_KEY");
            env::remove_var("LYRA_PUBLIC_URL");
            env::remove_var("LYRA_CAPTCHA_PROVIDER");
        }
    }

    #[test]
    fn captcha_hcaptcha_loads_when_complete() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            env::set_var("LYRA_MASTER_KEY", "test-master-key-with-32-bytes-minimum!!");
            env::set_var("LYRA_PUBLIC_URL", "http://localhost:3000");
            env::set_var("LYRA_CAPTCHA_PROVIDER", "hcaptcha");
            env::set_var("LYRA_CAPTCHA_SITE_KEY", "site-key-test");
            env::set_var("LYRA_CAPTCHA_SECRET", "secret-test");
        }
        let cfg = Config::from_env().unwrap();
        assert_eq!(
            cfg.captcha,
            CaptchaConfig::HCaptcha {
                site_key: "site-key-test".into(),
                secret: "secret-test".into(),
            }
        );
        unsafe {
            env::remove_var("LYRA_MASTER_KEY");
            env::remove_var("LYRA_PUBLIC_URL");
            env::remove_var("LYRA_CAPTCHA_PROVIDER");
            env::remove_var("LYRA_CAPTCHA_SITE_KEY");
            env::remove_var("LYRA_CAPTCHA_SECRET");
        }
    }

    #[test]
    fn captcha_recaptcha_loads_when_complete() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            env::set_var("LYRA_MASTER_KEY", "test-master-key-with-32-bytes-minimum!!");
            env::set_var("LYRA_PUBLIC_URL", "http://localhost:3000");
            env::set_var("LYRA_CAPTCHA_PROVIDER", "recaptcha");
            env::set_var("LYRA_CAPTCHA_SITE_KEY", "site-key-test");
            env::set_var("LYRA_CAPTCHA_SECRET", "secret-test");
        }
        let cfg = Config::from_env().unwrap();
        assert_eq!(
            cfg.captcha,
            CaptchaConfig::Recaptcha {
                site_key: "site-key-test".into(),
                secret: "secret-test".into(),
            }
        );
        assert_eq!(
            cfg.captcha.public().unwrap().provider,
            "recaptcha".to_string()
        );
        unsafe {
            env::remove_var("LYRA_MASTER_KEY");
            env::remove_var("LYRA_PUBLIC_URL");
            env::remove_var("LYRA_CAPTCHA_PROVIDER");
            env::remove_var("LYRA_CAPTCHA_SITE_KEY");
            env::remove_var("LYRA_CAPTCHA_SECRET");
        }
    }

    #[test]
    fn captcha_recaptcha_v3_loads_when_complete() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            env::set_var("LYRA_MASTER_KEY", "test-master-key-with-32-bytes-minimum!!");
            env::set_var("LYRA_PUBLIC_URL", "http://localhost:3000");
            env::set_var("LYRA_CAPTCHA_PROVIDER", "recaptcha-v3");
            env::set_var("LYRA_CAPTCHA_SITE_KEY", "site-key-test");
            env::set_var("LYRA_CAPTCHA_SECRET", "secret-test");
        }
        let cfg = Config::from_env().unwrap();
        assert_eq!(
            cfg.captcha,
            CaptchaConfig::RecaptchaV3 {
                site_key: "site-key-test".into(),
                secret: "secret-test".into(),
            }
        );
        assert_eq!(
            cfg.captcha.public().unwrap().provider,
            "recaptcha-v3".to_string()
        );
        unsafe {
            env::remove_var("LYRA_MASTER_KEY");
            env::remove_var("LYRA_PUBLIC_URL");
            env::remove_var("LYRA_CAPTCHA_PROVIDER");
            env::remove_var("LYRA_CAPTCHA_SITE_KEY");
            env::remove_var("LYRA_CAPTCHA_SECRET");
        }
    }

    #[test]
    fn captcha_rejects_legacy_provider_env_vars() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            env::set_var("LYRA_MASTER_KEY", "test-master-key-with-32-bytes-minimum!!");
            env::set_var("LYRA_PUBLIC_URL", "http://localhost:3000");
            env::remove_var("LYRA_CAPTCHA_PROVIDER");
            env::set_var("LYRA_HCAPTCHA_SECRET", "oops");
        }
        let err = Config::from_env().unwrap_err();
        assert!(matches!(err, ConfigError::CaptchaMisconfigured(_)));
        unsafe {
            env::remove_var("LYRA_MASTER_KEY");
            env::remove_var("LYRA_PUBLIC_URL");
            env::remove_var("LYRA_HCAPTCHA_SECRET");
        }
    }
}
