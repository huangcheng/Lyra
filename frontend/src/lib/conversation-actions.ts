/**
 * Batch actions over every message of a conversation row.
 *
 * The API is per-message, so each helper loops sequentially: a failure
 * stops the loop, keeping local state consistent with the server (the
 * messages in `done` were applied; the rest were not).
 */

import { api } from '@/lib/api-client';
import { mapApiMessage, type ApiMessage, type StandardFolderRole } from '@/lib/mail-api';
import { textToHtml } from '@/lib/compose-html';
import { buildForwardDraft, buildReplyDraft } from '@/lib/compose-draft';
import { useMailStore } from '@/stores/mail';
import { useUIStore } from '@/stores/ui';
import type { MailFolder, MailMessage } from '@/types';

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
      accountId: m.accountId,
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

/** Resolve the concrete folder holding `role` for an account (unified row drop targets). */
export function resolveRoleFolder(
  folders: Record<string, MailFolder>,
  accountId: string,
  role: StandardFolderRole,
): MailFolder | null {
  return Object.values(folders).find((f) => f.accountId === accountId && f.role === role) ?? null;
}
