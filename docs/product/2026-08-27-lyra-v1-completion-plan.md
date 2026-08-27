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

### W2 — Move to folder

Acceptance criteria:

- [ ] `POST /api/v1/messages/{id}/move { folderId }` (backend): IMAP MOVE (fallback COPY+DELETE),
      JMAP mailbox update; sync-safe with cursors.
- [ ] UI: move action in the message list/reading-pane menus with a folder picker; optimistic
      update via the existing RxJS event stream.
- [ ] Works from the unified inbox (resolves the message's account) and per-folder views.

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

### W4 — Server drafts

**Decided:** in v1, server-persisted (Thunderbird parity).

Acceptance criteria:

- [ ] Draft CRUD: create/update lands in the account's Drafts folder — IMAP `APPEND` with
      `\Seen`-tracked UID mapping; JMAP `$draft` handling.
- [ ] Compose autosaves (debounced) with visible saved-state; reopening a draft from the Drafts
      folder restores to/cc/subject/body (HTML included) and continues editing.
- [ ] Sending a draft removes it from Drafts; explicit discard deletes it.
- [ ] Drafts created outside Lyra (webmail/Thunderbird) open for editing in Lyra.

### W5 — Security & deploy polish

Acceptance criteria:

- [ ] Password policy: complexity beyond length (classes or passphrase-friendly rule), surfaced at
      signup/change with i18n messages.
- [ ] `deploy/README.md` gains a concrete HTTPS story (reverse proxy example: Caddy or nginx +
      certbot) so success criterion 1 is unambiguous.
- [ ] No new secrets/log surface (gitleaks stays green).

---

## Explicitly deferred (post-v1)

AI assist (BYOK roadmap), multi-user, SSO/GitHub/passkeys, native desktop/mobile, Google/Outlook
calendar APIs, OpenGPG P4 (WKD, Autocrypt, key-expiry UX), editor tables/templates/scheduled send,
drag-and-drop filing (menu-first in W2; drag is polish), spam ML.

## Exit checklist

v1 ships when: original success criteria 1–7 hold, **and** W1–W5 acceptance boxes are all checked,
**and** `make check` is green on the final commit. Each workstream lands as its own change set with
tests at the seams and OpenAPI/i18n/docs updated in the same change, per the engineering standards.
