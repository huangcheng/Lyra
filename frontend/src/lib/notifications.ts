/**
 * Mail notifications: permission management + new-inbox detection.
 *
 * New-mail awareness is frontend-driven v1: on `sync_complete` the inbox is
 * re-listed (limit 15, newest first) and diffed against a per-account
 * high-water mark of known message ids. The first run seeds the baseline
 * silently so opening Lyra doesn't fire a notification storm.
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

const BASELINE_KEY = 'lyra.notify.baseline.v1';
const PREFS_KEY = 'lyra.notifications';
const INBOX_DIFF_LIMIT = 15;
/** Never fire more than this many notifications per sync; extras fold into a summary. */
const MAX_PER_SYNC = 3;

export interface NotificationPrefs {
  enabled: boolean;
}

export function readNotificationPrefs(): NotificationPrefs {
  try {
    const raw = localStorage.getItem(PREFS_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as Partial<NotificationPrefs>;
      if (typeof parsed.enabled === 'boolean') return { enabled: parsed.enabled };
    }
  } catch {
    // corrupted blob → defaults
  }
  return { enabled: false };
}

export function writeNotificationPrefs(prefs: NotificationPrefs): void {
  localStorage.setItem(PREFS_KEY, JSON.stringify(prefs));
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
 * React to a sync event: when an account finished syncing, diff the inbox
 * and notify for messages not seen before. No-ops unless notifications are
 * enabled + permitted, and stays quiet while the tab is focused.
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
    messages = await api<ApiMessage[]>(`/messages?role=inbox&accountId=${accountId}`);
  } catch {
    return; // next sync retries
  }
  // Newest-first; only the freshest slice participates in the diff.
  const slice = messages.slice(0, INBOX_DIFF_LIMIT);
  const ids = slice.map((m) => m.id);
  const baseline = readBaseline();
  const known = new Set(baseline[accountId]);
  baseline[accountId] = ids;
  writeBaseline(baseline);
  if (known.size === 0) return; // first run seeds silently

  const fresh = slice.filter((m) => !known.has(m.id));
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
