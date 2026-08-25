//! Remote image proxy — signed URLs, disk cache, SSRF-safe upstream fetch.
//!
//! See `docs/specs/2026-08-23-lyra-remote-image-proxy-spec.md` M2.

use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// When true, [`validate_outbound_url`] accepts loopback hosts so integration
/// tests can drive a local mock upstream. Omitted from production builds.
#[cfg(test)]
static ALLOW_LOOPBACK_FOR_TESTS: AtomicBool = AtomicBool::new(false);

use axum::{
    extract::{Path as AxumPath, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use base64::Engine;
use hmac::{Hmac, Mac};
use rand::RngCore;
use reqwest::redirect::Policy;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::auth::AuthState;
use crate::kv::KvStore;
use crate::sync::SyncError;

const MEDIA_SECRET_KV_PREFIX: &str = "user:";
const MEDIA_SECRET_KV_SUFFIX: &str = ":media_secret";
const SIG_TTL_SECS: i64 = 86_400;
const MAX_CACHE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_UPSTREAM_BYTES: u64 = 10 * 1024 * 1024;
const MAX_REDIRECTS: usize = 3;
const UPSTREAM_TIMEOUT_SECS: u64 = 10;
const PROXY_USER_AGENT: &str = "Lyra/1.0";

/// 1×1 transparent GIF served on proxy errors (no oracle text).
const PLACEHOLDER_GIF: &[u8] = &[
    0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x01, 0x00, 0x01, 0x00, 0x80, 0x00, 0x00, 0xff, 0xff, 0xff,
    0x00, 0x00, 0x00, 0x21, 0xf9, 0x04, 0x01, 0x00, 0x00, 0x00, 0x00, 0x2c, 0x00, 0x00, 0x00, 0x00,
    0x01, 0x00, 0x01, 0x00, 0x00, 0x02, 0x02, 0x44, 0x01, 0x00, 0x3b,
];

type HmacSha256 = Hmac<Sha256>;

fn media_secret_kv_key(user_id: &str) -> String {
    format!("{MEDIA_SECRET_KV_PREFIX}{user_id}{MEDIA_SECRET_KV_SUFFIX}")
}

/// Load or mint a per-user media signing secret (stored in kv).
pub async fn load_media_secret(kv: &Arc<dyn KvStore>, user_id: &str) -> Result<Vec<u8>, SyncError> {
    let key = media_secret_kv_key(user_id);
    let raw = kv
        .get(&key)
        .await
        .map_err(|e| SyncError::Internal(e.to_string()))?;
    if let Some(existing) = raw {
        return base64_decode(&existing)
            .ok_or_else(|| SyncError::Internal("corrupt media secret".into()));
    }
    let mut secret = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut secret);
    let encoded = base64_encode(&secret);
    kv.set(&key, &encoded, None)
        .await
        .map_err(|e| SyncError::Internal(e.to_string()))?;
    Ok(secret.to_vec())
}

fn base64_encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::STANDARD.decode(s).ok()
}

fn now_unix() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs(),
    )
    .unwrap_or(i64::MAX)
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

fn cache_key_for_url(url: &str) -> String {
    let hash = Sha256::digest(url.as_bytes());
    hex_encode(&hash)
}

fn cache_file_path(cache_root: &Path, url: &str) -> PathBuf {
    let hash = cache_key_for_url(url);
    let prefix = hash.get(..2).unwrap_or("00");
    cache_root.join(prefix).join(&hash)
}

fn cache_meta_path(data_path: &Path) -> PathBuf {
    data_path.with_extension("meta")
}

/// Encode original image URL for the `/api/v1/proxy/…` path segment.
pub fn encode_proxy_path(url: &str) -> String {
    url.replace('?', "%3F").replace('#', "%23")
}

/// Decode path segment back to the original URL.
pub fn decode_proxy_path(encoded: &str) -> String {
    encoded.replace("%3F", "?").replace("%23", "#")
}

pub struct ProxySigner {
    pub user_id: String,
    pub(crate) secret: Vec<u8>,
}

