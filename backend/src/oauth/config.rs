//! Mail OAuth provider credentials matrix (TOML file).
//!
//! Provider endpoints, scopes, and domain rules live in code (`providers.rs`).
//! Deployers only supply per-provider client credentials here.

use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::microsoft::MsOAuthConfig;
use super::oauth_callback_url;
use super::providers::{self, PROVIDER_CATALOG};
use super::yandex::YandexOAuthConfig;

/// Runtime OAuth registry loaded at boot from [`LYRA_OAUTH_CONFIG`].
#[derive(Debug, Clone, Default)]
pub struct OAuthRegistry {
    microsoft: Option<MsOAuthConfig>,
    yandex: Option<YandexOAuthConfig>,
}

/// Per-provider secrets from the TOML matrix.
#[derive(Debug, Clone, Deserialize)]
pub struct ProviderCredentials {
    pub client_id: String,
    #[serde(default)]
    pub client_secret: Option<String>,
    /// Microsoft Entra tenant (`common`, `organizations`, or tenant GUID).
    #[serde(default)]
    pub tenant: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OAuthProvidersFile {
    #[serde(default)]
    providers: HashMap<String, ProviderCredentials>,
}

#[derive(Debug, thiserror::Error)]
pub enum OAuthConfigError {
    #[error("read OAuth config {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("parse OAuth config {path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("OAuth provider {provider}: client_id must not be empty")]
    EmptyClientId { provider: String },
}

impl OAuthRegistry {
    /// Load provider matrix from `LYRA_OAUTH_CONFIG` or `{data_dir}/oauth-providers.toml`.
    ///
    /// Missing file → empty registry (mail OAuth disabled). Legacy `LYRA_MS_OAUTH_*` env
    /// vars still populate `[providers.microsoft]` when the file is absent.
    pub fn load(public_url: &str, data_dir: &str) -> Result<Self, OAuthConfigError> {
        let path = oauth_config_path(data_dir);
        let mut credentials = if path.is_file() {
            parse_oauth_file(&path)?
        } else {
            HashMap::new()
        };

        // File entries win; legacy env vars only fill providers the file omits.
        if let Some(legacy) = legacy_microsoft_from_env() {
            credentials
                .entry(providers::MICROSOFT.to_string())
                .or_insert(legacy);
        }
        if let Some(legacy) = legacy_yandex_from_env() {
            credentials
                .entry(providers::YANDEX.to_string())
                .or_insert(legacy);
        }

        let redirect_uri = oauth_callback_url(public_url);
        let microsoft = credentials
            .get(providers::MICROSOFT)
            .map(|c| ms_config_from_credentials(c, &redirect_uri));
        let yandex = credentials
            .get(providers::YANDEX)
            .map(|c| yandex_config_from_credentials(c, &redirect_uri));

        if microsoft.is_some() {
            tracing::info!(
                redirect_uri = %redirect_uri,
                "Microsoft mail OAuth configured"
            );
        }
        if yandex.is_some() {
            tracing::info!(
                redirect_uri = %redirect_uri,
                "Yandex mail OAuth configured"
            );
        }

        let configured: Vec<_> = PROVIDER_CATALOG
            .iter()
            .filter(|p| credentials_configured(&credentials, p.id))
            .map(|p| p.id)
            .collect();
        if !configured.is_empty() {
            tracing::info!(providers = ?configured, "mail OAuth providers configured");
        }

        Ok(Self { microsoft, yandex })
    }

    pub fn microsoft(&self) -> Option<&MsOAuthConfig> {
        self.microsoft.as_ref()
    }

    pub fn yandex(&self) -> Option<&YandexOAuthConfig> {
        self.yandex.as_ref()
    }

    /// OAuth refresh configs for sync/send workers.
    ///
    /// Re-reads the provider matrix so credential rotations land without a
    /// restart. A runtime load failure logs and falls back to env credentials
    /// rather than silently disabling refresh (boot fails closed instead).
    pub fn refresh_configs() -> super::tokens::OAuthRefreshConfigs {
        let public_url = env::var("LYRA_PUBLIC_URL")
            .ok()
            .filter(|s| !s.trim().is_empty());
        let data_dir = env::var("DATA_DIR").unwrap_or_else(|_| "./data".to_string());
        if let Some(url) = public_url.as_deref() {
            match Self::load(url, &data_dir) {
                Ok(reg) => {
                    return super::tokens::OAuthRefreshConfigs {
                        microsoft: reg.microsoft.clone(),
                        yandex: reg.yandex.clone(),
                    };
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "mail OAuth provider config unreadable; falling back to env credentials"
                    );
                }
            }
        }
        super::tokens::OAuthRefreshConfigs::from_env()
    }
}

