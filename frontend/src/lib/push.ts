/**
 * Web Push plumbing: subscription lifecycle against /api/v1/push and the
 * server-side copy of the mute prefs. Page-driven banners stay in
 * notifications.ts; this module only manages closed-app push.
 */

import { api } from '@/lib/api-client';

export type PushStatus = 'unsupported' | 'denied' | 'subscribed' | 'unsubscribed';

function urlBase64ToUint8Array(base64: string): Uint8Array {
  const padding = '='.repeat((4 - (base64.length % 4)) % 4);
  const raw = atob((base64 + padding).replace(/-/g, '+').replace(/_/g, '/'));
  return Uint8Array.from(raw, (c) => c.charCodeAt(0));
}

async function currentSubscription(): Promise<PushSubscription | null> {
  if (!('serviceWorker' in navigator)) return null;
  const reg = await navigator.serviceWorker.getRegistration();
  return reg?.pushManager.getSubscription() ?? null;
}

export async function pushStatus(): Promise<PushStatus> {
  if (typeof window === 'undefined') return 'unsupported';
  if (!('serviceWorker' in navigator) || !('PushManager' in window)) return 'unsupported';
  if (typeof Notification !== 'undefined' && Notification.permission === 'denied') return 'denied';
  return (await currentSubscription()) ? 'subscribed' : 'unsubscribed';
}

/** Subscribe this browser and register the subscription with the server. */
export async function subscribePush(): Promise<void> {
  const reg = await navigator.serviceWorker.ready;
  const { publicKey } = await api<{ publicKey: string }>('/push/vapid-key');
  const sub = await reg.pushManager.subscribe({
    userVisibleOnly: true,
    applicationServerKey: urlBase64ToUint8Array(publicKey) as BufferSource,
  });
  const json = sub.toJSON();
  await api('/push/subscription', {
    method: 'PUT',
    body: JSON.stringify({ endpoint: json.endpoint, keys: json.keys }),
  });
}

/** Remove the server-side subscription, then unsubscribe the browser. */
export async function unsubscribePush(): Promise<void> {
  const sub = await currentSubscription();
  if (!sub) return;
  try {
    await api('/push/subscription', {
      method: 'DELETE',
      body: JSON.stringify({ endpoint: sub.endpoint }),
    });
  } finally {
    await sub.unsubscribe().catch(() => {});
  }
}

export interface PushPrefsPayload {
  mutedFolderIds: string[];
  mutedThreadIds: string[];
  locale: string;
}

/**
 * Write-through of the mute lists so the server can filter before sending.
 * No-op when this browser has no active push subscription (mutes made on an
 * unsubscribed device reach the server at the next write from a subscribed
 * one). Fire-and-forget: failures are swallowed; the next prefs write retries.
 */
export async function syncPushPrefs(prefs: PushPrefsPayload): Promise<void> {
  if (!(await currentSubscription())) return;
  try {
    await api('/push/prefs', { method: 'PUT', body: JSON.stringify(prefs) });
  } catch {
    // best-effort
  }
}

/**
 * Heal-on-open: when this browser has an active push subscription, make sure
 * the server has it (covers pushservice endpoint rotation and re-installs).
 * Cheap: one local read + one idempotent PUT. Gated on subscription state,
 * not the banner pref — a user may rely on push alone.
 */
export async function reconcilePushSubscription(): Promise<void> {
  const sub = await currentSubscription();
  if (!sub) return;
  const json = sub.toJSON();
  try {
    await api('/push/subscription', {
      method: 'PUT',
      body: JSON.stringify({ endpoint: json.endpoint, keys: json.keys }),
    });
  } catch {
    // best-effort
  }
}
