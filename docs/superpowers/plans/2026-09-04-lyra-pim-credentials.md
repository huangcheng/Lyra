# PIM credentials + calendar read-completeness — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let bearer/JMAP mail accounts sync CardDAV/CalDAV via an encrypted app password in Settings (with Calendar/Contacts deep-links), then finish calendar read behaviors (detail, multi-day, RRULE window, persisted ICS visibility) without a visual redesign.

**Architecture:** Add nullable `mail_account.pim_credential` (same DEK-encrypted JSON as `credential`). A single resolver supplies the DAV Basic password (`pim_credential` → else password-auth mail secret → else `pim_password_required`). Settings owns the write UI; Calendar/Contacts only deep-link. Phase B extends frontend `calendar-grid` helpers + a thin event detail panel; RRULE expansion stays display-only on the client.

**Tech Stack:** Rust/Axum, SeaORM, sqlx dual-DB migrations, React + TanStack Router, existing `api()` client, vitest + `cargo test --bin lyra_backend`.

## Global Constraints

- Spec: `docs/superpowers/specs/2026-09-04-lyra-pim-credentials-design.md` (Approved).
- Never use JMAP bearer token as HTTP Basic for DAV.
- No manual CardDAV/CalDAV URL fields in this plan.
- No event create/edit; no calendar UI polish (density/typography pass is later).
- Dual-DB: every migration has sqlite + postgres up/down.
- OpenAPI + en/zh i18n updated with API/UI strings.
- **Prerequisite numbering:** Working tree already uses `0020_calendar_subscription` for ICS. This plan’s schema is **`0021_pim_credential`**. If ICS is not on the branch yet, land or keep `0020` first so `0021` stays free. Phase A (Tasks 1–5) does not require ICS runtime; Phase B Task 8 (ICS `isActive`) does.

## File map

| File | Responsibility |
|------|----------------|
| `backend/migrations/{sqlite,postgres}/0021_pim_credential.{up,down}.sql` | Add `pim_credential` column |
| `backend/src/entities/mail_account.rs` | Entity column |
| `backend/src/pim_dav.rs` | `DavSecret` resolver + unit tests |
| `backend/src/pim.rs` | Discover/sync use resolver; `PimPasswordRequired` error |
| `backend/src/accounts.rs` | DTO `hasPimCredential`; PATCH `pimPassword` / `clearPimPassword` |
| `docs/openapi/api-v1.yaml` | Account schema + error code |
| `frontend/src/components/settings-page.tsx` | PIM password field + deep-link focus |
| `frontend/src/router.tsx` | Settings `validateSearch` for `account` + `pim` |
| `frontend/src/components/calendar-page.tsx` / `contacts-page.tsx` | Connect CTA → Settings |
| `frontend/src/lib/calendar-grid.ts` (+ tests) | Multi-day span helpers |
| `frontend/src/lib/calendar-rrule.ts` (+ tests) | Visible-window RRULE expansion |
| `frontend/src/i18n/{en,zh}.json` | Copy |

---

### Task 1: Migration + entity `pim_credential`

**Files:**
- Create: `backend/migrations/sqlite/0021_pim_credential.up.sql`
- Create: `backend/migrations/sqlite/0021_pim_credential.down.sql`
- Create: `backend/migrations/postgres/0021_pim_credential.up.sql`
- Create: `backend/migrations/postgres/0021_pim_credential.down.sql`
- Modify: `backend/src/entities/mail_account.rs`

**Interfaces:**
- Produces: `mail_account.pim_credential: Option<String>` (SeaORM `Option<String>`)

- [ ] **Step 1: Write migrations**

SQLite up:
```sql
ALTER TABLE mail_account ADD COLUMN pim_credential TEXT;
```

SQLite down:
```sql
-- SQLite cannot DROP COLUMN portably in older versions used here:
-- recreate is unnecessary; leave a no-op comment OR follow prior Lyra down style.
-- Prefer matching repo pattern for additive columns (check 0018/0019 downs).
```

Postgres up:
```sql
ALTER TABLE mail_account ADD COLUMN IF NOT EXISTS pim_credential TEXT;
```

Postgres down:
```sql
ALTER TABLE mail_account DROP COLUMN IF EXISTS pim_credential;
```

