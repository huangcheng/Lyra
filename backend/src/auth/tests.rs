use std::sync::Arc;

use axum::Json;
use axum::extract::{FromRequestParts, State};
use axum::http::{StatusCode, header};

use crate::auth::dek::{TEST_MASTER_KEY, install_master_key, install_test_master_key, master_key};
use crate::crypto::{self, CryptoError};
use crate::kv::{KvStore, MemoryKv};
use crate::storage::DbPool;

use super::db::{find_user_by_id, insert_user, is_unique_violation};
use super::handlers::{
    auth_bootstrap, auth_login, change_password, patch_preferences, totp_disable, totp_enroll,
    totp_enroll_confirm, totp_verify,
};
use super::password::{hash_password, validate_password, verify_password};
use super::session::{
    SessionStore, fetch_sess_epoch, is_rate_limited, pending_key, record_failed_attempt, sess_key,
    tok_key,
};
use super::state::AuthState;
use super::totp::{build_totp, decrypt_totp_secret, encrypt_totp_secret};
use super::types::{
    BootstrapRequest, ChangePasswordRequest, LoginRequest, PreferencesRequest, TotpDisableRequest,
    TotpEnrollConfirmRequest, TotpVerifyRequest,
};
use super::{
    AuthUser, PENDING_TTL_SECS, RATE_LIMIT_MAX_ATTEMPTS, SESSION_TTL_SECS, generate_token,
    invalidate_user_sessions,
};

#[test]
fn password_validation() {
    assert!(validate_password("Ab1", 8).is_err());
    assert!(validate_password("abcdefgh1", 8).is_err());
    assert!(validate_password("ABCDEFGH1", 8).is_err());
    assert!(validate_password("Abcdefgh", 8).is_err());
    assert!(validate_password("Abcdefg1", 8).is_ok());
    assert!(validate_password("Abcdefg1!@#", 8).is_ok());
}

#[tokio::test]
async fn password_hashing_roundtrip() {
    let password = "TestPassw0rd!";
    let hash = hash_password(password).await.unwrap();
    assert!(verify_password(password, &hash).await.unwrap());
    assert!(!verify_password("WrongPassw0rd!", &hash).await.unwrap());
}

#[test]
fn install_master_key_first_wins() {
    install_master_key(TEST_MASTER_KEY);
    install_master_key(b"different-key-that-is-also-32-bytes!!");
    assert_eq!(master_key().unwrap(), TEST_MASTER_KEY);
}

#[test]
fn token_generation() {
    let token1 = generate_token();
    let token2 = generate_token();
    assert_ne!(token1, token2);
    assert!(token1.len() > 20);
}

#[test]
fn totp_roundtrip() {
    let secret_bytes = totp_rs::Secret::generate_secret().to_bytes().unwrap();
    let secret_base32 = base32::encode(base32::Alphabet::Rfc4648 { padding: false }, &secret_bytes);
    let totp = build_totp(&secret_base32, "testuser").unwrap();
    let code = totp.generate_current().unwrap();
    assert!(totp.check_current(&code).unwrap());
}

#[tokio::test]
async fn session_store_operations() {
    let db = test_pool().await;
    seed_user(&db, "user-1").await;
    let store = SessionStore::new(db, Arc::new(MemoryKv::new()));
    let token = store.create_session("user-1").await.unwrap();
    assert_eq!(store.get_session(&token).await, Some("user-1".to_string()));
    store.remove_session(&token).await;
    assert_eq!(store.get_session(&token).await, None);
}

#[tokio::test]
async fn pending_session_promotion() {
    let db = test_pool().await;
    seed_user(&db, "user-1").await;
    let store = SessionStore::new(db, Arc::new(MemoryKv::new()));
    let pending_token = store.create_pending_session("user-1").await.unwrap();
    assert!(store.get_pending_session(&pending_token).await.is_some());
    let session_token = store
        .promote_pending_session(&pending_token)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        store.get_session(&session_token).await,
        Some("user-1".to_string())
    );
    assert!(store.get_pending_session(&pending_token).await.is_none());
}

#[tokio::test]
async fn bump_epoch_invalidates_old_tokens() {
    let db = test_pool().await;
    seed_user(&db, "user-1").await;
    let kv = Arc::new(MemoryKv::new());
    let store = SessionStore::new(db.clone(), Arc::clone(&kv) as Arc<dyn KvStore>);
    let token = store.create_session("user-1").await.unwrap();
    assert_eq!(store.get_session(&token).await, Some("user-1".to_string()));

    let epoch_before = fetch_sess_epoch(&db, "user-1").await.unwrap();
    invalidate_user_sessions(&db, kv.as_ref(), "user-1")
        .await
        .unwrap();
    let epoch_after = fetch_sess_epoch(&db, "user-1").await.unwrap();
    assert_eq!(epoch_after, epoch_before + 1);

    assert_eq!(store.get_session(&token).await, None);
    // New sessions at epoch 1 still work.
    let token2 = store.create_session("user-1").await.unwrap();
    assert_eq!(store.get_session(&token2).await, Some("user-1".to_string()));
}

