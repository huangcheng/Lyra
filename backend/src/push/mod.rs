//! Web Push (RFC 8030/8291/8292): closed-app new-mail notifications.
//!
//! See `docs/superpowers/specs/2026-09-07-lyra-web-push-design.md`.

#![allow(clippy::doc_markdown)]

// Subscriptions/baselines/prefs accessors are exercised by tests now and by
// the fan-out task in a later plan task; allow until those consumers land.
#[allow(dead_code)]
mod store;

// Diff is exercised by tests now and by the fan-out task in a later plan
// task; allow until that consumer lands.
#[allow(dead_code)]
mod diff;

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

    fn msg(
        id: &str,
        message_id: Option<&str>,
        role: Option<&str>,
        thread: Option<&str>,
    ) -> crate::sync::queries::MessageResponse {
        crate::sync::queries::MessageResponse {
            id: id.into(),
            account_id: "acc".into(),
            folder_id: format!("folder-{id}"),
            folder_role: role.map(str::to_string),
            thread_id: thread.map(str::to_string),
            message_id_header: message_id.map(str::to_string),
            in_reply_to: None,
            references_headers: None,
            subject: Some(format!("Subject {id}")),
            from_address: Some(r#"[{"name":"Alice","email":"alice@example.com"}]"#.into()),
            to_addresses: None,
            cc_addresses: None,
            date: None,
            snippet: None,
            body_text: None,
            body_html: None,
            is_read: false,
            is_starred: false,
            is_draft: false,
            has_attachments: false,
            labels: None,
            remote_content_blocked: false,
            opengpg: None,
            attachments: None,
            dkim: None,
        }
    }

    #[test]
    fn diff_seeds_silently_then_notifies_only_new() {
        use super::diff::diff_new_messages;
        let messages = vec![msg("m1", Some("<a@x>"), Some("inbox"), None)];
        let first = diff_new_messages(&messages, &[], &[], &[]);
        assert!(first.seeded);
        assert!(first.fresh.is_empty());
        assert_eq!(first.new_baseline, vec!["<a@x>"]);

        // Same state again: nothing new.
        let again = diff_new_messages(&messages, &first.new_baseline, &[], &[]);
        assert!(!again.seeded);
        assert!(again.fresh.is_empty());

        // New message arrives (newest first).
        let messages = vec![
            msg("m2", Some("<b@x>"), Some("inbox"), None),
            msg("m1", Some("<a@x>"), Some("inbox"), None),
        ];
        let out = diff_new_messages(&messages, &first.new_baseline, &[], &[]);
        assert_eq!(out.fresh.len(), 1);
        assert_eq!(out.fresh[0].identity, "<b@x>");
        assert_eq!(out.fresh[0].title, "Alice");
        assert_eq!(out.fresh[0].body, "Subject m2");
    }

    #[test]
    fn diff_incoming_roles_and_identity_fallback() {
        use super::diff::diff_new_messages;
        let messages = vec![
            msg("sent1", Some("<s@x>"), Some("sent"), None),
            msg("arch1", Some("<ar@x>"), Some("archive"), None),
            msg("custom1", Some("<c@x>"), None, None),
            msg("norole", None, Some("inbox"), None), // identity falls back to row id
        ];
        let seeded = diff_new_messages(&messages, &[], &[], &[]);
        assert_eq!(seeded.new_baseline, vec!["<ar@x>", "<c@x>", "norole"]);

        let baseline = vec!["<c@x>".to_string()];
        let out = diff_new_messages(&messages, &baseline, &[], &[]);
        let ids: Vec<&str> = out.fresh.iter().map(|c| c.identity.as_str()).collect();
        assert_eq!(ids, vec!["<ar@x>", "norole"]);
    }

    #[test]
    fn diff_mutes_gate_sends_but_not_baseline() {
        use super::diff::diff_new_messages;
        let messages = vec![
            msg("m1", Some("<a@x>"), Some("inbox"), None),
            msg("m2", Some("<b@x>"), Some("inbox"), Some("thread-9")),
        ];
        let out = diff_new_messages(
            &messages,
            &["<old@x>".to_string()],
            &["folder-m1".to_string()],
            &["thread-9".to_string()],
        );
        assert!(out.fresh.is_empty(), "both muted");
        assert_eq!(
            out.new_baseline,
            vec!["<a@x>", "<b@x>"],
            "mutes never touch the baseline"
        );
    }

    #[test]
    fn diff_sender_label_matches_frontend_edge_cases() {
        use super::diff::diff_new_messages;
        // Frontend `senderLabel`: bare-string array entry is returned as-is.
        let mut string_entry = msg("m1", Some("<a@x>"), Some("inbox"), None);
        string_entry.from_address = Some(r#"["alice@x.com"]"#.into());
        // Empty-string name is returned unchanged (?? falls through only on
        // null/undefined); the fan-out's empty-title fallback handles it.
        let mut empty_name = msg("m2", Some("<b@x>"), Some("inbox"), None);
        empty_name.from_address = Some(r#"[{"name":"","email":"e@x.com"}]"#.into());
        // Object with neither name nor email yields "".
        let mut no_fields = msg("m3", Some("<c@x>"), Some("inbox"), None);
        no_fields.from_address = Some(r"[{}]".into());
        // Empty array falls through to the raw string.
        let mut empty_array = msg("m4", Some("<d@x>"), Some("inbox"), None);
        empty_array.from_address = Some("[]".into());
        // Null name falls through to email.
        let mut null_name = msg("m5", Some("<e@x>"), Some("inbox"), None);
        null_name.from_address = Some(r#"[{"name":null,"email":"e@x.com"}]"#.into());

        let messages = vec![string_entry, empty_name, no_fields, empty_array, null_name];
        let out = diff_new_messages(&messages, &["<old@x>".to_string()], &[], &[]);
        let titles: Vec<&str> = out.fresh.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, vec!["alice@x.com", "", "", "[]", "e@x.com"]);
    }

    #[test]
    fn diff_identity_falls_back_to_row_id_on_empty_header() {
        use super::diff::diff_new_messages;
        // Frontend `messageIdentity` is `msg.messageIdHeader || msg.id`: an
        // empty header string is falsy and falls back to the row id.
        let messages = vec![msg("row-1", Some(""), Some("inbox"), None)];
        let seeded = diff_new_messages(&messages, &[], &[], &[]);
        assert_eq!(seeded.new_baseline, vec!["row-1"]);

        let out = diff_new_messages(&messages, &["<old@x>".to_string()], &[], &[]);
        assert_eq!(out.fresh.len(), 1);
        assert_eq!(out.fresh[0].identity, "row-1");
        assert_eq!(out.fresh[0].id, "row-1");
    }
}