Match exact down-style of the nearest prior additive-column migration in this repo (copy that file’s pattern rather than inventing).

- [ ] **Step 2: Add entity field**

In `Model` after `smtp_credential`:
```rust
    /// Optional DEK-encrypted PIM / app password for CardDAV/CalDAV.
    pub pim_credential: Option<String>,
```

- [ ] **Step 3: Compile check**

Run: `cd backend && cargo check --bin lyra_backend`  
Expected: success (or only pre-existing unrelated errors).

- [ ] **Step 4: Commit**

```bash
git add backend/migrations/sqlite/0021_pim_credential.* backend/migrations/postgres/0021_pim_credential.* backend/src/entities/mail_account.rs
git commit -m "$(cat <<'EOF'
feat(db): add mail_account.pim_credential for DAV app passwords

EOF
)"
```

---

### Task 2: DAV secret resolver (TDD)

**Files:**
- Modify: `backend/src/pim_dav.rs`
- Test: unit tests in `pim_dav.rs` `#[cfg(test)]`

**Interfaces:**
- Consumes: decrypted blobs via callers (resolver itself is pure over already-loaded options)
- Produces:
```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DavAuthError {
    PimPasswordRequired,
}

/// Resolve HTTP Basic password for CardDAV/CalDAV.
/// `pim_password`: decrypted Option from `pim_credential` column (None if column null).
/// `mail_password`: decrypted mail `credential` when usable as a password.
/// `auth_type`: account `auth_type` (`password` | `bearer` | …).
pub fn resolve_dav_password(
    pim_password: Option<&str>,
    mail_password: Option<&str>,
    auth_type: &str,
) -> Result<String, DavAuthError>
```

Rules:
1. If `pim_password` is `Some(s)` and `!s.is_empty()` → `Ok(s.to_string())`
2. Else if `auth_type` equals ignore-ascii `password` (or empty/unset treated as password) and `mail_password` is non-empty → `Ok(mail_password)`
3. Else → `Err(DavAuthError::PimPasswordRequired)`

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn dav_password_prefers_pim() {
    assert_eq!(
        resolve_dav_password(Some("app-pass"), Some("mail-pass"), "bearer").unwrap(),
        "app-pass"
    );
}

#[test]
fn dav_password_falls_back_for_password_auth() {
    assert_eq!(
        resolve_dav_password(None, Some("mail-pass"), "password").unwrap(),
        "mail-pass"
    );
}

#[test]
fn dav_password_requires_pim_for_bearer() {
    assert!(matches!(
        resolve_dav_password(None, Some("bearer-token"), "bearer"),
        Err(DavAuthError::PimPasswordRequired)
    ));
}
```

- [ ] **Step 2: Run tests — expect FAIL**

Run: `cd backend && cargo test --bin lyra_backend pim_dav::tests::dav_password -- --nocapture`  
Expected: compile fail or test fail (`resolve_dav_password` missing).

- [ ] **Step 3: Implement `resolve_dav_password` + `DavAuthError`**

- [ ] **Step 4: Run tests — expect PASS**

- [ ] **Step 5: Commit**

```bash
git add backend/src/pim_dav.rs
git commit -m "$(cat <<'EOF'
feat(pim): resolve DAV Basic password without using bearer tokens