async fn test_pool() -> DbPool {
    let storage = crate::storage::Storage::new("sqlite::memory:")
        .await
        .unwrap();
    storage.run_migrations().await.unwrap();
    storage.pool().clone()
}

async fn seed_user(db: &DbPool, id: &str) {
    match db {
        DbPool::Sqlite(pool) => {
            sqlx::query("INSERT INTO lyra_user (id, username, password_hash) VALUES (?, ?, ?)")
                .bind(id)
                .bind(format!("user-{id}"))
                .bind("hash")
                .execute(pool)
                .await
                .unwrap();
        }
        #[cfg(feature = "postgres")]
        DbPool::Postgres(_) => panic!("expected sqlite in tests"),
    }
}

// ── DEK hierarchy & TOTP-at-rest tests ──────────────────────────

fn test_config() -> crate::config::Config {
    crate::config::Config {
        listen_addr: "127.0.0.1:0".into(),
        database_url: "sqlite::memory:".into(),
        data_dir: std::env::temp_dir().to_string_lossy().into_owned(),
        min_password_length: 8,
        sync_max_concurrent: 3,
        sync_poll_secs: 300,
        max_attachment_bytes: 25 * 1024 * 1024,
        redis_url: None,
        master_key: TEST_MASTER_KEY.to_vec(),
        ms_oauth: None,
        yandex_oauth: None,
    }
}

fn test_state(db: DbPool) -> AuthState {
    AuthState::new(
        db,
        &test_config(),
        Arc::new(crate::kernel::App::new()),
        Arc::new(MemoryKv::new()),
    )
    .unwrap()
}

fn sqlite_pool(db: &DbPool) -> &sqlx::SqlitePool {
    match db {
        DbPool::Sqlite(pool) => pool,
        #[cfg(feature = "postgres")]
        DbPool::Postgres(_) => panic!("expected sqlite in tests"),
    }
}

/// Seed a user with a freshly generated DEK, stored wrapped.
/// Returns the plaintext DEK for assertions.
async fn seed_user_with_dek(db: &DbPool, id: &str) -> Vec<u8> {
    install_test_master_key();
    seed_user(db, id).await;
    let dek = crypto::generate_key();
    let kek = crypto::derive_user_kek(TEST_MASTER_KEY, id);
    let wrapped = crypto::wrap_dek(&kek, &dek).unwrap();
    sqlx::query("UPDATE lyra_user SET encrypted_dek = ? WHERE id = ?")
        .bind(&wrapped)
        .bind(id)
        .execute(sqlite_pool(db))
        .await
        .unwrap();
    dek.to_vec()
}

#[tokio::test]
async fn bootstrap_creates_and_persists_encrypted_dek() {
    let db = test_pool().await;
    let state = test_state(db.clone());

    auth_bootstrap(
        State(state),
        Json(BootstrapRequest {
            username: "alice".into(),
            password: "Str0ngPass1".into(),
            display_name: None,
            locale: None,
        }),
    )
    .await
    .map(|(_status, Json(_))| ())
    .unwrap();

    let pool = sqlite_pool(&db);
    let (user_id, stored): (String, String) =
        sqlx::query_as("SELECT id, encrypted_dek FROM lyra_user")
            .fetch_one(pool)
            .await
            .unwrap();

    // At rest it is a wrapped-key blob (JSON ciphertext + nonce), not raw key material.
    let blob: crypto::EncryptedCredential = serde_json::from_str(&stored).unwrap();
    assert!(!blob.ciphertext.is_empty());
    assert!(!blob.nonce.is_empty());

    // Unwrap round-trip via the public lookup matches a manual unwrap.
    let dek = AuthState::get_user_dek(&db, &user_id).await.unwrap();
    assert_eq!(dek.len(), 32);
    let kek = crypto::derive_user_kek(TEST_MASTER_KEY, &user_id);
    assert_eq!(crypto::unwrap_dek(&kek, &stored).unwrap(), dek);
}

