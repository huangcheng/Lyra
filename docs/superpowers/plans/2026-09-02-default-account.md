# Default Mail Account Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let users pick a default mail account (Settings → Accounts, Thunderbird-style) used as the From fallback for new compose in the unified inbox, and fix replies/forwards from the unified view to use the receiving account.

**Architecture:** Frontend-only. The default account id is a per-user UI preference stored in the existing `lyra_user.ui_state` blob via `PATCH /api/v1/auth/preferences` (same seam as `accountOrder`). A pure `resolveFromAccountId` helper centralizes From-account resolution; `ComposeDraft` gains `accountId` so reply/forward/draft compose seeds from the source message's account.

**Tech Stack:** React + Zustand + vitest. No backend changes.

**Spec:** `docs/superpowers/specs/2026-09-02-default-account-design.md`

**Verification gate (applies to every task's final check):** this repo uses a project-reference tsconfig — `npx tsc --noEmit` under-checks it and has let real errors through. Always run `cd frontend && npm run check` (= `tsc -b` + oxlint + prettier) plus `npx vitest run`.

---

### Task 1: `resolveFromAccountId` pure helper

**Files:**
- Create: `frontend/src/lib/resolve-from-account.ts`
- Test: `frontend/src/lib/resolve-from-account.test.ts`

- [ ] **Step 1: Write the failing test**

```ts
import { describe, expect, it } from 'vitest';

import { ALL_ACCOUNTS } from '@/lib/mail-api';
import { resolveFromAccountId } from '@/lib/resolve-from-account';
import type { MailAccount } from '@/types';

function account(id: string): MailAccount {
  return {
    id,
    displayName: id,
    emailAddress: `${id}@example.com`,
    protocol: 'imap',
    isActive: true,
    syncEnabled: true,
  };
}

const accounts = [account('a'), account('b'), account('c')];

describe('resolveFromAccountId', () => {
  it('prefers the draft source account (reply/forward/edit)', () => {
    expect(
      resolveFromAccountId({
        draftAccountId: 'c',
        selectedAccountId: 'b',
        defaultAccountId: 'a',
        accounts,
      }),
    ).toBe('c');
  });

  it('uses the browsed account when not in unified view', () => {
    expect(
      resolveFromAccountId({
        selectedAccountId: 'b',
        defaultAccountId: 'a',
        accounts,
      }),
    ).toBe('b');
  });

  it('uses the default account in unified view', () => {
    expect(
      resolveFromAccountId({
        selectedAccountId: ALL_ACCOUNTS,
        defaultAccountId: 'b',
        accounts,
      }),
    ).toBe('b');
  });

  it('falls back to the first account when no default is set', () => {
    expect(
      resolveFromAccountId({
        selectedAccountId: ALL_ACCOUNTS,
        defaultAccountId: null,
        accounts,
      }),
    ).toBe('a');
  });

  it('falls back when the default account was deleted', () => {
    expect(
      resolveFromAccountId({
        selectedAccountId: ALL_ACCOUNTS,
        defaultAccountId: 'gone',
        accounts,
      }),
    ).toBe('a');
  });

  it('falls back when the draft account is unknown', () => {
    expect(
      resolveFromAccountId({
        draftAccountId: 'gone',
        selectedAccountId: 'b',
        defaultAccountId: null,
        accounts,
      }),
    ).toBe('b');
  });

  it('returns empty string with no accounts', () => {
    expect(
      resolveFromAccountId({
        selectedAccountId: ALL_ACCOUNTS,
        defaultAccountId: null,
        accounts: [],
      }),
    ).toBe('');
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd frontend && npx vitest run src/lib/resolve-from-account.test.ts`
Expected: FAIL — module `@/lib/resolve-from-account` not found.

- [ ] **Step 3: Write the implementation**

```ts
/**
 * Which account a compose is sent From. Resolution order (first hit that
 * still exists in `accounts` wins):
 *
 * 1. the draft's source account (reply/forward/edit-draft),
 * 2. the account being browsed (when not the unified view),
 * 3. the user's default account,
 * 4. the first account (legacy behavior).
 */

import { ALL_ACCOUNTS } from '@/lib/mail-api';
import type { MailAccount } from '@/types';

export function resolveFromAccountId(opts: {
  draftAccountId?: string;
  selectedAccountId: string;
  defaultAccountId: string | null;
  accounts: MailAccount[];
}): string {
  const { draftAccountId, selectedAccountId, defaultAccountId, accounts } = opts;
  const exists = (id: string | null | undefined): id is string =>
    typeof id === 'string' && accounts.some((a) => a.id === id);
  if (exists(draftAccountId)) return draftAccountId;
  if (selectedAccountId !== ALL_ACCOUNTS && exists(selectedAccountId)) return selectedAccountId;
  if (exists(defaultAccountId)) return defaultAccountId;
  return accounts[0]?.id ?? '';
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd frontend && npx vitest run src/lib/resolve-from-account.test.ts`
Expected: PASS (7 tests).

- [ ] **Step 5: Commit**

```bash
git add frontend/src/lib/resolve-from-account.ts frontend/src/lib/resolve-from-account.test.ts
git commit -m "feat(frontend): resolveFromAccountId helper for compose From resolution"
```

---

### Task 2: UI store — `defaultAccountId` + `ComposeDraft.accountId`

**Files:**
- Modify: `frontend/src/stores/ui.ts`

- [ ] **Step 1: Extend `ComposeDraft` (line 11-23)**

Add the field after `draftMessageId`:

```ts
  /** Local message id of the server draft being edited (autosave replaces it). */
  draftMessageId?: string;
  /** Source message's account — reply/forward/draft send From this account. */
  accountId?: string;
```

- [ ] **Step 2: Add state + setter**

In `UIState` interface, after `accountOrder: string[];` (line 47):

```ts
  /** Default compose From account (persisted server-side); null = first account. */
  defaultAccountId: string | null;
```

In the actions block, after `setAccountOrder: (ids: string[]) => void;` (line 66):

```ts
  setDefaultAccount: (id: string | null) => void;
```

In the store initializer, after `accountOrder: [],` (line 83):

```ts
  defaultAccountId: null,
```

After the `setAccountOrder` implementation (line 156):

```ts
  setDefaultAccount: (id) => set({ defaultAccountId: id }),
```

- [ ] **Step 3: Pass `accountId` through `openCompose`**

In `openCompose` (line 111-124), add one line to the draft construction:

```ts
        draftMessageId: draft?.draftMessageId,
        accountId: draft?.accountId,
```

(Without this the field is silently dropped — `openCompose` whitelists fields.)

- [ ] **Step 4: Check + commit**

Run: `cd frontend && npm run check && npx vitest run`
Expected: all green (existing 96 tests; no new tests in this task).

```bash
git add frontend/src/stores/ui.ts
git commit -m "feat(frontend): ui store defaultAccountId + ComposeDraft.accountId"
```

---

### Task 3: Persist `defaultAccountId` in the ui_state blob

**Files:**
- Modify: `frontend/src/lib/persist-view-state.ts`
- Test: `frontend/src/lib/persist-view-state.test.ts` (new)

- [ ] **Step 1: Write the failing test**

```ts
import { beforeEach, describe, expect, it } from 'vitest';

import { applyViewState } from '@/lib/persist-view-state';
import { useUIStore } from '@/stores/ui';

beforeEach(() => {
  useUIStore.setState({ defaultAccountId: null, accountOrder: [] });
});

describe('applyViewState defaultAccountId', () => {
  it('restores a valid string', () => {
    applyViewState({ defaultAccountId: 'acc-1' });
    expect(useUIStore.getState().defaultAccountId).toBe('acc-1');
  });

  it('ignores non-string values', () => {
    applyViewState({ defaultAccountId: 42 });
    expect(useUIStore.getState().defaultAccountId).toBeNull();
  });

  it('leaves the current value when the key is absent', () => {
    useUIStore.setState({ defaultAccountId: 'keep' });
    applyViewState({ accountOrder: ['x'] });
    expect(useUIStore.getState().defaultAccountId).toBe('keep');
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd frontend && npx vitest run src/lib/persist-view-state.test.ts`
Expected: FAIL — first test gets `null` instead of `'acc-1'`.

- [ ] **Step 3: Implement**

In `applyViewState` in `frontend/src/lib/persist-view-state.ts`, after the `accountOrder` block (line 47-49):

```ts
  if (typeof uiState.defaultAccountId === 'string' && uiState.defaultAccountId) {
    ui.setDefaultAccount(uiState.defaultAccountId);
  }
```

Also update the header comment's persistence list (line 3-4) to mention the default account:

```ts
 * Persist mail view-state (selected account/folder, sidebar folder
 * expansion, sidebar account order, default account) to the server so the
 * sidebar restores identically after a reload — and on any other device.
```

- [ ] **Step 4: Add it to the save path**

In `startViewStatePersistence`, extend the change-detection condition (line 57-61) with one clause:

```ts
      state.accountOrder === prev.accountOrder &&
      state.defaultAccountId === prev.defaultAccountId
```

and the PATCH body's `uiState` object (line 72-78) with one field:

```ts
            accountOrder: s.accountOrder,
            defaultAccountId: s.defaultAccountId,
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cd frontend && npx vitest run src/lib/persist-view-state.test.ts`
Expected: PASS (3 tests).

- [ ] **Step 6: Commit**

```bash
git add frontend/src/lib/persist-view-state.ts frontend/src/lib/persist-view-state.test.ts
git commit -m "feat(frontend): persist defaultAccountId in ui_state blob"
```

---

### Task 4: Draft builders carry the source account

**Files:**
- Modify: `frontend/src/lib/compose-draft.ts`
- Modify: `frontend/src/lib/conversation-actions.ts` (lines 148-156)
- Modify: `frontend/src/components/mail/mail-display.tsx` (lines 292-300)

- [ ] **Step 1: `buildReplyDraft` and `buildForwardDraft`**

In `frontend/src/lib/compose-draft.ts`, add `accountId` to both returned objects:

In `buildReplyDraft` (line 46-52):

```ts
  return {
    mode: 'reply',
    accountId: m.accountId,
    to,
    subject: m.subject.startsWith('Re:') ? m.subject : `Re: ${m.subject}`,
    body: quoteBody(m),
    initialHtml: quotedReplyHtml(quoteSource(m), signatureOf(accounts, m.accountId)),
  };
```

In `buildForwardDraft` (line 60-67):

```ts
  return {
    mode: 'forward',
    accountId: m.accountId,
    to: '',
    subject: m.subject.startsWith('Fwd:') ? m.subject : `Fwd: ${m.subject}`,
    body: quoteBody(m),
    initialHtml: forwardHtml(quoteSource(m), signatureOf(accounts, m.accountId)),
    forwardAttachments: forwardAttachments.length > 0 ? forwardAttachments : undefined,
  };
```

- [ ] **Step 2: `editDraftFromList` (context menu path)**

In `frontend/src/lib/conversation-actions.ts` line 148-156, add one field:

```ts
    useUIStore.getState().openCompose({
      mode: 'draft',
      accountId: m.accountId,
      to: m.to.map((a) => a.email).join(', '),
```

- [ ] **Step 3: `handleEditDraft` (reader path)**

In `frontend/src/components/mail/mail-display.tsx` line 292-300, add one field:

```ts
    openCompose({
      mode: 'draft',
      accountId: mail.accountId,
      to: mail.to.map((a) => a.email).join(', '),
```

- [ ] **Step 4: Check + commit**

Run: `cd frontend && npm run check && npx vitest run`
Expected: all green.

```bash
git add frontend/src/lib/compose-draft.ts frontend/src/lib/conversation-actions.ts frontend/src/components/mail/mail-display.tsx
git commit -m "feat(frontend): compose drafts carry the source account id"
```

---

### Task 5: Compose dialog uses `resolveFromAccountId`

**Files:**
- Modify: `frontend/src/components/compose-dialog.tsx` (lines 30, 60, 136-137)

- [ ] **Step 1: Import + read the default account**

Add to the imports (next to line 30, `import { ALL_ACCOUNTS } from '@/lib/mail-api';`):

```ts
import { resolveFromAccountId } from '@/lib/resolve-from-account';
```

After line 60 (`const selectedAccountId = useUIStore((s) => s.selectedAccountId);`):

```ts
  const defaultAccountId = useUIStore((s) => s.defaultAccountId);
```

- [ ] **Step 2: Replace the `effectiveFrom` expression**

Replace lines 136-137:

```ts
    const effectiveFrom =
      selectedAccountId === ALL_ACCOUNTS ? (accounts[0]?.id ?? '') : selectedAccountId;
```

with:

```ts
    const effectiveFrom = resolveFromAccountId({
      draftAccountId: composeDraft?.accountId,
      selectedAccountId,
      defaultAccountId,
      accounts,
    });
```

Nothing else changes — the signature lookup at line 167 already follows `effectiveFrom`.

- [ ] **Step 3: Check + commit**

Run: `cd frontend && npm run check && npx vitest run`
Expected: all green.

```bash
git add frontend/src/components/compose-dialog.tsx
git commit -m "fix(frontend): compose From uses draft source account, then default account"
```

---

### Task 6: Settings → Accounts UI + i18n

**Files:**
- Modify: `frontend/src/components/settings-page.tsx` (line 10 imports; account row at lines 1026-1090)
- Modify: `frontend/src/i18n/en.json`, `frontend/src/i18n/zh.json`

- [ ] **Step 1: i18n strings**

In `frontend/src/i18n/en.json`, inside the existing `settings.accounts` object (alphabetical-ish placement near `add`):

```json
"setDefault": "Set as default",
"defaultBadge": "Default",
```

In `frontend/src/i18n/zh.json`, same location:

```json
"setDefault": "设为默认",
"defaultBadge": "默认",
```

(Both files use flat nested JSON; mirror the exact placement of neighboring keys like `add`.)

- [ ] **Step 2: Store access + icon import**

`settings-page.tsx` already imports `useUIStore` (line 17). Add `Star` to the lucide import on line 10:

```ts
import { Flag, KeyRound, Plus, Shield, SlidersHorizontal, Star, Users, X } from 'lucide-react';
```

Inside the `SettingsPage` component, near the other store hooks (e.g. next to existing `useUIStore` usage), add:

```ts
  const defaultAccountId = useUIStore((s) => s.defaultAccountId);
  const setDefaultAccount = useUIStore((s) => s.setDefaultAccount);
```

- [ ] **Step 3: Star badge on the default row**

In the account meta line (the `flex flex-wrap items-center gap-x-2 …` div at line 1040), after the protocol `<span>` (line 1051), add:

```tsx
                          {defaultAccountId === account.id && (
                            <span className="inline-flex items-center gap-1 text-amber-600 dark:text-amber-400">
                              <Star size={11} className="fill-current" />
                              {t(locale, 'settings.accounts.defaultBadge')}
                            </span>
                          )}
```

- [ ] **Step 4: "Set as default" button**

In the actions div (line 1062), before the Sync button, add:

```tsx
                        {defaultAccountId !== account.id && (
                          <Button
                            variant="ghost"
                            size="sm"
                            onClick={() => setDefaultAccount(account.id)}
                          >
                            {t(locale, 'settings.accounts.setDefault')}
                          </Button>
                        )}
```

Persistence is automatic via the debounced subscription from Task 3.

- [ ] **Step 5: Check + commit**

Run: `cd frontend && npm run check && npx vitest run`
Expected: all green.

```bash
git add frontend/src/components/settings-page.tsx frontend/src/i18n/en.json frontend/src/i18n/zh.json
git commit -m "feat(frontend): set default account from Settings → Accounts"
```

---

### Task 7: Full verification

- [ ] **Step 1: Static gates**

Run: `cd frontend && npm run check && npx vitest run`
Expected: tsc -b, oxlint, prettier all clean; ~99 vitest tests pass (96 previous + new).

- [ ] **Step 2: Backend untouched**

Run: `git diff --stat main -- backend/` (or confirm no backend files in this branch's commits)
Expected: no backend changes.

- [ ] **Step 3: Live browser check (kimi-webbridge session "mail-ux-verify" or a fresh tab)**

The Vite dev server runs in the background (started this session; it proxies `/api` to the Docker backend on :3000). Find its port from the dev-server output (5173, or 5174/5175 if taken — note other projects may occupy ports; use the port the Lyra vite process printed), then in the browser, logged in as cheng:

1. Settings → Accounts: one row shows the amber star + "Default"; other rows show "Set as default". Click it on another account → star moves. Reload the page → star persists (server round-trip).
2. Switch locale to 中文 → the button reads 设为默认, badge reads 默认.
3. Mail → All inboxes → Compose: From = the default account.
4. All inboxes → right-click a message received by a non-default account → Reply: From = the receiving account (not the default, not the first account).

- [ ] **Step 4: Sync the spec if behavior diverged**

If anything shipped differently from `docs/superpowers/specs/2026-09-02-default-account-design.md`, update the spec to match the as-built behavior and commit it with the final commit.
