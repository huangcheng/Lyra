//! Redis [`KvStore`] adapter for sessions and short-lived codes.
//!
//! Used when `REDIS_URL` is set. Connect fails closed at boot (no memory fallback).

use async_trait::async_trait;
use redis::AsyncCommands;
use redis::aio::ConnectionManager;

use super::{KvError, KvStore};

/// Redis-backed kv store using a multiplexed connection manager.
pub struct RedisKv {
    conn: ConnectionManager,
}

impl RedisKv {
    /// Open a Redis client and establish a connection manager.
    ///
    /// Returns an error if the URL is invalid or Redis is unreachable.
    pub async fn connect(url: &str) -> anyhow::Result<Self> {
        let client = redis::Client::open(url)?;
        let conn = ConnectionManager::new(client).await?;
        Ok(Self { conn })
    }

    fn map_err(err: &redis::RedisError) -> KvError {
        KvError::Internal(err.to_string())
    }
}

#[async_trait]
impl KvStore for RedisKv {
    async fn get(&self, key: &str) -> Result<Option<String>, KvError> {
        let mut conn = self.conn.clone();
        conn.get(key).await.map_err(|e| Self::map_err(&e))
    }

    async fn set(&self, key: &str, value: &str, ttl_secs: Option<u64>) -> Result<(), KvError> {
        let mut conn = self.conn.clone();
        match ttl_secs {
            Some(secs) => {
                let _: () = conn
                    .set_ex(key, value, secs)
                    .await
                    .map_err(|e| Self::map_err(&e))?;
            }
            None => {
                let _: () = conn.set(key, value).await.map_err(|e| Self::map_err(&e))?;
            }
        }
        Ok(())
    }

    async fn del(&self, key: &str) -> Result<(), KvError> {
        let mut conn = self.conn.clone();
        let _: () = conn.del(key).await.map_err(|e| Self::map_err(&e))?;
        Ok(())
    }

    async fn del_prefix(&self, prefix: &str) -> Result<(), KvError> {
        let pattern = format!("{prefix}*");
        let mut conn = self.conn.clone();
        let mut cursor: u64 = 0;
        loop {
            let (next, keys): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(&pattern)
                .arg("COUNT")
                .arg(100)
                .query_async(&mut conn)
                .await
                .map_err(|e| Self::map_err(&e))?;
            if !keys.is_empty() {
                let _: () = conn.del(keys).await.map_err(|e| Self::map_err(&e))?;
            }
            cursor = next;
            if cursor == 0 {
                break;
            }
        }
        Ok(())
    }

    async fn incr(&self, key: &str, delta: i64) -> Result<i64, KvError> {
        let mut conn = self.conn.clone();
        conn.incr(key, delta).await.map_err(|e| Self::map_err(&e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "needs redis"]
    async fn redis_roundtrip() {
        let url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".into());
        let kv = RedisKv::connect(&url)
            .await
            .expect("REDIS_URL set but connect failed");

        let key = format!("lyra:test:roundtrip:{}", uuid::Uuid::now_v7());
        kv.set(&key, "ok", Some(30)).await.unwrap();
        assert_eq!(kv.get(&key).await.unwrap().as_deref(), Some("ok"));

        let prefix = format!("lyra:test:prefix:{}:", uuid::Uuid::now_v7());
        kv.set(&format!("{prefix}a"), "1", None).await.unwrap();
        kv.set(&format!("{prefix}b"), "2", None).await.unwrap();
        kv.del_prefix(&prefix).await.unwrap();
        assert!(kv.get(&format!("{prefix}a")).await.unwrap().is_none());
        assert!(kv.get(&format!("{prefix}b")).await.unwrap().is_none());

        kv.del(&key).await.unwrap();
        assert!(kv.get(&key).await.unwrap().is_none());

        let counter = format!("lyra:test:incr:{}", uuid::Uuid::now_v7());
        assert_eq!(kv.incr(&counter, 1).await.unwrap(), 1);
        assert_eq!(kv.incr(&counter, 2).await.unwrap(), 3);
        kv.del(&counter).await.unwrap();
    }
}
