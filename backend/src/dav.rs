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
pub struct DavClient {
    http: reqwest::Client,
    username: String,
    password: String,
}

impl DavClient {
    /// Build a client with basic auth credentials.
    pub fn new(username: String, password: String) -> Self {
        Self {
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::limited(10))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            username,
            password,
        }
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
        let body = r#"<?xml version="1.0" encoding="utf-8" ?>
<d:propfind xmlns:d="DAV:">
  <d:prop><d:displayname/><d:resourcetype/></d:prop>
</d:propfind>"#;

        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, self.auth_header());
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/xml; charset=utf-8"));
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
pub fn parse_vcard_fields(vcard: &str) -> (Option<String>, Vec<String>, Vec<String>, Option<String>) {
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
