//! Remote image proxy — signed URLs, disk cache, SSRF-safe upstream fetch.
//!
//! See `docs/specs/2026-08-23-lyra-remote-image-proxy-spec.md` M2.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::{
    extract::{Path as AxumPath, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use hmac::{Hmac, Mac};
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
    0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x01, 0x00, 0x01, 0x00, 0x80, 0x00, 0x00, 0xff, 0xff,
    0xff, 0x00, 0x00, 0x00, 0x21, 0xf9, 0x04, 0x01, 0x00, 0x00, 0x00, 0x00, 0x2c, 0x00, 0x00,
    0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x02, 0x02, 0x44, 0x01, 0x00, 0x3b,
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
    use rand::RngCore;
    rand::thread_rng().fill_bytes(&mut secret);
    let encoded = base64_encode(&secret);
    kv
        .set(&key, &encoded, None)
        .await
        .map_err(|e| SyncError::Internal(e.to_string()))?;
    Ok(secret.to_vec())
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(s).ok()
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs() as i64
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
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
    let mut mac =
        HmacSha256::new_from_slice(secret).expect("HMAC accepts any key length");
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

async fn validate_outbound_url(url: &str) -> Result<(), SyncError> {
    let parsed = reqwest::Url::parse(url)
        .map_err(|_| SyncError::InvalidInput("invalid proxy target URL".into()))?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err(SyncError::InvalidInput("proxy target must be http(s)".into()));
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
        return Err(SyncError::InvalidInput("proxy target blocked by SSRF policy".into()));
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
                .map(|s| s.to_string());
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
        if body.len() > MAX_UPSTREAM_BYTES as usize {
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

async fn write_cache(cache_root: &Path, url: &str, bytes: &[u8], content_type: &str) -> Result<(), SyncError> {
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
        let read_dir = match std::fs::read_dir(&dir) {
            Ok(d) => d,
            Err(_) => continue,
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
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
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
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("image/gif"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store"),
    );
    (StatusCode::NOT_FOUND, headers, PLACEHOLDER_GIF).into_response()
}

fn image_response(bytes: Vec<u8>, content_type: &str, cached: bool) -> Response {
    let mut headers = HeaderMap::new();
    if let Ok(ct) = HeaderValue::from_str(content_type) {
        headers.insert(header::CONTENT_TYPE, ct);
    }
    let cache_control = if cached {
        "private, max-age=31536000, immutable"
    } else {
        "private, max-age=31536000, immutable"
    };
    if let Ok(v) = HeaderValue::from_str(cache_control) {
        headers.insert(header::CACHE_CONTROL, v);
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
}
