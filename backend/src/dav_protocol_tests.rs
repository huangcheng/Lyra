//! Integration tests for the DAV protocol layer against a loopback mock
//! server (avatar-test pattern): RFC 6764 discovery, homeset, collections,
//! sync-collection REPORT with removals, multiget, and etag-guarded writes.

#![cfg(test)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::extract::Request;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;

use crate::dav::DavClient;

const JOHN_VCF: &str =
    "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:John Doe\r\nEMAIL:john@example.com\r\nEND:VCARD\r\n";

fn xml(body: &str) -> Response {
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/xml")],
        body.to_string(),
    )
        .into_response()
}

fn homeset_xml(href: &str) -> String {
    format!(
        r#"<D:multistatus xmlns:D="DAV:"><D:response><D:propstat><D:prop><card:addressbook-home-set xmlns:card="urn:ietf:params:xml:ns:carddav"><D:href>{href}</D:href></card:addressbook-home-set></D:prop></D:propstat></D:response></D:multistatus>"#
    )
}

fn principal_xml(href: &str) -> String {
    format!(
        r#"<D:multistatus xmlns:D="DAV:"><D:response><D:propstat><D:prop><D:current-user-principal><D:href>{href}</D:href></D:current-user-principal></D:prop></D:propstat></D:response></D:multistatus>"#
    )
}

fn collections_xml() -> &'static str {
    r#"<D:multistatus xmlns:D="DAV:" xmlns:card="urn:ietf:params:xml:ns:carddav">
  <D:response><D:href>/dav/ab/personal/</D:href><D:propstat><D:prop><D:resourcetype><card:addressbook/></D:resourcetype><D:displayname>Personal</D:displayname></D:prop></D:propstat></D:response>
  <D:response><D:href>/dav/ab/other.vcf</D:href><D:propstat><D:prop><D:resourcetype/><D:getetag>"x"</D:getetag></D:prop></D:propstat></D:response>
</D:multistatus>"#
}

fn sync_xml(token: &str, changed: &str, removed: Option<&str>) -> String {
    let removed = removed
        .map(|h| format!(
            "<D:response><D:href>{h}</D:href><D:status>HTTP/1.1 404 Not Found</D:status></D:response>"
        ))
        .unwrap_or_default();
    format!(
        r#"<D:multistatus xmlns:D="DAV:"><D:response><D:href>{changed}</D:href><D:propstat><D:prop><D:getetag>"e2"</D:getetag></D:prop></D:propstat></D:response>{removed}<D:sync-token>{token}</D:sync-token></D:multistatus>"#
    )
}

fn multiget_xml(href: &str, etag: &str, data: &str) -> String {
    format!(
        r#"<D:multistatus xmlns:D="DAV:" xmlns:card="urn:ietf:params:xml:ns:carddav"><D:response><D:href>{href}</D:href><D:propstat><D:prop><D:getetag>{etag}</D:getetag><card:address-data>{data}</card:address-data></D:prop></D:propstat></D:response></D:multistatus>"#
    )
}

struct MockDav {
    puts: AtomicUsize,
    last_precondition: std::sync::Mutex<Option<String>>,
}

