# Lyra v1 — Completion Plan (Goal Freeze)

**Date:** 2026-08-27
**Status:** Active goal — this doc defines what "v1 done" means
**Audience:** Contributors scoping the remaining v1 work
**Parent spec:** [v1 product spec](./2026-08-20-lyra-v1-product-spec.md) (unmodified; this doc closes its gaps)

---

## Why this doc exists

An audit against the v1 product spec (2026-08-27) found the skeleton complete — every numbered
commitment has an implementation — but two core-UX items half-built and a compose experience that
would not survive contact with real users. This doc freezes the **remaining scope**: the five
workstreams below are the finish line. When they pass their acceptance criteria and the original
success criteria 1–7 hold, v1 ships. Nothing else gets added to v1.

## Audit snapshot (2026-08-27)

| v1 commitment | Status |
|---|---|
| 1. Self-hosted, single-user + optional TOTP | done |
| 2. Multi-account mail | done (unified inbox + switcher) |
| 3. JMAP / IMAP / SMTP | done |
| 4. Sync engine (store + index) | done (loops, recovery, SSE events, cursors) |
| 5. Core mail UX | **partial** — folders, conversations, compose/reply/forward, search, flags, snooze, trash/archive/spam ✓; **attachments UI missing**; **move-to-folder missing** |
| 6. Auto server config | done (`/accounts/probe`) |
| 7. CardDAV + CalDAV | done (`dav.rs`, contacts/calendar surfaces) |
| 8. shadcn mail product UI | done (redesign v2) |
| 9. i18n en/zh | done |
| 10. Docker Compose + install script | done (Dockerfile, compose, `deploy/`, `scripts/install.sh`) |
| 11. Dual-DB data layer | done (SQLite + PostgreSQL migrations) |
| 12. Security basics | **partial** — DEK-encrypted credentials ✓, length-only password policy, HTTPS guidance thin |

OpenGPG P1–P3 (per-account binding) is done and **not** reopening; P4 (WKD/Autocrypt, key expiry)
stays optional/post-v1 per its spec.

---

## The v1 goal, in one sentence

A self-hoster brings Lyra up, adds two accounts, and lives in it for a week — reading with
attachments, filing mail into folders, and writing properly formatted replies with drafts —
without once feeling "this is a demo".

## Workstreams

### W1 — Attachments (read + compose) — the blocker

Status 2026-08-27: shipped except JMAP-receive metadata (see note below).

Backend list/download endpoints already exist (`GET /messages/{id}/attachments[/{attachmentId}]`).

Acceptance criteria:

- [x] Reading pane lists attachments (name, size, icon) and downloads via the existing endpoints.
- [x] Inline images in HTML bodies render (cid/inline parts), not just attachments.
- [x] Compose supports attaching files; SMTP path sends proper MIME `multipart/mixed`
      (+ `multipart/alternative` inner body); JMAP path sets attachments per protocol
      (blob upload → `Email/set` `attachments`).
- [x] Reply/forward carries original attachments: **forward carries the original's
      non-inline attachments (fetched client-side and re-attached); reply drops them**
      (Thunderbird default behavior).
- [x] Attachment size cap enforced server-side with a typed error (`LYRA_MAX_ATTACHMENT_BYTES`,
      default 25 MiB per file, max 10 per send; request-body limit raised accordingly); i18n'd.
- [ ] **Deferred:** JMAP *receive* attachment metadata persistence + on-demand
      `Blob/download` proxy (needs `attachment.external_blob_id` migration + session
      `downloadUrl`). Until then JMAP accounts show the has-attachments flag only.
      IMAP accounts (metadata persisted at lazy body-fill) are fully covered.

### W2 — Move to folder — shipped 2026-08-27

Acceptance criteria:

- [x] `POST /api/v1/messages/{id}/move { folderId }` (backend): IMAP MOVE (fallback COPY+DELETE),
      JMAP `Email/set` mailboxIds replace (role actions trash/archive/spam now take the same
      server-side path on JMAP instead of local-only rewrites); cross-account rejected, same
      folder is a noop.
