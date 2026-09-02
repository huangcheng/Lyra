# Mail List Actions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add drag-conversation-to-folder, a right-click context menu on mail list rows, and a sync-all-accounts button — frontend only, reusing existing `/api/v1` endpoints.

**Architecture:** Shared batch-action helpers in `frontend/src/lib/conversation-actions.ts` (sequential per-message API loops + local store updates). One dnd-kit `DndContext` hoisted to `mail.tsx` serving both conversation drags (draggable list rows → droppable folder rows) and the existing account reorder (its inner `DndContext` is removed; `SortableContext` stays). Radix `ContextMenu` (already available via the `radix-ui` monolith) wrapped in a new `ui/context-menu.tsx` shadcn port. Sync-all button loops the existing per-account `POST /accounts/{id}/sync`.

**Tech Stack:** React 19, dnd-kit (`@dnd-kit/core` + `sortable`, already installed), radix-ui monolith (includes `ContextMenu`), Zustand stores, vitest.

**Spec:** `docs/superpowers/specs/2026-09-02-mail-list-actions-design.md`

**Key existing code (read before editing):**
- `frontend/src/components/mail/mail-list.tsx` — conversation rows (lines 314-436), quick actions (305-313), `fetchError`/`ErrorBanner` pattern (117-133, 255-268).
- `frontend/src/components/mail/mail-display.tsx` — `replyToMessage` (238-256), `forwardMessage` (271-292), `handleMoveToFolder` (351-367), `handleSnooze` (369-385), `handlePatch` (387-406), `quoteBody` (61-72).
- `frontend/src/components/mail/sidebar-folders.tsx` — `UnifiedRow` (80), `CustomFolderBranch` (99), `AccountSection` role rows (238-306), `SortableAccountSections` with the inner `DndContext` to remove (355-390).
- `frontend/src/components/mail/mail.tsx` — `SyncStatusDot` (32-51), `NavContent` footer (100-106), desktop/mobile returns (232-289).
- `frontend/src/stores/mail.ts` — `removeMessage`, `markMessageRead`, `toggleStar`, `upsertMessage`.
- `frontend/src/stores/ui.ts` — `openCompose(draft)`, `ComposeDraft` type.
- `frontend/src/lib/compose-html.ts` — `quotedReplyHtml`, `forwardHtml`.
- `frontend/src/lib/mail-api.ts` — `api` (via `@/lib/api-client`), `mapApiMessage`, `ApiMessage`.
- `frontend/src/components/ui/dropdown-menu.tsx` — class conventions to mirror.
- `frontend/src/lib/persist-view-state.test.ts` — vitest `vi.mock('@/lib/api-client')` pattern.
- i18n: `frontend/src/i18n/en.json` + `zh.json`; existing keys to reuse: `mail.reply`, `mail.replyAll`, `mail.forward`, `mail.archive`, `mail.moveToJunk`, `mail.moveToTrash`, `mail.moveToFolder`, `mail.noFolders`, `mail.markRead`, `mail.markUnread`, `mail.star`, `mail.unstar`, `mail.snooze`, `mail.laterToday`, `mail.tomorrow`, `mail.thisWeekend`, `mail.nextWeek`, `mail.editDraft`.

---

### Task 1: `lib/compose-draft.ts` — shared compose-draft builders

Extract the reply/forward draft construction from `mail-display.tsx` into a tested lib module, so both the reader and the list context menu use it.

**Files:**
- Create: `frontend/src/lib/compose-draft.ts`
- Test: `frontend/src/lib/compose-draft.test.ts`
- Modify: `frontend/src/components/mail/mail-display.tsx` (delete local `quoteBody` at 61-72; replace `replyToMessage`/`forwardMessage` bodies to call the new builders)

- [ ] **Step 1: Write the failing test**

`frontend/src/lib/compose-draft.test.ts`:

```ts
import { describe, expect, it } from 'vitest';

import { buildForwardDraft, buildReplyDraft, quoteBody } from '@/lib/compose-draft';
import type { MailAccount, MailMessage } from '@/types';

const accounts: MailAccount[] = [
  {
    id: 'acc1',
    displayName: 'Work',
    emailAddress: 'me@work.example',
    protocol: 'imap',
    isActive: true,
    signature: 'Cheers,\nMe',
    syncEnabled: true,
  },
];

function msg(over: Partial<MailMessage> = {}): MailMessage {
  return {
    id: 'm1',
    accountId: 'acc1',
    folderId: 'f1',
    subject: 'Hello',
    from: { name: 'Alice', email: 'alice@example.com' },
    to: [{ email: 'me@work.example' }],
    date: '2026-09-01T10:00:00Z',
    snippet: 'hi',
    bodyText: 'plain body',
    isRead: true,
    isStarred: false,
    isDraft: false,
    hasAttachments: false,
    ...over,
  };
}

describe('quoteBody', () => {
  it('quotes bodyText with > prefixes', () => {
    const out = quoteBody(msg({ bodyText: 'a\nb' }));
    expect(out).toContain('> a\n> b');
    expect(out).toContain('alice@example.com wrote:');
  });

  it('falls back to the snippet when no bodyText', () => {
    const out = quoteBody(msg({ bodyText: undefined, snippet: 'snip' }));
    expect(out).toContain('> snip');
  });
});

describe('buildReplyDraft', () => {
  it('replies to the sender and prefixes Re:', () => {
    const d = buildReplyDraft(msg(), false, accounts);
    expect(d.mode).toBe('reply');
    expect(d.to).toBe('alice@example.com');
    expect(d.subject).toBe('Re: Hello');
    expect(d.initialHtml).toContain('plain body');
    expect(d.initialHtml).toContain('Cheers'); // signature above the quote
  });

  it('does not double-prefix Re:', () => {
    expect(buildReplyDraft(msg({ subject: 'Re: Hello' }), false, accounts).subject).toBe(
      'Re: Hello',
    );
  });

  it('reply-all includes original to recipients', () => {
    const d = buildReplyDraft(msg(), true, accounts);
    expect(d.to).toBe('alice@example.com, me@work.example');
  });
});

describe('buildForwardDraft', () => {
  it('prefixes Fwd: and carries non-inline attachments', () => {
    const d = buildForwardDraft(
      msg({
        attachments: [
          { id: 'a1', filename: 'x.pdf', isInline: false },
          { id: 'a2', filename: 'logo.png', isInline: true },
        ],
      }),
      accounts,
    );
    expect(d.mode).toBe('forward');
    expect(d.subject).toBe('Fwd: Hello');
    expect(d.forwardAttachments).toEqual([{ id: 'a1', filename: 'x.pdf', contentType: undefined }]);
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd frontend && npx vitest run src/lib/compose-draft.test.ts`
Expected: FAIL — module `@/lib/compose-draft` does not exist.

- [ ] **Step 3: Implement `compose-draft.ts`**

`frontend/src/lib/compose-draft.ts`:

```ts
/**
 * Compose-draft builders shared by the reading pane and the list context
 * menu. Pure functions: given a message (+ accounts for the signature),
 * produce the Partial<ComposeDraft> for `useUIStore.openCompose`.
 */

import { forwardHtml, quotedReplyHtml } from '@/lib/compose-html';
import { sanitizeEmailHtml } from '@/lib/sanitize-email-html';
import type { ComposeDraft } from '@/stores/ui';
import type { MailAccount, MailMessage } from '@/types';

/** Plain-text fallback quote used as ComposeDraft.body. */
export function quoteBody(
  message: Pick<MailMessage, 'from' | 'date' | 'snippet'> & { bodyText?: string },
): string {
  const quoted = (message.bodyText ?? message.snippet)
    .split('\n')
    .map((line) => `> ${line}`)
    .join('\n');
  return `\n\nOn ${message.date}, ${message.from.email} wrote:\n${quoted}`;
}

function signatureOf(accounts: MailAccount[], accountId: string): string | undefined {
  return accounts.find((a) => a.id === accountId)?.signature ?? undefined;
}

function quoteSource(m: MailMessage) {
  return {
    fromName: m.from.name ?? '',
    fromEmail: m.from.email,
    date: m.date,
    bodyHtml: m.bodyHtml ? sanitizeEmailHtml(m.bodyHtml) : undefined,
    bodyText: m.bodyText,
  };
}

/** Reply draft; `all` adds the original To recipients. */
export function buildReplyDraft(
  m: MailMessage,
  all: boolean,
  accounts: MailAccount[],
): Partial<ComposeDraft> {
  const to = all
    ? [m.from.email, ...m.to.map((a) => a.email)].filter(Boolean).join(', ')
    : m.from.email;
  return {
    mode: 'reply',
    to,
    subject: m.subject.startsWith('Re:') ? m.subject : `Re: ${m.subject}`,
    body: quoteBody(m),
    initialHtml: quotedReplyHtml(quoteSource(m), signatureOf(accounts, m.accountId)),
  };
}

/** Forward draft; carries the original's non-inline attachment metadata. */
export function buildForwardDraft(
  m: MailMessage,
  accounts: MailAccount[],
): Partial<ComposeDraft> {
  const forwardAttachments = (m.attachments ?? [])
    .filter((a) => !a.isInline)
    .map((a) => ({ id: a.id, filename: a.filename, contentType: a.contentType }));
  return {
    mode: 'forward',
    to: '',
    subject: m.subject.startsWith('Fwd:') ? m.subject : `Fwd: ${m.subject}`,
    body: quoteBody(m),
    initialHtml: forwardHtml(quoteSource(m), signatureOf(accounts, m.accountId)),
    forwardAttachments: forwardAttachments.length > 0 ? forwardAttachments : undefined,
  };
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd frontend && npx vitest run src/lib/compose-draft.test.ts`
Expected: PASS (6 tests).

