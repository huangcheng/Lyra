/**
 * Mail notifications: permission management + new-mail detection.
 *
 * New-mail awareness is frontend-driven v1: on `sync_complete` the account's
 * newest messages are re-listed and diffed against a per-account high-water
 * mark of known message ids. The first run seeds the baseline silently so
 * opening Lyra doesn't fire a notification storm.
 *
 * "New mail" means any message landing outside outgoing/system folders
 * (sent, drafts, trash, spam, outbox) — not just role=inbox. Providers with
 * server-side rules file most mail directly into custom folders (no special
 * role) or straight to archive, so an inbox-only diff would never fire for
 * those accounts. The diff identity is the RFC 5322 Message-ID, which is
 * stable across folders: moving/archiving mail yourself never re-notifies.
 *
 * Notifications render through the service worker when present (works from
 * background tabs, one surface per origin) and fall back to the page-level
 * Notification API otherwise. Clicking focuses Lyra and opens the message
 * (the SW posts `lyra:open-message`; main.tsx routes it here).
 *
 * Limitation (by design, see the notifications/PWA spec): delivery requires
 * the app to be running — the SSE stream lives in the page. Closed-app
 * push is the Push-API workstream.
 */

import { api } from '@/lib/api-client';
import { mapApiMessage, type ApiMessage } from '@/lib/mail-api';
import { useAuthStore } from '@/stores/auth';
import { useMailStore } from '@/stores/mail';
import { useUIStore } from '@/stores/ui';
import type { SyncEvent } from '@/types';

// v2: identities are RFC 5322 Message-IDs, not row ids (stable across
// folders, so user-driven moves don't re-notify). v1 rows ids never match,
// which is fine: the first sync under v2 re-seeds silently.
const BASELINE_KEY = 'lyra.notify.baseline.v2';
const PREFS_KEY = 'lyra.notifications';
const INBOX_DIFF_LIMIT = 15;
/** Never fire more than this many notifications per sync; extras fold into a summary. */
const MAX_PER_SYNC = 3;
/**
 * Folder roles that never count as incoming mail. Archive IS incoming: with
 * Message-ID identity, self-archived mail is already known and stays quiet,
 * while server rules that skip the inbox (e.g. Gmail "Skip Inbox") notify.
 */
const NON_INCOMING_ROLES = new Set(['sent', 'drafts', 'trash', 'spam', 'junk', 'outbox']);

/** True when the folder role counts as incoming mail (inbox, archive, or a custom folder). */
export function isIncomingFolderRole(folderRole: string | null | undefined): boolean {
  return !NON_INCOMING_ROLES.has(folderRole ?? '');
}

/**
 * Diff identity for a message: the RFC 5322 Message-ID is stable across
 * folders and copies; fall back to the row id when a message lacks one.
 */
export function messageIdentity(msg: ApiMessage): string {
  return msg.messageIdHeader || msg.id;
}

export interface NotificationPrefs {
  enabled: boolean;
  /** Folder ids that never produce banners (right-click a folder to mute). */
  mutedFolderIds: string[];
  /** Thread ids that never produce banners (conversation context menu → Mute). */
  mutedThreadIds: string[];
}

const prefsListeners = new Set<() => void>();

/** Subscribe to preference changes (e.g. folder mute toggles); returns unsubscribe. */
export function subscribeNotificationPrefs(listener: () => void): () => void {
  prefsListeners.add(listener);
  return () => {
    prefsListeners.delete(listener);
  };
}

export function readNotificationPrefs(): NotificationPrefs {
  try {
    const raw = localStorage.getItem(PREFS_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as Partial<NotificationPrefs>;
      const stringList = (v: unknown): string[] =>
        Array.isArray(v) ? v.filter((id): id is string => typeof id === 'string') : [];
      const mutedFolderIds = stringList(parsed.mutedFolderIds);
      const mutedThreadIds = stringList(parsed.mutedThreadIds);
      if (typeof parsed.enabled === 'boolean') {
        return { enabled: parsed.enabled, mutedFolderIds, mutedThreadIds };
      }
    }
  } catch {
    // corrupted blob → defaults
  }
  return { enabled: false, mutedFolderIds: [], mutedThreadIds: [] };
}

