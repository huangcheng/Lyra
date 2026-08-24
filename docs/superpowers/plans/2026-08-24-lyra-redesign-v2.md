# Lyra UI Redesign v2 — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the stock shadcn mail UI with the approved "Redesign v2" (stamp brand, cool-gray tokens, folder-tree mail sidebar, standalone Dashboard and Settings pages), light + dark, en + zh.

**Architecture:** All visual change flows from CSS tokens in `frontend/src/index.css` plus a small set of new components (stamp logo, slim page nav, folder-tree sidebar, dashboard). The backend gains exactly one additive read-only endpoint (`GET /api/v1/messages/stats`) aggregating the local messages table for the dashboard. Locked stack unchanged: React 19, TanStack Router, Tailwind 4, shadcn/ui, Zustand, Rust + Axum + sqlx (SQLite + PostgreSQL).

**Spec:** `docs/superpowers/specs/2026-08-24-lyra-redesign-v2-design.md` (token tables, screen inventories, navigation model). Visual reference: Ardot file 716978471157674, page `7:1`.

**Tech Stack:** React 19, Tailwind 4, shadcn/ui, Zustand, TanStack Router; Rust + Axum + sqlx.

**Testing note:** the frontend has no unit-test runner — verification there is `npm run check` (tsc + oxlint + prettier) plus browser walkthrough. The backend has cargo tests (`cargo test --bin lyra_backend`); the new endpoint gets real tests.

---

### Task 1: Design tokens + fonts

**Files:**
- Modify: `frontend/src/index.css` (token blocks `:root` and `.dark`)
- Modify: `frontend/src/main.tsx` (font imports)
- Modify: `frontend/package.json` (via npm install)

- [ ] **Step 1: Install fonts**

```bash
cd frontend && npm install @fontsource-variable/inter @fontsource-variable/inter-tight @fontsource/instrument-serif && npm uninstall @fontsource-variable/geist
```

- [ ] **Step 2: Replace font import in `main.tsx`**

Remove `import '@fontsource-variable/geist';`, add:

```ts
import '@fontsource-variable/inter';
import '@fontsource-variable/inter-tight';
import '@fontsource/instrument-serif';
```

- [ ] **Step 3: Rewrite the token palettes in `index.css`**

Keep the Tailwind 4 structure (`@import`, `@custom-variant dark`, `@theme inline` mapping) exactly as-is; only change the color values. In `:root`:

```css
:root {
  --radius: 0.5rem;
  --background: #ffffff;
  --foreground: #1a1b1f;
  --card: #ffffff;
  --card-foreground: #1a1b1f;
  --popover: #ffffff;
  --popover-foreground: #1a1b1f;
  --primary: #1a1b1f;            /* ink — used for text emphasis, not paint */
  --primary-foreground: #ffffff;
  --secondary: #f6f6f8;          /* LIST */
  --secondary-foreground: #1a1b1f;
  --muted: #eff0f2;              /* PANEL */
  --muted-foreground: #6b6f76;   /* SEC */
  --accent: #e8e8e9;             /* HOV */
  --accent-foreground: #1a1b1f;
  --destructive: #b4453c;        /* muted red, text/borders only */
  --destructive-foreground: #ffffff;
  --border: #e2e2e5;             /* HAIR */
  --input: #e1e2e4;              /* BTNB */
  --ring: #9b9ba3;               /* TER */
  --sidebar: #eff0f2;
  --sidebar-foreground: #1a1b1f;
  --sidebar-primary: #1a1b1f;
  --sidebar-primary-foreground: #ffffff;
  --sidebar-accent: #e8e8e9;
  --sidebar-accent-foreground: #1a1b1f;
  --sidebar-border: #e2e2e5;
  --sidebar-ring: #9b9ba3;
  /* Lyra v2 extras */
  --unread: #e2a336;             /* amber — unread dots, today bar */
  --ok: #3d9a5f;                 /* green — sync dot, toggles on */
  --ter-foreground: #9b9ba3;     /* TER */
}
```

In `.dark`:

