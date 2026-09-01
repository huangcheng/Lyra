//! Sender avatar resolution: contact photo → BIMI (VMC-validated) →
//! opt-in Gravatar. One endpoint hides the chain; every upstream fetch goes
//! through the media pipeline (SSRF guard, caps, sniffing), so no third
//! party sees the user's IP and Gravatar sees nothing unless opted in.
//!
//! `GET /api/v1/avatars/{email}` is bearer-gated (frontend uses `apiBlob`);
//! it is intentionally not `<img>`-safe.

use std::future::Future;
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
    gravatar_url_with_base(&gravatar_base(), email)
}

fn gravatar_url_with_base(base: &str, email: &str) -> String {
    let digest = md5::Md5::digest(email.trim().to_ascii_lowercase().as_bytes());
    format!("{base}/avatar/{digest:x}?d=404&s=128")
}

/// Gravatar base URL. Endpoint tests override it to point at a loopback
/// mock upstream (`media::LoopbackGuard` serializes those tests, so the
/// override can never leak between tests or into production builds).
fn gravatar_base() -> String {
    #[cfg(test)]
    if let Some(base) = GRAVATAR_BASE_FOR_TESTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
    {
        return base;
    }
    "https://www.gravatar.com".to_string()
}

#[cfg(test)]
static GRAVATAR_BASE_FOR_TESTS: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

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
    if !parts
        .next()
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("v=DMARC1"))
    {
        return false;
    }
    parts
        .map(str::trim)
        .find_map(|part| part.strip_prefix("p="))
        .is_some_and(|p| p == "quarantine" || p == "reject")
}

/// Cap on the VMC evidence document (PEM bundle) fetched from `a=`.
const MAX_VMC_BUNDLE_BYTES: u64 = 1024 * 1024;

/// DMARC/BIMI lookup levels for a From domain, most specific first
/// (RFC 7489 §6.6.3 tree walk). Without a public-suffix list (not in the
/// dependency tree) the organizational domain is approximated: the last
/// two labels, plus — for domains with ≥4 labels — the last three labels
/// first (`example.co.uk`-style). Deeper guesses come first so a public
/// suffix like `co.uk` can never shadow the registrable candidate; the
/// suffix itself may be queried last, pointlessly but harmlessly.
pub(crate) fn candidate_domains(domain: &str) -> Vec<String> {
    let labels: Vec<&str> = domain.split('.').filter(|l| !l.is_empty()).collect();
    let mut out = vec![labels.join(".")];
    if labels.len() >= 4 {
        out.push(labels[labels.len() - 3..].join("."));
    }
    if labels.len() >= 3 {
        out.push(labels[labels.len() - 2..].join("."));
    }
    out.dedup();
    out
}

/// DNS answer classification for the DMARC/BIMI tree walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DnsAnswer {
    Record(Vec<u8>),
    /// NXDOMAIN/NODATA: no record at this level — walk up.
    NoRecord,
    /// Timeout/SERVFAIL/resolver failure: transient — retry soon.
    Error,
}

fn classify_dns(result: mail_auth::Result<Vec<u8>>) -> DnsAnswer {
    match result {
        Ok(txt) if txt.is_empty() => DnsAnswer::NoRecord,
        Ok(txt) => DnsAnswer::Record(txt),
        Err(mail_auth::Error::Dns(mail_auth::DnsError::RecordNotFound(_))) => DnsAnswer::NoRecord,
        Err(_) => DnsAnswer::Error,
    }
}

/// What the DNS tree walk decided for a From domain.
#[derive(Debug, PartialEq, Eq)]
enum BimiPlan {
    /// No enforcing DMARC record, or no parseable BIMI record anywhere up
    /// the tree.
    CleanMiss,
    /// Transient DNS failure — negative-cache briefly, not for a day.
    Error,
    /// BIMI record found at `level`; proceed to VMC validation there.
    Fetch { level: String, record: BimiRecord },
}

type BoxedLookup<'a> = dyn Fn(String) -> std::pin::Pin<Box<dyn Future<Output = DnsAnswer> + Send + 'a>>
    + Send
    + Sync
    + 'a;