export function writeNotificationPrefs(prefs: NotificationPrefs): void {
  localStorage.setItem(PREFS_KEY, JSON.stringify(prefs));
  for (const listener of prefsListeners) listener();
}

/** True when banners are muted for a folder. */
export function isFolderMuted(folderId: string): boolean {
  return readNotificationPrefs().mutedFolderIds.includes(folderId);
}

/** Mute/unmute banners for a folder (persists with the notification prefs). */
export function setFolderMuted(folderId: string, muted: boolean): void {
  const prefs = readNotificationPrefs();
  const next = new Set(prefs.mutedFolderIds);
  if (muted) {
    next.add(folderId);
  } else {
    next.delete(folderId);
  }
  writeNotificationPrefs({ ...prefs, mutedFolderIds: [...next] });
}

/** True when banners are muted for a conversation thread. */
export function isThreadMuted(threadId: string): boolean {
  return readNotificationPrefs().mutedThreadIds.includes(threadId);
}

/** Mute/unmute banners for a conversation thread (persists with the prefs). */
export function setThreadMuted(threadId: string, muted: boolean): void {
  const prefs = readNotificationPrefs();
  const next = new Set(prefs.mutedThreadIds);
  if (muted) {
    next.add(threadId);
  } else {
    next.delete(threadId);
  }
  writeNotificationPrefs({ ...prefs, mutedThreadIds: [...next] });
}

export function notificationPermission(): NotificationPermission | 'unsupported' {
  if (typeof window === 'undefined' || !('Notification' in window)) return 'unsupported';
  return Notification.permission;
}

/** Ask the browser for permission (must run from a user gesture). */
export async function requestNotificationPermission(): Promise<NotificationPermission> {
  if (!('Notification' in window)) return 'denied';
  try {
    return await Notification.requestPermission();
  } catch {
    return Notification.permission;
  }
}

// Navigation is router-owned; main.tsx registers the navigator so this
// module stays free of router imports.
let navigateToMail: (() => void) | null = null;

export function setOpenMessageNavigator(fn: (() => void) | null): void {
  navigateToMail = fn;
}

function readBaseline(): Record<string, string[]> {
  try {
    const raw = localStorage.getItem(BASELINE_KEY);
    if (!raw) return {};
    const parsed = JSON.parse(raw) as Record<string, unknown>;
    const out: Record<string, string[]> = {};
    for (const [account, ids] of Object.entries(parsed)) {
      if (Array.isArray(ids) && ids.every((id) => typeof id === 'string')) {
        out[account] = ids;
      }
    }
    return out;
  } catch {
    return {};
  }
}

function writeBaseline(baseline: Record<string, string[]>): void {
  localStorage.setItem(BASELINE_KEY, JSON.stringify(baseline));
}

/** Display label for a message's sender (exported for tests). */
export function senderLabel(msg: ApiMessage): string {
  const from = msg.fromAddress ?? '';
  // `fromAddress` is a JSON-encoded address array (or a bare string); show
  // the first entry's display form.
  try {
    const parsed = JSON.parse(from) as unknown;
    if (Array.isArray(parsed) && parsed.length > 0) {
      const first = parsed[0] as { name?: string; email?: string } | string;
      if (typeof first === 'string') return first;
      return first.name ?? first.email ?? '';
    }
  } catch {
    // bare string
  }
  return from;
}

async function showNotification(title: string, body: string, tag: string, messageId: string) {
  const options: NotificationOptions & { data?: { messageId: string } } = {
    body,
    tag,
    icon: '/icons/icon-192.png',
    badge: '/icons/icon-192.png',
    data: { messageId },
  };
  const reg = await navigator.serviceWorker?.getRegistration();
  if (reg) {
    await reg.showNotification(title, options);
  } else {
    const n = new Notification(title, options);
    n.onclick = () => {
      window.focus();
      void openMessage(messageId);
      n.close();
    };
  }
}

