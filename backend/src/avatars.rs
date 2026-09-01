//! Sender avatar resolution: contact photo → BIMI (VMC-validated) →
//! opt-in Gravatar. One endpoint hides the chain; every upstream fetch goes
//! through the media pipeline (SSRF guard, caps, sniffing), so no third
//! party sees the user's IP and Gravatar sees nothing unless opted in.
//!
//! `GET /api/v1/avatars/{email}` is bearer-gated (frontend uses `apiBlob`);
//! it is intentionally not `<img>`-safe.

use std::path::Path;
use std::time::{Duration, SystemTime};

use axum::{
    Router,
    extract::{Path as AxumPath, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use md5::Digest as _;
use sea_orm::sea_query::Query as Sq;
use sea_orm::{ColumnTrait, ConnectionTrait, Value};

use crate::auth::{AuthState, AuthUser};
use crate::db_row::{IdParam, id_param};
use crate::entities::{contact, mail_account};
use crate::media::{self, FetchedImage};
use crate::privacy;
use crate::sync::SyncError;

/// Positive-cache freshness: refetch upstream avatars older than 7 days.
/// Freshness uses the cache file's mtime — `media::write_cache` always
/// recreates the file, so mtime == fetch time; the `.meta` sidecar keeps
/// only `contentType` and stays untouched.
const AVATAR_CACHE_FRESH: Duration = Duration::from_hours(24 * 7);
/// Negative-cache TTL when every source cleanly had nothing.
const MISS_TTL_CLEAN_SECS: u64 = 86_400;
/// Negative-cache TTL when an upstream fetch/DNS error occurred.
const MISS_TTL_ERROR_SECS: u64 = 600;
/// Browser cache for a resolved avatar (all 200 responses, incl. contacts).
const AVATAR_CACHE_CONTROL: &str = "private, max-age=86400";

/// md5 hex of the trimmed, lowercased address (Gravatar's contract).
pub(crate) fn gravatar_url(email: &str) -> String {
    let digest = md5::Md5::digest(email.trim().to_ascii_lowercase().as_bytes());
    format!("https://www.gravatar.com/avatar/{digest:x}?d=404&s=128")
}

/// Parsed `default._bimi` TXT record payload.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct BimiRecord {
    logo_url: String,
    authority_url: Option<String>,
}

/// Parse a `default._bimi` TXT payload: `v=BIMI1; l=<logo>; a=<authority>`.
pub(crate) fn parse_bimi_record(txt: &[u8]) -> Option<BimiRecord> {
    let txt = std::str::from_utf8(txt).ok()?;
    let mut version_ok = false;
    let mut logo_url = None;
    let mut authority_url = None;
    for part in txt.split(';') {
        let part = part.trim();
        if let Some(v) = part.strip_prefix("v=") {
            version_ok = v.trim() == "BIMI1";
        } else if let Some(l) = part.strip_prefix("l=") {
            let l = l.trim();
            if !l.is_empty() {
                logo_url = Some(l.to_string());
            }
        } else if let Some(a) = part.strip_prefix("a=") {
            let a = a.trim();
            if !a.is_empty() {
                authority_url = Some(a.to_string());
            }
        }
    }
    if !version_ok {
        return None;
    }
    Some(BimiRecord {
        logo_url: logo_url?,
        authority_url,
    })
}

/// BIMI requires DMARC enforcement on the From domain (client-side gate:
/// policy record only — no alignment evaluation).
pub(crate) fn dmarc_allows_bimi(txt: &str) -> bool {
    let mut parts = txt.split(';');
    if !parts.next().is_some_and(|v| v.trim().eq_ignore_ascii_case("v=DMARC1")) {
        return false;
    }
    parts
        .map(str::trim)
        .find_map(|part| part.strip_prefix("p="))
        .is_some_and(|p| p == "quarantine" || p == "reject")
}

/// Cap on the VMC evidence document (PEM bundle) fetched from `a=`.
const MAX_VMC_BUNDLE_BYTES: u64 = 1024 * 1024;

/// BIMI logo for a From domain, or None. DMARC gate → record parse →
/// VMC validation → logo fetch. Every failure is a silent miss.
async fn resolve_bimi_logo(state: &AuthState, domain: &str) -> Option<FetchedImage> {
    let _ = state; // DNS goes through the process-wide DKIM authenticator.
    let auth = crate::dkim::authenticator().ok()?;
    let dmarc_txt = auth.txt_raw_lookup(format!("_dmarc.{domain}")).await.ok()?;
    if !dmarc_allows_bimi(&String::from_utf8_lossy(&dmarc_txt)) {
        return None;
    }
    let bimi_txt = auth
        .txt_raw_lookup(format!("default._bimi.{domain}"))
        .await
        .ok()?;
    let record = parse_bimi_record(&bimi_txt)?;
    let authority = record.authority_url?;
    let pem = fetch_text(&authority).await?;
    if let Err(e) = crate::bimi::validate_vmc(pem.as_bytes(), domain).await {
        tracing::debug!(domain, error = %e, "bimi vmc validation failed");
        return None;
    }
    fetch_logo(&record.logo_url).await
}

/// VMC evidence fetch: SSRF-guarded, 1 MiB cap, no content-type
/// requirement. PEM is ASCII; non-UTF-8 bytes are a miss.
async fn fetch_text(url: &str) -> Option<String> {
    let bytes = media::fetch_bytes(url, MAX_VMC_BUNDLE_BYTES).await.ok()?;
    String::from_utf8(bytes).ok()
}

/// Logo fetch through the media pipeline, accepting raster or SVG.
async fn fetch_logo(url: &str) -> Option<FetchedImage> {
    match media::fetch_bimi_logo(url).await {
        Ok(img) => Some(img),
        Err(e) => {
            tracing::debug!(error = %e, "bimi logo fetch failed");
            None
        }
    }
}

/// Content type from image magic bytes (contact photos carry no header).
fn sniff_image_content_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() < 4 {
        return None;
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.starts_with(&[0x89, 0x50, 0x4e, 0x47]) {
        Some("image/png")
    } else if bytes.starts_with(b"GIF8") {
        Some("image/gif")
    } else if bytes.starts_with(b"RIFF") && bytes.len() >= 12 && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// 200 response for a resolved avatar. Deliberately not
/// `media::image_response`: avatar URLs are address-keyed, not
/// content-addressed, so `immutable, max-age=31536000` would wedge a stale
/// image for a year; 24h matches the positive-cache miss/revalidate cadence.
fn avatar_response(bytes: Vec<u8>, content_type: &str) -> Response {
    let mut headers = HeaderMap::new();
    if let Ok(ct) = HeaderValue::from_str(content_type) {
        headers.insert(header::CONTENT_TYPE, ct);
    }
    // BIMI logos are SVG; served as a top-level document an SVG can run
    // scripts in our origin. Forbid that — raster types need nothing.
    if content_type.eq_ignore_ascii_case("image/svg+xml") {
        headers.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("script-src 'none'"),
        );
    }
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(AVATAR_CACHE_CONTROL),
    );
    (StatusCode::OK, headers, bytes).into_response()
}

