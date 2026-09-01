//! Mail OAuth provider credentials matrix (TOML file).
//!
//! Provider endpoints, scopes, and domain rules live in code (`providers.rs`).
//! Deployers only supply per-provider client credentials here.

use std::collections::{HashMap, HashSet};
use std::env;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

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
        // Boxed: toml::de::Error is large (span + message); keeps
        // Result<_, OAuthConfigError> small (clippy::result_large_err).
        source: Box<toml::de::Error>,
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
        // When both define the same provider, warn once if the secrets differ:
        // a stale value silently ignored by file precedence is exactly how a
        // secret rotation gets lost. Fingerprints only — never log values.
        let mut credentials_source: HashMap<&str, &str> = HashMap::new();
        for (id, legacy) in [
            (providers::MICROSOFT, legacy_microsoft_from_env()),
            (providers::YANDEX, legacy_yandex_from_env()),
        ] {
            let Some(legacy) = legacy else { continue };
            if let Some(file_entry) = credentials.get(id) {
                credentials_source.insert(id, "file");
                warn_once_on_secret_divergence(
                    id,
                    file_entry.client_secret.as_deref(),
                    legacy.client_secret.as_deref(),
                );
            } else {
                credentials_source.insert(id, "env");
                credentials.insert(id.to_string(), legacy);
            }
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
                credentials = credentials_source
                    .get(providers::MICROSOFT)
                    .copied()
                    .unwrap_or("file"),
                "Microsoft mail OAuth configured"
            );
        }
        if yandex.is_some() {
            tracing::info!(
                redirect_uri = %redirect_uri,
                credentials = credentials_source
                    .get(providers::YANDEX)
                    .copied()
                    .unwrap_or("file"),
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

/// Divergence situations already warned about. `load` also runs on every
/// sync tick via [`OAuthRegistry::refresh_configs`], so without dedup the
/// warning would spam once per reload.
static WARNED_SECRET_DIVERGENCES: OnceLock<Mutex<HashSet<u64>>> = OnceLock::new();

fn warn_once_on_secret_divergence(
    provider: &str,
    file_secret: Option<&str>,
    env_secret: Option<&str>,
) {
    let (Some(file), Some(env)) = (file_secret, env_secret) else {
        return;
    };
    let (file, env) = (file.trim(), env.trim());
    if file.is_empty() || env.is_empty() || file == env {
        return;
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (
        provider,
        file.len(),
        secret_shape(file),
        env.len(),
        secret_shape(env),
    )
        .hash(&mut hasher);
    let warned = WARNED_SECRET_DIVERGENCES.get_or_init(|| Mutex::new(HashSet::new()));
    if !warned.lock().unwrap().insert(hasher.finish()) {
        return;
    }
    tracing::warn!(
        provider = provider,
        file_secret = %secret_fingerprint(file),
        env_secret = %secret_fingerprint(env),
        "mail OAuth provider configured in both oauth-providers.toml and env with different secrets; the FILE value wins and env is ignored — update the TOML file to rotate"
    );
}

/// Non-reversible fingerprint for diagnostics; never log the value itself.
fn secret_fingerprint(secret: &str) -> String {
    format!("len={} {}", secret.len(), secret_shape(secret))
}

/// Coarse shape class. A 36-char UUID is almost always the Entra secret *ID*
/// pasted instead of the secret *Value* (AADSTS7000215).
fn secret_shape(secret: &str) -> &'static str {
    let uuidish = secret.len() == 36 && secret.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
    if uuidish { "uuid-shaped" } else { "opaque" }
}

fn parse_oauth_file(path: &Path) -> Result<HashMap<String, ProviderCredentials>, OAuthConfigError> {
    let raw = std::fs::read_to_string(path).map_err(|source| OAuthConfigError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let file: OAuthProvidersFile =
        toml::from_str(&raw).map_err(|source| OAuthConfigError::Parse {
            path: path.to_path_buf(),
            source: Box::new(source),
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
    fn secret_fingerprint_shapes_uuid_vs_value() {
        // The exact failure this shape check exists for: an Entra secret ID
        // (UUID) pasted where the Value belongs.
        let secret_id = "410a0d91-2466-48e7-94dc-7ae3ad58262c";
        assert_eq!(secret_shape(secret_id), "uuid-shaped");
        assert_eq!(secret_fingerprint(secret_id), "len=36 uuid-shaped");
        // Synthetic 40-char Entra-style Value (tilde + alphanumerics) — never
        // a real credential.
        assert_eq!(
            secret_shape("EXAMPLE~NOTAREALSECRETvALUE0000000000000000"),
            "opaque"
        );
    }

    #[test]
    fn divergence_warn_tolerates_repeats_and_noops() {
        // Repeats dedup (no panic, no unbounded set growth); absent, blank,
        // and equal secrets are no-ops. The dedup set is keyed by provider +
        // secret shapes, so each test run uses distinct providers.
        let (file, env) = (Some("file-secret"), Some("env-secret"));
        warn_once_on_secret_divergence("microsoft-divergence-a", file, env);
        warn_once_on_secret_divergence("microsoft-divergence-a", file, env);
        warn_once_on_secret_divergence("microsoft-divergence-b", None, env);
        warn_once_on_secret_divergence("microsoft-divergence-c", file, Some("   "));
        warn_once_on_secret_divergence("microsoft-divergence-d", file, file);
    }

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
