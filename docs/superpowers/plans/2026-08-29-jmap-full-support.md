
# JMAP Full Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace Lyra's hand-rolled JMAP client (`backend/src/jmap.rs`) with the `jmap-client` crate (v0.4.2, Stalwart Labs) behind a single seam module, closing the functional gaps: Bearer auth (Fastmail API tokens), `Email/queryChanges` `removed` applied as local deletes, `Email/changes` for flag/move propagation, `thread_id` persistence, attachment blob downloads, batched send with `Email/import` fallback for OpenGPG MIME, crate EventSource push, JMAP flag push from `PATCH /messages`, and `Core`-level probing.

**Architecture:** One seam — `backend/src/sync/jmap_client.rs` — is the only module importing `jmap_client`. Behind it, everything speaks Lyra's existing DTOs (`JmapMailbox`, `JmapEmail`, …) so `sync/store.rs` persistence keeps its shape (additive changes only: `jmap_thread_id` column, `folder_id` updates on upsert, removed-ids deletes, per-message persist results). The seam owns credentials (Basic or Bearer by `auth_type`), discovery (same-origin redirect pre-resolution + crate `connect()`), session-URL origin pinning (`netsec::origin_of`), a process-wide per-account session cache, typed error classification into `SyncError` (`is_transient` / `is_auth` / `is_stale_query_state`), and all crate↔Lyra type mapping. Call sites (`jmap_loop`, `send`, `http`, `jmap_push`, `accounts`, `plugins/jmap_send`) are rewritten mechanically, one commit per rollout stage; the old transport in `jmap.rs` shrinks progressively and is deleted last.

**Tech Stack:** Rust + Axum backend; `jmap-client 0.4.2` (`default-features = false, features = ["async", "aws_lc_rs"]` — no WebSocket; brings reqwest 0.13 alongside Lyra's reqwest 0.12, both compile); sea-orm 2.0 / sqlx dual SQLite+PostgreSQL; existing test harness (`cargo test --bin lyra_backend`, in-memory SQLite via `Storage::new("sqlite::memory:")`).

**Source of truth:** `docs/superpowers/specs/2026-08-29-lyra-jmap-full-support-design.md` (approved). This plan implements its 7-commit rollout exactly.

---

## Crate API quick reference (verified against `stalwartlabs/jmap-client` @ `main` = 0.4.2)

All names below were read from the crate source; the file column is where to re-check if a compile error disagrees.

| What | Exact API | Crate file |
|---|---|---|
| Build client | `Client::new()` → `ClientBuilder`; `.credentials(impl Into<Credentials>)`, `.timeout(Duration)`, `.follow_redirects(impl IntoIterator<Item = impl Into<String>>)`, `.connect(url)` fetches `{url}/.well-known/jmap` itself | `src/client.rs` |
| Credentials | `Credentials::basic(u, p)` / `Credentials::bearer(token)`; enum variants `Basic(String)` (pre-base64ed) / `Bearer(String)` are public | `src/client.rs` |
| Redirect policy | host-allowlist custom policy; **default empty = every redirect errors**; `Client::redirect_policy()` reused by upload/download/event_source | `src/client.rs` |
| Session | `client.session() -> Arc<Session>`; `session.api_url()/.upload_url()/.download_url()/.event_source_url()` (all `&str`, required fields); `session.has_capability(impl AsRef<str>)`; `session.primary_accounts() -> impl Iterator<Item = (&String, &String)>`; `session.accounts() -> impl Iterator<Item = &String>`; `session.core_capabilities() -> Option<&CoreCapabilities>` → `.max_calls_in_request()` | `src/core/session.rs` |
| Session upkeep | `client.is_session_updated()`, `client.refresh_session()`, `client.set_default_account_id(id)`, `client.default_account_id()` (`connect()` picks an *arbitrary first* primary account — always override with the mail one) | `src/client.rs` |
| Request building | `client.build() -> Request`; `request.get_email()/.query_email()/.query_email_changes(since)/.changes_email(since)/.set_email()/.import_email()/.get_identity()/.get_mailbox()/.set_email_submission()` each return a mutable args builder; `request.send() -> Response<TaggedMethodResponse>`; `request.send_single::<T>() -> T` (single-call requests) | `src/core/request.rs`, `src/email/helpers.rs`, `src/mailbox/helpers.rs`, `src/identity/helpers.rs`, `src/email_submission/helpers.rs` |
| Responses | `Response::unwrap_method_responses() -> Vec<TaggedMethodResponse>`; per-type unwraps `.unwrap_get_email()/.unwrap_query_email()/.unwrap_query_changes_email()/.unwrap_set_email()/.unwrap_import_email()/.unwrap_get_identity()/.unwrap_get_mailbox()/.unwrap_set_email_submission()` (each `Result<…>`, method errors mapped) | `src/core/response.rs` |
| Query | `QueryRequest::filter/sort/position(i32)/limit(usize)/calculate_total(bool)`; `result_reference() -> ResultReference` (`/ids`); `QueryResponse::take_ids()/take_query_state()`; `email::query::Filter::in_mailbox(id)`; `email::query::Comparator::received_at().descending()` | `src/core/query.rs`, `src/email/query.rs` |
| QueryChanges | `QueryChangesRequest::filter/sort/max_changes(usize)`; `QueryChangesResponse::added() -> &[AddedItem]` (`AddedItem::id()`), `.removed() -> &[String]`, `.new_query_state()` | `src/core/query_changes.rs` |
| Changes | `ChangesRequest::max_changes(usize)`; `ChangesResponse::take_updated()/take_destroyed()/take_new_state()/has_more_changes()` | `src/core/changes.rs` |
| Get | `GetRequest::ids(iter)/.ids_ref(ResultReference)/.properties(iter)`; `.arguments()` → `email::GetArguments`: `.fetch_text_body_values(true)/.fetch_html_body_values(true)/.max_body_value_bytes(usize)`; `GetResponse::take_list()/take_state()` | `src/core/get.rs`, `src/email/mod.rs` |
| Email (read) | `Email<Get>` accessors: `id(), blob_id(), thread_id(), mailbox_ids() -> Vec<&str>, keywords() -> Vec<&str>, size() -> usize, received_at() -> Option<i64>, message_id()/in_reply_to()/references() -> Option<&[String]>, sender()/from()/to()/cc()/bcc()/reply_to() -> Option<&[EmailAddress]>, subject(), body_value(part_id) -> Option<&EmailBodyValue> (.value()/.is_truncated()), text_body()/html_body()/attachments() -> Option<&[EmailBodyPart]>, has_attachment() -> bool, preview()`; `EmailBodyPart::part_id()/blob_id()/name()/content_type()/content_disposition()/content_id()/size()`; `EmailAddress::name()/.email()` | `src/email/get.rs` |
| Email (write) | `Email<Set>` builders: `mailbox_ids(iter)` (full replace), `mailbox_id(id, bool)` (patch), `keywords(iter)` (full replace), `keyword(k, bool)` (patch), `from/to/cc/bcc(iter of impl Into<EmailAddress>)`, `subject()`, `body_value(id: String, impl Into<EmailBodyValue>)`, `text_body()/html_body()/attachment(impl Into<EmailBodyPart<Get>>)`, `in_reply_to()/references()`; `EmailAddress::from((name, email))` / `from(email)`; `EmailBodyPart::new().part_id()/.blob_id()/.name()/.content_type()` | `src/email/set.rs` |
| Set protocol | `SetRequest::create_with_id("draft") -> &mut O`; `.update(id) -> &mut O`; `.destroy([id])`; `SetResponse::created(id)/.updated(id)/.destroyed(id)` map `notCreated/notUpdated/notDestroyed` to `Err(Error::Set)` | `src/core/set.rs` |
| Submission | `EmailSubmission<Set>`: `.email_id("#draft")`, `.identity_id(id)`; `SetArguments` via `set_req.arguments()`: `.on_success_update_email("sub")` (auto-prefixes `#`) → `&mut Email<Set>` patch object; response `created("sub")?.take_id()` | `src/email_submission/set.rs`, `src/email_submission/get.rs` |
| Import | `request.import_email().email(blob_id) -> &mut EmailImport`: `.mailbox_ids(iter)/.keywords(iter)/.create_id()` (`"i0"`); `EmailImportResponse::created(id)` | `src/email/import.rs` |
| Client send helpers | `client.email_set_mailboxes(id, ids)`, `client.email_destroy(id)` (used by flags/drafts wiring) | `src/email/helpers.rs` |
| Blob | `client.upload(Option<&str> account_id, Vec<u8>, Option<&str> content_type) -> UploadResponse` (`.take_blob_id()`); `client.download(blob_id) -> Vec<u8>` (uses `downloadUrl` template; origin-pinned at connect) | `src/blob/upload.rs`, `src/blob/download.rs` |
| Mailbox (read) | `Mailbox<Get>` accessors: `id(), name(), parent_id(), role() -> Role, sort_order() -> u32, total_emails()/unread_emails() -> usize`; `Role::{Inbox, Sent, Trash, Drafts, Junk, Archive, Important, Other(String), None}` | `src/mailbox/mod.rs`, `src/mailbox/get.rs` |
| Identity | `Identity { pub id/name/email: Option<String>, … }` | `src/identity/mod.rs` |
| Push | `client.event_source(Option<impl IntoIterator<Item = DataType>>, close_after_state: bool, ping: Option<u32>, last_event_id: Option<&str>) -> impl Stream<Item = Result<PushNotification>> + Unpin`; `None::<Vec<DataType>>` = `types=*`; ping events are filtered by the parser; stream uses **connect_timeout only** (no stream kill); `PushNotification::StateChange(Changes)`; `Changes::has_type(DataType)`, `Changes` is `Deserialize` (test-constructible via JSON) | `src/event_source/stream.rs`, `src/event_source/mod.rs`, `src/event_source/parser.rs` |
| Errors | `jmap_client::Error::{Transport(reqwest 0.13), Parse(serde_json), Internal(String), Problem(Box<ProblemDetails>), Server(String /* "401 Unauthorized" */), Method(MethodError), Set(SetError<String>)}`; `MethodError { pub p_type: MethodErrorType }`, `.error()`; `MethodErrorType::CannotCalculateChanges/ServerUnavailable/ServerPartialFail/TooManyChanges`; `ProblemDetails::status() -> Option<u32>`, `ProblemDetails::new(...)` (pub); `SetError::error() -> &SetErrorType::{RateLimit, OverQuota, …}`; `From` impls: `MethodError`, `ProblemDetails`, `SetError<String>` → `Error` | `src/lib.rs`, `src/core/error.rs`, `src/core/set.rs` |
| Test construction | `Email<Get>`, `Mailbox<Get>`, `Identity`, `Changes` all derive `Deserialize` (private fields OK — build via `serde_json::from_value`); `SetRequest::<Email<Set>>::new(RequestParams::new("acc", Method::SetEmail, 0))` + builders → `serde_json::to_value` for wire-shape assertions; `EmailImportRequest::new(RequestParams…)` likewise | `src/core/mod.rs`, `src/core/set.rs`, `src/email/mod.rs` |
| **Not sendable** | `Core/echo`: `Method::Echo` + `MethodResponse::Echo` exist for responses, but `Arguments` has **no Echo variant** and no `Request::echo()` — 0.4.2 cannot emit it. Probe = connect + capability instead. | `src/core/request.rs` |

## Decisions & deviations from the design spec

1. **Discovery redirect handling (deviates from "keep the crate allowlist empty").** `Client::connect(url)` unconditionally fetches `{url}/.well-known/jmap` and the crate offers no session-URL constructor (`src/client.rs`). With the default empty allowlist, a redirecting server (Fastmail: `/.well-known/jmap` → `/jmap/session`, the case Lyra's `083614a` fix handled) could never connect. The seam therefore: (a) pre-resolves the chain itself with Lyra's existing origin-scoped follower (`resolve_discovery_redirect`, max 5 hops, credentials never cross origin), and (b) only when ≥1 redirect was observed, calls `.follow_redirects([configured_host])` — the single host the follower already vetted. Post-connect origin pinning of all four session URLs (`apiUrl`/`uploadUrl`/`downloadUrl`/`eventSourceUrl`) is the compensating control. Non-redirecting servers keep the deny-all default.
2. **Probe cannot use `Core/echo`** (not sendable in 0.4.2 — see table). The account probe is `connect` + `session().has_capability("urn:ietf:params:jmap:submission")`, which is the same authenticated round trip today's probe performs. Same functional outcome, no behavior change.
3. **`thread_id` goes to a new `message.jmap_thread_id TEXT` column** (migration 0014, both engines), *not* `message.thread_id`: on Postgres `thread.id`/`message.thread_id` are UUID with an FK, and JMAP threadIds are opaque server strings. Nothing reads the local `thread` table today; a future threading UI reads `jmap_thread_id`.
4. **Account-level `email_state` cursor anchors on the inbox folder row.** `Email/changes` state is account-scoped but `sync_cursor.folder_id` is `NOT NULL` with a folder FK; the row hangs off the account's `inbox`-role folder (`cursor_type = "email_state"`). No inbox → `Email/changes` skipped (folders still sync via `queryChanges`). Cascade-delete of the folder drops the cursor, which is the correct reset.
5. **Session cache is a process-wide `OnceLock<Mutex<HashMap<account_id, Arc<JmapSeam>>>>`** in the seam (the design's "on the sync state" — no such struct exists; `SyncCtx` carries only ids). Evicted on auth errors and on account update/delete.
6. **`Role::Junk` maps to Lyra role `"spam"`** in mailbox mapping (Lyra's role vocabulary is `inbox/sent/drafts/trash/spam/archive`; the old client stored `"junk"` verbatim, which `spam_message`'s role lookup could never find).
7. **Send requests are fixed at 2 method calls** (`Email/set`+`EmailSubmission/set` or `Email/import`+`EmailSubmission/set`) and the identities+mailboxes read is one batched request; only the sync page batch checks `max_calls_in_request()` and splits when `< 2`. `maxCallsInRequest >= 2` is universal in practice.
8. **Patch removal emits `false`** (crate's `keyword()`/`mailbox_id()` patch builders) where the RFC 8621 §7.5.1 example shows `null`. The post-send patch avoids the question entirely with full-value replacement (`keywords: {}`, `mailboxIds: {sent}`) — valid because the email was created in the same request with exactly `$draft`. The flags push (`set_email_keywords`) uses the crate's patch form; if the live Fastmail check rejects it, switch to read-modify-write full replacement (fallback noted in Open questions).
9. **Attachments download eagerly during sync, but only for newly-inserted messages** (`was_new`), capped at 25 MiB per blob (mirrors `recovery::MAX_MESSAGE_BODY_BYTES`); failures mark `flags.fetch_error` and never abort sync. `data_dir` reaches the loop via a `plugins::bind_data_dir` OnceLock mirroring `bind_storage`. No backfill of pre-existing rows (v1 JMAP was effectively unreleased).
10. **`Mailbox/changes|query|set`, `Email/copy`, `SearchSnippet/get`, `VacationResponse/get|set`** are crate-supported but intentionally **not wired** — no call site exists in the rollout; they remain reachable through the seam's `Request` builder when their UI lands.

## Conventions for every task

- Work from the repo root: `cd F:/Lyra` (Git Bash; forward slashes everywhere).
- Scoped tests: `cd backend && cargo test --bin lyra_backend jmap`
- Full suite: `cd backend && cargo test --bin lyra_backend` — expect `test result: ok` **except** 3 pre-existing gpg-interop failures (`opengpg::interop::tests::gpg_decrypts_lyra_encrypted_message`, `lyra_decrypts_gpg_encrypted_message`, `gpg_verifies_lyra_detached_signature`) caused by Git-Bash path mangling. They are pre-existing; do not fix, do not add new failures. Identify failures with `cd backend && cargo test --bin lyra_backend 2>&1 | grep -E "FAILED|failures:"`.
- Clippy: `cd backend && cargo clippy --all-targets --all-features 2>&1 | grep "warning:" | grep -v "oauth/config.rs"` — expect **empty** (the 2 `result_large_err` warnings in `src/oauth/config.rs` are pre-existing on this toolchain; out of scope). Anything mentioning a changed file must be fixed before committing. Do not run with `-D warnings` as a gate — it fails on the pre-existing two.
- Format only changed files: `cd backend && rustfmt --edition 2024 <file> [<file>…]`, then confirm with `cd backend && cargo fmt --check`.
- Commits: `git add <changed files> && git commit -m "<exact message from the spec rollout>"`. Never commit secrets; the tree is public forever (AGENTS.md).
- Hermetic tests only: pure mapping/classification/serialization functions, and in-memory SQLite via the existing `test_pool()` / `seed_user_and_account()` helpers in `backend/src/sync/mod.rs` tests. No network, no mock servers.
- Existing test style: `#[cfg(test)] mod tests` in the same file; `#[test]` for pure fns, `#[tokio::test]` for DB.

---

### Task 1: Add `jmap-client` + seam module (Bearer auth, session cache, origin pinning, DTO mapping, error classification)

Commit: `feat: add jmap-client seam with Bearer auth, session cache, origin pinning`

**Files:**
- Modify: `backend/Cargo.toml`
- Create: `backend/src/sync/jmap_client.rs`
- Modify: `backend/src/sync/mod.rs` (declare module)
- Modify: `backend/src/jmap.rs` (shrink to old client + re-exports)

- [ ] **Step 1: Add the dependency**

In `backend/Cargo.toml`, immediately after the `reqwest` line, add:

```toml
# JMAP protocol client (RFC 8620/8621). Pulls reqwest 0.13 alongside our 0.12
# (separate 0.x majors coexist); websockets off — push uses its EventSource stream.
jmap-client = { version = "0.4.2", default-features = false, features = ["async", "aws_lc_rs"] }
```

Run: `cd backend && cargo check`
Expected: `Finished` … (green; `Cargo.lock` gains reqwest 0.13, `ahash`, `parking_lot`, `maybe-async`, `async-stream`, `aws-lc-rs` if absent). Commit nothing yet.

- [ ] **Step 2: Write the failing seam tests**

Create `backend/src/sync/jmap_client.rs` containing only the test module below (it references items that do not exist yet):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use jmap_client::core::error::{MethodError, MethodErrorType, ProblemDetails, ProblemType};

    // ── redirect pre-resolution (moved from jmap.rs) ────────────────

    #[test]
    fn discovery_redirect_relative_same_origin_accepted() {
        let resolved = resolve_discovery_redirect(
            "https://api.fastmail.com/.well-known/jmap",
            "/jmap/session",
            "https://api.fastmail.com:443",
        )
        .expect("same-origin relative redirect must be followed");
        assert_eq!(resolved, "https://api.fastmail.com/jmap/session");
    }

    #[test]
    fn discovery_redirect_absolute_same_origin_accepted() {
        let resolved = resolve_discovery_redirect(
            "https://jmap.example.com/.well-known/jmap",
            "https://jmap.example.com/jmap/session",
            "https://jmap.example.com:443",
        )
        .expect("same-origin absolute redirect must be followed");
        assert_eq!(resolved, "https://jmap.example.com/jmap/session");
    }

    #[test]
    fn discovery_redirect_cross_origin_rejected() {
        let err = resolve_discovery_redirect(
            "https://jmap.example.com/.well-known/jmap",
            "https://evil.example/session",
            "https://jmap.example.com:443",
        )
        .unwrap_err();
        assert!(matches!(err, JmapError::CrossOrigin(_)), "got: {err}");
    }

    #[test]
    fn discovery_redirect_scheme_downgrade_rejected() {
        let err = resolve_discovery_redirect(
            "https://jmap.example.com/.well-known/jmap",
            "http://jmap.example.com/jmap/session",
            "https://jmap.example.com:443",
        )
        .unwrap_err();
        assert!(matches!(err, JmapError::CrossOrigin(_)), "got: {err}");
    }

    #[test]
    fn discovery_redirect_garbage_location_rejected() {
        let err = resolve_discovery_redirect(
            "https://jmap.example.com/.well-known/jmap",
            "http://[::1",
            "https://jmap.example.com:443",
        )
        .unwrap_err();
        assert!(matches!(err, JmapError::SessionDiscovery(_)), "got: {err}");
    }

    // ── session URL origin pinning ──────────────────────────────────

    #[test]
    fn session_url_pinning_accepts_same_origin() {
        pin_session_urls(
            "https://jmap.example.com:443",
            &[
                "https://jmap.example.com/api/",
                "https://jmap.example.com/upload/{accountId}",
                "https://jmap.example.com/download/{accountId}/{blobId}/{name}/{type}",
                "https://jmap.example.com/events/?types={types}&closeafter={closeafter}&ping={ping}",
            ],
        )
        .unwrap();
    }

    #[test]
    fn session_url_pinning_rejects_cross_origin_and_garbage() {
        // A malicious JMAP server pointing uploadUrl elsewhere must never
        // receive our Authorization header.
        let err = pin_session_urls(
            "https://jmap.example.com:443",
            &["https://jmap.example.com/api/", "https://evil.example/upload/"],
        )
        .unwrap_err();
        assert!(matches!(err, JmapError::CrossOrigin(_)), "got: {err}");
        // https → http on the same host is a different origin.
        assert!(pin_session_urls("https://jmap.example.com:443", &["http://jmap.example.com/api/"]).is_err());
        // Unparseable URL.
        assert!(pin_session_urls("https://jmap.example.com:443", &["not a url"]).is_err());
    }

    // ── credentials ─────────────────────────────────────────────────

    #[test]
    fn bearer_auth_type_selects_bearer_credential() {
        let creds = credentials_for("bearer", "u@example.com", "api-token");
        assert_eq!(authorization_header(&creds), "Bearer api-token");
        // case-insensitive
        let creds = credentials_for("Bearer", "u@example.com", "api-token");
        assert_eq!(authorization_header(&creds), "Bearer api-token");
    }

    #[test]
    fn password_auth_type_selects_basic_credential() {
        use base64::Engine as _;
        let creds = credentials_for("password", "u@example.com", "pw");
        let expected = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode("u@example.com:pw")
        );
        assert_eq!(authorization_header(&creds), expected);
    }

    // ── error classification ────────────────────────────────────────

    #[test]
    fn stale_query_state_detects_rfc_code() {
        let err = JmapError::from(jmap_client::Error::Method(MethodError {
            p_type: MethodErrorType::CannotCalculateChanges,
        }));
        assert!(err.is_stale_query_state());
        // Legacy string-matched arm (hand-rolled transport until Task 7).
        let legacy = JmapError::Method {
            code: "cannotCalculateChanges".into(),
            description: String::new(),
        };
        assert!(legacy.is_stale_query_state());
        assert!(!JmapError::InvalidResponse("nope".into()).is_stale_query_state());
    }

    #[test]
    fn transient_classification_matrix() {
        let unavailable = JmapError::from(jmap_client::Error::Method(MethodError {
            p_type: MethodErrorType::ServerUnavailable,
        }));
        assert!(unavailable.is_transient());

        let server_500 = JmapError::from(jmap_client::Error::Server("500 Internal Server Error".into()));
        assert!(server_500.is_transient());

        let problem_429 = JmapError::from(jmap_client::Error::Problem(Box::new(ProblemDetails::new(
            ProblemType::Other("slowDown".into()),
            Some(429),
            None,
            None,
            None,
            None,
        ))));
        assert!(problem_429.is_transient());

        let rate_limited: jmap_client::Error =
            serde_json::from_value::<jmap_client::core::set::SetError<String>>(
                serde_json::json!({"type": "rateLimit"}),
            )
            .unwrap()
            .into();
        assert!(JmapError::from(rate_limited).is_transient());

        // Permanent classifications.
        let invalid = JmapError::from(jmap_client::Error::Method(MethodError {
            p_type: MethodErrorType::InvalidArguments,
        }));
        assert!(!invalid.is_transient());
        assert!(!JmapError::InvalidResponse("nope".into()).is_transient());
        assert!(!JmapError::SessionDiscovery("nope".into()).is_transient());
    }

    #[test]
    fn auth_classification_maps_401_to_authentication() {
        let problem_401 = JmapError::from(jmap_client::Error::Problem(Box::new(ProblemDetails::new(
            ProblemType::Other("unauthorized".into()),
            Some(401),
            None,
            None,
            None,
            None,
        ))));
        assert!(
            matches!(problem_401, JmapError::Authentication(_)),
            "401 ProblemDetails must map to Authentication, got: {problem_401:?}"
        );
        assert!(problem_401.is_auth());

        let server_401 = JmapError::from(jmap_client::Error::Server("401 Unauthorized".into()));
        assert!(matches!(server_401, JmapError::Authentication(_)));
        assert!(server_401.is_auth());

        assert!(!JmapError::SessionDiscovery("x".into()).is_auth());
    }

    // ── credential decrypt (moved from jmap.rs) ─────────────────────

    #[test]
    fn decrypt_roundtrip() {
        let key = crypto::generate_key();
        let password = "jmap-test-password";
        let encrypted = crypto::encrypt(&key, password.as_bytes()).unwrap();
        let json = serde_json::to_string(&encrypted).unwrap();
        let decrypted = decrypt_account_password(&json, &key).unwrap();
        assert_eq!(decrypted, password);
    }

    // ── crate → DTO mapping ─────────────────────────────────────────

    #[test]
    fn map_email_maps_keywords_thread_body_and_addresses() {
        let crate_email: Email<Get> = serde_json::from_value(serde_json::json!({
            "id": "em1",
            "threadId": "th1",
            "mailboxIds": { "mb1": true },
            "keywords": { "$seen": true },
            "size": 12345,
            "receivedAt": "2025-01-15T10:00:00Z",
            "messageId": ["<msg1@example.com>"],
            "from": [{ "name": "Alice", "email": "alice@example.com" }],
            "to": [{ "name": "Bob", "email": "bob@example.com" }],
            "subject": "Hello!",
            "preview": "Hi Bob, ...",
            "hasAttachment": false,
            "bodyValues": { "p1": { "value": "Hello world" } },
            "textBody": [{ "partId": "p1", "type": "text/plain" }],
            "htmlBody": []
        }))
        .unwrap();

        let mapped = map_email(&crate_email);
        assert_eq!(mapped.id, "em1");
        assert_eq!(mapped.thread_id.as_deref(), Some("th1"));
        assert!(mapped.is_seen());
        assert!(!mapped.is_flagged());
        assert_eq!(mapped.format_from().as_deref(), Some("Alice <alice@example.com>"));
        assert_eq!(mapped.to_string_list().as_deref(), Some("Bob <bob@example.com>"));
        assert_eq!(mapped.subject.as_deref(), Some("Hello!"));
        assert_eq!(mapped.body_text().as_deref(), Some("Hello world"));
        assert_eq!(mapped.body_html(), None);
        assert_eq!(mapped.received_at.as_deref(), Some("2025-01-15T10:00:00+00:00"));
        assert_eq!(mapped.message_id_header().as_deref(), Some("<msg1@example.com>"));
        assert_eq!(mapped.size, Some(12345));
    }

    #[test]
    fn map_email_empty_keywords_stay_absent() {
        let crate_email: Email<Get> =
            serde_json::from_value(serde_json::json!({ "id": "em2", "keywords": {} })).unwrap();
        let mapped = map_email(&crate_email);
        assert!(!mapped.is_seen());
        assert!(mapped.keywords.is_none());
    }

    #[test]
    fn map_mailbox_normalizes_junk_to_lyra_spam_role() {
        let junk: Mailbox<Get> = serde_json::from_value(serde_json::json!({
            "id": "mb-junk",
            "name": "Junk",
            "role": "junk",
            "totalEmails": 3
        }))
        .unwrap();
        let mapped = map_mailbox(&junk).unwrap();
        assert_eq!(mapped.id, "mb-junk");
        // Lyra's role vocabulary is spam (move_message_to_role queries "spam").
        assert_eq!(mapped.role.as_deref(), Some("spam"));
        assert_eq!(mapped.total_emails, Some(3));
    }

    #[test]
    fn map_mailbox_handles_missing_role_and_parent() {
        let mb: Mailbox<Get> = serde_json::from_value(serde_json::json!({
            "id": "mb2",
            "name": "Projects",
            "parentId": "mb1"
        }))
        .unwrap();
        let mapped = map_mailbox(&mb).unwrap();
        assert_eq!(mapped.role, None);
        assert_eq!(mapped.parent_id.as_deref(), Some("mb1"));
        // A mailbox without a server id is skipped, not persisted.
        let no_id: Mailbox<Get> =
            serde_json::from_value(serde_json::json!({ "name": "Ghost" })).unwrap();
        assert!(map_mailbox(&no_id).is_none());
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cd backend && cargo test --bin lyra_backend jmap_client 2>&1 | head -40`
Expected: FAIL to compile — errors like `unresolved import` / `cannot find type `JmapError` in this scope` naming the missing items. (The file is not yet declared in `sync/mod.rs`; declare `pub(crate) mod jmap_client;` first — alphabetical, between `mod jmap_loop;`… actually place it between `mod imap_loop;` and `mod jmap_loop;` — so the compiler reaches the test module and reports the missing items.)

- [ ] **Step 4: Implement the seam**

Write the full content of `backend/src/sync/jmap_client.rs` above the test module:

```rust
//! JMAP seam over the `jmap-client` crate (RFC 8620/8621).
//!
//! This is the ONLY module that imports `jmap_client`. Everything behind it
//! speaks Lyra's plain DTOs (`JmapMailbox`, `JmapEmail`, …) so `sync/store.rs`
//! persistence keeps its shape.
//!
//! Security: `/.well-known/jmap` redirects are pre-resolved with our own
//! same-origin follower; the crate client is then allowed to re-follow only
//! the configured host (the crate's allowlist is host-scoped — our follower
//! already validated the chain origin-scoped). Post-connect, every
//! credential-bearing session URL is pinned to the configured origin.
//!
//! See `docs/superpowers/specs/2026-08-29-lyra-jmap-full-support-design.md`.

#![allow(clippy::doc_markdown)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;

use jmap_client::client::{Client, Credentials};
use jmap_client::core::error::MethodErrorType;
use jmap_client::core::set::SetErrorType;
use jmap_client::email::{Email, EmailAddress, EmailBodyPart};
use jmap_client::mailbox::{Mailbox, Role};
use jmap_client::{Get, URI};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::crypto::{self, EncryptedCredential};

/// Whole-request timeout for JMAP API calls (matches the retired client).
const JMAP_TIMEOUT: Duration = Duration::from_secs(30);
/// Redirect hops accepted during well-known pre-resolution.
const MAX_DISCOVERY_HOPS: u32 = 5;

// ── Errors ──────────────────────────────────────────────────────────

/// Errors specific to the JMAP seam.
#[derive(Debug, Error)]
pub enum JmapError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("session discovery failed: {0}")]
    SessionDiscovery(String),
    #[error("authentication failed: {0}")]
    Authentication(String),
    /// Legacy wire-level method error (hand-rolled transport; pruned in Task 7).
    #[error("JMAP method error: {code} — {description}")]
    Method { code: String, description: String },
    #[error("crypto error: {0}")]
    Crypto(#[from] crypto::CryptoError),
    #[error("invalid response: {0}")]
    InvalidResponse(String),
    #[error("invalid server URL: {0}")]
    InvalidServerUrl(String),
    #[error("cross-origin URL rejected (credentials stay pinned): {0}")]
    CrossOrigin(String),
    /// Typed error from the `jmap-client` crate.
    #[error("JMAP protocol error: {0}")]
    Client(jmap_client::Error),
}

impl From<jmap_client::Error> for JmapError {
    fn from(err: jmap_client::Error) -> Self {
        // 401 surfaces as ProblemDetails (RFC 7807 body) or a bare status line.
        let is_auth = match &err {
            jmap_client::Error::Problem(p) => p.status() == Some(401),
            jmap_client::Error::Server(s) => s.starts_with("401"),
            _ => false,
        };
        if is_auth {
            Self::Authentication("JMAP server rejected credentials (HTTP 401)".into())
        } else {
            Self::Client(err)
        }
    }
}

impl JmapError {
    /// `Email/queryChanges` / `Email/changes` cannot resume from the stored
    /// token; the caller clears the cursor and full-queries.
    #[must_use]
    pub fn is_stale_query_state(&self) -> bool {
        match self {
            Self::Method { code, .. } => {
                code.eq_ignore_ascii_case("cannotCalculateChanges")
                    || code.eq_ignore_ascii_case("cannotCalculateChangesFrom")
            }
            Self::Client(jmap_client::Error::Method(m)) => {
                m.error() == &MethodErrorType::CannotCalculateChanges
            }
            _ => false,
        }
    }

    /// Transient failure worth retrying with backoff: transport/timeout,
    /// 5xx/429, `serverUnavailable`/`serverPartialFail`/`tooManyChanges`
    /// method errors, `rateLimit`/`overQuota` set errors.
    #[must_use]
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Http(_) => true,
            Self::Client(err) => match err {
                jmap_client::Error::Transport(_) => true,
                jmap_client::Error::Server(status) => {
                    status.starts_with("429") || status.starts_with('5')
                }
                jmap_client::Error::Problem(p) => {
                    p.status().is_some_and(|s| s == 429 || s >= 500)
                }
                jmap_client::Error::Method(m) => matches!(
                    m.error(),
                    MethodErrorType::ServerUnavailable
                        | MethodErrorType::ServerPartialFail
                        | MethodErrorType::TooManyChanges
                ),
                jmap_client::Error::Set(s) => {
                    matches!(s.error(), SetErrorType::RateLimit | SetErrorType::OverQuota)
                }
                _ => false,
            },
            _ => false,
        }
    }

    /// Authentication/authorization failure; callers evict the cached session.
    #[must_use]
    pub fn is_auth(&self) -> bool {
        match self {
            Self::Authentication(_) => true,
            Self::Client(jmap_client::Error::Problem(p)) => {
                matches!(p.status(), Some(401) | Some(403))
            }
            Self::Client(jmap_client::Error::Server(s)) => {
                s.starts_with("401") || s.starts_with("403")
            }
            _ => false,
        }
    }
}

// ── Lyra DTOs (persistence boundary; moved from jmap.rs) ────────────

/// A JMAP Mailbox object.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JmapMailbox {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub total_emails: Option<u64>,
    #[serde(default)]
    pub unread_emails: Option<u64>,
    #[serde(default)]
    pub sort_order: Option<u32>,
}