fn not_found_response() -> Response {
    StatusCode::NOT_FOUND.into_response()
}

/// Self-describing negative-cache key: the `{g}` segment is the Gravatar
/// opt-in state, so toggling the setting bypasses old misses (kv has no
/// enumeration to invalidate them).
fn miss_key(user_id: &str, gravatar_enabled: bool, email: &str) -> String {
    let g = u8::from(gravatar_enabled);
    let hash = crate::blobs::sha256_hex(email.as_bytes());
    format!("user:{user_id}:avatar-miss:{g}:{hash}")
}

/// Serve a cached avatar younger than [`AVATAR_CACHE_FRESH`] (mtime-based;
/// see the constant's doc).
async fn read_fresh_cache(path: &Path) -> Option<(Vec<u8>, String)> {
    let meta = tokio::fs::metadata(path).await.ok()?;
    let modified = meta.modified().ok()?;
    let age = SystemTime::now().duration_since(modified).ok()?;
    if age > AVATAR_CACHE_FRESH {
        return None;
    }
    media::read_cache(path).await
}

/// Contact photo for `email`, streamed from the blob store (never copied
/// into media-cache). Blob read failures fall through to the rest of the
/// chain instead of erroring the request.
async fn contact_photo_response(
    state: &AuthState,
    user_id: &str,
    email: &str,
) -> Result<Option<Response>, SyncError> {
    let db = state.db();
    let user = match id_param(db, user_id)? {
        IdParam::Text(s) => Value::String(Some(s)),
        IdParam::Uuid(u) => Value::Uuid(Some(u)),
    };
    let mut accounts = Sq::select();
    accounts
        .column(mail_account::Column::Id)
        .from(mail_account::Entity)
        .and_where(mail_account::Column::UserId.eq(user));

    // Contacts per user are few: load (email_addresses, photo_path) and
    // match the JSON array in Rust — dialect-safe across SQLite/Postgres.
    let mut stmt = Sq::select();
    stmt.column(contact::Column::EmailAddresses)
        .column(contact::Column::PhotoPath)
        .from(contact::Entity)
        .and_where(contact::Column::AccountId.in_subquery(accounts))
        .and_where(contact::Column::PhotoPath.is_not_null());
    let rows = db
        .orm()
        .query_all(&stmt)
        .await
        .map_err(|e| SyncError::Internal(format!("contact lookup: {e}")))?;

    for row in &rows {
        let addresses: Vec<String> = row
            .try_get::<Option<serde_json::Value>>("", "email_addresses")
            .ok()
            .flatten()
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();
        if !addresses
            .iter()
            .any(|a| a.trim().eq_ignore_ascii_case(email))
        {
            continue;
        }
        let photo_path: Option<String> = row
            .try_get("", "photo_path")
            .map_err(|e| SyncError::Internal(format!("contact row: {e}")))?;
        let Some(photo_path) = photo_path else {
            continue;
        };
        let Ok(bytes) = crate::blobs::read(&state.data_dir, &photo_path).await else {
            tracing::debug!(
                photo_path,
                "avatar contact photo unreadable; falling through"
            );
            continue;
        };
        let Some(content_type) = sniff_image_content_type(&bytes) else {
            continue;
        };
        if !media::looks_like_image(content_type, &bytes) {
            continue;
        }
        return Ok(Some(avatar_response(bytes, content_type)));
    }
    Ok(None)
}

