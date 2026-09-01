//! Minimal CardDAV / CalDAV helpers (generic HTTP + simple parsers).
//!
//! Not a full WebDAV client — enough for v1 sync of address books and calendars
//! when the account stores `carddav_url` / `caldav_url`.

#![allow(clippy::doc_markdown)]

use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use thiserror::Error;

/// DAV / HTTP errors.
#[derive(Debug, Error)]
pub enum DavError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("protocol error: {0}")]
    Protocol(String),
}

/// Authenticated HTTP client for DAV endpoints.
///
/// Credentials are pinned to the origin of the configured base URL: hrefs
/// pointing at a different origin are rejected before any request is made,
/// so a malicious server cannot exfiltrate the account password by
/// returning `<d:href>https://evil.example/x.vcf</d:href>`.
pub struct DavClient {
    http: reqwest::Client,
    username: String,
    password: String,
    origin: String,
}

impl DavClient {
    /// Build a client with basic auth credentials pinned to `base_url`'s origin.
    pub fn new(username: String, password: String, base_url: &str) -> Result<Self, DavError> {
        crate::netsec::validate_server_url(base_url).map_err(DavError::Protocol)?;
        let origin = crate::netsec::origin_of(base_url).map_err(DavError::Protocol)?;
        let http = reqwest::Client::builder()
            // Never follow redirects: a redirect could carry the request
            // to a different host, and we must never replay credentials
            // cross-origin. Redirect responses surface as errors instead.
            .redirect(reqwest::redirect::Policy::none())
            // No fallback: a silent default client would follow redirects.
            .build()
            .map_err(DavError::Http)?;
        Ok(Self {
            http,
            username,
            password,
            origin,
        })
    }

    /// Refuse to send credentials to any origin other than the configured one.
    fn check_origin(&self, url: &str) -> Result<(), DavError> {
        let target = crate::netsec::origin_of(url).map_err(DavError::Protocol)?;
        if target != self.origin {
            tracing::warn!(
                target_origin = %target,
                expected_origin = %self.origin,
                "DAV: refusing cross-origin request (credentials stay pinned)"
            );
            return Err(DavError::Protocol(format!(
                "cross-origin DAV URL rejected: {url}"
            )));
        }
        Ok(())
    }

    fn auth_header(&self) -> HeaderValue {
        use base64::Engine;
        let token = base64::engine::general_purpose::STANDARD
            .encode(format!("{}:{}", self.username, self.password));
        HeaderValue::from_str(&format!("Basic {token}"))
            .unwrap_or_else(|_| HeaderValue::from_static("Basic"))
    }

    /// PROPFIND Depth 1 — returns hrefs found in the multistatus body.
    pub async fn propfind_hrefs(&self, url: &str) -> Result<Vec<String>, DavError> {
        self.check_origin(url)?;
        let body = r#"<?xml version="1.0" encoding="utf-8" ?>
<d:propfind xmlns:d="DAV:">
  <d:prop><d:displayname/><d:resourcetype/></d:prop>
</d:propfind>"#;

        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, self.auth_header());
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/xml; charset=utf-8"),
        );
        headers.insert("Depth", HeaderValue::from_static("1"));

        let res = self
            .http
            .request(reqwest::Method::from_bytes(b"PROPFIND").unwrap(), url)
            .headers(headers)
            .body(body)
            .send()
            .await?;

        if !res.status().is_success() && res.status().as_u16() != 207 {
            return Err(DavError::Protocol(format!(
                "PROPFIND {} → {}",
                url,
                res.status()
            )));
        }

        let text = res.text().await?;
        Ok(extract_hrefs(&text))
    }

    /// GET a resource as bytes/text.
    pub async fn get_text(&self, url: &str) -> Result<String, DavError> {
        self.check_origin(url)?;
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, self.auth_header());
        let res = self.http.get(url).headers(headers).send().await?;
        if !res.status().is_success() {
            return Err(DavError::Protocol(format!("GET {url} → {}", res.status())));
        }
        Ok(res.text().await?)
    }
}

fn extract_hrefs(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let lower = xml.to_lowercase();
    let mut search_from = 0;
    while let Some(start) = lower[search_from..].find("<d:href>") {
        let abs = search_from + start + "<d:href>".len();
        if let Some(end_rel) = lower[abs..].find("</d:href>") {
            let href = xml[abs..abs + end_rel].trim().to_string();
            if !href.is_empty() {
                out.push(href);
            }
            search_from = abs + end_rel;
        } else {
            break;
        }
    }
    // Also match unprefixed <href>
    search_from = 0;
    while let Some(start) = lower[search_from..].find("<href>") {
        let abs = search_from + start + "<href>".len();
        if let Some(end_rel) = lower[abs..].find("</href>") {
            let href = xml[abs..abs + end_rel].trim().to_string();
            if !href.is_empty() && !out.contains(&href) {
                out.push(href);
            }
            search_from = abs + end_rel;
        } else {
            break;
        }
    }
    out
}