- [x] UI: reading-pane toolbar "Move to…" popover listing the message's account folders with a
      current-folder check; optimistic remove from the current list on success.
- [x] Works from the unified inbox (the menu only offers the message's own account folders) and
      per-folder views.

### W3 — Compose: real editor, quoting, signatures

**Decided:** Plate.js (MIT, shadcn-native, active 2026) as the editor; **HTML is the default
mode**, plaintext a per-message toggle (textarea); alternatives considered and rejected —
Tiptap (more assembly, UI partly paid), BlockNote/Novel (doc-app block chrome), Lexical (framework, not product).

Acceptance criteria:

- [ ] Rich text: bold/italic/underline/strike, lists, links (Ctrl+K), block quotes; toolbar
      collapses to a usable mobile layout.
- [ ] Paste from Word/GDocs/web produces clean HTML — DOMPurify at the editor edge, ammonia still
      guards the server.
- [ ] Inline images, including clipboard-paste screenshots.
- [ ] Reply/forward fetches the original and renders an attributed blockquote; cursor starts above it.
- [ ] Per-account signature (account settings), appended in HTML and plaintext modes.
- [ ] HTML sends as `multipart/alternative` with an auto-generated plaintext alternative;
      plaintext mode sends `text/plain` only.
- [ ] Ctrl/Cmd+Enter sends; OpenGPG sign/encrypt interop unchanged (P/GP/MIME wraps the final MIME).
- [ ] All editor chrome i18n'd (en/zh).

### W4 — Server drafts — shipped 2026-08-27

**Decided:** in v1, server-persisted (Thunderbird parity).

Acceptance criteria:

- [x] Draft CRUD: `POST /api/v1/drafts` (IMAP `APPEND` with \Draft \Seen + delete/expunge of the
      replaced draft + targeted resync located by stamped Message-ID; JMAP `Email/set` `$draft`
      create/destroy with direct local row upsert) and `DELETE /messages/{id}/draft`. Message
      responses now carry `isDraft`.
- [x] Compose autosaves (1.5 s debounce) with a "Draft saved" indicator; reopening a draft from
      the Drafts folder (Edit-draft action in the reader toolbar) restores to/cc/subject/body and
      continues editing — including drafts created outside Lyra (they are ordinary Drafts-folder
      messages). Plain-text bodies in v1; HTML draft bodies ride the same wire (`bodyHtml` field)
      and light up with the W3 editor.
- [x] Sending a draft removes it from Drafts (delete-on-send); Discard deletes it.
- [ ] Deferred: autosave while compose attachments are pending (draft-APPEND of full
      `multipart/mixed` MIME) — autosave pauses while files are attached.

### W5 — Security & deploy polish — shipped 2026-08-27

Acceptance criteria:

- [x] Password policy: 3-of-4 character classes (upper/lower/digit/symbol) or a ≥20-character
      passphrase escape (NIST SP 800-63B style), enforced at signup and password change; server
      message text is the single source of truth and is displayed verbatim by the frontend.
- [x] `deploy/README.md` gains a concrete HTTPS story: full Caddyfile (auto-cert) and
      nginx + certbot examples, including the SSE no-buffering/proxy_read_timeout settings the
      sync event stream needs behind a proxy.
- [x] No new secrets/log surface (gitleaks stays green).

---

## Explicitly deferred (post-v1)

AI assist (BYOK roadmap), multi-user, SSO/GitHub/passkeys, native desktop/mobile, Google/Outlook
calendar APIs, OpenGPG P4 (WKD, Autocrypt, key-expiry UX), editor tables/templates/scheduled send,
drag-and-drop filing (menu-first in W2; drag is polish), spam ML.

## Exit checklist

v1 ships when: original success criteria 1–7 hold, **and** W1–W5 acceptance boxes are all checked,
**and** `make check` is green on the final commit. Each workstream lands as its own change set with
tests at the seams and OpenAPI/i18n/docs updated in the same change, per the engineering standards.
