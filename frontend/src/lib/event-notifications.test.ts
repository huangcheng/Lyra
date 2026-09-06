import { beforeEach, describe, expect, it } from 'vitest';

import {
  dueReminders,
  eventsInReminderWindow,
  readEventBaseline,
  reminderBody,
  writeEventBaseline,
  EVENT_WINDOW_AFTER_MS,
  EVENT_WINDOW_BEFORE_MS,
} from './event-notifications';

const NOW = Date.UTC(2026, 8, 6, 12, 0, 0); // 2026-09-06 12:00Z

function ev(id: string, dtstart: string, isAllDay = false) {
  return { id, dtstart, isAllDay, summary: `event ${id}` };
}

describe('eventsInReminderWindow', () => {
  it('includes starts within [now-30min, now+15min]', () => {
    const inStart = new Date(NOW + EVENT_WINDOW_BEFORE_MS).toISOString(); // exactly +15
    const inPast = new Date(NOW - EVENT_WINDOW_AFTER_MS + 1).toISOString();
    const events = [ev('a', inStart), ev('b', inPast)];
    const hits = eventsInReminderWindow(events, NOW);
    expect(hits.map((h) => h.id)).toEqual(['a', 'b']);
  });

  it('excludes starts outside the window', () => {
    const tooLate = new Date(NOW + EVENT_WINDOW_BEFORE_MS + 1).toISOString();
    const tooOld = new Date(NOW - EVENT_WINDOW_AFTER_MS - 1).toISOString();
    expect(eventsInReminderWindow([ev('a', tooLate), ev('b', tooOld)], NOW)).toEqual([]);
  });

  it('skips all-day events and unparseable starts', () => {
    const midnight = new Date(NOW).toISOString();
    expect(eventsInReminderWindow([ev('all', midnight, true)], NOW)).toEqual([]);
    expect(eventsInReminderWindow([ev('none', '')], NOW)).toEqual([]);
  });
});

describe('dueReminders', () => {
  const dtstart = new Date(NOW + 10 * 60_000).toISOString();

  it('first run seeds silently (no storm for a full schedule)', () => {
    const { notify, seed } = dueReminders([ev('a', dtstart)], {}, NOW);
    expect(seed).toBe(true);
    expect(notify).toEqual([]);
  });

  it('notifies only unseen in-window events', () => {
    const { notify } = dueReminders([ev('a', dtstart), ev('b', dtstart)], { a: NOW }, NOW);
    expect(notify.map((n) => n.id)).toEqual(['b']);
  });
});

describe('reminderBody', () => {
  const e = (startMs: number) => ({ id: 'x', isAllDay: false, summary: 'Standup', startMs });

  it('phrases upcoming, now, and overdue per locale', () => {
    expect(reminderBody(e(NOW + 10 * 60_000), NOW, 'en')).toBe('in 10 minutes · Standup');
    expect(reminderBody(e(NOW), NOW, 'zh')).toBe('现在 · Standup');
    expect(reminderBody(e(NOW - 5 * 60_000), NOW, 'zh')).toBe('已开始 5 分钟 · Standup');
    expect(reminderBody(e(NOW - 60_000), NOW, 'en')).toBe('started 1 minute ago · Standup');
  });

  it('survives empty summaries', () => {
    expect(reminderBody({ ...e(NOW + 60_000), summary: '' }, NOW, 'en')).toBe('in 1 minute');
  });
});

describe('baseline persistence', () => {
  beforeEach(() => localStorage.clear());

  it('round-trips and prunes stale markers', () => {
    writeEventBaseline({ fresh: NOW, stale: NOW - 25 * 60 * 60_000 }, NOW);
    expect(readEventBaseline()).toEqual({ fresh: NOW });
  });
});
