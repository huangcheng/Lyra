# Inline images in compose — design

Date: 2026-09-03
Status: approved direction (brainstorming)

## Goal

Users can insert images into the rich-text compose editor (toolbar button,
paste, drag-drop); those images are embedded in the message itself as inline
MIME parts and render in place for recipients. Drafts with inline images
persist across sessions (autosave + reopen). File attachments keep working
unchanged alongside.

## RFC alignment

The produced MIME is textbook standard:

```
multipart/mixed                     ← RFC 2046 (only when file attachments exist)
├── multipart/related               ← RFC 2387
│   ├── multipart/alternative       ← RFC 2046 (text + html)
│   │   ├── text/plain
│   │   └── text/html               ← contains <img src="cid:x"> (RFC 2392)
│   └── image/png …                 ← Content-ID: <x> (RFC 2045 §7.5.2),
│                                     Content-Disposition: inline (RFC 2183)
└── application/pdf …               ← Content-Disposition: attachment (RFC 2183)
```

- `multipart/related` is omitted when there are no inline images; `mixed` is
  omitted when there are no file attachments; `alternative` collapses to a
  single body part when only one body form exists (current behavior).
- Content-ID values: `<uuid@lyra>` — RFC 2045 msg-id syntax, locally generated.
- Non-ASCII filenames keep relying on lettre's RFC 2047/2231 encoding.
- Historical context: RFC 2557 (MHTML) describes this exact "HTML mail with
  embedded resources" pattern.

## Non-goals

- No image downscaling/re-encoding (bytes go as-is; existing 25 MB per-file cap
  applies to inline images too).
- No remote-URL image insertion (paste of `<img src="https://…">` keeps the
  remote URL — sanitizer/reading side already governs display).
- No OpenGPG support for inline images in v1: when sign/encrypt is active, the
  crypto wrapper (`mime_body`) replaces the body and inline images are
  downgraded to regular attachments (they still arrive, just not inline).
- No `Content-Location` support (cid only).

## Current state (verified)

- Reading side already consumes this format: `MailAttachment.isInline` /
  `contentId` are parsed, and `frontend/src/lib/attachments.ts`
  `resolveInlineImages` rewrites `cid:` to object URLs in the message view.
- Production side has none of it: `OutboundAttachment`
  (`backend/src/smtp.rs:121`) is filename/content-type/bytes only;
  `build_message` emits `attachment` disposition for everything
  (`smtp.rs:378`); the Plate editor has no image plugin
  (`frontend/src/components/compose/rich-text-editor.tsx`); `/api/v1/drafts`
  hardcodes `attachments: Vec::new()` (`backend/src/sync/http.rs:1922`) and the
  compose autosave is disabled whenever files are attached.
- `save_draft` builds drafts through the same `smtp::build_message` as sending
  (http.rs:1931, IMAP arm) — so once `build_message` understands inline parts,
  IMAP draft persistence inherits it. JMAP creates drafts via `Email/set`
  (`jmap_client.rs` `create_draft` / `fill_outbound_email`), which needs the
  same `cid`/`disposition` treatment as the submit path.

## Architecture

### Backend

1. **`OutboundAttachment` gains `content_id: Option<String>`** (serde
   `default`, so the plugin/job JSON wire stays backward compatible).
   `content_id.is_some()` ⇔ inline part.
2. **`smtp.rs build_message`**: split attachments into inline vs regular.
   Inline parts get `Content-ID` + `Disposition: inline` and are grouped with
   the body in `multipart/related`; regular attachments keep
   `multipart/mixed`. Empty layers are omitted (structure above). Unit tests
   assert the header structure of the formatted output for the four
   combinations (plain/html × none/inline/files/both).
3. **`send.rs` multipart parse**: accept an optional `inlineMeta` form field
   (JSON `[{filename, contentId}]`); matching `files` parts get `content_id`
   set. Unmatched names are ignored (they just stay regular attachments).
4. **`http.rs SaveDraftRequest`**: accept `inlineAttachments:
   [{filename, contentType, contentId, dataBase64}]`, mapped into
   `outbound.attachments` with `content_id` — IMAP APPEND then stores the full
   MIME including related parts, and the next sync re-parses them through the
   existing receive path.