impl ProxySigner {
    pub async fn new(kv: &Arc<dyn KvStore>, user_id: &str) -> Result<Self, SyncError> {
        let secret = load_media_secret(kv, user_id).await?;
        Ok(Self {
            user_id: user_id.to_string(),
            secret,
        })
    }

    pub fn sign_url(&self, original_url: &str) -> String {
        let exp = now_unix() + SIG_TTL_SECS;
        let sig = compute_sig(&self.secret, &self.user_id, original_url, exp);
        let encoded = encode_proxy_path(original_url);
        format!(
            "/api/v1/proxy/{encoded}?exp={exp}&sig={sig}&uid={}",
            self.user_id
        )
    }
}

fn compute_sig(secret: &[u8], user_id: &str, url: &str, exp: i64) -> String {
    let payload = format!("{user_id}:{exp}:{url}");
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(payload.as_bytes());
    hex_encode(&mac.finalize().into_bytes())
}

fn verify_sig(secret: &[u8], user_id: &str, url: &str, exp: i64, sig: &str) -> bool {
    if exp < now_unix() {
        return false;
    }
    let expected = compute_sig(secret, user_id, url, exp);
    expected == sig
}

fn looks_like_image(content_type: &str, bytes: &[u8]) -> bool {
    let ct = content_type.to_ascii_lowercase();
    if !ct.starts_with("image/") {
        return false;
    }
    if bytes.len() < 4 {
        return false;
    }
    // GIF, PNG, JPEG, WEBP (RIFF)
    bytes.starts_with(&[0x47, 0x49, 0x46, 0x38])
        || bytes.starts_with(&[0x89, 0x50, 0x4e, 0x47])
        || bytes.starts_with(&[0xff, 0xd8, 0xff])
        || bytes.starts_with(b"RIFF")
}

/// Tiny / 1×1-class image payload (CHE-60 advisory tracking-pixel heuristic).
pub(crate) fn is_tiny_tracking_payload(bytes: &[u8]) -> bool {
    // Classic 1×1 GIF is ~43 bytes; keep a small ceiling for similar beacons.
    if bytes.len() <= 100 {
        return true;
    }
    if let Some((w, h)) = sniff_raster_dimensions(bytes) {
        return w <= 4 && h <= 4;
    }
    false
}

fn sniff_raster_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.starts_with(b"GIF8") && bytes.len() >= 10 {
        let w = u32::from(u16::from_le_bytes([bytes[6], bytes[7]]));
        let h = u32::from(u16::from_le_bytes([bytes[8], bytes[9]]));
        return Some((w, h));
    }
    if bytes.starts_with(&[0x89, 0x50, 0x4e, 0x47]) && bytes.len() >= 24 {
        // IHDR width/height are big-endian at offsets 16..24
        let w = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
        let h = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
        return Some((w, h));
    }
    None
}

async fn validate_outbound_url(url: &str) -> Result<(), SyncError> {
    let parsed = reqwest::Url::parse(url)
        .map_err(|_| SyncError::InvalidInput("invalid proxy target URL".into()))?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err(SyncError::InvalidInput(
            "proxy target must be http(s)".into(),
        ));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| SyncError::InvalidInput("proxy target missing host".into()))?;
    let port = parsed.port_or_known_default().unwrap_or(443);
    let addrs = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| SyncError::InvalidInput("proxy target host lookup failed".into()))?;
    let public = crate::netsec::filter_public_addrs(addrs.map(|a| a.ip()));
    if public.is_empty() {
        // Test-only escape hatch for a loopback mock upstream.
        #[cfg(test)]
        if ALLOW_LOOPBACK_FOR_TESTS.load(Ordering::SeqCst)
            && (host == "127.0.0.1" || host.eq_ignore_ascii_case("localhost") || host == "::1")
        {
            return Ok(());
        }
        return Err(SyncError::InvalidInput(
            "proxy target blocked by SSRF policy".into(),
        ));
    }
    Ok(())
}

struct FetchedImage {
    bytes: Vec<u8>,
    content_type: String,
}