- [ ] **Step 5: Refactor `mail-display.tsx` to use the builders**

Delete the local `quoteBody` function (lines 61-72) and replace the `replyToMessage` and `forwardMessage` bodies:

```ts
  const replyToMessage = (m: MailMessage, all: boolean) => {
    openCompose(buildReplyDraft(m, all, accounts));
  };
```

```ts
  const forwardMessage = (m: MailMessage) => {
    openCompose(buildForwardDraft(m, accounts));
  };
```

Add the import, remove now-unused ones (`forwardHtml`, `quotedReplyHtml`, `sanitizeEmailHtml` — check nothing else in the file still uses them; `textToHtml` stays for `handleEditDraft`):

```ts
import { buildForwardDraft, buildReplyDraft } from '@/lib/compose-draft';
```

- [ ] **Step 6: Verify + commit**

Run: `cd frontend && npx tsc --noEmit && npx vitest run src/lib/compose-draft.test.ts src/lib/compose-html.test.ts`
Expected: clean typecheck, tests PASS. Then manually confirm the reading pane still replies/forwards correctly if a dev server is running (optional here; full manual pass is Task 7).

```bash
git add frontend/src/lib/compose-draft.ts frontend/src/lib/compose-draft.test.ts frontend/src/components/mail/mail-display.tsx
git commit -m "refactor(frontend): extract compose-draft builders for reply/forward"
```

---

### Task 2: `lib/conversation-actions.ts` — batch conversation actions

Sequential per-message API loops with local store updates, shared by the context menu and drag-and-drop. Sequential (not `Promise.all`) so a failure stops the loop and already-applied changes stay consistent with the server.

**Files:**
- Create: `frontend/src/lib/conversation-actions.ts`
- Test: `frontend/src/lib/conversation-actions.test.ts`

- [ ] **Step 1: Write the failing test**

`frontend/src/lib/conversation-actions.test.ts`:

```ts
import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@/lib/api-client', () => ({ api: vi.fn().mockResolvedValue({}) }));

import { api } from '@/lib/api-client';
import {
  actOnMessages,
  canDropConversation,
  moveMessages,
  patchMessages,
} from '@/lib/conversation-actions';
import { useMailStore } from '@/stores/mail';
import { useUIStore } from '@/stores/ui';
import type { MailMessage } from '@/types';

const mockedApi = vi.mocked(api);

function msg(id: string, over: Partial<MailMessage> = {}): MailMessage {
  return {
    id,
    accountId: 'acc1',
    folderId: 'f1',
    subject: `S${id}`,
    from: { email: 'a@example.com' },
    to: [],
    date: '2026-09-01T10:00:00Z',
    snippet: '',
    isRead: false,
    isStarred: false,
    isDraft: false,
    hasAttachments: false,
    ...over,
  };
}

beforeEach(() => {
  mockedApi.mockClear();
  mockedApi.mockResolvedValue({});
  useMailStore.setState({
    messages: { m1: msg('m1'), m2: msg('m2'), m3: msg('m3') },
  });
  useUIStore.setState({ selectedMessageId: null });
});

describe('moveMessages', () => {
  it('moves each message and removes it locally', async () => {
    const res = await moveMessages(['m1', 'm2'], 'f2');
    expect(res.error).toBeNull();
    expect(res.done).toEqual(['m1', 'm2']);
    expect(mockedApi).toHaveBeenCalledTimes(2);
    expect(mockedApi).toHaveBeenCalledWith('/messages/m1/move', {
      method: 'POST',
      body: JSON.stringify({ folderId: 'f2' }),
    });
    expect(useMailStore.getState().messages.m1).toBeUndefined();
    expect(useMailStore.getState().messages.m3).toBeDefined();
  });

  it('stops at the first failure and reports it', async () => {
    mockedApi.mockResolvedValueOnce({}).mockRejectedValueOnce(new Error('IMAP MOVE failed'));
    const res = await moveMessages(['m1', 'm2', 'm3'], 'f2');
    expect(res.done).toEqual(['m1']);
    expect(res.error).toBe('IMAP MOVE failed');
    expect(mockedApi).toHaveBeenCalledTimes(2);
    expect(useMailStore.getState().messages.m2).toBeDefined();
  });

  it('clears the selection when the selected message is moved', async () => {
    useUIStore.setState({ selectedMessageId: 'm1' });
    await moveMessages(['m1'], 'f2');
    expect(useUIStore.getState().selectedMessageId).toBeNull();
  });
});

describe('actOnMessages', () => {
  it('posts the action per message and removes locally', async () => {
    const res = await actOnMessages(['m1', 'm2'], 'archive');
    expect(res.error).toBeNull();
    expect(mockedApi).toHaveBeenCalledWith('/messages/m2/archive', { method: 'POST' });
    expect(useMailStore.getState().messages.m1).toBeUndefined();
  });
});

describe('patchMessages', () => {
  it('marks read locally after the patch', async () => {
    await patchMessages(['m1'], { isRead: true });
    expect(mockedApi).toHaveBeenCalledWith('/messages/m1', {
      method: 'PATCH',
      body: JSON.stringify({ isRead: true }),
    });
    expect(useMailStore.getState().messages.m1.isRead).toBe(true);
  });

  it('only toggles star when the local state differs', async () => {
    useMailStore.setState({ messages: { m1: msg('m1', { isStarred: true }) } });
    await patchMessages(['m1'], { isStarred: true });
    expect(useMailStore.getState().messages.m1.isStarred).toBe(true);
  });
});

describe('canDropConversation', () => {
  const drag = { accountId: 'acc1', folderIds: ['f1'] };
  it('rejects cross-account drops', () => {
    expect(canDropConversation(drag, { accountId: 'acc2', folderId: 'f9' })).toBe(false);
  });
  it('rejects dropping into the current folder', () => {
    expect(canDropConversation(drag, { accountId: 'acc1', folderId: 'f1' })).toBe(false);
  });
  it('accepts a same-account different folder', () => {
    expect(canDropConversation(drag, { accountId: 'acc1', folderId: 'f2' })).toBe(true);
  });
});
```

