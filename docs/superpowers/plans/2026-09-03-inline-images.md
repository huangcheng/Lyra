# Inline Images in Compose Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Users can insert images into compose (toolbar/paste/drop); images are embedded as inline MIME parts (multipart/related + Content-ID, RFC 2387/2045/2392/2183), persist in drafts, and file attachments keep working alongside.

**Architecture:** Backend: `OutboundAttachment.content_id` + `smtp.rs build_message` emits inline parts in `multipart/related`; `/messages/send` multipart gains an `inlineMeta` field; `/api/v1/drafts` JSON gains `inlineAttachments`; JMAP routes inline-bearing messages through raw-MIME `Email/import`. Frontend: pure helpers in `src/lib/inline-images.ts`; Plate `ImagePlugin` in the rich editor; compose dialog tracks `objectURL → {file, contentId}` and rewrites to `cid:` on send/save.

**Tech Stack:** Rust + Axum + lettre + jmap-client 0.4.2 (backend); React + Plate.js v53 + `@platejs/media` (frontend); vitest + cargo test.

Spec: `docs/superpowers/specs/2026-09-03-inline-images-design.md`

**Repo conventions the engineer must follow:**
- Commit directly on `main`; a gitleaks pre-commit hook runs automatically.
- The working tree contains the user's unrelated uncommitted work. NEVER `git add` broad paths; stage exactly the files each task lists. Before committing a modified file, check `git status --porcelain -- <file>`; if it was dirty BEFORE your edit, STOP and report.
- Backend tests: `cargo test --bin lyra_backend` (binary crate, not `--lib`). Backend lint: `cd backend && cargo clippy --all-targets --all-features -- -D warnings`. Format: `cargo fmt`.
- Frontend gate: `cd frontend && npm run check` (oxlint + tsc + prettier; 4 pre-existing oxlint warnings in `ui/*`, `router.tsx`, `avatar.ts` are known).
- Frontend logic lives in `frontend/src/lib/` with colocated vitest tests; `@/` import alias works in tests.
- i18n: every new user-facing string goes in BOTH `frontend/src/i18n/en.json` and `zh.json` (a key-parity test enforces this).

---

## Task 1: Backend — `OutboundAttachment.content_id` + multipart/related assembly

**Files:**
- Modify: `backend/src/smtp.rs` (struct ~line 121, `attachment_part` ~line 379, `build_message` ~line 388, tests from ~line 448)

- [ ] **Step 1: Add the field + validation, write failing tests**

Add to `OutboundAttachment` (backend/src/smtp.rs:121):

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboundAttachment {
    pub filename: String,
    pub content_type: String,
    pub data_base64: String,
    /// RFC 2045 Content-ID (without angle brackets); `Some` = inline part
    /// referenced as `cid:` from the HTML body (RFC 2392).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_id: Option<String>,
}
```

Add to `impl OutboundAttachment`:

```rust
    /// Inline part constructor: bytes + the Content-ID the HTML references.
    #[must_use]
    pub fn from_bytes_inline(filename: &str, content_type: &str, data: &[u8], content_id: &str) -> Self {
        let mut att = Self::from_bytes(filename, content_type, data);
        att.content_id = Some(content_id.to_owned());
        att
    }
