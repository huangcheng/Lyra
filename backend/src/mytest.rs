//! Shared live-MySQL test harness (mirrors `pgtest`).
//!
//! Set `LYRA_TEST_MYSQL_URL=mysql://…` against an *ephemeral* database
//! (CI service container or a scratch docker run) and run the whole
//! `mysql_live` family via `cargo test -- --ignored mysql_live`.
//!
//! Same process-wide invariants as the postgres harness: one runtime, one
//! migrated pool, one singleton user.

#[cfg(test)]
pub(crate) mod support {
    use crate::storage::{DbPool, Storage};

    /// The one shared runtime for every `mysql_live` test.
    pub(crate) fn rt() -> &'static tokio::runtime::Runtime {
        static RT: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
        RT.get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("build shared test runtime")
        })
    }

    /// The one migrated pool + the shared singleton user id.
    pub(crate) async fn setup() -> (DbPool, String) {
        static MY: tokio::sync::OnceCell<(DbPool, String)> = tokio::sync::OnceCell::const_new();
        MY.get_or_init(|| async {
            let url = std::env::var("LYRA_TEST_MYSQL_URL").expect("LYRA_TEST_MYSQL_URL=mysql://…");
            let storage = Storage::new(&url).await.expect("connect mysql");
            storage.run_migrations().await.expect("run migrations");
            let DbPool::Mysql(pool) = storage.pool().clone() else {
                panic!("expected mysql pool");
            };
            // Idempotent across runs against a non-ephemeral test database.
            sqlx::query(
                "INSERT INTO lyra_user (id, username, password_hash, encrypted_dek) \
                 VALUES (?, 'mysql-live', 'hash', '[]') ON DUPLICATE KEY UPDATE id = id",
            )
            .bind(crate::sync::store::new_uuid_text())
            .execute(&pool)
            .await
            .unwrap();
            let user_id: String = sqlx::query_scalar("SELECT id FROM lyra_user LIMIT 1")
                .fetch_one(&pool)
                .await
                .unwrap();
            (storage.pool().clone(), user_id)
        })
        .await
        .clone()
    }
}

#[cfg(test)]
mod mysql_live {
    use super::support;

    /// Boot + full migration chain + the singleton user insert. This is the
    /// ported-schema gate: every `migrations/mysql/*.up.sql` must apply.
    #[test]
    #[ignore = "needs mysql"]
    fn migrations_apply_and_user_seeds() {
        support::rt().block_on(async {
            let (db, user_id) = support::setup().await;
            assert!(!user_id.is_empty());
            assert_eq!(db.engine_name(), "mysql");
        });
    }

    /// IMAP-shaped account + INBOX folder seed (direct inserts; credentials
    /// are opaque here — these tests never decrypt).
    async fn seed_imap(
        db: &crate::storage::DbPool,
        user_id: &str,
        email: &str,
    ) -> (String, String) {
        use sea_orm::{ConnectionTrait, Statement};
        let id = crate::sync::store::new_uuid_text();
        db.orm()
            .execute_raw(Statement::from_sql_and_values(
                db.backend(),
                r"INSERT INTO mail_account (id, user_id, email_address, protocol, auth_type, credential, is_active, sync_enabled)
                  VALUES (?, ?, ?, 'imap', 'password', 'x', 1, 1)",
                [
                    sea_orm::Value::from(id.clone()),
                    sea_orm::Value::from(user_id.to_owned()),
                    sea_orm::Value::from(email.to_owned()),
                ],
            ))
            .await
            .unwrap();
        crate::sync::store::upsert_folder(db, &id, "INBOX", None, &[])
            .await
            .unwrap();
        let folder_id = crate::sync::store::get_folder_id(db, &id, "INBOX")
            .await
            .unwrap();
        (id, folder_id)
    }

