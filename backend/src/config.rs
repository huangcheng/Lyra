//! Environment-based configuration.
//!
//! All values come from environment variables with sensible defaults.
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
    /// Session signing secret (32+ bytes, base64 encoded).
    /// Loaded from `SESSION_SECRET` env var.
    #[allow(dead_code)]
    pub session_secret: Vec<u8>,
    /// Minimum password length (default: 8).
    pub min_password_length: usize,
    /// Max concurrent mailbox syncs (`SYNC_MAX_CONCURRENT`, default 3).
    pub sync_max_concurrent: usize,
}

impl Config {
    /// Load configuration from environment variables.
    ///
    /// Required:
    ///   - `DATABASE_URL` — connection string
    ///   - `SESSION_SECRET` — signing secret for session cookies (32+ bytes)
    ///
    /// Optional (with defaults):
    ///   - `LISTEN_ADDR` — default `0.0.0.0:3000`
    ///   - `DATA_DIR`    — default `./data`
    ///   - `SYNC_MAX_CONCURRENT` — default `3`
    pub fn from_env() -> Self {
        use rand::RngCore;

        let listen_addr = env::var("LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:3000".to_string());

        let database_url = env::var("DATABASE_URL").unwrap_or_else(|_| {
            tracing::warn!("DATABASE_URL not set; defaulting to sqlite:./data/lyra.db");
            "sqlite:./data/lyra.db".to_string()
        });

        let data_dir = env::var("DATA_DIR").unwrap_or_else(|_| "./data".to_string());

        let session_secret = env::var("SESSION_SECRET").map_or_else(
            |_| {
                tracing::warn!(
                    "SESSION_SECRET not set; generating ephemeral secret (sessions will not persist across restarts)"
                );
                // Generate a random 64-byte secret for dev/testing.
                // In production this MUST be set via env var.
                let mut rng = rand::thread_rng();
                let mut bytes = vec![0u8; 64];
                rng.fill_bytes(&mut bytes);
                bytes
            },
            String::into_bytes,
        );

        let min_password_length: usize = env::var("MIN_PASSWORD_LENGTH")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(8);

        let sync_max_concurrent: usize = env::var("SYNC_MAX_CONCURRENT")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|&n| n > 0)
            .unwrap_or(3);

        Self {
            listen_addr,
            database_url,
            data_dir,
            session_secret,
            min_password_length,
            sync_max_concurrent,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sensible() {
        // SAFETY: Tests run single-threaded per process and these vars
        // are only touched here. This is the standard pattern for
        // testing env-based config in Rust.
        unsafe {
            env::remove_var("LISTEN_ADDR");
            env::remove_var("DATABASE_URL");
            env::remove_var("DATA_DIR");
            env::remove_var("SESSION_SECRET");
            env::remove_var("MIN_PASSWORD_LENGTH");
            env::remove_var("SYNC_MAX_CONCURRENT");
        }

        let cfg = Config::from_env();
        assert_eq!(cfg.listen_addr, "0.0.0.0:3000");
        assert!(cfg.database_url.contains("sqlite"));
        assert_eq!(cfg.data_dir, "./data");
        assert_eq!(cfg.min_password_length, 8);
        assert_eq!(cfg.sync_max_concurrent, 3);
        assert!(cfg.session_secret.len() >= 32);
    }
}