```

Add a free function (used by build_message and the HTTP layer):

```rust
/// Content-IDs become a header value: printable ASCII, no whitespace, no
/// angle brackets (build_message adds them). Anything else is rejected.
pub(crate) fn validate_content_id(cid: &str) -> Result<(), SmtpError> {
    let ok = !cid.is_empty()
        && cid
            .chars()
            .all(|c| c.is_ascii_graphic() && c != '<' && c != '>');
    if ok {
        Ok(())
    } else {
        Err(SmtpError::Permanent(format!("invalid content-id: {cid:?}")))
    }
}
```

Add failing tests in the `tests` module:

```rust
    fn inline_att(name: &str, cid: &str) -> OutboundAttachment {
        OutboundAttachment::from_bytes_inline(name, "image/png", b"\x89PNG", cid)
    }

    fn base_msg() -> OutboundMessage {
        OutboundMessage {
            from_email: "a@example.com".into(),
            from_name: None,
            to: vec![(None, "b@example.com".into())],
            cc: vec![],
            bcc: vec![],
            subject: "s".into(),
            body_text: Some("hi".into()),
            body_html: Some("<p>hi</p><img src=\"cid:img1@lyra\">".into()),
            in_reply_to: None,
            references: None,
            mime_content_type: None,
            mime_body: None,
            attachments: vec![],
            message_id: None,
        }
    }

    fn formatted(msg: &OutboundMessage) -> String {
        String::from_utf8(build_message(msg).unwrap().formatted()).unwrap()
    }

    #[test]
    fn inline_attachment_produces_multipart_related() {
        let mut msg = base_msg();
        msg.attachments.push(inline_att("a.png", "img1@lyra"));
        let raw = formatted(&msg);
        assert!(raw.contains("multipart/related"), "{raw}");
        assert!(raw.contains("Content-ID: <img1@lyra>"), "{raw}");
        assert!(raw.contains("Content-Disposition: inline"), "{raw}");
        assert!(!raw.contains("multipart/mixed"), "{raw}");
        // related wraps the alternative body: html appears before the image part
        let html_pos = raw.find("text/html").unwrap();
        let img_pos = raw.find("Content-ID: <img1@lyra>").unwrap();
        assert!(html_pos < img_pos, "{raw}");
    }

    #[test]
    fn inline_plus_file_produces_mixed_wrapping_related() {
        let mut msg = base_msg();
        msg.attachments.push(inline_att("a.png", "img1@lyra"));
        msg.attachments
            .push(OutboundAttachment::from_bytes("doc.pdf", "application/pdf", b"PDF"));
        let raw = formatted(&msg);
        assert!(raw.contains("multipart/mixed"), "{raw}");
        assert!(raw.contains("multipart/related"), "{raw}");
        assert!(raw.contains("Content-Disposition: inline"), "{raw}");
        assert!(raw.contains("filename=\"doc.pdf\""), "{raw}");
        let mixed_pos = raw.find("multipart/mixed").unwrap();
        let related_pos = raw.find("multipart/related").unwrap();
        assert!(mixed_pos < related_pos, "{raw}");
    }

    #[test]
    fn regular_attachments_only_keep_todays_structure() {
        let mut msg = base_msg();
        msg.attachments
            .push(OutboundAttachment::from_bytes("doc.pdf", "application/pdf", b"PDF"));
        let raw = formatted(&msg);
        assert!(raw.contains("multipart/mixed"), "{raw}");
        assert!(!raw.contains("multipart/related"), "{raw}");
        assert!(!raw.contains("Content-Disposition: inline"), "{raw}");
    }

    #[test]
    fn no_attachments_no_inline_markers() {
        let raw = formatted(&base_msg());
        assert!(!raw.contains("multipart/related"), "{raw}");
        assert!(!raw.contains("multipart/mixed"), "{raw}");
        assert!(raw.contains("multipart/alternative"), "{raw}");
    }

    #[test]
    fn invalid_content_id_is_rejected() {
        let mut msg = base_msg();
        msg.attachments.push(inline_att("a.png", "bad>\nbcc:evil@example.com"));
        assert!(build_message(&msg).is_err());
    }

    #[test]
    fn opengpg_wrapper_downgrades_inline_to_regular_attachment() {
        let mut msg = base_msg();
        msg.mime_content_type = Some("multipart/encrypted; protocol=\"application/pgp-encrypted\"".into());
        msg.mime_body = Some("wrapped".into());
        msg.body_text = None;
        msg.body_html = None;
        msg.attachments.push(inline_att("a.png", "img1@lyra"));
        let raw = formatted(&msg);
        assert!(!raw.contains("multipart/related"), "{raw}");
        assert!(!raw.contains("Content-Disposition: inline"), "{raw}");
        assert!(raw.contains("multipart/encrypted"), "{raw}");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd backend && cargo test --bin lyra_backend smtp::tests`
Expected: FAIL (compile error — no `content_id` field yet, or assertion failures).

- [ ] **Step 3: Implement**

In `backend/src/smtp.rs`:

1. Add a generic part enum near `BodyPart`:

```rust
/// Any MIME level: a single leaf part or a multipart container.
enum Part {
    Single(SinglePart),
    Multi(MultiPart),
}
```

2. Replace `attachment_part` and add `inline_part`:

```rust
/// Attachment → lettre part (base64 body, `attachment` disposition).
fn attachment_part(att: &OutboundAttachment) -> Result<SinglePart, SmtpError> {
    let bytes = att.decode()?;
    let content_type = ContentType::parse(&att.content_type).unwrap_or_else(|_| {
        ContentType::parse("application/octet-stream").expect("static MIME type parses")
    });
    Ok(Attachment::new(att.filename.clone()).body(bytes, content_type))
}

/// Inline attachment → lettre part: `inline` disposition (RFC 2183) +
/// `Content-ID` (RFC 2045) so the HTML body's `cid:` refs (RFC 2392) resolve.
fn inline_part(att: &OutboundAttachment) -> Result<SinglePart, SmtpError> {
    let cid = att.content_id.as_deref().ok_or_else(|| {
        SmtpError::Permanent(format!("inline attachment {}: missing content_id", att.filename))
    })?;
    validate_content_id(cid)?;
    let bytes = att.decode()?;
    let content_type = ContentType::parse(&att.content_type).unwrap_or_else(|_| {
        ContentType::parse("application/octet-stream").expect("static MIME type parses")
    });
    Ok(Attachment::new_inline(att.filename.clone())
        .body(bytes, content_type)
        .header(lettre::message::header::ContentId::from(format!("<{cid}>"))))
}
```

NOTE: verify against the lettre version in `backend/Cargo.lock` that `Attachment::new_inline` and `header::ContentId::from(String)` exist (lettre 0.11 has both). If `ContentId::from` isn't available, use `ContentId::from(format!("<{cid}>"))` alternatives per the installed crate's API — the test in Step 2 is the contract.

3. Rewrite the assembly tail of `build_message` (currently lines 425-443):

```rust
    let body_part = build_body_part(msg)?;
    // OpenGPG replaces the body with a crypto wrapper (mime_body); inline
    // parts would leak outside the envelope, so they degrade to attachments.
    let inline_allowed = msg.mime_body.is_none();
    let (inline, regular): (Vec<&OutboundAttachment>, Vec<&OutboundAttachment>) = msg
        .attachments
        .iter()
        .partition(|a| inline_allowed && a.content_id.is_some());

    // RFC 2387 multipart/related: body + inline parts, only when present.
    let body_level: Part = if inline.is_empty() {
        match body_part {
            BodyPart::Single(sp) => Part::Single(sp),
            BodyPart::Alternative(mp) => Part::Multi(mp),
        }
    } else {
        let mut related = match body_part {
            BodyPart::Single(sp) => MultiPart::related().singlepart(sp),
            BodyPart::Alternative(mp) => MultiPart::related().multipart(mp),
        };
        for att in inline {
            related = related.singlepart(inline_part(att)?);
        }
        Part::Multi(related)
    };

    // RFC 2046 multipart/mixed: body level first, then regular attachments.
    let message = match (body_level, regular.is_empty()) {
        (Part::Single(sp), true) => builder.singlepart(sp)?,
        (Part::Multi(mp), true) => builder.multipart(mp)?,
        (level, false) => {
            let mut mixed = match level {
                Part::Single(sp) => MultiPart::mixed().singlepart(sp),
                Part::Multi(mp) => MultiPart::mixed().multipart(mp),
            };
            for att in regular {
                mixed = mixed.singlepart(attachment_part(att)?);
            }
            builder.multipart(mixed)?
        }
    };

    Ok(message)
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd backend && cargo test --bin lyra_backend smtp::`
Expected: PASS (all new + existing smtp tests).

- [ ] **Step 5: Clippy + fmt + commit**

```bash
cd backend && cargo fmt && cargo clippy --all-targets --all-features -- -D warnings
git add backend/src/smtp.rs
git commit -m "feat(backend): inline image MIME parts (multipart/related, Content-ID)"
```

---

## Task 2: Backend — `inlineMeta` in the send multipart parser

**Files:**
- Modify: `backend/src/sync/send.rs` (`SendRequest::from_request` ~lines 116-200, tests at bottom of file)

- [ ] **Step 1: Write failing tests**

The file already has tests (check existing test module names with `grep -n "mod tests" backend/src/sync/send.rs`). Add tests that drive the parser via a constructed multipart request, mirroring whatever style the existing tests use. If no extractor-level test seam exists, add one factored helper and test that instead:

Refactor target (do this as part of implementation if needed): extract the metadata-application step into a pure function so it's unit-testable without HTTP:

```rust
/// Apply `inlineMeta` entries to parsed files (matched by filename, first
/// unmatched wins). Unmatched meta entries are ignored; invalid ids error.
pub(crate) fn apply_inline_meta(
    mut files: Vec<OutboundAttachment>,
    inline_meta: &[InlineMetaEntry],
) -> Result<Vec<OutboundAttachment>, SyncError> {
    let mut used = vec![false; inline_meta.len()];
    for file in &mut files {
        if let Some((idx, meta)) = inline_meta
            .iter()
            .enumerate()
            .find(|(i, m)| !used[*i] && m.filename == file.filename)
        {
            crate::smtp::validate_content_id(&meta.content_id)
                .map_err(|e| SyncError::InvalidInput(e.to_string()))?;
            file.content_id = Some(meta.content_id.clone());
            used[idx] = true;
        }
    }
    Ok(files)
}

/// `inlineMeta` form-field JSON entry (camelCase on the wire).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InlineMetaEntry {
    pub filename: String,
    pub content_id: String,
}
```

Tests:

```rust
    #[test]
    fn inline_meta_marks_matching_file() {
        let files = vec![
            OutboundAttachment::from_bytes("a.png", "image/png", b"1"),
            OutboundAttachment::from_bytes("b.pdf", "application/pdf", b"2"),
        ];
        let meta = [InlineMetaEntry { filename: "a.png".into(), content_id: "x@lyra".into() }];
        let out = apply_inline_meta(files, &meta).unwrap();
        assert_eq!(out[0].content_id.as_deref(), Some("x@lyra"));
        assert_eq!(out[1].content_id, None);
    }

    #[test]
    fn inline_meta_ignores_unknown_names_and_rejects_bad_ids() {
        let files = vec![OutboundAttachment::from_bytes("a.png", "image/png", b"1")];
        let unknown = [InlineMetaEntry { filename: "nope.png".into(), content_id: "x@lyra".into() }];
        assert!(apply_inline_meta(files.clone(), &unknown).unwrap()[0].content_id.is_none());
        let bad = [InlineMetaEntry { filename: "a.png".into(), content_id: "a>b".into() }];
        assert!(apply_inline_meta(files, &bad).is_err());
    }
