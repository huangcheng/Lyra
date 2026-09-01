# DKIM Verification Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Verify DKIM signatures at view time (body-fill hook + lazy refetch on open), store verdicts on the `message` row, and show a status line + details popover in the reading pane.

**Architecture:** `mail-auth` 0.12.1 does the crypto/DNS behind a new `backend/src/dkim.rs` deep module with a pure, testable selection policy. Verdicts persist in nine new `message` columns (dual-DB migration `0017`). The IMAP body-fill path (`maybe_fill_imap_body`) verifies for free when it fetches raw bytes; a lazy path in the `get_message` handler refetches raw for rows without a verdict (both protocols). Frontend adds a status line + popover to expanded message cards.

**Tech Stack:** Rust + Axum + sea-orm 2.0 raw statements, `mail-auth` 0.12.1 (default features: aws-lc-rs + dns-hickory), React + vitest frontend.

**Spec:** `docs/superpowers/specs/2026-09-01-dkim-verification-design.md` (read first; note the spec was corrected to view-time verification — sync never holds raw bytes).

**Key facts established by codebase exploration (trust these):**

- mail-auth 0.12.1 API: `MessageAuthenticator::new_system_conf() -> Result<Self, NetError>`; `AuthenticatedMessage::parse(bytes: &[u8]) -> Option<AuthenticatedMessage>`; `authenticator.verify_dkim(&msg).await -> Vec<DkimOutput>`; `DkimOutput::result() -> &DkimResult` (variants `Pass`, `Fail(Error)`, `Neutral(Error)`, `PermError(Error)`, `TempError(Error)`, `None`); `DkimOutput::signature() -> Option<&Signature>`; `Signature` has public fields `a: Algorithm` (`RsaSha1 | RsaSha256 | Ed25519Sha256`), `d: String`, `s: String`, `i: String`, `h: Vec<String>`, `t: u64`, `x: u64` (epoch seconds, 0 = absent).
- Migrations: `backend/migrations/{sqlite,postgres}/NNNN_name.{up,down}.sql`; latest is `0016_anti_spam` → next `0017_dkim_verdict`. SQLite timestamps are `TEXT` (`"YYYY-MM-DD HH:MM:SS"`), Postgres `TIMESTAMPTZ`. Down files use `ALTER TABLE message DROP COLUMN x;`.
- Message insert: `MessageInsert` + `message_insert` in `backend/src/sync/store.rs:1247-1325` — **not touched**; verdicts are set later by UPDATE.
- Message load: `MESSAGE_LOAD_COLS` (`backend/src/sync/queries.rs:592`), `MessageRow` (`:528`), `message_response_from_row` (`:552`), `MessageResponse` (`:337`, camelCase serde, `#[serde(skip_serializing_if = "Option::is_none")]` precedent on `opengpg`). Value helpers: `opt_str_value` (store.rs:133), `ts_value(db, Option<DateTime<Utc>>)` (queries.rs:96), `now_value` (queries.rs:106), `id_value` (store.rs:114).
- IMAP fill path: `maybe_fill_imap_body` (`backend/src/sync/http.rs:1056`); raw bytes at `fetched.body: Option<Vec<u8>>` after `client.fetch_bodies(&[uid])` (`:1108`); row fields updated in memory after the UPDATE (`:1239-1256`); the UPDATE statement is built at `:1160-1236`.
- Lazy refetch factories: `connect_imap_for_account(db, user_id, account_id) -> Result<(ImapClient, String), SyncError>` (`http.rs:846`), `connect_jmap_for_account(...) -> Result<Arc<JmapSeam>, SyncError>` (`http.rs:950`). JMAP: `JmapSeam::get_emails(&[id])` re-resolves `blob_id` (jmap_client.rs:870; `JmapEmail.blob_id` at :206), `JmapSeam::download_blob(&blob_id) -> Result<Vec<u8>>` (:891). IMAP UID parse: `parse_imap_uid(row.external_id.as_deref())` (used at http.rs:1077).
- Oversize guard: `super::recovery::body_exceeds_limit(row.size_bytes)` + `MAX_MESSAGE_BODY_BYTES` (http.rs:1067).
- Test patterns: SQLite in-memory (`Storage::new("sqlite::memory:")` + `run_migrations`, e.g. `backend/src/spam.rs:548`); `postgres_live` gating pattern at `backend/src/sync/queries.rs:714-726` with harness `backend/src/pgtest.rs` (`support::rt()`, `support::setup()`, `support::seed_account/seed_inbox/message`). Run: `cargo test --bin lyra_backend`; live: `LYRA_TEST_DATABASE_URL=... cargo test --bin lyra_backend -- postgres_live --ignored`.
- Frontend: `MailMessage` type in `frontend/src/types/index.ts`; API mapping in `frontend/src/lib/mail-api.ts` (`mapApiMessage`); `MessageCard` at `frontend/src/components/mail/message-card.tsx`; `Popover` exists at `frontend/src/components/ui/popover.tsx`; i18n keys in `frontend/src/i18n/{en,zh}.json`; frontend checks: `cd frontend && npm run check && npm test`.

---

### Task 1: Dependency + migration + entity columns

**Files:**
- Modify: `backend/Cargo.toml`
- Create: `backend/migrations/sqlite/0017_dkim_verdict.up.sql`, `backend/migrations/sqlite/0017_dkim_verdict.down.sql`, `backend/migrations/postgres/0017_dkim_verdict.up.sql`, `backend/migrations/postgres/0017_dkim_verdict.down.sql`
- Modify: `backend/src/entities/message.rs`
- Test: `backend/src/storage.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Add the dependency**

In `backend/Cargo.toml` `[dependencies]`, add (alphabetical position, next to `mail-parser`):

```toml
mail-auth = "0.12.1"
```

Run `cd backend && cargo fetch` to update `Cargo.lock`.

- [ ] **Step 2: Write the failing migration test**

In `backend/src/storage.rs` `mod tests`, add:

```rust
    #[tokio::test]
    async fn migration_0017_adds_dkim_columns() {
        let storage = Storage::new("sqlite::memory:").await.unwrap();
        storage.run_migrations().await.unwrap();
        for col in [
            "dkim_status",
            "dkim_sdid",
            "dkim_auid",
            "dkim_selector",
            "dkim_algorithm",
            "dkim_signed_headers",
            "dkim_warnings",
            "dkim_signed_at",
            "dkim_expires_at",
        ] {
            let q = format!("SELECT {col} FROM message LIMIT 0");
            sqlx::query(&q)
                .execute(storage.pool().orm())
                .await
                .unwrap_or_else(|e| panic!("column {col} missing: {e}"));
        }
    }
