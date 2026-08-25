//! Per-session in-memory unlock ring for OpenGPG secret keys.
//!
//! Passphrases are held only in process memory (never at rest), keyed by
//! auth session token. Entries zeroize on drop. Idle timeout relocks.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use zeroize::{Zeroize, Zeroizing};

/// How long an unlock is remembered (opengpg-spec).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheMode {
    /// Validate only; nothing retained after the unlock request.
    Once,
    /// Cached until TTL or idle timeout.
    Timed,
    /// Cached until logout / explicit lock (still subject to idle timeout).
    Session,
}

impl CacheMode {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "once" => Ok(Self::Once),
            "timed" => Ok(Self::Timed),
            "session" => Ok(Self::Session),
            other => Err(format!(
                "cache must be once, timed, or session (got '{other}')"
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Once => "once",
            Self::Timed => "timed",
            Self::Session => "session",
        }
    }
}

/// Default timed TTL (gpg-agent `default-cache-ttl` analog).
pub const DEFAULT_TTL_MINUTES: u32 = 10;
/// Max configurable TTL.
pub const MAX_TTL_MINUTES: u32 = 120;
/// Idle timeout before relock (all cached modes).
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);

struct CachedUnlock {
    passphrase: Zeroizing<String>,
    #[allow(dead_code)] // retained for debugging / future policy
    mode: CacheMode,
    /// Absolute expiry for `Timed`; `None` for `Session`.
    expires_at: Option<Instant>,
    last_used: Instant,
}

impl Drop for CachedUnlock {
    fn drop(&mut self) {
        // Zeroizing already clears on drop; explicit touch keeps the intent obvious.
        self.passphrase.zeroize();
    }
}

/// Process-wide unlock rings, one map per auth session token.
#[derive(Default)]
pub struct UnlockRing {
    inner: Mutex<HashMap<String, HashMap<String, CachedUnlock>>>,
}

impl UnlockRing {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace a cached unlock. `Once` is a no-op (caller only verified).
    pub fn put(
        &self,
        session_token: &str,
        key_id: &str,
        passphrase: Zeroizing<String>,
        mode: CacheMode,
        ttl_minutes: u32,
    ) {
        if mode == CacheMode::Once {
            return;
        }
        let ttl = ttl_minutes.clamp(1, MAX_TTL_MINUTES);
        let now = Instant::now();
        let entry = CachedUnlock {
            passphrase,
            mode,
            expires_at: match mode {
                CacheMode::Timed => Some(now + Duration::from_secs(u64::from(ttl) * 60)),
                CacheMode::Session | CacheMode::Once => None,
            },
            last_used: now,
        };
        let mut guard = self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let ring = guard.entry(session_token.to_string()).or_default();
        ring.insert(key_id.to_string(), entry);
    }

    /// Return a cached passphrase if still valid; refreshes idle clock.
    #[must_use]
    pub fn get(&self, session_token: &str, key_id: &str) -> Option<Zeroizing<String>> {
        let mut guard = self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let ring = guard.get_mut(session_token)?;
        let now = Instant::now();
        ring.retain(|_, e| entry_alive(e, now));
        let entry = ring.get_mut(key_id)?;
        entry.last_used = now;
        Some(Zeroizing::new(entry.passphrase.to_string()))
    }

    /// Whether `key_id` is currently unlocked for this session.
    #[must_use]
    pub fn is_unlocked(&self, session_token: &str, key_id: &str) -> bool {
        self.get(session_token, key_id).is_some()
    }

    /// Clear one key or the whole session ring.
    pub fn lock(&self, session_token: &str, key_id: Option<&str>) {
        let mut guard = self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        match key_id {
            Some(id) => {
                if let Some(ring) = guard.get_mut(session_token) {
                    ring.remove(id);
                    if ring.is_empty() {
                        guard.remove(session_token);
                    }
                }
            }
            None => {
                guard.remove(session_token);
            }
        }
    }

    /// List key ids still unlocked (after idle/ttl prune).
    pub fn unlocked_ids(&self, session_token: &str) -> Vec<String> {
        let mut guard = self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(ring) = guard.get_mut(session_token) else {
            return Vec::new();
        };
        let now = Instant::now();
        ring.retain(|_, e| entry_alive(e, now));
        ring.keys().cloned().collect()
    }
}

fn entry_alive(e: &CachedUnlock, now: Instant) -> bool {
    if now.duration_since(e.last_used) > IDLE_TIMEOUT {
        return false;
    }
    !matches!(e.expires_at, Some(exp) if now >= exp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn once_does_not_cache() {
        let ring = UnlockRing::new();
        ring.put(
            "tok",
            "k1",
            Zeroizing::new("secret".into()),
            CacheMode::Once,
            10,
        );
        assert!(!ring.is_unlocked("tok", "k1"));
    }

    #[test]
    fn session_caches_until_lock() {
        let ring = UnlockRing::new();
        ring.put(
            "tok",
            "k1",
            Zeroizing::new("secret".into()),
            CacheMode::Session,
            10,
        );
        assert!(ring.is_unlocked("tok", "k1"));
        let pw = ring.get("tok", "k1").expect("cached");
        assert_eq!(pw.as_str(), "secret");
        ring.lock("tok", None);
        assert!(!ring.is_unlocked("tok", "k1"));
    }

    #[test]
    fn timed_expires() {
        let ring = UnlockRing::new();
        {
            let mut guard = ring.inner.lock().unwrap();
            let ring_map = guard.entry("tok".into()).or_default();
            ring_map.insert(
                "k1".into(),
                CachedUnlock {
                    passphrase: Zeroizing::new("x".into()),
                    mode: CacheMode::Timed,
                    expires_at: Instant::now()
                        .checked_sub(Duration::from_secs(1)),
                    last_used: Instant::now(),
                },
            );
        }
        assert!(!ring.is_unlocked("tok", "k1"));
    }

    #[test]
    fn cache_mode_parse() {
        assert_eq!(CacheMode::parse("timed").unwrap(), CacheMode::Timed);
        assert!(CacheMode::parse("forever").is_err());
    }
}