```

- [ ] **Step 2: Run to verify fail**

Run: `cd backend && cargo test --bin lyra_backend inline_meta`
Expected: FAIL (function doesn't exist).

- [ ] **Step 3: Wire into the multipart parser**

In `SendRequest::from_request`, add a `let mut inline_meta: Vec<InlineMetaEntry> = Vec::new();` before the field loop, add a match arm:

```rust
                Some("inlineMeta") => {
                    let text = field
                        .text()
                        .await
                        .map_err(|e| SyncError::InvalidInput(e.to_string()))?;
                    inline_meta = serde_json::from_str(&text)
                        .map_err(|e| SyncError::InvalidInput(format!("inlineMeta: {e}")))?;
                }
```

and change the final `Ok(Self { ... })` to apply it:

```rust
        Ok(Self {
            json: payload.ok_or_else(|| {
                SyncError::InvalidInput("multipart send requires a payload part".into())
            })?,
            files: apply_inline_meta(files, &inline_meta)?,
        })
```

- [ ] **Step 4: Run to verify pass**

Run: `cd backend && cargo test --bin lyra_backend send`
Expected: PASS.

- [ ] **Step 5: Clippy + fmt + commit**

```bash
cd backend && cargo fmt && cargo clippy --all-targets --all-features -- -D warnings
git add backend/src/sync/send.rs
git commit -m "feat(backend): inlineMeta field marks inline attachments on send"
```

---

## Task 3: Backend — drafts accept inline attachments

**Files:**
- Modify: `backend/src/sync/http.rs` (`SaveDraftRequest` ~line 1857, `save_draft` ~line 1874, especially the hardcoded `attachments: Vec::new()` at ~line 1922)

- [ ] **Step 1: Write the failing test**

There are existing HTTP-level tests for drafts in `backend/src/main.rs` (e.g. `http_save_draft_requires_drafts_folder` ~line 825). Add a sibling test: POST `/api/v1/drafts` with an `inlineAttachments` array containing one tiny PNG (`dataBase64` of a few bytes, `contentId: "img1@lyra"`, `filename: "a.png"`, `contentType: "image/png"`), expect 200 and `"status": "saved"`. Follow the exact setup style of the existing draft tests (seeded drafts folder/account). Also a rejection test: `contentId: "bad>id"` → 400-level error.

- [ ] **Step 2: Run to verify fail**

Run: `cd backend && cargo test --bin lyra_backend draft`
Expected: the new test FAILS (field unknown / attachments dropped).

- [ ] **Step 3: Implement**

In `SaveDraftRequest` add:

```rust
    /// Inline images referenced as `cid:` from `body_html` (RFC 2392);
    /// persisted inside the draft's MIME so reopens restore them.
    #[serde(default)]
    inline_attachments: Vec<InlineAttachment>,
