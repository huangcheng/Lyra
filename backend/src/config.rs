//! Environment-based configuration.
//!
//! All values come from environment variables with sensible defaults.
//! No secrets are stored in the tree; credentials are loaded at runtime.

use std::env;

/// Application configuration loaded from environment variables.
#[derive(Debug, Clone)]
#[allow(dead_code)] // Fields will be used when DB/storage layers are implemented.
pub struct Config {
    /// Address to listen on (e.g. "0.0.0.0:3000").
    pub listen_addr: String,
    /// Database connection URL (`sqlite:///path` or `postgres://...`).
    pub database_url: String,
    /// Directory for storing message blobs and attachments.
    pub data_dir: String,
}

impl Config {
    /// Load configuration from environment variables.
    ///
    /// Required:
    ///   - `DATABASE_URL` — connection string
    ///
    /// Optional (with defaults):
    ///   - `LISTEN_ADDR` — default `0.0.0.0:3000`
    ///   - `DATA_DIR`    — default `./data`
    pub fn from_env() -> Self {
        let listen_addr = env::var("LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:3000".to_string());

        let database_url = env::var("DATABASE_URL").unwrap_or_else(|_| {
            tracing::warn!("DATABASE_URL not set; defaulting to sqlite:./data/lyra.db");
            "sqlite:./data/lyra.db".to_string()
        });

        let data_dir = env::var("DATA_DIR").unwrap_or_else(|_| "./data".to_string());

        Self {
            listen_addr,
            database_url,
            data_dir,
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
        }

        let cfg = Config::from_env();
        assert_eq!(cfg.listen_addr, "0.0.0.0:3000");
        assert!(cfg.database_url.contains("sqlite"));
        assert_eq!(cfg.data_dir, "./data");
    }
}