#[tokio::test]
async fn two_users_get_different_deks() {
    // The single-user guard (migration 0005) forbids two rows in one
    // database, so each user lives in its own in-memory DB here.
    let db_a = test_pool().await;
    let db_b = test_pool().await;
    let dek_a = seed_user_with_dek(&db_a, "user-a").await;
    let dek_b = seed_user_with_dek(&db_b, "user-b").await;

    assert_eq!(
        AuthState::get_user_dek(&db_a, "user-a").await.unwrap(),
        dek_a
    );
    assert_eq!(
        AuthState::get_user_dek(&db_b, "user-b").await.unwrap(),
        dek_b
    );
    assert_ne!(dek_a, dek_b);

    // A user's wrapped DEK cannot be unwrapped with another user's KEK.
    let stored_a: String =
        sqlx::query_scalar("SELECT encrypted_dek FROM lyra_user WHERE id = 'user-a'")
            .fetch_one(sqlite_pool(&db_a))
            .await
            .unwrap();
    let kek_b = crypto::derive_user_kek(TEST_MASTER_KEY, "user-b");
    assert!(crypto::unwrap_dek(&kek_b, &stored_a).is_err());
}

#[tokio::test]
async fn bootstrap_rejects_second_user_with_conflict() {
    let db = test_pool().await;
    let state = test_state(db.clone());
    bootstrap_alice(&state).await;

    let err = auth_bootstrap(
        State(state),
        Json(BootstrapRequest {
            username: "bob".into(),
            password: "Str0ngPass1".into(),
            display_name: None,
            locale: None,
        }),
    )
    .await
    .expect_err("second bootstrap must fail");
    assert_eq!(err.status(), StatusCode::CONFLICT);

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM lyra_user")
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
    assert_eq!(count, 1, "exactly one user may exist");
}