```css
.dark {
  --background: #17181c;         /* LIST dark = app canvas */
  --foreground: #ecedef;
  --card: #24262b;               /* READER dark */
  --card-foreground: #ecedef;
  --popover: #24262b;
  --popover-foreground: #ecedef;
  --primary: #ecedef;
  --primary-foreground: #1a1b1f;
  --secondary: #1a1c20;          /* PANEL dark */
  --secondary-foreground: #ecedef;
  --muted: #1a1c20;
  --muted-foreground: #9ba0a8;
  --accent: #26282e;             /* HOV dark */
  --accent-foreground: #ecedef;
  --destructive: #d4756b;
  --destructive-foreground: #1a1b1f;
  --border: #2e3138;
  --input: #35383f;
  --ring: #6e737b;
  --sidebar: #1a1c20;
  --sidebar-foreground: #ecedef;
  --sidebar-primary: #ecedef;
  --sidebar-primary-foreground: #1a1b1f;
  --sidebar-accent: #26282e;
  --sidebar-accent-foreground: #ecedef;
  --sidebar-border: #2e3138;
  --sidebar-ring: #6e737b;
  --unread: #e2a336;
  --ok: #3d9a5f;
  --ter-foreground: #6e737b;
}
```

In the `@theme inline` block, add mappings alongside the existing ones:

```css
--color-unread: var(--unread);
--color-ok: var(--ok);
--color-ter-foreground: var(--ter-foreground);
--font-sans: 'Inter Variable', ui-sans-serif, system-ui, sans-serif;
--font-display: 'Inter Tight Variable', 'Inter Variable', ui-sans-serif, system-ui, sans-serif;
--font-brand: 'Instrument Serif', ui-serif, Georgia, serif;
```

(Keep whatever other `--font-*` lines exist; these three become the canonical families. Tailwind classes then available: `font-sans`, `font-display`, `font-brand`, `text-unread`, `bg-ok`, `text-ter-foreground`, etc.)

- [ ] **Step 4: Remove hardcoded indigo leaks**

- `frontend/src/components/calendar-page.tsx:175,233`: replace the `#4f46e5` fallback color with `var(--unread)` (calendar color fallback is now amber).
- `frontend/src/components/mail/mail.tsx`: remove any `text-zinc-*` / zinc overrides on TabsTriggers; tabs get restyled in Task 6 anyway.

- [ ] **Step 5: Check + commit**

```bash
cd frontend && npm run check
git add frontend/src/index.css frontend/src/main.tsx frontend/package.json frontend/package-lock.json frontend/src/components/calendar-page.tsx frontend/src/components/mail/mail.tsx
git commit -m "feat(ui): redesign v2 design tokens and brand fonts"
```

---

### Task 2: Stamp logo component

**Files:**
- Create: `frontend/src/components/stamp-logo.tsx`
- Modify: `frontend/src/components/lyra-wordmark.tsx` (rewrite to use StampLogo)
- Test: visual (login + sidebar footer)

- [ ] **Step 1: Create `stamp-logo.tsx`**

```tsx
import { cn } from '@/lib/utils';

export function StampLogo({ size = 20, className }: { size?: number; className?: string }) {
  return (
    <span
      className={cn('inline-flex items-center justify-center bg-primary font-brand text-primary-foreground', className)}
      style={{ width: size, height: size, borderRadius: Math.round(size * 0.22), fontSize: size * 0.6, lineHeight: 1 }}
      aria-hidden
    >
      L
    </span>
  );
}
```

- [ ] **Step 2: Rewrite `lyra-wordmark.tsx`**

```tsx
import { StampLogo } from '@/components/stamp-logo';
import { cn } from '@/lib/utils';

export function LyraWordmark({ className }: { className?: string }) {
  return (
    <span className={cn('inline-flex items-center gap-2', className)}>
      <StampLogo size={20} />
      <span className="font-brand text-[15px] leading-none text-foreground">Lyra</span>
    </span>
  );
}
```

This replaces the star wordmark everywhere it is already used (mail sidebar footer, login page) without touching call sites.

- [ ] **Step 3: Check + commit**

```bash
cd frontend && npm run check
git add frontend/src/components/stamp-logo.tsx frontend/src/components/lyra-wordmark.tsx
git commit -m "feat(ui): stamp logo component, serif wordmark"
```

---

### Task 3: Login page

**Files:**
- Modify: `frontend/src/components/login-form.tsx`
- Modify: `frontend/src/components/auth-page.tsx` (page backdrop only)
- Modify: `frontend/src/i18n/en.json`, `frontend/src/i18n/zh.json`