```

with

```rust
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InlineAttachment {
    filename: String,
    content_type: String,
    content_id: String,
    data_base64: String,
}
```

In `save_draft`, replace `attachments: Vec::new(),` with:

```rust
        attachments: parse_inline_attachments(&body.inline_attachments, state.max_attachment_bytes)?,
```

and add:

```rust
/// Draft inline images → outbound inline parts; same caps as send.
fn parse_inline_attachments(
    inline: &[InlineAttachment],
    max_bytes: u64,
) -> Result<Vec<crate::smtp::OutboundAttachment>, SyncError> {
    use base64::Engine as _;
    let mut out = Vec::with_capacity(inline.len());
    for att in inline {
        crate::smtp::validate_content_id(&att.content_id)
            .map_err(|e| SyncError::InvalidInput(e.to_string()))?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(att.data_base64.as_bytes())
            .map_err(|e| SyncError::InvalidInput(format!("inline {}: bad base64: {e}", att.filename)))?;
        if bytes.len() as u64 > max_bytes {
            return Err(SyncError::InvalidInput(format!(
                "inline image {} exceeds {max_bytes} bytes",
                att.filename
            )));
        }
        out.push(crate::smtp::OutboundAttachment::from_bytes_inline(
            &att.filename,
            &att.content_type,
            &bytes,
            &att.content_id,
        ));
    }
    Ok(out)
}
```

NOTE: `from_bytes_inline` double-encodes here (decode then re-encode) — that is intentional: validation of the wire format happens at the boundary. If clippy complains about needless allocation, accept it; clarity wins.

IMAP needs no further change (`save_draft` already runs `build_message` → APPEND). JMAP `create_draft` is Task 4.

- [ ] **Step 4: Run to verify pass**

Run: `cd backend && cargo test --bin lyra_backend draft`
Expected: PASS.

- [ ] **Step 5: Clippy + fmt + commit**

```bash
cd backend && cargo fmt && cargo clippy --all-targets --all-features -- -D warnings
git add backend/src/sync/http.rs backend/src/main.rs
git commit -m "feat(backend): drafts persist inline image attachments"
```

---

## Task 4: Backend — JMAP send/draft via raw-MIME import for inline messages

**Files:**
- Modify: `backend/src/sync/jmap_client.rs` (`submit_outbound` ~line 915, `create_draft` ~line 1335)

**Context:** The jmap-client 0.4.2 `EmailBodyPart<Set>` builder exposes `.content_id()` (sets `cid`) but NO `disposition` setter, so `Email/set` cannot reliably produce `Content-Disposition: inline` on every server. The proven raw-MIME path already exists: `submit_mime_wrapped` (Email/import of an RFC822 blob + EmailSubmission, used by OpenGPG) and `create_draft` (Email/set, currently never receives attachments). Route any message with inline parts through raw MIME built by `crate::smtp::build_message` — byte-exact control of the wire format.

- [ ] **Step 1: Write the failing test**

Add a pure gate + tests (bottom test module of jmap_client.rs):

```rust
/// True when the message must go through raw-MIME import instead of
/// Email/set: inline parts need exact Content-ID/inline disposition control.
fn needs_mime_import(outbound: &OutboundMessage) -> bool {
    outbound.mime_body.is_some()
        || outbound.attachments.iter().any(|a| a.content_id.is_some())
}

#[cfg(test)]
mod mime_import_gate_tests {
    use super::*;

    fn minimal_msg() -> OutboundMessage {
        OutboundMessage {
            from_email: "a@example.com".into(),
            from_name: None,
            to: vec![(None, "b@example.com".into())],
            cc: vec![],
            bcc: vec![],
            subject: "s".into(),
            body_text: Some("hi".into()),
            body_html: None,
            in_reply_to: None,
            references: None,
            mime_content_type: None,
            mime_body: None,
            attachments: vec![],
            message_id: None,
        }
    }

    #[test]
    fn plain_message_uses_email_set() {
        assert!(!needs_mime_import(&minimal_msg()));
    }

    #[test]
    fn opengpg_wrapper_forces_mime_import() {
        let mut msg = minimal_msg();
        msg.mime_content_type = Some("multipart/encrypted".into());
        msg.mime_body = Some("wrapped".into());
        assert!(needs_mime_import(&msg));
    }

    #[test]
    fn inline_attachment_forces_mime_import() {
        let mut msg = minimal_msg();
        msg.attachments.push(crate::smtp::OutboundAttachment::from_bytes_inline(
            "a.png", "image/png", b"x", "img1@lyra",
        ));
        assert!(needs_mime_import(&msg));
    }

    #[test]
    fn regular_attachment_stays_on_email_set() {
        let mut msg = minimal_msg();
        msg.attachments.push(crate::smtp::OutboundAttachment::from_bytes("d.pdf", "application/pdf", b"x"));
        assert!(!needs_mime_import(&msg));
    }
}
```

- [ ] **Step 2: Run to verify fail**

Run: `cd backend && cargo test --bin lyra_backend mime_import_gate`
Expected: FAIL (function doesn't exist).

- [ ] **Step 3: Implement**

In `submit_outbound`, replace the `if let Some(mime_body) = &outbound.mime_body` branch with:

```rust
        if needs_mime_import(outbound) {
            let mime_body = match &outbound.mime_body {
                // OpenGPG pre-built wrapper (legacy path).
                Some(wrapper) => wrapper.clone(),
                None => String::from_utf8(
                    crate::smtp::build_message(outbound)
                        .map_err(|e| JmapError::InvalidResponse(format!("mime build: {e}")))?
                        .formatted(),
                )
                .map_err(|e| JmapError::InvalidResponse(format!("mime utf8: {e}")))?,
            };
            return self
                .submit_mime_wrapped(&mime_body, &identity_id, drafts_id, sent_id, &mailboxes)
                .await;
        }