EOF
)"
```

---

### Task 3: Wire discover/sync to resolver + error code

**Files:**
- Modify: `backend/src/pim.rs` (sync_contacts, sync_calendars, pim_discover)
- Possibly small helper in `pim_dav.rs` or `pim.rs`:
```rust
async fn load_dav_basic_password(
    db: &DbPool,
    user_id: &str,
    account_id: &str,
) -> Result<String, PimError>
```
  Loads DEK, selects `credential`, `pim_credential`, `auth_type`, decrypts, calls `resolve_dav_password`.

**Interfaces:**
- Consumes: `resolve_dav_password`
- Produces: `PimError::PimPasswordRequired` mapped to HTTP 400, `code: "pim_password_required"`, message suitable for UI CTA

- [ ] **Step 1: Extend `PimError`**

```rust
#[error("PIM app password required for CardDAV/CalDAV")]
PimPasswordRequired,
```

In `IntoResponse`, map to `(BAD_REQUEST, "…", Some("pim_password_required"))`.

- [ ] **Step 2: Implement `load_dav_basic_password`**

Do **not** use `get_user_dek_and_credential` alone (mail blob only). Query:
`credential`, `pim_credential`, `auth_type`, `email_address` for the account owned by `user_id`.  
Decrypt `pim_credential` if `Some` with same `decrypt_account_password` / crypto path as mail.  
For `auth_type == "bearer"`, pass `mail_password: None` into the resolver (even if blob decrypts — bearer must not be used as Basic). For password auth, pass decrypted mail secret.

- [ ] **Step 3: Replace password loads in `pim_discover`, `sync_contacts`, `sync_calendars`**

Use `load_dav_basic_password`; on `PimPasswordRequired` return that error (not a generic sync error).

- [ ] **Step 4: Unit/compile**

Run: `cd backend && cargo test --bin lyra_backend pim_dav::tests::dav_password -- --nocapture`  
Run: `cd backend && cargo clippy --bin lyra_backend -- -D warnings`  
Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add backend/src/pim.rs backend/src/pim_dav.rs
git commit -m "$(cat <<'EOF'
feat(pim): use pim_credential for CardDAV/CalDAV discover and sync

EOF
)"
```

---

### Task 4: Account API — `hasPimCredential`, PATCH set/clear

**Files:**
- Modify: `backend/src/accounts.rs`
- Modify: `docs/openapi/api-v1.yaml` (`Account` schema + update request)
- Modify: `docs/specs/2026-08-20-lyra-data-model-spec.md` (one row for `pim_credential`) if that table documents columns — keep short.

**Interfaces:**
- Produces on `Account`: `has_pim_credential: bool` → JSON `hasPimCredential`
- `UpdateAccountRequest`: `pim_password: Option<String>`, `clear_pim_password: Option<bool>`
- Encrypt with `crypto::encrypt(&dek, password.as_bytes())` then `serde_json::to_string` — same as mail `password` update

- [ ] **Step 1: Extend structs**

```rust
// Account
pub has_pim_credential: bool,

// UpdateAccountRequest
pub pim_password: Option<String>,
pub clear_pim_password: Option<bool>,
```

When mapping rows → `Account`, set `has_pim_credential` from `pim_credential.is_some() && !blank`.

- [ ] **Step 2: Update handler**

Include `pim_password` / `clear_pim_password` in `has_update`.  
If `clear_pim_password == Some(true)` → set column NULL.  
Else if `pim_password` is `Some(p)` and non-empty → encrypt and set.  
Reject empty string `pim_password` with `InvalidInput`.  
Omit both → leave column unchanged.

- [ ] **Step 3: OpenAPI**

Add `hasPimCredential: boolean` to Account; `pimPassword`, `clearPimPassword` on update request body.

- [ ] **Step 4: Test**

Prefer a focused unit test if account update is hard to hit; otherwise `cargo test --bin lyra_backend` smoke + manual docker later. If `postgres_live` helpers seed accounts, add an ignored roundtrip for the column when cheap.

- [ ] **Step 5: Commit**

```bash
git add backend/src/accounts.rs docs/openapi/api-v1.yaml docs/specs/2026-08-20-lyra-data-model-spec.md
git commit -m "$(cat <<'EOF'
feat(api): set and clear encrypted PIM app password on mail accounts

EOF
)"
```

---

### Task 5: Settings UI + deep-link search params

**Files:**
- Modify: `frontend/src/router.tsx` — settings `validateSearch`
- Modify: `frontend/src/components/settings-page.tsx`
- Modify: `frontend/src/i18n/en.json`, `frontend/src/i18n/zh.json`
- Modify: `frontend/src/components/calendar-page.tsx`, `frontend/src/components/contacts-page.tsx` (CTA only)

**Interfaces:**
- Search: `{ account?: string; pim?: boolean }` on `/settings`
- PATCH body: `{ pimPassword?: string; clearPimPassword?: true }`
- Navigate: `router.navigate({ to: '/settings', search: { account: id, pim: true } })`

- [ ] **Step 1: Router search**

```tsx
const settingsRoute = createRoute({
  // ...
  path: '/settings',
  validateSearch: (s: Record<string, unknown>) => ({
    account: typeof s.account === 'string' ? s.account : undefined,
    pim: s.pim === true || s.pim === '1' || s.pim === 'true',
  }),
  // ...
});
```

