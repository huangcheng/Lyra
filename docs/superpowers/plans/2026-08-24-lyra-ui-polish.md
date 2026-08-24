# Lyra UI Polish & Identity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the broken/unstyled pages (calendar, contacts, settings sync feedback) and give Lyra a coherent visual identity (indigo accent, wordmark, dark mode, designed empty states) within the existing shadcn/Tailwind stack.

**Architecture:** Frontend-only changes under `frontend/src`. Theme state lives in the existing Zustand UI store with `localStorage` persistence and a `dark` class on `<html>` (Tailwind `dark` variant is already configured via `@custom-variant`). Accent identity is purely CSS-token-level in `index.css`. All new UI strings go through the existing `t(locale, key)` i18n helper.

**Tech Stack:** React 19, Tailwind CSS 4, shadcn/ui, Zustand, TanStack Router, lucide-react. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-08-24-lyra-ui-polish-design.md`

**Deviations from spec (approved simplifications):**
- Keep the already-bundled Geist Variable font instead of adding Inter (`@fontsource-variable/geist` is imported in `main.tsx`; adding Inter would be a redundant dependency).
- Sync errors surface inline on the account card instead of via toast (no toast library exists in the project).

**Notes on process:**
- The frontend has no test runner (verification is `npm run check` = tsc + oxlint + prettier). TDD steps are replaced by typecheck/lint + browser screenshot verification.
- Commit steps are included per repo convention; the executing agent must ask the user before running any `git commit`.

---

### Task 1: i18n keys (en + zh) and Trash rename

**Files:**
- Modify: `frontend/src/i18n/en.json`
- Modify: `frontend/src/i18n/zh.json`

- [ ] **Step 1: Add new keys to `en.json`**

In the `mail` object, replace the `noMessages` / `selectMessage` values and add hints:

```json
    "noMessages": "No messages",
    "noMessagesHint": "New mail will appear here after the next sync.",
    "selectMessage": "Select a message to read",
```

Add to the `settings` object (after `"theme": "Theme",` — reuse it):

```json
    "themeMode": {
      "light": "Light",
      "dark": "Dark",
      "system": "System"
    },
```

Add to the `sync` object:

```json
    "syncFailed": "Sync failed",
```

Add to the `contacts` object:

```json
    "emptyHint": "Contacts from your accounts will appear here after a sync."
```

Add to the `calendar` object:

```json
    "empty": "No calendars yet",
    "emptyHint": "Calendars from your accounts will appear here after a sync.",
    "loadError": "Couldn't load calendars.",
    "moreEvents": "+{{count}} more"
```

Add to the `auth` object:

```json
    "usernamePlaceholder": "your username",
```

- [ ] **Step 2: Mirror keys in `zh.json` and rename Trash**

Change `"trash": "垃圾箱"` → `"trash": "已删除"` (Spam stays `垃圾邮件`).

Add the mirrors:

```json
    "noMessages": "没有邮件",
    "noMessagesHint": "下次同步后，新邮件会显示在这里。",
    "selectMessage": "选择一封邮件以阅读",
```

```json
    "themeMode": {
      "light": "浅色",
      "dark": "深色",
      "system": "跟随系统"
    },
```

```json
    "syncFailed": "同步失败",
```

```json
    "emptyHint": "同步后，账户中的联系人会显示在这里。"
```

```json
    "empty": "暂无日历",
    "emptyHint": "同步后，账户中的日历会显示在这里。",
    "loadError": "无法加载日历。",
    "moreEvents": "还有 {{count}} 项"
```

```json
    "usernamePlaceholder": "用户名",
```

- [ ] **Step 3: Verify JSON validity and typecheck**

Run: `cd frontend && node -e "JSON.parse(require('fs').readFileSync('src/i18n/en.json'));JSON.parse(require('fs').readFileSync('src/i18n/zh.json'))" && npm run typecheck`
Expected: no output from node, typecheck passes.

- [ ] **Step 4: Commit**

```bash
git add frontend/src/i18n/en.json frontend/src/i18n/zh.json
git commit -m "feat: add i18n keys for UI polish and rename zh Trash"
```

---

### Task 2: Theme system (light / dark / system)

**Files:**
- Create: `frontend/src/lib/theme.ts`
- Modify: `frontend/src/stores/ui.ts`
- Modify: `frontend/src/main.tsx`

- [ ] **Step 1: Create `frontend/src/lib/theme.ts`**

```ts
/**
 * Light / dark / system theme handling.
 * Persisted in localStorage; applied as a `dark` class on <html>
 * (Tailwind dark variant is wired via @custom-variant in index.css).
 */