async fn fetch_upstream(url: &str) -> Result<FetchedImage, SyncError> {
    let client = reqwest::Client::builder()
        .redirect(Policy::none())
        .timeout(Duration::from_secs(UPSTREAM_TIMEOUT_SECS))
        .user_agent(PROXY_USER_AGENT)
        .build()
        .map_err(|e| SyncError::Internal(format!("proxy client: {e}")))?;

    let mut current = url.to_string();
    for _ in 0..=MAX_REDIRECTS {
        validate_outbound_url(&current).await?;
        let resp = client
            .get(&current)
            .header(header::REFERER, "")
            .send()
            .await
            .map_err(|e| SyncError::Internal(format!("upstream fetch failed: {e}")))?;

        if resp.status().is_redirection() {
            let loc = resp
                .headers()
                .get(header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .map(std::string::ToString::to_string);
            if let Some(next) = loc {
                current = resp
                    .url()
                    .join(&next)
                    .map(|u| u.to_string())
                    .unwrap_or(next);
                continue;
            }
            break;
        }

        if !resp.status().is_success() {
            return Err(SyncError::Internal("upstream non-success status".into()));
        }

        let content_type = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/octet-stream")
            .split(';')
            .next()
            .unwrap_or("application/octet-stream")
            .trim()
            .to_string();

        let mut bytes = Vec::new();
        let body = resp
            .bytes()
            .await
            .map_err(|e| SyncError::Internal(format!("upstream read: {e}")))?;
        if body.len() > usize::try_from(MAX_UPSTREAM_BYTES).unwrap_or(usize::MAX) {
            return Err(SyncError::InvalidInput("upstream image too large".into()));
        }
        bytes.extend_from_slice(&body);

        if !looks_like_image(&content_type, &bytes) {
            return Err(SyncError::InvalidInput("upstream not an image".into()));
        }

        return Ok(FetchedImage {
            bytes,
            content_type,
        });
    }

    Err(SyncError::InvalidInput("too many redirects".into()))
}

async fn read_cache(path: &Path) -> Option<(Vec<u8>, String)> {
    let meta_path = cache_meta_path(path);
    let meta_raw = tokio::fs::read_to_string(&meta_path).await.ok()?;
    let meta: serde_json::Value = serde_json::from_str(&meta_raw).ok()?;
    let content_type = meta
        .get("contentType")
        .and_then(|v| v.as_str())
        .unwrap_or("image/gif")
        .to_string();
    let bytes = tokio::fs::read(path).await.ok()?;
    Some((bytes, content_type))
}

async fn write_cache(
    cache_root: &Path,
    url: &str,
    bytes: &[u8],
    content_type: &str,
) -> Result<(), SyncError> {
    let path = cache_file_path(cache_root, url);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| SyncError::Internal(format!("cache mkdir: {e}")))?;
    }
    let mut file = tokio::fs::File::create(&path)
        .await
        .map_err(|e| SyncError::Internal(format!("cache write: {e}")))?;
    file.write_all(bytes)
        .await
        .map_err(|e| SyncError::Internal(format!("cache write: {e}")))?;
    let meta = serde_json::json!({ "contentType": content_type });
    tokio::fs::write(cache_meta_path(&path), meta.to_string())
        .await
        .map_err(|e| SyncError::Internal(format!("cache meta: {e}")))?;
    evict_cache_if_needed(cache_root, MAX_CACHE_BYTES).await?;
    Ok(())
}

async fn evict_cache_if_needed(cache_root: &Path, max_bytes: u64) -> Result<(), SyncError> {
    let mut entries: Vec<(PathBuf, u64, SystemTime)> = Vec::new();
    let mut total = 0u64;
    let mut stack = vec![cache_root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(read_dir) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_some_and(|e| e == "meta") {
                continue;
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            let len = meta.len();
            let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            total += len;
            entries.push((path, len, modified));
        }
    }
    if total <= max_bytes {
        return Ok(());
    }
    entries.sort_by_key(|(_, _, modified)| *modified);
    for (path, len, _) in entries {
        if total <= max_bytes {
            break;
        }
        let _ = tokio::fs::remove_file(&path).await;
        let _ = tokio::fs::remove_file(cache_meta_path(&path)).await;
        total = total.saturating_sub(len);
    }
    Ok(())
}