Use `settingsRoute.useSearch()` inside `SettingsPage` (or `useSearch({ from: '/settings' })`).

- [ ] **Step 2: Settings field**

Per account card near existing PIM menu:
- Label: `settings.pim.password` (“PIM / app password”)
- Help: `settings.pim.passwordHelp` (Fastmail app password; CardDAV+CalDAV)
- If `account.hasPimCredential`, show muted “Configured” + optional Clear button (`clearPimPassword: true`)
- Input + Save calls existing account update PATCH
- When `search.pim && search.account === account.id`, scroll into view / expand card and focus the input (`useEffect` + `ref`)

- [ ] **Step 3: Calendar/Contacts CTA**

When CalDAV/CardDAV sources empty (or sync error `code === 'pim_password_required'`):
- Button `calendar.connectDav` / `contacts.connectDav` → navigate to settings with first mail account id (from existing accounts list API already used on page) and `pim: true`. If multiple accounts, pick the one user would sync (or simple select — first active is OK for v1).

- [ ] **Step 4: i18n en+zh**

Keys: `settings.pim.password`, `settings.pim.passwordHelp`, `settings.pim.configured`, `settings.pim.clear`, `settings.pim.saved`, `calendar.connectDav`, `contacts.connectDav`.

- [ ] **Step 5: Frontend check**

Run: `cd frontend && npx tsc --noEmit`  
Expected: exit 0.

- [ ] **Step 6: Commit**

```bash
git add frontend/src/router.tsx frontend/src/components/settings-page.tsx frontend/src/components/calendar-page.tsx frontend/src/components/contacts-page.tsx frontend/src/i18n/en.json frontend/src/i18n/zh.json
git commit -m "$(cat <<'EOF'
feat(frontend): PIM app password in Settings with Calendar/Contacts deep-link

EOF
)"
```

---

### Task 6: Multi-day span helpers (TDD)

**Files:**
- Modify: `frontend/src/lib/calendar-grid.ts`
- Modify: `frontend/src/lib/calendar-grid.test.ts`

**Interfaces:**
```ts
/** Inclusive local calendar dates the event occupies (all-day end exclusive per iCal → last day = dtend-1 day). */
export function eventSpanDays(event: EventTimeFields): Date[]

export function eventOverlapsLocalDay(event: EventTimeFields, day: Date): boolean
```

All-day: treat `dtend` as exclusive (RFC 5545). Timed: overlap if interval intersects `[day, day+1)`.

- [ ] **Step 1: Failing tests** for a 3-day all-day holiday and a timed event crossing midnight

- [ ] **Step 2: Run `cd frontend && npm test -- src/lib/calendar-grid.test.ts` — FAIL**

- [ ] **Step 3: Implement helpers**

- [ ] **Step 4: Tests PASS**

- [ ] **Step 5: Wire month cells** in `calendar-page.tsx` to list events with `eventOverlapsLocalDay` (replace naive same-day-only filter if present)

- [ ] **Step 6: Commit**

```bash
git add frontend/src/lib/calendar-grid.ts frontend/src/lib/calendar-grid.test.ts frontend/src/components/calendar-page.tsx
git commit -m "$(cat <<'EOF'
feat(calendar): show multi-day events across month/week day cells

EOF
)"
```

---

### Task 7: RRULE expansion for visible window (TDD)

**Files:**
- Create: `frontend/src/lib/calendar-rrule.ts`
- Create: `frontend/src/lib/calendar-rrule.test.ts`
- Modify: `frontend/src/components/calendar-page.tsx`

**Interfaces:**
```ts
export type ExpandableEvent = EventTimeFields & {
  id: string
  recurrenceRule?: string | null
  summary?: string | null
  // …other display fields passed through
}

/** Expand FREQ=DAILY|WEEKLY|MONTHLY|YEARLY with UNTIL/COUNT/INTERVAL/BYDAY (best-effort). Unsupported → [master] only. */
export function expandEventsForRange(
  events: ExpandableEvent[],
  rangeStart: Date,
  rangeEnd: Date,
): ExpandableEvent[]
```