/** Focus + select a message (loading it first when the store lacks it). */
export async function openMessage(messageId: string): Promise<void> {
  if (!messageId) {
    navigateToMail?.();
    return;
  }
  const mail = useMailStore.getState();
  if (!mail.messages[messageId]) {
    try {
      const raw = await api<ApiMessage>(`/messages/${messageId}`);
      mail.upsertMessage(mapApiMessage(raw));
    } catch {
      return; // message vanished (moved/deleted) — nothing to open
    }
  }
  useUIStore.getState().setSelectedMessage(messageId);
  navigateToMail?.();
}

/**
 * React to a sync event: when an account finished syncing, diff its newest
 * incoming messages and notify for ids not seen before. No-ops unless
 * notifications are enabled + permitted.
 */
export async function handleSyncEventForNotifications(ev: SyncEvent): Promise<void> {
  if (ev.type !== 'sync_complete' && ev.type !== 'incremental_complete') return;
  if (!readNotificationPrefs().enabled) return;
  if (notificationPermission() !== 'granted') return;
  // Tab visibility no longer suppresses notifications: users expect OS
  // banners even when Lyra is the active tab (Apple Mail parity). The
  // unread-diff already prevents re-notifying for messages the user has
  // seen (the baseline updates on every sync).
  if (!useAuthStore.getState().token) return;

  const { accountId } = ev;
  let messages: ApiMessage[];
  try {
    // All folders, newest first: providers with server-side rules file mail
    // straight into custom folders, so an inbox-only diff never fires there.
    messages = await api<ApiMessage[]>(`/messages?accountId=${accountId}`);
  } catch {
    return; // next sync retries
  }
  const incoming = messages.filter((m) => isIncomingFolderRole(m.folderRole));
  const slice = incoming.slice(0, INBOX_DIFF_LIMIT);
  const identities = slice.map(messageIdentity);
  const baseline = readBaseline();
  const known = new Set(baseline[accountId]);
  baseline[accountId] = identities;
  writeBaseline(baseline);
  if (known.size === 0) return; // first run seeds silently

  // The baseline records everything incoming; the mute lists only gate banners.
  const prefs = readNotificationPrefs();
  const mutedFolders = new Set(prefs.mutedFolderIds);
  const mutedThreads = new Set(prefs.mutedThreadIds);
  const fresh = slice.filter(
    (m) =>
      !known.has(messageIdentity(m)) &&
      !mutedFolders.has(m.folderId) &&
      !(m.threadId && mutedThreads.has(m.threadId)),
  );
  if (fresh.length === 0) return;

  const locale = useUIStore.getState().locale;
  for (const msg of fresh.slice(0, MAX_PER_SYNC)) {
    const title = senderLabel(msg) || (locale === 'zh' ? '新邮件' : 'New message');
    await showNotification(title, msg.subject ?? '', `lyra-${msg.id}`, msg.id);
  }
  const more = fresh.length - MAX_PER_SYNC;
  if (more > 0) {
    const title = locale === 'zh' ? `还有 ${more} 封新邮件` : `${more} more new messages`;
    await showNotification(title, '', 'lyra-summary', fresh[MAX_PER_SYNC].id);
  }
}

/** Test notification from Settings. */
export async function sendTestNotification(locale: 'en' | 'zh'): Promise<boolean> {
  if (notificationPermission() !== 'granted') return false;
  await showNotification(
    locale === 'zh' ? 'Lyra 通知已启用' : 'Lyra notifications are on',
    locale === 'zh' ? '新邮件到达时会像这样提醒你。' : 'New mail will look like this.',
    'lyra-test',
    '',
  );
  return true;
}

/**
 * Calendar event reminder. Click-through focuses Lyra (the mail home);
 * deep-linking to the event is future work alongside event detail routes.
 */
export async function showEventNotification(
  title: string,
  body: string,
  tag: string,
): Promise<void> {
  await showNotification(title, body, tag, '');
}
