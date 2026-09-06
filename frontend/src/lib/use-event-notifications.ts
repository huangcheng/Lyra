/**
 * Bridge calendar sources to event reminders.
 *
 * Mounted once in the root layout next to `useMailNotifications`. Polls
 * every minute while the app is open, collects events from every calendar
 * and ICS subscription, and lets `event-notifications` decide what is due.
 * Same limitations as mail notifications: delivery needs the app running.
 */

import { useEffect } from 'react';

import { t } from '@/i18n';
import { api } from '@/lib/api-client';
import {
  dueReminders,
  eventsInReminderWindow,
  readEventBaseline,
  reminderBody,
  writeEventBaseline,
  type EventBaseline,
  type ReminderEvent,
} from '@/lib/event-notifications';
import {
  notificationPermission,
  readNotificationPrefs,
  showEventNotification,
} from '@/lib/notifications';
import { useAuthStore } from '@/stores/auth';
import { useUIStore } from '@/stores/ui';

const POLL_MS = 60_000;
/** Never fire more than this many event reminders per poll. */
const MAX_PER_POLL = 3;

async function collectEvents(startIso: string, endIso: string): Promise<ReminderEvent[]> {
  const [cals, subs] = await Promise.all([
    api<{ id: string }[]>('/calendars').catch(() => [] as { id: string }[]),
    api<{ id: string }[]>('/calendar-subscriptions').catch(() => [] as { id: string }[]),
  ]);
  const q = `start=${encodeURIComponent(startIso)}&end=${encodeURIComponent(endIso)}`;
  const sources = [
    ...cals.map((c) => `/calendars/${c.id}/events?${q}`),
    ...subs.map((s) => `/calendar-subscriptions/${s.id}/events?${q}`),
  ];
  const chunks = await Promise.all(
    sources.map((path) => api<ReminderEvent[]>(path).catch(() => [] as ReminderEvent[])),
  );
  return chunks.flat();
}

async function poll(): Promise<void> {
  if (!readNotificationPrefs().enabled) return;
  if (notificationPermission() !== 'granted') return;
  if (!useAuthStore.getState().token) return;

  const now = Date.now();
  const startIso = new Date(now - 40 * 60_000).toISOString();
  const endIso = new Date(now + 20 * 60_000).toISOString();
  let events: ReminderEvent[];
  try {
    events = await collectEvents(startIso, endIso);
  } catch {
    return; // next poll retries
  }

  const baseline = readEventBaseline();
  const { notify, seed } = dueReminders(events, baseline, now);
  if (seed) {
    // First run: record in-window occurrences silently so opening Lyra
    // doesn't fire a storm; later occurrences notify normally.
    const next: EventBaseline = { ...baseline };
    for (const ev of eventsInReminderWindow(events, now)) {
      next[ev.id] = now;
    }
    writeEventBaseline(next, now);
    return;
  }

  const locale = useUIStore.getState().locale;
  const capped = notify.slice(0, MAX_PER_POLL);
  for (const ev of capped) {
    const title = t(locale, 'settings.notifications.eventTitle');
    await showEventNotification(title, reminderBody(ev, now, locale), `lyra-event-${ev.id}`);
  }
  if (capped.length > 0) {
    // Only mark what was actually shown — over-cap events retry next poll.
    const next: EventBaseline = { ...baseline };
    for (const ev of capped) {
      next[ev.id] = now;
    }
    writeEventBaseline(next, now);
  }
}

export function useEventNotifications(): void {
  useEffect(() => {
    void poll();
    const id = window.setInterval(() => void poll(), POLL_MS);
    return () => window.clearInterval(id);
  }, []);
}
