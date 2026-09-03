# CalDAV / CardDAV Sync — Design

Date: 2026-09-03
Status: approved (implementation in progress)
Scope: backend PIM sync (`dav.rs`, `pim.rs`, migration 0018) + minimal UI hooks.

## Goal

Replace the v1 "PROPFIND everything + GET each item" PIM sync with an
RFC-aligned DAV stack: automatic discovery, sync-token incremental pull,
batched multiget, delete reconciliation, and two-way writes.

## Standards

| Concern | RFC | Behavior |
|---|---|---|
| Discovery | RFC 6764 | `/.well-known/carddav`/`caldav` redirect/bootstrap; SRV `_carddav._tcp`/`_caldav._tcp`; TXT `path` context |
| Principal | RFC 5397 | PROPFIND `DAV: current-user-principal` |
| Homesets | 6352 §7.1 / 4791 §6.2.1 | `CARDDAV: addressbook-home-set`, `CALDAV: calendar-home-set` |
| Collections | 4918 | PROPFIND depth 1, `resourcetype` filter, displayname |
| Incremental | RFC 6578 | `sync-collection` REPORT + `sync-token`, honor `507` truncation pagination; etag-diff fallback when server lacks the report |
| Batch fetch | 6352 §8.7 / 4791 §7.9 | `addressbook-multiget` / `calendar-multiget` REPORT |
| Writes | 4918 §9 | PUT create `If-None-Match: *`, update `If-Match: <etag>`, DELETE `If-Match` |
| Data | 6350 / 5545 | vCard 3.0/4.0 read; 4.0 write (FN, N, EMAIL, TEL, ORG, PHOTO). VEVENT read + write (UID, DTSTAMP, DTSTART, DTEND, SUMMARY, DESCRIPTION, LOCATION) |
| Security | — | Credentials stay origin-pinned (existing DavClient rule); SSRF guards reuse `netsec` for discovered hosts |

## Storage (migration 0018)

- `contact.etag TEXT` — DAV etag for If-Match writes
- `calendar_event.etag TEXT`
- `calendar.sync_token TEXT` — per-collection RFC 6578 cursor
- `dav_cursor(account_id, kind, token)` — per-account CardDAV cursor
  (`kind = 'carddav'`), PK `(account_id, kind)`

## Sync algorithm

1. Resolve base: stored `carddav_url`/`caldav_url` if set, else RFC 6764
   discovery from the account's mail domain (stored on success).
2. Discover homeset → collections (addressbooks; calendars).
3. Per collection: if we hold a sync-token → `sync-collection` REPORT
   (limit 500; loop while `sync-token` element lacks `valid` attr i.e.
   truncated responses), apply changed items via multiget, tombstone
   removed hrefs. No token yet → full PROPFIND etag listing + multiget
   (chunked ×50), store token returned in the collection PROPFIND.
4. Upsert by `(account_id, external_id)`; keep parsed fields + blob;
   store etag. Deletes mark rows deleted locally (no server call).

## Writes (two-way)

- Contact/event create: build vCard 4.0 / VEVENT, `PUT <new-uuid>.vcf/ics`
  with `If-None-Match: *` at the collection href.
- Update: PUT with `If-Match: <stored etag>`; on 412 → re-sync that item
  then surface conflict (v1: last-write-wins refetch).
- Delete: `DELETE` with `If-Match`, then local tombstone.
- Accounts without DAV configured return a clear actionable error.

## API additions (pim routes)

- `POST /api/v1/accounts/{id}/pim/discover` — run 6764, persist found URLs
- `POST /api/v1/accounts/{id}/contacts` / `PATCH /contacts/{id}` /
  `DELETE /contacts/{id}` — DAV-backed CRUD
- `POST /api/v1/accounts/{id}/events` / `PATCH /events/{id}` /
  `DELETE /events/{id}`
- Existing `sync_contacts` / `sync_calendars` upgraded in place.

## Testing

Mock DAV server (axum loopback, existing pattern): fixture XML for
principal/homeset/sync-collection/multiget responses incl. truncation +
removals; etag write path asserting If-Match headers; discovery against
loopback well-known redirects. Postgres_live roundtrip for the new columns.

## Non-goals (v1)

- CalDAV scheduling (5546 free/busy, invites), VALARM, shared calendars
- vCard photo round-trip on write (read-only PHOTO today)
- Recurrence expansion for editing (RRULEs display as-is; edits touch the
  master event only)
