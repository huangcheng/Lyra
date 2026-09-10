//! Web Push (RFC 8030/8291/8292): closed-app new-mail notifications.
//!
//! See `docs/superpowers/specs/2026-09-07-lyra-web-push-design.md`.

#![allow(clippy::doc_markdown)]

mod diff;
mod fanout;
mod http;
mod send;
mod store;

pub(crate) use fanout::spawn_fanout;
pub(crate) use http::routes;

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

    /// Spin up a one-shot TCP server that answers the first HTTP request
    /// with `status`, capturing the raw request for assertions.
    async fn mock_push_server(status: u16) -> (String, tokio::sync::oneshot::Receiver<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 65536];
            let n = socket.read(&mut buf).await.unwrap();
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            let reason = match status {
                201 => "Created",
                410 => "Gone",
                403 => "Forbidden",
                500 => "Internal Server Error",
                _ => "OK",
            };
            let response = format!("HTTP/1.1 {status} {reason}\r\nContent-Length: 0\r\n\r\n");
            socket.write_all(response.as_bytes()).await.unwrap();
            let _ = tx.send(request);
        });
        (format!("http://{addr}/wpush/v1/abc"), rx)
    }

    /// A real P-256 subscription keypair (p256dh) + 16-byte auth secret,
    /// base64url-no-pad, as a browser would produce them.
    fn test_subscription(endpoint: String) -> super::store::StoredSubscription {
        use base64::Engine;
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let ua = p256::ecdsa::SigningKey::random(&mut rand::rngs::OsRng);
        let point = ua.verifying_key().to_encoded_point(false);
        super::store::StoredSubscription {
            endpoint,
            keys: super::store::StoredKeys {
                p256dh: URL_SAFE_NO_PAD.encode(point.as_bytes()),
                auth: URL_SAFE_NO_PAD.encode([7u8; 16]),
            },
            created_at: "2026-09-07T00:00:00Z".into(),
        }
    }

    fn test_vapid_pem() -> String {
        use p256::pkcs8::{EncodePrivateKey, LineEnding};
        p256::ecdsa::SigningKey::random(&mut rand::rngs::OsRng)
            .to_pkcs8_pem(LineEnding::LF)
            .unwrap()
            .to_string()
    }

    #[tokio::test]
    async fn send_builds_encrypted_request_and_maps_statuses() {
        use super::send::{SendOutcome, send_push};
        let client = reqwest::Client::new();
        let vapid_pem = test_vapid_pem();

        // 201 → Delivered, with the RFC 8291/8292 headers on the wire.
        let (endpoint, rx) = mock_push_server(201).await;
        let outcome = send_push(
            &client,
            &vapid_pem,
            "mailto:test@example.com",
            &test_subscription(endpoint),
            r#"{"title":"t"}"#,
        )
        .await
        .unwrap();
        assert_eq!(outcome, SendOutcome::Delivered);
        let request = rx.await.unwrap();
        assert!(
            request.starts_with("POST /wpush/v1/abc HTTP/1.1"),
            "{request}"
        );
        // reqwest/hyper lowercase header names on the wire.
        assert!(request.contains("\r\nttl: 3600\r\n"), "{request}");
        assert!(request.contains("\r\nurgency: normal\r\n"), "{request}");
        assert!(
            request.contains("\r\ncontent-encoding: aes128gcm\r\n"),
            "{request}"
        );
        assert!(request.contains("\r\nauthorization: vapid t="), "{request}");
        // aes128gcm (RFC 8291/8188) embeds the salt and record size in the
        // encrypted body, so there is no `Encryption` header (unlike the
        // legacy aesgcm encoding). The binary body follows the blank line.
        let body = request.split("\r\n\r\n").nth(1).unwrap_or("");
        assert!(body.len() > 16, "encrypted payload present: {request}");

        // 410 → Gone (subscription must be deleted).
        let (endpoint, _rx) = mock_push_server(410).await;
        let outcome = send_push(
            &client,
            &vapid_pem,
            "mailto:test@example.com",
            &test_subscription(endpoint),
            "{}",
        )
        .await
        .unwrap();
        assert_eq!(outcome, SendOutcome::Gone);

        // 403 → Unauthorized (VAPID misconfiguration).
        let (endpoint, _rx) = mock_push_server(403).await;
        let outcome = send_push(
            &client,
            &vapid_pem,
            "mailto:test@example.com",
            &test_subscription(endpoint),
            "{}",
        )
        .await
        .unwrap();
        assert_eq!(outcome, SendOutcome::Unauthorized);

        // 500 → Failed, no panic.
        let (endpoint, _rx) = mock_push_server(500).await;
        let outcome = send_push(
            &client,
            &vapid_pem,
            "mailto:test@example.com",
            &test_subscription(endpoint),
            "{}",
        )
        .await
        .unwrap();
        assert!(matches!(outcome, SendOutcome::Failed(_)));
    }

    /// Seed an in-memory SQLite db with one user/account/INBOX and `n`
    /// messages (uid 1..=n, message-ids <1@x>..<n@x>), oldest first.
    async fn seed_mail_db(n: u32) -> (crate::storage::DbPool, String, String) {
        let storage = crate::storage::Storage::new("sqlite::memory:")
            .await
            .unwrap();
        storage.run_migrations().await.unwrap();
        let db = storage.pool().clone();
        let crate::storage::DbPool::Sqlite(pool) = &db else {
            panic!("sqlite")
        };
        let user_id = uuid::Uuid::new_v4().to_string();
        let account_id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO lyra_user (id, username, password_hash, encrypted_dek) \
             VALUES (?, ?, 'hash', '[]')",
        )
        .bind(&user_id)
        .bind(format!("push-{user_id}"))
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO mail_account (\
                 id, user_id, display_name, email_address, protocol, auth_type, \
                 credential, imap_host, imap_port, imap_security, is_active, sync_enabled\
             ) VALUES (?, ?, 'Push', 'push@example.com', 'imap', 'password', \
                       'cred', 'imap.example.com', 993, 'tls', 1, 1)",
        )
        .bind(&account_id)
        .bind(&user_id)
        .execute(pool)
        .await
        .unwrap();
        crate::sync::upsert_folder(&db, &account_id, "INBOX", None, &[])
            .await
            .unwrap();
        let folder_id = crate::sync::get_folder_id(&db, &account_id, "INBOX")
            .await
            .unwrap();
        for uid in 1..=n {
            crate::sync::upsert_message(
                &db,
                &account_id,
                &folder_id,
                &crate::imap::ImapMessage {
                    uid,
                    message_id: Some(format!("<{uid}@x>")),
                    subject: Some(format!("Mail {uid}")),
                    from: Some("alice@example.com".into()),
                    to: Some("me@example.com".into()),
                    cc: None,
                    date: Some(format!("2025-09-07T12:00:{uid:02}Z")),
                    in_reply_to: None,
                    references: None,
                    flags: vec![],
                    size: Some(1),
                    body: None,
                    body_text: None,
                    body_html: None,
                    has_attachments: false,
                    attachments: vec![],
                },
            )
            .await
            .unwrap();
        }
        (db, user_id, account_id)
    }

    #[tokio::test]
    async fn fanout_seeds_then_pushes_new_mail_and_drops_gone_subs() {
        use super::fanout::fan_out_account;
        install_test_master_key();
        let kv: Arc<dyn KvStore> = Arc::new(MemoryKv::new());
        let client = reqwest::Client::new();

        let (db, user_id, account_id) = seed_mail_db(1).await;

        // Run 1: one live sub (201) + one dead sub (410). First run seeds
        // silently — no sends — so the dead sub survives (no send attempted).
        let (ep1, _rx1) = mock_push_server(201).await;
        let (gone_endpoint, _gone_rx) = mock_push_server(410).await;
        upsert_subscription(&kv, &user_id, test_subscription(ep1))
            .await
            .unwrap();
        upsert_subscription(&kv, &user_id, test_subscription(gone_endpoint.clone()))
            .await
            .unwrap();
        fan_out_account(&db, &kv, &client, &account_id, "mailto:test@example.com")
            .await
            .unwrap();
        assert_eq!(
            load_baseline(&kv, &account_id).await.unwrap(),
            vec!["<1@x>"]
        );
        assert_eq!(load_subscriptions(&kv, &user_id).await.unwrap().len(), 2);

        // New mail arrives; run 2 sends. Mock servers are one-shot, so
        // register a fresh live endpoint and a fresh dead (410) endpoint.
        let (live2, rx2) = mock_push_server(201).await;
        let (gone2, _g2) = mock_push_server(410).await;
        upsert_subscription(&kv, &user_id, test_subscription(live2))
            .await
            .unwrap();
        upsert_subscription(&kv, &user_id, test_subscription(gone2.clone()))
            .await
            .unwrap();

        let folder_id = crate::sync::get_folder_id(&db, &account_id, "INBOX")
            .await
            .unwrap();
        crate::sync::upsert_message(
            &db,
            &account_id,
            &folder_id,
            &crate::imap::ImapMessage {
                uid: 2,
                message_id: Some("<2@x>".into()),
                subject: Some("Mail 2".into()),
                from: Some("alice@example.com".into()),
                to: Some("me@example.com".into()),
                cc: None,
                date: Some("2025-09-07T12:05:00Z".into()),
                in_reply_to: None,
                references: None,
                flags: vec![],
                size: Some(1),
                body: None,
                body_text: None,
                body_html: None,
                has_attachments: false,
                attachments: vec![],
            },
        )
        .await
        .unwrap();

        fan_out_account(&db, &kv, &client, &account_id, "mailto:test@example.com")
            .await
            .unwrap();

        // Live endpoint got one POST carrying the encrypted payload.
        let request = rx2.await.unwrap();
        assert!(
            request.starts_with("POST /wpush/v1/abc HTTP/1.1"),
            "{request}"
        );
        // Dead endpoints were pruned (run-1's 410 listener is consumed, but
        // gone2 answered this run); the two live subscriptions remain.
        let endpoints: Vec<String> = load_subscriptions(&kv, &user_id)
            .await
            .unwrap()
            .into_iter()
            .map(|s| s.endpoint)
            .collect();
        assert!(
            !endpoints.contains(&gone2),
            "410 endpoint pruned: {endpoints:?}"
        );
        assert_eq!(
            load_baseline(&kv, &account_id).await.unwrap(),
            vec!["<2@x>", "<1@x>"]
        );
    }

    fn push_test_config() -> crate::config::Config {
        crate::config::Config {
            listen_addr: "127.0.0.1:0".into(),
            database_url: "sqlite::memory:".into(),
            data_dir: std::env::temp_dir().to_string_lossy().into_owned(),
            min_password_length: 8,
            sync_max_concurrent: 3,
            sync_poll_secs: 300,
            max_attachment_bytes: 25 * 1024 * 1024,
            redis_url: None,
            sentry_dsn: None,
            sentry_frontend_dsn: None,
            sentry_traces_sample_rate: 0.0,
            master_key: crate::auth::TEST_MASTER_KEY.to_vec(),
            ms_oauth: None,
            yandex_oauth: None,
            captcha: crate::config::CaptchaConfig::None,
            vapid_subject: "mailto:test@example.com".into(),
        }
    }

    fn push_state(db: crate::storage::DbPool, kv: Arc<dyn KvStore>) -> crate::auth::AuthState {
        install_test_master_key();
        crate::auth::AuthState::new(
            db,
            &push_test_config(),
            Arc::new(crate::kernel::App::new()),
            kv,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn vapid_key_endpoint_returns_public_key() {
        let kv: Arc<dyn KvStore> = Arc::new(MemoryKv::new());
        let storage = crate::storage::Storage::new("sqlite::memory:")
            .await
            .unwrap();
        storage.run_migrations().await.unwrap();
        let state = push_state(storage.pool().clone(), kv);

        let axum::Json(body) = super::http::get_vapid_key(
            axum::extract::State(state),
            crate::auth::AuthUser("alice".into()),
        )
        .await
        .unwrap();
        assert_eq!(body.public_key.len(), 87);
    }

    #[tokio::test]
    async fn subscription_put_and_delete() {
        let kv: Arc<dyn KvStore> = Arc::new(MemoryKv::new());
        let storage = crate::storage::Storage::new("sqlite::memory:")
            .await
            .unwrap();
        storage.run_migrations().await.unwrap();
        let state = push_state(storage.pool().clone(), kv.clone());

        super::http::put_subscription(
            axum::extract::State(state.clone()),
            crate::auth::AuthUser("alice".into()),
            axum::Json(super::http::PutSubscription {
                endpoint: "https://push.example/abc".into(),
                keys: super::store::StoredKeys {
                    p256dh: "p".into(),
                    auth: "a".into(),
                },
            }),
        )
        .await
        .unwrap();
        assert_eq!(
            super::store::load_subscriptions(&kv, "alice")
                .await
                .unwrap()
                .len(),
            1
        );

        super::http::delete_subscription(
            axum::extract::State(state),
            crate::auth::AuthUser("alice".into()),
            axum::Json(super::http::DeleteSubscription {
                endpoint: "https://push.example/abc".into(),
            }),
        )
        .await
        .unwrap();
        assert!(
            super::store::load_subscriptions(&kv, "alice")
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn put_subscription_rejects_non_https_endpoint() {
        let kv: Arc<dyn KvStore> = Arc::new(MemoryKv::new());
        let storage = crate::storage::Storage::new("sqlite::memory:")
            .await
            .unwrap();
        storage.run_migrations().await.unwrap();
        let state = push_state(storage.pool().clone(), kv);

        let err = super::http::put_subscription(
            axum::extract::State(state),
            crate::auth::AuthUser("alice".into()),
            axum::Json(super::http::PutSubscription {
                endpoint: "ftp://evil.example/x".into(),
                keys: super::store::StoredKeys {
                    p256dh: "p".into(),
                    auth: "a".into(),
                },
            }),
        )
        .await
        .unwrap_err();
        // 400
        assert!(matches!(err, super::http::PushHttpError::BadRequest(_)));
    }
}