Note: tests seed `useMailStore.setState` with a partial — cast as needed (`as never` or the store's state type) to satisfy TS; look at an existing store-touching test for the local convention.

- [ ] **Step 2: Run test to verify it fails**

Run: `cd frontend && npx vitest run src/lib/conversation-actions.test.ts`
Expected: FAIL — module does not exist.

- [ ] **Step 3: Implement `conversation-actions.ts`**

`frontend/src/lib/conversation-actions.ts`:

```ts
/**
 * Batch actions over every message of a conversation row.
 *
 * The API is per-message, so each helper loops sequentially: a failure
 * stops the loop, keeping local state consistent with the server (the
 * messages in `done` were applied; the rest were not).
 */

import { api } from '@/lib/api-client';
import { mapApiMessage, type ApiMessage } from '@/lib/mail-api';
import { textToHtml } from '@/lib/compose-html';
import { buildForwardDraft, buildReplyDraft } from '@/lib/compose-draft';
import { useMailStore } from '@/stores/mail';
import { useUIStore } from '@/stores/ui';
import type { MailMessage } from '@/types';

export interface BatchResult {
  /** Message ids successfully processed, in order. */
  done: string[];
  /** First error message, or null when everything succeeded. */
  error: string | null;
}

async function runBatch(
  messageIds: string[],
  fn: (id: string) => Promise<void>,
  onProgress?: (doneCount: number) => void,
): Promise<BatchResult> {
  const done: string[] = [];
  for (const id of messageIds) {
    try {
      await fn(id);
    } catch (err) {
      return { done, error: err instanceof Error ? err.message : String(err) };
    }
    done.push(id);
    onProgress?.(done.length);
  }
  return { done, error: null };
}

/** Remove locally + clear the reader selection if it pointed at this message. */
function removeLocally(id: string) {
  useMailStore.getState().removeMessage(id);
  if (useUIStore.getState().selectedMessageId === id) {
    useUIStore.getState().setSelectedMessage(null);
  }
}

/** Move messages to a folder (same account only — validated by callers/server). */
export function moveMessages(
  messageIds: string[],
  folderId: string,
  onProgress?: (doneCount: number) => void,
): Promise<BatchResult> {
  return runBatch(
    messageIds,
    async (id) => {
      await api(`/messages/${id}/move`, { method: 'POST', body: JSON.stringify({ folderId }) });
      removeLocally(id);
    },
    onProgress,
  );
}

/** Archive / spam / trash every message. */
export function actOnMessages(
  messageIds: string[],
  action: 'archive' | 'spam' | 'trash',
): Promise<BatchResult> {
  return runBatch(messageIds, async (id) => {
    await api(`/messages/${id}/${action}`, { method: 'POST' });
    removeLocally(id);
  });
}

/** Patch flags (isRead / isStarred) on every message. */
export function patchMessages(
  messageIds: string[],
  patch: { isRead?: boolean; isStarred?: boolean },
): Promise<BatchResult> {
  return runBatch(messageIds, async (id) => {
    await api(`/messages/${id}`, { method: 'PATCH', body: JSON.stringify(patch) });
    const store = useMailStore.getState();
    const m = store.messages[id];
    if (!m) return;
    if (patch.isRead === true) store.markMessageRead(id);
    if (patch.isRead === false) store.upsertMessage({ ...m, isRead: false });
    if (patch.isStarred !== undefined && m.isStarred !== patch.isStarred) store.toggleStar(id);
  });
}

/** Snooze every message until the given time. */
export function snoozeMessages(messageIds: string[], until: Date): Promise<BatchResult> {
  return runBatch(messageIds, async (id) => {
    await api(`/messages/${id}/snooze`, {
      method: 'POST',
      body: JSON.stringify({ until: until.toISOString() }),
    });
    removeLocally(id);
  });
}

/** Fetch the full message (body) into the store if we only have list data. Throws on fetch failure. */
export async function ensureFullMessage(id: string): Promise<MailMessage> {
  const store = useMailStore.getState();
  const cached = store.messages[id];
  if (cached && (cached.bodyHtml != null || cached.bodyText != null)) return cached;
  const raw = await api<ApiMessage>(`/messages/${id}`);
  const full = mapApiMessage(raw);
  useMailStore.getState().upsertMessage(full);
  return full;
}

function errorMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

/** Right-click reply: select the message, then open the composer. Returns an error message on failure. */
export async function replyFromList(id: string, all: boolean): Promise<string | null> {
  try {
    const m = await ensureFullMessage(id);
    useUIStore.getState().setSelectedMessage(id);
    useUIStore.getState().openCompose(buildReplyDraft(m, all, useMailStore.getState().accounts));
    return null;
  } catch (err) {
    return errorMessage(err);
  }
}

/** Right-click forward: select the message, then open the composer. Returns an error message on failure. */
export async function forwardFromList(id: string): Promise<string | null> {
  try {
    const m = await ensureFullMessage(id);
    useUIStore.getState().setSelectedMessage(id);
    useUIStore.getState().openCompose(buildForwardDraft(m, useMailStore.getState().accounts));
    return null;
  } catch (err) {
    return errorMessage(err);
  }
}

/** Open an existing draft for editing (mirrors the reader's Edit draft). Returns an error message on failure. */
export async function editDraftFromList(id: string): Promise<string | null> {
  try {
    const m = await ensureFullMessage(id);
    useUIStore.getState().setSelectedMessage(id);
    useUIStore.getState().openCompose({
      mode: 'draft',
      to: m.to.map((a) => a.email).join(', '),
      cc: (m.cc ?? []).map((a) => a.email).join(', '),
      subject: m.subject ?? '',
      body: m.bodyText ?? '',
      initialHtml: m.bodyHtml ?? textToHtml(m.bodyText ?? ''),
      draftMessageId: m.id,
    });
    return null;
  } catch (err) {
    return errorMessage(err);
  }
}

/** Drag payload for a conversation row (dnd-kit `data.current`). */
export interface ConversationDragData {
  type: 'conversation';
  accountId: string;
  messageIds: string[];
  /** Distinct folders the messages currently live in. */
  folderIds: string[];
  subject: string;
  count: number;
}

/** Drop validation: same account, not already in the target folder. */
export function canDropConversation(
  drag: Pick<ConversationDragData, 'accountId' | 'folderIds'>,
  target: { accountId: string; folderId: string },
): boolean {
  return drag.accountId === target.accountId && !drag.folderIds.includes(target.folderId);
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd frontend && npx vitest run src/lib/conversation-actions.test.ts`
Expected: PASS.

- [ ] **Step 5: Typecheck + commit**

Run: `cd frontend && npx tsc --noEmit`
Expected: clean.

```bash
git add frontend/src/lib/conversation-actions.ts frontend/src/lib/conversation-actions.test.ts
git commit -m "feat(frontend): batch conversation action helpers (move/flag/snooze/reply)"
```

---

### Task 3: `ui/context-menu.tsx` shadcn port

The `radix-ui` monolith (package.json `radix-ui@^1.6.7`) already includes `ContextMenu`; only the styled wrapper is missing. Port shadcn's context-menu, mirroring the class conventions of the existing `ui/dropdown-menu.tsx`.

**Files:**
- Create: `frontend/src/components/ui/context-menu.tsx`

- [ ] **Step 1: Create the component**

`frontend/src/components/ui/context-menu.tsx`:

```tsx
'use client';

import * as React from 'react';
import { CheckIcon, ChevronRightIcon } from 'lucide-react';
import { ContextMenu as ContextMenuPrimitive } from 'radix-ui';

import { cn } from '@/lib/utils';

function ContextMenu({ ...props }: React.ComponentProps<typeof ContextMenuPrimitive.Root>) {
  return <ContextMenuPrimitive.Root data-slot="context-menu" {...props} />;
}

function ContextMenuTrigger({
  ...props
}: React.ComponentProps<typeof ContextMenuPrimitive.Trigger>) {
  return <ContextMenuPrimitive.Trigger data-slot="context-menu-trigger" {...props} />;
}

function ContextMenuGroup({ ...props }: React.ComponentProps<typeof ContextMenuPrimitive.Group>) {
  return <ContextMenuPrimitive.Group data-slot="context-menu-group" {...props} />;
}

function ContextMenuPortal({ ...props }: React.ComponentProps<typeof ContextMenuPrimitive.Portal>) {
  return <ContextMenuPrimitive.Portal data-slot="context-menu-portal" {...props} />;
}

function ContextMenuSub({ ...props }: React.ComponentProps<typeof ContextMenuPrimitive.Sub>) {
  return <ContextMenuPrimitive.Sub data-slot="context-menu-sub" {...props} />;
}

function ContextMenuContent({
  className,
  ...props
}: React.ComponentProps<typeof ContextMenuPrimitive.Content>) {
  return (
    <ContextMenuPrimitive.Portal>
      <ContextMenuPrimitive.Content
        data-slot="context-menu-content"
        className={cn(
          'z-50 max-h-(--radix-context-menu-content-available-height) min-w-[8rem] origin-(--radix-context-menu-content-transform-origin) overflow-x-hidden overflow-y-auto rounded-md border bg-popover p-1 text-popover-foreground shadow-md data-[side=bottom]:slide-in-from-top-2 data-[side=left]:slide-in-from-right-2 data-[side=right]:slide-in-from-left-2 data-[side=top]:slide-in-from-bottom-2 data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=closed]:zoom-out-95 data-[state=open]:animate-in data-[state=open]:fade-in-0 data-[state=open]:zoom-in-95',
          className,
        )}
        {...props}
      />
    </ContextMenuPrimitive.Portal>
  );
}

function ContextMenuItem({
  className,
  inset,
  variant = 'default',
  ...props
}: React.ComponentProps<typeof ContextMenuPrimitive.Item> & {
  inset?: boolean;
  variant?: 'default' | 'destructive';
}) {
  return (
    <ContextMenuPrimitive.Item
      data-slot="context-menu-item"
      data-inset={inset}
      data-variant={variant}
      className={cn(
        "relative flex cursor-default items-center gap-2 rounded-sm px-2 py-1.5 text-sm outline-hidden select-none focus:bg-accent focus:text-accent-foreground data-[disabled]:pointer-events-none data-[disabled]:opacity-50 data-[inset]:pl-8 data-[variant=destructive]:text-destructive data-[variant=destructive]:focus:bg-destructive/10 data-[variant=destructive]:focus:text-destructive [&_svg]:pointer-events-none [&_svg]:shrink-0 [&_svg:not([class*='size-'])]:size-4 [&_svg:not([class*='text-'])]:text-muted-foreground data-[variant=destructive]:*:[svg]:text-destructive!",
        className,
      )}
      {...props}
    />
  );
}

function ContextMenuLabel({
  className,
  inset,
  ...props
}: React.ComponentProps<typeof ContextMenuPrimitive.Label> & { inset?: boolean }) {
  return (
    <ContextMenuPrimitive.Label
      data-slot="context-menu-label"
      data-inset={inset}
      className={cn('px-2 py-1.5 text-sm font-medium data-[inset]:pl-8', className)}
      {...props}
    />
  );
}

function ContextMenuSeparator({
  className,
  ...props
}: React.ComponentProps<typeof ContextMenuPrimitive.Separator>) {
  return (
    <ContextMenuPrimitive.Separator
      data-slot="context-menu-separator"
      className={cn('-mx-1 my-1 h-px bg-border', className)}
      {...props}
    />
  );
}

function ContextMenuShortcut({ className, ...props }: React.ComponentProps<'span'>) {
  return (
    <span
      data-slot="context-menu-shortcut"
      className={cn('ml-auto text-xs tracking-widest text-muted-foreground', className)}
      {...props}
    />
  );
}

function ContextMenuSubTrigger({
  className,
  inset,
  children,
  ...props
}: React.ComponentProps<typeof ContextMenuPrimitive.SubTrigger> & { inset?: boolean }) {
  return (
    <ContextMenuPrimitive.SubTrigger
      data-slot="context-menu-sub-trigger"
      data-inset={inset}
      className={cn(
        "flex cursor-default items-center gap-2 rounded-sm px-2 py-1.5 text-sm outline-hidden select-none focus:bg-accent focus:text-accent-foreground data-[state=open]:bg-accent data-[state=open]:text-accent-foreground data-[inset]:pl-8 [&_svg]:pointer-events-none [&_svg]:shrink-0 [&_svg:not([class*='size-'])]:size-4 [&_svg:not([class*='text-'])]:text-muted-foreground",
        className,
      )}
      {...props}
    >
      {children}
      <ChevronRightIcon className="ml-auto size-4" />
    </ContextMenuPrimitive.SubTrigger>
  );
}

function ContextMenuSubContent({
  className,
  ...props
}: React.ComponentProps<typeof ContextMenuPrimitive.SubContent>) {
  return (
    <ContextMenuPrimitive.SubContent
      data-slot="context-menu-sub-content"
      className={cn(
        'z-50 min-w-[8rem] origin-(--radix-context-menu-content-transform-origin) overflow-hidden rounded-md border bg-popover p-1 text-popover-foreground shadow-lg data-[side=bottom]:slide-in-from-top-2 data-[side=left]:slide-in-from-right-2 data-[side=right]:slide-in-from-left-2 data-[side=top]:slide-in-from-bottom-2 data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=closed]:zoom-out-95 data-[state=open]:animate-in data-[state=open]:fade-in-0 data-[state=open]:zoom-in-95',
        className,
      )}
      {...props}
    />
  );
}

export {
  ContextMenu,
  ContextMenuTrigger,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuLabel,
  ContextMenuSeparator,
  ContextMenuShortcut,
  ContextMenuGroup,
  ContextMenuPortal,
  ContextMenuSub,
  ContextMenuSubTrigger,
  ContextMenuSubContent,
};
```

(`CheckIcon` import is unused — drop it; oxlint runs with `-D warnings`-equivalent strictness.)

- [ ] **Step 2: Verify + commit**

Run: `cd frontend && npx tsc --noEmit && npx oxlint src/components/ui/context-menu.tsx`
Expected: clean.

```bash
git add frontend/src/components/ui/context-menu.tsx
git commit -m "feat(frontend): add shadcn context-menu primitive"
```

---

### Task 4: Conversation context menu + mail-list wiring

**Files:**
- Create: `frontend/src/components/mail/conversation-context-menu.tsx`
- Modify: `frontend/src/components/mail/mail-list.tsx`
- Modify: `frontend/src/i18n/en.json`, `frontend/src/i18n/zh.json`

- [ ] **Step 1: Add i18n keys**

In `frontend/src/i18n/en.json`, under `mail`:

```json
    "filterFolders": "Filter folders",
    "syncAllAccounts": "Sync all accounts",
    "syncStartFailed": "Couldn't start sync",
    "movingMessages": "Moving {{done}}/{{total}}…",
```

In `frontend/src/i18n/zh.json`, under `mail` (same position relative to neighbors as en.json):

```json
    "filterFolders": "筛选文件夹",
    "syncAllAccounts": "同步所有账户",
    "syncStartFailed": "无法开始同步",
    "movingMessages": "正在移动 {{done}}/{{total}}…",
```

Run: `cd frontend && npx vitest run src/i18n/i18n.test.ts`
Expected: PASS (key parity between en/zh is tested there).

- [ ] **Step 2: Create `conversation-context-menu.tsx`**

`frontend/src/components/mail/conversation-context-menu.tsx`:

```tsx
/**
 * Right-click menu for a conversation row in the mail list.
 *
 * Every action loops the whole conversation via `lib/conversation-actions`;
 * Reply/Reply All/Forward (and Edit draft) target the latest message only.
 */

import { addDays, addHours, format, nextSaturday } from 'date-fns';
import {
  Archive,
  ArchiveX,
  BellOff,
  Check,
  Clock,
  FolderInput,
  Forward,
  MailOpen,
  Mail,
  PenSquare,
  Reply,
  ReplyAll,
  Star,
  StarOff,
  Trash2,
} from 'lucide-react';
import { useEffect, useRef, useState, type ReactNode } from 'react';

import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuLabel,
  ContextMenuSeparator,
  ContextMenuSub,
  ContextMenuSubContent,
  ContextMenuSubTrigger,
  ContextMenuTrigger,
} from '@/components/ui/context-menu';
import { t } from '@/i18n';
import {
  actOnMessages,
  editDraftFromList,
  forwardFromList,
  moveMessages,
  patchMessages,
  replyFromList,
  snoozeMessages,
} from '@/lib/conversation-actions';
import type { Conversation } from '@/lib/conversation';
import { useMailStore } from '@/stores/mail';
import { useUIStore } from '@/stores/ui';

/** Filter input that focuses itself on mount (i.e. when the submenu opens).
 *  Radix omits `onOpenAutoFocus` from SubContent props, and its own mount
 *  focus runs in a parent effect — so we focus in a rAF after it. */
function FilterInput({
  value,
  onChange,
  placeholder,
}: {
  value: string;
  onChange: (value: string) => void;
  placeholder: string;
}) {
  const ref = useRef<HTMLInputElement>(null);
  useEffect(() => {
    const raf = requestAnimationFrame(() => ref.current?.focus());
    return () => cancelAnimationFrame(raf);
  }, []);
  return (
    <input
      ref={ref}
      value={value}
      onChange={(e) => onChange(e.target.value)}
      onKeyDown={(e) => e.stopPropagation()}
      placeholder={placeholder}
      className="h-8 w-full rounded-md border border-input bg-transparent px-2 text-sm outline-none focus:border-ring"
    />
  );
}

/** Move-to submenu with a folder-name filter (Fastmail/Yandex pattern). */
function MoveToSub({
  convo,
  onMove,
}: {
  convo: Conversation;
  onMove: (folderId: string) => void;
}) {
  const locale = useUIStore((s) => s.locale);
  const folders = useMailStore((s) => s.folders);
  const [query, setQuery] = useState('');
  const accountFolders = Object.values(folders)
    .filter((f) => f.accountId === convo.latest.accountId)
    .sort((a, b) => a.sortOrder - b.sortOrder || a.name.localeCompare(b.name));
  const q = query.trim().toLowerCase();
  const shown = q
    ? accountFolders.filter((f) => f.name.toLowerCase().includes(q))
    : accountFolders;
  const currentFolderIds = new Set(convo.messages.map((m) => m.folderId));

  return (
    <ContextMenuSub>
      <ContextMenuSubTrigger>
        <FolderInput />
        {t(locale, 'mail.moveToFolder')}
      </ContextMenuSubTrigger>
      <ContextMenuSubContent className="w-56">
        <div className="px-1 pb-1">
          <FilterInput
            value={query}
            onChange={setQuery}
            placeholder={t(locale, 'mail.filterFolders')}
          />
        </div>
        <div className="max-h-64 overflow-y-auto">
          {shown.length === 0 ? (
            <ContextMenuLabel>{t(locale, 'mail.noFolders')}</ContextMenuLabel>
          ) : (
            shown.map((f) => (
              <ContextMenuItem
                key={f.id}
                disabled={currentFolderIds.has(f.id)}
                onSelect={() => onMove(f.id)}
              >
                <span className="truncate">{f.name}</span>
                {currentFolderIds.has(f.id) ? <Check className="ml-auto" /> : null}
              </ContextMenuItem>
            ))
          )}
        </div>
      </ContextMenuSubContent>
    </ContextMenuSub>
  );
}

export function ConversationContextMenu({
  convo,
  onActionError,
  children,
}: {
  convo: Conversation;
  /** Surface a batch failure in the list's error line. */
  onActionError: (message: string) => void;
  children: ReactNode;
}) {
  const locale = useUIStore((s) => s.locale);
  const latest = convo.latest;
  const ids = convo.messages.map((m) => m.id);
  const today = new Date();

  const report = (error: string | null) => {
    if (error) onActionError(error);
  };
  const run = (p: Promise<{ error: string | null }>) => void p.then((r) => report(r.error));

  const snoozeOptions: Array<{ key: string; until: Date }> = [
    { key: 'mail.laterToday', until: addHours(today, 4) },
    { key: 'mail.tomorrow', until: addDays(today, 1) },
    { key: 'mail.thisWeekend', until: nextSaturday(today) },
    { key: 'mail.nextWeek', until: addDays(today, 7) },
  ];

  return (
    <ContextMenu>
      <ContextMenuTrigger asChild>{children}</ContextMenuTrigger>
      <ContextMenuContent className="w-56">
        {latest.isDraft ? (
          <ContextMenuItem onSelect={() => void editDraftFromList(latest.id).then(report)}>
            <PenSquare />
            {t(locale, 'mail.editDraft')}
          </ContextMenuItem>
        ) : (
          <>
            <ContextMenuItem onSelect={() => void replyFromList(latest.id, false).then(report)}>
              <Reply />
              {t(locale, 'mail.reply')}
            </ContextMenuItem>
            <ContextMenuItem onSelect={() => void replyFromList(latest.id, true).then(report)}>
              <ReplyAll />
              {t(locale, 'mail.replyAll')}
            </ContextMenuItem>
            <ContextMenuItem onSelect={() => void forwardFromList(latest.id).then(report)}>
              <Forward />
              {t(locale, 'mail.forward')}
            </ContextMenuItem>
          </>
        )}
        <ContextMenuSeparator />
        <ContextMenuItem onSelect={() => run(actOnMessages(ids, 'archive'))}>
          <Archive />
          {t(locale, 'mail.archive')}
        </ContextMenuItem>
        <ContextMenuItem onSelect={() => run(actOnMessages(ids, 'spam'))}>
          <ArchiveX />
          {t(locale, 'mail.moveToJunk')}
        </ContextMenuItem>
        <ContextMenuItem variant="destructive" onSelect={() => run(actOnMessages(ids, 'trash'))}>
          <Trash2 />
          {t(locale, 'mail.moveToTrash')}
        </ContextMenuItem>
        <MoveToSub convo={convo} onMove={(folderId) => run(moveMessages(ids, folderId))} />
        <ContextMenuSeparator />
        {convo.unreadCount > 0 ? (
          <ContextMenuItem onSelect={() => run(patchMessages(ids, { isRead: true }))}>
            <MailOpen />
            {t(locale, 'mail.markRead')}
          </ContextMenuItem>
        ) : (
          <ContextMenuItem onSelect={() => run(patchMessages(ids, { isRead: false }))}>
            <Mail />
            {t(locale, 'mail.markUnread')}
          </ContextMenuItem>
        )}
        <ContextMenuItem
          onSelect={() => run(patchMessages(ids, { isStarred: !convo.anyStarred }))}
        >
          {convo.anyStarred ? <StarOff /> : <Star />}
          {t(locale, convo.anyStarred ? 'mail.unstar' : 'mail.star')}
        </ContextMenuItem>
        <ContextMenuItem
          onSelect={() => {
            // Session-local mute, same store the reader's overflow menu uses.
            const ui = useUIStore.getState();
            for (const id of ids) {
              if (!ui.mutedMessageIds.includes(id)) ui.toggleMuteMessage(id);
            }
          }}
        >
          <BellOff />
          {t(locale, 'mail.muteThread')}
        </ContextMenuItem>
        <ContextMenuSub>
          <ContextMenuSubTrigger>
            <Clock />
            {t(locale, 'mail.snooze')}
          </ContextMenuSubTrigger>
          <ContextMenuSubContent className="w-48">
            {snoozeOptions.map((opt) => (
              <ContextMenuItem
                key={opt.key}
                onSelect={() => run(snoozeMessages(ids, opt.until))}
              >
                {t(locale, opt.key)}
                <span className="ml-auto text-xs text-muted-foreground">
                  {format(opt.until, 'h:mm a')}
                </span>
              </ContextMenuItem>
            ))}
          </ContextMenuSubContent>
        </ContextMenuSub>
      </ContextMenuContent>
    </ContextMenu>
  );
}
```

- [ ] **Step 3: Wire into `mail-list.tsx`**

In `frontend/src/components/mail/mail-list.tsx`:

1. Add import:

```ts
import { ConversationContextMenu } from '@/components/mail/conversation-context-menu';
```

2. Add an action-error state next to `fetchError` (line ~117):

```ts
  const [actionError, setActionError] = useState<string | null>(null);
```

3. Render the error line inside the root flex column, right after the `ErrorBanner` block (~line 268):

```tsx
      {actionError ? (
        <div className="border-b bg-destructive/10 px-4 py-2 text-sm text-destructive">
          {actionError}
        </div>
      ) : null}
```

4. Wrap the conversation row `div` (the one with `key={convo.key}`, line ~315) in the menu, and select the conversation on right-click. The row keeps all its existing props; add `onContextMenu`:

```tsx
            return (
              <ConversationContextMenu key={convo.key} convo={convo} onActionError={setActionError}>
                <div
                  role="button"
                  tabIndex={0}
                  className={cn(/* unchanged */}
                  onClick={() => {/* unchanged */}}
                  onContextMenu={() => {
                    const target = convo.messages.find((m) => !m.isRead) ?? convo.latest;
                    setSelectedMessage(target.id);
                  }}
                  onKeyDown={(e) => {/* unchanged */}}
                >
                  {/* unchanged row content */}
                </div>
              </ConversationContextMenu>
            );
```

(The `key` moves to the `ConversationContextMenu` wrapper; the inner `div` drops its `key`.)

- [ ] **Step 4: Verify + commit**

Run: `cd frontend && npx tsc --noEmit && npx oxlint src/components/mail/ src/i18n && npx vitest run src/i18n`
Expected: clean. Then with the dev stack up, right-click a row: menu opens, the row becomes selected, Reply opens the composer, Trash removes the conversation, Move to… filters folders.

```bash
git add frontend/src/components/mail/conversation-context-menu.tsx frontend/src/components/mail/mail-list.tsx frontend/src/i18n/en.json frontend/src/i18n/zh.json
git commit -m "feat(frontend): right-click context menu on conversation rows"
```

---

### Task 5: Drag conversation → folder

One `DndContext` hoisted into `mail.tsx` serving both conversation drags and the existing account reorder. dnd-kit registers draggables/droppables with the *nearest* `DndContext`, so the sidebar's inner context must be removed — otherwise folder droppables would be invisible to list draggables.

**Files:**
- Create: `frontend/src/components/mail/mail-dnd.tsx`
- Modify: `frontend/src/components/mail/mail.tsx`
- Modify: `frontend/src/components/mail/mail-list.tsx`
- Modify: `frontend/src/components/mail/sidebar-folders.tsx`

- [ ] **Step 1: Create `mail-dnd.tsx` (provider + overlay + drop handling)**

`frontend/src/components/mail/mail-dnd.tsx`:

```tsx
/**
 * Drag-and-drop context for the mail shell.
 *
 * One DndContext covers both panes: conversation rows (useDraggable in
 * mail-list) drop onto folder rows (useDroppable in sidebar-folders), and
 * the account sections stay sortable (SortableContext in sidebar-folders).
 * Drag kinds are told apart by `active.data.current?.type`.
 */

import {
  DndContext,
  DragOverlay,
  PointerSensor,
  TouchSensor,
  useSensor,
  useSensors,
  type DragEndEvent,
  type DragStartEvent,
} from '@dnd-kit/core';
import { useEffect, useState, type ReactNode } from 'react';

import { Badge } from '@/components/ui/badge';
import { t } from '@/i18n';
import { moveId, orderAccounts } from '@/lib/account-order';
import {
  moveMessages,
  resolveRoleFolder,
  type ConversationDragData,
} from '@/lib/conversation-actions';
import type { StandardFolderRole } from '@/lib/mail-api';
import { useMailStore } from '@/stores/mail';
import { useUIStore } from '@/stores/ui';

/** Drop-target payload on a concrete folder row. */
export interface FolderDropData {
  type: 'folder';
  accountId: string;
  folderId: string;
}

/** Drop-target payload on a unified role row (resolved per drag account). */
export interface UnifiedRoleDropData {
  type: 'folder';
  unified: true;
  role: StandardFolderRole;
}

export function MailDndProvider({ children }: { children: ReactNode }) {
  const locale = useUIStore((s) => s.locale);
  const accounts = useMailStore((s) => s.accounts);
  const accountOrder = useUIStore((s) => s.accountOrder);
  const setAccountOrder = useUIStore((s) => s.setAccountOrder);
  // Pointer for mouse/pen; TouchSensor with a long-press delay so list
  // scrolling still wins over drag on touch devices.
  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 6 } }),
    useSensor(TouchSensor, { activationConstraint: { delay: 200, tolerance: 6 } }),
  );

  const [drag, setDrag] = useState<ConversationDragData | null>(null);
  const [progress, setProgress] = useState<{ done: number; total: number } | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!error) return;
    const handle = window.setTimeout(() => setError(null), 6000);
    return () => window.clearTimeout(handle);
  }, [error]);

  const onDragStart = (event: DragStartEvent) => {
    const data = event.active.data.current as ConversationDragData | undefined;
    if (data?.type === 'conversation') setDrag(data);
  };

  const dropFolderId = (
    data: ConversationDragData,
    overData: FolderDropData | UnifiedRoleDropData | undefined,
  ): string | null => {
    if (!overData || overData.type !== 'folder') return null;
    if ('unified' in overData && overData.unified) {
      // Unified role row: target is the drag account's folder with that role.
      const target = resolveRoleFolder(
        useMailStore.getState().folders,
        data.accountId,
        overData.role,
      );
      return target?.id ?? null;
    }
    const folder = overData as FolderDropData;
    if (folder.accountId !== data.accountId) return null;
    return folder.folderId;
  };

  const handleConversationDrop = async (
    data: ConversationDragData,
    overData: FolderDropData | UnifiedRoleDropData | undefined,
  ) => {
    const targetFolderId = dropFolderId(data, overData);
    if (!targetFolderId) return;
    // Skip messages already sitting in the target (cross-folder copies).
    const messages = useMailStore.getState().messages;
    const ids = data.messageIds.filter((id) => messages[id]?.folderId !== targetFolderId);
    if (ids.length === 0) return;
    setProgress({ done: 0, total: ids.length });
    const res = await moveMessages(ids, targetFolderId, (done) =>
      setProgress({ done, total: ids.length }),
    );
    setProgress(null);
    if (res.error) setError(res.error);
  };

  const onDragEnd = (event: DragEndEvent) => {
    setDrag(null);
    const { active, over } = event;
    if (!over) return;
    const data = active.data.current as ConversationDragData | undefined;
    if (data?.type === 'conversation') {
      void handleConversationDrop(
        data,
        over.data.current as FolderDropData | UnifiedRoleDropData | undefined,
      );
      return;
    }
    // Account reorder (draggables without data.type): existing behavior.
    if (active.id === over.id) return;
    const ids = orderAccounts(accounts, accountOrder).map((a) => a.id);
    const from = String(active.id);
    const to = String(over.id);
    if (ids.includes(from) && ids.includes(to)) {
      setAccountOrder(moveId(ids, from, to));
    }
  };

  return (
    <DndContext
      sensors={sensors}
      onDragStart={onDragStart}
      onDragEnd={onDragEnd}
      onDragCancel={() => setDrag(null)}
    >
      {children}
      <DragOverlay dropAnimation={null}>
        {drag ? (
          <div className="flex max-w-64 items-center gap-2 rounded-lg border bg-card px-3 py-2 text-sm shadow-md">
            <span className="truncate">{drag.subject || '—'}</span>
            {drag.count > 1 ? <Badge variant="secondary">{drag.count}</Badge> : null}
          </div>
        ) : null}
      </DragOverlay>
      {progress ? (
        <div className="pointer-events-none fixed bottom-4 left-1/2 z-50 -translate-x-1/2 rounded-full border bg-popover px-3 py-1.5 text-xs shadow-md">
          {t(locale, 'mail.movingMessages', { done: progress.done, total: progress.total })}
        </div>
      ) : null}
      {error ? (
        <div className="pointer-events-none fixed bottom-4 left-1/2 z-50 -translate-x-1/2 rounded-full border border-destructive/40 bg-destructive/10 px-3 py-1.5 text-xs text-destructive shadow-md">
          {error}
        </div>
      ) : null}
    </DndContext>
  );
}
```

The shared role→folder lookup lives in `conversation-actions.ts` (used by both this provider and the sidebar hook):

```ts
/** Resolve the concrete folder holding `role` for an account (unified row drop targets). */
export function resolveRoleFolder(
  folders: Record<string, MailFolder>,
  accountId: string,
  role: StandardFolderRole,
): MailFolder | null {
  return Object.values(folders).find((f) => f.accountId === accountId && f.role === role) ?? null;
}
```

- [ ] **Step 2: Make conversation rows draggable in `mail-list.tsx`**

Wrap the row rendering in a small component (hook rules require a component per row):

At the bottom of `mail-list.tsx` (before `MailList` or after — match file conventions), add:

```tsx
import { useDraggable } from '@dnd-kit/core';
import type { Conversation } from '@/lib/conversation';

/** Draggable wrapper around a conversation row. */
function DraggableConversationRow({
  convo,
  children,
}: {
  convo: Conversation;
  children: React.ReactNode;
}) {
  const messageIds = convo.messages.map((m) => m.id);
  const folderIds = [...new Set(convo.messages.map((m) => m.folderId))];
  // No `attributes` spread: without a KeyboardSensor they would only add a
  // duplicate role="button" tab stop around the row's own interactive div.
  const { listeners, setNodeRef, isDragging } = useDraggable({
    id: `convo:${convo.key}`,
    data: {
      type: 'conversation',
      accountId: convo.latest.accountId,
      messageIds,
      folderIds,
      subject: convo.latest.subject,
      count: convo.messages.length,
    } satisfies ConversationDragData,
  });
  return (
    <div ref={setNodeRef} {...listeners} className={isDragging ? 'opacity-50' : undefined}>
      {children}
    </div>
  );
}
```

Also import `type { ConversationDragData }` from `@/lib/conversation-actions`.

Then in the `listRows.map`, wrap the `ConversationContextMenu` from Task 4:

```tsx
            return (
              <DraggableConversationRow key={convo.key} convo={convo}>
                <ConversationContextMenu convo={convo} onActionError={setActionError}>
                  <div role="button" /* …unchanged… */>
                    {/* unchanged */}
                  </div>
                </ConversationContextMenu>
              </DraggableConversationRow>
            );
```

Do not spread dnd listeners onto the row `div` itself — the wrapper div carries them, so the row's `onClick`/`onKeyDown` keep working (PointerSensor's 6px threshold lets clicks through).

