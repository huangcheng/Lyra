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

    use super::store::{
        StoredKeys, StoredPrefs, StoredSubscription, load_baseline, load_prefs, load_subscriptions,
        remove_subscription, save_baseline, save_prefs, upsert_subscription,
    };

    fn sub(endpoint: &str) -> StoredSubscription {
        StoredSubscription {
            endpoint: endpoint.to_string(),
            keys: StoredKeys {
                p256dh: "p256dh".into(),
                auth: "auth".into(),
            },
            created_at: "2026-09-07T00:00:00Z".into(),
        }
    }

    #[tokio::test]
    async fn subscriptions_upsert_remove_and_cap() {
        let kv: Arc<dyn KvStore> = Arc::new(MemoryKv::new());

        upsert_subscription(&kv, "u1", sub("https://push.example/a"))
            .await
            .unwrap();
        upsert_subscription(&kv, "u1", sub("https://push.example/b"))
            .await
            .unwrap();
        // Same endpoint upserts instead of duplicating.
        upsert_subscription(&kv, "u1", sub("https://push.example/a"))
            .await
            .unwrap();
        let subs = load_subscriptions(&kv, "u1").await.unwrap();
        assert_eq!(subs.len(), 2);
        assert_eq!(subs[1].endpoint, "https://push.example/a");

        // Users are isolated.
        assert!(load_subscriptions(&kv, "u2").await.unwrap().is_empty());

        // Cap at 10, dropping the oldest.
        for i in 0..12 {
            upsert_subscription(&kv, "u1", sub(&format!("https://push.example/{i}")))
                .await
                .unwrap();
        }
        let subs = load_subscriptions(&kv, "u1").await.unwrap();
        assert_eq!(subs.len(), 10);
        assert_eq!(subs[0].endpoint, "https://push.example/2");

        assert!(
            remove_subscription(&kv, "u1", "https://push.example/2")
                .await
                .unwrap()
        );
        assert!(
            !remove_subscription(&kv, "u1", "https://push.example/nope")
                .await
                .unwrap()
        );
        assert_eq!(load_subscriptions(&kv, "u1").await.unwrap().len(), 9);
    }

    #[tokio::test]
    async fn baseline_roundtrips_and_caps_at_15() {
        let kv: Arc<dyn KvStore> = Arc::new(MemoryKv::new());
        assert!(load_baseline(&kv, "acc").await.unwrap().is_empty());

        let ids: Vec<String> = (0..20).map(|i| format!("<{i}@example.com>")).collect();
        save_baseline(&kv, "acc", &ids).await.unwrap();
        let loaded = load_baseline(&kv, "acc").await.unwrap();
        assert_eq!(loaded.len(), 15);
        assert_eq!(loaded[0], "<0@example.com>");
    }

    #[tokio::test]
    async fn prefs_default_and_roundtrip() {
        let kv: Arc<dyn KvStore> = Arc::new(MemoryKv::new());
        let defaults = load_prefs(&kv, "u1").await.unwrap();
        assert_eq!(
            defaults,
            StoredPrefs {
                muted_folder_ids: vec![],
                muted_thread_ids: vec![],
                locale: "en".into()
            }
        );

        let prefs = StoredPrefs {
            muted_folder_ids: vec!["f1".into()],
            muted_thread_ids: vec!["t1".into()],
            locale: "zh".into(),
        };
        save_prefs(&kv, "u1", &prefs).await.unwrap();
        assert_eq!(load_prefs(&kv, "u1").await.unwrap(), prefs);
    }
}