- [ ] **Step 1: Restyle `login-form.tsx`**

Keep all machine props and TOTP/bootstrap branches exactly as they are; only the presentation changes:

- Card: `w-[380px] rounded-xl border border-border bg-card px-9 pb-8 pt-10` (no shadow).
- Brand block at top, centered: `<StampLogo size={40} className="rounded-[9px]" />` + `<span className="font-brand text-[28px]">Lyra</span>` side by side (`flex items-center justify-center gap-3`), then tagline below: `t(locale, 'auth.tagline')`, `text-[13px] text-ter-foreground`, `pb-7`.
- Inputs: default shadcn `Input` is fine once tokens land (border-input, radius). Placeholders stay `t(locale, 'auth.username')` / `auth.password`.
- Submit `Button`: `variant="outline"`, `className="h-[42px] w-full rounded-lg border-foreground font-medium"` — the white pill with ink border.
- Footer row inside card (`flex items-center gap-1.5 pt-5 text-xs`): EN / 中文 buttons keep existing behavior (`text-xs`, active locale `font-medium text-foreground`, inactive `text-ter-foreground`), spacer, then `t(locale, 'auth.selfHosted', { version })` right-aligned `text-[11px] text-ter-foreground` (version from the existing `/version` fetch if already wired; otherwise static copy without version).

- [ ] **Step 2: Backdrop in `auth-page.tsx`**

Page container: `bg-[#f7f7f8] dark:bg-[#101114]` (login canvas tokens), centered card as today.

- [ ] **Step 3: Add i18n keys**

`en.json` → `"auth.tagline": "Mail you host yourself."`, `"auth.selfHosted": "self-hosted · v{{version}}"`.
`zh.json` → `"auth.tagline": "自己托管的邮件。"`, `"auth.selfHosted": "自托管 · v{{version}}"`.

- [ ] **Step 4: Check + commit**

```bash
cd frontend && npm run check
git add frontend/src/components/login-form.tsx frontend/src/components/auth-page.tsx frontend/src/i18n/
git commit -m "feat(ui): login page v2 — stamp brand, canvas backdrop"
```

---

### Task 4: Slim page nav (shared shell for Dashboard + Settings)

**Files:**
- Create: `frontend/src/components/slim-page-nav.tsx`
- Test: visual

- [ ] **Step 1: Create `slim-page-nav.tsx`**

```tsx
import { Link } from '@tanstack/react-router';
import { ArrowLeft, type LucideIcon } from 'lucide-react';
import { StampLogo } from '@/components/stamp-logo';
import { cn } from '@/lib/utils';
import { t } from '@/i18n';
import { useUIStore } from '@/stores/ui';

export type SlimNavItem = { key: string; label: string; icon: LucideIcon; active?: boolean; onClick?: () => void };

export function SlimPageNav({ section, items }: { section: string; items: SlimNavItem[] }) {
  const locale = useUIStore((s) => s.locale);
  return (
    <aside className="flex w-[220px] shrink-0 flex-col gap-px bg-secondary px-2 py-3">
      <div className="flex items-center gap-2.5 px-2.5 pb-3 pt-1">
        <StampLogo size={28} />
        <span className="font-brand text-lg text-foreground">Lyra</span>
      </div>
      <Link to="/" className="mb-1 flex items-center gap-2 rounded-[7px] px-2.5 py-1.5 text-[13px] text-muted-foreground hover:bg-accent">
        <ArrowLeft size={16} /> {t(locale, 'nav.mail')}
      </Link>
      <div className="px-2.5 pb-1 pt-0.5 text-[10.5px] font-semibold tracking-[0.8px] text-ter-foreground">{section}</div>
      {items.map((item) => (
        <button
          key={item.key}
          onClick={item.onClick}
          className={cn(
            'flex items-center gap-2 rounded-[7px] px-2.5 py-1.5 text-left text-[13px]',
            item.active ? 'bg-accent font-medium text-foreground' : 'text-foreground hover:bg-accent',
          )}
        >
          <item.icon size={16} className={item.active ? 'text-foreground' : 'text-ter-foreground'} />
          {item.label}
        </button>
      ))}
    </aside>
  );
}
```

- [ ] **Step 2: Add i18n key** — `en.json`: `"nav.mail": "Mail"` (exists already; reuse), `zh.json` same key exists ("邮件"). No new keys needed.

