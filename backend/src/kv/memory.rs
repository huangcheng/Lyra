//! In-memory [`KvStore`] with optional TTL.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::sync::RwLock;

use super::{KvError, KvStore};

struct Entry {
    value: String,
    expires_at: Option<Instant>,
}

/// Process-local kv for tests and local runs without Redis.
#[derive(Clone, Default)]
pub struct MemoryKv {
    inner: Arc<RwLock<HashMap<String, Entry>>>,
}

impl MemoryKv {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Remaining TTL for a key; `None` if the key is missing, expired, or
    /// has no TTL. Test-only inspection hook for TTL behavior tests.
    #[cfg(test)]
    pub async fn ttl_remaining(&self, key: &str) -> Option<Duration> {
        let map = self.inner.read().await;
        let entry = map.get(key)?;
        entry
            .expires_at
            .and_then(|e| e.checked_duration_since(Instant::now()))
    }
}

#[async_trait]
impl KvStore for MemoryKv {
    async fn get(&self, key: &str) -> Result<Option<String>, KvError> {
        let mut map = self.inner.write().await;
        let Some(entry) = map.get(key) else {
            return Ok(None);
        };
        if let Some(expires_at) = entry.expires_at
            && Instant::now() >= expires_at
        {
            map.remove(key);
            return Ok(None);
        }
        Ok(Some(entry.value.clone()))
    }

    async fn set(&self, key: &str, value: &str, ttl_secs: Option<u64>) -> Result<(), KvError> {
        let expires_at = ttl_secs.map(|s| Instant::now() + Duration::from_secs(s));
        self.inner.write().await.insert(
            key.to_string(),
            Entry {
                value: value.to_string(),
                expires_at,
            },
        );
        Ok(())
    }

    async fn del(&self, key: &str) -> Result<(), KvError> {
        self.inner.write().await.remove(key);
        Ok(())
    }

    async fn del_prefix(&self, prefix: &str) -> Result<(), KvError> {
        let mut map = self.inner.write().await;
        map.retain(|k, _| !k.starts_with(prefix));
        Ok(())
    }

    async fn incr(&self, key: &str, delta: i64, ttl_secs: Option<u64>) -> Result<i64, KvError> {
        let mut map = self.inner.write().await;
        // Drop an expired entry first so a fixed window restarts cleanly.
        if let Some(entry) = map.get(key)
            && let Some(expires_at) = entry.expires_at
            && Instant::now() >= expires_at
        {
            map.remove(key);
        }
        let existing = map.get(key).map(|e| (e.value.clone(), e.expires_at));
        if let Some((value, expires_at)) = existing {
            let current = value
                .parse::<i64>()
                .map_err(|e| KvError::Internal(e.to_string()))?;
            let next = current.saturating_add(delta);
            // Keep the existing expiry: the window started at first incr.
            map.insert(
                key.to_string(),
                Entry {
                    value: next.to_string(),
                    expires_at,
                },
            );
            Ok(next)
        } else {
            map.insert(
                key.to_string(),
                Entry {
                    value: delta.to_string(),
                    expires_at: ttl_secs.map(|s| Instant::now() + Duration::from_secs(s)),
                },
            );
            Ok(delta)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn set_get_del() {
        let kv = MemoryKv::new();
        kv.set("a", "1", None).await.unwrap();
        assert_eq!(kv.get("a").await.unwrap().as_deref(), Some("1"));
        kv.del("a").await.unwrap();
        assert!(kv.get("a").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn del_prefix_drops_user_sessions() {
        let kv = MemoryKv::new();
        kv.set("sess:3:aaa", "user-1", None).await.unwrap();
        kv.set("sess:3:bbb", "user-1", None).await.unwrap();
        kv.set("sess:4:ccc", "user-1", None).await.unwrap();
        kv.del_prefix("sess:3:").await.unwrap();
        assert!(kv.get("sess:3:aaa").await.unwrap().is_none());
        assert!(kv.get("sess:3:bbb").await.unwrap().is_none());
        assert_eq!(
            kv.get("sess:4:ccc").await.unwrap().as_deref(),
            Some("user-1")
        );
    }

    #[tokio::test]
    async fn ttl_expires_on_get() {
        let kv = MemoryKv::new();
        kv.set("ephemeral", "x", Some(1)).await.unwrap();
        assert_eq!(kv.get("ephemeral").await.unwrap().as_deref(), Some("x"));
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert!(kv.get("ephemeral").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn incr_starts_at_zero() {
        let kv = MemoryKv::new();
        assert_eq!(kv.incr("n", 1, None).await.unwrap(), 1);
        assert_eq!(kv.incr("n", 2, None).await.unwrap(), 3);
    }

    #[tokio::test]
    async fn incr_applies_ttl_only_on_first_create() {
        let kv = MemoryKv::new();
        assert_eq!(kv.incr("c", 1, Some(1)).await.unwrap(), 1);
        let first_ttl = kv.ttl_remaining("c").await.expect("ttl set on create");
        // A later incr without a TTL must not clear the window.
        assert_eq!(kv.incr("c", 1, None).await.unwrap(), 2);
        assert!(kv.ttl_remaining("c").await.is_some_and(|t| t <= first_ttl));
        tokio::time::sleep(Duration::from_millis(1100)).await;
        // Window expired: the counter starts over.
        assert_eq!(kv.incr("c", 1, Some(60)).await.unwrap(), 1);
    }
}
