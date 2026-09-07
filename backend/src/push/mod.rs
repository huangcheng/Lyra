//! Web Push (RFC 8030/8291/8292): closed-app new-mail notifications.
//!
//! See `docs/superpowers/specs/2026-09-07-lyra-web-push-design.md`.

#![allow(clippy::doc_markdown)]

// Subscriptions/baselines/prefs accessors are exercised by tests now and by
// the fan-out task in a later plan task; allow until those consumers land.
#[allow(dead_code)]
mod store;

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::auth::install_test_master_key;
    use crate::kv::{KvStore, MemoryKv};

    #[tokio::test]
    async fn vapid_identity_is_generated_once_then_reused() {
        install_test_master_key();
        let kv: Arc<dyn KvStore> = Arc::new(MemoryKv::new());

        let first = super::store::load_or_generate_vapid(&kv).await.unwrap();
        let second = super::store::load_or_generate_vapid(&kv).await.unwrap();

        assert_eq!(first.private_pem, second.private_pem);
        assert_eq!(first.public_key_b64, second.public_key_b64);
        // Uncompressed P-256 point: 65 bytes, base64url-no-pad = 87 chars.
        assert_eq!(first.public_key_b64.len(), 87);
        assert!(first.private_pem.contains("BEGIN PRIVATE KEY"));
        // The kv blob must not contain the raw PEM (encrypted at rest).
        let raw = kv.get(super::store::VAPID_KEY).await.unwrap().unwrap();
        assert!(!raw.contains("BEGIN PRIVATE KEY"));
    }
}