- [ ] **Step 3: Check + commit**

```bash
cd frontend && npm run check
git add frontend/src/components/slim-page-nav.tsx
git commit -m "feat(ui): shared slim page nav shell"
```

---

### Task 5: Backend stats endpoint (dashboard data)

**Files:**
- Modify: `backend/src/sync/http.rs` (add route + handler) — or a new `backend/src/stats.rs` module wired in `main.rs`, whichever matches existing module layout; prefer new `stats.rs` for a clean seam.
- Test: `backend/src/stats.rs` `#[cfg(test)]` module (binary crate tests)

**Endpoint:** `GET /api/v1/messages/stats?days=14` (default 14, clamp 1..=90), bearer-authed like other routes. Response:

```json
{
  "days": 14,
  "daily": [{ "date": "2026-08-11", "received": 12 }],
  "topSenders": [{ "address": "notifications@github.com", "name": "GitHub", "count": 41 }],
  "totals": { "received": 190, "sent": 58, "unread": 12 }
}
```

- [ ] **Step 1: Write the failing test**

In `backend/src/stats.rs` (or wherever the handler lands), follow the existing test pattern in the codebase (`cargo test --bin lyra_backend`, in-memory SQLite). Seed a messages table with known rows across 3 days and 2 senders, call the query function, assert aggregate shape. First inspect the schema:

```bash
ls backend/migrations/sqlite && grep -n "CREATE TABLE" backend/migrations/sqlite/*.sql | grep -i message
```

Use the real column names found there (date column, from-address columns, direction/role marker for "sent" — likely a folder role or `is_sent`/folder join; derive from what `GET /messages?role=sent` already filters on in `sync/http.rs`).

- [ ] **Step 2: Run test, expect fail** — `cd backend && cargo test --bin lyra_backend stats` → compile error / not found.

- [ ] **Step 3: Implement the query (dual-DB)**

Follow the existing dual-database pattern in `backend/src/storage.rs` (the storage seam abstracts SQLite + PostgreSQL). Aggregation SQL, both dialects:

```sql
-- SQLite
SELECT date(date_column) AS d, COUNT(*) AS c FROM messages
WHERE date_column >= datetime('now', ?1) GROUP BY d ORDER BY d;
-- PostgreSQL
SELECT date(date_column) AS d, COUNT(*) AS c FROM messages
WHERE date_column >= now() - $1::interval GROUP BY d ORDER BY d;
```

Pass days as a bound interval string (`-14 days` SQLite / `14 days` Postgres). Top senders: `GROUP BY from_address ORDER BY c DESC LIMIT 5`. Totals: unread = messages in inbox-role folders with `read_at IS NULL` (or whatever the read flag column is — mirror the filter `GET /messages?role=&accountId=` uses for unread), sent = count in sent-role folders, received = count over the window.

- [ ] **Step 4: Run test, expect pass** — `cargo test --bin lyra_backend stats` green, then full `cargo test --bin lyra_backend` and `cargo clippy --all-targets --all-features -- -D warnings`.

- [ ] **Step 5: Wire route + commit**

Mount `GET /api/v1/messages/stats` behind the same auth middleware as `/messages`. Then:

```bash
git add backend/src/
git commit -m "feat(api): read-only /messages/stats aggregate for dashboard"
```

---

### Task 6: Mail sidebar — folder tree + footer shortcuts

**Files:**
- Create: `frontend/src/components/mail/sidebar-folders.tsx`
- Modify: `frontend/src/components/mail/mail.tsx` (nav pane composition)
- Modify: `frontend/src/i18n/en.json`, `frontend/src/i18n/zh.json`

Current state: nav pane renders `AccountSwitcher`, compose button, `Nav` (flat folder links), `FolderNavTree` (custom folders per account), second `Nav` (contacts/calendar/settings links), footer with `LyraWordmark` + `ThemeToggle`. Data: `useMailStore.getUnifiedFolders()` (per-role aggregates) and `getFoldersForAccount(accountId)`; selection via `useUIStore` (`selectedAccountId`, `selectedFolderId`, `selectedFolderRole`).

- [ ] **Step 1: Create `sidebar-folders.tsx`**

One component rendering both sections:

```tsx
// UNIFIED section: rows for roles inbox/drafts/sent/trash from getUnifiedFolders()
//   row: role icon (lucide: Inbox, File, Send, Trash2), label t(locale, `mail.folder.${role}`),
//        unread count (text-[11.5px] text-muted-foreground, only when > 0), active = bg-accent rounded-[7px]
// ACCOUNTS section: for each account in useMailStore.accounts:
//   header row: ChevronDown/ChevronRight (collapsible, local useState per account, default expanded),
//     `${displayName} — ${providerName}` (text-[12.5px] font-semibold), total unread right
//   children (indent pl-[26px] via padding): folders from getFoldersForAccount(id) sorted by sortOrder:
//     role folders get role icons; custom folders get Folder icon; custom folders nest by parentId
//     (reuse buildCustomFolderTree from '@/lib/folder-tree'), each level adds 16px left padding,
//     collapsible when it has children
// Selection: clicking sets useUIStore selectedAccountId/selectedFolderId/selectedFolderRole exactly
//   like the current Nav/FolderNavTree click handlers do — read those and mirror the behavior.
```

Section labels: `text-[10.5px] font-semibold tracking-[0.8px] text-ter-foreground`, padded `px-2.5 pb-1 pt-3.5`.

- [ ] **Step 2: Rewire `mail.tsx` nav pane**

Replace the flat `Nav` + `FolderNavTree` block with `<SidebarFolders />`. Keep `AccountSwitcher` + compose button on top. Replace the second `Nav` (contacts/calendar/settings links): remove it — those destinations move out of the sidebar (settings/dashboard in footer; contacts/calendar keep their routes but are no longer sidebar links — they remain reachable by URL; do not delete the pages).

- [ ] **Step 3: Footer shortcuts**

In the nav-pane footer: `LyraWordmark`, sync dot (`span className="size-1.5 rounded-full bg-ok"` — wired to `useSyncEventSource`/sync status: green when not syncing, amber pulse while syncing), spacer, then three ghost icon buttons (`size-[26px] rounded-[7px] hover:bg-accent`, icon 14, `text-ter-foreground`): `BarChart3` → `navigate('/dashboard')`, `Settings` (gear) → `navigate('/settings')`, then existing `ThemeToggle`. Remove the sync text label if present.

- [ ] **Step 4: i18n keys**

`en.json`: `"mail.section.unified": "Unified"`, `"mail.section.accounts": "Accounts"`. `zh.json`: `"统一"`, `"账户"`. Folder role labels reuse existing `mail.folder.*` keys if present; add any missing (inbox/drafts/sent/trash/spam/archive) in both locales.

- [ ] **Step 5: Check + commit**

```bash
cd frontend && npm run check
git add frontend/src/components/mail/
git commit -m "feat(ui): folder-tree mail sidebar with unified + per-account sections"
```

---

### Task 7: Mail list column + reader polish

**Files:**
- Modify: `frontend/src/components/mail/mail.tsx` (list header)
- Modify: `frontend/src/components/mail/mail-list.tsx` (rows)
- Modify: `frontend/src/components/mail/mail-display.tsx` (toolbar, privacy banner, reply)
- Modify: `frontend/src/components/mail-layout.tsx` (remove padded card frame)

- [ ] **Step 1: Full-bleed panes** — `mail-layout.tsx`: remove outer padding/rounded card; panes touch viewport edges; dividers are 1px `bg-border` (react-resizable-panels `PanelResizeHandle` styled as hairline).

- [ ] **Step 2: List header** — folder title `font-display text-xl`; All/Unread `Tabs` restyled as segmented control: `TabsList` `bg-accent rounded-lg p-0.5`, `TabsTrigger` `rounded-md data-[state=active]:bg-card`; search `Input` below.

- [ ] **Step 3: List rows** — in `mail-list.tsx`: unread row gets `span className="size-1.5 rounded-full bg-unread"` before the avatar; selected row: `rounded-lg border border-input bg-card`; unselected: transparent. Keep badges for labels, `date-fns` timestamps, EmptyState.

- [ ] **Step 4: Reader** — toolbar icon buttons: `variant="ghost" size="icon" className="rounded-[7px] border border-input bg-card"` groups as in the Ardot frame (archive/spam/trash | snooze | reply/reply-all/forward; star + overflow right). Remote-image banner (existing `remote_content` logic): `flex items-center gap-2 rounded-lg border border-border px-3.5 py-2.5 text-[12.5px] text-muted-foreground` with `Shield` icon and underlined action buttons (`Show images`, `Always allow this sender`). Bottom reply box: rounded-lg border, reply icon, placeholder `t(locale, 'mail.replyTo', { name })`, Send pill button right.