- [ ] **Step 3: Make folder rows droppable in `sidebar-folders.tsx`**

Add imports:

```ts
import { useDndContext, useDroppable } from '@dnd-kit/core';
import {
  canDropConversation,
  resolveRoleFolder,
  type ConversationDragData,
} from '@/lib/conversation-actions';
import type { FolderDropData, UnifiedRoleDropData } from '@/components/mail/mail-dnd';
```

Add the drop-target hook (module scope):

```tsx
/** Drop-target state for a folder row during a conversation drag. */
function useFolderDropTarget(drop: FolderDropData | UnifiedRoleDropData, dropId: string) {
  const { active } = useDndContext();
  const drag = active?.data.current as ConversationDragData | undefined;
  const isConvoDrag = drag?.type === 'conversation';

  let enabled = false;
  if (isConvoDrag && drag) {
    if ('unified' in drop && drop.unified) {
      const target = resolveRoleFolder(useMailStore.getState().folders, drag.accountId, drop.role);
      enabled = target !== null && !drag.folderIds.includes(target.id);
    } else {
      enabled = canDropConversation(drag, drop as FolderDropData);
    }
  }

  const { isOver, setNodeRef } = useDroppable({ id: dropId, data: drop, disabled: !enabled });
  const rowClass = isConvoDrag
    ? enabled
      ? isOver
        ? 'bg-accent ring-1 ring-ring/40'
        : undefined
      : 'opacity-40'
    : undefined;
  return { setNodeRef, rowClass };
}
```

