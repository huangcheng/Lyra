# Sender Avatars Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Real sender avatars (contacts → VMC-validated BIMI → opt-in Gravatar) via one backend resolver endpoint, replacing monogram initials where a photo exists.

**Architecture:** New `backend/src/avatars.rs` deep module serving `GET /api/v1/avatars/{email}` (bearer auth; frontend fetches via the existing `apiBlob` pattern — no HMAC needed). All upstream fetches reuse the `media.rs` pipeline (SSRF guard, caps, sniffing). Contact photos come from new CardDAV vCard `PHOTO` extraction into the blob store. BIMI requires DMARC `p=quarantine|reject` + a VMC chain validated against embedded Mark Verifying Authority roots with CRL revocation checks. Gravatar is gated on a new `gravatarAvatars` privacy setting (default off).

**Tech Stack:** Rust + Axum backend; new deps `rustls-webpki` (chain validation), `x509-parser` (extension + CRL parsing), `md-5` (Gravatar hash); React frontend.

**Spec:** `docs/superpowers/specs/2026-09-01-sender-avatars-design.md` (read first).

**Key facts established by codebase recon (trust these):**

- `media.rs`: `validate_outbound_url(url)` (:209), `fetch_upstream(url) -> Result<FetchedImage, SyncError>` (:245, private struct `{bytes, content_type}`), `cache_key_for_url` = sha256 hex (:102), `cache_file_path` sharding (:107), `read_cache`/`write_cache` (:325/:338, sidecar `.meta` JSON `contentType`), `image_response(bytes, ct, cached)` (:412) — all private; widen to `pub(crate)` as needed. `looks_like_image` (:167) requires raster magic bytes — avatars need an SVG path too.
- `privacy.rs`: `PrivacySettings` (:30) + `PrivacySettingsResponse` (:52) + `PatchPrivacyRequest` (:59); kv key `user:{uid}:privacy`, `load_settings` (:81, `pub`); PATCH validation at :381. `sender_email_from_json` (:111, `pub`).
- `pim.rs`: contact sync `sync_contacts` (:568); vCard text fetched per href (:620-623); UPDATE column list :638-655, INSERT :658-683 — `PhotoPath` never written. `dav.rs:174` `parse_vcard_fields` is hand-rolled (no unfolding, no params).
- `blobs/mod.rs`: `store(data_dir, account_id, bytes) -> Result<String>` (:44, content-addressed, returns relative path), `read(data_dir, storage_path)` (:62).
- Router: modules expose `pub fn routes() -> Router<AuthState>`; merged in `main.rs:139-164` `api_router`. Auth: `AuthUser(pub String)` extractor (bearer). `AuthState` fields: `db`, `data_dir`, `sessions`, `app`; `kv()` accessor.
- `dkim.rs`: `authenticator()` (:164) is private — widen to `pub(crate)` for BIMI/DMARC DNS. mail-auth: `txt_raw_lookup(fqdn) -> Result<Vec<u8>>` (concatenated TXT); `txt_lookup::<Dmarc>` has a cfg(test) short-circuit inside mail-auth — fine in prod builds.
- kv interface: `get`/`set` only (no enumeration) — negative-cache keys must be self-describing (see Task 4 note).
- No direct X.509 crates; transitive: `rustls-webpki 0.103.14`, `aws-lc-rs 1.18`, `rustls-pki-types 1`.
- Frontend: `apiBlob(path) -> Promise<Blob>` (`api-client.ts:137`); avatar sites: `message-card.tsx` collapsed (:231) + expanded (:259, `AvatarImage` with no `src`), `mail-list.tsx:355`, `contacts-page.tsx` (:105/:128 hand-rolled monograms, `photoPath` already in the `Contact` interface :25). Privacy UI: `settings-page.tsx` privacy section (:1233), `privacy-api.ts` (`updatePrivacySettings` accepts only `remoteImages` — extend).
- OpenAPI: `docs/openapi/api-v1.yaml` documents `/api/v1/settings/privacy` (:1431); keep the contract current (repo convention).

---

### Task 1: Visibility plumbing + `gravatarAvatars` privacy setting

**Files:**
- Modify: `backend/src/media.rs`, `backend/src/dkim.rs`, `backend/src/privacy.rs`
- Modify: `docs/openapi/api-v1.yaml` (`PrivacySettings` schema)
- Test: colocated in `privacy.rs`

- [ ] **Step 1: Failing test**

In `backend/src/privacy.rs` `mod tests`, add:

```rust
    #[test]
    fn gravatar_avatars_defaults_off_and_roundtrips() {
        let parsed: PrivacySettings = serde_json::from_str("{}").unwrap();
        assert!(!parsed.gravatar_avatars);
        let on: PrivacySettings =
            serde_json::from_str(r#"{"gravatarAvatars":true}"#).unwrap();
        assert!(on.gravatar_avatars);
        let back = serde_json::to_value(&on).unwrap();
        assert_eq!(back["gravatarAvatars"], serde_json::json!(true));
    }
```

Run: `cd backend && cargo test --bin lyra_backend gravatar`
Expected: FAIL (field doesn't exist).

- [ ] **Step 2: Implement**

`privacy.rs` — add to `PrivacySettings` (:30):

```rust
    /// Opt-in: allow Gravatar lookups for sender avatars (default off —
    /// Gravatar learns hashed correspondent addresses per lookup).
    #[serde(default)]
    pub gravatar_avatars: bool,
```

Mirror the field into `PrivacySettingsResponse` and `PatchPrivacyRequest` (`gravatar_avatars: Option<bool>`), map it in the GET response builder and PATCH apply path (follow the `remote_images` handling at :364-395; bools need no enum validation).

- [ ] **Step 3: Visibility widening**

- `media.rs`: make `pub(crate)` — `validate_outbound_url`, `fetch_upstream`, `FetchedImage` (and its fields), `cache_key_for_url`, `cache_file_path`, `read_cache`, `write_cache`, `image_response`, `looks_like_image`. Do NOT change any logic.
- `dkim.rs`: make `authenticator()` `pub(crate)`.

If clippy's `-D warnings` flags now-unreachable privacy, keep items as-is; `pub(crate)` on used-elsewhere items is not flagged.

- [ ] **Step 4: Test + OpenAPI + commit**

Run: `cd backend && cargo test --bin lyra_backend privacy && cargo clippy --all-targets --all-features -- -D warnings`
Expected: PASS.

In `docs/openapi/api-v1.yaml` `PrivacySettings` schema (:1431 area), add `gravatarAvatars: boolean` property (default false) mirroring the existing fields.

```bash
git add backend/src/media.rs backend/src/dkim.rs backend/src/privacy.rs docs/openapi/api-v1.yaml
git commit -m "feat(backend): gravatarAvatars privacy setting + widen avatar seams"
```

---

### Task 2: CardDAV PHOTO extraction

**Files:**
- Modify: `backend/src/dav.rs` (parser)
- Modify: `backend/src/pim.rs` (sync hook)
- Test: colocated in `dav.rs` and `pim.rs`

- [ ] **Step 1: Failing parser tests**

In `backend/src/dav.rs` `mod tests`:

```rust
    #[test]
    fn photo_uri_form_extracts_url() {
        let vcard = "BEGIN:VCARD\r\nFN:Ada\r\nPHOTO;VALUE=URI:https://example.com/a.jpg\r\nEND:VCARD\r\n";
        assert_eq!(
            parse_vcard_photo(vcard),
            Some(VcardPhoto::Uri("https://example.com/a.jpg".into()))
        );
    }

    #[test]
    fn photo_inline_base64_extracts_bytes() {
        // 1x1 PNG, base64
        let png_b64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";
        let vcard = format!(
            "BEGIN:VCARD\r\nPHOTO;ENCODING=b;TYPE=PNG:{png_b64}\r\nEND:VCARD\r\n"
        );
        match parse_vcard_photo(&vcard) {
            Some(VcardPhoto::Inline(bytes)) => assert_eq!(bytes[..4], [0x89, 0x50, 0x4E, 0x47]),
            other => panic!("expected inline photo, got {other:?}"),
        }
    }

    #[test]
    fn photo_folded_line_unfolds() {
        // RFC 6350: continuation lines start with a space.
        let vcard = "BEGIN:VCARD\r\nPHOTO;VALUE=URI:https://example.com/very/\r\n  long/photo.png\r\nEND:VCARD\r\n";
        assert_eq!(
            parse_vcard_photo(vcard),
            Some(VcardPhoto::Uri("https://example.com/very/long/photo.png".into()))
        );
    }

    #[test]
    fn no_photo_returns_none() {
        assert_eq!(parse_vcard_photo("BEGIN:VCARD\r\nFN:Ada\r\nEND:VCARD\r\n"), None);
    }
```

Run: `cd backend && cargo test --bin lyra_backend photo`
Expected: FAIL.

- [ ] **Step 2: Parser in `dav.rs`**

Add (next to `parse_vcard_fields`, matching its hand-rolled style):

```rust
/// Extracted vCard PHOTO property (RFC 6350 §6.2.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum VcardPhoto {
    Uri(String),
    Inline(Vec<u8>),
}

/// Parse the first PHOTO property. Handles RFC 6350 line folding, the
/// `VALUE=URI` parameter form, and the inline `ENCODING=b` base64 form.
/// Garbage yields `None` — a bad photo must never fail contact sync.
pub(crate) fn parse_vcard_photo(vcard: &str) -> Option<VcardPhoto> {
    // Unfold: a line starting with SP/HTAB continues the previous line.
    let mut lines: Vec<String> = Vec::new();
    for raw in vcard.split("\r\n").flat_map(|l| l.split('\n')) {
        if raw.starts_with([' ', '\t'])
            && let Some(prev) = lines.last_mut()
        {
            prev.push_str(&raw[1..]);
            continue;
        }
        lines.push(raw.to_string());
    }
    for line in lines {
        let Some(colon) = line.find(':') else { continue };
        let (name_part, value) = (&line[..colon], line[colon + 1..].trim());
        let mut segs = name_part.split(';');
        if !segs.next().is_some_and(|n| n.eq_ignore_ascii_case("photo")) {
            continue;
        }
        let params: Vec<String> = segs.map(|s| s.to_ascii_uppercase()).collect();
        if params.iter().any(|p| p == "VALUE=URI") {
            return Some(VcardPhoto::Uri(value.to_string()));
        }
        if params.iter().any(|p| p == "ENCODING=b" || p == "ENCODING=BASE64") {
            use base64::Engine;
            let clean: String = value.chars().filter(|c| !c.is_whitespace()).collect();
            if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(clean) {
                return Some(VcardPhoto::Inline(bytes));
            }
        }
    }
    None
}
```

(`base64` is already a dependency — confirm and use the same import idiom as existing code.)

- [ ] **Step 3: Sync hook in `pim.rs`**

In `sync_contacts`, after `parse_vcard_fields` (:623):

```rust
            let photo_path = match dav::parse_vcard_photo(&vcard_text) {
                Some(dav::VcardPhoto::Inline(bytes)) => {
                    crate::blobs::store(&state.data_dir, &account_id, &bytes)
                        .await
                        .ok()
                }
                Some(dav::VcardPhoto::Uri(url)) => {
                    match crate::media::fetch_upstream(&url).await {
                        Ok(img) => {
                            crate::blobs::store(&state.data_dir, &account_id, &img.bytes)
                                .await
                                .ok()
                        }
                        Err(_) => None, // bad photo must not fail sync
                    }
                }
                None => None,
            };
```

(Variable names: match the actual locals at pim.rs:620-623 — `vcard_text`/`account_id` may differ; read the function first. `state.data_dir`: check what `sync_contacts` receives — if it lacks `data_dir`, widen its signature from the handler at pim.rs:568; the handler has `State(state): State<AuthState>`.)

Add `contact::Column::PhotoPath` to BOTH the UPDATE (:638) and INSERT (:658) column lists with `photo_path` as the value (use the file's existing `Option<String>` value idiom). Note for UPDATE: only overwrite when `photo_path.is_some()` — use a `COALESCE`-style guard matching how the file handles optional refreshes, or simplest: include the column only when `Some` (two UPDATE shapes). Pick the simpler one that never wipes an existing photo with NULL.

- [ ] **Step 4: Sync-path test**

In `pim.rs` tests: unit-test the extraction→store glue is hard without a DAV server; instead test the column-write decision as a pure helper if you extracted one, and rely on Task 3's resolver test for contact-photo serving. Keep it honest: if no clean seam exists, test only the parser (Task 2 Step 1) and say so.

Run: `cargo test --bin lyra_backend` + clippy.
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add backend/src/dav.rs backend/src/pim.rs
git commit -m "feat(backend): extract vCard PHOTO into blob store during contact sync"
```

---

### Task 3: `avatars.rs` — endpoint, contact photo, Gravatar, caching

**Files:**
- Create: `backend/src/avatars.rs`
- Modify: `backend/src/main.rs` (module + route merge)
- Test: colocated in `avatars.rs`

Design notes (controller, binding):
- Endpoint: `GET /api/v1/avatars/{email}` with `AuthUser` bearer (frontend uses `apiBlob`; NOT `<img>`-safe, intentionally).
- Positive cache: `media-cache` under key `cache_key_for_url(&format!("avatar:{email}"))` — reuse `write_cache`/`read_cache` including the `.meta` content-type sidecar. Positive TTL check: read the meta sidecar; if the cache file's mtime is older than 7 days, refetch (use file mtime; if that's awkward, store `fetchedAt` in the meta JSON — extend `write_cache` minimally or write the sidecar yourself).
- Negative cache: kv, key `user:{uid}:avatar-miss:{g}:{sha256(email)}` where `{g}` is `0|1` = gravatar setting state, TTL 24h; error-misses TTL 10 min. (Self-describing keys because kv has no enumeration; toggling Gravatar on naturally bypasses old misses — satisfies the spec's toggle intent.)
- Contact photos are streamed from the blob store directly, not copied into media-cache.

- [ ] **Step 1: Failing tests**

Create `backend/src/avatars.rs` with the module skeleton + tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gravatar_url_hashes_lowercased_trimmed_email() {
        assert_eq!(
            gravatar_url("  HuangCheng@Example.COM "),
            "https://www.gravatar.com/avatar/93942e96f5acd0e96e47ad22e44e2b6c?d=404&s=128"
        );
    }

    #[test]
    fn bimi_record_parses_logo_and_authority() {
        let rec = parse_bimi_record(b"v=BIMI1; l=https://example.com/logo.svg; a=https://example.com/vmc.pem");
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
        assert_eq!(parse_bimi_record(b"v=BIMI1; a=https://x.test/vmc.pem"), None);
    }

    #[test]
    fn dmarc_policy_gate() {
        assert!(dmarc_allows_bimi("v=DMARC1; p=reject;"));
        assert!(dmarc_allows_bimi("v=DMARC1; p=quarantine;"));
        assert!(!dmarc_allows_bimi("v=DMARC1; p=none;"));
        assert!(!dmarc_allows_bimi("garbage"));
    }
}
```

(`gravatar_url` hash above is md5 of "huangcheng@example.com" — verify it with `echo -n huangcheng@example.com | md5` before finalizing the test; fix the expected value if it differs.)

Run: `cd backend && cargo test --bin lyra_backend avatars`
Expected: FAIL.

- [ ] **Step 2: Implement the module**

New dep in `backend/Cargo.toml`: `md-5 = "0.10"` (RustCrypto, consistent with existing `sha2`/`hmac`).

Module structure (write it fully; helpers below are complete where the logic is self-contained):

```rust
//! Sender avatar resolution: contact photo → BIMI (VMC-validated) →
//! opt-in Gravatar. One endpoint hides the chain; every upstream fetch goes
//! through the media pipeline (SSRF guard, caps, sniffing), so no third
//! party sees the user's IP and Gravatar sees nothing unless opted in.

use axum::{
    extract::{Path, State},
    response::Response,
    routing::get,
    Router,
};

use crate::auth::{AuthState, AuthUser};
use crate::sync::SyncError;

const POSITIVE_TTL: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 3600);
const NEGATIVE_TTL_SECS: u64 = 24 * 3600;
const ERROR_TTL_SECS: u64 = 600;

pub(crate) struct BimiRecord {
    logo_url: String,
    authority_url: Option<String>,
}

/// md5 hex of the trimmed, lowercased address (Gravatar's contract).
pub(crate) fn gravatar_url(email: &str) -> String {
    let digest = md5::compute(email.trim().to_ascii_lowercase().as_bytes());
    format!("https://www.gravatar.com/avatar/{digest:x}?d=404&s=128")
}

/// Parse a `default._bimi` TXT payload: `v=BIMI1; l=<logo>; a=<authority>`.
pub(crate) fn parse_bimi_record(txt: &[u8]) -> Option<BimiRecord> {
    let txt = std::str::from_utf8(txt).ok()?;
    if !txt.split(';').next()?.trim().eq_ignore_ascii_case("v=BIMI1") {
        return None;
    }
    let mut logo_url = None;
    let mut authority_url = None;
    for part in txt.split(';').skip(1) {
        let part = part.trim();
        if let Some(v) = part.strip_prefix("l=").or_else(|| part.strip_prefix("L=")) {
            logo_url = Some(v.trim().to_string());
        } else if let Some(v) = part.strip_prefix("a=").or_else(|| part.strip_prefix("A=")) {
            authority_url = Some(v.trim().to_string());
        }
    }
    logo_url.map(|logo_url| BimiRecord { logo_url, authority_url })
}

/// BIMI requires DMARC enforcement on the From domain (client-side gate:
/// policy record only — no alignment evaluation).
pub(crate) fn dmarc_allows_bimi(txt: &str) -> bool {
    let txt = txt.trim();
    if !txt.to_ascii_lowercase().starts_with("v=dmarc1") {
        return false;
    }
    txt.split(';').skip(1).any(|part| {
        let p = part.trim().to_ascii_lowercase();
        p == "p=quarantine" || p == "reject" || p.starts_with("p=quarantine") || p.starts_with("p=reject")
    })
}
```

Wait — cleaner: `p.trim()` then match on `strip_prefix("p=")` value ∈ {"quarantine","reject"}. Write it that way.

Endpoint handler (complete):

```rust
pub fn routes() -> Router<AuthState> {
    Router::new().route("/api/v1/avatars/{email}", get(get_avatar))
}

async fn get_avatar(
    State(state): State<AuthState>,
    AuthUser(user_id): AuthUser,
    Path(email): Path<String>,
) -> Result<Response, SyncError> {
    let email = email.trim().to_ascii_lowercase();
    // … chain: contact photo → positive cache → negative cache check →
    //   BIMI (Task 4) → Gravatar (setting-gated) → write cache / 404
}
```

Chain logic (write completely, following this shape):
1. Contact: query `contact` rows for the user (join account for user_id) whose `email_addresses` JSON contains the address — see `pim.rs` list_contacts for the query idiom; on hit with `photo_path`, `blobs::read` + respond with sniffed content type (reuse `looks_like_image`; for contact photos stored by us, trust the stored bytes but still sniff).
2. Positive media-cache hit, fresh (< `POSITIVE_TTL`) → respond.
3. Negative kv hit → 404.
4. BIMI via Task 4's `resolve_bimi_logo(state, domain)`.
5. Gravatar: only when `privacy::load_settings(state.kv(), &user_id).await.gravatar_avatars` — `fetch_upstream(&gravatar_url(&email))`; Gravatar 404 = miss (fetch_upstream errors on non-image/404 — treat as miss).
6. Hit → `write_cache` + respond; miss → negative-cache (ERROR_TTL when the failure was a fetch/DNS error, NEGATIVE_TTL when sources cleanly had nothing) + 404.
7. Response: bytes + content-type, `Cache-Control: private, max-age=86400` (build via `image_response` if it fits, else a small local response builder).

`main.rs`: `mod avatars;` + `.merge(avatars::routes())` in `api_router` after `media::routes()`.

- [ ] **Step 3: Tests pass + clippy**

Run: `cd backend && cargo test --bin lyra_backend avatars && cargo clippy --all-targets --all-features -- -D warnings`
Expected: PASS. (BIMI resolution is Task 4 — gate it behind a stub `resolve_bimi_logo` returning `None` for now, marked so Task 4 replaces it.)

- [ ] **Step 4: Commit**

```bash
git add backend/Cargo.toml backend/Cargo.lock backend/src/avatars.rs backend/src/main.rs
git commit -m "feat(backend): avatar resolver endpoint (contact photo + opt-in Gravatar)"
```

---

### Task 4: BIMI with full VMC validation

**Files:**
- Create: `backend/src/bimi.rs` (VMC validation — keep it out of avatars.rs)
- Modify: `backend/src/avatars.rs` (replace the Task 3 stub)
- Create: `backend/src/bimi/roots.rs` or PEM constants (embedded MVA roots)
- Modify: `backend/src/main.rs`, `backend/Cargo.toml`
- Test: colocated in `bimi.rs`

- [ ] **Step 1: New deps + MVA roots**

`backend/Cargo.toml`: add `rustls-webpki = "0.103"`, `x509-parser = "0.18"` (verify latest 0.103.x/0.18.x compatibility with the lockfile's existing versions — match what's already transitive where possible).

Embed Mark Verifying Authority roots as PEM constants. Obtain them from official sources and verify fingerprints:
- DigiCert: "DigiCert Verified Mark Root CA" (from https://www.digicert.com/kb/digicert-root-certificates.htm — the VMC root, NOT the TLS roots).
- Entrust: "Entrust Verified Mark Root CA" / VMC chain roots (from entrust.com VMC documentation).
Fetch with curl, verify SHA-256 fingerprints against the published values, and document source URL + fingerprint in a comment above each constant. If a root can't be obtained/verified, ship DigiCert-only and note it.

- [ ] **Step 2: Failing tests (validation policy, hermetic)**

Build a test chain with openssl (script the generation in the test setup or as a committed fixture under `backend/testdata/bimi/`): self-signed "Test MVA root" → leaf VMC for `example.com` with the logotype extension OID 1.3.6.1.5.5.7.1.12 (openssl x509v3 extension config), plus variants: expired leaf, leaf for `other.com`, chain anchored to a different root, leaf without the logotype OID. Tests:

```rust
    #[test]
    fn valid_chain_passes() { /* anchored at test root, domain match, OID present */ }

    #[test]
    fn wrong_domain_fails() { /* leaf SAN/subject for other.com */ }

    #[test]
    fn expired_cert_fails() {}

    #[test]
    fn untrusted_root_fails() {}

    #[test]
    fn missing_logotype_oid_fails() {}
```

(Generating the fixture chain with openssl in a `build`-free test util is fine; committing pre-generated PEM fixtures is also fine. Pick whichever is more reproducible and document it in the test file header.)

- [ ] **Step 3: `bimi.rs` validation module**

Public surface (all `pub(crate)`):

```rust
/// Validate a VMC evidence document (PEM bundle: leaf + intermediates)
/// for `domain`. Checks: chain to an embedded MVA root (webpki path
/// validation), validity window, domain binding (leaf SAN dNSName or
/// subject CN/O matches the From domain), logotype extension OID present,
/// CRL revocation (leaf + intermediates with cDP).
pub(crate) async fn validate_vmc(pem_bundle: &[u8], domain: &str) -> Result<(), VmcError>;
```

Implementation guidance (verify exact API names against the vendored `rustls-webpki 0.103` and `x509-parser` sources in `~/.cargo/registry` before writing):
- Parse PEM blocks with `x509_parser::pem::parse_x509_pem` iteratively.
- Path validation: `webpki::EndEntityCert::try_from(leaf_der)` → `verify_for_usage` with `TrustAnchor`s built from the embedded roots (`webpki:: anchor_from_trusted_cert`), `ALL_VERIFICATION_ALGS`, time now. No EKU requirement (VMCs don't carry serverAuth EKU — check; if webpki insists on an EKU, use the `KeyPurposeId` of anyEKU or document the workaround).
- Domain binding: parse leaf with x509-parser; check SAN dNSName entries, fall back to subject CN; require == domain or parent-domain match (BIMI allows the cert's domain to be the organizational domain).
- Logotype: find extension OID `1.3.6.1.5.5.7.1.12` in the leaf (x509-parser extension iteration). Presence is the gate; matching the l= URL inside the extension is a bonus (logotype extensions embed image URIs — implement only if trivially parseable, else note it).
- CRL: from leaf + intermediate cDP extension (`1.3.6.1.5.5.7.2.2`? no — cDP is `2.5.29.31`), fetch each CRL URL via `media::fetch_upstream` (it enforces image/*… — for CRLs use `validate_outbound_url` + a plain reqwest GET instead; reuse the timeout/UA constants), parse with x509-parser's CRL parser, reject if the leaf serial is listed. CRL fetch/parse failure = soft-fail: log + accept, cache nothing (short retry on next resolve). Document this.
- Negative validation results are NOT cached (cheap DNS-gated anyway).

`VmcError`: small enum (Chain, Expired, DomainMismatch, NoLogotype, Revoked, Malformed) with Display; log specifics server-side, never to the client.

- [ ] **Step 4: Wire BIMI into the avatar chain**

In `avatars.rs`, replace the Task 3 stub with `resolve_bimi_logo`:

```rust
/// BIMI logo for a From domain, or None. DMARC gate → record parse →
/// VMC validation → logo fetch. Every failure is a silent miss.
async fn resolve_bimi_logo(state: &AuthState, domain: &str) -> Option<crate::media::FetchedImage> {
    let auth = crate::dkim::authenticator().ok()?;
    // DMARC gate
    let dmarc_txt = auth.txt_raw_lookup(format!("_dmarc.{domain}")).await.ok()?;
    if !dmarc_allows_bimi(&String::from_utf8_lossy(&dmarc_txt)) {
        return None;
    }
    // BIMI record
    let bimi_txt = auth
        .txt_raw_lookup(format!("default._bimi.{domain}"))
        .await
        .ok()?;
    let record = parse_bimi_record(&bimi_txt)?;
    // VMC evidence (required per spec: full validation)
    let authority = record.authority_url?;
    let pem = fetch_text(&authority).await?; // small helper: validate_outbound_url + reqwest GET, 10s, 1 MiB cap
    crate::bimi::validate_vmc(pem.as_bytes(), domain).await.ok()?;
    // Logo fetch via the media pipeline; SVG accepted for BIMI.
    fetch_logo(&record.logo_url).await
}
```

`fetch_logo`: `validate_outbound_url` + `fetch_upstream`, but `looks_like_image` rejects SVG — extend minimally: if the response content-type is `image/svg+xml` AND the bytes start with `<` and contain `<svg`, accept as SVG (add a `looks_like_svg` helper in media.rs). Never serve unverified bytes.

- [ ] **Step 5: Tests + clippy + commit**

Run: `cd backend && cargo test --bin lyra_backend bimi && cargo test --bin lyra_backend avatars && cargo clippy --all-targets --all-features -- -D warnings`
Expected: PASS.

```bash
git add backend/Cargo.toml backend/Cargo.lock backend/src/bimi.rs backend/src/avatars.rs backend/src/main.rs backend/src/media.rs backend/testdata
git commit -m "feat(backend): BIMI avatars with VMC chain validation + CRL revocation"
```

---

### Task 5: Frontend wiring

**Files:**
- Create: `frontend/src/lib/avatar.ts`
- Test: `frontend/src/lib/avatar.test.ts`
- Modify: `frontend/src/components/mail/message-card.tsx`, `frontend/src/components/mail/mail-list.tsx`, `frontend/src/components/contacts-page.tsx`, `frontend/src/lib/privacy-api.ts`, `frontend/src/components/settings-page.tsx`, `frontend/src/i18n/en.json`, `frontend/src/i18n/zh.json`

- [ ] **Step 1: Failing test**

`frontend/src/lib/avatar.test.ts`:

```ts
import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@/lib/api-client', () => ({ apiBlob: vi.fn() }));

import { apiBlob } from '@/lib/api-client';
import { avatarState, loadAvatar, resetAvatarCacheForTests } from '@/lib/avatar';

const mockedApiBlob = vi.mocked(apiBlob);

beforeEach(() => {
  resetAvatarCacheForTests();
  mockedApiBlob.mockReset();
});

describe('loadAvatar', () => {
  it('returns an object URL for a hit and memoizes it', async () => {
    mockedApiBlob.mockResolvedValue(new Blob(['x'], { type: 'image/png' }));
    const first = await loadAvatar('a@example.com');
    const second = await loadAvatar('a@example.com');
    expect(first).not.toBeNull();
    expect(second).toBe(first);
    expect(mockedApiBlob).toHaveBeenCalledTimes(1);
    expect(mockedApiBlob).toHaveBeenCalledWith('/avatars/a%40example.com');
  });

  it('memoizes misses as null', async () => {
    mockedApiBlob.mockRejectedValue(new Error('404'));
    expect(await loadAvatar('b@example.com')).toBeNull();
    expect(await loadAvatar('b@example.com')).toBeNull();
    expect(mockedApiBlob).toHaveBeenCalledTimes(1);
  });

  it('exposes state for components without async work', async () => {
    mockedApiBlob.mockResolvedValue(new Blob(['x']));
    await loadAvatar('c@example.com');
    expect(avatarState('c@example.com')).not.toBeNull();
    expect(avatarState('d@example.com')).toBeUndefined();
  });
});
```

- [ ] **Step 2: `lib/avatar.ts`**

```ts
/**
 * Sender avatars via the backend resolver (`GET /api/v1/avatars/{email}`).
 * Authenticated binary → apiBlob (img src can't carry the bearer header,
 * see attachments.ts). Hits AND misses are memoized per session so lists
 * never refetch; object URLs are shared, not revoked per component.
 */

import { apiBlob } from '@/lib/api-client';

const cache = new Map<string, string | null>();

/** Synchronous read for render: undefined = unknown, null = no avatar. */
export function avatarState(email: string): string | null | undefined {
  return cache.get(email.trim().toLowerCase());
}

export async function loadAvatar(email: string): Promise<string | null> {
  const key = email.trim().toLowerCase();
  const known = cache.get(key);
  if (known !== undefined) return known;
  try {
    const blob = await apiBlob(`/avatars/${encodeURIComponent(key)}`);
    const url = URL.createObjectURL(blob);
    cache.set(key, url);
    return url;
  } catch {
    cache.set(key, null);
    return null;
  }
}

/** Test-only: clear the session cache. */
export function resetAvatarCacheForTests(): void {
  cache.clear();
}
```

- [ ] **Step 3: Hook + wire the three sites**

Add a tiny hook to `lib/avatar.ts`:

```ts
import { useEffect, useState } from 'react';

/** Component seam: current avatar URL for an address (null while loading/miss). */
export function useAvatar(email: string | undefined): string | null {
  const [url, setUrl] = useState<string | null>(() =>
    email ? (avatarState(email) ?? null) : null,
  );
  useEffect(() => {
    if (!email) return;
    let live = true;
    void loadAvatar(email).then((u) => {
      if (live && u) setUrl(u);
    });
    return () => {
      live = false;
    };
  }, [email]);
  return url;
}
```

Wire (each site: keep the existing monogram as fallback):

- `message-card.tsx` expanded header (:259): `const avatarUrl = useAvatar(mail.from.email);` then `<AvatarImage src={avatarUrl ?? undefined} alt={fromLabel} />`. Collapsed header (:231): add `<AvatarImage>` the same way.
- `mail-list.tsx:355`: import `AvatarImage` and wire with `useAvatar(item.from.email)`.
- `contacts-page.tsx`: replace the hand-rolled monogram spans (:105, :128) with the same pattern: `useAvatar(contact.emailAddresses[0])` — render an `<img>` with the monogram span as fallback (keep the existing styling/classes on both branches).

- [ ] **Step 4: Gravatar toggle in Settings → Privacy**

`frontend/src/lib/privacy-api.ts`: extend the `PrivacySettings` interface with `gravatarAvatars: boolean` and `updatePrivacySettings` to accept `{ remoteImages?: …; gravatarAvatars?: boolean }`.

`settings-page.tsx` privacy section: below the remote-images row, add a row with the existing `Switch` component (`components/ui/switch.tsx` exists — check how other toggles render in this file and mirror exactly), wired like `handleRemoteImagesModeChange` (guard → saving → `updatePrivacySettings({ gravatarAvatars: v })` → set state → error path).

i18n keys (en/zh):

```json
"gravatarAvatars": "Gravatar avatars",
"gravatarAvatarsHint": "Look up sender photos from Gravatar when no contact photo or brand logo exists. Gravatar sees a hash of each sender's address on lookup.",
```

zh:

```json
"gravatarAvatars": "Gravatar 头像",
"gravatarAvatarsHint": "当没有联系人照片或品牌标识时，从 Gravatar 查询发件人头像。查询时 Gravatar 会看到发件人地址的哈希值。",
```

- [ ] **Step 5: Verify + commit**

Run: `cd frontend && npm run check && npm test`
Expected: PASS.

```bash
git add frontend/src/lib/avatar.ts frontend/src/lib/avatar.test.ts frontend/src/components/mail/message-card.tsx frontend/src/components/mail/mail-list.tsx frontend/src/components/contacts-page.tsx frontend/src/lib/privacy-api.ts frontend/src/components/settings-page.tsx frontend/src/i18n/en.json frontend/src/i18n/zh.json
git commit -m "feat(frontend): sender avatars via backend resolver + Gravatar opt-in toggle"
```

---

### Task 6: Full verification

- [ ] **Step 1: `make fmt && make check`** — everything green; commit formatting deltas if any.
- [ ] **Step 2: Live smoke** — rebuild the dev stack (`docker compose up -d --build lyra`); open mail from a BIMI brand (e.g. LinkedIn/paypal if present in the mailbox) and a contact with a CardDAV photo; enable the Gravatar toggle in Settings → Privacy and reload a sender known to have one; confirm misses still show monograms; confirm no remote fetch happens from the browser (dev tools network tab — avatar requests go to `/api/v1/avatars/…` only).
- [ ] **Step 3: Commit any formatting deltas.**

---

## Self-review notes

- Spec coverage: endpoint + chain → Task 3; contact photos (extraction → Task 2, serving → Task 3); BIMI DNS + DMARC gate + VMC + CRL → Task 4; Gravatar opt-in → Tasks 1/3/5; negative/positive caching + TTLs → Task 3; frontend sites + toggle → Task 5; OpenAPI currency → Tasks 1 (+ detail payloads unchanged — avatars ride their own endpoint).
- Deliberate spec-wording deviation: negative entries are keyed with the Gravatar setting state instead of "cleared on toggle" (kv has no enumeration; equivalent outcome — a freshly enabled user is never stuck behind old misses).
- `dmarc_allows_bimi` in Task 3 Step 2's draft had a sloppy `starts_with` chain — implementers use the `strip_prefix("p=")` match described right below it.
- VMC validity-domain rule: leaf SAN dNSName match (equal or parent domain), not exact-only — matches how MVAs issue for organizational domains.
- Type consistency: `VcardPhoto::{Uri,Inline}`, `BimiRecord`, `gravatar_url`, `parse_bimi_record`, `dmarc_allows_bimi`, `validate_vmc`, `loadAvatar`/`avatarState`/`useAvatar`/`resetAvatarCacheForTests` used identically across tasks.