- [ ] **Step 5: i18n keys** — add `mail.replyTo` ("Reply to {{name}}…" / "回复 {{name}}…"), `mail.privacyBanner` ("Remote images are hidden to protect your privacy." / "已隐藏远程图片以保护隐私。"), `mail.showImages`, `mail.alwaysAllowSender` in both locales (reuse if they already exist).

- [ ] **Step 6: Check + commit**

```bash
cd frontend && npm run check
git add frontend/src/components/
git commit -m "feat(ui): mail list and reader v2 styling"
```

---

### Task 8: Dashboard page

**Files:**
- Create: `frontend/src/components/dashboard-page.tsx`
- Modify: `frontend/src/router.tsx` (add `/dashboard` route, same auth `beforeLoad` as `/settings`)
- Modify: `frontend/src/i18n/en.json`, `zh.json`

- [ ] **Step 1: API client + hook**

In `frontend/src/lib/`, add `stats-api.ts`: `fetchStats(days: number)` → `api<StatsResponse>(\`/messages/stats?days=\${days}\`)` with a `StatsResponse` type in `frontend/src/types/index.ts` matching the Task 5 JSON.

- [ ] **Step 2: `dashboard-page.tsx`**

Layout: `flex h-svh` → `<SlimPageNav section={t(locale,'dash.section')} items={[{key:'overview', label: t(locale,'dash.overview'), icon: BarChart3, active: true}]} />` + `<main className="flex-1 overflow-auto bg-background">`.

Header: `flex items-center gap-4 border-b border-border px-8 pb-5 pt-7` — title `font-display text-xl` + subtitle `text-[12.5px] text-ter-foreground`, spacer, range `Tabs` (7/30/90 days) driving a `useState<number>(30)` and refetch.

KPI row (`grid grid-cols-4 gap-4 px-8 pt-6`): cards `rounded-[10px] border border-border bg-card p-4`: label `text-[10.5px] font-semibold tracking-[0.8px] text-ter-foreground`, value `font-display text-2xl`, sub `text-[11.5px] text-muted-foreground`.
- Unread: `stats.totals.unread`; Received today: last `daily` entry; Sent this week: from `totals.sent` (label notes the window); Storage: **no endpoint** — render empty state: value `—`, sub `t(locale,'dash.storageUnavailable')`.

Volume card (`mx-8 mt-4`): title `text-[13.5px] font-semibold`; bars: `flex h-[140px] items-end gap-1.5` of `flex-1` divs, each `div` with `style={{ height: pct }}`, `rounded-sm bg-primary`, last bar `bg-unread`; day letters row `text-[10px] text-ter-foreground` centered per bar (letters from `date-fns` `format(date, 'EEEEE')`, locale-aware).

Bottom row (`grid grid-cols-2 gap-4 px-8 py-4`): Top senders card — rows: avatar tile (`size-6 rounded-md bg-muted flex items-center justify-center text-[10px] font-medium text-muted-foreground`, initials via `getInitials` from `lib/utils`), name `text-[13px]`, count right. By-account card — one row per `useMailStore.accounts`: name, count right, then bar `h-2 rounded bg-accent` with inner `bg-primary h-2 rounded` width = `count/total` %.

Empty states: `EmptyState` when `daily` is empty / no senders.

- [ ] **Step 3: i18n keys** (both locales): `dash.section` ("Dashboard"/"仪表盘"), `dash.overview`, `dash.subtitle` ("Your email at a glance"/"邮件一览"), `dash.unread`, `dash.receivedToday`, `dash.sentThisWeek`, `dash.storage`, `dash.storageUnavailable` ("Storage info not available yet"/"存储信息暂不可用"), `dash.volume` ("Volume — last {{days}} days"/"近 {{days}} 天邮件量"), `dash.topSenders`, `dash.byAccount`, `dash.range7/30/90`.

- [ ] **Step 4: Check + commit**

```bash
cd frontend && npm run check
git add frontend/src/
git commit -m "feat(ui): analytics dashboard page + route"
```

---

### Task 9: Settings page — standalone slim-nav layout