Apply it in three places:

1. `UnifiedRow` — wrap the `<button>` in a div carrying the ref/class:

```tsx
function UnifiedRow({ folder, active }: { folder: UnifiedFolder; active: boolean }) {
  const locale = useUIStore((s) => s.locale);
  const Icon = ROLE_ICONS[folder.role];
  const { setNodeRef, rowClass } = useFolderDropTarget(
    { type: 'folder', unified: true, role: folder.role },
    `drop:unified:${folder.role}`,
  );
  return (
    <div ref={setNodeRef} className={cn('rounded-[7px]', rowClass)}>
      <button type="button" onClick={() => selectUnifiedRole(folder.role)} className={cn(/* unchanged */)}>
        {/* unchanged */}
      </button>
    </div>
  );
}
```

2. Role folder rows in `AccountSection` — extract the row into a component so the hook is legal:

```tsx
function RoleFolderRow({
  folder,
  account,
  selectedFolderId,
  childrenExpanded,
  hasChildren,
  onToggleExpanded,
  expandedIds,
}: {
  folder: MailFolder;
  account: MailAccount;
  selectedFolderId: string | null;
  childrenExpanded: boolean;
  hasChildren: boolean;
  onToggleExpanded: (id: string) => void;
  expandedIds: Set<string>;
}) {
  const locale = useUIStore((s) => s.locale);
  const role = folder.role as StandardFolderRole;
  const Icon = ROLE_ICONS[role] ?? Folder;
  const active = selectedFolderId === folder.id;
  const { setNodeRef, rowClass } = useFolderDropTarget(
    { type: 'folder', accountId: account.id, folderId: folder.id },
    `drop:folder:${folder.id}`,
  );
  return (
    <div>
      <div
        ref={setNodeRef}
        className={cn(
          'flex items-center rounded-[7px] border',
          active ? 'border-input bg-card shadow-whisper' : 'border-transparent hover:bg-accent/60',
          rowClass,
        )}
      >
        {/* expand/collapse button + label button: unchanged from the existing map body */}
      </div>
      {/* children CustomFolderBranch list: unchanged */}
    </div>
  );
}
```