/// A JMAP Email object (partial, only the fields we need).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JmapEmail {
    pub id: String,
    #[serde(default)]
    pub blob_id: Option<String>,
    #[serde(default)]
    pub thread_id: Option<String>,
    #[serde(default)]
    pub mailbox_ids: Option<serde_json::Value>,
    #[serde(default)]
    pub keywords: Option<serde_json::Value>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub received_at: Option<String>,
    #[serde(default)]
    pub message_id: Option<Vec<String>>,
    #[serde(default)]
    pub in_reply_to: Option<Vec<String>>,
    #[serde(default)]
    pub references: Option<Vec<String>>,
    #[serde(default)]
    pub sender: Option<Vec<JmapEmailAddress>>,
    #[serde(default)]
    pub from: Option<Vec<JmapEmailAddress>>,
    #[serde(default)]
    pub to: Option<Vec<JmapEmailAddress>>,
    #[serde(default)]
    pub cc: Option<Vec<JmapEmailAddress>>,
    #[serde(default)]
    pub bcc: Option<Vec<JmapEmailAddress>>,
    #[serde(default)]
    pub reply_to: Option<Vec<JmapEmailAddress>>,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub body_structure: Option<serde_json::Value>,
    #[serde(default)]
    pub body_values: Option<serde_json::Map<String, serde_json::Value>>,
    #[serde(default)]
    pub text_body: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    pub html_body: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    pub has_attachment: Option<bool>,
    #[serde(default)]
    pub attachments: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    pub preview: Option<String>,
}

/// A JMAP email address.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct JmapEmailAddress {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
}

impl JmapEmail {
    /// Extract the plain-text body from bodyValues.
    pub fn body_text(&self) -> Option<String> {
        extract_body_part(self, "text/plain")
    }

    /// Extract the HTML body from bodyValues (unsanitized; persist via `persist_body_html`).
    pub fn body_html(&self) -> Option<String> {
        extract_body_part(self, "text/html")
    }

    /// Get the `from` address as a formatted string.
    pub fn format_from(&self) -> Option<String> {
        self.from
            .as_ref()
            .and_then(|addrs| addrs.first())
            .map(|a| match (&a.name, &a.email) {
                (Some(name), Some(email)) => format!("{name} <{email}>"),
                (None, Some(email)) => email.clone(),
                (Some(name), None) => name.clone(),
                _ => String::new(),
            })
    }

    /// Get the `to` addresses as a formatted string.
    pub fn to_string_list(&self) -> Option<String> {
        self.to.as_ref().map(|addrs| {
            addrs
                .iter()
                .map(|a| match (&a.name, &a.email) {
                    (Some(name), Some(email)) => format!("{name} <{email}>"),
                    (None, Some(email)) => email.clone(),
                    (Some(name), None) => name.clone(),
                    _ => String::new(),
                })
                .collect::<Vec<_>>()
                .join(", ")
        })
    }

    /// Get the first Message-ID header value.
    pub fn message_id_header(&self) -> Option<String> {
        self.message_id
            .as_ref()
            .and_then(|ids| ids.first().cloned())
    }