export type ThemeMode = 'light' | 'dark' | 'system';

const STORAGE_KEY = 'lyra_theme';

export function getStoredTheme(): ThemeMode {
  const value = localStorage.getItem(STORAGE_KEY);
  return value === 'light' || value === 'dark' || value === 'system' ? value : 'system';
}

export function applyTheme(mode: ThemeMode): void {
  const prefersDark = window.matchMedia('(prefers-color-scheme: dark)').matches;
  const dark = mode === 'dark' || (mode === 'system' && prefersDark);
  document.documentElement.classList.toggle('dark', dark);
}

export function storeTheme(mode: ThemeMode): void {
  localStorage.setItem(STORAGE_KEY, mode);
}

/** Apply the stored theme and follow OS changes while mode is `system`. */
export function initTheme(): ThemeMode {
  const mode = getStoredTheme();
  applyTheme(mode);
  window.matchMedia('(prefers-color-scheme: dark)').addEventListener('change', () => {
    if (getStoredTheme() === 'system') applyTheme('system');
  });
  return mode;
}
```

- [ ] **Step 2: Wire theme into the UI store**

In `frontend/src/stores/ui.ts`:

Add the import at the top:

```ts
import { applyTheme, getStoredTheme, storeTheme, type ThemeMode } from '@/lib/theme';
```

Add to the `UIState` interface (after `markReadPolicy: MarkReadPolicy;`):

```ts
  theme: ThemeMode;
  setTheme: (theme: ThemeMode) => void;
```

Add to the initial state (after `markReadPolicy: 'on_open' as MarkReadPolicy,`):

```ts
  theme: getStoredTheme(),
```

Add the action (after `setMarkReadPolicy`):

```ts
  setTheme: (theme) => {
    storeTheme(theme);
    applyTheme(theme);
    set({ theme });
  },