/// Resolve a possibly-relative href against a base URL.
pub fn resolve_href(base: &str, href: &str) -> String {
    if href.starts_with("http://") || href.starts_with("https://") {
        return href.to_string();
    }
    if let Ok(base_url) = reqwest::Url::parse(base)
        && let Ok(joined) = base_url.join(href)
    {
        return joined.to_string();
    }
    href.to_string()
}

/// Very small vCard 3.0/4.0 field extractor.
pub fn parse_vcard_fields(
    vcard: &str,
) -> (Option<String>, Vec<String>, Vec<String>, Option<String>) {
    let mut display_name = None;
    let mut emails = Vec::new();
    let mut phones = Vec::new();
    let mut org = None;

    for raw_line in vcard.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((name_part, value)) = line.split_once(':') else {
            continue;
        };
        let prop = name_part.split(';').next().unwrap_or("").to_uppercase();
        let value = value.trim();
        match prop.as_str() {
            "FN" => display_name = Some(value.to_string()),
            "EMAIL" => emails.push(value.to_string()),
            "TEL" => phones.push(value.to_string()),
            "ORG" => org = Some(value.split(';').next().unwrap_or(value).to_string()),
            _ => {}
        }
    }

    (display_name, emails, phones, org)
}

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
            // Strip all leading WSP: RFC 6350 removes exactly one char, but
            // servers commonly indent with more; a stray space corrupts URLs.
            prev.push_str(raw.trim_start());
            continue;
        }
        lines.push(raw.to_string());
    }
    for line in lines {
        let Some(colon) = line.find(':') else {
            continue;
        };
        let (name_part, value) = (&line[..colon], line[colon + 1..].trim());
        let mut segs = name_part.split(';');
        if !segs.next().is_some_and(|n| n.eq_ignore_ascii_case("photo")) {
            continue;
        }
        let params: Vec<String> = segs.map(str::to_ascii_uppercase).collect();
        if params.iter().any(|p| p == "VALUE=URI") {
            return Some(VcardPhoto::Uri(value.to_string()));
        }
        if params
            .iter()
            .any(|p| p == "ENCODING=B" || p == "ENCODING=BASE64")
        {
            use base64::Engine;
            let clean: String = value.chars().filter(|c| !c.is_whitespace()).collect();
            if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(clean) {
                return Some(VcardPhoto::Inline(bytes));
            }
        }
    }
    None
}

type VEventFields = (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    bool,
);