```

In `create_draft`, add at the top (after `drafts_id` resolution):

```rust
        if !outbound.attachments.is_empty() {
            // Email/set cannot express inline parts; import exact MIME instead.
            let mime = String::from_utf8(
                crate::smtp::build_message(outbound)
                    .map_err(|e| JmapError::InvalidResponse(format!("mime build: {e}")))?
                    .formatted(),
            )
            .map_err(|e| JmapError::InvalidResponse(format!("mime utf8: {e}")))?;
            let blob_id = self
                .client
                .upload(None, mime.into_bytes(), Some("message/rfc822"))
                .await?
                .take_blob_id();
            let mut request = self.build_request();
            let create_id = {
                let import_req = request.import_email();
                let import = import_req.email(blob_id);
                import.mailbox_ids([drafts_id.as_str()]);
                import.keywords(["$draft"]);
                import.create_id()
            };
            let mut resp = request
                .send_single::<EmailImportResponse>()
                .await?;
            let mut created = resp.created(&create_id)?;
            return Ok(created.take_id());
        }
```

NOTE: check the import response type name used by `submit_mime_wrapped`'s `unwrap_import_email()` — for `send_single` the response type may be `EmailImportResponse` or similar; mirror whatever the crate exposes (`grep -n "import_email\|ImportEmail" backend/src/sync/jmap_client.rs` and the crate source under `~/.cargo/registry/src/*/jmap-client-0.4.2/src/email/import.rs`). If `send_single` doesn't support import responses, send the batched request and unwrap like `submit_mime_wrapped` does.

- [ ] **Step 4: Run to verify pass**

Run: `cd backend && cargo test --bin lyra_backend`
Expected: PASS (full backend suite green — this touches shared paths).

- [ ] **Step 5: Clippy + fmt + commit**

```bash
cd backend && cargo fmt && cargo clippy --all-targets --all-features -- -D warnings
git add backend/src/sync/jmap_client.rs
git commit -m "feat(backend): JMAP inline messages via raw-MIME Email/import"
```

---

## Task 5: Frontend — `src/lib/inline-images.ts` pure helpers

**Files:**
- Create: `frontend/src/lib/inline-images.ts`
- Test: `frontend/src/lib/inline-images.test.ts`

- [ ] **Step 1: Write the failing test**

Create `frontend/src/lib/inline-images.test.ts`:

```ts
import { describe, expect, it } from 'vitest';

import {
  extractInlineImages,
  fileToBase64,
  newContentId,
  resolveInlineSources,
} from '@/lib/inline-images';

const png = new File([new Uint8Array([1, 2, 3])], 'photo.png', { type: 'image/png' });

describe('newContentId', () => {
  it('is a msg-id-style value without angle brackets', () => {
    const cid = newContentId();
    expect(cid).toMatch(/^[0-9a-f-]+@lyra$/);
  });
});

describe('extractInlineImages', () => {
  it('rewrites tracked blob URLs to cid and collects parts once', () => {
    const map = new Map([['blob:x', { file: png, contentId: 'c1@lyra' }]]);
    const { html, parts } = extractInlineImages(
      '<p>a</p><img src="blob:x"><img src="blob:x">',
      map,
    );
    expect(html).toBe('<p>a</p><img src="cid:c1@lyra"><img src="cid:c1@lyra">');
    expect(parts).toHaveLength(1);
    expect(parts[0]).toMatchObject({
      filename: 'photo.png',
      contentType: 'image/png',
      contentId: 'c1@lyra',
    });
  });

  it('leaves unknown blob URLs and remote URLs untouched', () => {
    const { html, parts } = extractInlineImages(
      '<img src="blob:unknown"><img src="https://example.com/x.png">',
      new Map(),
    );
    expect(html).toContain('src="blob:unknown"');
    expect(html).toContain('src="https://example.com/x.png"');
    expect(parts).toHaveLength(0);
  });

  it('no blob URLs at all → unchanged input, no parts', () => {
    const { html, parts } = extractInlineImages('<p>plain</p>', new Map());
    expect(html).toBe('<p>plain</p>');
    expect(parts).toHaveLength(0);
  });
});

describe('resolveInlineSources', () => {
  const source = { id: 'att1', filename: 'a.png', contentType: 'image/png', contentId: 'c1@lyra' };
  const fetchBlob = async () => new Blob([new Uint8Array([9])], { type: 'image/png' });

  it('rewrites matching cid refs to object URLs and maps them back', async () => {
    const { html, urlToImage } = await resolveInlineSources(
      '<img src="cid:c1@lyra">',
      [source],
      fetchBlob,
    );
    expect(html).not.toContain('cid:');
    const [url, entry] = [...urlToImage.entries()][0];
    expect(html).toContain(`src="${url}"`);
    expect(entry.contentId).toBe('c1@lyra');
    expect(entry.file.type).toBe('image/png');
    expect(entry.file.name).toBe('a.png');
  });

  it('skips sources whose cid is not referenced', async () => {
    const { html, urlToImage } = await resolveInlineSources('<p>none</p>', [source], fetchBlob);
    expect(html).toBe('<p>none</p>');
    expect(urlToImage.size).toBe(0);
  });

  it('degrades when a fetch fails: cid stays, other images still resolve', async () => {
    const sources = [source, { id: 'att2', filename: 'b.png', contentId: 'c2@lyra' }];
    const flaky = async (id: string) => {
      if (id === 'att1') throw new Error('gone');
      return new Blob([new Uint8Array([1])]);
    };
    const { html, urlToImage } = await resolveInlineSources(
      '<img src="cid:c1@lyra"><img src="cid:c2@lyra">',
      sources,
      flaky,
    );
    expect(html).toContain('src="cid:c1@lyra"');
    expect(urlToImage.size).toBe(1);
  });
});

describe('fileToBase64', () => {
  it('round-trips bytes', async () => {
    const b64 = await fileToBase64(png);
    expect(b64).toBe('AQID');
  });
});
```

- [ ] **Step 2: Run to verify fail**

Run: `cd frontend && npx vitest run src/lib/inline-images.test.ts`
Expected: FAIL (module doesn't exist).

- [ ] **Step 3: Implement**

Create `frontend/src/lib/inline-images.ts`:

```ts
/**
 * Compose-side inline image plumbing: object-URL ↔ cid: rewriting, Content-ID
 * generation, base64 for the drafts JSON. Pure helpers; the only side effects
 * are `URL.createObjectURL` and the caller-provided `fetchBlob`.
 */