/// `GET /api/v1/avatars/{email}` — resolve a sender avatar.
async fn get_avatar(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    AxumPath(email): AxumPath<String>,
) -> Result<Response, SyncError> {
    let email = email.trim().to_lowercase();
    if !email.contains('@') {
        return Ok(not_found_response());
    }

    // 1. Contact photo (blob store, always preferred).
    if let Some(resp) = contact_photo_response(&state, &user_id, &email).await? {
        return Ok(resp);
    }

    // 2. Fresh positive media-cache hit.
    let cache_root = state.data_dir.join("media-cache");
    let avatar_key = format!("avatar:{email}");
    let cache_path = media::cache_file_path(&cache_root, &avatar_key);
    if let Some((bytes, content_type)) = read_fresh_cache(&cache_path).await {
        return Ok(avatar_response(bytes, &content_type));
    }

    // 3. Negative cache.
    let settings = privacy::load_settings(state.kv(), &user_id).await?;
    let miss_key = miss_key(&user_id, settings.gravatar_avatars, &email);
    let known_miss = state
        .kv()
        .get(&miss_key)
        .await
        .map_err(|e| SyncError::Internal(e.to_string()))?
        .is_some();
    if known_miss {
        return Ok(not_found_response());
    }

    // 4. BIMI (DMARC gate + VMC validation), 5. opt-in Gravatar.
    let domain = email.rsplit('@').next().unwrap_or_default();
    let mut fetched = resolve_bimi_logo(&state, domain).await;
    let mut upstream_error = false;
    if fetched.is_none() && settings.gravatar_avatars {
        match media::fetch_upstream(&gravatar_url(&email)).await {
            Ok(img) => fetched = Some(img),
            // Any failure — Gravatar's own 404 included — is a miss.
            Err(e) => {
                tracing::debug!(error = %e, "gravatar avatar fetch failed");
                upstream_error = true;
            }
        }
    }

    if let Some(img) = fetched {
        media::write_cache(&cache_root, &avatar_key, &img.bytes, &img.content_type).await?;
        return Ok(avatar_response(img.bytes, &img.content_type));
    }

    let ttl = if upstream_error {
        MISS_TTL_ERROR_SECS
    } else {
        MISS_TTL_CLEAN_SECS
    };
    state
        .kv()
        .set(&miss_key, "1", Some(ttl))
        .await
        .map_err(|e| SyncError::Internal(e.to_string()))?;
    Ok(not_found_response())
}

