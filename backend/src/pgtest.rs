//! Shared live-PostgreSQL test harness.
//!
//! Every SQL-bearing seam module has (or gets) a `mod postgres_live` with
//! `#[ignore = "needs postgres"]` tests: SQLite's loose typing forgives
//! statements PostgreSQL refuses to prepare, so the SQLite suite alone can
//! never green-light dual-DB SQL. The CI postgres job runs them all via the
//! single filter `-- --ignored postgres_live`.
//!
//! Two process-wide invariants live here because libtest runs tests in
//! parallel across *modules*:
//!
//! - **One tokio runtime.** sqlx pools pin background tasks to the runtime
//!   that created them; a pool shared across per-test `#[tokio::test]`
//!   runtimes outlives its tasks and later acquires time out. All live
//!   tests therefore `pgtest::rt().block_on(async { … })`.
//! - **One pool + one migration pass.** Concurrent migrations race on
//!   catalog objects (`pg_type_typname_nsp_index` duplicates), and
//!   `lyra_user` is a singleton table, so all tests share one user and only
//!   per-test accounts/folders/messages under fresh UUIDs.
//!
//! Set `LYRA_TEST_DATABASE_URL=postgres://…` against an *ephemeral*
//! database (CI service container); seeding is idempotent but rows persist.

#[cfg(test)]
pub(crate) mod support {
    use crate::imap::ImapMessage;
    use crate::storage::{DbPool, Storage};

    /// The one shared runtime for every `postgres_live` test.
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
        static PG: tokio::sync::OnceCell<(DbPool, String)> = tokio::sync::OnceCell::const_new();
        PG.get_or_init(|| async {
            let url = std::env::var("LYRA_TEST_DATABASE_URL")
                .expect("LYRA_TEST_DATABASE_URL=postgres://…");
            let storage = Storage::new(&url).await.expect("connect postgres");
            storage.run_migrations().await.expect("run migrations");
            let DbPool::Postgres(pool) = storage.pool().clone() else {
                panic!("expected postgres pool");
            };
            // Idempotent across runs against a non-ephemeral test database.
            sqlx::query(
                "INSERT INTO lyra_user (id, username, password_hash, encrypted_dek) \
                 VALUES ($1::uuid, 'pg-live', 'hash', '[]') ON CONFLICT DO NOTHING",
            )
            .bind(crate::sync::store::new_uuid_text())
            .execute(&pool)
            .await
            .unwrap();
            let user_id: String = sqlx::query_scalar("SELECT id::text FROM lyra_user LIMIT 1")
                .fetch_one(&pool)
                .await
                .unwrap();
            (storage.pool().clone(), user_id)
        })
        .await
        .clone()
    }

    /// Fresh IMAP account row under the shared user; returns its id.
    pub(crate) async fn seed_account(db: &DbPool, user_id: &str, email: &str) -> String {
        let account_id = crate::sync::store::new_uuid_text();
        let DbPool::Postgres(pool) = db else {
            panic!("expected postgres pool");
        };
        sqlx::query(
            "INSERT INTO mail_account (\
                 id, user_id, display_name, email_address, protocol, auth_type, credential, \
                 imap_host, imap_port, imap_security, is_active, sync_enabled\
             ) VALUES ($1::uuid, $2::uuid, $3, $4, 'imap', 'password', '{}', \
                       'imap.example.com', 993, 'tls', true, true)",
        )
        .bind(&account_id)
        .bind(user_id)
        .bind(format!("PG Live {email}"))
        .bind(email)
        .execute(pool)
        .await
        .unwrap();
        account_id
    }

    /// Fresh INBOX folder for the account; returns its id.
    pub(crate) async fn seed_inbox(db: &DbPool, account_id: &str) -> String {
        crate::sync::store::upsert_folder(db, account_id, "INBOX", None, &[])
            .await
            .unwrap();
        crate::sync::store::get_folder_id(db, account_id, "INBOX")
            .await
            .unwrap()
    }

    /// Minimal envelope for store-seam seeding.
    pub(crate) fn message(uid: u32, subject: &str, from: &str) -> ImapMessage {
        ImapMessage {
            uid,
            message_id: Some(format!("<{uid}@pg-live.example.com>")),
            subject: Some(subject.into()),
            from: Some(from.into()),
            to: Some("to@example.com".into()),
            cc: None,
            date: None,
            in_reply_to: None,
            references: None,
            flags: vec!["\\Seen".into()],
            size: Some(1024),
            body: None,
            body_text: None,
            body_html: None,
            has_attachments: false,
            attachments: vec![],
        }
    }
}