5. **JMAP (`jmap_client.rs`)**: `upload_attachments` +
   `fill_outbound_email` set `cid` and `disposition: "inline"` on
   `EmailBodyPart` for inline attachments (RFC 8621 EmailBodyPart carries both
   fields — verify the jmap-client 0.4.2 builder exposes them; if not, set via
   the raw arguments JSON). Apply to both `submit_outbound` and `create_draft`.
   Live-verify against the Fastmail account that the produced MIME is
   `multipart/related`.

### Frontend

6. **`src/lib/inline-images.ts`** (pure, unit-tested):
   - `newContentId()` → `<uuid@lyra>`
   - `extractInlineImages(html, urlToFile)` → `{ html, parts }`: rewrites
     object-URL `src`s to `cid:` and returns the matching parts
     `{filename, contentType, contentId, file}`; HTML with no inline images
     returns unchanged with empty parts.
   - `resolveDraftInlineImages(html, attachments, fetchBlob)` → rewrites
     `cid:` refs in a reopened draft's HTML to object URLs and returns a
     `url → {file, contentId}` map (mirrors `attachments.ts` reading-side
     logic, but keeps the original Content-ID so a re-send reuses it).
7. **Editor (`rich-text-editor.tsx`)**: add the Plate image element —
   `@platejs/media` (v53-compatible line) if it installs cleanly, otherwise a
   minimal custom void `img` element plugin (deserialize/serialize HTML rules).
   Toolbar image button + paste/drop handlers turn an image `File` into an
   object-URL image node; object URLs are revoked when the node is removed or
   the dialog closes. Non-image files dropped/pasted go to the existing
   attachment list.
8. **Compose dialog (`compose-dialog.tsx`)**:
   - Track `url → File` for inserted images.
   - Send: run `extractInlineImages`; when parts exist, always use the
     FormData path, appending image bytes as `files` and the metadata as
     `inlineMeta`.
   - Autosave: no longer disabled by inline images (still disabled by regular
     file attachments); `inlineAttachments` (base64) ride the existing
     `/drafts` JSON. The dirty-check hash excludes the base64 payloads (HTML +
     fields only; inserting/removing an image changes the HTML anyway).
   - Reopen draft: `resolveDraftInlineImages` against the draft detail's
     attachments before seeding the editor.
   - Reply/forward quotes: quoted `cid:` refs are resolved the same way
     against the original message's attachments and become inline parts on
     send (fixes today's broken images in quoted bodies).
9. **i18n**: en + zh strings for the image button, per-file size error
   (reuse), and draft/upload failures.

## Error handling

- Oversized image → the existing `attachmentTooLarge` string; the image is not
  inserted.
- Blob fetch failure while resolving a reopened draft → that `cid:` stays
  inert in the editor (image renders broken; send degrades it to nothing
  rather than failing the send).
- `inlineMeta` entries that match no uploaded part are ignored; parts named in
  `files` but absent from `inlineMeta` stay regular attachments.
- Backend rejects a `contentId` containing `>`/whitespace (defense against
  header injection) with `InvalidInput`.

## Testing

- `smtp.rs`: formatted-MIME structure tests for the four layer combinations;
  Content-ID/inline-disposition presence; header-injection rejection.
- `send.rs` / `http.rs`: multipart `inlineMeta` parse; drafts JSON with
  inlineAttachments roundtrip (SQLite suite).
- `jmap_client.rs`: unit test that `fill_outbound_email` sets cid/disposition
  on inline parts (existing test seams); live Fastmail verification manually.
- Frontend: vitest for `inline-images.ts` (extract, rewrite, reopen mapping,
  edge cases: no images, duplicate cids, unknown urls).
- Gate: `make check` (frontend `npm run check` + backend clippy/tests).

## Notes

- No DB migration: outbound inline parts live inside the stored MIME and are
  re-parsed by the existing receive pipeline.
- Autosave payload size: inline images are base64 in the `/drafts` JSON. With
  the 25 MB per-file cap this is bounded but heavy; if it proves sluggish, a
  follow-up can threshold autosave off above N MB of inline bytes.