fn placeholder_response() -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("image/gif"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    (StatusCode::NOT_FOUND, headers, PLACEHOLDER_GIF).into_response()
}

fn image_response(bytes: Vec<u8>, content_type: &str, _cached: bool) -> Response {
    let mut headers = HeaderMap::new();
    if let Ok(ct) = HeaderValue::from_str(content_type) {
        headers.insert(header::CONTENT_TYPE, ct);
    }
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=31536000, immutable"),
    );
    if is_tiny_tracking_payload(&bytes) {
        headers.insert("x-lyra-pixel", HeaderValue::from_static("1"));
    }
    (StatusCode::OK, headers, bytes).into_response()
}

#[derive(Debug, Deserialize)]
pub(crate) struct ProxyQuery {
    exp: i64,
    sig: String,
    uid: String,
}

/// `GET /api/v1/proxy/{*target}` — HMAC-gated image proxy (no bearer; `<img>` safe).
pub async fn proxy_image(
    State(state): State<AuthState>,
    AxumPath(target): AxumPath<String>,
    Query(query): Query<ProxyQuery>,
) -> Response {
    let result = proxy_image_inner(&state, &target, &query).await;
    match result {
        Ok(resp) => resp,
        Err(e) => {
            tracing::debug!(error = %e, "media proxy rejected or failed");
            placeholder_response()
        }
    }
}

async fn proxy_image_inner(
    state: &AuthState,
    target: &str,
    query: &ProxyQuery,
) -> Result<Response, SyncError> {
    let url = decode_proxy_path(target);
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(SyncError::InvalidInput("invalid proxy path".into()));
    }

    let secret_raw = state
        .kv()
        .get(&media_secret_kv_key(&query.uid))
        .await
        .map_err(|e| SyncError::Internal(e.to_string()))?;
    let secret = secret_raw
        .and_then(|s| base64_decode(&s))
        .ok_or_else(|| SyncError::InvalidInput("missing media secret".into()))?;

    if !verify_sig(&secret, &query.uid, &url, query.exp, &query.sig) {
        return Err(SyncError::InvalidInput("bad or expired signature".into()));
    }

    let cache_root = state.data_dir.join("media-cache");
    let cache_path = cache_file_path(&cache_root, &url);
    if let Some((bytes, content_type)) = read_cache(&cache_path).await {
        tracing::debug!(cache_key = %cache_key_for_url(&url), "media cache hit");
        return Ok(image_response(bytes, &content_type, true));
    }

    tracing::debug!(cache_key = %cache_key_for_url(&url), "media cache miss");
    let fetched = fetch_upstream(&url).await?;
    write_cache(&cache_root, &url, &fetched.bytes, &fetched.content_type).await?;
    Ok(image_response(fetched.bytes, &fetched.content_type, false))
}