**Files:**
- Modify: `frontend/src/components/settings-page.tsx` (restructure, keep all working logic)
- Modify: `frontend/src/i18n/en.json`, `zh.json`

Restructure the 897-line page into the slim-nav shell; **do not rewrite the working logic** (account CRUD + probe, TOTP, privacy PATCH, locale/theme/mark-read) — only reparent and restyle.

- [ ] **Step 1: Shell** — replace `SecondaryPage` wrapper with `flex h-svh`: `SlimPageNav` (section `settings.section`, items General/Accounts/Spam & Filters/Privacy/Appearance with icons SlidersHorizontal/Users/Flag/Shield/Gear, `active` + `onClick` setting a local `useState<SettingsSection>`) + `<main className="flex-1 overflow-auto bg-background">` with a header (`font-display text-xl` title + `text-[12.5px] text-ter-foreground` subtitle + `border-b border-border`) per section.

- [ ] **Step 2: Section mapping**
- **General**: language buttons + theme select + mark-read policy + logout (today's Session + Preferences sections).
- **Accounts**: account cards restyled per spec (avatar tile with provider initial, address, `size-1.5 rounded-full bg-ok` + last-synced + protocol badge, Manage button opening the existing edit modal; Add account card `bg-secondary border border-input` with `Plus` icon; default-sending-account row if the API exposes a default — otherwise omit the row). Keep sync-now per-account state and error surfacing exactly as-is.
- **Spam & Filters**: new section, all controls **disabled with a "Soon" badge** (`Badge variant="outline"` with `t(locale,'common.soon')`): three `Switch` rows (enable filtering / learn from actions / auto-delete 30 days), sensitivity segmented (Lenient/Standard/Strict), blocked-senders card (two example rows with `X` buttons + disabled input with Add).
- **Privacy**: Remote content card wired to the existing `privacy-api.ts` GET/PATCH (real, functional); Tracking protection card disabled with "Soon"; Your data card (Export / Delete… in `text-destructive`) disabled with "Soon".
- **Appearance**: omit the Appearance nav item for v1 (theme + language live in General). The slim nav shows four items: General, Accounts, Spam & Filters, Privacy.

- [ ] **Step 3: i18n keys** (both locales): `settings.section` ("Settings"/"设置"), `settings.general/accounts/spam/privacy/appearance`, section subtitles, `common.soon` ("Soon"/"即将推出"), spam toggle labels/descriptions, blocked-senders strings, tracking-protection strings, data strings. Reuse existing `settings.*` keys wherever they exist.

- [ ] **Step 4: Check + commit**

```bash
cd frontend && npm run check
git add frontend/src/components/settings-page.tsx frontend/src/i18n/
git commit -m "feat(ui): standalone settings with slim nav and sections"
```

---

### Task 10: Cleanup + docs

- [ ] **Step 1: Remove dead code** — `components/secondary-page.tsx` is still used by contacts/calendar (keep). Delete `components/mail/folder-nav-tree.tsx` and `components/mail/nav.tsx` **only if** nothing imports them after Task 6 (`grep -r "folder-nav-tree\|from '@/components/mail/nav'" frontend/src`). `lib/folder-tree.ts` stays (reused).
- [ ] **Step 2: `AGENTS.md`** — no structural change needed (spec pointer already updated); confirm the frontend description line still matches (stamp brand, folder tree, dashboard route).
- [ ] **Step 3: Commit**

```bash
git add -A frontend/src AGENTS.md
git commit -m "chore(ui): remove superseded nav components"
```

---

### Task 11: Verification walkthrough

- [ ] **Step 1: Checks** — `cd frontend && npm run check`; `cd backend && cargo clippy --all-targets --all-features -- -D warnings && cargo test --bin lyra_backend`. All green.
- [ ] **Step 2: Docker run** — `docker compose up -d --build lyra`, open `http://127.0.0.1:3000`, log in (cheng / Lyra@2026).
- [ ] **Step 3: Browser pass** (agent-browser screenshots): login → mail (expand/collapse account tree, select unified Inbox, select a nested folder, open message, privacy banner, reply box) → dashboard (switch 7/30/90) → settings (all 4 sections) → toggle dark via footer moon → re-check each page → switch locale to zh → spot-check mail + settings.
- [ ] **Step 4: Compare against Ardot** page `7:1` frames; fix visual deltas; commit fixes.