```

- [ ] **Step 3: Apply theme at startup**

In `frontend/src/main.tsx`, add the import and call `initTheme()` before `restoreSession()`:

```ts
import { initTheme } from './lib/theme';
```

```ts
initTheme();
void restoreSession().then(() => {
```

- [ ] **Step 4: Verify**

Run: `cd frontend && npm run typecheck`
Expected: passes.

- [ ] **Step 5: Browser check**

Run the dev server or use the running Docker build (rebuild required for Docker; for iteration use `cd frontend && npm run dev` on a free port with a proxy to the backend, or verify after the final build). In the browser console: `document.documentElement.classList.contains('dark')` should follow the OS theme by default, and `localStorage.setItem('lyra_theme','dark')` + reload should render dark.

- [ ] **Step 6: Commit**

```bash
git add frontend/src/lib/theme.ts frontend/src/stores/ui.ts frontend/src/main.tsx
git commit -m "feat: light/dark/system theme with persistence"
```

---

### Task 3: Indigo accent tokens

**Files:**
- Modify: `frontend/src/index.css`

- [ ] **Step 1: Update `:root` tokens**

In `frontend/src/index.css`, in `:root` change:

```css
  --primary: #4f46e5;
  --primary-foreground: #ffffff;
```

and

```css
  --ring: #4f46e5;
```

- [ ] **Step 2: Update `.dark` tokens**

In the `.dark` block change:

```css
  --primary: #a5b4fc;
  --primary-foreground: #1e1b4b;
```

and

```css
  --ring: #a5b4fc;
```

- [ ] **Step 3: Unread dot uses the accent**

In `frontend/src/components/mail/mail-list.tsx` change the unread indicator class `bg-blue-600` → `bg-primary`.

- [ ] **Step 4: Verify**

Run: `cd frontend && npm run typecheck && npm run lint`
Expected: passes. Then visual check in browser: primary buttons (Log In, Send, active nav item, Compose icon button hover ring) render indigo in light mode and light indigo in dark mode.

- [ ] **Step 5: Commit**

```bash
git add frontend/src/index.css frontend/src/components/mail/mail-list.tsx
git commit -m "feat: indigo accent color tokens for light and dark"
```

---

### Task 4: Lyra wordmark + login page branding

**Files:**
- Create: `frontend/src/components/lyra-wordmark.tsx`
- Modify: `frontend/src/components/login-form.tsx`
- Modify: `frontend/src/components/mail/mail.tsx`

- [ ] **Step 1: Create `frontend/src/components/lyra-wordmark.tsx`**

```tsx
/**
 * Lyra brand mark: four-point star + wordmark. Inline SVG, theme-aware.
 */

import { cn } from '@/lib/utils';

export function LyraWordmark({ className }: { className?: string }) {
  return (
    <span className={cn('inline-flex items-center gap-2 select-none', className)}>
      <svg
        viewBox="0 0 24 24"
        className="h-5 w-5 text-primary"
        fill="currentColor"
        aria-hidden="true"
      >
        <path d="M12 2c.6 4.8 4.6 8.8 9.4 9.4v1.2c-4.8.6-8.8 4.6-9.4 9.4h-1.2c-.6-4.8-4.6-8.8-9.4-9.4v-1.2C6.2 10.8 10.2 6.8 10.8 2H12Z" />
      </svg>
      <span className="text-lg font-semibold tracking-tight">Lyra</span>
    </span>
  );
}
```

- [ ] **Step 2: Brand the login page**

In `frontend/src/components/login-form.tsx`:

Add the import:

```tsx
import { LyraWordmark } from '@/components/lyra-wordmark';
```

Inside the outer `div` (before `<Card>`), add:

```tsx
      <div className="flex justify-center">
        <LyraWordmark />
      </div>
```

Change the username Input placeholder in the login form (line with `placeholder="admin"`):

```tsx
                    placeholder={t(locale, 'auth.usernamePlaceholder')}
```

- [ ] **Step 3: Wordmark in the mail sidebar footer**

In `frontend/src/components/mail/mail.tsx`:

Add the import:

```tsx
import { LyraWordmark } from '@/components/lyra-wordmark';
```

Inside `<ResizablePanel id="nav" …>`, wrap the existing children in a flex column and add a footer pinned to the bottom (the toggle is added beside the wordmark in Task 5):

```tsx
          <div className="flex h-full flex-col">
            …existing header/Nav content unchanged…
            <div className="mt-auto flex items-center justify-between px-3 py-2">
              {isCollapsed ? null : <LyraWordmark className="[&>span:last-child]:text-sm" />}
            </div>
          </div>
```

(The existing `<Separator />` between sections stays; the footer sits below the final Nav, pushed down by `mt-auto`.)

- [ ] **Step 4: Verify**

Run: `cd frontend && npm run typecheck`
Expected: passes. Browser: login page shows the star + "Lyra" above the card; sidebar footer shows the wordmark bottom-left.

- [ ] **Step 5: Commit**

```bash
git add frontend/src/components/lyra-wordmark.tsx frontend/src/components/login-form.tsx frontend/src/components/mail/mail.tsx
git commit -m "feat: Lyra wordmark on login and mail sidebar"
```

---

### Task 5: ThemeToggle component + settings theme selector

**Files:**
- Create: `frontend/src/components/theme-toggle.tsx`
- Modify: `frontend/src/components/mail/mail.tsx` (sidebar footer)
- Modify: `frontend/src/components/settings-page.tsx` (Session section)

- [ ] **Step 1: Create `frontend/src/components/theme-toggle.tsx`**

```tsx
/**
 * Light / dark / system theme picker (dropdown).
 */

import { Monitor, Moon, Sun } from 'lucide-react';

import { Button } from '@/components/ui/button';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu';
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip';
import { t } from '@/i18n';
import type { ThemeMode } from '@/lib/theme';
import { useUIStore } from '@/stores/ui';

export function ThemeToggle({ isCollapsed = false }: { isCollapsed?: boolean }) {
  const locale = useUIStore((s) => s.locale);
  const theme = useUIStore((s) => s.theme);
  const setTheme = useUIStore((s) => s.setTheme);

  const Icon = theme === 'dark' ? Moon : theme === 'light' ? Sun : Monitor;

  const items: { value: ThemeMode; label: string }[] = [
    { value: 'light', label: t(locale, 'settings.themeMode.light') },
    { value: 'dark', label: t(locale, 'settings.themeMode.dark') },
    { value: 'system', label: t(locale, 'settings.themeMode.system') },
  ];

  const trigger = (
    <Button variant="ghost" size="icon" className="h-8 w-8" aria-label={t(locale, 'settings.theme')}>
      <Icon className="h-4 w-4" />
    </Button>
  );

  return (
    <DropdownMenu>
      {isCollapsed ? (
        <Tooltip delayDuration={0}>
          <TooltipTrigger asChild>
            <DropdownMenuTrigger asChild>{trigger}</DropdownMenuTrigger>
          </TooltipTrigger>
          <TooltipContent side="right">{t(locale, 'settings.theme')}</TooltipContent>
        </Tooltip>
      ) : (
        <DropdownMenuTrigger asChild>{trigger}</DropdownMenuTrigger>
      )}
      <DropdownMenuContent align="end">
        {items.map((item) => (
          <DropdownMenuItem key={item.value} onClick={() => setTheme(item.value)}>
            {item.label}
            {theme === item.value ? <span className="ml-auto text-primary">●</span> : null}
          </DropdownMenuItem>
        ))}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
```

Check `frontend/src/components/ui/dropdown-menu.tsx` exports first (`DropdownMenu`, `DropdownMenuContent`, `DropdownMenuItem`, `DropdownMenuTrigger`) and match its actual export names.

- [ ] **Step 2: Add the toggle to the sidebar footer**

In `frontend/src/components/mail/mail.tsx`, add the `ThemeToggle` import and place `<ThemeToggle isCollapsed={isCollapsed} />` beside the wordmark in the footer added in Task 4 (if Task 4 deferred it).

- [ ] **Step 3: Theme selector in Settings → Session**

In `frontend/src/components/settings-page.tsx`, inside the Session `<section>`, after the language buttons and before the Log Out button, add:

```tsx
            <Select value={theme} onValueChange={(v) => setTheme(v as ThemeMode)}>
              <SelectTrigger size="sm" className="min-w-[140px]">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {(['light', 'dark', 'system'] as const).map((mode) => (
                  <SelectItem key={mode} value={mode}>
                    {t(locale, `settings.themeMode.${mode}`)}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
```

Add store hooks at the top of the component:

```tsx
  const theme = useUIStore((s) => s.theme);
  const setTheme = useUIStore((s) => s.setTheme);
```

and the type import `import type { ThemeMode } from '@/lib/theme';`. Add a label row: wrap the Select with `<span className="text-sm text-muted-foreground">{t(locale, 'settings.theme')}</span>` following the existing mark-read row pattern.

- [ ] **Step 4: Verify**

Run: `cd frontend && npm run typecheck && npm run lint`
Expected: passes. Browser: toggle via sidebar dropdown — `dark` class flips on `<html>`, all pages restyle; selection survives reload; Settings select reflects current mode.

- [ ] **Step 5: Commit**

```bash
git add frontend/src/components/theme-toggle.tsx frontend/src/components/mail/mail.tsx frontend/src/components/settings-page.tsx
git commit -m "feat: theme toggle in sidebar and settings"
```

---

### Task 6: EmptyState component + mail empty states

**Files:**
- Create: `frontend/src/components/empty-state.tsx`
- Modify: `frontend/src/components/mail/mail-list.tsx:129-139`
- Modify: `frontend/src/components/mail/mail-display.tsx:576-580`

- [ ] **Step 1: Create `frontend/src/components/empty-state.tsx`**

```tsx
/**
 * Shared empty state: muted icon disc + headline + optional hint.
 */

import type { LucideIcon } from 'lucide-react';

export function EmptyState({
  icon: Icon,
  title,
  hint,
}: {
  icon: LucideIcon;
  title: string;
  hint?: string;
}) {
  return (
    <div className="flex h-full min-h-[200px] flex-col items-center justify-center gap-2 p-8 text-center">
      <div className="mb-1 flex h-12 w-12 items-center justify-center rounded-full bg-muted">
        <Icon className="h-6 w-6 text-muted-foreground" />
      </div>
      <p className="text-sm font-medium">{title}</p>
      {hint ? <p className="max-w-xs text-sm text-muted-foreground">{hint}</p> : null}
    </div>
  );
}
```

- [ ] **Step 2: Use it in the mail list**

In `frontend/src/components/mail/mail-list.tsx` replace the `filtered.length === 0` early return with:

```tsx
  if (filtered.length === 0) {
    return (
      <EmptyState
        icon={Inbox}
        title={t(locale, 'mail.noMessages')}
        hint={t(locale, 'mail.noMessagesHint')}
      />
    );
  }
```

Add imports: `import { Inbox } from 'lucide-react';` and `import { EmptyState } from '@/components/empty-state';`. (Use `Search` icon + `mail.noMessages` without hint when `searchHits` is an empty array and `searchQuery` is non-empty — i.e. a search with no results: `title={t(locale, 'mail.noMessages')}`, hint omitted.)

- [ ] **Step 3: Use it in the message pane**

In `frontend/src/components/mail/mail-display.tsx` replace the final `: (<div className="p-8 …">{t(locale, 'mail.selectMessage')}</div>)` with:

```tsx
        <EmptyState icon={MailOpen} title={t(locale, 'mail.selectMessage')} />
```

Add imports `import { MailOpen } from 'lucide-react';` (check existing lucide imports in that file to avoid duplicates) and `import { EmptyState } from '@/components/empty-state';`.

- [ ] **Step 4: Verify**

Run: `cd frontend && npm run typecheck`
Expected: passes. Browser: empty inbox shows icon + headline + hint; right pane shows the new empty state.

- [ ] **Step 5: Commit**

```bash
git add frontend/src/components/empty-state.tsx frontend/src/components/mail/mail-list.tsx frontend/src/components/mail/mail-display.tsx
git commit -m "feat: shared EmptyState for mail list and message pane"
```

---

### Task 7: Contacts page polish

**Files:**
- Modify: `frontend/src/components/contacts-page.tsx`

- [ ] **Step 1: Remove the duplicate heading and restyle**

In `frontend/src/components/contacts-page.tsx`:

Delete the inner `<h1>{t(locale, 'contacts.title')}</h1>` (the `SecondaryPage` header already renders the title) and the wrapping `contacts-header` div, keeping the search form.

Replace the search `<input>` and `<button>` with shadcn components (add imports `Input`, `Button`):

```tsx
            <form onSubmit={handleSearch} className="flex gap-2">
              <Input
                placeholder={t(locale, 'contacts.search')}
                value={searchQuery}
                onChange={(e) => setSearchQuery(e.target.value)}
              />
              <Button type="submit" variant="outline" size="sm">
                {t(locale, 'common.search')}
              </Button>
            </form>
```

- [ ] **Step 2: Replace undefined CSS classes with Tailwind**

Loading / error / empty branches:

```tsx
          {loading ? (
            <div className="p-4 text-sm text-muted-foreground">{t(locale, 'common.loading')}</div>
          ) : error ? (
            <div className="p-4 text-sm text-destructive">{error}</div>
          ) : contacts.length === 0 ? (
            <EmptyState
              icon={Users}
              title={t(locale, 'contacts.empty')}
              hint={t(locale, 'contacts.emptyHint')}
            />
          ) : (
```

Contact list rows (replace `contacts-list` / `contact-item` / `contact-avatar` / `contact-info` / `contact-name` / `contact-email` classes):

```tsx
            <div className="space-y-1">
              {contacts.map((contact) => (
                <button
                  key={contact.id}
                  type="button"
                  className={cn(
                    'flex w-full items-center gap-3 rounded-lg border p-3 text-left transition-colors hover:bg-accent',
                    selectedContact?.id === contact.id && 'bg-muted',
                  )}
                  onClick={() => setSelectedContact(contact)}
                >
                  <span className="flex h-9 w-9 shrink-0 items-center justify-center rounded-full bg-primary/10 text-sm font-medium text-primary">
                    {getInitials(contact.displayName)}
                  </span>
                  <span className="min-w-0">
                    <span className="block truncate text-sm font-medium">
                      {contact.displayName || t(locale, 'contacts.noName')}
                    </span>
                    {contact.emailAddresses[0] && (
                      <span className="block truncate text-xs text-muted-foreground">
                        {contact.emailAddresses[0]}
                      </span>
                    )}
                  </span>
                </button>
              ))}
            </div>
```

Detail pane (replace `contacts-detail` / `contact-detail-card` / `detail-section` / `detail-item` / `no-selection`):

```tsx
        <div className="min-w-0 flex-1">
          {selectedContact ? (
            <div className="space-y-6 rounded-lg border p-6">
              <div className="flex items-center gap-4">
                <span className="flex h-14 w-14 items-center justify-center rounded-full bg-primary/10 text-xl font-medium text-primary">
                  {getInitials(selectedContact.displayName)}
                </span>
                <h2 className="text-lg font-semibold">
                  {selectedContact.displayName || t(locale, 'contacts.noName')}
                </h2>
              </div>
              {selectedContact.emailAddresses.length > 0 && (
                <div className="space-y-1">
                  <h3 className="text-sm font-medium text-muted-foreground">
                    {t(locale, 'contacts.email')}
                  </h3>
                  {selectedContact.emailAddresses.map((email, i) => (
                    <div key={i} className="text-sm">
                      <a href={`mailto:${email}`} className="text-primary hover:underline">
                        {email}
                      </a>
                    </div>
                  ))}
                </div>
              )}
              {selectedContact.phoneNumbers.length > 0 && (
                <div className="space-y-1">
                  <h3 className="text-sm font-medium text-muted-foreground">
                    {t(locale, 'contacts.phone')}
                  </h3>
                  {selectedContact.phoneNumbers.map((phone, i) => (
                    <div key={i} className="text-sm">
                      <a href={`tel:${phone}`} className="text-primary hover:underline">
                        {phone}
                      </a>
                    </div>
                  ))}
                </div>
              )}
              {selectedContact.organisation && (
                <div className="space-y-1">
                  <h3 className="text-sm font-medium text-muted-foreground">
                    {t(locale, 'contacts.organisation')}
                  </h3>
                  <p className="text-sm">{selectedContact.organisation}</p>
                </div>
              )}
            </div>
          ) : (
            <EmptyState icon={UserRound} title={t(locale, 'contacts.selectContact')} />
          )}
        </div>
```

Add imports: `import { UserRound, Users } from 'lucide-react';`, `import { EmptyState } from '@/components/empty-state';`, `import { cn } from '@/lib/utils';`, `import { Button } from '@/components/ui/button';`, `import { Input } from '@/components/ui/input';`.

- [ ] **Step 3: Verify**

Run: `cd frontend && npm run typecheck && npm run lint`
Expected: passes. Browser: single "Contacts" heading; styled rows with avatar circles; empty state with icon.

- [ ] **Step 4: Commit**

```bash
git add frontend/src/components/contacts-page.tsx
git commit -m "fix: restyle contacts page, drop duplicate heading"
```

---

### Task 8: Calendar page rebuild

**Files:**
- Modify: `frontend/src/components/calendar-page.tsx` (full rewrite of the render layer; data fetching logic stays)

- [ ] **Step 1: Fix the swallowed error state**

Change `const [, setError] = useState<string | null>(null);` to:

```tsx
  const [error, setError] = useState<string | null>(null);
```

- [ ] **Step 2: Rewrite the render with Tailwind/shadcn**

Replace the `renderCalendarGrid` function and the return JSX. Keep `fetchCalendars`, `fetchEvents`, `formatEventTime`, `formatEventDate`, `getEventsForDate`, `navigateMonth`, and all state as-is.

New `renderCalendarGrid`:

```tsx
  function renderCalendarGrid() {
    const year = currentDate.getFullYear();
    const month = currentDate.getMonth();
    const firstDay = new Date(year, month, 1);
    const lastDay = new Date(year, month + 1, 0);
    const startDate = new Date(firstDay);
    startDate.setDate(startDate.getDate() - firstDay.getDay());

    const days: Date[] = [];
    const current = new Date(startDate);
    while (current <= lastDay || current.getDay() !== 0) {
      days.push(new Date(current));
      current.setDate(current.getDate() + 1);
    }

    const weekdays = ['sun', 'mon', 'tue', 'wed', 'thu', 'fri', 'sat'];

    return (
      <div className="grid grid-cols-7 gap-px overflow-hidden rounded-lg border bg-border">
        {weekdays.map((day) => (
          <div
            key={day}
            className="bg-muted px-2 py-1.5 text-center text-xs font-medium text-muted-foreground"
          >
            {t(locale, `calendar.days.${day}`)}
          </div>
        ))}
        {days.map((day, i) => {
          const dayEvents = getEventsForDate(day);
          const isCurrentMonth = day.getMonth() === month;
          const isToday = day.toDateString() === new Date().toDateString();

          return (
            <div
              key={i}
              className={cn(
                'min-h-24 bg-background p-1.5',
                !isCurrentMonth && 'bg-muted/40 text-muted-foreground',
              )}
            >
              <div
                className={cn(
                  'flex h-6 w-6 items-center justify-center rounded-full text-xs',
                  isToday && 'bg-primary font-semibold text-primary-foreground',
                )}
              >
                {day.getDate()}
              </div>
              <div className="mt-1 space-y-0.5">
                {dayEvents.slice(0, 3).map((event) => (
                  <button
                    key={event.id}
                    type="button"
                    className="block w-full truncate rounded px-1 py-0.5 text-left text-xs text-white"
                    style={{ backgroundColor: selectedCalendar?.color || '#4f46e5' }}
                    onClick={() => setSelectedEvent(event)}
                  >
                    {event.summary || t(locale, 'calendar.noTitle')}
                  </button>
                ))}
                {dayEvents.length > 3 && (
                  <div className="px-1 text-xs text-muted-foreground">
                    {t(locale, 'calendar.moreEvents', { count: dayEvents.length - 3 })}
                  </div>
                )}
              </div>
            </div>
          );
        })}
      </div>
    );
  }
```

New return JSX:

```tsx
  return (
    <SecondaryPage title={t(locale, 'calendar.title')}>
      <div className="mx-auto flex max-w-5xl gap-6">
        <div className="w-48 shrink-0 space-y-2">
          <h2 className="text-sm font-medium text-muted-foreground">
            {t(locale, 'calendar.calendars')}
          </h2>
          {loading ? (
            <div className="text-sm text-muted-foreground">{t(locale, 'common.loading')}</div>
          ) : error ? (
            <div className="text-sm text-destructive">{t(locale, 'calendar.loadError')}</div>
          ) : calendars.length === 0 ? (
            <EmptyState
              icon={Calendar}
              title={t(locale, 'calendar.empty')}
              hint={t(locale, 'calendar.emptyHint')}
            />
          ) : (
            <div className="space-y-1">
              {calendars.map((calendar) => (
                <button
                  key={calendar.id}
                  type="button"
                  className={cn(
                    'flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm transition-colors hover:bg-accent',
                    selectedCalendar?.id === calendar.id && 'bg-muted font-medium',
                  )}
                  onClick={() => setSelectedCalendar(calendar)}
                >
                  <span
                    className="h-3 w-3 shrink-0 rounded-full"
                    style={{ backgroundColor: calendar.color || '#4f46e5' }}
                  />
                  <span className="truncate">{calendar.name}</span>
                </button>
              ))}
            </div>
          )}
        </div>

        <div className="min-w-0 flex-1 space-y-4">
          <div className="flex items-center gap-2">
            <Button variant="outline" size="icon" className="h-8 w-8" onClick={() => navigateMonth(-1)}>
              <ChevronLeft className="h-4 w-4" />
            </Button>
            <h2 className="min-w-40 text-center text-base font-semibold">
              {currentDate.toLocaleDateString(locale === 'zh' ? 'zh-CN' : 'en-US', {
                month: 'long',
                year: 'numeric',
              })}
            </h2>
            <Button variant="outline" size="icon" className="h-8 w-8" onClick={() => navigateMonth(1)}>
              <ChevronRight className="h-4 w-4" />
            </Button>
            <Button variant="outline" size="sm" className="ml-auto" onClick={() => setCurrentDate(new Date())}>
              {t(locale, 'calendar.today')}
            </Button>
          </div>

          {renderCalendarGrid()}
        </div>

        {selectedEvent && (
          <div className="w-72 shrink-0 space-y-3 self-start rounded-lg border p-4">
            <div className="flex items-start justify-between gap-2">
              <h3 className="text-sm font-semibold">
                {selectedEvent.summary || t(locale, 'calendar.noTitle')}
              </h3>
              <Button
                variant="ghost"
                size="icon"
                className="h-6 w-6"
                onClick={() => setSelectedEvent(null)}
              >
                <X className="h-4 w-4" />
              </Button>
            </div>
            <div className="text-sm text-muted-foreground">
              {formatEventDate(selectedEvent)} • {formatEventTime(selectedEvent)}
            </div>
            {selectedEvent.location && (
              <div className="text-sm">{selectedEvent.location}</div>
            )}
            {selectedEvent.description && (
              <div className="text-sm whitespace-pre-wrap">{selectedEvent.description}</div>
            )}
          </div>
        )}
      </div>
    </SecondaryPage>
  );
```

Add imports: `import { Calendar, ChevronLeft, ChevronRight, X } from 'lucide-react';`, `import { Button } from '@/components/ui/button';`, `import { EmptyState } from '@/components/empty-state';`, `import { cn } from '@/lib/utils';`. Use `@/` alias imports for consistency where the file already uses relative ones — match the file's existing style.

- [ ] **Step 3: Verify**

Run: `cd frontend && npm run typecheck && npm run lint`
Expected: passes. Browser: month renders as a 7-column grid; today is accent-highlighted; prev/next/Today work; with no calendars the empty state shows; killing the backend and reloading shows the load error line.

- [ ] **Step 4: Commit**

```bash
git add frontend/src/components/calendar-page.tsx
git commit -m "fix: rebuild calendar page with Tailwind grid and states"
```

---

### Task 9: Settings sync feedback per account

**Files:**
- Modify: `frontend/src/components/settings-page.tsx:95-96,146-174,603-608,618-661`

- [ ] **Step 1: Per-account syncing state**

Replace:

```tsx
  const [syncing, setSyncing] = useState(false);
  const [syncMessage, setSyncMessage] = useState<string | null>(null);
```

with:

```tsx
  const [syncingId, setSyncingId] = useState<string | null>(null);
  const [syncErrors, setSyncErrors] = useState<Record<string, string>>({});
  const [syncMessage, setSyncMessage] = useState<string | null>(null);
```

- [ ] **Step 2: Rewrite `handleSync`**

```tsx
  async function handleSync(id: string) {
    try {
      setSyncingId(id);
      setSyncMessage(null);
      setSyncErrors((prev) => {
        const next = { ...prev };
        delete next[id];
        return next;
      });
      await api(`/accounts/${id}/sync`, { method: 'POST' });
      await pollUntilSyncIdle();
      await fetchAccounts();
      setSyncMessage(t(locale, 'sync.syncComplete'));
    } catch (err: unknown) {
      const message = err instanceof Error ? err.message : String(err);
      setSyncErrors((prev) => ({ ...prev, [id]: message }));
    } finally {
      setSyncingId(null);
    }
  }
```

(Note: the global `setError` call is removed from this path so a failed sync shows on the account card instead of the page-level error banner.)

- [ ] **Step 3: Update the accounts section JSX**

Replace the `syncMessage` block:

```tsx
          {syncMessage && (
            <div className="text-sm text-muted-foreground" role="status">
              {syncMessage}
            </div>
          )}
```

(Delete the `{syncing ? t(locale, 'sync.syncing') : syncMessage}` conditional.)

Replace the three per-account raw `<button>` elements with shadcn `Button`s and per-account state, and add the inline error under the account meta:

```tsx
                    {account.lastSyncAt && (
                      <p className="text-xs text-muted-foreground">
                        {t(locale, 'sync.lastSync')}: {formatLastSync(account.lastSyncAt)}
                      </p>
                    )}
                    {syncErrors[account.id] && (
                      <p className="text-xs text-destructive">
                        {t(locale, 'sync.syncFailed')}: {syncErrors[account.id]}
                      </p>
                    )}
                  </div>
                  <div className="flex gap-2">
                    <Button
                      variant="outline"
                      size="sm"
                      disabled={syncingId === account.id}
                      onClick={() => void handleSync(account.id)}
                    >
                      {syncingId === account.id
                        ? t(locale, 'sync.syncing')
                        : t(locale, 'settings.syncNow')}
                    </Button>
                    <Button variant="outline" size="sm" onClick={() => handleEdit(account)}>
                      {t(locale, 'common.edit')}
                    </Button>
                    <Button
                      variant="outline"
                      size="sm"
                      className="text-destructive"
                      onClick={() => handleDelete(account.id)}
                    >
                      {t(locale, 'common.delete')}
                    </Button>
                  </div>
```

Also replace the "Add Account" raw `<button>` with:

```tsx
            <Button
              variant="outline"
              size="sm"
              onClick={() => {
                resetForm();
                setEditingAccount(null);
                setShowAddForm(true);
              }}
            >
              {t(locale, 'settings.accounts.add')}
            </Button>
```

- [ ] **Step 4: Replace undefined `error-message` / `empty-state` classes**

In the same file, replace every `className="error-message"` with `className="text-sm text-destructive"` and the accounts `empty-state` div with:

```tsx
            <div className="rounded-lg border border-dashed p-6 text-center text-sm text-muted-foreground">
              {t(locale, 'settings.accounts.empty')}
            </div>
```

- [ ] **Step 5: Verify**

Run: `cd frontend && npm run typecheck && npm run lint`
Expected: passes. Browser against the Docker backend: click "Sync now" on one account — only that button spins; when the sync job fails, a red "Sync failed: …" line appears on that account card and stays until the next attempt.

- [ ] **Step 6: Commit**

```bash
git add frontend/src/components/settings-page.tsx
git commit -m "fix: per-account sync state and visible sync errors"
```

---

### Task 10: Full verification

- [ ] **Step 1: Lint / typecheck / format**

Run: `make fmt && cd frontend && npm run check`
Expected: all pass (tsc, oxlint, prettier check).

- [ ] **Step 2: Rebuild the Docker image so the running app reflects changes**

Run: `docker compose up -d --build lyra` (service name per `docker-compose.yml`; check with `docker compose config --services`)
Expected: container healthy, app serving the new build on `http://127.0.0.1:3000`.

- [ ] **Step 3: Browser matrix walkthrough**

With `agent-browser` (open `http://127.0.0.1:3000`, log in as needed), screenshot each of: login, mail inbox, compose dialog, contacts, calendar, settings — in light and dark mode, and re-check mail + login in zh. Confirm: indigo accent applied, wordmark visible, calendar grid correct, no duplicate contacts heading, empty states styled, theme toggle works and persists.

- [ ] **Step 4: Update spec/plan checkboxes and report**

Check off completed steps in this plan. Report screenshots and any residual issues to the user.