/// Routes for the avatar resolver.
pub fn routes() -> Router<AuthState> {
    Router::new().route("/api/v1/avatars/{email}", get(get_avatar))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{AuthState, TEST_MASTER_KEY, install_test_master_key};
    use crate::kernel::App;
    use crate::kv::MemoryKv;
    use crate::storage::{DbPool, Storage};
    use axum::body::to_bytes;
    use axum::http::header;
    use std::path::PathBuf;
    use std::sync::Arc;

    #[test]
    fn gravatar_url_hashes_lowercased_trimmed_email() {
        assert_eq!(
            gravatar_url("  HuangCheng@Example.COM "),
            "https://www.gravatar.com/avatar/64774d1724f12eae92bd80a2feb660b1?d=404&s=128"
        );
    }

    #[test]
    fn bimi_record_parses_logo_and_authority() {
        let rec = parse_bimi_record(
            b"v=BIMI1; l=https://example.com/logo.svg; a=https://example.com/vmc.pem",
        );
        assert_eq!(
            rec,
            Some(BimiRecord {
                logo_url: "https://example.com/logo.svg".into(),
                authority_url: Some("https://example.com/vmc.pem".into()),
            })
        );
    }

    #[test]
    fn bimi_record_rejects_wrong_version_and_missing_logo() {
        assert_eq!(parse_bimi_record(b"v=DMARC1; p=reject;"), None);
        assert_eq!(
            parse_bimi_record(b"v=BIMI1; a=https://x.test/vmc.pem"),
            None
        );
    }

    #[test]
    fn dmarc_policy_gate() {
        assert!(dmarc_allows_bimi("v=DMARC1; p=reject;"));
        assert!(dmarc_allows_bimi("v=DMARC1; p=quarantine;"));
        assert!(!dmarc_allows_bimi("v=DMARC1; p=none;"));
        assert!(!dmarc_allows_bimi("v=DMARC1; p=rejectfoo;"));
        assert!(!dmarc_allows_bimi("garbage"));
    }

    // ── Endpoint tests (media.rs harness style) ──────────────────────

    async fn test_pool() -> sqlx::SqlitePool {
        let storage = Storage::new("sqlite::memory:").await.unwrap();
        storage.run_migrations().await.unwrap();
        match storage.pool().clone() {
            DbPool::Sqlite(pool) => pool,
            #[cfg(feature = "postgres")]
            DbPool::Postgres(_) => panic!("expected sqlite"),
        }
    }

    fn test_auth_state(pool: sqlx::SqlitePool, data_dir: &Path) -> AuthState {
        install_test_master_key();
        let config = crate::config::Config {
            listen_addr: "127.0.0.1:0".into(),
            database_url: "sqlite::memory:".into(),
            data_dir: data_dir.to_string_lossy().into_owned(),
            min_password_length: 8,
            sync_max_concurrent: 3,
            sync_poll_secs: 300,
            max_attachment_bytes: 25 * 1024 * 1024,
            redis_url: None,
            master_key: TEST_MASTER_KEY.to_vec(),
            ms_oauth: None,
            yandex_oauth: None,
        };
        AuthState::new(
            DbPool::Sqlite(pool),
            &config,
            Arc::new(App::new()),
            Arc::new(MemoryKv::new()),
        )
        .unwrap()
    }

    fn temp_data_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lyra-avatars-test-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A minimal but magic-correct PNG (header + IHDR chunk bytes).
    const MOCK_PNG: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x10,
    ];

    fn sqlite_pool(state: &AuthState) -> &sqlx::SqlitePool {
        match state.db() {
            DbPool::Sqlite(pool) => pool,
            #[cfg(feature = "postgres")]
            DbPool::Postgres(_) => panic!("expected sqlite"),
        }
    }

    async fn seed_user_account(state: &AuthState, user_id: &str) -> String {
        let pool = sqlite_pool(state);
        sqlx::query("INSERT INTO lyra_user (id, username, password_hash) VALUES (?, ?, ?)")
            .bind(user_id)
            .bind("avatast")
            .bind("hash")
            .execute(pool)
            .await
            .unwrap();
        let account_id = uuid::Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO mail_account (\
                id, user_id, display_name, email_address, protocol, auth_type,\
                credential, is_active, sync_enabled\
             ) VALUES (?, ?, ?, ?, 'imap', 'password', 'unused', 1, 1)",
        )
        .bind(&account_id)
        .bind(user_id)
        .bind("Test Account")
        .bind("alice@example.com")
        .execute(pool)
        .await
        .unwrap();
        account_id
    }

    async fn seed_contact(
        state: &AuthState,
        account_id: &str,
        emails: &[&str],
        photo_path: Option<&str>,
    ) {
        let pool = sqlite_pool(state);
        let emails_json = serde_json::to_string(&emails).unwrap();
        sqlx::query(
            "INSERT INTO contact (id, account_id, display_name, email_addresses, photo_path) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(uuid::Uuid::now_v7().to_string())
        .bind(account_id)
        .bind("Friend")
        .bind(emails_json)
        .bind(photo_path)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn call_avatar(state: &AuthState, user_id: &str, email: &str) -> Response {
        get_avatar(
            State(state.clone()),
            AuthUser(user_id.to_string()),
            AxumPath(email.to_string()),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn contact_photo_served_from_blob_store() {
        let data_dir = temp_data_dir();
        let state = test_auth_state(test_pool().await, &data_dir);
        let account_id = seed_user_account(&state, "u1").await;
        let photo_path = crate::blobs::store(&data_dir, &account_id, MOCK_PNG)
            .await
            .unwrap();
        seed_contact(
            &state,
            &account_id,
            &["friend@example.com"],
            Some(&photo_path),
        )
        .await;

        // Mixed case + whitespace must normalize to the stored address.
        let resp = call_avatar(&state, "u1", "  Friend@Example.COM ").await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get(header::CONTENT_TYPE).unwrap(),
            "image/png"
        );
        assert_eq!(
            resp.headers().get(header::CACHE_CONTROL).unwrap(),
            AVATAR_CACHE_CONTROL
        );
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        assert_eq!(body.as_ref(), MOCK_PNG);

        // Contact photos are served from the blob store only — never copied
        // into the media-cache.
        let cache_path =
            media::cache_file_path(&data_dir.join("media-cache"), "avatar:friend@example.com");
        assert!(!cache_path.exists());

        let _ = std::fs::remove_dir_all(&data_dir);
    }

    #[tokio::test]
    async fn unknown_email_404s_and_sets_negative_marker() {
        let data_dir = temp_data_dir();
        let state = test_auth_state(test_pool().await, &data_dir);
        seed_user_account(&state, "u1").await;

        let resp = call_avatar(&state, "u1", "ghost@example.com").await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        // Clean miss (Gravatar off by default) → 24h negative marker; the
        // key embeds the opt-in state (`0`) and the hashed address.
        let key = miss_key("u1", false, "ghost@example.com");
        assert!(state.kv().get(&key).await.unwrap().is_some());

        // Second call: still 404, short-circuited by the marker.
        let resp = call_avatar(&state, "u1", "ghost@example.com").await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        let _ = std::fs::remove_dir_all(&data_dir);
    }

    #[tokio::test]
    async fn unreadable_contact_photo_falls_through_to_404() {
        let data_dir = temp_data_dir();
        let state = test_auth_state(test_pool().await, &data_dir);
        let account_id = seed_user_account(&state, "u1").await;
        seed_contact(
            &state,
            &account_id,
            &["broken@example.com"],
            Some("blobs/does/not/exist"),
        )
        .await;

        let resp = call_avatar(&state, "u1", "broken@example.com").await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert!(
            state
                .kv()
                .get(&miss_key("u1", false, "broken@example.com"))
                .await
                .unwrap()
                .is_some()
        );

        let _ = std::fs::remove_dir_all(&data_dir);
    }
}