Occurrence ids: `${id}::${yyyy-mm-dd}` for expanded clones so React keys stay unique.

- [ ] **Step 1: Failing tests** — weekly event with 4 hits in September; unsupported `RRULE:FREQ=MONTHLY;BYMONTHDAY=32` returns master only

- [ ] **Step 2: Implement minimal parser** (no new npm dependency unless an existing small one is already in package.json — prefer hand-rolled for DAILY/WEEKLY/YEARLY)

- [ ] **Step 3: Tests PASS**

- [ ] **Step 4: In calendar page**, after loading events, `expandEventsForRange` over visible month/week/day bounds (±1 day padding ok)

- [ ] **Step 5: Commit**

```bash
git add frontend/src/lib/calendar-rrule.ts frontend/src/lib/calendar-rrule.test.ts frontend/src/components/calendar-page.tsx
git commit -m "$(cat <<'EOF'
feat(calendar): expand simple RRULEs for the visible date range

EOF
)"
```

---

### Task 8: Event detail + persist ICS `isActive`

**Files:**
- Modify: `frontend/src/components/calendar-page.tsx`
- Modify: `frontend/src/i18n/en.json`, `zh.json`

**Interfaces:**
- Click event → set `selectedEvent`; render read-only Dialog/Sheet (existing shadcn) with summary, when, location, description, source name
- ICS checkbox: on toggle call `PATCH /calendar-subscriptions/{id}` with `{ isActive: boolean }` then update local state (in addition to `visibleIds`)

- [ ] **Step 1: Detail panel** (functional, no visual redesign — reuse Dialog)

- [ ] **Step 2: Persist ICS visibility** via PATCH; keep local `visibleIds` in sync with `isActive` from API on load (`visibleIds` init from `isActive !== false`)

- [ ] **Step 3: i18n** for detail labels (`calendar.detail.*`)

- [ ] **Step 4: `npx tsc --noEmit` + `npm test -- src/lib/calendar`

- [ ] **Step 5: Commit**

```bash
git add frontend/src/components/calendar-page.tsx frontend/src/i18n/en.json frontend/src/i18n/zh.json
git commit -m "$(cat <<'EOF'
feat(calendar): read-only event detail and persisted ICS visibility

EOF
)"
```

---

### Task 9: Docker smoke + docs touch-up

**Files:**
- Modify: `docs/specs/2026-09-03-caldav-carddav-spec.md` — note PIM password resolution
- Modify: `docs/superpowers/specs/2026-09-04-lyra-pim-credentials-design.md` — status Implemented when done

- [ ] **Step 1: Rebuild**

`docker compose up --build -d lyra`  
Confirm migrations apply (`0021`).

- [ ] **Step 2: Manual smoke**

1. Fastmail (or bearer) account → Settings → set app password → Discover → Sync calendars/contacts.  
2. Calendar empty CTA → lands Settings with PIM focused.  
3. ICS holiday feed still shows; click event → detail; toggle ICS off/on survives refresh.

- [ ] **Step 3: Commit docs**

```bash
git add docs/specs/2026-09-03-caldav-carddav-spec.md docs/superpowers/specs/2026-09-04-lyra-pim-credentials-design.md
git commit -m "$(cat <<'EOF'
docs: mark PIM credentials design implemented

EOF
)"
```

---

## Spec coverage (self-review)

| Spec item | Task |
|-----------|------|
| `pim_credential` column + encrypt | 1, 4 |
| Resolver never uses bearer as Basic | 2, 3 |
| Discover/sync wired | 3 |
| Settings field + clear | 5 |
| Deep-link Calendar/Contacts | 5 |
| Password-only / no URL fields | (global — no task adds URLs) |
| Event detail | 8 |
| Multi-day | 6 |
| RRULE window | 7 |
| ICS `isActive` persist | 8 |
| OpenAPI / i18n / tests | 4, 5, 6, 7, 9 |
| UI polish | Explicitly omitted |

## Placeholder scan

No TBD steps; migration down must follow an existing Lyra additive-column down file at implement time.

## Type consistency

- Error code string: `pim_password_required` (API + frontend).
- PATCH fields: `pimPassword`, `clearPimPassword`, `hasPimCredential`.
- Search: `account`, `pim`.
