//! Remote image privacy — block tracking pixels by default (M1).
//!
//! See `docs/specs/2026-08-23-lyra-remote-image-proxy-spec.md`.

use std::fmt::Write;
use std::sync::Arc;
use std::sync::LazyLock;

use axum::{
    Json, Router,
    extract::{Path, State},
    routing::{delete, get, post},
};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::auth::{AuthState, AuthUser};
use crate::kv::KvStore;
use crate::sync::SyncError;

const DEFAULT_REMOTE_IMAGES: &str = "block";

static IMG_TAG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<img\b([^>]*?)>").expect("img tag regex"));

static SRC_ATTR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?is)\bsrc\s*=\s*['"]([^'"]*)['"]"#).expect("src attr regex")
});

/// Per-user privacy settings stored in kv.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PrivacySettings {
    #[serde(default = "default_remote_images")]
    pub remote_images: String,
    #[serde(default)]
    pub remote_content_allowlist: Vec<String>,
}

fn default_remote_images() -> String {
    DEFAULT_REMOTE_IMAGES.to_string()
}

impl Default for PrivacySettings {
    fn default() -> Self {
        Self {
            remote_images: DEFAULT_REMOTE_IMAGES.to_string(),
            remote_content_allowlist: Vec::new(),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrivacySettingsResponse {
    pub remote_images: String,
    pub remote_content_allowlist: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchPrivacyRequest {
    pub remote_images: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AllowSenderRequest {
    pub sender: String,
}

/// Result of serve-time HTML rewriting.
#[derive(Debug)]
pub struct RewriteResult {
    pub html: String,
    pub blocked: bool,
}

fn kv_key(user_id: &str) -> String {
    format!("user:{user_id}:privacy")
}

pub async fn load_settings(kv: &Arc<dyn KvStore>, user_id: &str) -> Result<PrivacySettings, SyncError> {
    let key = kv_key(user_id);
    let raw = kv
        .get(&key)
        .await
        .map_err(|e| SyncError::Internal(e.to_string()))?;
    match raw {
        None => Ok(PrivacySettings::default()),
        Some(json) => serde_json::from_str(&json)
            .map_err(|e| SyncError::Internal(format!("privacy settings corrupt: {e}"))),
    }
}

async fn save_settings(
    kv: &Arc<dyn KvStore>,
    user_id: &str,
    settings: &PrivacySettings,
) -> Result<(), SyncError> {
    let json = serde_json::to_string(settings)
        .map_err(|e| SyncError::Internal(format!("privacy settings encode: {e}")))?;
    kv
        .set(&kv_key(user_id), &json, None)
        .await
        .map_err(|e| SyncError::Internal(e.to_string()))?;
    Ok(())
}

/// Lowercased email from stored `from_address` JSON (`raw` or `email` field).
pub fn sender_email_from_json(from_address: Option<&str>) -> Option<String> {
    let raw = from_address?;
    let parsed: serde_json::Value = serde_json::from_str(raw).ok()?;
    if let Some(s) = parsed.get("raw").and_then(|v| v.as_str()) {
        return extract_email(s);
    }
    if let Some(s) = parsed.get("email").and_then(|v| v.as_str()) {
        return Some(s.trim().to_lowercase());
    }
    if let Some(s) = parsed.as_str() {
        return extract_email(s);
    }
    None
}

fn extract_email(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if let Some(start) = trimmed.rfind('<')
        && let Some(end) = trimmed.rfind('>')
        && start < end
    {
        return Some(trimmed[start + 1..end].trim().to_lowercase());
    }
    if trimmed.contains('@') {
        return Some(trimmed.to_lowercase());
    }
    None
}

fn is_remote_http_url(url: &str) -> bool {
    let u = url.trim();
    u.starts_with("http://") || u.starts_with("https://")
}

/// Replace remote `img` tags per privacy policy.
pub fn rewrite_remote_images(
    html: &str,
    allow_remote: bool,
    proxy_signer: Option<&crate::media::ProxySigner>,
) -> RewriteResult {
    if html.is_empty() {
        return RewriteResult {
            html: html.to_string(),
            blocked: false,
        };
    }

    if !allow_remote {
        return rewrite_blocked(html);
    }

    if let Some(signer) = proxy_signer {
        return rewrite_proxy(html, signer);
    }

    RewriteResult {
        html: html.to_string(),
        blocked: false,
    }
}

fn rewrite_blocked(html: &str) -> RewriteResult {
    let mut blocked = false;
    let mut out = String::with_capacity(html.len());
    let mut last = 0;

    for caps in IMG_TAG_RE.captures_iter(html) {
        let full = caps.get(0).expect("full match");
        let attrs = caps.get(1).map_or("", |m| m.as_str());
        out.push_str(&html[last..full.start()]);

        let src = SRC_ATTR_RE
            .captures(attrs)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str());

        if src.is_some_and(is_remote_http_url) {
            blocked = true;
            let alt = extract_alt(attrs).unwrap_or_else(|| "Image".to_string());
            let title = escape_attr(&alt);
            let _ = write!(
                out,
                r#"<span data-lyra-blocked-img="1" class="lyra-blocked-img" title="{title}" aria-label="{title}">[Image]</span>"#
            );
        } else {
            out.push_str(full.as_str());
        }

        last = full.end();
    }

    out.push_str(&html[last..]);

    RewriteResult {
        html: out,
        blocked,
    }
}

fn rewrite_proxy(html: &str, signer: &crate::media::ProxySigner) -> RewriteResult {
    let mut out = String::with_capacity(html.len());
    let mut last = 0;

    for caps in IMG_TAG_RE.captures_iter(html) {
        let full = caps.get(0).expect("full match");
        let attrs = caps.get(1).map_or("", |m| m.as_str());
        out.push_str(&html[last..full.start()]);

        let src = SRC_ATTR_RE
            .captures(attrs)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str());

        if src.is_some_and(is_remote_http_url) {
            let proxy_url = signer.sign_url(src.unwrap_or_default());
            let new_attrs = SRC_ATTR_RE.replace(attrs, format!("src=\"{proxy_url}\""));
            let _ = write!(out, "<img{new_attrs}>");
        } else {
            out.push_str(full.as_str());
        }

        last = full.end();
    }

    out.push_str(&html[last..]);

    RewriteResult {
        html: out,
        blocked: false,
    }
}

fn extract_alt(attrs: &str) -> Option<String> {
    static ALT_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"(?is)\balt\s*=\s*['"]([^'"]*)['"]"#).expect("alt regex"));
    ALT_RE
        .captures(attrs)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().trim().to_string())
        .filter(|s| !s.is_empty())
}

fn escape_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
}

pub fn should_allow_remote(
    settings: &PrivacySettings,
    sender_email: Option<&str>,
    query_allow: bool,
) -> bool {
    if query_allow {
        return true;
    }
    if let Some(email) = sender_email {
        let lower = email.to_lowercase();
        if settings.remote_content_allowlist.iter().any(|e| e == &lower) {
            return true;
        }
    }
    settings.remote_images != DEFAULT_REMOTE_IMAGES
}

pub fn routes() -> Router<AuthState> {
    Router::new()
        .route("/api/v1/settings/privacy", get(get_privacy).patch(patch_privacy))
        .route(
            "/api/v1/settings/privacy/allow-sender",
            post(allow_sender),
        )
        .route(
            "/api/v1/settings/privacy/allow-sender/{sender}",
            delete(remove_allow_sender),
        )
}

async fn get_privacy(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<PrivacySettingsResponse>, SyncError> {
    let settings = load_settings(state.kv(), &user_id).await?;
    Ok(Json(PrivacySettingsResponse {
        remote_images: settings.remote_images,
        remote_content_allowlist: settings.remote_content_allowlist,
    }))
}

async fn patch_privacy(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Json(body): Json<PatchPrivacyRequest>,
) -> Result<Json<PrivacySettingsResponse>, SyncError> {
    let mut settings = load_settings(state.kv(), &user_id).await?;
    if let Some(mode) = body.remote_images {
        if mode != "block" && mode != "proxy" {
            return Err(SyncError::InvalidInput(
                "remoteImages must be block or proxy".into(),
            ));
        }
        settings.remote_images = mode;
    }
    save_settings(state.kv(), &user_id, &settings).await?;
    Ok(Json(PrivacySettingsResponse {
        remote_images: settings.remote_images,
        remote_content_allowlist: settings.remote_content_allowlist,
    }))
}

async fn allow_sender(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Json(body): Json<AllowSenderRequest>,
) -> Result<Json<PrivacySettingsResponse>, SyncError> {
    let sender = body.sender.trim().to_lowercase();
    if !sender.contains('@') {
        return Err(SyncError::InvalidInput("sender must be an email address".into()));
    }
    let mut settings = load_settings(state.kv(), &user_id).await?;
    if !settings.remote_content_allowlist.contains(&sender) {
        settings.remote_content_allowlist.push(sender);
        save_settings(state.kv(), &user_id, &settings).await?;
    }
    Ok(Json(PrivacySettingsResponse {
        remote_images: settings.remote_images,
        remote_content_allowlist: settings.remote_content_allowlist,
    }))
}

async fn remove_allow_sender(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Path(sender): Path<String>,
) -> Result<Json<PrivacySettingsResponse>, SyncError> {
    let sender = sender.trim().to_lowercase();
    let mut settings = load_settings(state.kv(), &user_id).await?;
    settings.remote_content_allowlist.retain(|e| e != &sender);
    save_settings(state.kv(), &user_id, &settings).await?;
    Ok(Json(PrivacySettingsResponse {
        remote_images: settings.remote_images,
        remote_content_allowlist: settings.remote_content_allowlist,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::ProxySigner;

    #[test]
    fn blocks_remote_img_src() {
        let html = r#"<p>Hi <img src="https://evil.com/pixel.gif" alt="track">!</p>"#;
        let r = rewrite_remote_images(html, false, None);
        assert!(r.blocked);
        assert!(!r.html.contains("evil.com"));
        assert!(r.html.contains("data-lyra-blocked-img"));
    }

    #[test]
    fn allows_when_requested() {
        let html = r#"<img src="https://evil.com/pixel.gif">"#;
        let r = rewrite_remote_images(html, true, None);
        assert!(!r.blocked);
        assert!(r.html.contains("evil.com"));
    }

    #[test]
    fn rewrites_remote_to_proxy_url() {
        let signer = ProxySigner {
            user_id: "user-1".to_string(),
            secret: b"test-secret-key-32-bytes-long!!!".to_vec(),
        };
        let html = r#"<img src="https://evil.com/pixel.gif" alt="x">"#;
        let r = rewrite_remote_images(html, true, Some(&signer));
        assert!(!r.blocked);
        assert!(r.html.contains("/api/v1/proxy/"));
        assert!(r.html.contains("sig="));
        assert!(!r.html.contains("src=\"https://evil.com"));
    }

    #[test]
    fn keeps_cid_and_relative() {
        let html = r#"<img src="cid:part1"><img src="/local">"#;
        let r = rewrite_remote_images(html, false, None);
        assert!(!r.blocked);
        assert!(r.html.contains("cid:part1"));
    }

    #[test]
    fn sender_email_from_json_raw() {
        let j = r#"{"raw":"Sales <sales@example.com>"}"#;
        assert_eq!(
            sender_email_from_json(Some(j)),
            Some("sales@example.com".to_string())
        );
    }
}
