import { describe, expect, it } from 'vitest';

import { expandEventsForRange, type ExpandableEvent } from '@/lib/calendar-rrule';

function event(overrides: Partial<ExpandableEvent> = {}): ExpandableEvent {
  return {
    id: 'e1',
    dtstart: '2026-09-01T09:00:00.000Z',
    dtend: '2026-09-01T10:00:00.000Z',
    isAllDay: false,
    summary: 'Standup',
    ...overrides,
  };
}

const SEP_START = new Date('2026-09-01T00:00:00Z');
const SEP_END = new Date('2026-09-30T23:59:59Z');

describe('expandEventsForRange', () => {
  it('passes non-recurring events through unchanged', () => {
    const ev = event();
    const result = expandEventsForRange([ev], SEP_START, SEP_END);
    expect(result).toEqual([ev]);
  });

  it('expands weekly RRULE into September occurrences', () => {
    // Every Tuesday in September 2026 (Sep 1 is a Tuesday)
    const ev = event({ recurrenceRule: 'RRULE:FREQ=WEEKLY' });
    const result = expandEventsForRange([ev], SEP_START, SEP_END);
    expect(result.length).toBeGreaterThanOrEqual(4); // at least 4 Tuesdays
    expect(result.length).toBeLessThanOrEqual(5); // at most 5
    expect(result[0].id).toContain('::');
    expect(result.every((r) => r.id.startsWith('e1::'))).toBe(true);
  });

  it('expands daily RRULE with INTERVAL=2 (every other day)', () => {
    const ev = event({ recurrenceRule: 'RRULE:FREQ=DAILY;INTERVAL=2' });
    const result = expandEventsForRange([ev], SEP_START, SEP_END);
    expect(result.length).toBeGreaterThanOrEqual(14); // ~15 for 30 days / 2
  });

  it('honors COUNT limit', () => {
    const ev = event({ recurrenceRule: 'RRULE:FREQ=DAILY;COUNT=3' });
    const result = expandEventsForRange([ev], SEP_START, SEP_END);
    expect(result).toHaveLength(3);
  });

  it('honors UNTIL date', () => {
    const ev = event({ recurrenceRule: 'RRULE:FREQ=DAILY;UNTIL=20260905T000000Z' });
    const result = expandEventsForRange([ev], SEP_START, SEP_END);
    expect(result.length).toBeGreaterThanOrEqual(4); // Sep 1-4 (Sep 5 boundary)
    expect(result.length).toBeLessThanOrEqual(5);
  });

  it('supports yearly RRULE', () => {
    const ev = event({
      dtstart: '2025-09-15T09:00:00.000Z',
      recurrenceRule: 'RRULE:FREQ=YEARLY',
    });
    const result = expandEventsForRange([ev], SEP_START, SEP_END);
    expect(result).toHaveLength(1); // one occurrence in Sep 2026
    expect(result[0].dtstart).toContain('2026-09-15');
  });

  it('returns master only for unsupported BYMONTHDAY=32', () => {
    const ev = event({ recurrenceRule: 'RRULE:FREQ=MONTHLY;BYMONTHDAY=32' });
    const result = expandEventsForRange([ev], SEP_START, SEP_END);
    expect(result).toEqual([ev]); // unchanged master
  });

  it('returns master only for SECONDLY frequency', () => {
    const ev = event({ recurrenceRule: 'RRULE:FREQ=SECONDLY' });
    const result = expandEventsForRange([ev], SEP_START, SEP_END);
    expect(result).toEqual([ev]);
  });

  it('preserves event duration across expansions', () => {
    const ev = event({
      dtstart: '2026-09-01T09:00:00.000Z',
      dtend: '2026-09-01T11:30:00.000Z',
      recurrenceRule: 'RRULE:FREQ=DAILY;COUNT=2',
    });
    const result = expandEventsForRange([ev], SEP_START, SEP_END);
    expect(result).toHaveLength(2);
    const first = new Date(result[0].dtstart!);
    const firstEnd = new Date(result[0].dtend!);
    expect(firstEnd.getTime() - first.getTime()).toBe(2.5 * 3600 * 1000);
  });

  it('expands weekly with BYDAY=MO,WE,FRI', () => {
    const ev = event({
      dtstart: '2026-09-02T09:00:00.000Z', // Wednesday
      recurrenceRule: 'RRULE:FREQ=WEEKLY;BYDAY=MO,WE,FR',
    });
    const result = expandEventsForRange([ev], SEP_START, SEP_END);
    // Should get ~3 per week × ~4.3 weeks ≈ 13
    expect(result.length).toBeGreaterThanOrEqual(12);
    expect(result.length).toBeLessThanOrEqual(14);
  });
});
