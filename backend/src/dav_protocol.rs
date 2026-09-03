//! RFC-aligned DAV protocol layer for CardDAV (RFC 6352) and CalDAV
//! (RFC 4791), on top of the origin-pinned [`super::dav`] client.
//!
//! Covers what the v1 sync skipped: RFC 6764 discovery, RFC 5397
//! principal + homeset resolution, `sync-collection` REPORT (RFC 6578)
//! with invalid-token detection, multiget REPORTs, and etag two-way
//! writes. XML is matched by local tag name (servers disagree on
//! prefixes) and every extractor degrades to empty rather than erroring
//! on unknown structure.

use std::time::Duration;

use super::dav::{DavClient, DavError};

const DAV_TIMEOUT: Duration = Duration::from_secs(30);
const SYNC_REPORT_LIMIT: usize = 500;
const MULTIGET_CHUNK: usize = 50;
const XML_CONTENT_TYPE: &str = "application/xml";

#[derive(Debug, Default, Clone)]
pub struct DavItem {
    pub href: String,
    pub etag: Option<String>,
    pub data: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct SyncChanges {
    pub changed: Vec<DavItem>,
    pub removed: Vec<String>,
    pub token: Option<String>,
    pub invalid: bool,
}

// ── tolerant XML scanning ─────────────────────────────────────────────

/// `(start, end_after_gt)` of the next opening tag whose local name
/// matches; namespace prefix agnostic (`D:href`, `d:href`, `href`).
fn find_open(xml: &str, tag: &str) -> Option<(usize, usize)> {
    let bytes = xml.as_bytes();
    let mut from = 0;
    while let Some(lt) = xml[from..].find('<') {
        let start = from + lt;
        if bytes.get(start + 1) != Some(&b'/')
            && let Some(gt_rel) = xml[start..].find('>')
        {
            let gt = start + gt_rel;
            let name_full = &xml[start + 1..gt];
            let name = name_full
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_end_matches('/');
            let local = name.rsplit(':').next().unwrap_or(name);
            if local == tag {
                return Some((start, gt + 1));
            }
        }
        from = start + 1;
    }
    None
}

/// `(start, end_after_gt)` of the next closing tag with a matching local name.
fn find_close(xml: &str, tag: &str) -> Option<(usize, usize)> {
    let mut from = 0;
    while let Some(lt) = xml[from..].find("</") {
        let start = from + lt;
        if let Some(gt_rel) = xml[start..].find('>') {
            let gt = start + gt_rel;
            let local = xml[start + 2..gt].rsplit(':').next().unwrap_or("");
            if local == tag {
                return Some((start, gt + 1));
            }
        }
        from = start + 2;
    }
    None
}

/// `(start, len)` of the first complete element (nesting-aware).
fn element(xml: &str, tag: &str) -> Option<(usize, usize)> {
    let (open_start, after_open) = find_open(xml, tag)?;
    let mut pos = after_open;
    let mut depth = 1usize;
    while depth > 0 {
        let next_open = find_open(&xml[pos..], tag).map(|(s, e)| (pos + s, pos + e));
        let next_close = find_close(&xml[pos..], tag).map(|(s, e)| (pos + s, pos + e));
        match (next_close, next_open) {
            (Some((cs, ce)), Some((os, _))) if cs < os => {
                depth -= 1;
                if depth == 0 {
                    return Some((open_start, ce - open_start));
                }
                pos = ce;
            }
            (Some((_, ce)), None) => {
                depth -= 1;
                if depth == 0 {
                    return Some((open_start, ce - open_start));
                }
                pos = ce;
            }
            (_, Some((os, _))) => {
                // A nested same-name open comes before the next close:
                // push a level and continue scanning after it.
                depth += 1;
                pos = os + 1;
            }
            (None, None) => return None,
        }
    }
    None
}

/// Every complete `<response>` block.
fn response_blocks(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some((start, len)) = element(rest, "response") {
        out.push(rest[start..start + len].to_string());
        rest = &rest[start + len..];
    }
    out
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Text content of the first element (child tags stripped).
fn tag_text(xml: &str, tag: &str) -> Option<String> {
    let (start, len) = element(xml, tag)?;
    let block = &xml[start..start + len];
    let (_, after_open) = find_open(block, tag)?;
    let close = find_close(block, tag)?;
    let inner = &block[after_open..close.0];
    let stripped: String = strip_tags(inner);
    let trimmed = unescape(&stripped).trim().to_string();
    (!trimmed.is_empty()).then_some(trimmed)
}

/// Text content of every matching element.
#[allow(dead_code)]
fn all_tag_text(xml: &str, tag: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some((start, len)) = element(rest, tag) {
        if let Some(text) = tag_text(&rest[start..start + len], tag) {
            out.push(text);
        }
        rest = &rest[start + len..];
    }
    out
}

fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < s.len() {
        if bytes[i] == b'<' {
            if s[i..].starts_with("<![CDATA[") {
                // CDATA section: pass content through verbatim, strip markers.
                let content_start = i + "<![CDATA[".len();
                let content_end = s[content_start..]
                    .find("]]>")
                    .map_or(s.len(), |n| content_start + n);
                out.push_str(&s[content_start..content_end]);
                i = (content_end + 3).min(s.len());
            } else {
                // Regular tag: skip through its '>'.
                match s[i..].find('>') {
                    Some(gt) => i += gt + 1,
                    None => break,
                }
            }
        } else {
            let start = i;
            let end = s[start..].find('<').map_or(s.len(), |n| start + n);
            out.push_str(&s[start..end]);
            i = end;
        }
    }
    out
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn propfind_body(props: &str) -> String {
    format!(
        "<?xml version='1.0' encoding='utf-8'?><D:propfind xmlns:D='DAV:'><D:prop>{props}</D:prop></D:propfind>"
    )
}

// ── protocol ──────────────────────────────────────────────────────────

#[allow(dead_code)] // write API exercised in tests; UI CRUD follows
impl DavClient {
    /// RFC 6764 + RFC 5397 + homeset chain: well-known URL (following
    /// redirects) → current-user-principal → `addressbook-home-set` /
    /// `calendar-home-set`. `homeset_prop` is a local name like
    /// `addressbook-home-set`.
    pub async fn discover_homeset(
        &self,
        well_known_path: &str,
        homeset_prop: &str,
    ) -> Result<String, DavError> {
        let resp = self
            .http
            .get(format!("{}{}", self.origin, well_known_path))
            .timeout(DAV_TIMEOUT)
            .send()
            .await
            .map_err(DavError::Http)?;
        let mut base = resp.url().to_string();
        if let Some(body) = self
            .propfind_text(&base, "<D:current-user-principal/>")
            .await
            .ok()
            && let Some((start, len)) = element(&body, "current-user-principal")
            && let Some(principal) = tag_text(&body[start..start + len], "href")
        {
            base = super::dav::resolve_href(&base, &principal);
        }
        let ns = if homeset_prop.starts_with("calendar") {
            "urn:ietf:params:xml:ns:caldav"
        } else {
            "urn:ietf:params:xml:ns:carddav"
        };
        let prop = format!("<x:{homeset_prop} xmlns:x='{ns}'/>");
        let body = self.propfind_text(&base, &prop).await?;
        let href = tag_text(&body, homeset_prop)
            .or_else(|| tag_text(&body, "href"))
            .ok_or_else(|| DavError::Protocol(format!("no {homeset_prop} in response")))?;
        Ok(super::dav::resolve_href(&base, &href))
    }

    /// Principal → homeset starting from a known DAV root (no well-known
    /// chain): RFC 5397 current-user-principal, then the homeset property.
    pub async fn homeset_direct(&self, base: &str, homeset_prop: &str) -> Result<String, DavError> {
        let mut principal_base = base.to_string();
        if let Some(body) = self
            .propfind_text(base, "<D:current-user-principal/>")
            .await
            .ok()
            && let Some((start, len)) = element(&body, "current-user-principal")
            && let Some(principal) = tag_text(&body[start..start + len], "href")
        {
            principal_base = super::dav::resolve_href(base, &principal);
        }
        let ns = if homeset_prop.starts_with("calendar") {
            "urn:ietf:params:xml:ns:caldav"
        } else {
            "urn:ietf:params:xml:ns:carddav"
        };
        let prop = format!("<x:{homeset_prop} xmlns:x='{ns}'/>");
        let body = self.propfind_text(&principal_base, &prop).await?;
        let href = tag_text(&body, homeset_prop)
            .or_else(|| tag_text(&body, "href"))
            .ok_or_else(|| DavError::Protocol(format!("no {homeset_prop} in response")))?;
        Ok(super::dav::resolve_href(&principal_base, &href))
    }

    async fn propfind_text(&self, url: &str, props: &str) -> Result<String, DavError> {
        // RFC 6764 discovery chains redirect (302 to the DAV root); reqwest
        // does not follow redirects for PROPFIND, so follow Location
        // manually, bounded, staying within the client's origin pin.
        let mut current = url.to_string();
        for _ in 0..3 {
            let resp = self
                .http
                .request(reqwest::Method::from_bytes(b"PROPFIND").unwrap(), &current)
                .timeout(DAV_TIMEOUT)
                .header("Depth", "0")
                .header(reqwest::header::AUTHORIZATION, self.auth_header())
                .header(reqwest::header::CONTENT_TYPE, XML_CONTENT_TYPE)
                .body(propfind_body(props))
                .send()
                .await
                .map_err(DavError::Http)?;
            if resp.status().is_redirection() {
                let loc = resp
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| {
                        DavError::Protocol(format!("PROPFIND {current}: redirect without Location"))
                    })?;
                current = super::dav::resolve_href(&self.origin, loc);
                continue;
            }
            if !resp.status().is_success() {
                return Err(DavError::Protocol(format!(
                    "PROPFIND {current}: {}",
                    resp.status()
                )));
            }
            return Ok(resp.text().await.unwrap_or_default());
        }
        Err(DavError::Protocol(format!(
            "PROPFIND {url}: too many redirects"
        )))
    }

    /// Collections matching `resourcetype` local name under `home`
    /// (depth 1). Returns `(href, displayname)`.
    pub async fn list_collections(
        &self,
        home: &str,
        resourcetype: &str,
    ) -> Result<Vec<(String, String)>, DavError> {
        let resp = self
            .http
            .request(reqwest::Method::from_bytes(b"PROPFIND").unwrap(), home)
            .timeout(DAV_TIMEOUT)
            .header("Depth", "1")
            .header(reqwest::header::AUTHORIZATION, self.auth_header())
            .header(reqwest::header::CONTENT_TYPE, XML_CONTENT_TYPE)
            .body(propfind_body("<D:resourcetype/><D:displayname/>"))
            .send()
            .await
            .map_err(DavError::Http)?;
        let body = resp.text().await.unwrap_or_default();
        let mut out = Vec::new();
        for block in response_blocks(&body) {
            if find_open(&block, resourcetype).is_none() {
                continue;
            }
            let href = tag_text(&block, "href").unwrap_or_default();
            if href.is_empty() || super::dav::resolve_href(home, &href) == home {
                continue;
            }
            let name = tag_text(&block, "displayname")
                .unwrap_or_else(|| href.trim_matches('/').to_string());
            out.push((super::dav::resolve_href(home, &href), name));
        }
        Ok(out)
    }

    /// The collection's current sync-token (RFC 6578 §3.2), when published.
    pub async fn sync_token(&self, collection: &str) -> Result<Option<String>, DavError> {
        let body = self.propfind_text(collection, "<D:sync-token/>").await?;
        Ok(tag_text(&body, "sync-token"))
    }

    /// `sync-collection` REPORT (RFC 6578 §3.6). `token = None` requests a
    /// full etag listing. Honors the `invalid` marker → full resync.
    pub async fn sync_collection(
        &self,
        collection: &str,
        token: Option<&str>,
    ) -> Result<SyncChanges, DavError> {
        let token_xml = match token {
            Some(t) => format!("<D:sync-token>{}</D:sync-token>", xml_escape(t)),
            None => String::new(),
        };
        let body = format!(
            "<?xml version='1.0' encoding='utf-8'?><D:sync-collection xmlns:D='DAV:'><D:sync-token/>{token_xml}<D:limit><D:nresults>{SYNC_REPORT_LIMIT}</D:nresults></D:limit><D:prop><D:getetag/></D:prop></D:sync-collection>"
        );
        let resp = self.report(collection, body).await?;
        parse_sync_response(&resp)
    }

    /// `addressbook-multiget` (RFC 6352 §8.7). Returns href/etag/data.
    pub async fn addressbook_multiget(
        &self,
        collection: &str,
        hrefs: &[String],
    ) -> Result<Vec<DavItem>, DavError> {
        self.multiget(collection, "addressbook-multiget", "address-data", hrefs)
            .await
    }

    /// `calendar-multiget` (RFC 4791 §7.9). Returns href/etag/data.
    pub async fn calendar_multiget(
        &self,
        collection: &str,
        hrefs: &[String],
    ) -> Result<Vec<DavItem>, DavError> {
        self.multiget(collection, "calendar-multiget", "calendar-data", hrefs)
            .await
    }

    async fn multiget(
        &self,
        collection: &str,
        report: &str,
        data_prop: &str,
        hrefs: &[String],
    ) -> Result<Vec<DavItem>, DavError> {
        let mut out = Vec::new();
        for chunk in hrefs.chunks(MULTIGET_CHUNK) {
            let items: String = chunk
                .iter()
                .map(|h| format!("<D:href>{}</D:href>", xml_escape(h)))
                .collect::<Vec<_>>()
                .concat();
            let body = format!(
                "<?xml version='1.0' encoding='utf-8'?><R:{report} xmlns:R='urn:ietf:params:xml:ns:{ns}' xmlns:D='DAV:'><D:prop><D:getetag/><R:{data_prop}/></D:prop>{items}</R:{report}>",
                ns = if data_prop == "address-data" {
                    "carddav"
                } else {
                    "caldav"
                },
            );
            let resp = self.report(collection, body).await?;
            for block in response_blocks(&resp) {
                let href = tag_text(&block, "href").unwrap_or_default();
                if href.is_empty() {
                    continue;
                }
                out.push(DavItem {
                    href,
                    etag: tag_text(&block, "getetag"),
                    data: tag_text(&block, data_prop),
                });
            }
        }
        Ok(out)
    }

    async fn report(&self, url: &str, body: String) -> Result<String, DavError> {
        let resp = self
            .http
            .request(reqwest::Method::from_bytes(b"REPORT").unwrap(), url)
            .timeout(DAV_TIMEOUT)
            .header(reqwest::header::AUTHORIZATION, self.auth_header())
            .header(reqwest::header::CONTENT_TYPE, XML_CONTENT_TYPE)
            .body(body)
            .send()
            .await
            .map_err(DavError::Http)?;
        if !resp.status().is_success() {
            return Err(DavError::Protocol(format!(
                "REPORT {url}: {}",
                resp.status()
            )));
        }
        Ok(resp.text().await.unwrap_or_default())
    }

    /// Etag listing via depth-1 PROPFIND — fallback for servers without
    /// RFC 6578 sync-collection.
    pub async fn list_etags(&self, collection: &str) -> Result<Vec<DavItem>, DavError> {
        let resp = self
            .http
            .request(
                reqwest::Method::from_bytes(b"PROPFIND").unwrap(),
                collection,
            )
            .timeout(DAV_TIMEOUT)
            .header("Depth", "1")
            .header(reqwest::header::AUTHORIZATION, self.auth_header())
            .header(reqwest::header::CONTENT_TYPE, XML_CONTENT_TYPE)
            .body(propfind_body("<D:getetag/>"))
            .send()
            .await
            .map_err(DavError::Http)?;
        let body = resp.text().await.unwrap_or_default();
        let mut out = Vec::new();
        for block in response_blocks(&body) {
            let (Some(href), Some(etag)) = (tag_text(&block, "href"), tag_text(&block, "getetag"))
            else {
                continue;
            };
            if href.is_empty() || etag.is_empty() {
                continue;
            }
            out.push(DavItem {
                href,
                etag: Some(etag),
                data: None,
            });
        }
        Ok(out)
    }

    /// PUT create (`If-None-Match: *`) — returns the stored etag when the
    /// server echoes one.
    pub async fn put_new(
        &self,
        href: &str,
        body: &str,
        content_type: &str,
    ) -> Result<String, DavError> {
        let resp = self
            .http
            .put(href)
            .timeout(DAV_TIMEOUT)
            .header(reqwest::header::AUTHORIZATION, self.auth_header())
            .header(reqwest::header::CONTENT_TYPE, content_type)
            .header("If-None-Match", "*")
            .body(body.to_string())
            .send()
            .await
            .map_err(DavError::Http)?;
        check_put(href, &resp)
    }

    /// PUT update with `If-Match` — returns the new etag.
    pub async fn put_update(
        &self,
        href: &str,
        body: &str,
        content_type: &str,
        etag: &str,
    ) -> Result<String, DavError> {
        let resp = self
            .http
            .put(href)
            .timeout(DAV_TIMEOUT)
            .header(reqwest::header::AUTHORIZATION, self.auth_header())
            .header(reqwest::header::CONTENT_TYPE, content_type)
            .header("If-Match", etag)
            .body(body.to_string())
            .send()
            .await
            .map_err(DavError::Http)?;
        check_put(href, &resp)
    }

    /// DELETE with `If-Match` (None = unconditional). 404 is success.
    pub async fn delete(&self, href: &str, etag: Option<&str>) -> Result<(), DavError> {
        let mut req = self
            .http
            .delete(href)
            .timeout(DAV_TIMEOUT)
            .header(reqwest::header::AUTHORIZATION, self.auth_header());
        if let Some(etag) = etag {
            req = req.header("If-Match", etag);
        }
        let resp = req.send().await.map_err(DavError::Http)?;
        if !resp.status().is_success() && resp.status().as_u16() != 404 {
            return Err(DavError::Protocol(format!(
                "DELETE {href}: {}",
                resp.status()
            )));
        }
        Ok(())
    }
}

#[allow(dead_code)]
fn check_put(href: &str, resp: &reqwest::Response) -> Result<String, DavError> {
    let status = resp.status();
    if !status.is_success() {
        return Err(DavError::Protocol(format!("PUT {href}: {status}")));
    }
    let etag = resp
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    Ok(etag)
}

fn parse_sync_response(body: &str) -> Result<SyncChanges, DavError> {
    let mut changes = SyncChanges::default();
    if let Some((start, len)) = element(body, "sync-token") {
        let block = &body[start..start + len];
        // RFC 6578 §3.6: an invalid/obsolete token is flagged on the element.
        if let Some((open_start, _)) = find_open(block, "sync-token") {
            let head_end = block[open_start..]
                .find('>')
                .map_or(block.len(), |i| open_start + i);
            let tag_head = &block[open_start..head_end];
            if tag_head.contains("invalid") || tag_head.contains("denied") {
                changes.invalid = true;
            }
        }
        changes.token = tag_text(block, "sync-token");
    }
    for block in response_blocks(body) {
        let Some(href) = tag_text(&block, "href") else {
            continue;
        };
        // Removed: propstat 404 (RFC 6578 §3.6) or a bare status 404.
        if block.contains("404") && tag_text(&block, "getetag").is_none() {
            changes.removed.push(href);
        } else if let Some(etag) = tag_text(&block, "getetag") {
            changes.changed.push(DavItem {
                href,
                etag: Some(etag),
                data: None,
            });
        }
    }
    Ok(changes)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SYNC_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:" xmlns:card="urn:ietf:params:xml:ns:carddav">
  <D:response><D:href>/ab/john.vcf</D:href><D:propstat><D:prop><D:getetag>"v1"</D:getetag></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>
  <D:response><D:href>/ab/gone.vcf</D:href><D:status>HTTP/1.1 404 Not Found</D:status></D:response>
  <D:sync-token>http://s.example/ns/3</D:sync-token>
</D:multistatus>"#;

    #[test]
    fn parses_sync_report() {
        let c = parse_sync_response(SYNC_BODY).unwrap();
        assert_eq!(c.changed.len(), 1);
        assert_eq!(c.changed[0].href, "/ab/john.vcf");
        assert_eq!(c.changed[0].etag.as_deref(), Some("\"v1\""));
        assert_eq!(c.removed, vec!["/ab/gone.vcf".to_string()]);
        assert_eq!(c.token.as_deref(), Some("http://s.example/ns/3"));
        assert!(!c.invalid);
    }

    #[test]
    fn detects_invalid_token() {
        let body = SYNC_BODY.replacen(
            "<D:sync-token>",
            "<D:sync-token xmlns:ns='http://s' invalid=\"yes\">",
            1,
        );
        let c = parse_sync_response(&body).unwrap();
        assert!(c.invalid);
    }

    #[test]
    fn response_blocks_find_all_regardless_of_prefix() {
        let blocks = response_blocks(SYNC_BODY);
        assert_eq!(blocks.len(), 2);
        assert!(blocks[1].contains("gone.vcf"));
    }

    #[test]
    fn tag_text_unescapes_entities() {
        let xml = "<D:href>/a&amp;b.vcf</D:href>";
        assert_eq!(tag_text(xml, "href").unwrap(), "/a&b.vcf");
    }

    #[test]
    fn tag_text_strips_children() {
        let xml = "<card:address-data>BEGIN:VCARD\r\nEND:VCARD</card:address-data>";
        assert!(
            tag_text(xml, "address-data")
                .unwrap()
                .contains("BEGIN:VCARD")
        );
    }

    #[test]
    fn element_is_nesting_aware() {
        let xml = "<a><a><b/></a></a><a>2</a>";
        assert_eq!(element(xml, "a").unwrap().1, 18);
        // all_tag_text collects per-element leaf text; the outer wrapper
        // contributes none here.
        assert_eq!(all_tag_text(xml, "a").len(), 1);
        assert_eq!(all_tag_text("<x><a>1</a></x><a>2</a>", "a"), vec!["1", "2"]);
    }

    #[test]
    fn parses_cdata_address_data() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:R="urn:ietf:params:xml:ns:carddav" xmlns:D="DAV:">
  <D:response>
    <D:href>/dav/addressbooks/user/x/Default/abc.vcf</D:href>
    <D:propstat>
      <D:prop>
        <D:getetag>"966f3cb2"</D:getetag>
        <R:address-data><![CDATA[BEGIN:VCARD
VERSION:3.0
UID:abc
FN:Test User
EMAIL:test@example.com
END:VCARD
]]></R:address-data>
      </D:prop>
    </D:propstat>
  </D:response>
</D:multistatus>"#;
        let blocks = response_blocks(xml);
        assert_eq!(blocks.len(), 1, "blocks: {blocks:?}");
        let href = tag_text(&blocks[0], "href");
        assert_eq!(href.unwrap(), "/dav/addressbooks/user/x/Default/abc.vcf");
        let data = tag_text(&blocks[0], "address-data");
        assert!(data.is_some(), "address-data not found");
        let d = data.unwrap();
        assert!(d.contains("BEGIN:VCARD"), "no vcard start in: {d:?}");
        assert!(d.contains("Test User"), "no FN in: {d:?}");
        assert!(!d.contains("CDATA"), "CDATA marker leaked: {d:?}");
    }

    #[test]
    fn multistatus_without_token_is_ok() {
        let body = r#"<D:multistatus xmlns:D="DAV:"><D:response><D:href>/x.vcf</D:href><D:propstat><D:prop><D:getetag>"e"</D:getetag></D:prop></D:propstat></D:response></D:multistatus>"#;
        let c = parse_sync_response(body).unwrap();
        assert!(c.token.is_none());
        assert_eq!(c.changed.len(), 1);
    }
}