/// Extract a few iCalendar properties from a VEVENT blob.
pub fn parse_vevent_fields(ical: &str) -> VEventFields {
    let mut summary = None;
    let mut description = None;
    let mut dtstart = None;
    let mut dtend = None;
    let mut location = None;
    let mut is_all_day = false;

    let mut in_event = false;
    for raw_line in ical.lines() {
        let line = raw_line.trim();
        if line.eq_ignore_ascii_case("BEGIN:VEVENT") {
            in_event = true;
            continue;
        }
        if line.eq_ignore_ascii_case("END:VEVENT") {
            break;
        }
        if !in_event {
            continue;
        }
        let Some((name_part, value)) = line.split_once(':') else {
            continue;
        };
        let prop = name_part.split(';').next().unwrap_or("").to_uppercase();
        let value = value.trim();
        match prop.as_str() {
            "SUMMARY" => summary = Some(value.to_string()),
            "DESCRIPTION" => description = Some(value.to_string()),
            "DTSTART" => {
                is_all_day = !name_part.to_uppercase().contains("VALUE=DATE-TIME")
                    && (name_part.to_uppercase().contains("VALUE=DATE") || value.len() == 8);
                dtstart = Some(value.to_string());
            }
            "DTEND" => dtend = Some(value.to_string()),
            "LOCATION" => location = Some(value.to_string()),
            _ => {}
        }
    }

    (summary, description, dtstart, dtend, location, is_all_day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_construction_validates_url() {
        assert!(DavClient::new("u".into(), "p".into(), "not a url").is_err());
        assert!(DavClient::new("u".into(), "p".into(), "ftp://x/").is_err());
        // http to a public host is rejected; https or LAN http is fine.
        assert!(DavClient::new("u".into(), "p".into(), "http://dav.example.com/").is_err());
        assert!(DavClient::new("u".into(), "p".into(), "https://dav.example.com/").is_ok());
        assert!(DavClient::new("u".into(), "p".into(), "http://192.168.1.10/").is_ok());
    }

    #[test]
    fn check_origin_same_origin_ok() {
        let client =
            DavClient::new("u".into(), "p".into(), "https://dav.example.com/book/").unwrap();
        assert!(
            client
                .check_origin("https://dav.example.com/book/alice.vcf")
                .is_ok()
        );
        assert!(
            client
                .check_origin("https://dav.example.com:443/other")
                .is_ok()
        );
    }

    #[test]
    fn check_origin_cross_origin_rejected() {
        let client =
            DavClient::new("u".into(), "p".into(), "https://dav.example.com/book/").unwrap();
        // A malicious server returning an absolute href to another host
        // must never receive our credentials.
        assert!(client.check_origin("https://evil.example/x.vcf").is_err());
        assert!(client.check_origin("http://dav.example.com/x.vcf").is_err());
        assert!(
            client
                .check_origin("https://dav.example.com:8443/x")
                .is_err()
        );
        assert!(client.check_origin("not a url").is_err());
    }

    #[test]
    fn resolve_then_check_pins_relative_hrefs() {
        let base = "https://dav.example.com/book/";
        let client = DavClient::new("u".into(), "p".into(), base).unwrap();
        let url = resolve_href(base, "alice.vcf");
        assert!(client.check_origin(&url).is_ok());
        let url = resolve_href(base, "https://evil.example/x.vcf");
        assert!(client.check_origin(&url).is_err());
    }

    #[test]
    fn extract_hrefs_basic() {
        let xml = r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:">
          <d:response><d:href>/book/alice.vcf</d:href></d:response>
          <d:response><d:href>/book/bob.vcf</d:href></d:response>
        </d:multistatus>"#;
        let hrefs = extract_hrefs(xml);
        assert_eq!(hrefs.len(), 2);
        assert!(hrefs[0].contains("alice.vcf"));
    }

    #[test]
    fn parse_vcard_fn_email() {
        let vcard = "BEGIN:VCARD\nFN:Ada Lovelace\nEMAIL:ada@example.com\nTEL:+1-555\nORG:Analytical;Eng\nEND:VCARD\n";
        let (fn_, emails, phones, org) = parse_vcard_fields(vcard);
        assert_eq!(fn_.as_deref(), Some("Ada Lovelace"));
        assert_eq!(emails, vec!["ada@example.com"]);
        assert_eq!(phones, vec!["+1-555"]);
        assert_eq!(org.as_deref(), Some("Analytical"));
    }

    #[test]
    fn photo_uri_form_extracts_url() {
        let vcard =
            "BEGIN:VCARD\r\nFN:Ada\r\nPHOTO;VALUE=URI:https://example.com/a.jpg\r\nEND:VCARD\r\n";
        assert_eq!(
            parse_vcard_photo(vcard),
            Some(VcardPhoto::Uri("https://example.com/a.jpg".into()))
        );
    }

    #[test]
    fn photo_inline_base64_extracts_bytes() {
        // 1x1 PNG, base64
        let png_b64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";
        let vcard = format!("BEGIN:VCARD\r\nPHOTO;ENCODING=b;TYPE=PNG:{png_b64}\r\nEND:VCARD\r\n");
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
            Some(VcardPhoto::Uri(
                "https://example.com/very/long/photo.png".into()
            ))
        );
    }

    #[test]
    fn no_photo_returns_none() {
        assert_eq!(
            parse_vcard_photo("BEGIN:VCARD\r\nFN:Ada\r\nEND:VCARD\r\n"),
            None
        );
    }

    #[test]
    fn parse_vevent_summary() {
        let ical = "BEGIN:VCALENDAR\nBEGIN:VEVENT\nSUMMARY:Ship\nDTSTART;VALUE=DATE:20260821\nDTEND;VALUE=DATE:20260822\nLOCATION:HQ\nEND:VEVENT\nEND:VCALENDAR\n";
        let (summary, _, start, end, loc, all_day) = parse_vevent_fields(ical);
        assert_eq!(summary.as_deref(), Some("Ship"));
        assert_eq!(start.as_deref(), Some("20260821"));
        assert_eq!(end.as_deref(), Some("20260822"));
        assert_eq!(loc.as_deref(), Some("HQ"));
        assert!(all_day);
    }
}