/// DMARC gate + BIMI record discovery, injectable DNS for tests.
/// Both walks are independent: the gate passes at the level where a
/// DMARC record exists; the BIMI record may live at a different level.
async fn plan_bimi(domain: &str, lookup: &BoxedLookup<'_>) -> BimiPlan {
    let candidates = candidate_domains(domain);
    // DMARC gate: the first level (walking up) that publishes a DMARC
    // record decides; NXDOMAIN falls through to the parent.
    let mut gate_passed = false;
    for level in &candidates {
        match lookup(format!("_dmarc.{level}")).await {
            DnsAnswer::NoRecord => {}
            DnsAnswer::Error => return BimiPlan::Error,
            DnsAnswer::Record(txt) => {
                if !dmarc_allows_bimi(&String::from_utf8_lossy(&txt)) {
                    return BimiPlan::CleanMiss;
                }
                gate_passed = true;
                break;
            }
        }
    }
    if !gate_passed {
        return BimiPlan::CleanMiss;
    }
    // BIMI record: the first level with a record wins; an unparseable
    // record is the publisher's mistake — clean miss, not an error.
    for level in candidates {
        match lookup(format!("default._bimi.{level}")).await {
            DnsAnswer::NoRecord => {}
            DnsAnswer::Error => return BimiPlan::Error,
            DnsAnswer::Record(txt) => {
                return match parse_bimi_record(&txt) {
                    Some(record) => BimiPlan::Fetch { level, record },
                    None => BimiPlan::CleanMiss,
                };
            }
        }
    }
    BimiPlan::CleanMiss
}

/// Outcome of the full BIMI pipeline (DNS → VMC → logo fetch). Drives the
/// negative-cache TTL: clean misses cache for a day, transient errors
/// heal in minutes.
enum BimiOutcome {
    Logo(FetchedImage),
    CleanMiss,
    Error,
}

/// BIMI logo for a From domain. DMARC gate → record parse → VMC
/// validation → logo fetch, walking up to the organizational domain.
async fn resolve_bimi_logo(state: &AuthState, domain: &str) -> BimiOutcome {
    let _ = state; // DNS goes through the process-wide DKIM authenticator.

    // Test-only DNS stub: endpoint tests can't rely on a working resolver
    // in CI sandboxes, so they answer from a static map instead. (Bind
    // first: an `if let` on the lock expression would hold the MutexGuard
    // across the awaits below and make the handler future non-Send.)
    #[cfg(test)]
    let stubbed = BIMI_DNS_FOR_TESTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    #[cfg(test)]
    if let Some(stub) = stubbed {
        let lookup = move |name: String| {
            let answer = stub.get(&name).cloned().unwrap_or(DnsAnswer::NoRecord);
            Box::pin(async move { answer })
                as std::pin::Pin<Box<dyn Future<Output = DnsAnswer> + Send>>
        };
        let plan = plan_bimi(domain, &lookup).await;
        return finish_bimi(plan).await;
    }

    let Ok(auth) = crate::dkim::authenticator() else {
        // No system resolver at all: environment failure, not a miss.
        return BimiOutcome::Error;
    };
    let lookup = move |name: String| {
        Box::pin(async move { classify_dns(auth.txt_raw_lookup(name).await) })
            as std::pin::Pin<Box<dyn Future<Output = DnsAnswer> + Send>>
    };
    let plan = plan_bimi(domain, &lookup).await;
    finish_bimi(plan).await
}

#[cfg(test)]
static BIMI_DNS_FOR_TESTS: std::sync::Mutex<
    Option<std::sync::Arc<std::collections::HashMap<String, DnsAnswer>>>,
> = std::sync::Mutex::new(None);

/// VMC validation + logo fetch for a discovered BIMI record. Fetch
/// failures (network, 5xx) are transient errors; invalid content (bad
/// PEM, rejected VMC, non-image logo) is a clean miss.
async fn finish_bimi(plan: BimiPlan) -> BimiOutcome {
    let BimiPlan::Fetch { level, record } = plan else {
        return match plan {
            BimiPlan::CleanMiss => BimiOutcome::CleanMiss,
            BimiPlan::Error => BimiOutcome::Error,
            BimiPlan::Fetch { .. } => unreachable!(),
        };
    };
    let Some(authority) = record.authority_url.as_deref() else {
        // No VMC to validate against: BIMI without evidence is a clean miss.
        return BimiOutcome::CleanMiss;
    };
    let pem = match media::fetch_bytes(authority, MAX_VMC_BUNDLE_BYTES).await {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(pem) => pem,
            Err(_) => return BimiOutcome::CleanMiss, // PEM is ASCII
        },
        Err(e) => {
            tracing::debug!(error = %e, "bimi vmc fetch failed");
            return BimiOutcome::Error;
        }
    };
    if let Err(e) = crate::bimi::validate_vmc(pem.as_bytes(), &level).await {
        tracing::debug!(level, error = %e, "bimi vmc validation failed");
        return BimiOutcome::CleanMiss;
    }
    fetch_logo(&record.logo_url).await
}