(The body is exactly the existing per-folder JSX from `AccountSection`, moved verbatim; `AccountSection`'s `roleFolders.map` then renders `<RoleFolderRow key={folder.id} … />`.)

3. `CustomFolderBranch` — same pattern on its outer row div:

```tsx
  const { setNodeRef, rowClass } = useFolderDropTarget(
    { type: 'folder', accountId, folderId: node.id },
    `drop:folder:${node.id}`,
  );
  // then: <div ref={setNodeRef} className={cn('flex items-center rounded-[7px] border', active ? … : …, rowClass)}>
```

Note `CustomFolderBranch` already receives `accountId` as a prop.

- [ ] **Step 4: Remove the sidebar's inner DndContext; hoist the provider in `mail.tsx`**

In `sidebar-folders.tsx`:

- Delete the `DndContext`, `PointerSensor`, `closestCenter`, `useSensor`, `useSensors`, `DragEndEvent` imports from `@dnd-kit/core` (keep `useDndContext`/`useDroppable` from Step 3).
- Delete `moveId`, `orderAccounts` imports and `setAccountOrder`/`accountOrder` usage in `SortableAccountSections`.
- `SortableAccountSections` becomes:

```tsx
/** ACCOUNTS section with drag-to-reorder; the DndContext lives in mail.tsx. */
function SortableAccountSections({
  accounts,
  selectedFolderId,
}: {
  accounts: MailAccount[];
  selectedFolderId: string | null;
}) {
  const accountOrder = useUIStore((s) => s.accountOrder);
  const ordered = orderAccounts(accounts, accountOrder);
  const ids = ordered.map((a) => a.id);

  return (
    <SortableContext items={ids} strategy={verticalListSortingStrategy}>
      <div className="grid gap-0.5">
        {ordered.map((account) => (
          <SortableAccountSection
            key={account.id}
            account={account}
            selectedFolderId={selectedFolderId}
          />
        ))}
      </div>
    </SortableContext>
  );
}
```

(Keep the `orderAccounts` import; drop `moveId`. The reorder `onDragEnd` logic now lives in `MailDndProvider`.)

In `mail.tsx`:

- Import `MailDndProvider` and wrap BOTH layout returns. Desktop:

```tsx
  return (
    <TooltipProvider delayDuration={0}>
      <MailDndProvider>
        <div className="flex h-full">
          {/* …AppRail + ResizablePanelGroup unchanged… */}
        </div>
      </MailDndProvider>
    </TooltipProvider>
  );
```

- Mobile: wrap the mobile return's outer `<div className="flex h-full flex-col bg-background">` (and the drawer) in `MailDndProvider` the same way — droppables in the drawer must live inside a context or `useDroppable` has no parent.

- [ ] **Step 5: Verify + commit**

Run: `cd frontend && npx tsc --noEmit && npx oxlint src/components/mail/ && npx vitest run`
Expected: clean, all tests PASS.

Manual (dev stack): drag a conversation onto another folder of the same account → moves; drag onto another account's folder → dimmed, no drop; drag onto the unified Trash row → moves to that account's Trash; account reorder still works.

```bash
git add frontend/src/components/mail/mail-dnd.tsx frontend/src/components/mail/mail.tsx frontend/src/components/mail/mail-list.tsx frontend/src/components/mail/sidebar-folders.tsx
git commit -m "feat(frontend): drag conversations onto sidebar folders to move them"
```

---

### Task 6: Sync-all button

**Files:**
- Create: `frontend/src/lib/use-syncing-accounts.ts`
- Test: `frontend/src/lib/use-syncing-accounts.test.ts`
- Create: `frontend/src/components/mail/sync-all-button.tsx`
- Modify: `frontend/src/components/mail/mail.tsx` (footer + `SyncStatusDot` refactor)

- [ ] **Step 1: Write the failing test for the reducer**

`frontend/src/lib/use-syncing-accounts.test.ts`:

```ts
import { describe, expect, it } from 'vitest';

import { reduceSyncEvent } from '@/lib/use-syncing-accounts';

describe('reduceSyncEvent', () => {
  it('adds on sync_started, removes on sync_complete', () => {
    let s = reduceSyncEvent(new Set(), { type: 'sync_started', accountId: 'a' });
    s = reduceSyncEvent(s, { type: 'sync_started', accountId: 'b' });
    expect([...s].sort()).toEqual(['a', 'b']);
    s = reduceSyncEvent(s, { type: 'sync_complete', accountId: 'a' });
    expect([...s]).toEqual(['b']);
  });

  it('removes on sync_error too', () => {
    const s = reduceSyncEvent(new Set(['a']), {
      type: 'sync_error',
      accountId: 'a',
      error: 'x',
    });
    expect(s.size).toBe(0);
  });

  it('does not mutate the input set', () => {
    const before = new Set(['a']);
    reduceSyncEvent(before, { type: 'sync_complete', accountId: 'a' });
    expect(before.has('a')).toBe(true);
  });
});
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd frontend && npx vitest run src/lib/use-syncing-accounts.test.ts`
Expected: FAIL — module does not exist.

- [ ] **Step 3: Implement the hook**

`frontend/src/lib/use-syncing-accounts.ts`:

```ts
/**
 * Tracks which accounts currently have a sync running, from the SSE stream.
 * Replaces boolean "any syncing" flags that went false as soon as *one*
 * account finished while another was still running.
 */

import { useEffect, useState } from 'react';

import { syncEvents$ } from '@/rxjs/sync-events';
import type { SyncEvent } from '@/types';

/** Pure reducer: apply one sync event to the in-flight account id set. */
export function reduceSyncEvent(active: ReadonlySet<string>, ev: SyncEvent): Set<string> {
  const next = new Set(active);
  if (ev.type === 'sync_started') next.add(ev.accountId);
  if (ev.type === 'sync_complete' || ev.type === 'sync_error') next.delete(ev.accountId);
  return next;
}

/** Ids of accounts currently syncing (empty set = idle). */
export function useSyncingAccounts(): ReadonlySet<string> {
  const [active, setActive] = useState<ReadonlySet<string>>(new Set());
  useEffect(() => {
    const sub = syncEvents$.subscribe((ev) => setActive((prev) => reduceSyncEvent(prev, ev)));
    return () => sub.unsubscribe();
  }, []);
  return active;
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cd frontend && npx vitest run src/lib/use-syncing-accounts.test.ts`
Expected: PASS.

- [ ] **Step 5: Create `sync-all-button.tsx`**

`frontend/src/components/mail/sync-all-button.tsx`:

```tsx
/**
 * Manual "sync every account" button for the sidebar footer.
 * Loops the per-account trigger; the backend dedups queued/running jobs.
 * Spins while any account reports sync activity on the SSE stream.
 */

import { RefreshCw } from 'lucide-react';
import { useState } from 'react';

import { Button } from '@/components/ui/button';
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip';
import { t } from '@/i18n';
import { api } from '@/lib/api-client';
import { useSyncingAccounts } from '@/lib/use-syncing-accounts';
import { cn } from '@/lib/utils';
import { useMailStore } from '@/stores/mail';
import { useUIStore } from '@/stores/ui';

export function SyncAllButton() {
  const locale = useUIStore((s) => s.locale);
  const accounts = useMailStore((s) => s.accounts);
  const syncing = useSyncingAccounts().size > 0;
  const [failed, setFailed] = useState(false);

  const onClick = async () => {
    setFailed(false);
    const results = await Promise.all(
      accounts.map((a) =>
        api(`/accounts/${a.id}/sync`, { method: 'POST' }).then(
          () => true,
          () => false,
        ),
      ),
    );
    if (results.some((ok) => !ok)) setFailed(true);
  };

  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <Button
          variant="ghost"
          size="icon"
          className="h-8 w-8 shrink-0"
          disabled={syncing || accounts.length === 0}
          onClick={() => void onClick()}
          aria-label={t(locale, 'mail.syncAllAccounts')}
        >
          <RefreshCw
            className={cn('h-4 w-4', syncing && 'animate-spin', failed && 'text-destructive')}
          />
        </Button>
      </TooltipTrigger>
      <TooltipContent>
        {failed ? t(locale, 'mail.syncStartFailed') : t(locale, 'mail.syncAllAccounts')}
      </TooltipContent>
    </Tooltip>
  );
}
```

- [ ] **Step 6: Wire into `mail.tsx` + refactor `SyncStatusDot`**

Replace the `SyncStatusDot` body to use the shared hook (also fixes its one-account-finished-resets-the-dot flaw):

```tsx
/** Green sync dot; amber pulse while any account is syncing. */
function SyncStatusDot() {
  const locale = useUIStore((s) => s.locale);
  const syncing = useSyncingAccounts().size > 0;

  return (
    <span
      className={cn('size-1.5 rounded-full', syncing ? 'animate-pulse bg-unread' : 'bg-ok')}
      role="status"
      aria-label={t(locale, syncing ? 'sync.syncing' : 'sync.syncComplete')}
    />
  );
}
```

(Remove its local `useState`/`useEffect`/`syncEvents$` subscription; add `import { useSyncingAccounts } from '@/lib/use-syncing-accounts';` and drop `syncEvents$` import if now unused in this file.)

In `NavContent`'s footer (the block with `LyraWordmark` + `SyncStatusDot`, mail.tsx:100-106), add the button after `SyncStatusDot`:

```tsx
        <div className="mt-auto flex items-center gap-1.5 px-3 py-2">
          <LyraWordmark className="[&>span:last-child]:text-sm" />
          <SyncStatusDot />
          <div className="flex-1" />
          <SyncAllButton />
        </div>
```

Add the import: `import { SyncAllButton } from '@/components/mail/sync-all-button';`

- [ ] **Step 7: Verify + commit**

Run: `cd frontend && npx tsc --noEmit && npx oxlint src/ && npx vitest run`
Expected: clean, all PASS.

Manual: sidebar footer shows a refresh icon; clicking spins it and both accounts sync (watch for new mail); dot pulses amber meanwhile.

```bash
git add frontend/src/lib/use-syncing-accounts.ts frontend/src/lib/use-syncing-accounts.test.ts frontend/src/components/mail/sync-all-button.tsx frontend/src/components/mail/mail.tsx
git commit -m "feat(frontend): sync-all-accounts button in the sidebar footer"
```

---

### Task 7: Full verification

- [ ] **Step 1: Format + lint + test**

Run from repo root:

```bash
make fmt && cd frontend && npm run check && npx vitest run
```

Expected: prettier reformats if needed, oxlint + tsc clean, all vitest tests PASS.

- [ ] **Step 2: Update the spec for the as-built error UX**

The spec's drag-drop section says errors show as an "inline error line at the top of the mail list"; the plan builds a transient floating status chip in `mail-dnd.tsx` instead (the DnD provider lives above both panes, so an in-list banner would need lifted state for no real gain). Edit `docs/superpowers/specs/2026-09-02-mail-list-actions-design.md`, replacing that sentence with: "Progress and errors surface as a transient floating status chip (auto-dismiss ~6s) rendered by the DnD provider."

```bash
git add docs/superpowers/specs/2026-09-02-mail-list-actions-design.md
git commit -m "docs: reflect as-built drag-drop progress/error chip"
```

- [ ] **Step 3: Rebuild the dev stack and do a manual pass**

```bash
docker compose up -d --build lyra && sleep 90 && curl -s http://127.0.0.1:3000/health
```

Then in the UI (http://localhost:3000), verify:

1. Drag a conversation onto a same-account folder → disappears from the list, appears in the target folder; unread counts update after next sync.
2. Drag onto another account's folder → target dimmed during drag, drop does nothing.
3. Drag onto a unified role row (e.g. Trash) → moves to that account's Trash.
4. Account reorder by dragging the account header still works.
5. Right-click a conversation → row selects, menu opens; exercise Reply, Reply All, Forward, Archive, Spam, Trash, Move to… (with the filter input), Mark Read/Unread, Star/Unstar, Snooze.
6. Right-click a draft conversation → Edit draft opens the composer.
7. Sync-all button spins while syncing and new mail arrives.
8. Switch locale to zh and confirm the new labels render Chinese.
9. Touch (or device-emulated) check: long-press starts a conversation drag, quick swipe still scrolls the list; long-press account reorder still works.

- [ ] **Step 4: Final commit (if fmt changed files)**

```bash
git status --short
git add -A && git commit -m "chore: format" || true
```

---

## Self-review notes

- Spec coverage: drag-to-folder (Task 5), context menu with filter (Task 4), sync-all (Task 6), i18n (Task 4 step 1), tests (Tasks 1/2/6), spec-as-built sync (Task 7 step 2). Out-of-scope items stay out.
- Type consistency: `ConversationDragData` / `FolderDropData` / `UnifiedRoleDropData` are defined once (Task 2 lib, Task 5 provider) and imported elsewhere; `BatchResult` is the single error channel; `reduceSyncEvent`/`useSyncingAccounts` used by both the dot and the button.
- Known trade-off: no keyboard-accessible drag for conversations (matches the deferred keyboard-reorder note on account dnd); no `TouchSensor` — pointer only, consistent with existing account reorder.