    /// Whether the email has the `$seen` keyword (read).
    pub fn is_seen(&self) -> bool {
        self.keywords
            .as_ref()
            .and_then(|k| k.get("$seen"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    }

    /// Whether the email has the `$flagged` keyword (starred).
    pub fn is_flagged(&self) -> bool {
        self.keywords
            .as_ref()
            .and_then(|k| k.get("$flagged"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    }
}

/// Extract body text from `bodyValues` using the part type.
fn extract_body_part(email: &JmapEmail, content_type: &str) -> Option<String> {
    let body_parts = if content_type == "text/plain" {
        email.text_body.as_ref()?
    } else {
        email.html_body.as_ref()?
    };

    let part_id = body_parts.first()?.get("partId")?.as_str()?;
    let body_values = email.body_values.as_ref()?;
    let value = body_values.get(part_id)?;
    value.get("value")?.as_str().map(String::from)
}

/// Decrypt the stored credential for a JMAP account (password or Bearer token).
pub fn decrypt_account_password(credential_json: &str, dek: &[u8]) -> Result<String, JmapError> {
    let encrypted: EncryptedCredential = serde_json::from_str(credential_json)
        .map_err(|e| JmapError::InvalidResponse(format!("invalid credential blob: {e}")))?;

    let plaintext = crypto::decrypt(dek, &encrypted)?;

    String::from_utf8(plaintext)
        .map_err(|e| JmapError::InvalidResponse(format!("credential not valid UTF-8: {e}")))
}

// ── Discovery security ──────────────────────────────────────────────

/// Resolve one discovery redirect hop: `location` (possibly relative) against
/// the URL that produced it, rejecting any hop that leaves `origin`.
///
/// Pure decision function so the credential-pinning policy is unit-testable
/// without a network.
pub(crate) fn resolve_discovery_redirect(
    current_url: &str,
    location: &str,
    origin: &str,
) -> Result<String, JmapError> {
    let base = reqwest::Url::parse(current_url).map_err(|e| {
        JmapError::SessionDiscovery(format!("invalid current URL '{current_url}': {e}"))
    })?;
    let resolved = base.join(location).map_err(|e| {
        JmapError::SessionDiscovery(format!("invalid redirect target '{location}': {e}"))
    })?;
    let resolved = resolved.as_str().to_string();
    let target_origin = crate::netsec::origin_of(&resolved).map_err(JmapError::InvalidServerUrl)?;
    if target_origin != origin {
        tracing::warn!(
            target_origin = %target_origin,
            expected_origin = %origin,
            "JMAP: discovery redirect leaves the configured origin; refusing to follow"
        );
        return Err(JmapError::CrossOrigin(resolved));
    }
    Ok(resolved)
}

/// Pin every credential-bearing session URL to the configured origin.
///
/// `urls` are the session's `apiUrl` / `uploadUrl` / `downloadUrl` /
/// `eventSourceUrl`; empty entries are skipped defensively.
fn pin_session_urls(origin: &str, urls: &[&str]) -> Result<(), JmapError> {
    for url in urls.iter().filter(|u| !u.is_empty()) {
        let target = crate::netsec::origin_of(url).map_err(JmapError::InvalidServerUrl)?;
        if target != origin {
            tracing::warn!(
                target_origin = %target,
                expected_origin = %origin,
                "JMAP: session URL points at a different origin; refusing to send credentials"
            );
            return Err(JmapError::CrossOrigin((*url).to_owned()));
        }
    }
    Ok(())
}

/// `auth_type = "bearer"` (e.g. Fastmail API tokens) selects Bearer; anything
/// else is Basic with the account password.
fn credentials_for(auth_type: &str, email: &str, secret: &str) -> Credentials {
    if auth_type.eq_ignore_ascii_case("bearer") {
        Credentials::bearer(secret)
    } else {
        Credentials::basic(email, secret)
    }
}

/// Authorization header value for our own pre-resolution GETs. `Credentials`
/// stores the Basic value pre-encoded (jmap-client `src/client.rs`).
fn authorization_header(credentials: &Credentials) -> String {
    match credentials {
        Credentials::Basic(encoded) => format!("Basic {encoded}"),
        Credentials::Bearer(token) => format!("Bearer {token}"),
    }
}

/// Pre-validate the `/.well-known/jmap` redirect chain with our own
/// same-origin follower (the crate's `connect()` re-fetches the session
/// itself afterwards). Returns whether any redirect hop occurred.
///
/// Non-2xx final statuses are *not* an error here: `connect()` produces the
/// typed 401/problem error for them. Only the chain's shape matters.
async fn preflight_discovery(
    base: &str,
    auth_header: &str,
    origin: &str,
) -> Result<bool, JmapError> {
    let http = reqwest::Client::builder()
        .timeout(JMAP_TIMEOUT)
        // Automatic following stays disabled: every hop is checked below.
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let mut url = format!("{base}/.well-known/jmap");
    let mut redirected = false;
    for _hop in 0..MAX_DISCOVERY_HOPS {
        let resp = http
            .get(&url)
            .header(reqwest::header::AUTHORIZATION, auth_header)
            .send()
            .await?;
        if !resp.status().is_redirection() {
            return Ok(redirected);
        }
        redirected = true;
        let location = resp
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| {
                JmapError::SessionDiscovery(format!("redirect from {url} has no Location"))
            })?;
        url = resolve_discovery_redirect(&url, location, origin)?;
    }
    Err(JmapError::SessionDiscovery(format!(
        "too many redirects from {base}/.well-known/jmap"
    )))
}

// ── The seam client ─────────────────────────────────────────────────

/// A connected JMAP session pinned to its configured origin.
pub(crate) struct JmapSeam {
    client: Client,
    origin: String,
}

impl JmapSeam {
    /// Discover + connect (no caching). Pre-resolves well-known redirects
    /// with the same-origin follower, then pins the session URLs.
    async fn connect(
        base_url: &str,
        email: &str,
        secret: &str,
        auth_type: &str,
    ) -> Result<Self, JmapError> {
        crate::netsec::validate_server_url(base_url).map_err(JmapError::InvalidServerUrl)?;
        let trimmed = base_url.trim_end_matches('/');
        let base = trimmed.strip_suffix("/.well-known/jmap").unwrap_or(trimmed);
        let origin = crate::netsec::origin_of(base).map_err(JmapError::InvalidServerUrl)?;
        let host = reqwest::Url::parse(base)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned))
            .ok_or_else(|| JmapError::InvalidServerUrl(format!("no host in '{base}'")))?;

        let credentials = credentials_for(auth_type, email, secret);
        let redirected =
            preflight_discovery(base, &authorization_header(&credentials), &origin).await?;

        let mut builder = Client::new().credentials(credentials).timeout(JMAP_TIMEOUT);
        if redirected {
            // The crate's redirect policy is host-scoped and denies all hosts
            // by default (`Client::redirect_policy`). Our follower already
            // validated this exact chain origin-scoped; allow precisely the
            // configured host so connect() can re-follow it. The allowlist
            // stays empty for non-redirecting servers.
            builder = builder.follow_redirects([host]);
        }
        let mut client = builder.connect(base).await?;

        let session = client.session();
        pin_session_urls(
            &origin,
            &[
                session.api_url(),
                session.upload_url(),
                session.download_url(),
                session.event_source_url(),
            ],
        )?;
        // `connect()` defaults to an arbitrary first primary account (hash-map
        // order); pin the *mail* primary account instead.
        let mail_account = session
            .primary_accounts()
            .find(|(uri, _)| uri.as_str() == URI::Mail.as_ref())
            .map(|(_, id)| id.clone())
            .or_else(|| session.accounts().next().cloned())
            .ok_or_else(|| JmapError::SessionDiscovery("no mail account in JMAP session".into()))?;
        client.set_default_account_id(mail_account);

        Ok(Self { client, origin })
    }

    /// Cached connect for a Lyra account: one session per account per process.
    pub(crate) async fn connect_for_account(
        account_id: &str,
        base_url: &str,
        email: &str,
        secret: &str,
        auth_type: &str,
    ) -> Result<Arc<Self>, JmapError> {
        if let Some(seam) = lock_cache().get(account_id) {
            return Ok(Arc::clone(seam));
        }
        let seam = Arc::new(Self::connect(base_url, email, secret, auth_type).await?);
        lock_cache().insert(account_id.to_owned(), Arc::clone(&seam));
        Ok(seam)
    }

    /// Uncached connect (account probe before the account row exists).
    pub(crate) async fn connect_ephemeral(
        base_url: &str,
        email: &str,
        secret: &str,
        auth_type: &str,
    ) -> Result<Self, JmapError> {
        Self::connect(base_url, email, secret, auth_type).await
    }

    /// Drop the cached session (auth failure, credential change, delete).
    pub(crate) fn evict(account_id: &str) {
        lock_cache().remove(account_id);
    }

    /// Refresh the session when a response reported a newer `sessionState`;
    /// the refreshed URLs are pinned again.
    pub(crate) async fn refresh_if_stale(&self) -> Result<(), JmapError> {
        if !self.client.is_session_updated() {
            self.client.refresh_session().await?;
            let session = self.client.session();
            pin_session_urls(
                &self.origin,
                &[
                    session.api_url(),
                    session.upload_url(),
                    session.download_url(),
                    session.event_source_url(),
                ],
            )?;
        }
        Ok(())
    }

    /// True when `urn:ietf:params:jmap:submission` is advertised (RFC 8621).
    pub(crate) fn supports_submission(&self) -> bool {
        self.client.session().has_capability(URI::Submission.as_ref())
    }
}

static CLIENT_CACHE: OnceLock<Mutex<HashMap<String, Arc<JmapSeam>>>> = OnceLock::new();

fn lock_cache() -> MutexGuard<'static, HashMap<String, Arc<JmapSeam>>> {
    CLIENT_CACHE
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

// ── Crate → DTO mapping ─────────────────────────────────────────────

/// Map a crate `Email<Get>` onto the Lyra DTO that `sync/store.rs` persists.
///
/// `keywords`/`mailbox_ids` keep the old DTO shape (JSON object with `true`
/// values); the crate exposes only set keys, which is the same information.
fn map_email(email: &Email<Get>) -> JmapEmail {
    let keywords = email.keywords();
    let keywords = if keywords.is_empty() {
        None
    } else {
        Some(serde_json::Value::Object(
            keywords
                .into_iter()
                .map(|k| (k.to_owned(), serde_json::Value::Bool(true)))
                .collect(),
        ))
    };
    let mailbox_ids = email.mailbox_ids();
    let mailbox_ids = if mailbox_ids.is_empty() {
        None
    } else {
        Some(serde_json::Value::Object(
            mailbox_ids
                .into_iter()
                .map(|m| (m.to_owned(), serde_json::Value::Bool(true)))
                .collect(),
        ))
    };
    JmapEmail {
        id: email.id().unwrap_or_default().to_owned(),
        blob_id: email.blob_id().map(str::to_owned),
        thread_id: email.thread_id().map(str::to_owned),
        mailbox_ids,
        keywords,
        size: Some(email.size() as u64),
        received_at: email
            .received_at()
            .and_then(chrono::DateTime::from_timestamp(_, 0))
            .map(|dt| dt.to_rfc3339()),
        message_id: email.message_id().map(<[String]>::to_vec),
        in_reply_to: email.in_reply_to().map(<[String]>::to_vec),
        references: email.references().map(<[String]>::to_vec),
        sender: email.sender().map(map_addresses),
        from: email.from().map(map_addresses),
        to: email.to().map(map_addresses),
        cc: email.cc().map(map_addresses),
        bcc: email.bcc().map(map_addresses),
        reply_to: email.reply_to().map(map_addresses),
        subject: email.subject().map(str::to_owned),
        body_structure: None, // never read by store.rs
        body_values: map_body_values(email),
        text_body: map_body_refs(email.text_body()),
        html_body: map_body_refs(email.html_body()),
        has_attachment: Some(email.has_attachment()),
        attachments: None, // superseded by typed attachment meta (Task 3)
        preview: email.preview().map(str::to_owned),
    }
}

fn map_addresses(addrs: &[EmailAddress]) -> Vec<JmapEmailAddress> {
    addrs
        .iter()
        .map(|a| JmapEmailAddress {
            name: a.name().map(str::to_owned),
            email: Some(a.email().to_owned()),
        })
        .collect()
}

/// Rebuild the `bodyValues` JSON map (`partId → {value, isTruncated}`) that
/// `extract_body_part` reads, from the crate's keyed accessor.
fn map_body_values(email: &Email<Get>) -> Option<serde_json::Map<String, serde_json::Value>> {
    let mut map = serde_json::Map::new();
    for part in email
        .text_body()
        .into_iter()
        .flatten()
        .chain(email.html_body().into_iter().flatten())
    {
        if let Some(part_id) = part.part_id()
            && let Some(value) = email.body_value(part_id)
        {
            map.insert(
                part_id.to_owned(),
                serde_json::json!({ "value": value.value(), "isTruncated": value.is_truncated() }),
            );
        }
    }
    if map.is_empty() { None } else { Some(map) }
}

/// `textBody`/`htmlBody` as `[{partId, type}]` JSON (the DTO's wire shape).
fn map_body_refs(parts: Option<&[EmailBodyPart]>) -> Option<Vec<serde_json::Value>> {
    parts.map(|ps| {
        ps.iter()
            .map(|p| serde_json::json!({ "partId": p.part_id(), "type": p.content_type() }))
            .collect()
    })
}

/// Map a crate `Mailbox<Get>`; `None` when the server row has no id.
/// `Role::Junk` normalizes to Lyra's `"spam"` vocabulary.
fn map_mailbox(mb: &Mailbox<Get>) -> Option<JmapMailbox> {
    let id = mb.id()?.to_owned();
    let role = match mb.role() {
        Role::Inbox => Some("inbox".into()),
        Role::Sent => Some("sent".into()),
        Role::Trash => Some("trash".into()),
        Role::Drafts => Some("drafts".into()),
        Role::Junk => Some("spam".into()),
        Role::Archive => Some("archive".into()),
        Role::Important => Some("important".into()),
        Role::Other(other) => Some(other.clone()),
        Role::None => None,
    };
    Some(JmapMailbox {
        id,
        name: mb.name().unwrap_or_default().to_owned(),
        role,
        parent_id: mb.parent_id().map(str::to_owned),
        total_emails: Some(mb.total_emails() as u64),
        unread_emails: Some(mb.unread_emails() as u64),
        sort_order: Some(mb.sort_order()),
    })
}
```

- [ ] **Step 5: Shrink `jmap.rs` to old client + re-exports**

In `backend/src/jmap.rs`:

- Delete the moved items: the `JmapError` enum and its `impl` (including `is_stale_query_state`), `JmapMailbox`, `JmapEmail` + its `impl`, `JmapEmailAddress`, `decrypt_account_password`, `resolve_discovery_redirect`, and the helper `extract_body_part`.
- Delete the moved tests: `discovery_redirect_*` (5), `decrypt_roundtrip`, `parse_jmap_mailbox`, `parse_mailbox_with_null_role`, `parse_jmap_email`, `jmap_email_body_extraction`, `jmap_email_seen_flagged`, `stale_query_state_detects_rfc_code`.
- Keep everything else (the old `JmapClient` and its methods, `JmapSession`, `JmapRequest`/`JmapResponse`, `EmailQueryResult`, `EmailQueryChanges`, `EventSourceOutcome`, `JmapIdentity`, `JmapSyncState`, `check_session_urls`, `take_ok_args*`, `jmap_set_error`, `pick_identity`, `mailbox_id_for_role`, `build_email_create`, `jmap_address`, `expand_event_source_url`, `sse_frame_is_state_push`, `probe_jmap`, and the remaining tests).
- Replace the deleted items with re-exports so every existing path keeps compiling. Near the top of `jmap.rs` (after the `use` block):

```rust
pub use crate::sync::jmap_client::{
    JmapEmail, JmapEmailAddress, JmapError, JmapMailbox, decrypt_account_password,
};
use crate::sync::jmap_client::resolve_discovery_redirect;
```

- Remove now-duplicate imports from `jmap.rs`'s `use` block if the compiler flags them (`EncryptedCredential`/`crypto` are still used by the old client — check by compiler error, not by guessing).

Run: `cd backend && cargo check`
Expected: `Finished` (green). `sync/types.rs`, `jobs.rs`, `jmap_push.rs`, `accounts.rs`, `sync/store.rs` still resolve `crate::jmap::…` via the re-exports.

- [ ] **Step 6: Run the seam tests**

Run: `cd backend && cargo test --bin lyra_backend jmap`
Expected: `test result: ok` — all new `sync::jmap_client::tests::*` pass plus the retained `jmap::tests::*`.

- [ ] **Step 7: Format + lint the changed files**

Run:
```bash
cd backend && rustfmt --edition 2024 src/sync/jmap_client.rs src/jmap.rs src/sync/mod.rs && cargo fmt --check
cd backend && cargo clippy --all-targets --all-features 2>&1 | grep "warning:" | grep -v "oauth/config.rs"
```
Expected: `cargo fmt --check` clean; grep output empty.

- [ ] **Step 8: Run the full test suite**

Run: `cd backend && cargo test --bin lyra_backend 2>&1 | tail -20`
Expected: `test result: ok` (or only the 3 pre-existing gpg-interop failures — see Conventions).

- [ ] **Step 9: Commit**

```bash
git add backend/Cargo.toml backend/Cargo.lock backend/src/sync/jmap_client.rs backend/src/sync/mod.rs backend/src/jmap.rs
git commit -m "feat: add jmap-client seam with Bearer auth, session cache, origin pinning"
```

---

### Task 2: Rewrite the JMAP sync loop on the seam

Commit: `feat: rewrite JMAP sync loop on seam (removed ids, Email/changes, thread_id)`

**Files:**
- Create: `backend/migrations/sqlite/0014_jmap_thread_id.up.sql`, `backend/migrations/sqlite/0014_jmap_thread_id.down.sql`, `backend/migrations/postgres/0014_jmap_thread_id.up.sql`, `backend/migrations/postgres/0014_jmap_thread_id.down.sql`
- Modify: `backend/src/entities/message.rs`
- Modify: `backend/src/sync/store.rs`
- Modify: `backend/src/sync/jmap_client.rs` (sync methods)
- Modify: `backend/src/sync/jmap_loop.rs` (rewrite)
- Modify: `backend/src/jmap.rs` (delete orphaned query/get methods)
- Modify: `backend/src/sync/mod.rs` (new store tests)

- [ ] **Step 1: Migration 0014 + entity field**

Create `backend/migrations/sqlite/0014_jmap_thread_id.up.sql`:

```sql
-- JMAP threadId: server-opaque string, no FK (thread.id is a local UUID).
ALTER TABLE message ADD COLUMN jmap_thread_id TEXT;
```

Create `backend/migrations/sqlite/0014_jmap_thread_id.down.sql`:

```sql
ALTER TABLE message DROP COLUMN jmap_thread_id;
```

Create `backend/migrations/postgres/0014_jmap_thread_id.up.sql`:

```sql
-- JMAP threadId: server-opaque string, no FK (thread.id is a local UUID).
ALTER TABLE message ADD COLUMN jmap_thread_id TEXT;
```

Create `backend/migrations/postgres/0014_jmap_thread_id.down.sql`:

```sql
ALTER TABLE message DROP COLUMN jmap_thread_id;
```

In `backend/src/entities/message.rs`, after the `thread_id` field, add:

```rust
    /// JMAP `threadId` (server-opaque string; no FK — `thread.id` is a local UUID).
    pub jmap_thread_id: Option<String>,
```

Run: `cd backend && cargo check`
Expected: `Finished` (the new `message::Column::JmapThreadId` variant now exists).

- [ ] **Step 2: Write the failing store tests**

Add to `backend/src/sync/mod.rs`'s `mod tests` (helpers `test_pool`/`seed_user_and_account`/`as_db` already exist there):

```rust
    fn sample_jmap_email(id: &str, thread: Option<&str>) -> super::jmap_client::JmapEmail {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "threadId": thread,
            "subject": format!("Message {id}"),
            "keywords": {},
            "receivedAt": "2026-08-29T10:00:00Z"
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn jmap_upsert_persists_thread_and_folder_moves() {
        let pool = test_pool().await;
        let (_, account_id) = seed_user_and_account(&pool).await;
        upsert_folder(&as_db(&pool), &account_id, "INBOX", Some("/"), &[])
            .await
            .unwrap();
        upsert_folder(&as_db(&pool), &account_id, "Archive", Some("/"), &[])
            .await
            .unwrap();
        let inbox = get_folder_id(&as_db(&pool), &account_id, "INBOX").await.unwrap();
        let archive = get_folder_id(&as_db(&pool), &account_id, "Archive").await.unwrap();

        super::store::persist_jmap_folder_batch(
            &as_db(&pool),
            &account_id,
            &inbox,
            &[sample_jmap_email("em1", Some("th1"))],
            None,
        )
        .await
        .unwrap();
        let row: (Option<String>, String) = sqlx::query_as(
            "SELECT jmap_thread_id, folder_id FROM message WHERE account_id = ? AND external_id = ?",
        )
        .bind(&account_id)
        .bind("em1")
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.0.as_deref(), Some("th1"));
        assert_eq!(row.1, inbox);

        // Re-upsert under another folder = server-side move re-homes the row.
        super::store::persist_jmap_folder_batch(
            &as_db(&pool),
            &account_id,
            &archive,
            &[sample_jmap_email("em1", Some("th1"))],
            None,
        )
        .await
        .unwrap();
        let row: (String,) = sqlx::query_as(
            "SELECT folder_id FROM message WHERE account_id = ? AND external_id = ?",
        )
        .bind(&account_id)
        .bind("em1")
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.0, archive);
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM message WHERE account_id = ? AND external_id = ?",
        )
        .bind(&account_id)
        .bind("em1")
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 1, "move must not duplicate the row");
    }

    #[tokio::test]
    async fn jmap_delete_by_external_ids_removes_rows_and_updates_counts() {
        let pool = test_pool().await;
        let (_, account_id) = seed_user_and_account(&pool).await;
        upsert_folder(&as_db(&pool), &account_id, "INBOX", Some("/"), &[])
            .await
            .unwrap();
        let inbox = get_folder_id(&as_db(&pool), &account_id, "INBOX").await.unwrap();
        super::store::persist_jmap_folder_batch(
            &as_db(&pool),
            &account_id,
            &inbox,
            &[
                sample_jmap_email("em1", None),
                sample_jmap_email("em2", None),
                sample_jmap_email("em3", None),
            ],
            None,
        )
        .await
        .unwrap();

        let deleted = super::store::delete_jmap_messages_by_external_ids(
            &as_db(&pool),
            &account_id,
            &["em2".to_owned(), "emX".to_owned()],
        )
        .await
        .unwrap();
        assert_eq!(deleted, 1, "unknown ids are skipped");

        let remaining: Vec<String> = sqlx::query_scalar(
            "SELECT external_id FROM message WHERE account_id = ? ORDER BY external_id",
        )
        .bind(&account_id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(remaining, vec!["em1".to_owned(), "em3".to_owned()]);

        let total: i64 = sqlx::query_scalar("SELECT total_messages FROM folder WHERE id = ?")
            .bind(&inbox)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(total, 2);
    }

    #[tokio::test]
    async fn jmap_email_state_cursor_roundtrip() {
        let pool = test_pool().await;
        let (_, account_id) = seed_user_and_account(&pool).await;
        upsert_folder(&as_db(&pool), &account_id, "INBOX", Some("/"), &[])
            .await
            .unwrap();
        let inbox = get_folder_id(&as_db(&pool), &account_id, "INBOX").await.unwrap();

        assert!(
            super::store::load_jmap_email_state(&as_db(&pool), &account_id, &inbox)
                .await
                .unwrap()
                .is_none()
        );
        super::store::save_jmap_email_state(&as_db(&pool), &account_id, &inbox, "email-state-1")
            .await
            .unwrap();
        assert_eq!(
            super::store::load_jmap_email_state(&as_db(&pool), &account_id, &inbox)
                .await
                .unwrap()
                .as_deref(),
            Some("email-state-1")
        );
        super::store::save_jmap_email_state(&as_db(&pool), &account_id, &inbox, "email-state-2")
            .await
            .unwrap();
        assert_eq!(
            super::store::load_jmap_email_state(&as_db(&pool), &account_id, &inbox)
                .await
                .unwrap()
                .as_deref(),
            Some("email-state-2")
        );
        super::store::clear_jmap_email_state(&as_db(&pool), &account_id, &inbox)
            .await
            .unwrap();
        assert!(
            super::store::load_jmap_email_state(&as_db(&pool), &account_id, &inbox)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn folder_id_for_role_finds_inbox() {
        let pool = test_pool().await;
        let (_, account_id) = seed_user_and_account(&pool).await;
        upsert_folder(&as_db(&pool), &account_id, "INBOX", Some("/"), &[])
            .await
            .unwrap();
        let inbox = get_folder_id(&as_db(&pool), &account_id, "INBOX").await.unwrap();
        assert_eq!(
            super::store::folder_id_for_role(&as_db(&pool), &account_id, "inbox")
                .await
                .unwrap()
                .as_deref(),
            Some(inbox.as_str())
        );
        assert!(
            super::store::folder_id_for_role(&as_db(&pool), &account_id, "trash")
                .await
                .unwrap()
                .is_none()
        );
    }
```

Also add the failing loop test — append to `backend/src/sync/jmap_loop.rs` (the file has no test module yet):

```rust
#[cfg(test)]
mod tests {
    use super::plan_deletions;
    use std::collections::HashSet;

    #[test]
    fn plan_deletions_drops_refetched_ids() {
        let refetched: HashSet<String> = ["b".to_owned(), "d".to_owned()].into_iter().collect();
        let removed = vec!["a".to_owned(), "b".to_owned(), "c".to_owned()];
        assert_eq!(plan_deletions(removed, &refetched), vec!["a".to_owned(), "c".to_owned()]);
    }

    #[test]
    fn plan_deletions_empty_when_all_refetched() {
        let refetched: HashSet<String> = ["a".to_owned()].into_iter().collect();
        assert!(plan_deletions(vec!["a".to_owned()], &refetched).is_empty());
    }
}
```

And the failing seam request-shape tests — append to `backend/src/sync/jmap_client.rs`'s `mod tests`:

```rust
    // ── request wire shapes (Task 2) ────────────────────────────────

    #[test]
    fn email_query_serializes_rfc_shape() {
        let mut q = QueryRequest::<Email<Set>>::new(RequestParams::new("acc", Method::QueryEmail, 0));
        fill_email_query(&mut q, "mb1", 0, 100);
        let json = serde_json::to_value(&q).unwrap();
        assert_eq!(json["accountId"], "acc");
        assert_eq!(json["filter"]["inMailbox"], "mb1");
        assert_eq!(json["sort"][0]["property"], "receivedAt");
        assert_eq!(json["sort"][0]["isAscending"], false);
        assert_eq!(json["position"], 0);
        assert_eq!(json["limit"], 100);
        assert_eq!(json["calculateTotal"], true);
    }

    #[test]
    fn email_get_serializes_properties_and_body_flags() {
        let mut g = GetRequest::<Email<Set>>::new(RequestParams::new("acc", Method::GetEmail, 0));
        g.ids(["em1"]);
        fill_email_get(&mut g);
        let json = serde_json::to_value(&g).unwrap();
        assert_eq!(json["fetchTextBodyValues"], true);
        assert_eq!(json["fetchHTMLBodyValues"], true);
        let props = json["properties"].as_array().unwrap();
        assert!(props.contains(&serde_json::json!("threadId")));
        assert!(props.contains(&serde_json::json!("mailboxIds")));
        assert!(props.contains(&serde_json::json!("keywords")));
        assert!(props.contains(&serde_json::json!("attachments")));
    }
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cd backend && cargo test --bin lyra_backend jmap 2>&1 | grep -E "^error" | head -20`
Expected: compile errors naming the missing items (`fill_email_query`, `fill_email_get`, `delete_jmap_messages_by_external_ids`, `load_jmap_email_state`, …; `plan_deletions`).

- [ ] **Step 4: Implement the store changes**

In `backend/src/sync/store.rs`:

1. Generalize the JMAP cursor save (keep `save_jmap_cursor_in_tx`'s signature; delegate):

```rust
pub(crate) async fn save_jmap_cursor_in_tx(
    tx: &mut DbTxn,
    db: &DbPool,
    account_id: &str,
    folder_id: &str,
    query_state: &str,
) -> Result<(), SyncError> {
    save_state_cursor_in_tx(tx, db, account_id, folder_id, "state_token", query_state).await
}

/// Upsert a JMAP state cursor of `cursor_type` (`state_token` per folder,
/// `email_state` account-level). Opaque server tokens, stored verbatim.
async fn save_state_cursor_in_tx(
    tx: &mut DbTxn,
    db: &DbPool,
    account_id: &str,
    folder_id: &str,
    cursor_type: &str,
    cursor_value: &str,
) -> Result<(), SyncError> {
    let mut ins = Sq::insert();
    ins.into_table(sync_cursor::Entity)
        .columns([
            sync_cursor::Column::Id,
            sync_cursor::Column::AccountId,
            sync_cursor::Column::FolderId,
            sync_cursor::Column::Protocol,
            sync_cursor::Column::CursorType,
            sync_cursor::Column::CursorValue,
            sync_cursor::Column::UpdatedAt,
        ])
        .values_panic([
            Expr::val(new_uuid_text()),
            Expr::val(id_value(db, account_id)?),
            Expr::val(id_value(db, folder_id)?),
            Expr::val("jmap"),
            Expr::val(cursor_type),
            Expr::val(cursor_value),
            Expr::current_timestamp(),
        ])
        .on_conflict(
            OnConflict::columns([
                sync_cursor::Column::AccountId,
                sync_cursor::Column::FolderId,
                sync_cursor::Column::CursorType,
            ])
            .update_columns([
                sync_cursor::Column::CursorValue,
                sync_cursor::Column::UpdatedAt,
            ])
            .to_owned(),
        );
    tx_execute(tx, &ins).await?;
    Ok(())
}
```

(The body is the old `save_jmap_cursor_in_tx` verbatim with `cursor_type`/`cursor_value` parameters.)

2. Generalize clear the same way — replace `clear_jmap_cursor`'s body with a delegating pair:

```rust
pub(crate) async fn clear_jmap_cursor(
    db: &DbPool,
    account_id: &str,
    folder_id: &str,
) -> Result<(), SyncError> {
    clear_state_cursor(db, account_id, folder_id, "state_token").await
}

async fn clear_state_cursor(
    db: &DbPool,
    account_id: &str,
    folder_id: &str,
    cursor_type: &str,
) -> Result<(), SyncError> {
    let mut del = Sq::delete();
    del.from_table(sync_cursor::Entity)
        .and_where(sync_cursor::Column::AccountId.eq(id_value(db, account_id)?))
        .and_where(sync_cursor::Column::FolderId.eq(id_value(db, folder_id)?))
        .and_where(sync_cursor::Column::CursorType.eq(cursor_type));
    db.orm().execute(&del).await.map_err(orm_err)?;
    Ok(())
}
```

3. Add the account-level `email_state` cursor accessors and folder lookups:

```rust
/// Load the account-level `Email/changes` state.
///
/// `Email/changes` state is account-scoped, but `sync_cursor.folder_id` is
/// NOT NULL with a folder FK — the row anchors on the account's inbox folder.
pub(crate) async fn load_jmap_email_state(
    db: &DbPool,
    account_id: &str,
    anchor_folder_id: &str,
) -> Result<Option<String>, SyncError> {
    load_cursor_value(db, account_id, anchor_folder_id, "email_state").await
}

pub(crate) async fn save_jmap_email_state(
    db: &DbPool,
    account_id: &str,
    anchor_folder_id: &str,
    state: &str,
) -> Result<(), SyncError> {
    let mut tx = db.begin().await?;
    save_state_cursor_in_tx(tx, db, account_id, anchor_folder_id, "email_state", state).await?;
    tx.commit().await?;
    Ok(())
}

pub(crate) async fn clear_jmap_email_state(
    db: &DbPool,
    account_id: &str,
    anchor_folder_id: &str,
) -> Result<(), SyncError> {
    clear_state_cursor(db, account_id, anchor_folder_id, "email_state").await
}

/// Find the local folder id for a JMAP mailbox id (`external_id`), if synced.
pub(crate) async fn find_folder_id(
    db: &DbPool,
    account_id: &str,
    external_id: &str,
) -> Result<Option<String>, SyncError> {
    find_folder_id_by_external_id(db, account_id, external_id).await
}

/// The account's folder with `role` (local `role_override` wins), if any.
pub(crate) async fn folder_id_for_role(
    db: &DbPool,
    account_id: &str,
    role: &str,
) -> Result<Option<String>, SyncError> {
    let mut sel = Sq::select();
    sel.column(folder::Column::Id)
        .from(folder::Entity)
        .and_where(folder::Column::AccountId.eq(id_value(db, account_id)?))
        .and_where(Expr::cust_with_values(
            "COALESCE(role_override, role) = ?",
            [role],
        ))
        .order_by_expr(
            Expr::col(folder::Column::SortOrder),
            sea_orm::sea_query::Order::Asc,
        )
        .limit(1);
    let row = db.orm().query_one(&sel).await.map_err(orm_err)?;
    row.map(|r| row_id(&r, "id")).transpose()
}

/// Hard-delete JMAP messages by server ids (`external_id`), scoped to the
/// account. Server-gone and moved-out messages both land here; the caller
/// has already subtracted anything re-fetched this run. Attachment rows and
/// FTS entries cascade. Folder counts of affected folders are refreshed.
pub(crate) async fn delete_jmap_messages_by_external_ids(
    db: &DbPool,
    account_id: &str,
    external_ids: &[String],
) -> Result<usize, SyncError> {
    if external_ids.is_empty() {
        return Ok(0);
    }
    let account_bind = id_value(db, account_id)?;

    // Folders whose counts change (collected before the delete; the delete +
    // count refresh are deliberately non-transactional, as persist_attachments).
    let mut sel = Sq::select();
    sel.distinct()
        .column(message::Column::FolderId)
        .from(message::Entity)
        .and_where(message::Column::AccountId.eq(account_bind.clone()))
        .and_where(message::Column::ExternalId.is_in(external_ids.iter().cloned()));
    let rows = db.orm().query_all(&sel).await.map_err(orm_err)?;
    let folder_ids: Vec<String> = rows
        .iter()
        .map(|r| row_id(r, "folder_id"))
        .collect::<Result<Vec<_>, _>>()?;

    let mut del = Sq::delete();
    del.from_table(message::Entity)
        .and_where(message::Column::AccountId.eq(account_bind))
        .and_where(message::Column::ExternalId.is_in(external_ids.iter().cloned()));
    let res = db.orm().execute(&del).await.map_err(orm_err)?;
    let deleted = res.rows_affected();

    for folder_id in &folder_ids {
        update_folder_counts(db, folder_id).await?;
    }
    Ok(usize::try_from(deleted).unwrap_or(usize::MAX))
}
```

4. Add `jmap_thread_id` to the shared insert. In `MessageInsert`, add a field:

```rust
    jmap_thread_id: Option<&'a str>,
```

In `message_insert`, append `message::Column::JmapThreadId,` at the end of the `.columns([…])` list and append `Expr::val(opt_str_value(m.jmap_thread_id)),` at the end of the `.values_panic(vec![…])` list (the two lists stay parallel).

In `upsert_message_in_tx` (IMAP), add `jmap_thread_id: None,` to the `MessageInsert { … }` literal.

5. In `upsert_jmap_message_in_tx`: in the `else` (insert) branch, add `jmap_thread_id: email.thread_id.as_deref(),` to the `MessageInsert { … }` literal. In the `if let Some(id) = existing` (update) branch, add two `.value(...)` calls to the `upd` statement, after the `Flags` one:

```rust
            // Server-side moves re-home the row; the JMAP threadId is persisted.
            .value(message::Column::FolderId, Expr::val(id_value(db, folder_id)?))
            .value(
                message::Column::JmapThreadId,
                Expr::val(opt_str_value(email.thread_id.as_deref())),
            )
```

- [ ] **Step 5: Add the seam sync methods**

In `backend/src/sync/jmap_client.rs`, extend the imports:

```rust
use jmap_client::core::get::GetRequest;
use jmap_client::core::query::{QueryRequest, QueryResponse};
use jmap_client::core::query_changes::QueryChangesResponse;
use jmap_client::core::response::{EmailChangesResponse, EmailGetResponse, MailboxGetResponse};
use jmap_client::email::{self, Email, EmailAddress, EmailBodyPart, Property};
use jmap_client::{Get, Set, URI};
```

(i.e., add `self` + `Property` to the email import, `Set` to the root import, plus the four `core` lines.)

Add constants near the existing ones:

```rust
/// `Email/changes` page size and page bound (a page is ≤ this many ids).
const CHANGES_PAGE_SIZE: usize = 500;
const CHANGES_MAX_PAGES: usize = 8;
/// Cap on returned body values (mirrors the IMAP lazy-fetch body cap).
const MAX_BODY_VALUE_BYTES: usize = 25 * 1024 * 1024;
```

Add DTOs after the existing DTO block:

```rust
/// Incremental result from `Email/queryChanges` (moved from jmap.rs).
#[derive(Debug, Clone)]
pub(crate) struct EmailQueryChanges {
    pub(crate) added_ids: Vec<String>,
    pub(crate) removed_ids: Vec<String>,
    pub(crate) new_query_state: Option<String>,
}

/// One page of a (batched) `Email/query` + `Email/get`.
#[derive(Debug)]
pub(crate) struct EmailPage {
    /// Ids from the query (paging is driven by these, not the fetched list).
    pub(crate) ids: Vec<String>,
    pub(crate) emails: Vec<JmapEmail>,
    /// Folder cursor (`queryState`), committed only with the last page.
    pub(crate) query_state: Option<String>,
    /// Account-level `Email` state from the get response (`Email/changes` input).
    pub(crate) email_state: Option<String>,
}

/// Account-level `Email/changes` outcome.
#[derive(Debug)]
pub(crate) struct JmapEmailChanges {
    pub(crate) updated_ids: Vec<String>,
    pub(crate) destroyed_ids: Vec<String>,
    pub(crate) new_state: Option<String>,
}
```

Add to `impl JmapSeam`:

```rust
    /// `maxCallsInRequest` capability; conservative default when the session's
    /// core capabilities fail to type-check (untagged `Capabilities` is
    /// all-or-nothing per required field set).
    pub(crate) fn max_calls_in_request(&self) -> usize {
        self.client
            .session()
            .core_capabilities()
            .map_or(8, |c| c.max_calls_in_request())
    }

    /// List all mailboxes (folders) for this account (`Mailbox/get`, ids omitted = all).
    pub(crate) async fn list_mailboxes(&self) -> Result<Vec<JmapMailbox>, JmapError> {
        let mut request = self.client.build();
        request.get_mailbox();
        let mut resp = request.send_single::<MailboxGetResponse>().await?;
        Ok(resp.take_list().iter().filter_map(map_mailbox).collect())
    }

    /// One `Email/query` page with the matching `Email/get` batched into the
    /// same request via a `/ids` result reference (one round trip per page).
    /// Splits into two requests when `maxCallsInRequest < 2`.
    pub(crate) async fn query_emails_page(
        &self,
        mailbox_id: &str,
        position: usize,
        limit: usize,
    ) -> Result<EmailPage, JmapError> {
        let position = i32::try_from(position).unwrap_or(i32::MAX);
        if self.max_calls_in_request() < 2 {
            let mut query_req = self.client.build();
            fill_email_query(query_req.query_email(), mailbox_id, position, limit);
            let mut query_resp = query_req.send_single::<QueryResponse>().await?;
            let ids = query_resp.take_ids();
            let query_state = query_resp.take_query_state();
            let (emails, email_state) = self.get_emails(&ids).await?;
            return Ok(EmailPage {
                ids,
                emails,
                query_state: Some(query_state),
                email_state,
            });
        }

        let mut request = self.client.build();
        let ids_ref = {
            let q = request.query_email();
            fill_email_query(q, mailbox_id, position, limit);
            q.result_reference()
        };
        {
            let g = request.get_email();
            g.ids_ref(ids_ref);
            fill_email_get(g);
        }
        let mut responses = request.send().await?.unwrap_method_responses();
        if responses.len() != 2 {
            return Err(JmapError::InvalidResponse(format!(
                "expected Email/query + Email/get responses, got {}",
                responses.len()
            )));
        }
        let mut get_resp = responses.remove(1).unwrap_get_email()?;
        let mut query_resp = responses.remove(0).unwrap_query_email()?;
        let email_state = get_resp.take_state();
        let emails = get_resp.take_list().iter().map(map_email).collect();
        Ok(EmailPage {
            ids: query_resp.take_ids(),
            emails,
            query_state: Some(query_resp.take_query_state()),
            email_state: Some(email_state),
        })
    }

    /// Incremental mailbox changes since a stored `queryState` (RFC 8621 `Email/queryChanges`).
    pub(crate) async fn query_email_changes(
        &self,
        mailbox_id: &str,
        since_query_state: &str,
    ) -> Result<EmailQueryChanges, JmapError> {
        let mut request = self.client.build();
        {
            let q = request.query_email_changes(since_query_state);
            q.filter(email::query::Filter::in_mailbox(mailbox_id));
            q.sort([email::query::Comparator::received_at().descending()]);
            q.max_changes(CHANGES_PAGE_SIZE);
        }
        let resp = request.send_single::<QueryChangesResponse>().await?;
        Ok(EmailQueryChanges {
            added_ids: resp.added().iter().map(|a| a.id().to_owned()).collect(),
            removed_ids: resp.removed().to_vec(),
            new_query_state: Some(resp.new_query_state().to_owned()),
        })
    }

    /// Account-level `Email/changes`: keyword/mailbox updates and destroys that
    /// per-folder queryChanges cannot see (no membership change). Pages until
    /// `hasMoreChanges` clears or the bound is hit.
    pub(crate) async fn email_changes(&self, since_state: &str) -> Result<JmapEmailChanges, JmapError> {
        let mut updated_ids = Vec::new();
        let mut destroyed_ids = Vec::new();
        let mut new_state = None;
        let mut since = since_state.to_owned();
        for _page in 0..CHANGES_MAX_PAGES {
            let mut request = self.client.build();
            request.changes_email(since.clone()).max_changes(CHANGES_PAGE_SIZE);
            let mut resp = request.send_single::<EmailChangesResponse>().await?;
            updated_ids.extend(resp.take_updated());
            destroyed_ids.extend(resp.take_destroyed());
            since = resp.take_new_state();
            new_state = Some(since.clone());
            if !resp.has_more_changes() {
                break;
            }
        }
        Ok(JmapEmailChanges {
            updated_ids,
            destroyed_ids,
            new_state,
        })
    }

    /// Fetch email objects by id with the sync property set.
    /// Returns the emails plus the account-level `Email` state.
    pub(crate) async fn get_emails(
        &self,
        ids: &[String],
    ) -> Result<(Vec<JmapEmail>, Option<String>), JmapError> {
        if ids.is_empty() {
            return Ok((Vec::new(), None));
        }
        let mut request = self.client.build();
        {
            let get = request.get_email();
            get.ids(ids.iter().cloned());
            fill_email_get(get);
        }
        let mut resp = request.send_single::<EmailGetResponse>().await?;
        let state = resp.take_state();
        let emails = resp.take_list().iter().map(map_email).collect();
        Ok((emails, Some(state)))
    }
```

Add the free helpers after `map_mailbox`:

```rust
/// Properties fetched for every synced message (RFC 8621 §4.3 property list).
fn email_get_properties() -> Vec<Property> {
    use jmap_client::email::Property as P;
    vec![
        P::Id,
        P::BlobId,
        P::ThreadId,
        P::MailboxIds,
        P::Keywords,
        P::Size,
        P::ReceivedAt,
        P::MessageId,
        P::InReplyTo,
        P::References,
        P::Sender,
        P::From,
        P::To,
        P::Cc,
        P::Bcc,
        P::ReplyTo,
        P::Subject,
        P::BodyStructure,
        P::BodyValues,
        P::TextBody,
        P::HtmlBody,
        P::Attachments,
        P::HasAttachment,
        P::Preview,
    ]
}

/// `Email/query` args for one folder page (receivedAt desc, paged by position).
fn fill_email_query(q: &mut QueryRequest<Email<Set>>, mailbox_id: &str, position: i32, limit: usize) {
    q.filter(email::query::Filter::in_mailbox(mailbox_id));
    q.sort([email::query::Comparator::received_at().descending()]);
    q.position(position);
    q.limit(limit);
    q.calculate_total(true);
}

/// `Email/get` args for sync: full property set + text/HTML body values,
/// capped at `MAX_BODY_VALUE_BYTES` per value.
fn fill_email_get(get: &mut GetRequest<Email<Set>>) {
    get.properties(email_get_properties());
    get.arguments().fetch_text_body_values(true);
    get.arguments().fetch_html_body_values(true);
    get.arguments().max_body_value_bytes(MAX_BODY_VALUE_BYTES);
}
```

- [ ] **Step 6: Rewrite `jmap_loop.rs`**

Replace the entire content of `backend/src/sync/jmap_loop.rs` with:

```rust
//! JMAP mailbox fetch loop (IMAP fallback on failure).

use std::collections::{HashMap, HashSet};

use super::imap_loop::run_imap_sync;
use super::jmap_client::{JmapEmail, JmapSeam};
use super::store::{
    clear_jmap_cursor, clear_jmap_email_state, delete_jmap_messages_by_external_ids,
    find_folder_id, folder_id_for_role, get_folder_id, link_jmap_folder_parent,
    load_account_sync_row, load_jmap_cursor, load_jmap_email_state, outcome_from_response,
    persist_jmap_folder_batch, save_jmap_email_state, upsert_jmap_folder,
};
use super::types::{SyncError, SyncResponse};
use crate::protocol::SyncOutcome;
use crate::storage::DbPool;

/// `Email/query`/`Email/get` page size (kept small so one response stays bounded).
const QUERY_PAGE: usize = 100;

/// Load a JMAP account and run the JMAP fetch loop.
///
/// JMAP-then-IMAP fallback stays inside this plugin path, not core dispatch.
pub(crate) async fn jmap_sync_account(
    db: &DbPool,
    user_id: &str,
    account_id: &str,
) -> Result<SyncOutcome, SyncError> {
    let Ok(dek) = crate::auth::AuthState::get_user_dek(db, user_id).await else {
        return Err(super::recovery::fail_credential_decrypt(db, account_id).await);
    };
    let row = load_account_sync_row(db, user_id, account_id).await?;
    let credential_json = row.credential.clone();
    let email_address = row.email_address.clone();
    let jmap_base_url = row.jmap_base_url.clone();

    let result = if let Some(ref base_url) = jmap_base_url {
        let Ok(secret) = super::jmap_client::decrypt_account_password(&credential_json, &dek)
        else {
            return Err(super::recovery::fail_credential_decrypt(db, account_id).await);
        };
        match run_jmap_sync(db, account_id, base_url, &email_address, &secret, &row.auth_type).await
        {
            Ok(result) => result,
            Err(e) => {
                if e.is_auth() {
                    JmapSeam::evict(account_id);
                }
                tracing::warn!("JMAP sync failed ({e}), falling back to IMAP");
                let Ok(password) = crate::imap::decrypt_account_password(&credential_json, &dek)
                else {
                    return Err(super::recovery::fail_credential_decrypt(db, account_id).await);
                };
                run_imap_sync(db, account_id, &row, &password, false).await?
            }
        }
    } else {
        let Ok(password) = crate::imap::decrypt_account_password(&credential_json, &dek) else {
            return Err(super::recovery::fail_credential_decrypt(db, account_id).await);
        };
        run_imap_sync(db, account_id, &row, &password, false).await?
    };
    Ok(outcome_from_response(&result))
}

/// Run a JMAP sync for an account.
///
/// Cached session → `Mailbox/get` → account-level `Email/changes` (keyword /
/// mailbox updates, destroys) → per mailbox `Email/queryChanges` (added →
/// fetch; removed → delete) or paged `Email/query` + `Email/get` batched per
/// page → persist (additive: `thread_id`, `folder_id` moves, removed-ids
/// deletes).
pub(crate) async fn run_jmap_sync(
    db: &DbPool,
    account_id: &str,
    jmap_base_url: &str,
    email: &str,
    secret: &str,
    auth_type: &str,
) -> Result<SyncResponse, SyncError> {
    let seam = JmapSeam::connect_for_account(account_id, jmap_base_url, email, secret, auth_type).await?;
    seam.refresh_if_stale().await?;

    // 1. Mailboxes.
    let mailboxes = seam.list_mailboxes().await?;
    let mut folders_synced = 0;
    for mb in &mailboxes {
        upsert_jmap_folder(db, account_id, mb).await?;
        folders_synced += 1;
    }
    for mb in &mailboxes {
        if let Some(ref parent_id) = mb.parent_id {
            link_jmap_folder_parent(db, account_id, &mb.id, parent_id).await?;
        }
    }

    let mut total_new = 0usize;
    let mut total_updated = 0usize;
    let mut refetched: HashSet<String> = HashSet::new();
    let mut removed: Vec<String> = Vec::new();

    // 2. Account-level Email/changes: keyword/mailbox updates + destroys that
    // per-folder queryChanges cannot see (no membership change).
    let email_state_anchor = folder_id_for_role(db, account_id, "inbox").await?;
    let mut new_email_state: Option<String> = None;
    if let Some(ref anchor) = email_state_anchor
        && let Some(since) = load_jmap_email_state(db, account_id, anchor).await?
    {
        match seam.email_changes(&since).await {
            Ok(changes) => {
                removed.extend(changes.destroyed_ids);
                new_email_state = changes.new_state;
                let (n, u) = refetch_updated_emails(
                    db,
                    account_id,
                    &seam,
                    &changes.updated_ids,
                    &mut refetched,
                )
                .await?;
                total_new += n;
                total_updated += u;
            }
            Err(e) if e.is_stale_query_state() => {
                tracing::info!(account_id, "JMAP Email state expired; clearing email_state cursor");
                clear_jmap_email_state(db, account_id, anchor).await?;
            }
            Err(e) => return Err(e.into()),
        }
    }

    // 3. Per-folder queryChanges (or full query when no/expired cursor).
    for mb in &mailboxes {
        let folder_id = get_folder_id(db, account_id, &mb.id).await?;
        let since_state = load_jmap_cursor(db, account_id, &folder_id).await?;

        match since_state {
            Some(state) => match seam.query_email_changes(&mb.id, &state).await {
                Ok(changes) => {
                    removed.extend(changes.removed_ids.iter().cloned());
                    refetched.extend(changes.added_ids.iter().cloned());
                    if changes.added_ids.is_empty() {
                        // Advance the cursor even with no additions.
                        persist_jmap_folder_batch(
                            db,
                            account_id,
                            &folder_id,
                            &[],
                            changes.new_query_state.as_deref(),
                        )
                        .await?;
                        continue;
                    }
                    let mut chunks = changes.added_ids.chunks(QUERY_PAGE).peekable();
                    while let Some(chunk) = chunks.next() {
                        let last = chunks.peek().is_none();
                        let (emails, email_state) = seam.get_emails(chunk).await?;
                        if new_email_state.is_none() {
                            new_email_state = email_state;
                        }
                        // The queryState cursor commits only with the LAST chunk.
                        let (n, u) = persist_jmap_folder_batch(
                            db,
                            account_id,
                            &folder_id,
                            &emails,
                            if last { changes.new_query_state.as_deref() } else { None },
                        )
                        .await?;
                        total_new += n;
                        total_updated += u;
                    }
                }
                Err(e) if e.is_stale_query_state() => {
                    tracing::info!(
                        account_id,
                        folder = %mb.name,
                        "JMAP queryState expired; clearing cursor and running a full query"
                    );
                    clear_jmap_cursor(db, account_id, &folder_id).await?;
                    let (n, u) = full_folder_query(
                        db,
                        account_id,
                        &folder_id,
                        &mb.id,
                        &seam,
                        &mut refetched,
                        &mut new_email_state,
                    )
                    .await?;
                    total_new += n;
                    total_updated += u;
                }
                Err(e) => return Err(e.into()),
            },
            None => {
                let (n, u) = full_folder_query(
                    db,
                    account_id,
                    &folder_id,
                    &mb.id,
                    &seam,
                    &mut refetched,
                    &mut new_email_state,
                )
                .await?;
                total_new += n;
                total_updated += u;
            }
        }
    }

    // 4. Apply removals/destroys not re-fetched during this run (a message
    // moved between folders re-enters via the other folder's `added`, or via
    // Email/changes `updated`, and must not be deleted).
    let deletions = plan_deletions(removed, &refetched);
    let total_deleted = delete_jmap_messages_by_external_ids(db, account_id, &deletions).await?;

    // 5. Commit the account-level Email state last.
    if let (Some(ref anchor), Some(ref state)) = (email_state_anchor, new_email_state) {
        save_jmap_email_state(db, account_id, anchor, state).await?;
    }

    Ok(SyncResponse {
        account_id: account_id.to_string(),
        status: "completed".into(),
        folders_synced,
        messages_synced: total_new,
        messages_updated: total_updated,
        messages_deleted: total_deleted,
    })
}

/// Page through `Email/query` until the mailbox is exhausted, persisting each
/// page. The `queryState` cursor commits only with the LAST page: once it
/// lands, `Email/queryChanges` returns only deltas, so an early commit would
/// strand every message past the committed page on a crash (sync spec §4.1).
async fn full_folder_query(
    db: &DbPool,
    account_id: &str,
    folder_id: &str,
    mailbox_id: &str,
    seam: &JmapSeam,
    refetched: &mut HashSet<String>,
    new_email_state: &mut Option<String>,
) -> Result<(usize, usize), SyncError> {
    let mut new = 0usize;
    let mut updated = 0usize;
    let mut position = 0usize;
    loop {
        let page = seam.query_emails_page(mailbox_id, position, QUERY_PAGE).await?;
        let page_len = page.ids.len();
        refetched.extend(page.ids.iter().cloned());
        if new_email_state.is_none() {
            *new_email_state = page.email_state.clone();
        }
        let commit_state = if page_len < QUERY_PAGE { page.query_state.as_deref() } else { None };
        let (n, u) = persist_jmap_folder_batch(db, account_id, folder_id, &page.emails, commit_state).await?;
        new += n;
        updated += u;
        if page_len < QUERY_PAGE {
            break;
        }
        position += QUERY_PAGE;
    }
    Ok((new, updated))
}

/// Re-fetch `Email/changes`-updated messages and upsert them under the folder
/// of their (first locally-known) mailbox — this is how server-side flag and
/// mailbox changes reach local rows between full queries.
async fn refetch_updated_emails(
    db: &DbPool,
    account_id: &str,
    seam: &JmapSeam,
    updated_ids: &[String],
    refetched: &mut HashSet<String>,
) -> Result<(usize, usize), SyncError> {
    let mut new = 0usize;
    let mut updated = 0usize;
    for chunk in updated_ids.chunks(QUERY_PAGE) {
        let (emails, _email_state) = seam.get_emails(chunk).await?;
        let mut by_folder: HashMap<String, Vec<JmapEmail>> = HashMap::new();
        for email in emails {
            refetched.insert(email.id.clone());
            if let Some(folder_id) = resolve_jmap_email_folder(db, account_id, &email).await? {
                by_folder.entry(folder_id).or_default().push(email);
            }
        }
        for (folder_id, group) in by_folder {
            let (n, u) = persist_jmap_folder_batch(db, account_id, &folder_id, &group, None).await?;
            new += n;
            updated += u;
        }
    }
    Ok((new, updated))
}

/// First mailbox of the email that maps to a synced local folder.
async fn resolve_jmap_email_folder(
    db: &DbPool,
    account_id: &str,
    email: &JmapEmail,
) -> Result<Option<String>, SyncError> {
    let Some(serde_json::Value::Object(map)) = &email.mailbox_ids else {
        return Ok(None);
    };
    for mailbox_id in map.keys() {
        if let Some(folder_id) = find_folder_id(db, account_id, mailbox_id).await? {
            return Ok(Some(folder_id));
        }
    }
    Ok(None)
}

/// Removals/destroys minus everything re-fetched this run. Deleting only the
/// difference keeps server-side moves from becoming data loss.
fn plan_deletions(removed: Vec<String>, refetched: &HashSet<String>) -> Vec<String> {
    removed.into_iter().filter(|id| !refetched.contains(id)).collect()
}

#[cfg(test)]
mod tests {
    use super::plan_deletions;
    use std::collections::HashSet;

    #[test]
    fn plan_deletions_drops_refetched_ids() {
        let refetched: HashSet<String> = ["b".to_owned(), "d".to_owned()].into_iter().collect();
        let removed = vec!["a".to_owned(), "b".to_owned(), "c".to_owned()];
        assert_eq!(plan_deletions(removed, &refetched), vec!["a".to_owned(), "c".to_owned()]);
    }

    #[test]
    fn plan_deletions_empty_when_all_refetched() {
        let refetched: HashSet<String> = ["a".to_owned()].into_iter().collect();
        assert!(plan_deletions(vec!["a".to_owned()], &refetched).is_empty());
    }
}
```

- [ ] **Step 7: Delete the orphaned query/get methods from `jmap.rs`**

In `backend/src/jmap.rs`, delete (now fully replaced by the seam):

- `JmapClient::query_emails`, `JmapClient::query_email_changes`, `JmapClient::get_emails`
- The `EmailQueryResult` struct (also moved out of the file: it is *not* re-exported — the seam returns its own `EmailQueryChanges`/`EmailPage`; delete jmap.rs's `EmailQueryChanges` too, since the seam defines it now and nothing else uses the old one)
- The test `parse_email_query_result`

Keep `take_ok_args*` (still used by `list_mailboxes`, `submit_email`, `create_draft`, `destroy_email`, `set_email_mailboxes`).

Run: `cd backend && cargo check`
Expected: `Finished`. If the compiler reports newly-dead items (e.g. `EmailQueryChanges`-related imports), remove exactly those.

- [ ] **Step 8: Run the scoped tests**

Run: `cd backend && cargo test --bin lyra_backend jmap`
Expected: `test result: ok` — new `sync::tests::jmap_*` store tests, `sync::jmap_loop::tests::*`, `sync::jmap_client::tests::*` all pass.

- [ ] **Step 9: Format + lint + full suite**

Run:
```bash
cd backend && rustfmt --edition 2024 src/sync/jmap_client.rs src/sync/jmap_loop.rs src/sync/store.rs src/sync/mod.rs src/entities/message.rs src/jmap.rs && cargo fmt --check
cd backend && cargo clippy --all-targets --all-features 2>&1 | grep "warning:" | grep -v "oauth/config.rs"
cd backend && cargo test --bin lyra_backend 2>&1 | tail -20
```
Expected: fmt clean; clippy grep empty; suite green (3 pre-existing gpg failures allowed).

- [ ] **Step 10: Commit**

```bash
git add backend/migrations backend/src/entities/message.rs backend/src/sync/store.rs backend/src/sync/jmap_client.rs backend/src/sync/jmap_loop.rs backend/src/sync/mod.rs backend/src/jmap.rs
git commit -m "feat: rewrite JMAP sync loop on seam (removed ids, Email/changes, thread_id)"
```

---

### Task 3: Download JMAP attachments via the blob endpoint

Commit: `feat: download JMAP attachments via blob endpoint`

**Files:**
- Modify: `backend/src/sync/store.rs` (per-message persist results)
- Modify: `backend/src/sync/jmap_client.rs` (attachment meta + `download_blob`)
- Modify: `backend/src/plugins/mod.rs` (`bind_data_dir`)
- Modify: `backend/src/main.rs` (bind it)
- Modify: `backend/src/sync/jmap_loop.rs` (download wiring)
- Modify: `backend/src/sync/mod.rs` (tests)

- [ ] **Step 1: Write the failing tests**

Append to `backend/src/sync/jmap_client.rs`'s `mod tests`:

```rust
    // ── attachment meta mapping (Task 3) ────────────────────────────

    #[test]
    fn map_attachments_collects_downloadable_parts() {
        let email: Email<Get> = serde_json::from_value(serde_json::json!({
            "id": "em1",
            "attachments": [
                { "blobId": "b1", "name": "invoice.pdf", "type": "application/pdf", "size": 1234, "disposition": "attachment" },
                { "name": "no-blob.txt", "type": "text/plain" },
                { "blobId": "b2", "type": "image/png", "size": 10, "cid": "cid1", "disposition": "inline" }
            ]
        }))
        .unwrap();
        let meta = map_attachments(&email);
        assert_eq!(meta.len(), 2, "parts without blobId are skipped");
        assert_eq!(meta[0].blob_id, "b1");
        assert_eq!(meta[0].filename, "invoice.pdf");
        assert_eq!(meta[0].content_type, "application/pdf");
        assert_eq!(meta[0].size, 1234);
        assert!(!meta[0].is_inline);
        assert_eq!(meta[1].blob_id, "b2");
        assert!(meta[1].is_inline);
        assert_eq!(meta[1].content_id.as_deref(), Some("cid1"));
        assert_eq!(meta[1].filename, "attachment", "fallback filename");
    }

    #[test]
    fn map_attachments_empty_without_attachments() {
        let email: Email<Get> = serde_json::from_value(serde_json::json!({ "id": "em2" })).unwrap();
        assert!(map_attachments(&email).is_empty());
    }
```

Append to `backend/src/sync/mod.rs`'s `mod tests`:

```rust
    #[tokio::test]
    async fn jmap_persist_batch_reports_per_message_results() {
        let pool = test_pool().await;
        let (_, account_id) = seed_user_and_account(&pool).await;
        upsert_folder(&as_db(&pool), &account_id, "INBOX", Some("/"), &[])
            .await
            .unwrap();
        let inbox = get_folder_id(&as_db(&pool), &account_id, "INBOX").await.unwrap();

        let (new, updated, persisted) = super::store::persist_jmap_folder_batch(
            &as_db(&pool),
            &account_id,
            &inbox,
            &[sample_jmap_email("em1", None), sample_jmap_email("em2", None)],
            None,
        )
        .await
        .unwrap();
        assert_eq!((new, updated), (2, 0));
        assert_eq!(persisted.len(), 2, "one result per input email, in order");
        assert!(persisted.iter().all(|p| p.was_new));

        // Re-persist: same external id, now an update with the same local id.
        let (new, updated, persisted2) = super::store::persist_jmap_folder_batch(
            &as_db(&pool),
            &account_id,
            &inbox,
            &[sample_jmap_email("em1", None)],
            None,
        )
        .await
        .unwrap();
        assert_eq!((new, updated), (0, 1));
        assert!(!persisted2[0].was_new);
        assert_eq!(persisted2[0].local_id, persisted[0].local_id);
    }

    #[tokio::test]
    async fn jmap_attachments_persist_to_blob_store() {
        let pool = test_pool().await;
        let (_, account_id) = seed_user_and_account(&pool).await;
        let dir = tempfile::tempdir().unwrap();
        upsert_folder(&as_db(&pool), &account_id, "INBOX", Some("/"), &[])
            .await
            .unwrap();
        let inbox = get_folder_id(&as_db(&pool), &account_id, "INBOX").await.unwrap();
        let (_, _, persisted) = super::store::persist_jmap_folder_batch(
            &as_db(&pool),
            &account_id,
            &inbox,
            &[sample_jmap_email("em-att", None)],
            None,
        )
        .await
        .unwrap();
        let local_id = &persisted[0].local_id;

        let extracted = vec![crate::imap::ExtractedAttachment {
            filename: "a.txt".into(),
            content_type: "text/plain".into(),
            data: b"hello blob".to_vec(),
            content_id: None,
            is_inline: false,
        }];
        super::http::persist_attachments(&as_db(&pool), dir.path(), &account_id, local_id, &extracted)
            .await
            .unwrap();

        let row: (i64, String) = sqlx::query_as(
            "SELECT COUNT(*), MIN(storage_path) FROM attachment WHERE message_id = ?",
        )
        .bind(local_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.0, 1);
        let blob = crate::blobs::read(dir.path(), &row.1).await.unwrap();
        assert_eq!(blob, b"hello blob");

        let has: bool = sqlx::query_scalar("SELECT has_attachments FROM message WHERE id = ?")
            .bind(local_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(has);
    }
```

Note: `persist_attachments` is `pub(crate)` in `sync/http.rs` — reachable from the test module as `super::http::persist_attachments`. `tempfile` is an existing dev-dependency.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd backend && cargo test --bin lyra_backend jmap 2>&1 | grep -E "^error" | head -20`
Expected: compile errors — `map_attachments` missing; `persist_jmap_folder_batch` returns a 2-tuple, not 3.

- [ ] **Step 3: Per-message persist results in `store.rs`**

In `backend/src/sync/store.rs`, add near `persist_jmap_folder_batch`:

```rust
/// One JMAP message persisted this batch (same order as the input slice).
#[derive(Debug)]
pub(crate) struct JmapPersistedMessage {
    pub(crate) local_id: String,
    pub(crate) was_new: bool,
}
```

Change `upsert_jmap_message_in_tx`'s return type to `Result<(bool, String), SyncError>` — the tuple is `(was_new, local_id)`. The existing-row branch returns `Ok((false, id))`; the insert branch looks the id back up (one extra SELECT per new message, next to the existing pre-insert lookup):

```rust
pub(crate) async fn upsert_jmap_message_in_tx(
    tx: &mut DbTxn,
    db: &DbPool,
    account_id: &str,
    folder_id: &str,
    email: &crate::jmap::JmapEmail,
) -> Result<(bool, String), SyncError> {
    let external_id = &email.id;

    let existing = find_message_id_in_tx(tx, db, account_id, external_id).await?;

    let is_read = email.is_seen();
    let is_starred = email.is_flagged();
    let snippet = email
        .preview
        .clone()
        .or_else(|| email.subject.as_ref().map(|s| truncate_for_snippet(s)));

    let from_json = email
        .format_from()
        .map(|f| serde_json::json!({ "raw": f }).to_string());
    let to_json = email
        .to_string_list()
        .map(|t| serde_json::json!(vec![t]).to_string());
    let cc_json = email.cc.as_ref().map(|addrs| {
        let formatted: Vec<String> = addrs
            .iter()
            .map(|a| match (&a.name, &a.email) {
                (Some(name), Some(email)) => format!("{name} <{email}>"),
                (None, Some(email)) => email.clone(),
                _ => String::new(),
            })
            .collect();
        serde_json::json!(formatted).to_string()
    });

    let flags_json = serde_json::to_string(&email.keywords).unwrap_or_else(|_| "{}".into());

    if let Some(id) = existing {
        let mut upd = Sq::update();
        upd.table(message::Entity)
            .value(message::Column::IsRead, Expr::val(is_read))
            .value(message::Column::IsStarred, Expr::val(is_starred))
            .value(
                message::Column::Flags,
                Expr::val(opt_json_value(db, Some(flags_json.as_str()))),
            )
            // Server-side moves re-home the row; the JMAP threadId is persisted.
            .value(message::Column::FolderId, Expr::val(id_value(db, folder_id)?))
            .value(
                message::Column::JmapThreadId,
                Expr::val(opt_str_value(email.thread_id.as_deref())),
            )
            .value(message::Column::UpdatedAt, Expr::current_timestamp())
            .and_where(message::Column::Id.eq(id_value(db, &id)?));
        tx_execute(tx, &upd).await?;
        Ok((false, id))
    } else {
        let in_reply_to = email
            .in_reply_to
            .as_ref()
            .and_then(|ids| ids.first())
            .cloned();
        let references = email.references.as_ref().map(|refs| refs.join(" "));
        let message_id_header = email.message_id_header();
        let body_text = email.body_text();
        let body_html = persist_body_html(email.body_html().as_deref());
        let insert = message_insert(
            db,
            MessageInsert {
                account_bind: id_value(db, account_id)?,
                folder_bind: id_value(db, folder_id)?,
                external_id,
                message_id_header: message_id_header.as_deref(),
                subject: email.subject.as_deref(),
                from_json: from_json.as_deref(),
                to_json: to_json.as_deref(),
                cc_json: cc_json.as_deref(),
                date: email.received_at.as_deref(),
                is_read,
                is_starred,
                flags_json: &flags_json,
                size_bytes: email.size.map(|s| i32::try_from(s).unwrap_or(i32::MAX)),
                in_reply_to: in_reply_to.as_deref(),
                references_headers: references.as_deref(),
                snippet: snippet.as_deref(),
                has_attachments: email.has_attachment.unwrap_or(false),
                body_text: body_text.as_deref(),
                body_html: body_html.as_deref(),
                jmap_thread_id: email.thread_id.as_deref(),
            },
        );
        tx_execute(tx, &insert).await?;
        let id = find_message_id_in_tx(tx, db, account_id, external_id)
            .await?
            .ok_or_else(|| SyncError::Internal("JMAP message insert lost its row".into()))?;
        Ok((true, id))
    }
}
```

Change `persist_jmap_folder_batch` to collect and return them:

```rust
/// Persist one JMAP mailbox page: upserts, cursor, counts — one transaction.
/// Returns `(new, updated, per-message results in input order)`.
pub(crate) async fn persist_jmap_folder_batch(
    db: &DbPool,
    account_id: &str,
    folder_id: &str,
    emails: &[crate::jmap::JmapEmail],
    query_state: Option<&str>,
) -> Result<(usize, usize, Vec<JmapPersistedMessage>), SyncError> {
    let mut tx = db.begin().await?;
    let mut new = 0usize;
    let mut updated = 0usize;
    let mut persisted = Vec::with_capacity(emails.len());
    for email in emails {
        let (was_new, local_id) = upsert_jmap_message_in_tx(&mut tx, db, account_id, folder_id, email).await?;
        if was_new {
            new += 1;
        } else {
            updated += 1;
        }
        persisted.push(JmapPersistedMessage { local_id, was_new });
    }
    if let Some(qs) = query_state {
        save_jmap_cursor_in_tx(&mut tx, db, account_id, folder_id, qs).await?;
    }
    update_folder_counts_in_tx(&mut tx, db, folder_id).await?;
    tx.commit().await?;
    Ok((new, updated, persisted))
}
```

- [ ] **Step 4: Seam attachment meta + `download_blob`**

In `backend/src/sync/jmap_client.rs`:

Add to `JmapEmail` (after the `preview` field):

```rust
    /// Typed attachment locators for blob download (built by `map_attachments`).
    #[serde(default)]
    pub attachments_meta: Vec<JmapAttachmentMeta>,
```

Add the DTO after `JmapEmailAddress`:

```rust
/// Attachment locator for blob download (RFC 8621 EmailBodyPart subset).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct JmapAttachmentMeta {
    pub blob_id: String,
    pub filename: String,
    pub content_type: String,
    pub size: u64,
    pub content_id: Option<String>,
    pub is_inline: bool,
}
```

In `map_email`, replace `attachments: None, // superseded by typed attachment meta (Task 3)` with `attachments: None,` and add `attachments_meta: map_attachments(email),` as the final struct field.

Add after `map_body_refs`:

```rust
/// Collect downloadable attachment parts (blobId required) into typed meta.
fn map_attachments(email: &Email<Get>) -> Vec<JmapAttachmentMeta> {
    email
        .attachments()
        .map(|parts| {
            parts
                .iter()
                .filter_map(|p| {
                    let blob_id = p.blob_id()?.to_owned();
                    Some(JmapAttachmentMeta {
                        blob_id,
                        filename: p.name().unwrap_or("attachment").to_owned(),
                        content_type: p
                            .content_type()
                            .unwrap_or("application/octet-stream")
                            .to_owned(),
                        size: p.size() as u64,
                        content_id: p.content_id().map(str::to_owned),
                        is_inline: p.content_disposition() == Some("inline"),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}
```

Add to `impl JmapSeam`:

```rust
    /// Download a blob via the session `downloadUrl` (RFC 8620 §6.2; the URL
    /// template was origin-pinned at connect).
    pub(crate) async fn download_blob(&self, blob_id: &str) -> Result<Vec<u8>, JmapError> {
        Ok(self.client.download(blob_id).await?)
    }
```

- [ ] **Step 5: Bind `data_dir` for sync plugins**

In `backend/src/plugins/mod.rs`, after the `STORAGE` block, add:

```rust
static DATA_DIR: OnceLock<std::path::PathBuf> = OnceLock::new();

/// Bind the process-wide data directory (blob store root) used by protocol
/// plugins at sync time.
pub fn bind_data_dir(path: std::path::PathBuf) {
    let _ = DATA_DIR.set(path);
}

pub(crate) fn data_dir() -> Result<std::path::PathBuf, String> {
    DATA_DIR.get().cloned().ok_or_else(|| "data dir not bound".into())
}
```

In `backend/src/main.rs`, immediately after `plugins::bind_storage(db.clone());`, add:

```rust
    plugins::bind_data_dir(std::path::PathBuf::from(&config.data_dir));
```

- [ ] **Step 6: Wire downloads into the sync loop**

In `backend/src/sync/jmap_loop.rs`:

- Add `use std::path::Path;` to the imports and `use super::jmap_client::JmapAttachmentMeta;` alongside the existing `jmap_client` import.
- Change `run_jmap_sync`'s signature to take `data_dir: &Path` as the last parameter.
- In `jmap_sync_account`, resolve it via the plugin binding:

```rust
        let jmap_result = match crate::plugins::data_dir() {
            Ok(data_dir) => {
                run_jmap_sync(db, account_id, base_url, &email_address, &secret, &row.auth_type, &data_dir).await
            }
            Err(e) => Err(SyncError::Internal(e)),
        };
        match jmap_result {
            Ok(result) => result,
            Err(e) => {
                if e.is_auth() {
                    JmapSeam::evict(account_id);
                }
                tracing::warn!("JMAP sync failed ({e}), falling back to IMAP");
                …unchanged IMAP fallback…
            }
        }
```

- Replace every `persist_jmap_folder_batch(...)` call in `run_jmap_sync` / `full_folder_query` / `refetch_updated_emails` with `persist_and_download(...)`, threading `seam` and `data_dir` (the two helpers gain those parameters). Destructure the 3-tuple result inside `persist_and_download` instead.
- Add:

```rust
/// Cap per attachment blob (mirrors the IMAP lazy-fetch body cap).
const MAX_ATTACHMENT_DOWNLOAD_BYTES: u64 = 25 * 1024 * 1024;

/// Persist one page, then download attachments for newly-inserted messages
/// (blob bytes land in the content-addressed store under `data_dir`).
#[allow(clippy::too_many_arguments)]
async fn persist_and_download(
    db: &DbPool,
    account_id: &str,
    folder_id: &str,
    emails: &[JmapEmail],
    query_state: Option<&str>,
    seam: &JmapSeam,
    data_dir: &Path,
) -> Result<(usize, usize), SyncError> {
    let (new, updated, persisted) =
        persist_jmap_folder_batch(db, account_id, folder_id, emails, query_state).await?;
    // `persisted` is in the same order as `emails` (zip pairs them exactly).
    for (email, persisted) in emails.iter().zip(persisted.iter()) {
        if !persisted.was_new || email.attachments_meta.is_empty() {
            continue;
        }
        download_attachments(db, account_id, &persisted.local_id, &email.attachments_meta, seam, data_dir)
            .await?;
    }
    Ok((new, updated))
}

/// Download one message's attachment blobs into the blob store and persist
/// the attachment rows. Per-blob failures mark `flags.fetch_error` and never
/// abort the sync.
async fn download_attachments(
    db: &DbPool,
    account_id: &str,
    message_id: &str,
    attachments: &[JmapAttachmentMeta],
    seam: &JmapSeam,
    data_dir: &Path,
) -> Result<(), SyncError> {
    let mut extracted = Vec::new();
    for meta in attachments {
        if meta.size > MAX_ATTACHMENT_DOWNLOAD_BYTES {
            tracing::warn!(
                message_id,
                blob_id = %meta.blob_id,
                size = meta.size,
                "skipping oversized JMAP attachment"
            );
            super::recovery::mark_message_fetch_error(db, message_id, "attachment too large").await?;
            continue;
        }
        match seam.download_blob(&meta.blob_id).await {
            Ok(bytes) => extracted.push(crate::imap::ExtractedAttachment {
                filename: meta.filename.clone(),
                content_type: meta.content_type.clone(),
                data: bytes,
                content_id: meta.content_id.clone(),
                is_inline: meta.is_inline,
            }),
            Err(error) => {
                tracing::warn!(message_id, blob_id = %meta.blob_id, %error, "JMAP attachment download failed");
                super::recovery::mark_message_fetch_error(db, message_id, "attachment download failed")
                    .await?;
            }
        }
    }
    if !extracted.is_empty() {
        super::http::persist_attachments(db, data_dir, account_id, message_id, &extracted).await?;
    }
    Ok(())
}
```

- [ ] **Step 7: Run tests, format, lint, full suite**

Run:
```bash
cd backend && cargo test --bin lyra_backend jmap
cd backend && rustfmt --edition 2024 src/sync/jmap_client.rs src/sync/jmap_loop.rs src/sync/store.rs src/sync/mod.rs src/plugins/mod.rs src/main.rs && cargo fmt --check
cd backend && cargo clippy --all-targets --all-features 2>&1 | grep "warning:" | grep -v "oauth/config.rs"
cd backend && cargo test --bin lyra_backend 2>&1 | tail -20
```
Expected: scoped tests ok (incl. `jmap_persist_batch_reports_per_message_results`, `jmap_attachments_persist_to_blob_store`, `map_attachments_*`); fmt clean; clippy grep empty; suite green (3 pre-existing gpg failures allowed).

- [ ] **Step 8: Commit**

```bash
git add backend/src/sync backend/src/plugins/mod.rs backend/src/main.rs
git commit -m "feat: download JMAP attachments via blob endpoint"
```

---

### Task 4: Batched JMAP send with submission status, keep OpenGPG MIME

Commit: `feat: batched JMAP send with submission status, keep OpenGPG MIME`

**Files:**
- Modify: `backend/src/sync/jmap_client.rs` (send methods)
- Modify: `backend/src/sync/send.rs` (`deliver_jmap`, `prepare_jmap_send`)
- Modify: `backend/src/plugins/jmap_send.rs` (error classification)
- Modify: `backend/src/jmap.rs` (delete the orphaned send path)

- [ ] **Step 1: Write the failing seam tests**

Append to `backend/src/sync/jmap_client.rs`'s `mod tests` (imports needed at the top of the test fn or module: `use jmap_client::core::RequestParams; use jmap_client::core::set::SetRequest; use jmap_client::email::import::EmailImportRequest; use jmap_client::email_submission::EmailSubmission; use jmap_client::identity::Identity; use jmap_client::Method;`):

```rust
    // ── send path wire shapes (Task 4) ──────────────────────────────

    fn sample_outbound() -> crate::smtp::OutboundMessage {
        crate::smtp::OutboundMessage {
            from_email: "me@example.com".into(),
            from_name: Some("Me".into()),
            to: vec![(Some("You".into()), "you@example.com".into())],
            cc: vec![],
            bcc: vec![],
            subject: "Hi".into(),
            body_text: Some("Hello".into()),
            body_html: None,
            in_reply_to: None,
            references: None,
            mime_content_type: None,
            mime_body: None,
            attachments: Vec::new(),
            message_id: None,
        }
    }

    #[test]
    fn draft_email_serializes_jmap_create_shape() {
        let mut req = SetRequest::<Email<Set>>::new(RequestParams::new("acc", Method::SetEmail, 0));
        fill_outbound_email(
            req.create_with_id("draft"),
            &sample_outbound(),
            &["mb-drafts".to_owned()],
            &[],
        );
        let json = serde_json::to_value(&req).unwrap();
        let draft = &json["create"]["draft"];
        assert_eq!(draft["subject"], "Hi");
        assert_eq!(draft["keywords"]["$draft"], true);
        assert_eq!(draft["mailboxIds"]["mb-drafts"], true);
        assert_eq!(draft["from"][0]["name"], "Me");
        assert_eq!(draft["from"][0]["email"], "me@example.com");
        assert_eq!(draft["to"][0]["email"], "you@example.com");
        assert_eq!(draft["bodyValues"]["bd1"]["value"], "Hello");
        assert_eq!(draft["textBody"][0]["partId"], "bd1");
        assert_eq!(draft["textBody"][0]["type"], "text/plain");
    }

    #[test]
    fn draft_email_dual_body_and_threading_headers() {
        let mut outbound = sample_outbound();
        outbound.body_html = Some("<p>Hello</p>".into());
        outbound.in_reply_to = Some("<parent@example.com>".into());
        outbound.references = Some("<a@example.com> <b@example.com>".into());
        let mut req = SetRequest::<Email<Set>>::new(RequestParams::new("acc", Method::SetEmail, 0));
        fill_outbound_email(req.create_with_id("draft"), &outbound, &[], &[]);
        let json = serde_json::to_value(&req).unwrap();
        let draft = &json["create"]["draft"];
        assert_eq!(draft["bodyValues"]["bd2"]["value"], "<p>Hello</p>");
        assert_eq!(draft["htmlBody"][0]["partId"], "bd2");
        assert_eq!(draft["inReplyTo"][0], "<parent@example.com>");
        assert_eq!(draft["references"][1], "<b@example.com>");
    }

    #[test]
    fn draft_email_references_uploaded_attachment_blobs() {
        let uploaded = vec![UploadedAttachment {
            blob_id: "blob-1".into(),
            content_type: "application/pdf".into(),
            name: "invoice.pdf".into(),
        }];
        let mut req = SetRequest::<Email<Set>>::new(RequestParams::new("acc", Method::SetEmail, 0));
        fill_outbound_email(req.create_with_id("draft"), &sample_outbound(), &[], &uploaded);
        let json = serde_json::to_value(&req).unwrap();
        let draft = &json["create"]["draft"];
        assert_eq!(draft["attachments"][0]["blobId"], "blob-1");
        assert_eq!(draft["attachments"][0]["name"], "invoice.pdf");
        assert_eq!(draft["attachments"][0]["type"], "application/pdf");
    }

    #[test]
    fn on_success_patch_uses_full_value_replacement() {
        let mut req = SetRequest::<EmailSubmission<Set>>::new(RequestParams::new(
            "acc",
            Method::SetEmailSubmission,
            0,
        ));
        req.create_with_id("sub")
            .email_id("#draft")
            .identity_id("i1");
        fill_on_success_patch(req.arguments().on_success_update_email("sub"), Some("mb-sent"));
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["create"]["sub"]["emailId"], "#draft");
        assert_eq!(json["create"]["sub"]["identityId"], "i1");
        // Full-value replacement (RFC 8621 §7.5.1 semantics without patch-null):
        // we created the email in this same request with exactly `$draft`.
        assert_eq!(json["onSuccessUpdateEmail"]["#sub"]["keywords"], serde_json::json!({}));
        assert_eq!(json["onSuccessUpdateEmail"]["#sub"]["mailboxIds"]["mb-sent"], true);
    }

    #[test]
    fn import_request_serializes_rfc_shape() {
        let mut req = EmailImportRequest::new(RequestParams::new("acc", Method::ImportEmail, 0));
        let import = req.email("blob-mime");
        import.mailbox_ids(["mb-drafts"]);
        import.keywords(["$draft"]);
        let create_id = import.create_id();
        assert_eq!(create_id, "i0");
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["emails"]["i0"]["blobId"], "blob-mime");
        assert_eq!(json["emails"]["i0"]["mailboxIds"]["mb-drafts"], true);
        assert_eq!(json["emails"]["i0"]["keywords"]["$draft"], true);
    }

    #[test]
    fn pick_identity_prefers_matching_email() {
        let identities: Vec<Identity> = serde_json::from_value(serde_json::json!([
            { "id": "i1", "name": "Other", "email": "other@example.com" },
            { "id": "i2", "name": "Me", "email": "me@example.com" }
        ]))
        .unwrap();
        assert_eq!(pick_identity(&identities, "ME@example.com").as_deref(), Some("i2"));
        // No match → first identity.
        assert_eq!(pick_identity(&identities, "nobody@example.com").as_deref(), Some("i1"));
        assert!(pick_identity(&[], "me@example.com").is_none());
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd backend && cargo test --bin lyra_backend jmap_client 2>&1 | grep -E "^error" | head -20`
Expected: compile errors — `fill_outbound_email`, `fill_on_success_patch`, `UploadedAttachment`, `pick_identity` missing.

- [ ] **Step 3: Implement the seam send path**

In `backend/src/sync/jmap_client.rs`, extend imports:

```rust
use jmap_client::core::response::{
    EmailChangesResponse, EmailGetResponse, EmailSetResponse, EmailSubmissionSetResponse,
    IdentityGetResponse, MailboxGetResponse,
};
use jmap_client::email::import::EmailImportResponse;
use jmap_client::identity::Identity;
use crate::smtp::OutboundMessage;
```

(the `core::response` line replaces the Task-2 one; the rest are new lines.)

Add after `map_attachments`:

```rust
/// An attachment already uploaded to the server, referenced by blob id.
#[derive(Debug, Clone)]
struct UploadedAttachment {
    blob_id: String,
    content_type: String,
    name: String,
}

/// `EmailAddress` for a create (name+email tuple, or bare email).
fn crate_address(name: Option<&str>, email: &str) -> EmailAddress {
    match name {
        Some(n) if !n.is_empty() => EmailAddress::from((n.to_owned(), email.to_owned())),
        _ => EmailAddress::from(email.to_owned()),
    }
}

/// Fill the `Email/set` create object for a draft/submission (RFC 8621 §4.7).
/// Body parts: `bd1` text, `bd2` html when both exist; html-only uses `bd1`
/// as the html part.
fn fill_outbound_email(
    email: &mut Email<Set>,
    outbound: &OutboundMessage,
    mailbox_ids: &[String],
    uploaded: &[UploadedAttachment],
) {
    if !mailbox_ids.is_empty() {
        email.mailbox_ids(mailbox_ids.iter().map(String::as_str));
    }
    email.keywords(["$draft"]);
    email.from([crate_address(outbound.from_name.as_deref(), &outbound.from_email)]);
    email.to(
        outbound
            .to
            .iter()
            .map(|(n, e)| crate_address(n.as_deref(), e))
            .collect::<Vec<_>>(),
    );
    email.cc(
        outbound
            .cc
            .iter()
            .map(|(n, e)| crate_address(n.as_deref(), e))
            .collect::<Vec<_>>(),
    );
    email.bcc(
        outbound
            .bcc
            .iter()
            .map(|(n, e)| crate_address(n.as_deref(), e))
            .collect::<Vec<_>>(),
    );
    email.subject(outbound.subject.as_str());

    let text = outbound
        .body_text
        .clone()
        .or_else(|| outbound.body_html.clone())
        .unwrap_or_default();
    email.body_value("bd1".to_owned(), text.as_str());
    match (&outbound.body_text, &outbound.body_html) {
        (Some(_), Some(html)) => {
            email.text_body(EmailBodyPart::new().part_id("bd1").content_type("text/plain"));
            email.body_value("bd2".to_owned(), html.as_str());
            email.html_body(EmailBodyPart::new().part_id("bd2").content_type("text/html"));
        }
        (Some(_), None) | (None, None) => {
            email.text_body(EmailBodyPart::new().part_id("bd1").content_type("text/plain"));
        }
        (None, Some(_)) => {
            email.html_body(EmailBodyPart::new().part_id("bd1").content_type("text/html"));
        }
    }

    if let Some(irt) = &outbound.in_reply_to {
        email.in_reply_to([irt.clone()]);
    }
    if let Some(refs) = &outbound.references {
        email.references(refs.split_whitespace().map(str::to_owned).collect::<Vec<_>>());
    }

    for att in uploaded {
        email.attachment(
            EmailBodyPart::new()
                .blob_id(att.blob_id.clone())
                .name(att.name.clone())
                .content_type(att.content_type.clone()),
        );
    }
}

/// Post-submit patch as full-value replacement (RFC 8621 §7.5.1): the email
/// was created in this same request with exactly `$draft`, so replacing
/// `keywords` with the empty set is exactly "clear $draft", and replacing
/// `mailboxIds` is exactly the move to Sent (or mailbox-less when the account
/// has no Sent mailbox — the old client's behavior).
fn fill_on_success_patch(patch: &mut Email<Set>, sent_id: Option<&str>) {
    patch.keywords(Vec::<String>::new());
    patch.mailbox_ids(sent_id.into_iter().map(str::to_owned));
}

/// Pick the identity whose email matches `from` (case-insensitive), else the first.
fn pick_identity(identities: &[Identity], from: &str) -> Option<String> {
    identities
        .iter()
        .find(|i| i.email.as_deref().is_some_and(|e| e.eq_ignore_ascii_case(from)))
        .or_else(|| identities.first())
        .and_then(|i| i.id.clone())
}

/// First mailbox with `role` (server id).
fn mailbox_id_for_role(mailboxes: &[Mailbox<Get>], role: Role) -> Option<String> {
    mailboxes
        .iter()
        .find(|m| m.role() == role)
        .and_then(|m| m.id().map(str::to_owned))
}
```

Add to `impl JmapSeam`:

```rust
    /// Submit an outbound message. Attachments upload to `uploadUrl` first
    /// (blob upload is not a JMAP method call), then ONE batched request:
    /// `Email/set` create `#draft` + `EmailSubmission/set` with `#`
    /// back-references and an on-success patch (move to Sent, clear `$draft`).
    ///
    /// OpenGPG MIME-wrapped outbound (`mime_body` set) goes through
    /// `Email/import` of the uploaded RFC822 blob so the wrapper survives —
    /// an `Email/set` create would rebuild (and destroy) the MIME structure.
    pub(crate) async fn submit_outbound(&self, outbound: &OutboundMessage) -> Result<String, JmapError> {
        if !self.supports_submission() {
            return Err(JmapError::SessionDiscovery(
                "JMAP session does not advertise urn:ietf:params:jmap:submission".into(),
            ));
        }

        // Request A: identities + mailboxes (their results shape request B).
        let mut request_a = self.client.build();
        request_a.get_identity();
        request_a.get_mailbox();
        let mut responses = request_a.send().await?.unwrap_method_responses();
        if responses.len() != 2 {
            return Err(JmapError::InvalidResponse(format!(
                "expected Identity/get + Mailbox/get responses, got {}",
                responses.len()
            )));
        }
        let mailboxes = responses.remove(1).unwrap_get_mailbox()?.take_list();
        let identities = responses.remove(0).unwrap_get_identity()?.take_list();

        let identity_id = pick_identity(&identities, &outbound.from_email).ok_or_else(|| {
            JmapError::InvalidResponse(format!("no JMAP identity for {}", outbound.from_email))
        })?;
        let drafts_id = mailbox_id_for_role(&mailboxes, Role::Drafts);
        let sent_id = mailbox_id_for_role(&mailboxes, Role::Sent);

        if let Some(mime_body) = &outbound.mime_body {
            return self
                .submit_mime_wrapped(mime_body, &identity_id, drafts_id, sent_id, &mailboxes)
                .await;
        }

        let uploaded = self.upload_attachments(outbound).await?;

        // Request B: create + submit with creation-id references.
        let mut request = self.client.build();
        {
            let email_set = request.set_email();
            let create_boxes: Vec<String> = drafts_id.iter().cloned().collect();
            fill_outbound_email(email_set.create_with_id("draft"), outbound, &create_boxes, &uploaded);
        }
        {
            let sub_set = request.set_email_submission();
            sub_set
                .create_with_id("sub")
                .email_id("#draft")
                .identity_id(identity_id.as_str());
            fill_on_success_patch(sub_set.arguments().on_success_update_email("sub"), sent_id.as_deref());
        }
        let mut responses = request.send().await?.unwrap_method_responses();
        if responses.len() != 2 {
            return Err(JmapError::InvalidResponse(format!(
                "expected Email/set + EmailSubmission/set responses, got {}",
                responses.len()
            )));
        }
        let mut sub_resp = responses.remove(1).unwrap_set_email_submission()?;
        let mut email_resp = responses.remove(0).unwrap_set_email()?;
        // notCreated surfaces here as Error::Set (→ JmapError::Client).
        email_resp.created("draft")?;
        let mut submission = sub_resp.created("sub")?;
        Ok(submission.take_id())
    }

    /// OpenGPG path: upload the RFC822 MIME blob, `Email/import` into Drafts,
    /// submit — the signed/encrypted MIME wrapper survives.
    async fn submit_mime_wrapped(
        &self,
        mime_body: &str,
        identity_id: &str,
        drafts_id: Option<String>,
        sent_id: Option<String>,
        mailboxes: &[Mailbox<Get>],
    ) -> Result<String, JmapError> {
        let import_mailbox = drafts_id
            .as_deref()
            .or(sent_id.as_deref())
            .or_else(|| mailboxes.iter().find_map(|m| m.id()))
            .ok_or_else(|| JmapError::InvalidResponse("no mailbox to import into".into()))?
            .to_owned();

        let blob_id = self
            .client
            .upload(None, mime_body.as_bytes().to_vec(), Some("message/rfc822"))
            .await?
            .take_blob_id();

        let mut request = self.client.build();
        let import_create_id = {
            let import_req = request.import_email();
            let import = import_req.email(blob_id);
            import.mailbox_ids([import_mailbox.as_str()]);
            import.keywords(["$draft"]);
            import.create_id()
        };
        {
            let sub_set = request.set_email_submission();
            sub_set
                .create_with_id("sub")
                .email_id(format!("#{import_create_id}"))
                .identity_id(identity_id);
            fill_on_success_patch(sub_set.arguments().on_success_update_email("sub"), sent_id.as_deref());
        }
        let mut responses = request.send().await?.unwrap_method_responses();
        if responses.len() != 2 {
            return Err(JmapError::InvalidResponse(format!(
                "expected Email/import + EmailSubmission/set responses, got {}",
                responses.len()
            )));
        }
        let mut sub_resp = responses.remove(1).unwrap_set_email_submission()?;
        let mut import_resp = responses.remove(0).unwrap_import_email()?;
        import_resp.created(&import_create_id)?;
        let mut submission = sub_resp.created("sub")?;
        Ok(submission.take_id())
    }

    /// Upload each outbound attachment to the session `uploadUrl`
    /// (RFC 8620 §6.1), returning blob ids for the `Email/set` create.
    async fn upload_attachments(
        &self,
        outbound: &OutboundMessage,
    ) -> Result<Vec<UploadedAttachment>, JmapError> {
        let mut uploaded = Vec::new();
        for att in &outbound.attachments {
            let bytes = att.decode().map_err(|e| {
                JmapError::InvalidResponse(format!("attachment {}: {}", att.filename, e))
            })?;
            let blob_id = self
                .client
                .upload(None, bytes, Some(att.content_type.as_str()))
                .await?
                .take_blob_id();
            uploaded.push(UploadedAttachment {
                blob_id,
                content_type: att.content_type.clone(),
                name: att.filename.clone(),
            });
        }
        Ok(uploaded)
    }
```

- [ ] **Step 4: Rewire `send.rs` and `plugins/jmap_send.rs`**

In `backend/src/sync/send.rs`, replace `deliver_jmap` and `prepare_jmap_send`:

```rust
/// Submit an outbound message through the JMAP seam (cached session; batched
/// create+submit; OpenGPG MIME via Email/import).
pub(crate) async fn deliver_jmap(
    account_id: &str,
    jmap_base_url: &str,
    email: &str,
    password: &str,
    auth_type: &str,
    outbound: OutboundMessage,
) -> Result<String, SyncError> {
    let seam = crate::sync::jmap_client::JmapSeam::connect_for_account(
        account_id,
        jmap_base_url,
        email,
        password,
        auth_type,
    )
    .await?;
    Ok(seam.submit_outbound(&outbound).await?)
}

/// Load JMAP settings for `account_id` and build an outbound message from raw source.
pub(crate) async fn prepare_jmap_send(
    db: &DbPool,
    account_id: &str,
    raw: &str,
) -> Result<(String, String, String, String, OutboundMessage), SyncError> {
    let mut probe = Sq::select();
    probe
        .columns([
            mail_account::Column::EmailAddress,
            mail_account::Column::UserId,
            mail_account::Column::JmapBaseUrl,
            mail_account::Column::AuthType,
            mail_account::Column::IsActive,
        ])
        .from(mail_account::Entity)
        .and_where(mail_account::Column::Id.eq(id_value(db, account_id)?));
    let row = db
        .orm()
        .query_one(&probe)
        .await
        .map_err(orm_err)?
        .ok_or(SyncError::AccountNotFound)?;

    let (is_active, jmap_base_url, email_address, auth_type, user_id) = (
        row.try_get::<bool>("", "is_active").map_err(orm_err)?,
        row.try_get::<Option<String>>("", "jmap_base_url")
            .map_err(orm_err)?,
        row.try_get::<String>("", "email_address")
            .map_err(orm_err)?,
        row.try_get::<String>("", "auth_type").map_err(orm_err)?,
        row_id(&row, "user_id")?,
    );
    if !is_active {
        return Err(SyncError::AccountDisabled);
    }

    let base_url = jmap_base_url
        .ok_or_else(|| SyncError::InvalidInput("JMAP base URL not configured".into()))?;

    let (dek, credential_json) =
        crate::auth::AuthState::get_user_dek_and_credential(db, &user_id, account_id)
            .await
            .map_err(|e| SyncError::Crypto(e.to_string()))?;
    let password = crate::sync::jmap_client::decrypt_account_password(&credential_json, &dek)?;

    let outbound = outbound_from_raw(email_address.clone(), raw)?;
    Ok((base_url, email_address, password, auth_type, outbound))
}
```

In `backend/src/plugins/jmap_send.rs`, replace the `send` body and `jmap_send_err`:

```rust
    async fn send(&self, account_id: &str, raw: &str) -> Result<(), String> {
        let db = super::storage()?;
        let (base_url, email, password, auth_type, outbound) =
            crate::sync::prepare_jmap_send(&db, account_id, raw)
                .await
                .map_err(jmap_send_err)?;
        crate::sync::deliver_jmap(account_id, &base_url, &email, &password, &auth_type, outbound)
            .await
            .map(|_| ())
            .map_err(jmap_send_err)
    }
```

```rust
fn jmap_send_err(err: crate::sync::SyncError) -> String {
    match err {
        crate::sync::SyncError::Jmap(jmap) => {
            if jmap.is_auth() {
                "JMAP permanent: authentication failed".into()
            } else if jmap.is_transient() {
                "JMAP transient".into()
            } else {
                format!("JMAP permanent: {jmap}")
            }
        }
        other => other.to_string(),
    }
}
```

(`"JMAP transient"` feeds the existing capped-backoff reschedule in `jobs.rs::handle_send_message`; categories are unchanged.)

- [ ] **Step 5: Delete the orphaned send path from `jmap.rs`**

Delete from `backend/src/jmap.rs`:

- `JmapClient::submit_email`, `JmapClient::list_identities`, `JmapClient::upload_blob`
- helpers `pick_identity`, `mailbox_id_for_role`, `build_email_create`, `jmap_address`; struct `UploadedAttachment`; struct `JmapIdentity`
- tests `pick_identity_prefers_matching_email`, `build_email_create_sets_draft_keywords_and_recipients`, `build_email_create_references_uploaded_attachments`

Run: `cd backend && cargo check`
Expected: `Finished`. Remove any import the compiler now flags as unused (e.g. `OutboundMessage` in jmap.rs).

- [ ] **Step 6: Run tests, format, lint**

Run:
```bash
cd backend && cargo test --bin lyra_backend jmap
cd backend && rustfmt --edition 2024 src/sync/jmap_client.rs src/sync/send.rs src/plugins/jmap_send.rs src/jmap.rs && cargo fmt --check
cd backend && cargo clippy --all-targets --all-features 2>&1 | grep "warning:" | grep -v "oauth/config.rs"
```
Expected: seam send tests green; fmt clean; clippy grep empty.

- [ ] **Step 7: Run the full suite**

Run: `cd backend && cargo test --bin lyra_backend 2>&1 | tail -20`
Expected: green (3 pre-existing gpg failures allowed).

- [ ] **Step 8: Commit**

```bash
git add backend/src/sync/jmap_client.rs backend/src/sync/send.rs backend/src/plugins/jmap_send.rs backend/src/jmap.rs
git commit -m "feat: batched JMAP send with submission status, keep OpenGPG MIME"
```

---

### Task 5: JMAP push via the crate EventSource stream

Commit: `feat: JMAP push via crate EventSource stream`

**Files:**
- Modify: `backend/src/sync/jmap_client.rs` (`wait_for_state_change`, `push_implies_sync`, `EventSourceOutcome`)
- Modify: `backend/src/jmap_push.rs` (rewire)
- Modify: `backend/src/jmap.rs` (delete the hand SSE parser)

- [ ] **Step 1: Write the failing test**

Append to `backend/src/sync/jmap_client.rs`'s `mod tests`:

```rust
    // ── push classification (Task 5) ────────────────────────────────

    #[test]
    fn push_state_change_implies_sync() {
        use jmap_client::event_source::Changes;

        let changes: Changes = serde_json::from_value(serde_json::json!({
            "id": null,
            "changes": { "a1": { "Email": "s1", "Mailbox": "m2" } }
        }))
        .unwrap();
        assert!(push_implies_sync(&PushNotification::StateChange(changes)));

        let empty: Changes = serde_json::from_value(serde_json::json!({
            "id": null,
            "changes": { "a1": {} }
        }))
        .unwrap();
        assert!(!push_implies_sync(&PushNotification::StateChange(empty)));

        let unrelated: Changes = serde_json::from_value(serde_json::json!({
            "id": null,
            "changes": { "a1": { "Quota": "q1" } }
        }))
        .unwrap();
        assert!(!push_implies_sync(&PushNotification::StateChange(unrelated)));
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cd backend && cargo test --bin lyra_backend jmap_client 2>&1 | grep -E "^error" | head -10`
Expected: compile errors — `push_implies_sync`, `PushNotification` missing.

- [ ] **Step 3: Implement the seam push support**

In `backend/src/sync/jmap_client.rs`, extend imports:

```rust
use futures_util::StreamExt;
use jmap_client::event_source::PushNotification;
use jmap_client::{DataType, Get, Set, URI};
```

(i.e., add `DataType` to the root import; the other two lines are new.)

Add the moved `EventSourceOutcome` (from `jmap.rs`) after the DTO block:

```rust
/// Outcome of waiting on a JMAP EventSource stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventSourceOutcome {
    StateChanged,
    Unsupported,
    StreamEnded,
}
```

Add after `mailbox_id_for_role`:

```rust
/// Whether a push notification carries a mail-relevant state change
/// (ping frames are already filtered by the crate's SSE parser).
pub(crate) fn push_implies_sync(notification: &PushNotification) -> bool {
    match notification {
        PushNotification::StateChange(changes) => [
            DataType::Email,
            DataType::Mailbox,
            DataType::Thread,
            DataType::EmailSubmission,
        ]
        .iter()
        .any(|t| changes.has_type(t.clone())),
        PushNotification::CalendarAlert(_) => false,
    }
}
```

Add to `impl JmapSeam`:

```rust
    /// Open the session EventSource (`types=*`, `closeafter=no`, `ping=30`)
    /// and wait for the first mail-relevant state change. The stream itself
    /// has no read timeout — the crate times only the connect, which fixes
    /// the old bug where the 30s request timeout killed the stream cyclically.
    pub(crate) async fn wait_for_state_change(&self) -> Result<EventSourceOutcome, JmapError> {
        if self.client.session().event_source_url().is_empty() {
            return Ok(EventSourceOutcome::Unsupported);
        }
        let mut stream = self
            .client
            .event_source(None::<Vec<DataType>>, false, Some(30), None)
            .await?;
        while let Some(item) = stream.next().await {
            match item {
                Ok(notification) => {
                    if push_implies_sync(&notification) {
                        return Ok(EventSourceOutcome::StateChanged);
                    }
                }
                Err(err) => return Err(JmapError::from(err)),
            }
        }
        Ok(EventSourceOutcome::StreamEnded)
    }
```

- [ ] **Step 4: Rewire `jmap_push.rs`**

In `backend/src/jmap_push.rs`:

- Replace `use crate::jmap::{EventSourceOutcome, JmapClient};` with `use crate::sync::jmap_client::{EventSourceOutcome, JmapError, JmapSeam};`
- Add `auth_type` to `PushAccount`:

```rust
struct PushAccount {
    id: String,
    user_id: String,
    email_address: String,
    auth_type: String,
    credential: String,
    jmap_base_url: String,
}
```

- In `list_jmap_push_candidates`, add `mail_account::Column::AuthType` to the `.columns([…])` list (after `EmailAddress`), widen the tuple type to six elements, and add `auth_type` to the row decode + `PushAccount` construction:

```rust
    let tuples: Vec<(String, String, String, String, String, Option<String>)> = rows
        .iter()
        .map(|row| {
            Ok((
                row_id(row, "id")?,
                row_id(row, "user_id")?,
                row.try_get("", "email_address").map_err(orm_err)?,
                row.try_get("", "auth_type").map_err(orm_err)?,
                row.try_get("", "credential").map_err(orm_err)?,
                row.try_get("", "jmap_base_url").map_err(orm_err)?,
            ))
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()?;

    Ok(tuples
        .into_iter()
        .filter_map(|(id, user_id, email_address, auth_type, credential, jmap_base_url)| {
            let jmap_base_url = jmap_base_url.filter(|u| !u.is_empty())?;
            Some(PushAccount {
                id,
                user_id,
                email_address,
                auth_type,
                credential,
                jmap_base_url,
            })
        })
        .collect())
```

- Replace `watch_once`'s tail (the dek/decrypt/discover lines) with:

```rust
async fn watch_once(db: &DbPool, account: &PushAccount) -> Result<EventSourceOutcome, JmapError> {
    match has_pending_or_running_sync(db, &account.id).await {
        Ok(true) => {
            tokio::time::sleep(Duration::from_secs(5)).await;
            return Ok(EventSourceOutcome::StreamEnded);
        }
        Ok(false) => {}
        Err(e) => {
            tracing::warn!(account_id = %account.id, error = %e, "JMAP push: job status check failed");
            tokio::time::sleep(RECONNECT_DELAY).await;
            return Ok(EventSourceOutcome::StreamEnded);
        }
    }

    let dek = crate::auth::AuthState::get_user_dek(db, &account.user_id).await?;
    let secret = crate::sync::jmap_client::decrypt_account_password(&account.credential, &dek)?;
    let seam = match JmapSeam::connect_for_account(
        &account.id,
        &account.jmap_base_url,
        &account.email_address,
        &secret,
        &account.auth_type,
    )
    .await
    {
        Ok(seam) => seam,
        Err(error) => {
            if error.is_auth() {
                JmapSeam::evict(&account.id);
            }
            return Err(error);
        }
    };
    seam.wait_for_state_change().await
}
```

The supervisor loop (`run_account_push_loop`), backoff constants, and `enqueue_sync_if_idle` are unchanged. The existing test module keeps compiling (`EventSourceOutcome` variants unchanged).

- [ ] **Step 5: Delete the hand SSE parser from `jmap.rs`**

Delete from `backend/src/jmap.rs`:

- `JmapClient::wait_event_source_state`, `JmapClient::event_source_url_expanded`
- helpers `expand_event_source_url`, `sse_frame_is_state_push`
- the old `EventSourceOutcome` enum (the seam's is the only one now)
- tests `expand_event_source_url_fills_rfc_placeholders`, `sse_frame_detects_state_event`

Run: `cd backend && cargo check`
Expected: `Finished`.

- [ ] **Step 6: Run tests, format, lint, full suite, commit**

Run:
```bash
cd backend && cargo test --bin lyra_backend jmap
cd backend && rustfmt --edition 2024 src/sync/jmap_client.rs src/jmap_push.rs src/jmap.rs && cargo fmt --check
cd backend && cargo clippy --all-targets --all-features 2>&1 | grep "warning:" | grep -v "oauth/config.rs"
cd backend && cargo test --bin lyra_backend 2>&1 | tail -20
git add backend/src/sync/jmap_client.rs backend/src/jmap_push.rs backend/src/jmap.rs
git commit -m "feat: JMAP push via crate EventSource stream"
```

Expected: all gates green (3 pre-existing gpg failures allowed).

---

### Task 6: Push flags/moves for JMAP accounts; probe via session capability

Commit: `feat: push flags/moves for JMAP accounts; probe via Core/echo`

(The commit message is the spec's; the probe is connect + capability check because 0.4.2 cannot emit `Core/echo` — see Deviations §2.)

**Files:**
- Modify: `backend/src/sync/jmap_client.rs` (`set_email_keywords`, `set_email_mailboxes`, `create_draft`, `destroy_email`)
- Modify: `backend/src/sync/http.rs` (connect helper, patch/move/drafts arms)
- Modify: `backend/src/accounts.rs` (probe + cache eviction)
- Modify: `backend/src/jmap.rs` (delete the rest of the old client)

- [ ] **Step 1: Write the failing seam test**

Append to `backend/src/sync/jmap_client.rs`'s `mod tests`:

```rust
    // ── flags push wire shape (Task 6) ──────────────────────────────

    #[test]
    fn keyword_update_serializes_seen_and_flagged_patch() {
        let mut req = SetRequest::<Email<Set>>::new(RequestParams::new("acc", Method::SetEmail, 0));
        fill_keyword_update(req.update("em1"), Some(true), Some(false));
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["update"]["em1"]["keywords/$seen"], true);
        assert_eq!(json["update"]["em1"]["keywords/$flagged"], false);
    }

    #[test]
    fn keyword_update_skips_absent_flags() {
        let mut req = SetRequest::<Email<Set>>::new(RequestParams::new("acc", Method::SetEmail, 0));
        fill_keyword_update(req.update("em1"), None, Some(true));
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["update"]["em1"], serde_json::json!({ "keywords/$flagged": true }));
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cd backend && cargo test --bin lyra_backend jmap_client 2>&1 | grep -E "^error" | head -10`
Expected: compile error — `fill_keyword_update` missing.

- [ ] **Step 3: Add the seam write methods**

In `backend/src/sync/jmap_client.rs`, add after `push_implies_sync`:

```rust
/// Fill an `Email/set` update with keyword patches for read/starred.
/// `is_read = true` sets `$seen`; `is_starred = true` sets `$flagged`.
fn fill_keyword_update(email: &mut Email<Set>, is_read: Option<bool>, is_starred: Option<bool>) {
    if let Some(read) = is_read {
        email.keyword("$seen", read);
    }
    if let Some(star) = is_starred {
        email.keyword("$flagged", star);
    }
}
```

Add to `impl JmapSeam`:

```rust
    /// Push read/starred flags: one `Email/set` update patching the
    /// `$seen`/`$flagged` keywords. `None` flags are left untouched.
    pub(crate) async fn set_email_keywords(
        &self,
        email_id: &str,
        is_read: Option<bool>,
        is_starred: Option<bool>,
    ) -> Result<(), JmapError> {
        if is_read.is_none() && is_starred.is_none() {
            return Ok(());
        }
        let mut request = self.client.build();
        {
            let set = request.set_email();
            fill_keyword_update(set.update(email_id), is_read, is_starred);
        }
        let mut resp = request.send_single::<EmailSetResponse>().await?;
        resp.updated(email_id)?; // notUpdated surfaces here
        Ok(())
    }

    /// Move an email to exactly these mailboxes: full `mailboxIds`
    /// replacement (RFC 8621 §4.4).
    pub(crate) async fn set_email_mailboxes(
        &self,
        email_id: &str,
        mailbox_ids: &[String],
    ) -> Result<(), JmapError> {
        if mailbox_ids.is_empty() {
            return Err(JmapError::InvalidResponse(
                "move requires at least one destination mailbox".into(),
            ));
        }
        self.client
            .email_set_mailboxes(email_id, mailbox_ids.iter().cloned())
            .await?;
        Ok(())
    }

    /// Create a draft Email (no submission) in the Drafts mailbox; returns
    /// the server id.
    pub(crate) async fn create_draft(&self, outbound: &OutboundMessage) -> Result<String, JmapError> {
        let mailboxes = self.list_mailboxes().await?;
        let drafts_id = mailboxes
            .iter()
            .find(|m| m.role.as_deref() == Some("drafts"))
            .map(|m| m.id.clone())
            .ok_or_else(|| JmapError::InvalidResponse("no drafts mailbox on this account".into()))?;
        let mut request = self.client.build();
        {
            let set = request.set_email();
            fill_outbound_email(set.create_with_id("draft"), outbound, std::slice::from_ref(&drafts_id), &[]);
        }
        let mut resp = request.send_single::<EmailSetResponse>().await?;
        let mut created = resp.created("draft")?;
        Ok(created.take_id())
    }

    /// Destroy an Email server-side (draft cleanup after send/discard).
    pub(crate) async fn destroy_email(&self, email_id: &str) -> Result<(), JmapError> {
        Ok(self.client.email_destroy(email_id).await?)
    }
```

- [ ] **Step 4: Rewire `connect_jmap_for_account` + the flags push in `http.rs`**

In `backend/src/sync/http.rs`, replace `connect_jmap_for_account`:

```rust
/// Connect the cached JMAP seam for an account (discovers on cache miss).
pub(crate) async fn connect_jmap_for_account(
    db: &DbPool,
    user_id: &str,
    account_id: &str,
) -> Result<std::sync::Arc<crate::sync::jmap_client::JmapSeam>, SyncError> {
    let (dek, credential_json) =
        crate::auth::AuthState::get_user_dek_and_credential(db, user_id, account_id)
            .await
            .map_err(|e| SyncError::Crypto(e.to_string()))?;
    let acct_value = id_value(db, account_id)?;
    let user_value = id_value(db, user_id)?;

    let row = query_first(db, |q| {
        q.expr_as(
            Expr::col(mail_account::Column::JmapBaseUrl),
            Alias::new("jmap_base_url"),
        )
        .expr_as(
            Expr::col(mail_account::Column::EmailAddress),
            Alias::new("email_address"),
        )
        .expr_as(
            Expr::col(mail_account::Column::AuthType),
            Alias::new("auth_type"),
        )
        .from(mail_account::Entity)
        .and_where(Expr::col(mail_account::Column::Id).eq(acct_value))
        .and_where(Expr::col(mail_account::Column::UserId).eq(user_value))
        .and_where(Expr::col(mail_account::Column::IsActive).eq(true));
    })
    .await?
    .ok_or(SyncError::AccountNotFound)?;

    let jmap_base_url: Option<String> = row.try_get("", "jmap_base_url").map_err(orm_err)?;
    let email_address: String = row.try_get("", "email_address").map_err(orm_err)?;
    let auth_type: String = row.try_get("", "auth_type").map_err(orm_err)?;
    let base_url = jmap_base_url
        .ok_or_else(|| SyncError::InvalidInput("JMAP base URL not configured".into()))?;
    let password = crate::sync::jmap_client::decrypt_account_password(&credential_json, &dek)?;
    Ok(crate::sync::jmap_client::JmapSeam::connect_for_account(
        account_id,
        &base_url,
        &email_address,
        &password,
        &auth_type,
    )
    .await?)
}
```

In `patch_message`, change the IMAP-only flag push into a two-arm dispatch — replace the block starting `if row.protocol == "imap" && (body.is_read.is_some() || body.is_starred.is_some()) {` (its IMAP body stays byte-identical) and add the JMAP arm after it:

```rust
    if row.protocol == "imap" && (body.is_read.is_some() || body.is_starred.is_some()) {
        …unchanged IMAP arm…
    } else if row.protocol == "jmap" && (body.is_read.is_some() || body.is_starred.is_some()) {
        let email_id = row
            .external_id
            .as_deref()
            .ok_or_else(|| SyncError::InvalidInput("JMAP message has no server id".into()))?;
        let seam = connect_jmap_for_account(db, &user_id, &row.account_id).await?;
        if let Err(err) = seam
            .set_email_keywords(email_id, body.is_read, body.is_starred)
            .await
        {
            if err.is_auth() {
                crate::sync::jmap_client::JmapSeam::evict(&row.account_id);
            }
            return Err(err.into());
        }
    }
```

Update the `patch_message` doc comment to `/// PATCH /api/v1/messages/{id} — update read/starred flags (IMAP STORE / JMAP Email\set keywords).`

- [ ] **Step 5: Rewire move/drafts in `http.rs`**

In `apply_message_move`, replace the `"jmap"` match arm with:

```rust
        "jmap" => {
            let email_id = row
                .external_id
                .as_deref()
                .ok_or_else(|| SyncError::InvalidInput("JMAP message has no server id".into()))?;
            let mailbox_id = dest_external.clone().ok_or_else(|| {
                SyncError::InvalidInput("JMAP target folder has no server id".into())
            })?;
            let seam = connect_jmap_for_account(db, user_id, &row.account_id).await?;
            seam.set_email_mailboxes(email_id, &[mailbox_id]).await?;
        }
```

In `save_draft`, replace the `"jmap"` match arm with:

```rust
        "jmap" => {
            let seam = connect_jmap_for_account(db, &user_id, &body.account_id).await?;
            let server_id = seam.create_draft(&outbound).await?;
            if let Some(old) = &old {
                if let Some(ext) = old.external_id.as_deref() {
                    let _ = seam.destroy_email(ext).await;
                }
                soft_delete_message_row(db, &old.id).await?;
                update_folder_counts(db, &old.folder_id).await?;
            }
            let local_id =
                upsert_jmap_draft_row(db, &body.account_id, &dest_id, &server_id, &outbound)
                    .await?;
            Ok(Json(serde_json::json!({
                "status": "saved",
                "draftMessageId": local_id,
            })))
        }
```

In `discard_draft`, replace the `"jmap"` match arm with:

```rust
        "jmap" => {
            let email_id = row
                .external_id
                .as_deref()
                .ok_or_else(|| SyncError::InvalidInput("JMAP draft has no server id".into()))?;
            let seam = connect_jmap_for_account(db, &user_id, &row.account_id).await?;
            seam.destroy_email(email_id).await?;
        }
```

Also update the `save_draft` doc comment's JMAP sentence to: `JMAP: Email/set create with $draft (+ destroy of the replaced draft) and a direct local row upsert.` (unchanged semantics — the client is now the seam).

- [ ] **Step 6: Probe + cache eviction in `accounts.rs`**

In `create_account`: move the `let auth_type = body.auth_type.unwrap_or_else(|| "password".into());` line ABOVE the `choose_send_protocol(...)` call (it currently follows it), and pass it:

```rust
    let auth_type = body.auth_type.unwrap_or_else(|| "password".into());
    let send_protocol = choose_send_protocol(
        &protocol,
        jmap_base_url.as_deref(),
        &body.email_address,
        &body.password,
        &auth_type,
    )
    .await;
```

(delete the old standalone `let auth_type = …` line after the call.)

Replace `choose_send_protocol`:

```rust
/// Prefer JMAP EmailSubmission when the session advertises it; otherwise SMTP.
///
/// The probe is a session connect + capability check: `jmap-client` 0.4.2
/// cannot emit `Core/echo` (no Arguments variant), and the connect performs
/// the same authenticated round trip the retired probe did.
async fn choose_send_protocol(
    protocol: &str,
    jmap_base_url: Option<&str>,
    email: &str,
    password: &str,
    auth_type: &str,
) -> String {
    if protocol != "jmap" {
        return "smtp".into();
    }
    let Some(base) = jmap_base_url else {
        return "smtp".into();
    };
    match crate::sync::jmap_client::JmapSeam::connect_ephemeral(base, email, password, auth_type)
        .await
    {
        Ok(seam) if seam.supports_submission() => {
            tracing::info!(%email, "JMAP submission capability present; send_protocol=jmap");
            "jmap".into()
        }
        Ok(_) => {
            tracing::info!(%email, "JMAP session lacks submission; send_protocol=smtp");
            "smtp".into()
        }
        Err(err) => {
            tracing::warn!(%email, error = %err, "JMAP probe failed; send_protocol=smtp");
            "smtp".into()
        }
    }
}
```

In `update_account`, immediately after the successful `.exec(&conn)` (before `find_account`), add:

```rust
    // A credential/host change invalidates the cached JMAP session.
    crate::sync::jmap_client::JmapSeam::evict(&id);
```

In `delete_account`, capture the string id before the `id_value` shadowing and evict after the `rows_affected` check:

```rust
    let account_id = id.clone();
    let id = id_value(db, &id)?;
    let user_id = id_value(db, &user_id)?;
    …unchanged delete…
    if result.rows_affected == 0 {
        return Err(AccountError::NotFound);
    }
    crate::sync::jmap_client::JmapSeam::evict(&account_id);
```

- [ ] **Step 7: Delete the rest of the old client from `jmap.rs`**

Delete from `backend/src/jmap.rs`:

- `JmapClient` (struct + all remaining methods: `discover`, `from_session`, `account_id`, `api_url`, `has_capability`, `supports_submission`, `list_mailboxes`, `create_draft`, `destroy_email`, `set_email_mailboxes`, `send_request`)
- `JmapSession`, `PrimaryAccounts`, `JmapRequest`, `JmapResponse`, `MethodCall`, `MethodResponse`, `JmapSyncState`, `check_session_urls`, `take_ok_args`, `take_ok_args_ref`, `jmap_set_error`, `probe_jmap`
- the remaining tests: `session_urls_same_origin_accepted`, `session_urls_cross_origin_rejected`, `session_urls_unparseable_rejected`, `take_ok_args_maps_jmap_error_method`, `take_ok_args_returns_matching_method`, `take_ok_args_picks_named_method_among_several`, `session_supports_submission_capability`
- the `use crate::sync::jmap_client::resolve_discovery_redirect;` import (its only consumer was `discover`)

`jmap.rs` now contains only its module doc and:

```rust
//! Legacy module path — re-exports kept until the remaining callers migrate
//! (deleted wholesale in the transport-removal commit).

pub use crate::sync::jmap_client::{
    JmapEmail, JmapEmailAddress, JmapError, JmapMailbox, decrypt_account_password,
};
```

Run: `cd backend && cargo check`
Expected: `Finished`. Fix any unused-import warnings the compiler reports in `jmap.rs` (e.g. `reqwest::Client`, `base64`).

- [ ] **Step 8: Run tests, format, lint**

Run:
```bash
cd backend && cargo test --bin lyra_backend jmap
cd backend && rustfmt --edition 2024 src/sync/jmap_client.rs src/sync/http.rs src/accounts.rs src/jmap.rs && cargo fmt --check
cd backend && cargo clippy --all-targets --all-features 2>&1 | grep "warning:" | grep -v "oauth/config.rs"
```
Expected: green; fmt clean; clippy grep empty.

- [ ] **Step 9: Full suite + commit**

Run: `cd backend && cargo test --bin lyra_backend 2>&1 | tail -20`
Expected: green (3 pre-existing gpg failures allowed).

```bash
git add backend/src/sync/jmap_client.rs backend/src/sync/http.rs backend/src/accounts.rs backend/src/jmap.rs
git commit -m "feat: push flags/moves for JMAP accounts; probe via Core/echo"
```

---

### Task 7: Delete the hand-rolled transport; docs; final gates

Commit: `refactor: delete hand-rolled JMAP transport`

**Files:**
- Delete: `backend/src/jmap.rs`
- Modify: `backend/src/main.rs`, `backend/src/sync/types.rs`, `backend/src/sync/store.rs`, `backend/src/jobs.rs`, `backend/src/sync/jmap_client.rs` (prune legacy error variant)
- Modify: `AGENTS.md`, `docs/specs/2026-08-20-lyra-sync-and-protocols-spec.md`

- [ ] **Step 1: Delete `jmap.rs` and repoint the remaining references**

- Delete `backend/src/jmap.rs`.
- In `backend/src/main.rs`, delete the `mod jmap;` line (between `mod imap_idle;` and `mod jmap_push;`).
- In `backend/src/sync/types.rs`, change `use crate::jmap::JmapError;` to `use crate::sync::jmap_client::JmapError;`.
- In `backend/src/sync/store.rs`, change the three type references: `mailbox: &crate::jmap::JmapMailbox` → `mailbox: &crate::sync::jmap_client::JmapMailbox`; `emails: &[crate::jmap::JmapEmail]` → `emails: &[crate::sync::jmap_client::JmapEmail]` (both in `persist_jmap_folder_batch` and `upsert_jmap_message_in_tx`).
- In `backend/src/jobs.rs`, change `SyncError::Jmap(crate::jmap::JmapError::Authentication(_))` to `SyncError::Jmap(crate::sync::jmap_client::JmapError::Authentication(_))` (in `sanitize_error`), and in its test change `SyncError::Jmap(crate::jmap::JmapError::SessionDiscovery(…))` to the same `crate::sync::jmap_client::JmapError` path. The whitelist categories stay byte-identical.

Run: `cd backend && cargo check 2>&1 | grep -E "^error" | head`
Expected: empty (green). If anything still references `crate::jmap::`, repoint it to `crate::sync::jmap_client::` — do not re-create the file.

- [ ] **Step 2: Prune the legacy error variant**

In `backend/src/sync/jmap_client.rs`:

- Delete the `Method { code, description }` variant from `JmapError` (nothing constructs it anymore).
- Delete the legacy `Self::Method { code, .. }` arm from `is_stale_query_state`.
- In the tests, delete the legacy arm portion of `stale_query_state_detects_rfc_code` (the `let legacy = …` + its assert).

Run: `cd backend && cargo check && cargo test --bin lyra_backend jmap`
Expected: green.

- [ ] **Step 3: Update `AGENTS.md`**

Two edits (keeping the file's current style):

1. In the project map, change

```
      sync/                     ← sync HTTP, IMAP/JMAP loops, persist transactions
      imap.rs / jmap.rs / smtp.rs
```

to

```
      sync/                     ← sync HTTP, IMAP/JMAP loops, JMAP seam (jmap_client.rs), persist transactions
      imap.rs / smtp.rs
```

2. In the "Stack (locked for v1)" table, add a row after `| Backend | Rust + Axum |`:

```
| JMAP client | `jmap-client` 0.4.2 (Stalwart Labs; `async` + `aws_lc_rs` features, WebSocket off — brings reqwest 0.13 alongside 0.12) |
```

- [ ] **Step 4: Update the sync spec**

In `docs/specs/2026-08-20-lyra-sync-and-protocols-spec.md`:

1. Replace the §6.1 capability bullets with:

```
- Session discovery via `/.well-known/jmap` (same-origin redirect pre-resolution; Basic or Bearer by `auth_type`).
- Mailbox sync (folders, `parentId` hierarchy).
- Email query + fetch (messages, headers, body parts); `Email/query` + `Email/get` batched per page.
- Incremental sync: `Email/queryChanges` with `removed` applied as local deletes; account-level `Email/changes` for keyword/mailbox updates and destroys (`email_state` cursor).
- Attachment download via the session `downloadUrl` into the blob store.
- Email submission (batched `Email/set` + `EmailSubmission/set`; OpenGPG MIME via `Email/import`).
- Flag changes pushed (`Email/set` keyword patches) and pulled.
- State-based change tracking (`queryState` per folder, `email_state` per account).
- EventSource push via the `jmap-client` crate stream.
```

2. Replace the §6.2 numbered flow with:

```
1. Authenticate (Bearer token or Basic auth, depending on `auth_type`).
2. `Mailbox/get` (all folders).
3. Account-level `Email/changes` since `email_state` (flag/mailbox updates, destroys).
4. For each mailbox: `Email/queryChanges` since `queryState` (added → fetch; removed → delete), or paged `Email/query` + `Email/get` when the cursor is missing/expired.
5. Update local DB and state tokens (per-folder `queryState`, account `email_state`).
```

3. In the §12-ish decisions table (the `| JMAP cursor | … |` row), replace it with:

```
| JMAP cursors | Stored `queryState` is sent as `sinceQueryState` on `Email/queryChanges`; `cannotCalculateChanges` clears the cursor and falls back to a full `Email/query`. Account-level `Email/changes` uses an `email_state` cursor anchored on the inbox folder. |
```

4. In the same table's `| Sync module | … |` row, add `jmap_client` to the module list: `(http, store, imap_loop, jmap_loop, jmap_client, send, types)`.

5. In §13.1's table, change the JMAP row's module to `` `backend/src/sync/jmap_client.rs` (seam over the `jmap-client` crate) ``.

6. In §13.2's JMAP table:
   - `Keyword / flag changes` row: status **partial** → **done**, notes: `Email/set` keyword patches from PATCH /messages; full set vs RFC 8621 audited via crate
   - `Blob download / large attachments` row: status **gap** → **done**, notes: `downloadUrl` blobs → blob store during sync; 25 MiB per-blob cap
   - Add rows:

```
| `Email/queryChanges` `removed` applied | **done** | removed ∪ destroyed minus re-fetched → local hard deletes |
| `Email/changes` keyword/move propagation | **done** | account-level `email_state` cursor anchored on inbox folder |
| Bearer token auth (Fastmail API tokens) | **done** | `auth_type = "bearer"`; token in the encrypted credential field |
| Batched send (`Email/set` + `EmailSubmission/set`) | **done** | `#` creation-id references; OpenGPG MIME via `Email/import` |
| `threadId` persisted | **done** | `message.jmap_thread_id` (server-opaque; local `thread` table untouched) |
```

- [ ] **Step 5: Final gates**

Run:
```bash
cd backend && cargo fmt --check
cd backend && cargo clippy --all-targets --all-features 2>&1 | grep "warning:" | grep -v "oauth/config.rs"
cd backend && cargo test --bin lyra_backend 2>&1 | tail -20
```
Expected: fmt clean; clippy grep empty (only the 2 pre-existing `oauth/config.rs` `result_large_err` warnings remain); suite green except the 3 pre-existing gpg-interop failures. All three gates must match — no new warnings, no new failures.

- [ ] **Step 6: Commit**

```bash
git add -A backend AGENTS.md docs/specs/2026-08-20-lyra-sync-and-protocols-spec.md
git commit -m "refactor: delete hand-rolled JMAP transport"
```

- [ ] **Step 7: Post-merge verification (no code change)**

Confirm the tree state matches the rollout:

```bash
cd backend && cargo test --bin lyra_backend 2>&1 | grep -cE "^test .* ok$"
git log --oneline -7
```

Expected: seven commits in the spec's order, each message verbatim from the rollout; the test-ok count equals the pre-work baseline plus the new tests (~45 added across Tasks 1–6).

---

## Live acceptance checklist (gated on a user-provided Fastmail API token — manual, post-merge)

Against a real Fastmail account (`auth_type = "bearer"`, `jmap_base_url = https://api.fastmail.com`):

1. Account creation with a Bearer API token → `send_protocol = "jmap"`; first sync completes; folders incl. hierarchy appear.
2. Attachment-bearing message → attachment rows exist; download endpoint serves the bytes; inline images flagged.
3. Read/star in Lyra → flag visible in Fastmail web UI (round-trip both directions).
4. Move to Trash/Archive/Spam → server-side move visible in Fastmail; local `folder_id` follows on sync.
5. Delete in Fastmail web UI → next sync removes the local row (removed-ids path).
6. Read a message in Fastmail web UI → next sync marks it read locally (Email/changes path).
7. Draft save → appears in Fastmail Drafts; edit → replaced; discard → gone server-side.
8. Send plain text + HTML + attachment → delivered; message lands in Sent; `$draft` cleared.
9. Send with OpenGPG sign/encrypt → delivered with the MIME wrapper intact (`Email/import` path).
10. Push: new mail arrives → sync job enqueued within seconds without polling.
11. Token revoked → next sync/send surfaces a typed auth error, evicts the cached session, and `jobs.last_error` says `auth error` (never the token).

## Open questions / residual risks (verified-non-blocking at plan time)

1. **Patch removal `false` vs `null`:** the crate's `keyword()`/`mailbox_id()` emit `"keywords/$seen": false`; RFC 8621 §7.5.1's example uses `null`. The send path sidesteps this with full-value replacement; `set_email_keywords` uses the crate's patch form. If checklist item 3 fails, switch `set_email_keywords` to read-modify-write (`Email/get` keywords → full `keywords(...)` replacement) — a seam-local change, no call-site impact.
2. **Two-call send requests** assume `maxCallsInRequest >= 2` (universal on known servers; the sync page batch is the only caller that splits on the capability).
3. **reqwest dual stack (0.12 Lyra + 0.13 crate)** is a compile-time-only concern; both are rustls-based. `Cargo.lock` grows accordingly — committed in Task 1.
4. **The crate builds a fresh reqwest `Client` per `send()`** (no connection reuse) — accepted per the spec's Risks; batch aggressively (done: 2-call requests, 100-id pages) and consider an upstream PR later.
5. **Fastmail's well-known redirect** (`302 /.well-known/jmap → /jmap/session`) is asserted from Lyra's own `083614a` fix comment rather than re-verified live; the preflight + conditional-allowlist design is correct for both redirecting and non-redirecting servers.
6. **No attachment backfill** for messages synced before Task 3 (`was_new`-gated downloads); a cleared cursor (full re-query) heals it if ever needed.