```

Run: `cd backend && cargo test --bin lyra_backend migration_0017`
Expected: FAIL (columns don't exist).

- [ ] **Step 3: Write the migrations**

`backend/migrations/sqlite/0017_dkim_verdict.up.sql`:

```sql
ALTER TABLE message ADD COLUMN dkim_status TEXT;
ALTER TABLE message ADD COLUMN dkim_sdid TEXT;
ALTER TABLE message ADD COLUMN dkim_auid TEXT;
ALTER TABLE message ADD COLUMN dkim_selector TEXT;
ALTER TABLE message ADD COLUMN dkim_algorithm TEXT;
ALTER TABLE message ADD COLUMN dkim_signed_headers TEXT;
ALTER TABLE message ADD COLUMN dkim_warnings TEXT;
ALTER TABLE message ADD COLUMN dkim_signed_at TEXT;
ALTER TABLE message ADD COLUMN dkim_expires_at TEXT;
```

`backend/migrations/sqlite/0017_dkim_verdict.down.sql`:

```sql
ALTER TABLE message DROP COLUMN dkim_status;
ALTER TABLE message DROP COLUMN dkim_sdid;
ALTER TABLE message DROP COLUMN dkim_auid;
ALTER TABLE message DROP COLUMN dkim_selector;
ALTER TABLE message DROP COLUMN dkim_algorithm;
ALTER TABLE message DROP COLUMN dkim_signed_headers;
ALTER TABLE message DROP COLUMN dkim_warnings;
ALTER TABLE message DROP COLUMN dkim_signed_at;
ALTER TABLE message DROP COLUMN dkim_expires_at;
```

`backend/migrations/postgres/0017_dkim_verdict.up.sql`: same columns but `TIMESTAMPTZ` for the two timestamps:

```sql
ALTER TABLE message ADD COLUMN dkim_status TEXT;
ALTER TABLE message ADD COLUMN dkim_sdid TEXT;
ALTER TABLE message ADD COLUMN dkim_auid TEXT;
ALTER TABLE message ADD COLUMN dkim_selector TEXT;
ALTER TABLE message ADD COLUMN dkim_algorithm TEXT;
ALTER TABLE message ADD COLUMN dkim_signed_headers TEXT;
ALTER TABLE message ADD COLUMN dkim_warnings TEXT;
ALTER TABLE message ADD COLUMN dkim_signed_at TIMESTAMPTZ;
ALTER TABLE message ADD COLUMN dkim_expires_at TIMESTAMPTZ;
```

`backend/migrations/postgres/0017_dkim_verdict.down.sql`: same DROP COLUMN statements as SQLite.

- [ ] **Step 4: Entity columns**

In `backend/src/entities/message.rs`, add to `Model` after `spam_verdict`:

```rust
    /// DKIM verdict once verified at view time (NULL = never verified):
    /// 'pass' | 'fail' | 'none' | 'temperror'.
    pub dkim_status: Option<String>,
    pub dkim_sdid: Option<String>,
    pub dkim_auid: Option<String>,
    pub dkim_selector: Option<String>,
    pub dkim_algorithm: Option<String>,
    pub dkim_signed_headers: Option<String>,
    pub dkim_warnings: Option<String>,
    pub dkim_signed_at: Option<DateTimeUtc>,
    pub dkim_expires_at: Option<DateTimeUtc>,
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cd backend && cargo test --bin lyra_backend migration_0017`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add backend/Cargo.toml backend/Cargo.lock backend/migrations backend/src/entities/message.rs backend/src/storage.rs
git commit -m "feat(backend): mail-auth dep + dkim verdict columns (0017)"
```

---

### Task 2: `dkim.rs` — verdict model, selection policy, mail-auth seam

**Files:**
- Create: `backend/src/dkim.rs`
- Modify: `backend/src/main.rs` (register module)
- Test: colocated `#[cfg(test)] mod tests` in `dkim.rs`

- [ ] **Step 1: Write the failing test**

Create `backend/src/dkim.rs` with only the types (so the test compiles) — full code below in Step 3, but TDD order: write the test first against the intended API:

```rust
    #[test]
    fn select_best_prefers_aligned_pass() {
        let outputs = vec![
            sig_outcome(DkimStatus::Pass, "lists.example.org", "sel1"),
            sig_outcome(DkimStatus::Pass, "example.com", "sel2"),
        ];
        let v = select_best(outputs, "example.com");
        assert_eq!(v.status, DkimStatus::Pass);
        assert_eq!(v.sdid.as_deref(), Some("example.com"));
        assert_eq!(v.selector.as_deref(), Some("sel2"));
    }

    #[test]
    fn select_best_unaligned_pass_beats_fail() {
        let outputs = vec![
            sig_outcome(DkimStatus::Fail, "example.com", "sel1"),
            sig_outcome(DkimStatus::Pass, "bounces.example.org", "sel2"),
        ];
        let v = select_best(outputs, "example.com");
        assert_eq!(v.status, DkimStatus::Pass);
        assert_eq!(v.sdid.as_deref(), Some("bounces.example.org"));
    }

    #[test]
    fn select_best_no_signatures_is_none() {
        let v = select_best(Vec::new(), "example.com");
        assert_eq!(v.status, DkimStatus::None);
        assert!(v.sdid.is_none());
    }

    #[test]
    fn select_best_temperror_when_only_temperror() {
        let outputs = vec![sig_outcome(DkimStatus::TempError, "example.com", "sel1")];
        let v = select_best(outputs, "example.com");
        assert_eq!(v.status, DkimStatus::TempError);
    }

    #[test]
    fn warnings_flag_unsigned_common_headers() {
        let mut o = sig_outcome(DkimStatus::Pass, "example.com", "sel1");
        o.signed_headers = vec!["from".into(), "to".into(), "date".into()];
        let v = select_best(vec![o], "example.com");
        assert_eq!(v.warnings, vec!["Header 'Subject' is not signed".to_string()]);
    }

    #[test]
    fn subdomain_signature_counts_as_aligned() {
        let outputs = vec![sig_outcome(DkimStatus::Pass, "mail.example.com", "s")];
        let v = select_best(outputs, "example.com");
        assert_eq!(v.status, DkimStatus::Pass);
        assert_eq!(v.sdid.as_deref(), Some("mail.example.com"));
    }
```