async fn spawn_mock() -> (String, tokio::task::JoinHandle<()>, Arc<MockDav>) {
    let mock = Arc::new(MockDav {
        puts: AtomicUsize::new(0),
        last_precondition: std::sync::Mutex::new(None),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let m = mock.clone();
    let app = Router::new().route(
        "/{*path}",
        any(move |req: Request| {
            let m = m.clone();
            async move {
                let method = req.method().clone();
                let path = req.uri().path().to_string();
                let headers: HeaderMap = req.headers().clone();
                let body = axum::body::to_bytes(req.into_body(), usize::MAX)
                    .await
                    .map(|b| String::from_utf8_lossy(&b).to_string())
                    .unwrap_or_default();
                dispatch(&m, method.as_str(), &path, &headers, &body)
            }
        }),
    );
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    tokio::task::yield_now().await;
    (format!("http://{addr}"), handle, mock)
}

fn dispatch(m: &MockDav, method: &str, path: &str, headers: &HeaderMap, body: &str) -> Response {
    match (method, path) {
        ("GET", "/.well-known/carddav" | "/dav/") => xml(&homeset_xml("/dav/ab/")),
        ("PROPFIND", "/.well-known/carddav") => xml(&principal_xml("/dav/")),
        ("PROPFIND", "/dav/") => xml(&homeset_xml("/dav/ab/")),
        ("PROPFIND", "/dav/ab/personal/") => xml(&homeset_xml("/dav/ab/personal/")),
        ("PROPFIND", "/dav/ab/") => {
            if body.contains("sync-token") {
                xml(
                    "<D:multistatus xmlns:D='DAV:'><D:response><D:href>/dav/ab/</D:href><D:propstat><D:prop><D:sync-token>tok-0</D:sync-token></D:prop></D:propstat></D:response></D:multistatus>",
                )
            } else {
                xml(collections_xml())
            }
        }
        ("REPORT", "/dav/ab/") => {
            if body.contains("sync-collection") {
                xml(&sync_xml(
                    "tok-2",
                    "/dav/ab/john.vcf",
                    Some("/dav/ab/old.vcf"),
                ))
            } else {
                xml(&multiget_xml("/dav/ab/john.vcf", "\"e2\"", JOHN_VCF))
            }
        }
        ("PUT", _) => {
            m.puts.fetch_add(1, Ordering::SeqCst);
            *m.last_precondition.lock().unwrap() = headers
                .get("if-match")
                .or_else(|| headers.get("if-none-match"))
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            (StatusCode::CREATED, format!("stored {}", body.len())).into_response()
        }
        ("GET", p) if p.to_ascii_lowercase().ends_with(".vcf") => (
            StatusCode::OK,
            [("content-type", "text/vcard")],
            JOHN_VCF.to_string(),
        )
            .into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

#[tokio::test]
async fn discovery_resolves_homeset() {
    let (base, _h, _m) = spawn_mock().await;
    let client = DavClient::new("u".into(), "p".into(), &base).unwrap();
    let home = client
        .discover_homeset("/.well-known/carddav", "addressbook-home-set")
        .await
        .expect("discover");
    assert!(home.contains("/dav/ab"), "home={home}");
}

#[tokio::test]
async fn collections_filter_by_resourcetype() {
    let (base, _h, _m) = spawn_mock().await;
    let client = DavClient::new("u".into(), "p".into(), &base).unwrap();
    let cols = client
        .list_collections(&format!("{base}/dav/ab/"), "addressbook")
        .await
        .expect("collections");
    assert_eq!(cols.len(), 1, "{cols:?}");
    assert_eq!(cols[0].1, "Personal");
}

#[tokio::test]
async fn propfind_returns_sync_token() {
    let (base, _h, _m) = spawn_mock().await;
    let client = DavClient::new("u".into(), "p".into(), &base).unwrap();
    let token = client
        .sync_token(&format!("{base}/dav/ab/"))
        .await
        .expect("token");
    assert_eq!(token.as_deref(), Some("tok-0"));
}

#[tokio::test]
async fn sync_report_yields_changes_and_removals() {
    let (base, _h, _m) = spawn_mock().await;
    let client = DavClient::new("u".into(), "p".into(), &base).unwrap();
    let changes = client
        .sync_collection(&format!("{base}/dav/ab/"), Some("tok-1"))
        .await
        .expect("sync");
    assert_eq!(changes.changed.len(), 1);
    assert_eq!(changes.removed.len(), 1);
    assert_eq!(changes.token.as_deref(), Some("tok-2"));
}

#[tokio::test]
async fn multiget_returns_data() {
    let (base, _h, _m) = spawn_mock().await;
    let client = DavClient::new("u".into(), "p".into(), &base).unwrap();
    let items = client
        .addressbook_multiget(&format!("{base}/dav/ab/"), &["/dav/ab/john.vcf".into()])
        .await
        .expect("multiget");
    assert_eq!(items.len(), 1);
    assert!(
        items[0]
            .data
            .as_deref()
            .unwrap_or_default()
            .contains("John Doe")
    );
}

#[tokio::test]
async fn put_new_sends_precondition() {
    let (base, _h, m) = spawn_mock().await;
    let client = DavClient::new("u".into(), "p".into(), &base).unwrap();
    client
        .put_new(&format!("{base}/dav/ab/new.vcf"), JOHN_VCF, "text/vcard")
        .await
        .expect("put");
    assert_eq!(m.puts.load(Ordering::SeqCst), 1);
    assert_eq!(m.last_precondition.lock().unwrap().as_deref(), Some("*"));
}

#[tokio::test]
async fn put_update_sends_etag() {
    let (base, _h, m) = spawn_mock().await;
    let client = DavClient::new("u".into(), "p".into(), &base).unwrap();
    client
        .put_update(
            &format!("{base}/dav/ab/john.vcf"),
            JOHN_VCF,
            "text/vcard",
            "\"e2\"",
        )
        .await
        .expect("put update");
    assert_eq!(
        m.last_precondition.lock().unwrap().as_deref(),
        Some("\"e2\"")
    );
}