/// Logo fetch through the media pipeline, accepting raster or SVG.
async fn fetch_logo(url: &str) -> BimiOutcome {
    match media::fetch_bimi_logo(url).await {
        Ok(img) => BimiOutcome::Logo(img),
        // Not an image / oversize: the publisher's content is broken — a
        // clean miss, safe to negative-cache for a day.
        Err(SyncError::InvalidInput(e)) => {
            tracing::debug!(error = %e, "bimi logo rejected");
            BimiOutcome::CleanMiss
        }
        // Network/5xx: transient — retry in minutes.
        Err(e) => {
            tracing::debug!(error = %e, "bimi logo fetch failed");
            BimiOutcome::Error
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
    db: &crate::storage::DbPool,
    data_dir: &Path,
    user_id: &str,
    email: &str,
) -> Result<Option<Response>, SyncError> {
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
        let Ok(bytes) = crate::blobs::read(data_dir, &photo_path).await else {
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
    if let Some(resp) =
        contact_photo_response(state.db(), &state.data_dir, &user_id, &email).await?
    {
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
    let mut upstream_error = false;
    let mut fetched = match resolve_bimi_logo(&state, domain).await {
        BimiOutcome::Logo(img) => Some(img),
        BimiOutcome::CleanMiss => None,
        // DNS timeout / VMC or logo fetch failure: transient, heal in minutes.
        BimiOutcome::Error => {
            upstream_error = true;
            None
        }
    };
    if fetched.is_none() && settings.gravatar_avatars {
        match media::fetch_upstream_status(&gravatar_url(&email)).await {
            Ok(media::UpstreamOutcome::Image(img)) => fetched = Some(img),
            // Gravatar's `d=404` "no avatar" (any 4xx) is a clean miss;
            // 5xx counts as an upstream error.
            Ok(media::UpstreamOutcome::HttpStatus(status)) => {
                tracing::debug!(status, "gravatar avatar fetch non-success");
                if status >= 500 {
                    upstream_error = true;
                }
            }
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn gravatar_url_hashes_lowercased_trimmed_email() {
        // Pure helper: no global base override involved (endpoint tests
        // install one; this test must stay race-free against them).
        assert_eq!(
            gravatar_url_with_base("https://www.gravatar.com", "  HuangCheng@Example.COM "),
            "https://www.gravatar.com/avatar/64774d1724f12eae92bd80a2feb660b1?d=404&s=128"
        );
    }

    #[test]
    fn candidate_domains_walks_up_to_org_guess() {
        assert_eq!(candidate_domains("example.com"), vec!["example.com"]);
        assert_eq!(
            candidate_domains("mail.example.com"),
            vec!["mail.example.com", "example.com"]
        );
        // co.uk-style: the three-label guess comes before the bare suffix.
        assert_eq!(
            candidate_domains("mail.example.co.uk"),
            vec!["mail.example.co.uk", "example.co.uk", "co.uk"]
        );
        assert_eq!(
            candidate_domains("a.b.example.com"),
            vec!["a.b.example.com", "b.example.com", "example.com"]
        );
    }

    /// DNS stub for `plan_bimi` tests: name → answer, unlisted names are
    /// NXDOMAIN-class (`NoRecord`).
    fn lookup_from(
        records: &[(&str, DnsAnswer)],
    ) -> impl Fn(String) -> std::pin::Pin<Box<dyn Future<Output = DnsAnswer> + Send>> + use<> {
        let map: std::collections::HashMap<String, DnsAnswer> = records
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect();
        move |name| {
            let answer = map.get(&name).cloned().unwrap_or(DnsAnswer::NoRecord);
            Box::pin(async move { answer })
        }
    }

    fn txt(s: &str) -> DnsAnswer {
        DnsAnswer::Record(s.as_bytes().to_vec())
    }

    #[tokio::test]
    async fn plan_bimi_exact_domain_hit() {
        let lookup = lookup_from(&[
            ("_dmarc.example.com", txt("v=DMARC1; p=reject;")),
            (
                "default._bimi.example.com",
                txt("v=BIMI1; l=https://example.com/logo.svg;"),
            ),
        ]);
        let plan = plan_bimi("example.com", &lookup).await;
        assert_eq!(
            plan,
            BimiPlan::Fetch {
                level: "example.com".into(),
                record: BimiRecord {
                    logo_url: "https://example.com/logo.svg".into(),
                    authority_url: None,
                },
            }
        );
    }

    #[tokio::test]
    async fn plan_bimi_falls_back_to_organizational_domain() {
        // Nothing at the subdomain; both records live at the org domain.
        let lookup = lookup_from(&[
            ("_dmarc.example.com", txt("v=DMARC1; p=reject;")),
            (
                "default._bimi.example.com",
                txt("v=BIMI1; l=https://example.com/logo.svg;"),
            ),
        ]);
        let plan = plan_bimi("mail.example.com", &lookup).await;
        assert!(matches!(
            plan,
            BimiPlan::Fetch { ref level, .. } if level == "example.com"
        ));
    }

    #[tokio::test]
    async fn plan_bimi_dmarc_and_bimi_may_live_at_different_levels() {
        // DMARC at the org domain, BIMI record on the exact subdomain.
        let lookup = lookup_from(&[
            ("_dmarc.example.com", txt("v=DMARC1; p=quarantine;")),
            (
                "default._bimi.mail.example.com",
                txt("v=BIMI1; l=https://example.com/logo.svg;"),
            ),
        ]);
        let plan = plan_bimi("mail.example.com", &lookup).await;
        assert!(matches!(
            plan,
            BimiPlan::Fetch { ref level, .. } if level == "mail.example.com"
        ));
    }

    #[tokio::test]
    async fn plan_bimi_co_uk_style_org_domain() {
        let lookup = lookup_from(&[
            ("_dmarc.example.co.uk", txt("v=DMARC1; p=reject;")),
            (
                "default._bimi.example.co.uk",
                txt("v=BIMI1; l=https://example.co.uk/logo.svg;"),
            ),
        ]);
        let plan = plan_bimi("mail.example.co.uk", &lookup).await;
        assert!(matches!(
            plan,
            BimiPlan::Fetch { ref level, .. } if level == "example.co.uk"
        ));
    }

    #[tokio::test]
    async fn plan_bimi_clean_misses() {
        // DMARC exists but doesn't enforce: gate fails, walk stops (the
        // first DMARC record decides, even with an enforcing org record).
        let lookup = lookup_from(&[
            ("_dmarc.mail.example.com", txt("v=DMARC1; p=none;")),
            ("_dmarc.example.com", txt("v=DMARC1; p=reject;")),
        ]);
        assert_eq!(
            plan_bimi("mail.example.com", &lookup).await,
            BimiPlan::CleanMiss
        );

        // No DMARC anywhere up the tree.
        let lookup = lookup_from(&[]);
        assert_eq!(
            plan_bimi("mail.example.com", &lookup).await,
            BimiPlan::CleanMiss
        );

        // DMARC enforces but no BIMI record anywhere.
        let lookup = lookup_from(&[("_dmarc.example.com", txt("v=DMARC1; p=reject;"))]);
        assert_eq!(
            plan_bimi("mail.example.com", &lookup).await,
            BimiPlan::CleanMiss
        );

        // BIMI record present but unparseable.
        let lookup = lookup_from(&[
            ("_dmarc.example.com", txt("v=DMARC1; p=reject;")),
            ("default._bimi.example.com", txt("v=DMARC1; p=reject;")),
        ]);
        assert_eq!(plan_bimi("example.com", &lookup).await, BimiPlan::CleanMiss);
    }

    #[tokio::test]
    async fn plan_bimi_dns_error_is_an_error_not_a_miss() {
        let lookup = lookup_from(&[("_dmarc.mail.example.com", DnsAnswer::Error)]);
        assert_eq!(
            plan_bimi("mail.example.com", &lookup).await,
            BimiPlan::Error
        );

        // Gate passed; the BIMI lookup errors.
        let lookup = lookup_from(&[
            ("_dmarc.example.com", txt("v=DMARC1; p=reject;")),
            ("default._bimi.example.com", DnsAnswer::Error),
        ]);
        assert_eq!(plan_bimi("example.com", &lookup).await, BimiPlan::Error);
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
        test_auth_state_kv(pool, data_dir).0
    }

    /// AuthState plus its [`MemoryKv`] clone, for TTL inspection.
    fn test_auth_state_kv(pool: sqlx::SqlitePool, data_dir: &Path) -> (AuthState, MemoryKv) {
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
        let kv = MemoryKv::new();
        let state = AuthState::new(
            DbPool::Sqlite(pool),
            &config,
            Arc::new(App::new()),
            Arc::new(kv.clone()),
        )
        .unwrap();
        (state, kv)
    }

    fn temp_data_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lyra-avatars-test-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A minimal but magic-correct PNG (header + IHDR chunk bytes).
    pub(super) const MOCK_PNG: &[u8] = &[
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

    // ── Gravatar endpoint tests (loopback mock upstream) ─────────────

    /// What the loopback mock "Gravatar" replies with.
    #[derive(Clone, Copy)]
    enum MockReply {
        Png,
        NotFound,
        ServerError,
    }

    /// Loopback stand-in for Gravatar: counts hits so tests can prove
    /// whether upstream was contacted at all.
    async fn spawn_avatar_mock(
        hits: Arc<AtomicUsize>,
        reply: MockReply,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}");
        let app = axum::Router::new().route(
            "/avatar/{hash}",
            get(move |AxumPath(_hash): AxumPath<String>| {
                let hits = hits.clone();
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    match reply {
                        MockReply::Png => {
                            ([(header::CONTENT_TYPE, "image/png")], MOCK_PNG.to_vec())
                                .into_response()
                        }
                        MockReply::NotFound => StatusCode::NOT_FOUND.into_response(),
                        MockReply::ServerError => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
                    }
                }
            }),
        );
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        tokio::task::yield_now().await;
        (base, handle)
    }

    /// Endpoint-test network fixture: allows loopback upstreams, points
    /// the Gravatar base at the mock, and stubs BIMI DNS with NXDOMAIN
    /// answers (CI sandboxes may have no resolver). Serialized
    /// process-wide via `media::LoopbackGuard`.
    struct AvatarNet {
        _guard: media::LoopbackGuard,
    }

    impl AvatarNet {
        async fn enter(mock_base: &str) -> Self {
            let guard = media::LoopbackGuard::enter().await;
            *GRAVATAR_BASE_FOR_TESTS
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(mock_base.to_string());
            *BIMI_DNS_FOR_TESTS
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                Some(Arc::new(std::collections::HashMap::new()));
            Self { _guard: guard }
        }
    }

    impl Drop for AvatarNet {
        fn drop(&mut self) {
            *GRAVATAR_BASE_FOR_TESTS
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
            *BIMI_DNS_FOR_TESTS
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        }
    }

    async fn enable_gravatar(state: &AuthState, user_id: &str) {
        state
            .kv()
            .set(
                &format!("user:{user_id}:privacy"),
                r#"{"gravatarAvatars":true}"#,
                None,
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn gravatar_opt_out_never_contacts_upstream() {
        let data_dir = temp_data_dir();
        let state = test_auth_state(test_pool().await, &data_dir);
        seed_user_account(&state, "u1").await;
        let hits = Arc::new(AtomicUsize::new(0));
        let (base, handle) = spawn_avatar_mock(hits.clone(), MockReply::Png).await;
        let _net = AvatarNet::enter(&base).await;

        // gravatar_avatars defaults to false: 404 without any fetch.
        let resp = call_avatar(&state, "u1", "ghost@example.com").await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            hits.load(Ordering::SeqCst),
            0,
            "opted-out Gravatar must never be contacted"
        );
        assert!(
            state
                .kv()
                .get(&miss_key("u1", false, "ghost@example.com"))
                .await
                .unwrap()
                .is_some()
        );

        handle.abort();
        let _ = std::fs::remove_dir_all(&data_dir);
    }

    #[tokio::test]
    async fn gravatar_404_is_a_clean_miss_not_an_error() {
        let data_dir = temp_data_dir();
        let (state, kv) = test_auth_state_kv(test_pool().await, &data_dir);
        seed_user_account(&state, "u1").await;
        enable_gravatar(&state, "u1").await;
        let hits = Arc::new(AtomicUsize::new(0));
        let (base, handle) = spawn_avatar_mock(hits.clone(), MockReply::NotFound).await;
        let _net = AvatarNet::enter(&base).await;

        let resp = call_avatar(&state, "u1", "ghost@example.com").await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert_eq!(hits.load(Ordering::SeqCst), 1);

        // `d=404` means "this address has no Gravatar" — a clean miss
        // with the 24h TTL, not a 10-minute error retry.
        let key = miss_key("u1", true, "ghost@example.com");
        let ttl = kv.ttl_remaining(&key).await.expect("negative marker set");
        assert!(
            ttl > Duration::from_secs(MISS_TTL_CLEAN_SECS - 60),
            "gravatar 404 must take the clean-miss TTL, got {ttl:?}"
        );

        handle.abort();
        let _ = std::fs::remove_dir_all(&data_dir);
    }

    #[tokio::test]
    async fn gravatar_5xx_uses_short_error_ttl() {
        let data_dir = temp_data_dir();
        let (state, kv) = test_auth_state_kv(test_pool().await, &data_dir);
        seed_user_account(&state, "u1").await;
        enable_gravatar(&state, "u1").await;
        let hits = Arc::new(AtomicUsize::new(0));
        let (base, handle) = spawn_avatar_mock(hits, MockReply::ServerError).await;
        let _net = AvatarNet::enter(&base).await;

        let resp = call_avatar(&state, "u1", "ghost@example.com").await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        let key = miss_key("u1", true, "ghost@example.com");
        let ttl = kv.ttl_remaining(&key).await.expect("negative marker set");
        assert!(
            ttl <= Duration::from_secs(MISS_TTL_ERROR_SECS)
                && ttl > Duration::from_secs(MISS_TTL_ERROR_SECS - 60),
            "gravatar 5xx must take the 10-minute error TTL, got {ttl:?}"
        );

        handle.abort();
        let _ = std::fs::remove_dir_all(&data_dir);
    }

    #[tokio::test]
    async fn contact_photo_beats_positive_cache_and_gravatar() {
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

        // A fresh positive-cache entry that must lose to the contact photo.
        let cache_root = data_dir.join("media-cache");
        let stale_png = b"\x89\x50\x4e\x47-stale-cache-bytes";
        media::write_cache(
            &cache_root,
            "avatar:friend@example.com",
            stale_png,
            "image/png",
        )
        .await
        .unwrap();

        enable_gravatar(&state, "u1").await;
        let hits = Arc::new(AtomicUsize::new(0));
        let (base, handle) = spawn_avatar_mock(hits.clone(), MockReply::Png).await;
        let _net = AvatarNet::enter(&base).await;

        let resp = call_avatar(&state, "u1", "friend@example.com").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        assert_eq!(body.as_ref(), MOCK_PNG, "contact photo wins over cache");
        assert_eq!(
            hits.load(Ordering::SeqCst),
            0,
            "contact photo short-circuits before Gravatar"
        );

        handle.abort();
        let _ = std::fs::remove_dir_all(&data_dir);
    }
}

#[cfg(test)]
mod postgres_live {
    //! Contact-photo lookup roundtrip under PostgreSQL typing (uuid binds,
    //! jsonb `email_addresses`). See `pgtest` for the harness contract.

    use super::tests::MOCK_PNG;
    use super::*;
    use crate::pgtest::support;
    use crate::storage::DbPool;

    #[test]
    #[ignore = "needs postgres"]
    fn contact_photo_lookup_roundtrip() {
        support::rt().block_on(async {
            let (db, user_id) = support::setup().await;
            let account_id = support::seed_account(&db, &user_id, "pg-avatar@example.com").await;

            let data_dir =
                std::env::temp_dir().join(format!("lyra-avatars-pg-{}", uuid::Uuid::now_v7()));
            std::fs::create_dir_all(&data_dir).unwrap();
            let photo_path = crate::blobs::store(&data_dir, &account_id, MOCK_PNG)
                .await
                .unwrap();
            let DbPool::Postgres(pool) = &db else {
                panic!("expected postgres pool");
            };
            sqlx::query(
                "INSERT INTO contact (id, account_id, display_name, email_addresses, photo_path) \
                 VALUES ($1::uuid, $2::uuid, $3, $4, $5)",
            )
            .bind(crate::sync::store::new_uuid_text())
            .bind(&account_id)
            .bind("PG Friend")
            .bind(serde_json::json!(["pg-friend@example.com"]))
            .bind(&photo_path)
            .execute(pool)
            .await
            .unwrap();

            // Case-insensitive address match through the handler's lookup.
            let resp = contact_photo_response(&db, &data_dir, &user_id, "PG-Friend@Example.COM")
                .await
                .unwrap()
                .expect("contact photo resolves");
            assert_eq!(resp.status(), StatusCode::OK);
            let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap();
            assert_eq!(body.as_ref(), MOCK_PNG);

            // Unknown sender misses cleanly.
            assert!(
                contact_photo_response(&db, &data_dir, &user_id, "stranger@example.com")
                    .await
                    .unwrap()
                    .is_none()
            );

            let _ = std::fs::remove_dir_all(&data_dir);
        });
    }
}
