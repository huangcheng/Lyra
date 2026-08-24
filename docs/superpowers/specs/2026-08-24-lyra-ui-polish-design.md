# Lyra UI Polish & Identity — Design

Date: 2026-08-24
Status: approved (design)
Scope: frontend only (`frontend/`). Backend and uncommitted IMAP/sync work are untouched.

## Background

A browser walkthrough of the running app (Docker, `http://127.0.0.1:3000`) plus code review found:

1. **Calendar page is unstyled** — `calendar-page.tsx` uses custom classes (`calendar-grid`, `calendar-day`, `event-chip`, …) that exist in no stylesheet, so the month grid renders as a vertical text list. Fetch errors are also swallowed (`const [, setError] = useState(...)`), so failures are invisible.
2. **Contacts page** renders the "Contacts" heading twice (`SecondaryPage` header + page body `h1`).
3. **Settings sync feedback** uses one global `syncing` boolean (`settings-page.tsx`): clicking "Sync now" on one account marks every account as syncing, and sync failures surface nowhere.
4. Empty states are bare gray text; login page is the stock shadcn card with no branding; no dark mode toggle exists although components carry `dark:` variants.
5. zh locale: Trash is "垃圾箱", which collides with Spam "垃圾邮件".

## Approach

Systematic shadcn polish plus a light custom identity (option B from review). Everything stays within the locked stack (React, TanStack Router, Tailwind, shadcn/ui); no layout paradigm changes.

## 1. Visual identity

- **Accent**: deep celestial indigo/violet as `--primary` (light + dark tuned), replacing stock near-black for primary buttons, active nav, focus rings. Neutral grays stay as-is.
- **Wordmark**: "Lyra" text with a small four-point star SVG. Shown on the login card and at the top of the mail sidebar. Inline SVG component, no image assets.
- **Typography**: Inter Variable, self-hosted via `@fontsource-variable/inter` (new frontend dependency), applied to the base font stack with system fallbacks so Chinese renders natively. No CDN fonts.

## 2. Bug fixes

### Calendar page (`calendar-page.tsx`)

Rebuild in Tailwind/shadcn:

- `grid grid-cols-7` month grid; today highlighted with accent; out-of-month days muted.
- Sidebar calendar list with color-dot markers and selected state.
- Toolbar: prev / month-year title / next / Today, using shadcn `Button` variants.
- Event chips (calendar color, truncated, max 3 + "+N more") and a styled event detail panel with close button.
- Loading skeleton, error banner (fix the discarded error state), empty state when no calendars.
- Day-of-week headers localized via existing `calendar.days.*` keys.

### Contacts page (`contacts-page.tsx`)

- Remove duplicate `h1` (keep `SecondaryPage` title only).
- Two-pane layout polish: list rows with avatar initial circles (accent-tinted), detail pane with proper spacing.
- Designed empty states for "no contacts" and "no selection".

### Settings sync feedback (`settings-page.tsx`)

- Replace the single `syncing` boolean with per-account state (`Record<string, boolean>` or a `syncingId`), so "Sync now" only spins on the account clicked.
- Surface failures: toast on sync error plus an inline "last sync failed" line on the account card (from `/sync/status` or the sync endpoint's error response — whatever the API already returns; no backend changes).
- Keep "Last synced" timestamps.

## 3. Empty states

Shared `EmptyState` component (icon + headline + one-line hint) used by:

- Mail list (inbox zero → "All caught up" style message; if a sync error is known, hint to check Settings)
- Message pane ("Select a message to read")
- Contacts list / detail
- Calendar (no calendars)
- Search with no results

All strings in `en.json` + `zh.json`.

## 4. Dark mode

- Light / Dark / System selector in Settings (Session section) plus a compact toggle in the mail sidebar footer.
- Persisted in `localStorage`, applied by toggling the `dark` class on `<html>`; default follows `prefers-color-scheme`.
- Audit the mail view, compose dialog, contacts, calendar, settings in dark mode and fix any surface missing `dark:` coverage.

## 5. Login page

- Lyra wordmark above the card, accent-colored Log In button, friendlier username placeholder.
- Keep the EN / 中文 toggle. Card structure stays shadcn.

## 6. Copy fixes

- zh: Trash "垃圾箱" → "已删除" (Spam stays "垃圾邮件").
- Add all new strings to both locale files; no hardcoded UI strings.

## Verification

- `make fmt` and `cd frontend && npm run check` (oxlint + tsc) clean.
- Browser walkthrough with screenshots: login, mail (empty + populated if available), compose, contacts, calendar, settings — each in light+dark × en+zh.
- Confirm per-account "Sync now" behavior and sync error visibility against the running Docker backend.

## Non-goals

- No backend changes; no new API endpoints.
- No compose-dialog feature additions (Cc/Bcc/attachments stay out for v1).
- No changes to the mail three-pane layout paradigm or routing.
