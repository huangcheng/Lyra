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
    /// Redis URL for session/kv store (`REDIS_URL`). When unset, boot uses in-memory kv.
    pub redis_url: Option<String>,
    /// Master key for the per-user DEK hierarchy (`LYRA_MASTER_KEY`, 32+ bytes).
    /// Required: the backend refuses to start without it. Never logged.
    pub master_key: Vec<u8>,
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
    ///
    /// # Errors
    /// Returns `ConfigError` if `LYRA_MASTER_KEY` is missing or shorter than 32 bytes.
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

        let redis_url = env::var("REDIS_URL").ok().filter(|s| !s.is_empty());

        let master_key = master_key_from_env()?;

        Ok(Self {
            listen_addr,
            database_url,
            data_dir,
            min_password_length,
            sync_max_concurrent,
            sync_poll_secs,
            redis_url,
            master_key,
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
            env::set_var("LYRA_MASTER_KEY", "test-master-key-with-32-bytes-minimum!!");
        }

        let cfg = Config::from_env().unwrap();
        assert_eq!(cfg.listen_addr, "0.0.0.0:3000");
        assert!(cfg.database_url.contains("sqlite"));
        assert_eq!(cfg.data_dir, "./data");
        assert_eq!(cfg.min_password_length, 8);
        assert_eq!(cfg.sync_max_concurrent, 3);
        assert_eq!(cfg.sync_poll_secs, 300);
        assert!(cfg.redis_url.is_none());
        assert_eq!(cfg.master_key, b"test-master-key-with-32-bytes-minimum!!");

        unsafe {
            env::remove_var("LYRA_MASTER_KEY");
        }
    }

    #[test]
    fn missing_master_key_fails_closed() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: see `defaults_are_sensible`.
        unsafe {
            env::remove_var("LYRA_MASTER_KEY");
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
        }

        let err = Config::from_env().unwrap_err();
        assert!(matches!(err, ConfigError::MasterKeyTooShort(9)));
        assert!(err.to_string().contains("openssl rand -base64 32"));

        unsafe {
            env::remove_var("LYRA_MASTER_KEY");
        }
    }
}
