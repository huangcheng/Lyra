//! Key-value store seam for sessions and short-lived codes.
//!
//! Redis in production (Task 8); in-memory adapter for tests and local runs
//! without `REDIS_URL`. See `docs/specs/2026-08-22-lyra-plugin-kernel-spec.md` §5.2.

#![allow(clippy::doc_markdown)]
#![allow(clippy::missing_errors_doc)]

mod memory;
mod redis;

pub use memory::MemoryKv;
pub use redis::RedisKv;

use async_trait::async_trait;

/// Errors from a [`KvStore`] operation.
#[derive(Debug, thiserror::Error)]
#[allow(dead_code)]
pub enum KvError {
    #[error("kv internal error: {0}")]
    Internal(String),
}

/// Async key-value store used for sessions, pending TOTP tokens, and rate limits.
#[async_trait]
pub trait KvStore: Send + Sync {
    async fn get(&self, key: &str) -> Result<Option<String>, KvError>;
    async fn set(&self, key: &str, value: &str, ttl_secs: Option<u64>) -> Result<(), KvError>;
    async fn del(&self, key: &str) -> Result<(), KvError>;
    /// Delete all keys with the given prefix (used for epoch session wipe).
    #[allow(dead_code)]
    async fn del_prefix(&self, prefix: &str) -> Result<(), KvError>;
    /// Atomically increment a counter key (rate limits / OTP attempts).
    #[allow(dead_code)]
    async fn incr(&self, key: &str, delta: i64) -> Result<i64, KvError>;
}
