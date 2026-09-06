/**
 * Calendar event reminders: window selection + dedupe baseline.
 *
 * Pure decision logic for `use-event-notifications` — the poller fetches
 * events from every calendar/subscription source, these functions decide
 * which occurrences are inside the reminder window and not yet announced.
 *
 * Semantics: remind up to EVENT_WINDOW_BEFORE before an event starts, and
 * keep reminding (once) up to EVENT_WINDOW_AFTER after the start for events
 * that began while the app was closed — the "due already exceeded" case.
 * All-day events are skipped: their midnight start has no useful reminder
 * time.
 */

const BASELINE_KEY = 'lyra.notify.events.v1';

/** Notify this long before dtstart. */
export const EVENT_WINDOW_BEFORE_MS = 15 * 60_000;
/** Still notify this long after dtstart (missed-while-closed grace). */
export const EVENT_WINDOW_AFTER_MS = 30 * 60_000;
/** Prune baseline markers older than this. */
const BASELINE_TTL_MS = 24 * 60 * 60_000;

export interface ReminderEvent {
  id: string;
  summary?: string;
  dtstart?: string;
  isAllDay: boolean;
}

/** id → dtstart(ms) of already-announced occurrences. */
export type EventBaseline = Record<string, number>;

export function readEventBaseline(): EventBaseline {
  try {
    const raw = localStorage.getItem(BASELINE_KEY);
    if (!raw) return {};
    const parsed = JSON.parse(raw) as Record<string, unknown>;
    const out: EventBaseline = {};
    for (const [id, ts] of Object.entries(parsed)) {
      if (typeof ts === 'number') out[id] = ts;
    }
    return out;
  } catch {
    return {};
  }
}

export function writeEventBaseline(baseline: EventBaseline, now: number): void {
  const pruned: EventBaseline = {};
  for (const [id, ts] of Object.entries(baseline)) {
    if (now - ts < BASELINE_TTL_MS) pruned[id] = ts;
  }
  localStorage.setItem(BASELINE_KEY, JSON.stringify(pruned));
}

/**
 * Events whose start falls inside [now - AFTER, now + BEFORE].
 * All-day events and rows without a parseable start never qualify.
 */
export function eventsInReminderWindow(
  events: ReminderEvent[],
  now: number,
): Array<ReminderEvent & { startMs: number }> {
  const out: Array<ReminderEvent & { startMs: number }> = [];
  for (const ev of events) {
    if (ev.isAllDay) continue;
    if (!ev.dtstart) continue;
    const startMs = new Date(ev.dtstart).getTime();
    if (Number.isNaN(startMs)) continue;
    const lower = now - EVENT_WINDOW_AFTER_MS;
    const upper = now + EVENT_WINDOW_BEFORE_MS;
    if (startMs >= lower && startMs <= upper) {
      out.push({ ...ev, startMs });
    }
  }
  return out;
}

/**
 * First-seen events only. `seed=true` (first ever poll, or after the
 * baseline was lost) records everything silently instead of firing a
 * notification storm for a schedule full of imminent items.
 */
export function dueReminders(
  events: ReminderEvent[],
  baseline: EventBaseline,
  now: number,
): { notify: Array<ReminderEvent & { startMs: number }>; seed: boolean } {
  const inWindow = eventsInReminderWindow(events, now);
  const firstRun = Object.keys(baseline).length === 0;
  const notify = firstRun ? [] : inWindow.filter((ev) => !(ev.id in baseline));
  return { notify, seed: firstRun };
}

/** Notification body: relative-to-now phrasing per locale. */
export function reminderBody(
  ev: ReminderEvent & { startMs: number },
  now: number,
  locale: string,
): string {
  const summary = ev.summary?.trim() || '';
  const deltaMin = Math.round((ev.startMs - now) / 60_000);
  let when: string;
  if (deltaMin > 0) {
    when =
      locale === 'zh'
        ? `${deltaMin} 分钟后`
        : deltaMin === 1
          ? 'in 1 minute'
          : `in ${deltaMin} minutes`;
  } else if (deltaMin === 0) {
    when = locale === 'zh' ? '现在' : 'now';
  } else {
    when =
      locale === 'zh'
        ? `已开始 ${-deltaMin} 分钟`
        : `started ${-deltaMin === 1 ? '1 minute' : `${-deltaMin} minutes`} ago`;
  }
  return summary ? `${when} · ${summary}` : when;
}