Run: `cd backend && cargo test --bin lyra_backend dkim`
Expected: FAIL (module doesn't exist / functions undefined).

- [ ] **Step 2: Register the module**

In `backend/src/main.rs`, add `mod dkim;` next to the other module declarations.

- [ ] **Step 3: Implement `dkim.rs`**

```rust
//! DKIM verification (RFC 6376) at view time.
//!
//! Sync never holds raw RFC 822 bytes (metadata-only IMAP, parsed JMAP
//! bodies), so verification runs where bytes appear: the IMAP body-fill hook
//! and the lazy on-open refetch. The verdict model and the best-signature
//! selection policy are pure and unit-tested; mail-auth does crypto + DNS.

use mail_auth::{AuthenticatedMessage, DkimResult, MessageAuthenticator};

/// Stored verdict for one message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DkimVerdict {
    pub status: DkimStatus,
    pub sdid: Option<String>,
    pub auid: Option<String>,
    pub selector: Option<String>,
    pub algorithm: Option<String>,
    pub signed_headers: Vec<String>,
    pub warnings: Vec<String>,
    pub signed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DkimStatus {
    Pass,
    Fail,
    None,
    TempError,
}

impl DkimStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::None => "none",
            Self::TempError => "temperror",
        }
    }
}

/// One evaluated signature, flattened off mail-auth's types so the selection
/// policy stays pure and testable without DNS.
#[derive(Debug, Clone)]
pub(crate) struct SigOutcome {
    pub status: DkimStatus,
    pub sdid: Option<String>,
    pub auid: Option<String>,
    pub selector: Option<String>,
    pub algorithm: Option<String>,
    pub signed_headers: Vec<String>,
    pub signed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Headers Thunderbird-style verifiers warn about when unsigned.
const EXPECTED_SIGNED: &[&str] = &["from", "to", "subject", "date"];

/// d= aligns with the From domain when equal or a subdomain (relaxed
/// alignment, RFC 7489 §3.1.1 — the client-side analog).
fn aligns(sdid: &str, from_domain: &str) -> bool {
    sdid == from_domain || sdid.ends_with(&format!(".{from_domain}"))
}

fn warnings_for(signed_headers: &[String]) -> Vec<String> {
    EXPECTED_SIGNED
        .iter()
        .filter(|h| !signed_headers.iter().any(|s| s.eq_ignore_ascii_case(h)))
        .map(|h| format!("Header '{}' is not signed", title_case(h)))
        .collect()
}

fn title_case(h: &str) -> String {
    let mut c = h.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// Best of multiple signatures: aligned pass > any pass > first outcome.
pub(crate) fn select_best(outputs: Vec<SigOutcome>, from_domain: &str) -> DkimVerdict {
    let from_domain = from_domain.to_ascii_lowercase();
    let pick = outputs
        .iter()
        .find(|o| {
            o.status == DkimStatus::Pass
                && o.sdid
                    .as_deref()
                    .map(|d| aligns(&d.to_ascii_lowercase(), &from_domain))
                    .unwrap_or(false)
        })
        .or_else(|| outputs.iter().find(|o| o.status == DkimStatus::Pass))
        .or_else(|| outputs.first());
    match pick {
        None => DkimVerdict {
            status: DkimStatus::None,
            sdid: None,
            auid: None,
            selector: None,
            algorithm: None,
            signed_headers: Vec::new(),
            warnings: Vec::new(),
            signed_at: None,
            expires_at: None,
        },
        Some(o) => DkimVerdict {
            status: o.status,
            sdid: o.sdid.clone(),
            auid: o.auid.clone(),
            selector: o.selector.clone(),
            algorithm: o.algorithm.clone(),
            signed_headers: o.signed_headers.clone(),
            warnings: if o.status == DkimStatus::Pass {
                warnings_for(&o.signed_headers)
            } else {
                Vec::new()
            },
            signed_at: o.signed_at,
            expires_at: o.expires_at,
        },
    }
}

fn epoch(secs: u64) -> Option<chrono::DateTime<chrono::Utc>> {
    if secs == 0 {
        None
    } else {
        chrono::DateTime::from_timestamp(secs as i64, 0)
    }
}

fn map_status(r: &DkimResult) -> DkimStatus {
    match r {
        DkimResult::Pass => DkimStatus::Pass,
        DkimResult::TempError(_) => DkimStatus::TempError,
        DkimResult::None => DkimStatus::None,
        // Fail / Neutral / PermError all mean "does not validate".
        _ => DkimStatus::Fail,
    }
}

fn flatten(o: &mail_auth::DkimOutput<'_>) -> SigOutcome {
    let sig = o.signature();
    SigOutcome {
        status: map_status(o.result()),
        sdid: sig.map(|s| s.d.clone()),
        auid: sig.map(|s| s.i.clone()).filter(|i| !i.is_empty()),
        selector: sig.map(|s| s.s.clone()),
        algorithm: sig.map(|s| format!("{:?}", s.a)),
        signed_headers: sig.map(|s| s.h.clone()).unwrap_or_default(),
        signed_at: sig.and_then(|s| epoch(s.t)),
        expires_at: sig.and_then(|s| epoch(s.x)),
    }
}

fn authenticator() -> Result<&'static MessageAuthenticator, DkimStatus> {
    static AUTH: std::sync::OnceLock<MessageAuthenticator> = std::sync::OnceLock::new();
    if let Some(a) = AUTH.get() {
        return Ok(a);
    }
    match MessageAuthenticator::new_system_conf() {
        Ok(a) => Ok(AUTH.get_or_init(|| a)),
        Err(e) => {
            tracing::warn!(error = %e, "DKIM: DNS resolver init failed");
            Err(DkimStatus::TempError)
        }
    }
}

/// Verify all DKIM signatures on a raw RFC 822 message. `from_domain` is the
/// lowercased domain of the message's primary From address.
///
/// Never fails the caller: parse/resolver problems yield `temperror`.
pub(crate) async fn verify_raw(raw: &[u8], from_domain: &str) -> DkimVerdict {
    let Ok(auth) = authenticator() else {
        return select_best(
            vec![SigOutcome {
                status: DkimStatus::TempError,
                sdid: None,
                auid: None,
                selector: None,
                algorithm: None,
                signed_headers: Vec::new(),
                signed_at: None,
                expires_at: None,
            }],
            from_domain,
        );
    };
    let Some(msg) = AuthenticatedMessage::parse(raw) else {
        // Not parseable as RFC 5322 — treat as unsigned rather than broken.
        return select_best(Vec::new(), from_domain);
    };
    let outputs = auth.verify_dkim(&msg).await;
    select_best(outputs.iter().map(flatten).collect(), from_domain)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig_outcome(status: DkimStatus, sdid: &str, selector: &str) -> SigOutcome {
        SigOutcome {
            status,
            sdid: Some(sdid.to_string()),
            auid: Some(format!("ops@{sdid}")),
            selector: Some(selector.to_string()),
            algorithm: Some("RsaSha256".to_string()),
            signed_headers: vec!["from".into(), "to".into(), "subject".into(), "date".into()],
            signed_at: None,
            expires_at: None,
        }
    }

    // [Step 1 tests go here — paste the six tests verbatim]

    #[test]
    fn unsigned_raw_parses_to_none() {
        // No DNS involved: unsigned messages short-circuit before lookup.
        let raw = b"From: a@example.com\r\nTo: b@example.org\r\nSubject: hi\r\n\r\nbody\r\n";
        let v = crate::dkim::tests_rt().block_on(verify_raw(raw, "example.com"));
        assert_eq!(v.status, DkimStatus::None);
    }
}
```

Note: `tests_rt` helper — add a tiny shared runtime for the one async unit test:

```rust
#[cfg(test)]
fn tests_rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("rt")
}
```

(If `verify_dkim` on an unsigned message touches DNS in a way that fails offline, drop `unsigned_raw_parses_to_none` and keep the pure-policy tests only — mail-auth's own suite covers crypto correctness; do NOT add network-dependent tests to the default suite.)

- [ ] **Step 4: Run tests**

Run: `cd backend && cargo test --bin lyra_backend dkim`
Expected: PASS. Also `cargo clippy --all-targets --all-features -- -D warnings`.

- [ ] **Step 5: Commit**

```bash
git add backend/src/dkim.rs backend/src/main.rs
git commit -m "feat(backend): DKIM verdict model + mail-auth verification seam"
```

---

### Task 3: Persistence — `update_dkim_verdict` + row/response plumbing

**Files:**
- Modify: `backend/src/sync/store.rs`
- Modify: `backend/src/sync/queries.rs`
- Test: colocated in `store.rs` tests + `queries.rs` `postgres_live`

- [ ] **Step 1: Write the failing test**

In `backend/src/sync/store.rs` `#[cfg(test)] mod tests`, add (follow the existing test setup pattern at `backend/src/spam.rs:548-565`: in-memory SQLite, seed user/account/folder/message — look at how existing store tests seed a message and mirror it):

```rust
    #[tokio::test]
    async fn update_dkim_verdict_roundtrips_all_columns() {
        // … existing-pattern setup: storage, db, user, account, folder, one message …
        let verdict = crate::dkim::DkimVerdict {
            status: crate::dkim::DkimStatus::Pass,
            sdid: Some("example.com".into()),
            auid: Some("ops@example.com".into()),
            selector: Some("sel1".into()),
            algorithm: Some("RsaSha256".into()),
            signed_headers: vec!["from".into(), "to".into()],
            warnings: vec!["Header 'Subject' is not signed".into()],
            signed_at: chrono::DateTime::from_timestamp(1_756_700_000, 0),
            expires_at: None,
        };
        update_dkim_verdict(&db, &message_id, &verdict).await.unwrap();

        let row = crate::sync::queries::load_message_row(&db, &user_id, &message_id)
            .await
            .unwrap();
        assert_eq!(row.dkim_status.as_deref(), Some("pass"));
        assert_eq!(row.dkim_sdid.as_deref(), Some("example.com"));
        assert_eq!(row.dkim_auid.as_deref(), Some("ops@example.com"));
        assert_eq!(row.dkim_selector.as_deref(), Some("sel1"));
        assert_eq!(row.dkim_algorithm.as_deref(), Some("RsaSha256"));
        assert_eq!(row.dkim_signed_headers.as_deref(), Some(r#"["from","to"]"#));
        assert_eq!(
            row.dkim_warnings.as_deref(),
            Some(r#"["Header 'Subject' is not signed"]"#)
        );
        assert!(row.dkim_signed_at.is_some());
        assert!(row.dkim_expires_at.is_none());
    }
```

Note: `DkimVerdict` fields and `DkimStatus` variants need to be constructible from `store.rs` tests — they're `pub(crate)`, already fine. `MessageRow.dkim_*` fields come from Step 3 — the test fails to compile until then (expected for TDD here: compile error = red).

Run: `cd backend && cargo test --bin lyra_backend update_dkim_verdict`
Expected: FAIL (function/columns missing).

- [ ] **Step 2: `update_dkim_verdict` in `store.rs`**

Add (placed near `message_insert`; uses the same `Sq`/`Expr` idioms as the surrounding code):

```rust
/// Persist one message's DKIM verdict (view-time verification writes).
pub(crate) async fn update_dkim_verdict(
    db: &DbPool,
    message_id: &str,
    verdict: &crate::dkim::DkimVerdict,
) -> Result<(), SyncError> {
    let signed_headers = serde_json::to_string(&verdict.signed_headers)
        .map_err(|e| SyncError::Protocol(format!("dkim headers encode: {e}")))?;
    let warnings = serde_json::to_string(&verdict.warnings)
        .map_err(|e| SyncError::Protocol(format!("dkim warnings encode: {e}")))?;
    let mut update = Sq::update();
    update
        .table(message::Entity)
        .value(
            message::Column::DkimStatus,
            opt_str_value(Some(verdict.status.as_str())),
        )
        .value(
            message::Column::DkimSdid,
            opt_str_value(verdict.sdid.as_deref()),
        )
        .value(
            message::Column::DkimAuid,
            opt_str_value(verdict.auid.as_deref()),
        )
        .value(
            message::Column::DkimSelector,
            opt_str_value(verdict.selector.as_deref()),
        )
        .value(
            message::Column::DkimAlgorithm,
            opt_str_value(verdict.algorithm.as_deref()),
        )
        .value(message::Column::DkimSignedHeaders, Expr::val(signed_headers))
        .value(message::Column::DkimWarnings, Expr::val(warnings))
        .value(
            message::Column::DkimSignedAt,
            ts_value(db, verdict.signed_at),
        )
        .value(
            message::Column::DkimExpiresAt,
            ts_value(db, verdict.expires_at),
        )
        .value(message::Column::UpdatedAt, now_value(db))
        .and_where(Expr::col(message::Column::Id).eq(id_value(db, message_id)?));
    db.orm().execute(&update).await.map_err(orm_err)?;
    Ok(())
}
```

Check imports at the top of `store.rs`: `opt_str_value` is local; `ts_value`/`now_value` come from `super::queries` (adjust import); `SyncError::Protocol` — verify a suitable variant exists in `sync` error type, else reuse whatever `orm_err`-adjacent mapping the file already uses for encoding failures (grep for `serde_json::to_string` in `store.rs` and copy its error idiom).

- [ ] **Step 3: Load-side plumbing in `queries.rs`**

(a) `MESSAGE_LOAD_COLS` — append:

```rust
    message::Column::DkimStatus,
    message::Column::DkimSdid,
    message::Column::DkimAuid,
    message::Column::DkimSelector,
    message::Column::DkimAlgorithm,
    message::Column::DkimSignedHeaders,
    message::Column::DkimWarnings,
    message::Column::DkimSignedAt,
    message::Column::DkimExpiresAt,
```

(b) `MessageRow` — append fields:

```rust
    pub(super) dkim_status: Option<String>,
    pub(super) dkim_sdid: Option<String>,
    pub(super) dkim_auid: Option<String>,
    pub(super) dkim_selector: Option<String>,
    pub(super) dkim_algorithm: Option<String>,
    pub(super) dkim_signed_headers: Option<String>,
    pub(super) dkim_warnings: Option<String>,
    pub(super) dkim_signed_at: Option<String>,
    pub(super) dkim_expires_at: Option<String>,
```

(c) `load_message_row`'s `Ok(MessageRow { … })` — append reads (timestamps come back as strings through the same `row.try_get` idiom other TEXT columns use; check how `date: String` is read — `row.try_get("", "date")` at queries.rs:665 area — and mirror it):

```rust
        dkim_status: row.try_get("", "dkim_status").map_err(orm_err)?,
        dkim_sdid: row.try_get("", "dkim_sdid").map_err(orm_err)?,
        dkim_auid: row.try_get("", "dkim_auid").map_err(orm_err)?,
        dkim_selector: row.try_get("", "dkim_selector").map_err(orm_err)?,
        dkim_algorithm: row.try_get("", "dkim_algorithm").map_err(orm_err)?,
        dkim_signed_headers: row.try_get("", "dkim_signed_headers").map_err(orm_err)?,
        dkim_warnings: row.try_get("", "dkim_warnings").map_err(orm_err)?,
        dkim_signed_at: row.try_get("", "dkim_signed_at").map_err(orm_err)?,
        dkim_expires_at: row.try_get("", "dkim_expires_at").map_err(orm_err)?,
```

(d) Response DTO — add to `queries.rs` near `MessageResponse`:

```rust
/// DKIM verdict in the detail payload (`null` = never verified).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DkimResponse {
    pub status: String,
    pub sdid: Option<String>,
    pub auid: Option<String>,
    pub selector: Option<String>,
    pub algorithm: Option<String>,
    pub signed_headers: Vec<String>,
    pub warnings: Vec<String>,
    pub signed_at: Option<String>,
    pub expires_at: Option<String>,
}
```

Add to `MessageResponse`:

```rust
    /// DKIM verdict; detail endpoint only, absent when never verified.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dkim: Option<DkimResponse>,
```

and a builder used by `message_response_from_row`:

```rust
pub(super) fn dkim_response_from_row(row: &MessageRow) -> Option<DkimResponse> {
    let status = row.dkim_status.clone()?;
    Some(DkimResponse {
        status,
        sdid: row.dkim_sdid.clone(),
        auid: row.dkim_auid.clone(),
        selector: row.dkim_selector.clone(),
        algorithm: row.dkim_algorithm.clone(),
        signed_headers: serde_json::from_str(row.dkim_signed_headers.as_deref().unwrap_or("[]"))
            .unwrap_or_default(),
        warnings: serde_json::from_str(row.dkim_warnings.as_deref().unwrap_or("[]"))
            .unwrap_or_default(),
        signed_at: row.dkim_signed_at.clone(),
        expires_at: row.dkim_expires_at.clone(),
    })
}
```

In `message_response_from_row`, add `dkim: dkim_response_from_row(row),` to the struct literal.

- [ ] **Step 4: Run the roundtrip test + a postgres_live variant**

Add the same roundtrip as a `postgres_live` test in `store.rs`'s `mod postgres_live` (pattern: `queries.rs:714-726`; seed via `support::seed_account`/`seed_inbox`/`support::message`, then call `update_dkim_verdict` + `load_message_row`).

Run: `cd backend && cargo test --bin lyra_backend dkim`
Expected: PASS (SQLite). Then, with the local compose Postgres running (`docker compose up -d postgres`; the dev DB already exists — point at a scratch DB or accept it runs against the dev one):

Run: `cd backend && LYRA_TEST_DATABASE_URL=postgres://lyra:$POSTGRES_PASSWORD@127.0.0.1:5432/lyra cargo test --bin lyra_backend -- postgres_live --ignored` — use the password from `.env` without printing it, e.g. `set -a; source ../.env; set +a` first. NOTE: pgtest may create/drop scratch state; check `backend/src/pgtest.rs` `support::setup()` before pointing at the dev database — if it truncates anything, create a scratch DB `lyra_test` in the compose Postgres instead.
Expected: PASS.

Also: `cargo clippy --all-targets --all-features -- -D warnings`.

- [ ] **Step 5: Commit**

```bash
git add backend/src/sync/store.rs backend/src/sync/queries.rs
git commit -m "feat(backend): persist + load DKIM verdicts on message rows"
```

---

### Task 4: Body-fill hook (IMAP) + lazy verify in `get_message`

**Files:**
- Modify: `backend/src/sync/http.rs`
- Test: colocated in `http.rs` tests (handler-level, SQLite)

- [ ] **Step 1: Write the failing tests**

In `backend/src/sync/http.rs` `#[cfg(test)] mod tests` (mirror an existing `get_message` test setup — grep the file for an existing message-detail test to copy its fixture style):

```rust
    #[tokio::test]
    async fn get_message_without_verdict_serves_dkim_none() {
        // Seed a message whose body is already filled (no refetch possible in
        // tests) and dkim_status NULL… but the lazy path must attempt verify.
        // Because tests cannot reach IMAP/JMAP, assert the failure-safe
        // contract: response serves normally and carries no dkim object.
        // … existing-pattern setup …
        let resp = get_message(/* … */).await.unwrap();
        assert!(resp.dkim.is_none());
    }
```

The selection-policy and fill-hook logic is already unit-tested in Task 2; this test pins the failure-safe handler contract (refetch impossible in unit tests → no verdict, no error).

Run: `cd backend && cargo test --bin lyra_backend get_message`
Expected: FAIL to compile (`resp.dkim` unknown) → red.

- [ ] **Step 2: Extract the From-domain helper**

In `http.rs` (or `dkim.rs` if it fits better — it operates on a stored address JSON, so `http.rs` near `sender_email_from_json` in `privacy.rs:112` is the prior art; reuse that extraction):

```rust
/// Lowercased domain of the message's primary From address ("" when absent).
fn from_domain_of(row: &MessageRow) -> String {
    // from_address is stored as JSON: {"raw": "Name <a@b.c>"} or {"email": …}
    crate::privacy::extract_email_from_json(row.from_address.as_deref())
        .and_then(|addr| addr.rsplit('@').next().map(str::to_ascii_lowercase))
        .unwrap_or_default()
}
```

`privacy.rs:112-126` has `sender_email_from_json`/`extract_email` — check their visibility and either make them `pub(crate)` and reuse, or duplicate the 10 lines locally. Prefer making them `pub(crate)`.

- [ ] **Step 3: Verify in the body-fill hook**

In `maybe_fill_imap_body` (`http.rs:1056`), after the oversize check passes and `fetched` is bound (`:1109`), before building the UPDATE:

```rust
    // DKIM: the raw RFC 822 bytes are in hand exactly once — verify now.
    let dkim_verdict = if let Some(raw) = &fetched.body {
        Some(crate::dkim::verify_raw(raw, &from_domain_of(row)).await)
    } else {
        None
    };
```

Apply it inside the existing UPDATE (after `.value(message::Column::BodyHtml, …)`):

```rust
    if let Some(v) = &dkim_verdict {
        let headers_json = serde_json::to_string(&v.signed_headers).unwrap_or_default();
        let warnings_json = serde_json::to_string(&v.warnings).unwrap_or_default();
        update
            .value(message::Column::DkimStatus, opt_str_value(Some(v.status.as_str())))
            .value(message::Column::DkimSdid, opt_str_value(v.sdid.as_deref()))
            .value(message::Column::DkimAuid, opt_str_value(v.auid.as_deref()))
            .value(message::Column::DkimSelector, opt_str_value(v.selector.as_deref()))
            .value(message::Column::DkimAlgorithm, opt_str_value(v.algorithm.as_deref()))
            .value(message::Column::DkimSignedHeaders, Expr::val(headers_json))
            .value(message::Column::DkimWarnings, Expr::val(warnings_json))
            .value(message::Column::DkimSignedAt, ts_value(db, v.signed_at))
            .value(message::Column::DkimExpiresAt, ts_value(db, v.expires_at));
    }
```

and mirror into the in-memory row (next to `row.body_text = fetched_body_text;`):

```rust
    if let Some(v) = &dkim_verdict {
        row.dkim_status = Some(v.status.as_str().to_string());
        row.dkim_sdid = v.sdid.clone();
        row.dkim_auid = v.auid.clone();
        row.dkim_selector = v.selector.clone();
        row.dkim_algorithm = v.algorithm.clone();
        row.dkim_signed_headers =
            Some(serde_json::to_string(&v.signed_headers).unwrap_or_default());
        row.dkim_warnings = Some(serde_json::to_string(&v.warnings).unwrap_or_default());
        row.dkim_signed_at = v.signed_at.map(|d| d.to_rfc3339());
        row.dkim_expires_at = v.expires_at.map(|d| d.to_rfc3339());
    }
```

(`opt_str_value`, `ts_value`, `Expr`, `Sq` are already in scope in `http.rs` — verify against the imports at the top of the file; `opt_str_value` lives in `store.rs` so import it: `use super::store::opt_str_value;` — check its visibility, make `pub(crate)` if needed.)

- [ ] **Step 4: Lazy verify in `get_message`**

In `get_message` (`http.rs:1006`), after `maybe_fill_imap_body(...)`:

```rust
    maybe_verify_dkim(db, &state, &session.user_id, &mut row).await;
```

New function in `http.rs`:

```rust
/// Lazy DKIM for rows without a verdict: refetch raw bytes once, verify,
/// store, and reflect into the row. Failure-safe: on any error the message
/// serves without a verdict and the row stays NULL for a later retry.
async fn maybe_verify_dkim(
    db: &DbPool,
    state: &AuthState,
    user_id: &str,
    row: &mut MessageRow,
) {
    let needs = row.dkim_status.is_none() || row.dkim_status.as_deref() == Some("temperror");
    // The fill hook already verified when it fetched the body this request.
    if !needs || super::recovery::body_exceeds_limit(row.size_bytes) {
        return;
    }
    let raw: Option<Vec<u8>> = match row.protocol.as_str() {
        "imap" => {
            let uid = match parse_imap_uid(row.external_id.as_deref()) {
                Ok(uid) => uid,
                Err(_) => return,
            };
            let Ok((mut client, _)) = connect_imap_for_account(db, user_id, &row.account_id).await
            else {
                return;
            };
            if client.select(&row.folder_name).await.is_err() {
                return;
            }
            client
                .fetch_bodies(&[uid])
                .await
                .ok()
                .and_then(|b| b.into_iter().next())
                .and_then(|m| m.body)
        }
        "jmap" => {
            let Some(email_id) = row.external_id.clone() else {
                return;
            };
            let Ok(seam) = connect_jmap_for_account(db, user_id, &row.account_id).await else {
                return;
            };
            let blob_id = seam
                .get_emails(&[email_id])
                .await
                .ok()
                .and_then(|(emails, _)| emails.into_iter().next())
                .and_then(|e| e.blob_id);
            match blob_id {
                Some(id) => seam.download_blob(&id).await.ok(),
                None => None,
            }
        }
        _ => None,
    };
    let Some(raw) = raw else { return };
    let verdict = crate::dkim::verify_raw(&raw, &from_domain_of(row)).await;
    if crate::sync::store::update_dkim_verdict(db, &row.id, &verdict)
        .await
        .is_ok()
    {
        row.dkim_status = Some(verdict.status.as_str().to_string());
        row.dkim_sdid = verdict.sdid.clone();
        row.dkim_auid = verdict.auid.clone();
        row.dkim_selector = verdict.selector.clone();
        row.dkim_algorithm = verdict.algorithm.clone();
        row.dkim_signed_headers =
            Some(serde_json::to_string(&verdict.signed_headers).unwrap_or_default());
        row.dkim_warnings = Some(serde_json::to_string(&verdict.warnings).unwrap_or_default());
        row.dkim_signed_at = verdict.signed_at.map(|d| d.to_rfc3339());
        row.dkim_expires_at = verdict.expires_at.map(|d| d.to_rfc3339());
    }
}
```

Note: `maybe_verify_dkim` takes `state` only if needed for the factories — the factories take `(db, user_id, account_id)`, so drop the `state` param if unused (keep the signature minimal; adjust the call site).

One subtlety the implementer must check: `maybe_fill_imap_body` runs first and already sets a verdict when it fetches — but it also *sets body fields*, so `needs` stays true if the fill hook found `fetched.body == None` (rare). That double-verify is harmless (idempotent) — do not add coordination logic.

- [ ] **Step 5: Run tests + clippy**

Run: `cd backend && cargo test --bin lyra_backend && cargo clippy --all-targets --all-features -- -D warnings`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add backend/src/sync/http.rs backend/src/privacy.rs backend/src/sync/store.rs
git commit -m "feat(backend): DKIM verify on body fill + lazy on message open"
```

---

### Task 5: Frontend — status line + details popover

**Files:**
- Create: `frontend/src/lib/dkim.ts`
- Test: `frontend/src/lib/dkim.test.ts`
- Create: `frontend/src/components/mail/dkim-status.tsx`
- Modify: `frontend/src/types/index.ts`, `frontend/src/lib/mail-api.ts`, `frontend/src/components/mail/message-card.tsx`, `frontend/src/i18n/en.json`, `frontend/src/i18n/zh.json`

- [ ] **Step 1: Write the failing test**

Create `frontend/src/lib/dkim.test.ts`:

```ts
import { describe, expect, it } from 'vitest';

import { dkimSummary } from '@/lib/dkim';
import type { DkimInfo } from '@/types';

const base: DkimInfo = {
  status: 'pass',
  sdid: 'duck.com',
  auid: '@duck.com',
  selector: 'dkim',
  algorithm: 'RsaSha256',
  signedHeaders: ['date', 'from', 'to'],
  warnings: [],
  signedAt: null,
  expiresAt: null,
};

describe('dkimSummary', () => {
  it('pass names the signing domain', () => {
    expect(dkimSummary('en', base)).toBe('DKIM Valid (Signed by duck.com)');
    expect(dkimSummary('zh', base)).toBe('DKIM 验证通过（签名方 duck.com）');
  });

  it('fail reports modification', () => {
    const v = { ...base, status: 'fail' as const };
    expect(dkimSummary('en', v)).toBe('DKIM Invalid (E-Mail was modified)');
    expect(dkimSummary('zh', v)).toBe('DKIM 无效（邮件已被修改）');
  });

  it('none and temperror are neutral', () => {
    expect(dkimSummary('en', { ...base, status: 'none' })).toBe('Not signed');
    expect(dkimSummary('en', { ...base, status: 'temperror' })).toBe('Not signed');
    expect(dkimSummary('zh', { ...base, status: 'none' })).toBe('未签名');
  });

  it('pass without sdid falls back gracefully', () => {
    expect(dkimSummary('en', { ...base, sdid: null })).toBe('DKIM Valid');
    expect(dkimSummary('zh', { ...base, sdid: null })).toBe('DKIM 验证通过');
  });
});
```

Run: `cd frontend && npx vitest run src/lib/dkim.test.ts`
Expected: FAIL (module missing).

- [ ] **Step 2: Types + mapper**

In `frontend/src/types/index.ts`:

```ts
export interface DkimInfo {
  status: 'pass' | 'fail' | 'none' | 'temperror';
  sdid: string | null;
  auid: string | null;
  selector: string | null;
  algorithm: string | null;
  signedHeaders: string[];
  warnings: string[];
  signedAt: string | null;
  expiresAt: string | null;
}
```

Add to `MailMessage`: `dkim?: DkimInfo | null;`

In `frontend/src/lib/mail-api.ts`, check the API message type (snake_case wire format? No — `MessageResponse` is camelCase) and `mapApiMessage`: add `dkim: raw.dkim ?? null` (find the exact field-mapping block and mirror an adjacent optional field).

- [ ] **Step 3: `lib/dkim.ts`**

```ts
/**
 * DKIM verdict display strings. The verdict comes from the detail payload;
 * `temperror` is shown as unsigned (it means "we couldn't check", not
 * "broken signature").
 */

import { t, type Locale } from '@/i18n';
import type { DkimInfo } from '@/types';

export function dkimSummary(locale: Locale, dkim: DkimInfo): string {
  switch (dkim.status) {
    case 'pass':
      return dkim.sdid
        ? t(locale, 'mail.dkimValidSignedBy', { domain: dkim.sdid })
        : t(locale, 'mail.dkimValid');
    case 'fail':
      return t(locale, 'mail.dkimInvalid');
    default:
      return t(locale, 'mail.dkimNone');
  }
}
```

Check `t`'s locale param type in `frontend/src/i18n/index.ts` — use whatever it exports (`SupportedLocale` from `@/types` is the existing convention; match it, and if `t` takes `SupportedLocale`, import that instead of inventing `Locale`).

- [ ] **Step 4: i18n keys**

Add to `frontend/src/i18n/en.json` under `mail` (alphabetical neighbors):

```json
    "dkimValid": "DKIM Valid",
    "dkimValidSignedBy": "DKIM Valid (Signed by {{domain}})",
    "dkimInvalid": "DKIM Invalid (E-Mail was modified)",
    "dkimNone": "Not signed",
    "dkimDetails": "DKIM details",
    "dkimSdid": "Signing domain (SDID)",
    "dkimAuid": "Identity (AUID)",
    "dkimSelector": "Selector",
    "dkimAlgorithm": "Algorithm",
    "dkimSignedHeaders": "Signed headers",
    "dkimWarnings": "Warnings",
    "dkimSignedAt": "Sign date",
    "dkimExpiresAt": "Expiration date",
```

and `zh.json`:

```json
    "dkimValid": "DKIM 验证通过",
    "dkimValidSignedBy": "DKIM 验证通过（签名方 {{domain}}）",
    "dkimInvalid": "DKIM 无效（邮件已被修改）",
    "dkimNone": "未签名",
    "dkimDetails": "DKIM 详情",
    "dkimSdid": "签名域（SDID）",
    "dkimAuid": "身份（AUID）",
    "dkimSelector": "选择器",
    "dkimAlgorithm": "算法",
    "dkimSignedHeaders": "已签名头部",
    "dkimWarnings": "警告",
    "dkimSignedAt": "签名时间",
    "dkimExpiresAt": "过期时间",
```

- [ ] **Step 5: `dkim-status.tsx`**

```tsx
/**
 * DKIM status line + details popover for an expanded message card.
 * Renders nothing when the message was never verified (`dkim` null).
 */

import { ShieldCheck, ShieldAlert, ShieldMinus } from 'lucide-react';

import { Popover, PopoverContent, PopoverTrigger } from '@/components/ui/popover';
import { t } from '@/i18n';
import { dkimSummary } from '@/lib/dkim';
import { cn } from '@/lib/utils';
import type { DkimInfo, SupportedLocale } from '@/types';

function DetailRow({ label, value }: { label: string; value: string | null | undefined }) {
  if (!value) return null;
  return (
    <div className="flex gap-2 text-xs">
      <span className="w-36 shrink-0 text-ter-foreground">{label}</span>
      <span className="min-w-0 break-words">{value}</span>
    </div>
  );
}

export function DkimStatus({ dkim, locale }: { dkim: DkimInfo; locale: SupportedLocale }) {
  const summary = dkimSummary(locale, dkim);
  const Icon =
    dkim.status === 'pass' ? ShieldCheck : dkim.status === 'fail' ? ShieldAlert : ShieldMinus;
  return (
    <Popover>
      <PopoverTrigger asChild>
        <button
          type="button"
          className={cn(
            'flex items-center gap-1.5 rounded-md px-1.5 py-1 text-xs transition-colors hover:bg-accent/60',
            dkim.status === 'pass' && 'text-green-700 dark:text-green-400',
            dkim.status === 'fail' && 'text-destructive',
            (dkim.status === 'none' || dkim.status === 'temperror') && 'text-ter-foreground',
          )}
        >
          <Icon className="size-3.5 shrink-0" aria-hidden />
          {summary}
        </button>
      </PopoverTrigger>
      <PopoverContent className="w-96">
        <div className="mb-2 text-sm font-medium">{t(locale, 'mail.dkimDetails')}</div>
        <div className="grid gap-1.5">
          <DetailRow label={t(locale, 'mail.dkimSdid')} value={dkim.sdid} />
          <DetailRow label={t(locale, 'mail.dkimAuid')} value={dkim.auid} />
          <DetailRow label={t(locale, 'mail.dkimSelector')} value={dkim.selector} />
          <DetailRow label={t(locale, 'mail.dkimAlgorithm')} value={dkim.algorithm} />
          <DetailRow
            label={t(locale, 'mail.dkimSignedHeaders')}
            value={dkim.signedHeaders.length ? dkim.signedHeaders.join(', ') : null}
          />
          <DetailRow
            label={t(locale, 'mail.dkimWarnings')}
            value={dkim.warnings.length ? dkim.warnings.join('; ') : null}
          />
          <DetailRow label={t(locale, 'mail.dkimSignedAt')} value={dkim.signedAt} />
          <DetailRow label={t(locale, 'mail.dkimExpiresAt')} value={dkim.expiresAt} />
        </div>
      </PopoverContent>
    </Popover>
  );
}
```

- [ ] **Step 6: Mount in `MessageCard`**

In `frontend/src/components/mail/message-card.tsx`, in the expanded branch, directly above the body container `<div className="px-4 pt-1 pb-4 text-sm">`:

```tsx
      {mail.dkim ? (
        <div className="px-4 pb-1">
          <DkimStatus dkim={mail.dkim} locale={locale} />
        </div>
      ) : null}
```

Add the import:

```ts
import { DkimStatus } from '@/components/mail/dkim-status';
```

- [ ] **Step 7: Verify**

Run: `cd frontend && npm run check && npm test`
Expected: PASS (63 + new dkim tests).

- [ ] **Step 8: Commit**

```bash
git add frontend/src/lib/dkim.ts frontend/src/lib/dkim.test.ts frontend/src/components/mail/dkim-status.tsx frontend/src/types/index.ts frontend/src/lib/mail-api.ts frontend/src/components/mail/message-card.tsx frontend/src/i18n/en.json frontend/src/i18n/zh.json
git commit -m "feat(frontend): DKIM status line + details popover in reading pane"
```

---

### Task 6: Full verification

- [ ] **Step 1: `make fmt && make check`**

Run from repo root. Expected: format check, oxlint, tsc, clippy `-D warnings`, vitest, cargo test, gitleaks — all PASS. Commit formatting deltas if any.

- [ ] **Step 2: Live smoke test against real mail**

Rebuild the dev stack (`docker compose up -d --build lyra`), sign in, and open:

1. A DuckDuckGo/GitHub notification (DKIM-signed) → green "DKIM Valid (Signed by …)", popover shows selector/algorithm/headers.
2. A mailing-list or modified message if available → red invalid state.
3. An unsigned message → neutral "Not signed".
4. Reopen the same message → verdict now comes from the DB (no second refetch; check logs for a single verify).

Note: the local stack's DNS goes through the host resolver; DKIM key lookups need working DNS from the container. If lookups fail in this sandboxed network, the row stores `temperror` and the UI shows "Not signed" — check `docker compose logs lyra` for `DKIM` warnings before assuming a code bug.

- [ ] **Step 3: Final commit (only if Step 1 produced changes)**

```bash
git add -A
git commit -m "chore: format after DKIM feature"
```

---

## Self-review notes

- Spec coverage: verdict model + selection policy → Task 2; columns + migration → Task 1; persistence + load + API shape → Task 3; body-fill hook → Task 4 Step 3; lazy on open (both protocols) → Task 4 Step 4; frontend status line + popover + i18n → Task 5; oversize guard → Tasks 4 (both paths reuse `body_exceeds_limit`); failure-safe contracts → tested in Task 4 Step 1.
- Out-of-scope items (SPF, DMARC enforcement, ARC, signing, scoring, async verify) have no tasks — intentionally.
- Type consistency: `DkimVerdict`/`DkimStatus`/`SigOutcome`/`select_best`/`verify_raw` (dkim.rs) → used identically in Tasks 2/3/4; `DkimResponse`/`dkim_response_from_row` (queries.rs) → Task 3; `DkimInfo`/`dkimSummary`/`DkimStatus` component → Task 5. Column names `dkim_*` identical across migration, entity, store, queries.
- Known soft spot by design: `algorithm` stores `format!("{:?}", sig.a)` (`RsaSha256`); the spec's display string "RSA 2048 / SHA-256" needs key parsing mail-auth doesn't expose — the Debug tag is the honest value; UI shows it raw. Flagged here so nobody "fixes" it into a lie.