import { normalizeCid } from '@/lib/attachments';

export interface InlineImageEntry {
  file: File;
  contentId: string;
}

export interface InlineImagePart {
  filename: string;
  contentType: string;
  contentId: string;
  file: File;
}

/** RFC 2045 msg-id-style Content-ID value (brackets added by the backend). */
export function newContentId(): string {
  return `${crypto.randomUUID()}@lyra`;
}

/**
 * Rewrite tracked object-URL image srcs to `cid:` refs (RFC 2392) and collect
 * each referenced image once. Untracked/remote URLs pass through unchanged.
 */
export function extractInlineImages(
  html: string,
  urlToImage: ReadonlyMap<string, InlineImageEntry>,
): { html: string; parts: InlineImagePart[] } {
  if (!html.includes('blob:')) return { html, parts: [] };
  const parts: InlineImagePart[] = [];
  const seen = new Set<string>();
  const rewritten = html.replace(/src=["'](blob:[^"']+)["']/g, (match, url: string) => {
    const entry = urlToImage.get(url);
    if (!entry) return match;
    if (!seen.has(url)) {
      seen.add(url);
      parts.push({
        filename: entry.file.name || 'image',
        contentType: entry.file.type || 'image/png',
        contentId: entry.contentId,
        file: entry.file,
      });
    }
    return `src="cid:${entry.contentId}"`;
  });
  return { html: rewritten, parts };
}

/** Attachment metadata for an inline part of an existing message/draft. */
export interface InlineSourceMeta {
  id: string;
  filename?: string;
  contentType?: string;
  contentId?: string;
}

/**
 * Reopened draft / quoted body: rewrite `cid:` refs to fresh object URLs and
 * map each URL back to its bytes + original Content-ID (reused on re-send).
 */
export async function resolveInlineSources(
  html: string,
  sources: InlineSourceMeta[],
  fetchBlob: (id: string) => Promise<Blob>,
): Promise<{ html: string; urlToImage: Map<string, InlineImageEntry> }> {
  const urlToImage = new Map<string, InlineImageEntry>();
  if (!html.toLowerCase().includes('cid:')) return { html, urlToImage };
  let out = html;
  for (const source of sources) {
    if (!source.contentId) continue;
    const cid = normalizeCid(source.contentId);
    if (!out.toLowerCase().includes(`cid:${cid}`)) continue;
    try {
      const blob = await fetchBlob(source.id);
      const file = new File([blob], source.filename || 'image', {
        type: source.contentType || blob.type || 'image/png',
      });
      const url = URL.createObjectURL(blob);
      urlToImage.set(url, { file, contentId: cid });
      out = out.replace(
        new RegExp(`src=["']cid:${cid.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}["']`, 'gi'),
        `src="${url}"`,
      );
    } catch {
      // Broken part: leave the cid ref inert; send drops it via extractInlineImages.
    }
  }
  return { html: out, urlToImage };
}

/** File → base64 (chunked; large images don't blow the call stack). */
export async function fileToBase64(file: File): Promise<string> {
  const bytes = new Uint8Array(await file.arrayBuffer());
  let binary = '';
  const CHUNK = 0x8000;
  for (let i = 0; i < bytes.length; i += CHUNK) {
    binary += String.fromCharCode(...bytes.subarray(i, i + CHUNK));
  }
  return btoa(binary);
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cd frontend && npx vitest run src/lib/inline-images.test.ts`
Expected: PASS — 8 tests. (jsdom/happy-dom provides `URL.createObjectURL`; if the test env lacks it, add a stub at the top of the test file: `URL.createObjectURL ??= (b) => \`blob:mock-${(b as Blob).size}\`;` — check existing tests for the pattern first.)

- [ ] **Step 5: Commit**

```bash
git add frontend/src/lib/inline-images.ts frontend/src/lib/inline-images.test.ts
git commit -m "feat(frontend): inline image cid/object-url helpers"
```

---

## Task 6: Frontend — Plate image support in the rich editor

**Files:**
- Modify: `frontend/package.json` (add dependency)
- Modify: `frontend/src/components/compose/rich-text-editor.tsx`

- [ ] **Step 1: Install `@platejs/media` (v53 line)**

```bash
cd frontend && npm install @platejs/media@^53.1.4
```

Verify the installed version is 53.x (`npm ls @platejs/media`). If install fails or the plugin API below doesn't match the installed package, STOP and report — do not silently substitute a different major API.

- [ ] **Step 2: Add the image plugin + insert plumbing**

In `frontend/src/components/compose/rich-text-editor.tsx`:

1. Imports:

```ts
import { Image as ImageIcon } from 'lucide-react';
import { ImagePlugin } from '@platejs/media/react';
```

2. Add to `RichTextEditorProps`:

```ts
  /**
   * Image file from toolbar/paste/drop → object URL to display, or null to
   * reject (size/type). Ownership of the URL stays with the caller.
   */
  onImageFile?: (file: File) => string | null;
```

