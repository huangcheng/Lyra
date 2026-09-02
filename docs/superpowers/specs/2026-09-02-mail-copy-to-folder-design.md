# Same-account Copy to Folder — Design

Date: 2026-09-02  
Status: Approved  
Related: `2026-09-02-mail-list-actions-design.md` (Copy to was deferred there)

## Decision

Ship **Copy to…** as an Apple Mail–style twin of **Move to…**, **same account only**. Cross-account copy/move (FETCH + APPEND across logins) is **postponed**.

## Behavior

- Destinations: nested folder tree for the message’s account only (reuse `buildAccountMoveFolderEntries`).
- Current folder disabled; same-folder request → `200` `{ action: "noop" }`.
- Cross-account destination → `400` (same message as move).
- Conversation actions copy **every** member message (parity with Move).
- After success: original stays in place; new copy appears when sync picks it up (no optimistic duplicate row in v1).

## API

`POST /api/v1/messages/{message_id}/copy`  
Body: `{ "folderId": "<uuid>" }`  
Response: `{ "status": "ok", "action": "copied" | "noop", "folderId": "…" }`

## Protocols

| Protocol | Remote effect |
|----------|----------------|
| IMAP | `UID COPY` into dest mailbox; leave source |
| JMAP | `Email/set` mailboxIds = **union** of current mailboxes + dest (not replace) |

## UI

- List context menu: **Copy to…** submenu directly under **Move to…** (filter + tree + account label).
- Reader toolbar: Copy control next to Move.
- i18n: `mail.copyToFolder` (en + zh).

## Out of scope

Cross-account operations, optimistic local duplicate rows, Copy link / Apply Rules / flag colors.