#[tokio::test]
async fn second_user_insert_violates_singleton_guard() {
    let db = test_pool().await;
    insert_user(&db, "id-1", "alice", "hash", None, "en", "dek")
        .await
        .unwrap();

    // A second row with a *different* username must be rejected by the
    // singleton unique index, not merely by UNIQUE(username).
    let err = insert_user(&db, "id-2", "bob", "hash", None, "en", "dek")
        .await
        .unwrap_err();
    assert!(
        is_unique_violation(&err),
        "expected a unique violation, got {err}"
    );

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM lyra_user")
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn unknown_user_dek_is_a_typed_error() {
    let db = test_pool().await;
    install_test_master_key();
    let err = AuthState::get_user_dek(&db, "no-such-user")
        .await
        .unwrap_err();
    assert!(matches!(err, CryptoError::MissingDek));
}

#[tokio::test]
async fn legacy_user_without_dek_rotates_padded_master_key_credentials() {
    let db = test_pool().await;
    install_test_master_key();
    seed_user(&db, "user-legacy").await;

    // Pre-DEK accounts were encrypted with the first 32 bytes of
    // LYRA_MASTER_KEY (or the old hardcoded default).
    let mut legacy_key = [0u8; 32];
    let default = b"lyra-default-master-key-for-dev-only";
    legacy_key[..32].copy_from_slice(&default[..32]);
    let encrypted = crypto::encrypt(&legacy_key, b"imap-secret-pass").unwrap();
    let credential_json = serde_json::to_string(&encrypted).unwrap();
    sqlx::query(
        r"
            INSERT INTO mail_account (
                id, user_id, display_name, email_address, protocol, auth_type,
                credential, imap_host, imap_port, imap_security,
                is_active, sync_enabled
            ) VALUES ('acct-1', 'user-legacy', 'Test', 'test@example.com',
                      'imap', 'password', ?, 'imap.example.com', 993, 'tls', 1, 1)
            ",
    )
    .bind(&credential_json)
    .execute(sqlite_pool(&db))
    .await
    .unwrap();

    let dek = AuthState::get_user_dek(&db, "user-legacy").await.unwrap();
    assert_eq!(dek.len(), 32);

    // In-memory blobs loaded *before* get_user_dek are stale.
    let stale: crypto::EncryptedCredential = serde_json::from_str(&credential_json).unwrap();
    assert!(
        crypto::decrypt(&dek, &stale).is_err(),
        "decrypting a pre-rotation blob with the new DEK must fail"
    );

    let (dek2, reloaded) = AuthState::get_user_dek_and_credential(&db, "user-legacy", "acct-1")
        .await
        .unwrap();
    assert_eq!(dek2, dek);

    let stored: String =
        sqlx::query_scalar("SELECT encrypted_dek FROM lyra_user WHERE id = 'user-legacy'")
            .fetch_one(sqlite_pool(&db))
            .await
            .unwrap();
    assert!(!stored.is_empty());

    let blob: crypto::EncryptedCredential = serde_json::from_str(&reloaded).unwrap();
    assert_eq!(crypto::decrypt(&dek, &blob).unwrap(), b"imap-secret-pass");
    assert!(
        crypto::decrypt(&legacy_key, &blob).is_err(),
        "credentials must not remain under the legacy padded key"
    );
}

#[tokio::test]
async fn totp_secret_is_encrypted_at_rest() {
    let db = test_pool().await;
    let state = test_state(db.clone());

    auth_bootstrap(
        State(state.clone()),
        Json(BootstrapRequest {
            username: "alice".into(),
            password: "Str0ngPass1".into(),
            display_name: None,
            locale: None,
        }),
    )
    .await
    .map(|(_status, Json(_))| ())
    .unwrap();

    let user_id: String = sqlx::query_scalar("SELECT id FROM lyra_user")
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();

    // Enroll: returns the plaintext secret once, stores only ciphertext.
    let enroll = totp_enroll(State(state.clone()), AuthUser(user_id.clone()))
        .await
        .unwrap();
    let plaintext_secret = enroll.secret.clone();

    let stored: String = sqlx::query_scalar("SELECT totp_secret FROM lyra_user WHERE id = ?")
        .bind(&user_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
    assert_ne!(stored, plaintext_secret);
    assert!(!stored.contains(&plaintext_secret));
    serde_json::from_str::<crypto::EncryptedCredential>(&stored).unwrap();
    let enabled: bool = sqlx::query_scalar("SELECT totp_enabled FROM lyra_user WHERE id = ?")
        .bind(&user_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
    assert!(!enabled, "enroll alone must not enable 2FA");

    // Confirm with a valid code flips totp_enabled on.
    let totp = build_totp(&plaintext_secret, "alice").unwrap();
    let code = totp.generate_current().unwrap();
    let status = totp_enroll_confirm(
        State(state.clone()),
        AuthUser(user_id.clone()),
        Json(TotpEnrollConfirmRequest { code }),
    )
    .await
    .unwrap();
    assert!(status.totp_enabled);

    // The stored value still decrypts to the original secret.
    let dek = AuthState::get_user_dek(&db, &user_id).await.unwrap();
    let stored_after: String = sqlx::query_scalar("SELECT totp_secret FROM lyra_user WHERE id = ?")
        .bind(&user_id)
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
    assert_eq!(
        decrypt_totp_secret(&dek, &stored_after).unwrap(),
        plaintext_secret
    );
}

#[tokio::test]
async fn legacy_plaintext_totp_secret_fails_loudly() {
    let db = test_pool().await;
    seed_user_with_dek(&db, "user-1").await;
    // Simulate a pre-fix row: plaintext base32 secret in the column.
    sqlx::query("UPDATE lyra_user SET totp_secret = 'JBSWY3DPEHPK3PXP' WHERE id = 'user-1'")
        .execute(sqlite_pool(&db))
        .await
        .unwrap();

    let dek = AuthState::get_user_dek(&db, "user-1").await.unwrap();
    let err = decrypt_totp_secret(&dek, "JBSWY3DPEHPK3PXP").unwrap_err();
    assert!(matches!(err, CryptoError::Decrypt(_)));
    assert!(err.to_string().contains("re-enroll"));
}

// ── Session TTLs, rate limiting, password change, 2FA hardening ──

/// Seed a user with a known username/password and TOTP already enabled.
/// Returns the plaintext base32 TOTP secret.
async fn seed_user_with_totp(db: &DbPool, id: &str, username: &str, password: &str) -> String {
    seed_user_with_dek(db, id).await;
    let hash = hash_password(password).await.unwrap();
    let secret_bytes = totp_rs::Secret::generate_secret().to_bytes().unwrap();
    let secret_b32 = base32::encode(base32::Alphabet::Rfc4648 { padding: false }, &secret_bytes);
    let dek = AuthState::get_user_dek(db, id).await.unwrap();
    let stored = encrypt_totp_secret(&dek, &secret_b32).unwrap();
    sqlx::query(
            "UPDATE lyra_user SET username = ?, password_hash = ?, totp_secret = ?, totp_enabled = 1 WHERE id = ?",
        )
        .bind(username)
        .bind(hash)
        .bind(stored)
        .bind(id)
        .execute(sqlite_pool(db))
        .await
        .unwrap();
    secret_b32
}

/// Bootstrap user "alice" / "Str0ngPass1" and return a session token.
async fn bootstrap_alice(state: &AuthState) -> String {
    let (_status, Json(_login)) = auth_bootstrap(
        State(state.clone()),
        Json(BootstrapRequest {
            username: "alice".into(),
            password: "Str0ngPass1".into(),
            display_name: None,
            locale: None,
        }),
    )
    .await
    .unwrap();
    let user_id: String = sqlx::query_scalar("SELECT id FROM lyra_user WHERE username = 'alice'")
        .fetch_one(sqlite_pool(state.db()))
        .await
        .unwrap();
    state.sessions.create_session(&user_id).await.unwrap()
}

#[test]
fn password_max_length() {
    // 1024 chars passes, 1025 is rejected (Argon2 DoS cap).
    let ok = format!("Aa1{}", "a".repeat(1021));
    assert_eq!(ok.len(), 1024);
    assert!(validate_password(&ok, 8).is_ok());
    let too_long = format!("Aa1{}", "a".repeat(1022));
    assert_eq!(too_long.len(), 1025);
    assert!(validate_password(&too_long, 8).is_err());
}

#[tokio::test]
async fn session_entries_carry_a_ttl() {
    let db = test_pool().await;
    seed_user(&db, "user-1").await;
    let kv = MemoryKv::new();
    let store = SessionStore::new(db.clone(), Arc::new(kv.clone()));
    let token = store.create_session("user-1").await.unwrap();
    let epoch = fetch_sess_epoch(&db, "user-1").await.unwrap();

    let sess_ttl = kv.ttl_remaining(&sess_key(epoch, &token)).await;
    let tok_ttl = kv.ttl_remaining(&tok_key(&token)).await;
    assert!(sess_ttl.is_some(), "session entry must have a TTL");
    assert!(tok_ttl.is_some(), "token index must have a TTL");
    // 7-day TTL, minus slack for test execution time.
    let floor = std::time::Duration::from_secs(SESSION_TTL_SECS - 60);
    assert!(sess_ttl.unwrap() > floor);
    assert!(tok_ttl.unwrap() > floor);
}

#[tokio::test]
async fn pending_session_has_short_ttl() {
    let db = test_pool().await;
    seed_user(&db, "user-1").await;
    let kv = MemoryKv::new();
    let store = SessionStore::new(db, Arc::new(kv.clone()));
    let token = store.create_pending_session("user-1").await.unwrap();
    let ttl = kv.ttl_remaining(&pending_key(&token)).await;
    assert!(ttl.is_some(), "pending token must have a TTL");
    assert!(ttl.unwrap() <= std::time::Duration::from_secs(PENDING_TTL_SECS));
}

#[tokio::test]
async fn rate_limit_counter_window_expires() {
    let kv = MemoryKv::new();
    for _ in 0..RATE_LIMIT_MAX_ATTEMPTS {
        record_failed_attempt(&kv, "rl:test", 1).await.unwrap();
    }
    assert!(
        is_rate_limited(&kv, "rl:test", RATE_LIMIT_MAX_ATTEMPTS)
            .await
            .unwrap()
    );
    // Window of 1s expires; the counter resets and attempts are allowed again.
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    assert!(
        !is_rate_limited(&kv, "rl:test", RATE_LIMIT_MAX_ATTEMPTS)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn login_rate_limited_after_five_failures() {
    let db = test_pool().await;
    let state = test_state(db);
    bootstrap_alice(&state).await;

    let bad_login = |password: &str| LoginRequest {
        username: "alice".into(),
        password: password.into(),
    };
    for _ in 0..RATE_LIMIT_MAX_ATTEMPTS {
        let err = auth_login(State(state.clone()), Json(bad_login("Wr0ngPass1")))
            .await
            .unwrap_err();
        assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
    }
    // Sixth attempt is rejected before any password check.
    let err = auth_login(State(state.clone()), Json(bad_login("Wr0ngPass1")))
        .await
        .unwrap_err();
    assert_eq!(err.status(), StatusCode::TOO_MANY_REQUESTS);
    // Even the correct password is locked out inside the window.
    let err = auth_login(State(state.clone()), Json(bad_login("Str0ngPass1")))
        .await
        .unwrap_err();
    assert_eq!(err.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn successful_login_resets_rate_limit() {
    let db = test_pool().await;
    let state = test_state(db);
    bootstrap_alice(&state).await;

    let attempt = |password: &str| {
        let state = state.clone();
        let password = password.to_string();
        async move {
            auth_login(
                State(state),
                Json(LoginRequest {
                    username: "alice".into(),
                    password,
                }),
            )
            .await
        }
    };
    // Four failures stay under the cap; a success clears the counter.
    for _ in 0..4 {
        assert!(attempt("Wr0ngPass1").await.is_err());
    }
    assert!(attempt("Str0ngPass1").await.is_ok());
    // Four more failures must not trip the limit either.
    for _ in 0..4 {
        let err = attempt("Wr0ngPass1").await.unwrap_err();
        assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
    }
}

#[tokio::test]
async fn totp_verify_rate_limited_after_five_failures() {
    let db = test_pool().await;
    seed_user_with_totp(&db, "user-1", "alice", "Str0ngPass1").await;
    let state = test_state(db);

    let login = auth_login(
        State(state.clone()),
        Json(LoginRequest {
            username: "alice".into(),
            password: "Str0ngPass1".into(),
        }),
    )
    .await
    .unwrap();
    assert!(login.requires_totp);
    let pending = login.token.clone();

    for _ in 0..RATE_LIMIT_MAX_ATTEMPTS {
        let err = totp_verify(
            State(state.clone()),
            Json(TotpVerifyRequest {
                code: "000000".into(),
                pending_token: pending.clone(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
    }
    let err = totp_verify(
        State(state.clone()),
        Json(TotpVerifyRequest {
            code: "000000".into(),
            pending_token: pending,
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(err.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn change_password_kicks_all_sessions() {
    let db = test_pool().await;
    let state = test_state(db);
    let token_a = bootstrap_alice(&state).await;

    let login_b = auth_login(
        State(state.clone()),
        Json(LoginRequest {
            username: "alice".into(),
            password: "Str0ngPass1".into(),
        }),
    )
    .await
    .unwrap();
    let token_b = login_b.token.clone();

    let alice = AuthUser(state.sessions.get_session(&token_a).await.unwrap());
    let status = change_password(
        State(state.clone()),
        alice,
        Json(ChangePasswordRequest {
            current_password: "Str0ngPass1".into(),
            new_password: "N3wPassword!x".into(),
        }),
    )
    .await
    .unwrap();
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Every prior session (including the caller's) is dead.
    assert!(state.sessions.get_session(&token_a).await.is_none());
    assert!(state.sessions.get_session(&token_b).await.is_none());

    // Old password fails, new password logs in.
    let old = auth_login(
        State(state.clone()),
        Json(LoginRequest {
            username: "alice".into(),
            password: "Str0ngPass1".into(),
        }),
    )
    .await;
    assert!(old.is_err());
    let new = auth_login(
        State(state.clone()),
        Json(LoginRequest {
            username: "alice".into(),
            password: "N3wPassword!x".into(),
        }),
    )
    .await;
    assert!(new.is_ok());
}

#[tokio::test]
async fn change_password_wrong_current_password_rejected() {
    let db = test_pool().await;
    let state = test_state(db);
    let token = bootstrap_alice(&state).await;

    let alice = AuthUser(state.sessions.get_session(&token).await.unwrap());
    let err = change_password(
        State(state.clone()),
        alice,
        Json(ChangePasswordRequest {
            current_password: "Wr0ngPass1".into(),
            new_password: "N3wPassword!x".into(),
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
    // The session is untouched and the old password still works.
    assert!(state.sessions.get_session(&token).await.is_some());
    assert!(
        auth_login(
            State(state.clone()),
            Json(LoginRequest {
                username: "alice".into(),
                password: "Str0ngPass1".into(),
            }),
        )
        .await
        .is_ok()
    );
}

#[tokio::test]
async fn change_password_weak_new_password_rejected() {
    let db = test_pool().await;
    let state = test_state(db);
    let token = bootstrap_alice(&state).await;

    let alice = AuthUser(state.sessions.get_session(&token).await.unwrap());
    let err = change_password(
        State(state.clone()),
        alice,
        Json(ChangePasswordRequest {
            current_password: "Str0ngPass1".into(),
            new_password: "weak".into(),
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(err.status(), StatusCode::BAD_REQUEST);
    assert!(state.sessions.get_session(&token).await.is_some());
}

#[tokio::test]
async fn totp_disable_requires_current_password() {
    let db = test_pool().await;
    seed_user_with_totp(&db, "user-1", "alice", "Str0ngPass1").await;
    let state = test_state(db);
    let token = state.sessions.create_session("user-1").await.unwrap();

    let alice = AuthUser(state.sessions.get_session(&token).await.unwrap());
    // Wrong password → rejected, TOTP stays enabled.
    let err = totp_disable(
        State(state.clone()),
        AuthUser(alice.0.clone()),
        Json(TotpDisableRequest {
            password: "Wr0ngPass1".into(),
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
    let enabled: bool =
        sqlx::query_scalar("SELECT totp_enabled FROM lyra_user WHERE id = 'user-1'")
            .fetch_one(sqlite_pool(state.db()))
            .await
            .unwrap();
    assert!(enabled);

    // Correct password → disabled.
    let status = totp_disable(
        State(state.clone()),
        alice,
        Json(TotpDisableRequest {
            password: "Str0ngPass1".into(),
        }),
    )
    .await
    .unwrap();
    assert!(!status.totp_enabled);
}

#[tokio::test]
async fn totp_enroll_rejected_when_already_enabled() {
    let db = test_pool().await;
    seed_user_with_totp(&db, "user-1", "alice", "Str0ngPass1").await;
    let state = test_state(db);
    let token = state.sessions.create_session("user-1").await.unwrap();

    let err = totp_enroll(
        State(state.clone()),
        AuthUser(state.sessions.get_session(&token).await.unwrap()),
    )
    .await
    .unwrap_err();
    assert_eq!(err.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn totp_code_cannot_be_replayed() {
    let db = test_pool().await;
    let secret = seed_user_with_totp(&db, "user-1", "alice", "Str0ngPass1").await;
    let state = test_state(db);
    let totp = build_totp(&secret, "alice").unwrap();
    let code = totp.generate_current().unwrap();

    let login = || LoginRequest {
        username: "alice".into(),
        password: "Str0ngPass1".into(),
    };
    let first = auth_login(State(state.clone()), Json(login()))
        .await
        .unwrap();
    let res = totp_verify(
        State(state.clone()),
        Json(TotpVerifyRequest {
            code: code.clone(),
            pending_token: first.token.clone(),
        }),
    )
    .await
    .unwrap();
    assert!(!res.requires_totp);

    // Fresh login + same code → rejected as a replay.
    let second = auth_login(State(state.clone()), Json(login()))
        .await
        .unwrap();
    let err = totp_verify(
        State(state.clone()),
        Json(TotpVerifyRequest {
            code,
            pending_token: second.token.clone(),
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn totp_rate_limit_survives_fresh_pending_token() {
    let db = test_pool().await;
    seed_user_with_totp(&db, "user-1", "alice", "Str0ngPass1").await;
    let state = test_state(db);

    let login = || LoginRequest {
        username: "alice".into(),
        password: "Str0ngPass1".into(),
    };
    // Burn the 5 attempts on pending token A.
    let first = auth_login(State(state.clone()), Json(login()))
        .await
        .unwrap();
    for _ in 0..RATE_LIMIT_MAX_ATTEMPTS {
        let err = totp_verify(
            State(state.clone()),
            Json(TotpVerifyRequest {
                code: "000000".into(),
                pending_token: first.token.clone(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
    }

    // Regression: a fresh login mints pending token B, but the limiter is
    // keyed per user, so the bypass attempt is still blocked.
    let second = auth_login(State(state.clone()), Json(login()))
        .await
        .unwrap();
    assert_ne!(first.token, second.token);
    let err = totp_verify(
        State(state.clone()),
        Json(TotpVerifyRequest {
            code: "000000".into(),
            pending_token: second.token.clone(),
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(err.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn change_password_rate_limited_after_five_wrong_current() {
    let db = test_pool().await;
    let state = test_state(db);
    let token = bootstrap_alice(&state).await;

    let user_id = state.sessions.get_session(&token).await.unwrap();
    let attempt = |current: &str| {
        change_password(
            State(state.clone()),
            AuthUser(user_id.clone()),
            Json(ChangePasswordRequest {
                current_password: current.into(),
                new_password: "N3wPassword!x".into(),
            }),
        )
    };
    for _ in 0..RATE_LIMIT_MAX_ATTEMPTS {
        let err = attempt("Wr0ngPass1").await.unwrap_err();
        assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
    }
    // Even the correct current password is locked out inside the window.
    let err = attempt("Str0ngPass1").await.unwrap_err();
    assert_eq!(err.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn totp_disable_rate_limited_after_five_wrong_passwords() {
    let db = test_pool().await;
    seed_user_with_totp(&db, "user-1", "alice", "Str0ngPass1").await;
    let state = test_state(db);
    let token = state.sessions.create_session("user-1").await.unwrap();

    let user_id = state.sessions.get_session(&token).await.unwrap();
    let attempt = |password: &str| {
        totp_disable(
            State(state.clone()),
            AuthUser(user_id.clone()),
            Json(TotpDisableRequest {
                password: password.into(),
            }),
        )
    };
    for _ in 0..RATE_LIMIT_MAX_ATTEMPTS {
        let err = attempt("Wr0ngPass1").await.unwrap_err();
        assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
    }
    let err = attempt("Str0ngPass1").await.unwrap_err();
    assert_eq!(err.status(), StatusCode::TOO_MANY_REQUESTS);

    // TOTP must still be enabled after the blocked attempts.
    let enabled: bool =
        sqlx::query_scalar("SELECT totp_enabled FROM lyra_user WHERE id = 'user-1'")
            .fetch_one(sqlite_pool(state.db()))
            .await
            .unwrap();
    assert!(enabled);
}

#[tokio::test]
async fn change_password_clears_login_rate_limit() {
    let db = test_pool().await;
    let state = test_state(db);
    let token = bootstrap_alice(&state).await;

    // An attacker locks out the username at the login endpoint.
    for _ in 0..RATE_LIMIT_MAX_ATTEMPTS {
        let err = auth_login(
            State(state.clone()),
            Json(LoginRequest {
                username: "alice".into(),
                password: "Wr0ngPass1".into(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
    }

    // The legit user (valid session) changes their password...
    change_password(
        State(state.clone()),
        AuthUser(state.sessions.get_session(&token).await.unwrap()),
        Json(ChangePasswordRequest {
            current_password: "Str0ngPass1".into(),
            new_password: "N3wPassword!x".into(),
        }),
    )
    .await
    .unwrap();

    // ...and can immediately log in with it: the attacker's login lockout
    // was cleared by the password change.
    let res = auth_login(
        State(state.clone()),
        Json(LoginRequest {
            username: "alice".into(),
            password: "N3wPassword!x".into(),
        }),
    )
    .await;
    assert!(res.is_ok());
}

#[tokio::test]
async fn auth_user_extractor_resolves_bearer_token() {
    let db = test_pool().await;
    seed_user(&db, "user-1").await;
    let state = test_state(db);
    let token = state.sessions.create_session("user-1").await.unwrap();

    let req = axum::http::Request::builder()
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(())
        .unwrap();
    let (mut parts, ()) = req.into_parts();
    let user = AuthUser::from_request_parts(&mut parts, &state)
        .await
        .expect("valid token must resolve");
    assert_eq!(user.0, "user-1");
}

#[tokio::test]
async fn auth_user_extractor_rejects_missing_or_invalid_token() {
    let db = test_pool().await;
    let state = test_state(db);

    // No Authorization header.
    let req = axum::http::Request::builder().body(()).unwrap();
    let (mut parts, ()) = req.into_parts();
    let err = AuthUser::from_request_parts(&mut parts, &state)
        .await
        .err()
        .expect("missing header must fail");
    assert_eq!(err.status(), StatusCode::UNAUTHORIZED);

    // Unknown token.
    let req = axum::http::Request::builder()
        .header(header::AUTHORIZATION, "Bearer deadbeef")
        .body(())
        .unwrap();
    let (mut parts, ()) = req.into_parts();
    let err = AuthUser::from_request_parts(&mut parts, &state)
        .await
        .err()
        .expect("invalid token must fail");
    assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn patch_preferences_updates_locale() {
    let db = test_pool().await;
    let state = test_state(db.clone());
    seed_user(&db, "user-1").await;

    let Json(info) = patch_preferences(
        State(state.clone()),
        AuthUser("user-1".into()),
        Json(PreferencesRequest {
            mark_read_policy: None,
            locale: Some("zh".into()),
        }),
    )
    .await
    .expect("locale update must succeed");
    assert_eq!(info.locale, "zh");

    let user = find_user_by_id(&db, "user-1").await.unwrap().unwrap();
    assert_eq!(user.locale, "zh");
}

#[tokio::test]
async fn patch_preferences_rejects_unsupported_locale() {
    let db = test_pool().await;
    seed_user(&db, "user-1").await;
    let state = test_state(db);

    let err = patch_preferences(
        State(state),
        AuthUser("user-1".into()),
        Json(PreferencesRequest {
            mark_read_policy: None,
            locale: Some("fr".into()),
        }),
    )
    .await
    .expect_err("unsupported locale must fail");
    assert_eq!(err.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn patch_preferences_rejects_empty_request() {
    let db = test_pool().await;
    seed_user(&db, "user-1").await;
    let state = test_state(db);

    let err = patch_preferences(
        State(state),
        AuthUser("user-1".into()),
        Json(PreferencesRequest {
            mark_read_policy: None,
            locale: None,
        }),
    )
    .await
    .expect_err("empty preferences must fail");
    assert_eq!(err.status(), StatusCode::BAD_REQUEST);
}