3. Image render component (module level, above `PLUGINS`):

```tsx
/** Minimal void image node; selection children render in a hidden span. */
function ImageElement({
  attributes,
  children,
  element,
}: {
  attributes: Record<string, unknown>;
  children: React.ReactNode;
  element: { url?: string };
}) {
  return (
    <div {...attributes} contentEditable={false} className="my-1 select-none">
      <img src={element.url} alt="" className="max-h-64 max-w-full rounded" />
      {children}
    </div>
  );
}
```

NOTE: verify Plate v53's render-prop signature against the installed package — if `render.node` receives different props, adapt (the editor's existing `editor.api.html.deserialize` must produce `img` nodes from `<img src>` and `serializeHtml` must emit `<img src="…">`; that's the acceptance contract).

4. Register the plugin in `PLUGINS`:

```ts
  ImagePlugin.configure({
    render: { node: ImageElement },
  }),
```

5. Inside `RichTextEditor`, add the insert helper and paste/drop handlers:

```tsx
  const insertImageFile = (file: File) => {
    if (!onImageFile || !file.type.startsWith('image/')) return false;
    const url = onImageFile(file);
    if (!url) return true; // rejected (too large) — handled, error shown by caller
    editor.tf.insertNodes({ type: ImagePlugin.key, url, children: [{ text: '' }] });
    return true;
  };
```

Pass `onImageFile` through destructured props. On `PlateContent`, add:

```tsx
          onPaste={(e) => {
            const files = Array.from(e.clipboardData?.files ?? []);
            if (files.some((f) => f.type.startsWith('image/'))) {
              e.preventDefault();
              files.forEach(insertImageFile);
            }
          }}
          onDrop={(e) => {
            const files = Array.from(e.dataTransfer?.files ?? []);
            if (files.some((f) => f.type.startsWith('image/'))) {
              e.preventDefault();
              files.forEach(insertImageFile);
            }
          }}
```