    /// The heaviest dialect surface in one roundtrip: sea-orm rendered
    /// upsert (ON DUPLICATE KEY UPDATE), JSON-as-TEXT binds, TEXT id
    /// decode, and the sync-visible message shape.
    #[test]
    #[ignore = "needs mysql"]
    fn message_upsert_roundtrip() {
        support::rt().block_on(async {
            use sea_orm::ConnectionTrait as _;
            let (db, user_id) = support::setup().await;
            let (account_id, folder_id) = seed_imap(&db, &user_id, "mysql-upsert@example.com").await;

            let msg = crate::imap::ImapMessage {
                uid: 42,
                message_id: Some("<42@mysql-live.example.com>".into()),
                subject: Some("MySQL 测试 subject".into()),
                from: Some("sender@example.com".into()),
                to: Some("to@example.com".into()),
                cc: None,
                date: None,
                in_reply_to: None,
                references: None,
                mailer: None,
                flags: vec!["\\Seen".into()],
                size: Some(1024),
                body: None,
                body_text: None,
                body_html: None,
                has_attachments: false,
                attachments: vec![],
            };
            let inserted = crate::sync::store::upsert_message(&db, &account_id, &folder_id, &msg)
                .await
                .unwrap();
            assert!(inserted, "first upsert inserts");

            let mut again = msg.clone();
            again.flags = vec!["\\Seen".into(), "\\Flagged".into()];
            let updated = crate::sync::store::upsert_message(&db, &account_id, &folder_id, &again)
                .await
                .unwrap();
            assert!(!updated, "second upsert fills in, never duplicates");

            let stmt = sea_orm::Statement::from_sql_and_values(
                db.backend(),
                "SELECT COUNT(*) AS c FROM message WHERE account_id = ? AND external_id LIKE '%:42'",
                [sea_orm::Value::from(account_id.clone())],
            );
            let row = db
                .orm()
                .query_one_raw(stmt)
                .await
                .unwrap()
                .expect("count row");
            let n: i64 = row.try_get("", "c").unwrap();
            assert_eq!(n, 1, "exactly one row after re-upsert");
        });
    }

    /// Composite-unique ON DUPLICATE KEY semantics for the spam seam.
    #[test]
    #[ignore = "needs mysql"]
    fn spam_settings_and_senders_roundtrip() {
        support::rt().block_on(async {
            let (db, user_id) = support::setup().await;
            let on = crate::spam::SpamSettings {
                enabled: true,
                learn: false,
                auto_delete: true,
                sensitivity: crate::spam::Sensitivity::Lenient,
            };
            crate::spam::save_settings(&db, &user_id, &on)
                .await
                .unwrap();
            let back = crate::spam::load_settings(&db, &user_id).await.unwrap();
            assert_eq!(back, on);

            crate::spam::add_sender(
                &db,
                &user_id,
                "spam@example.com",
                crate::spam::SenderList::Blocked,
            )
            .await
            .unwrap();
            crate::spam::add_sender(
                &db,
                &user_id,
                "spam@example.com",
                crate::spam::SenderList::Allowed,
            )
            .await
            .unwrap();
            let senders = crate::spam::list_senders(&db, &user_id).await.unwrap();
            assert_eq!(senders.len(), 1, "list switch replaces, not duplicates");
        });
    }

    /// ensure_calendar + event upsert linkage + contact row roundtrip —
    /// the PIM seams that broke on Postgres via id binds.
    #[test]
    #[ignore = "needs mysql"]
    fn pim_calendar_and_contact_rows() {
        support::rt().block_on(async {
            let (db, user_id) = support::setup().await;
            let (account_id, _folder) = seed_imap(&db, &user_id, "mysql-pim@example.com").await;

            let cal_id = crate::pim_dav::ensure_calendar(
                &db,
                &account_id,
                "https://dav.example/cal/mysql/",
                "MySQL Cal",
            )
            .await;
            assert!(!cal_id.is_empty());

            let vcard = crate::pim_write::build_vcard(
                "mysql-1",
                &crate::pim_write::NewContact {
                    display_name: "MySQL 联系人".into(),
                    email: Some("mysql@example.com".into()),
                    phone: None,
                    organisation: None,
                },
            );
            let no_photo: &crate::pim_dav::PhotoStore = &|_p| Box::pin(std::future::ready(None));
            crate::pim_dav::upsert_contact(
                &db,
                &account_id,
                "https://dav.example/ab/mysql/",
                &crate::dav_protocol::DavItem {
                    href: "/mysql-1.vcf".into(),
                    etag: Some("\"v1\"".into()),
                    data: Some(vcard),
                },
                no_photo,
            )
            .await
            .unwrap();
        });
    }
}