pub fn routes() -> axum::Router<AuthState> {
    axum::Router::new().route("/api/v1/proxy/{*target}", axum::routing::get(proxy_image))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{AuthState, TEST_MASTER_KEY, install_test_master_key};
    use crate::kernel::App;
    use crate::kv::MemoryKv;
    use crate::storage::{DbPool, Storage};
    use axum::Router;
    use axum::body::to_bytes;
    use axum::extract::{Path as AxumPath, Query, State};
    use axum::http::{StatusCode, header};
    use axum::routing::get;
    use std::sync::Arc;
    use tokio::sync::{Mutex, MutexGuard};

    /// Serialize tests that touch [`ALLOW_LOOPBACK_FOR_TESTS`].
    static LOOPBACK_TEST_LOCK: Mutex<()> = Mutex::const_new(());

    #[test]
    fn tiny_tracking_payload_heuristic() {
        assert!(is_tiny_tracking_payload(PLACEHOLDER_GIF));
        assert!(is_tiny_tracking_payload(&[0u8; 50]));
        assert!(!is_tiny_tracking_payload(&[0u8; 200]));
        // Minimal GIF header with 1×1 dimensions (rest padded).
        let mut gif = vec![0u8; 120];
        gif[..6].copy_from_slice(b"GIF89a");
        gif[6] = 1;
        gif[7] = 0;
        gif[8] = 1;
        gif[9] = 0;
        assert!(is_tiny_tracking_payload(&gif));
    }

    #[test]
    fn proxy_path_roundtrip_query_and_fragment() {
        let url = "https://evil.com/pixel.gif?id=abc#frag";
        let enc = encode_proxy_path(url);
        assert!(enc.contains("%3F"));
        assert!(enc.contains("%23"));
        let dec = decode_proxy_path(&enc);
        assert_eq!(dec, url);
    }

    #[test]
    fn sig_roundtrip() {
        let secret = b"test-secret-key-32-bytes-long!!!";
        let user = "user-1";
        let url = "https://example.com/img.png";
        let exp = now_unix() + 3600;
        let sig = compute_sig(secret, user, url, exp);
        assert!(verify_sig(secret, user, url, exp, &sig));
        assert!(!verify_sig(secret, user, url, exp - 7200, &sig));
        assert!(!verify_sig(secret, user, "https://other.com/x", exp, &sig));
    }

    #[test]
    fn cache_key_stable() {
        let a = cache_key_for_url("https://a.com/x");
        let b = cache_key_for_url("https://a.com/x");
        let c = cache_key_for_url("https://a.com/y");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    struct LoopbackGuard {
        _lock: MutexGuard<'static, ()>,
    }

    impl LoopbackGuard {
        async fn enter() -> Self {
            let lock = LOOPBACK_TEST_LOCK.lock().await;
            ALLOW_LOOPBACK_FOR_TESTS.store(true, Ordering::SeqCst);
            Self { _lock: lock }
        }

        async fn hold_off() -> MutexGuard<'static, ()> {
            let lock = LOOPBACK_TEST_LOCK.lock().await;
            ALLOW_LOOPBACK_FOR_TESTS.store(false, Ordering::SeqCst);
            lock
        }
    }

    impl Drop for LoopbackGuard {
        fn drop(&mut self) {
            ALLOW_LOOPBACK_FOR_TESTS.store(false, Ordering::SeqCst);
        }
    }

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
            redis_url: None,
            master_key: TEST_MASTER_KEY.to_vec(),
            ms_oauth: None,
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
        let dir = std::env::temp_dir().join(format!("lyra-media-test-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    async fn signed_proxy_parts(
        state: &AuthState,
        user_id: &str,
        original_url: &str,
    ) -> (String, ProxyQuery) {
        let signer = ProxySigner::new(state.kv(), user_id).await.unwrap();
        let signed_url = signer.sign_url(original_url);
        let without_prefix = signed_url
            .strip_prefix("/api/v1/proxy/")
            .expect("signed URL prefix");
        let (path, query_str) = without_prefix.split_once('?').expect("signed URL query");
        let mut exp = 0i64;
        let mut sig = String::new();
        let mut uid = String::new();
        for part in query_str.split('&') {
            if let Some((k, v)) = part.split_once('=') {
                match k {
                    "exp" => exp = v.parse().unwrap(),
                    "sig" => sig = v.to_string(),
                    "uid" => uid = v.to_string(),
                    _ => {}
                }
            }
        }
        (path.to_string(), ProxyQuery { exp, sig, uid })
    }

    async fn call_proxy(state: AuthState, path: String, query: ProxyQuery) -> Response {
        proxy_image(State(state), AxumPath(path), Query(query)).await
    }

    async fn response_bytes(resp: Response) -> (StatusCode, Vec<u8>) {
        let status = resp.status();
        let limit = usize::try_from(MAX_UPSTREAM_BYTES).unwrap_or(usize::MAX) + 1024;
        let body = to_bytes(resp.into_body(), limit).await.unwrap().to_vec();
        (status, body)
    }

    const MOCK_GIF: &[u8] = PLACEHOLDER_GIF;

    #[derive(Clone, Copy)]
    enum MockMode {
        Gif,
        PlainText,
        RedirectChain { hops: usize },
    }

    async fn spawn_mock_upstream(
        hits: Arc<AtomicUsize>,
        mode: MockMode,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}");

        let app = match mode {
            MockMode::Gif => {
                let hits = hits.clone();
                Router::new().route(
                    "/img.gif",
                    get(move || {
                        let hits = hits.clone();
                        async move {
                            hits.fetch_add(1, Ordering::SeqCst);
                            ([(header::CONTENT_TYPE, "image/gif")], MOCK_GIF.to_vec())
                        }
                    }),
                )
            }
            MockMode::PlainText => Router::new().route(
                "/not-image",
                get(|| async {
                    (
                        [(header::CONTENT_TYPE, "text/plain")],
                        b"not an image".to_vec(),
                    )
                }),
            ),
            MockMode::RedirectChain { hops } => {
                let base = base.clone();
                Router::new().route(
                    "/r/{n}",
                    get(move |AxumPath(n): AxumPath<usize>| {
                        let base = base.clone();
                        async move {
                            if n >= hops {
                                (
                                    StatusCode::OK,
                                    [(header::CONTENT_TYPE, "image/gif")],
                                    MOCK_GIF.to_vec(),
                                )
                                    .into_response()
                            } else {
                                let loc = format!("{base}/r/{}", n + 1);
                                (StatusCode::FOUND, [(header::LOCATION, loc)], Vec::new())
                                    .into_response()
                            }
                        }
                    }),
                )
            }
        };

        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        tokio::task::yield_now().await;
        (base, handle)
    }

    #[tokio::test]
    async fn refuse_private_ip_targets() {
        let _hold = LoopbackGuard::hold_off().await;
        let data_dir = temp_data_dir();
        let state = test_auth_state(test_pool().await, &data_dir);
        let (path, query) =
            signed_proxy_parts(&state, "user-ssrf", "http://10.0.0.1/secret.gif").await;
        let (status, body) = response_bytes(call_proxy(state, path, query).await).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body, PLACEHOLDER_GIF);
        let _ = std::fs::remove_dir_all(&data_dir);
    }

    #[tokio::test]
    async fn refuse_loopback_without_test_escape() {
        let _hold = LoopbackGuard::hold_off().await;
        let data_dir = temp_data_dir();
        let state = test_auth_state(test_pool().await, &data_dir);
        let (path, query) =
            signed_proxy_parts(&state, "user-loop", "http://127.0.0.1:9/x.gif").await;
        let (status, body) = response_bytes(call_proxy(state, path, query).await).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body, PLACEHOLDER_GIF);
        let _ = std::fs::remove_dir_all(&data_dir);
    }

    #[tokio::test]
    async fn bad_signature_returns_placeholder() {
        let data_dir = temp_data_dir();
        let state = test_auth_state(test_pool().await, &data_dir);
        let (path, mut query) =
            signed_proxy_parts(&state, "user-sig", "https://example.com/a.gif").await;
        query.sig = "deadbeef".into();
        let (status, body) = response_bytes(call_proxy(state, path, query).await).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body, PLACEHOLDER_GIF);
        let _ = std::fs::remove_dir_all(&data_dir);
    }

    #[tokio::test]
    async fn expired_signature_returns_placeholder() {
        let data_dir = temp_data_dir();
        let state = test_auth_state(test_pool().await, &data_dir);
        let user = "user-exp";
        let url = "https://example.com/a.gif";
        let signer = ProxySigner::new(state.kv(), user).await.unwrap();
        let exp = now_unix() - 10;
        let sig = compute_sig(&signer.secret, user, url, exp);
        let path = encode_proxy_path(url);
        let query = ProxyQuery {
            exp,
            sig,
            uid: user.into(),
        };
        let (status, body) = response_bytes(call_proxy(state, path, query).await).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body, PLACEHOLDER_GIF);
        let _ = std::fs::remove_dir_all(&data_dir);
    }

    #[tokio::test]
    async fn reject_non_image_content_type() {
        let _guard = LoopbackGuard::enter().await;
        let data_dir = temp_data_dir();
        let state = test_auth_state(test_pool().await, &data_dir);
        let hits = Arc::new(AtomicUsize::new(0));
        let (base, handle) = spawn_mock_upstream(hits, MockMode::PlainText).await;
        let url = format!("{base}/not-image");
        let (path, query) = signed_proxy_parts(&state, "user-ct", &url).await;
        let (status, body) = response_bytes(call_proxy(state, path, query).await).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body, PLACEHOLDER_GIF);
        handle.abort();
        let _ = std::fs::remove_dir_all(&data_dir);
    }

    #[tokio::test]
    async fn cache_hit_fetches_upstream_once() {
        let _guard = LoopbackGuard::enter().await;
        let data_dir = temp_data_dir();
        let state = test_auth_state(test_pool().await, &data_dir);
        let hits = Arc::new(AtomicUsize::new(0));
        let (base, handle) = spawn_mock_upstream(hits.clone(), MockMode::Gif).await;
        let url = format!("{base}/img.gif");
        let (path, query) = signed_proxy_parts(&state, "user-cache", &url).await;

        let (status1, body1) = response_bytes(
            call_proxy(
                state.clone(),
                path.clone(),
                ProxyQuery {
                    exp: query.exp,
                    sig: query.sig.clone(),
                    uid: query.uid.clone(),
                },
            )
            .await,
        )
        .await;
        assert_eq!(status1, StatusCode::OK);
        assert_eq!(body1, MOCK_GIF);

        let (status2, body2) = response_bytes(call_proxy(state, path, query).await).await;
        assert_eq!(status2, StatusCode::OK);
        assert_eq!(body2, MOCK_GIF);
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "second render must be a cache hit"
        );

        handle.abort();
        let _ = std::fs::remove_dir_all(&data_dir);
    }

    #[tokio::test]
    async fn query_string_survives_proxy_roundtrip() {
        let _guard = LoopbackGuard::enter().await;
        let data_dir = temp_data_dir();
        let state = test_auth_state(test_pool().await, &data_dir);
        let hits = Arc::new(AtomicUsize::new(0));
        let (base, handle) = spawn_mock_upstream(hits.clone(), MockMode::Gif).await;
        let url = format!("{base}/img.gif?id=abc&utm_source=track");
        let (path, query) = signed_proxy_parts(&state, "user-qs", &url).await;
        assert!(path.contains("%3F"), "query must be path-encoded");
        let (status, body) = response_bytes(call_proxy(state, path, query).await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, MOCK_GIF);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        handle.abort();
        let _ = std::fs::remove_dir_all(&data_dir);
    }

    #[tokio::test]
    async fn too_many_redirects_returns_placeholder() {
        let _guard = LoopbackGuard::enter().await;
        let data_dir = temp_data_dir();
        let state = test_auth_state(test_pool().await, &data_dir);
        let hits = Arc::new(AtomicUsize::new(0));
        let (base, handle) = spawn_mock_upstream(hits, MockMode::RedirectChain { hops: 5 }).await;
        let url = format!("{base}/r/0");
        let (path, query) = signed_proxy_parts(&state, "user-redir", &url).await;
        let (status, body) = response_bytes(call_proxy(state, path, query).await).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body, PLACEHOLDER_GIF);
        handle.abort();
        let _ = std::fs::remove_dir_all(&data_dir);
    }

    #[tokio::test]
    async fn within_redirect_cap_succeeds() {
        let _guard = LoopbackGuard::enter().await;
        let data_dir = temp_data_dir();
        let state = test_auth_state(test_pool().await, &data_dir);
        let hits = Arc::new(AtomicUsize::new(0));
        let (base, handle) = spawn_mock_upstream(hits, MockMode::RedirectChain { hops: 3 }).await;
        let url = format!("{base}/r/0");
        let (path, query) = signed_proxy_parts(&state, "user-ok-redir", &url).await;
        let (status, body) = response_bytes(call_proxy(state, path, query).await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, MOCK_GIF);
        handle.abort();
        let _ = std::fs::remove_dir_all(&data_dir);
    }
}