pub fn oauth_config_path(data_dir: &str) -> PathBuf {
    env::var("LYRA_OAUTH_CONFIG")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map_or_else(
            || PathBuf::from(data_dir).join("oauth-providers.toml"),
            PathBuf::from,
        )
}

fn credentials_configured(credentials: &HashMap<String, ProviderCredentials>, id: &str) -> bool {
    credentials
        .get(id)
        .is_some_and(|c| !c.client_id.trim().is_empty())
}

fn parse_oauth_file(path: &Path) -> Result<HashMap<String, ProviderCredentials>, OAuthConfigError> {
    let raw = std::fs::read_to_string(path).map_err(|source| OAuthConfigError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let file: OAuthProvidersFile =
        toml::from_str(&raw).map_err(|source| OAuthConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
    for (provider, creds) in &file.providers {
        if creds.client_id.trim().is_empty() {
            return Err(OAuthConfigError::EmptyClientId {
                provider: provider.clone(),
            });
        }
    }
    Ok(file.providers)
}

fn ms_config_from_credentials(creds: &ProviderCredentials, redirect_uri: &str) -> MsOAuthConfig {
    let tenant = creds
        .tenant
        .as_deref()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or("common")
        .to_string();
    MsOAuthConfig {
        client_id: creds.client_id.trim().into(),
        client_secret: creds
            .client_secret
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string),
        redirect_uri: redirect_uri.into(),
        tenant,
    }
}

fn yandex_config_from_credentials(
    creds: &ProviderCredentials,
    redirect_uri: &str,
) -> YandexOAuthConfig {
    YandexOAuthConfig {
        client_id: creds.client_id.trim().into(),
        client_secret: creds
            .client_secret
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string),
        redirect_uri: redirect_uri.into(),
    }
}

fn legacy_microsoft_from_env() -> Option<ProviderCredentials> {
    let client_id = env::var("LYRA_MS_OAUTH_CLIENT_ID").ok()?;
    if client_id.trim().is_empty() {
        return None;
    }
    let client_secret = env::var("LYRA_MS_OAUTH_CLIENT_SECRET")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let tenant = env::var("LYRA_MS_OAUTH_TENANT").ok();
    Some(ProviderCredentials {
        client_id,
        client_secret,
        tenant,
    })
}

fn legacy_yandex_from_env() -> Option<ProviderCredentials> {
    let client_id = env::var("LYRA_YANDEX_OAUTH_CLIENT_ID").ok()?;
    if client_id.trim().is_empty() {
        return None;
    }
    let client_secret = env::var("LYRA_YANDEX_OAUTH_CLIENT_SECRET")
        .ok()
        .filter(|s| !s.trim().is_empty());
    Some(ProviderCredentials {
        client_id,
        client_secret,
        tenant: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn parses_provider_matrix() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("oauth-providers.toml");
        std::fs::write(
            &path,
            r#"
[providers.microsoft]
client_id = "ms-id"
client_secret = "ms-secret"
tenant = "common"

[providers.yandex]
client_id = "ya-id"
"#,
        )
        .unwrap();

        let creds = parse_oauth_file(&path).unwrap();
        assert_eq!(creds.len(), 2);
        assert_eq!(creds["microsoft"].client_id, "ms-id");
        assert_eq!(creds["yandex"].client_id, "ya-id");
    }

    #[test]
    fn load_builds_microsoft_runtime_config() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().to_string_lossy();
        let path = dir.path().join("oauth-providers.toml");
        std::fs::write(
            &path,
            r#"
[providers.microsoft]
client_id = "cid"
tenant = "common"
"#,
        )
        .unwrap();
        unsafe {
            env::set_var("LYRA_OAUTH_CONFIG", path.to_string_lossy().to_string());
            env::remove_var("LYRA_MS_OAUTH_CLIENT_ID");
        }

        let reg = OAuthRegistry::load("http://localhost:3000", &data).unwrap();
        assert!(reg.microsoft().is_some());
        assert!(reg.yandex().is_none());
        let ms = reg.microsoft().unwrap();
        assert_eq!(ms.client_id, "cid");
        assert_eq!(
            ms.redirect_uri,
            "http://localhost:3000/api/v1/oauth/callback"
        );

        unsafe {
            env::remove_var("LYRA_OAUTH_CONFIG");
        }
    }
}