(Non-image files in the same paste/drop are ignored here; the compose dialog's attachment input remains the path for those.)

6. Toolbar button — in `Toolbar`, after the Link button. The toolbar needs the same insert capability, so lift `insertImageFile` down via prop (`Toolbar({ disabled, position, onImageFile })` → hidden `<input type="file" accept="image/*">` + button):

```tsx
      <input
        ref={imageInputRef}
        type="file"
        accept="image/*"
        multiple
        className="hidden"
        onChange={(e) => {
          Array.from(e.target.files ?? []).forEach(insertImageFile);
          e.target.value = '';
        }}
      />
      <Button
        type="button"
        variant="ghost"
        size="icon"
        className={btn}
        disabled={disabled || !onImageFile}
        onMouseDown={(e) => e.preventDefault()}
        onClick={() => imageInputRef.current?.click()}
        aria-label={t(useUIStore.getState().locale, 'mail.insertImage')}
      >
        <ImageIcon className="size-3.5" />
      </Button>
```

Check how other components read locale — if `useUIStore` import is unwanted here, hardcode aria-label like the existing buttons do ("Bold" etc. are hardcoded English aria-labels in this file — follow that precedent: `aria-label="Insert image"`).

- [ ] **Step 3: Gate**

Run: `cd frontend && npm run check`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add frontend/package.json frontend/package-lock.json frontend/src/components/compose/rich-text-editor.tsx
git commit -m "feat(frontend): image plugin + insert paths in compose editor"
```

---

## Task 7: Frontend — compose dialog wiring (send + autosave + reopen)

**Files:**
- Modify: `frontend/src/stores/ui.ts` (`ComposeDraft` ~line 11)
- Modify: `frontend/src/lib/compose-draft.ts` (reply/forward builders)
- Modify: `frontend/src/lib/conversation-actions.ts` (`editDraftFromList` ~line 159)
- Modify: `frontend/src/components/compose-dialog.tsx`

- [ ] **Step 1: Carry inline sources on ComposeDraft**

In `frontend/src/stores/ui.ts`, add to `ComposeDraft`:

```ts
  /** Inline (cid:) parts of the source message/draft — resolved to object
   *  URLs when the dialog seeds, re-attached with their original Content-ID. */
  inlineSources?: Array<{ id: string; filename?: string; contentType?: string; contentId?: string }>;
```

CRITICAL: `openCompose` in this store whitelists draft fields (learned from the accountId work) — find the whitelist and add `inlineSources` or the field will be silently dropped.

In `frontend/src/lib/compose-draft.ts`, add to BOTH `buildReplyDraft` and `buildForwardDraft` returns:

```ts
    inlineSources: (m.attachments ?? [])
      .filter((a) => a.isInline && a.contentId)
      .map((a) => ({ id: a.id, filename: a.filename, contentType: a.contentType, contentId: a.contentId ?? undefined })),
```

(omit when empty). In `editDraftFromList` (`conversation-actions.ts:163`), add the same mapping to the `openCompose({...})` call. Check `MailAttachment` in `frontend/src/types/index.ts` for exact field names first.

- [ ] **Step 2: Dialog state + seed resolution**

In `compose-dialog.tsx`:

1. New imports:

```ts
import { extractInlineImages, fileToBase64, newContentId, resolveInlineSources, type InlineImageEntry } from '@/lib/inline-images';
```

2. New state/ref next to `files`:

```ts
  /** objectURL → {file, contentId} for every inline image in the editor. */
  const inlineImagesRef = useRef(new Map<string, InlineImageEntry>());
```

3. `insertImageFile` passed to the editor (validates + tracks):

```ts
  const insertImageFile = (file: File): string | null => {
    if (file.size > MAX_ATTACHMENT_BYTES) {
      setError(t(locale, 'mail.attachmentTooLarge', { name: file.name || 'image' }));
      return null;
    }
    const url = URL.createObjectURL(file);
    inlineImagesRef.current.set(url, { file, contentId: newContentId() });
    return url;
  };
```

Pass `onImageFile={insertImageFile}` to `<RichTextEditor ... />` (line ~597).

4. Seed resolution — extend the existing open/seed effect (the one keyed on `[composeOpen, composeDraft]`, ~line 135-178). After computing `seeded`, if `composeDraft?.inlineSources?.length` and `seeded` contains `cid:`, resolve BEFORE setting editor state:

```ts
    const sources = composeDraft?.inlineSources ?? [];
    if (sources.length > 0 && seeded.toLowerCase().includes('cid:')) {
      let cancelled = false;
      void resolveInlineSources(seeded, sources, (id) => apiBlob(`/attachments/${id}/download`)).then(
        ({ html, urlToImage }) => {
          if (cancelled) return;
          inlineImagesRef.current = urlToImage;
          setEditorHtml(html);
          setInitialHtml(html);
          setEditorKey((k) => k + 1);
        },
      );
      return () => { cancelled = true; };
    }
    setEditorHtml(seeded);
    setInitialHtml(seeded);
    setEditorKey((k) => k + 1);
```

Restructure the existing effect carefully — keep the From-account resolution logic exactly as-is.

5. Revoke object URLs on close: in the close/reset path (find where `files` and editor state reset — `handleClose` / the `composeOpen === false` cleanup), add:

```ts
    for (const url of inlineImagesRef.current.keys()) URL.revokeObjectURL(url);
    inlineImagesRef.current.clear();
```

- [ ] **Step 3: Send path**

In the send handler (~line 406), before building `payload`:

```ts
      const { html: sendHtml, parts: inlineParts } = extractInlineImages(
        editorHtml,
        inlineImagesRef.current,
      );
      const bodyHtml = richMode ? sendHtml || null : null;
```

(`bodyText` stays `htmlToText(editorHtml ?? '')`.) Change the send branch:

```ts
      if (files.length > 0 || inlineParts.length > 0) {
        const fd = new FormData();
        fd.append('payload', new Blob([JSON.stringify(payload)], { type: 'application/json' }));
        for (const f of files) fd.append('files', f, f.name);
        for (const part of inlineParts) fd.append('files', part.file, part.filename);
        if (inlineParts.length > 0) {
          fd.append(
            'inlineMeta',
            JSON.stringify(
              inlineParts.map((p) => ({ filename: p.filename, contentId: p.contentId })),
            ),
          );
        }
        await api('/messages/send', { method: 'POST', body: fd });
      } else { /* existing JSON branch */ }
```

NOTE: inline filenames must be unique within one send for the backend's filename matching. When two inserted files share a name, suffix on insert in `insertImageFile` (e.g. `name`, `name-2`) — implement a small uniqueness check against `inlineImagesRef.current` values.

- [ ] **Step 4: Autosave with inline attachments**

In the autosave effect (~lines 278-306): keep the `files.length > 0` skip (regular attachments still skip drafts), but allow inline images. Inside the async save, compute:

```ts
            const { html: draftHtml, parts: draftInline } = extractInlineImages(
              currentBodyHtml ?? '',
              inlineImagesRef.current,
            );
            const inlineAttachments = await Promise.all(
              draftInline.map(async (p) => ({
                filename: p.filename,
                contentType: p.contentType,
                contentId: p.contentId,
                dataBase64: await fileToBase64(p.file),
              })),
            );
```

and send `bodyHtml: richMode ? draftHtml || undefined : undefined` plus `inlineAttachments` (omit the key when empty) in the `/drafts` POST body. The `autosavePayload` dirty-hash must NOT include base64 — it already hashes `bodyHtml` (which contains the object URLs); that's sufficient since inserting/removing an image changes the HTML. Leave the hash as-is.

- [ ] **Step 5: Gate**

Run: `cd frontend && npm run check && npx vitest run`
Expected: PASS, all tests green.

- [ ] **Step 6: Commit**

```bash
git add frontend/src/stores/ui.ts frontend/src/lib/compose-draft.ts frontend/src/lib/conversation-actions.ts frontend/src/components/compose-dialog.tsx
git commit -m "feat(frontend): inline images through compose, send, and drafts"
```

---

## Task 8: i18n + OpenAPI doc

**Files:**
- Modify: `frontend/src/i18n/en.json`, `frontend/src/i18n/zh.json`
- Modify: `docs/openapi/api-v1.yaml` (send multipart + drafts request bodies)

- [ ] **Step 1: Strings** — add under `mail` in both files:

en: `"insertImage": "Insert image"`
zh: `"insertImage": "插入图片"`

Run: `cd frontend && npx vitest run src/i18n` → PASS (parity).

- [ ] **Step 2: OpenAPI** — document `inlineMeta` on the `/messages/send` multipart schema and `inlineAttachments` on `POST /drafts`. Match the file's existing style.

- [ ] **Step 3: Commit**

```bash
git add frontend/src/i18n/en.json frontend/src/i18n/zh.json docs/openapi/api-v1.yaml
git commit -m "docs: inline image API surface + i18n"
```

---

## Task 9: Full verification

- [ ] **Step 1: Full gates**

```bash
cd frontend && npm run check && npx vitest run
cd backend && cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test --bin lyra_backend
```

Expected: all green.

- [ ] **Step 2: Live smoke test**

With the Vite dev server (`http://127.0.0.1:5173`) and the Docker backend: compose a message with a pasted image + a file attachment, send to a real account, verify the received message renders the image inline (in Lyra's own message view — the reading side already resolves `cid:`) and shows the file as an attachment. Save a draft with an image, reload, reopen the draft, verify the image is still there. (Skip gracefully if no live backend is reachable; note it in the report.)

- [ ] **Step 3: Report**

Summarize. Do NOT push without explicit user approval.
