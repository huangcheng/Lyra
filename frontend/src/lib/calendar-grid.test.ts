import { describe, expect, it } from 'vitest';

import {
  addViewOffset,
  eventOccursOnDay,
  eventOverlapsLocalDay,
  eventSpanDays,
  eventsForDay,
  eventsStartingOnDay,
  hourSlots,
  monthGridDays,
  spansMultipleDays,
  startOfWeekMonday,
  viewTitle,
  visibleRangeIso,
  weekDays,
  yearMonths,
} from './calendar-grid';

describe('startOfWeekMonday', () => {
  it('returns Monday for a Wednesday', () => {
    // 2026-09-04 is Friday; 2026-09-02 is Wednesday
    const wed = new Date(2026, 8, 2, 15, 30);
    const mon = startOfWeekMonday(wed);
    expect(mon.getFullYear()).toBe(2026);
    expect(mon.getMonth()).toBe(7); // August
    expect(mon.getDate()).toBe(31); // Aug 31 2026 is Monday of that week
    expect(mon.getDay()).toBe(1);
    expect(mon.getHours()).toBe(0);
  });

  it('keeps Monday as itself', () => {
    const mon = new Date(2026, 7, 31); // Aug 31 2026 Monday
    const got = startOfWeekMonday(mon);
    expect(got.getDate()).toBe(31);
    expect(got.getMonth()).toBe(7);
  });
});

describe('monthGridDays', () => {
  it('starts on Monday and covers September 2026', () => {
    const days = monthGridDays(2026, 8);
    expect(days[0]!.getDay()).toBe(1);
    expect(days.length === 35 || days.length === 42).toBe(true);
    const hasSep1 = days.some(
      (d) => d.getFullYear() === 2026 && d.getMonth() === 8 && d.getDate() === 1,
    );
    const hasSep30 = days.some(
      (d) => d.getFullYear() === 2026 && d.getMonth() === 8 && d.getDate() === 30,
    );
    expect(hasSep1).toBe(true);
    expect(hasSep30).toBe(true);
  });
});

describe('weekDays', () => {
  it('returns seven Mon–Sun days', () => {
    const days = weekDays(new Date(2026, 8, 4));
    expect(days).toHaveLength(7);
    expect(days[0]!.getDay()).toBe(1);
    expect(days[6]!.getDay()).toBe(0);
  });
});

describe('addViewOffset', () => {
  it('shifts month/week/day', () => {
    const anchor = new Date(2026, 8, 15);
    expect(addViewOffset(anchor, 'month', 1).getMonth()).toBe(9);
    expect(addViewOffset(anchor, 'week', 1).getDate()).toBe(22);
    expect(addViewOffset(anchor, 'day', -1).getDate()).toBe(14);
  });
});

describe('eventOccursOnDay', () => {
  it('matches single-day timed events', () => {
    const ev = {
      dtstart: '2026-09-04T10:00:00Z',
      dtend: '2026-09-04T11:00:00Z',
      isAllDay: false,
    };
    expect(eventOccursOnDay(ev, new Date(2026, 8, 4))).toBe(true);
    expect(eventOccursOnDay(ev, new Date(2026, 8, 5))).toBe(false);
  });

  it('matches multi-day spans inclusive of start day', () => {
    const ev = {
      dtstart: '2026-09-03T00:00:00Z',
      dtend: '2026-09-05T00:00:00Z',
      isAllDay: true,
    };
    expect(eventOccursOnDay(ev, new Date(2026, 8, 3))).toBe(true);
    expect(eventOccursOnDay(ev, new Date(2026, 8, 4))).toBe(true);
    // end exclusive for all-day (iCal DTEND next day)
    expect(eventOccursOnDay(ev, new Date(2026, 8, 5))).toBe(false);
  });
});

describe('eventsForDay / hourSlots / viewTitle', () => {
  it('filters and titles', () => {
    const events = [
      {
        id: '1',
        dtstart: '2026-09-04T12:00:00Z',
        dtend: '2026-09-04T13:00:00Z',
        isAllDay: false,
      },
      {
        id: '2',
        dtstart: '2026-09-05T12:00:00Z',
        dtend: '2026-09-05T13:00:00Z',
        isAllDay: false,
      },
    ];
    expect(eventsForDay(events, new Date(2026, 8, 4)).map((e) => e.id)).toEqual(['1']);
    expect(hourSlots()).toHaveLength(24);
    expect(hourSlots()[0]).toBe(0);
    expect(viewTitle(new Date(2026, 8, 4), 'month', 'en-US')).toMatch(/2026/);
  });
});

describe('eventSpanDays', () => {
  it('returns single day for same-day timed event', () => {
    const ev = { dtstart: '2026-09-15T10:00:00', dtend: '2026-09-15T11:00:00', isAllDay: false };
    expect(eventSpanDays(ev)).toHaveLength(1);
  });

  it('spans 3 days for an all-day holiday (dtend exclusive)', () => {
    const ev = { dtstart: '2026-09-15', dtend: '2026-09-18', isAllDay: true };
    expect(eventSpanDays(ev)).toHaveLength(3); // 15, 16, 17
  });

  it('spans 2 days for a timed event crossing midnight', () => {
    const ev = { dtstart: '2026-09-15T22:00:00', dtend: '2026-09-16T02:00:00', isAllDay: false };
    const days = eventSpanDays(ev);
    expect(days).toHaveLength(2);
    expect(days[0].getDate()).toBe(15);
    expect(days[1].getDate()).toBe(16);
  });
});

describe('eventOverlapsLocalDay', () => {
  it('matches the start day', () => {
    const ev = { dtstart: '2026-09-15T10:00:00', dtend: '2026-09-15T11:00:00', isAllDay: false };
    expect(eventOverlapsLocalDay(ev, new Date(2026, 8, 15))).toBe(true);
  });

  it('does not match a different day', () => {
    const ev = { dtstart: '2026-09-15T10:00:00', dtend: '2026-09-15T11:00:00', isAllDay: false };
    expect(eventOverlapsLocalDay(ev, new Date(2026, 8, 16))).toBe(false);
  });

  it('matches day 2 of a midnight-crossing event', () => {
    const ev = { dtstart: '2026-09-15T22:00:00', dtend: '2026-09-16T02:00:00', isAllDay: false };
    expect(eventOverlapsLocalDay(ev, new Date(2026, 8, 16))).toBe(true);
  });

  it('matches middle day of a 3-day all-day span', () => {
    const ev = { dtstart: '2026-09-15', dtend: '2026-09-18', isAllDay: true };
    expect(eventOverlapsLocalDay(ev, new Date(2026, 8, 16))).toBe(true);
    expect(eventOverlapsLocalDay(ev, new Date(2026, 8, 18))).toBe(false); // exclusive
  });
});

describe('multi-day span anchoring', () => {
  // Regression: a TickTick range task (Sep 1 → Oct 2) chipped EVERY day of
  // September. Month cells must anchor it once, on its first local day.
  const span = {
    dtstart: '2026-09-01 00:00:00+00',
    dtend: '2026-10-02 00:00:00+00',
    isAllDay: false,
  };
  const single = {
    dtstart: '2026-09-08 09:00:00+00',
    dtend: '2026-09-08 10:00:00+00',
    isAllDay: false,
  };

  it('flags only spans crossing a local midnight', () => {
    expect(spansMultipleDays(span)).toBe(true);
    expect(spansMultipleDays(single)).toBe(false);
    expect(spansMultipleDays({ dtstart: null, dtend: null, isAllDay: false })).toBe(false);
  });

  it('anchors the chip on the first occupied day only', () => {
    const events = [span, single];
    expect(eventsStartingOnDay(events, new Date(2026, 8, 1)).map((e) => e.dtstart)).toEqual([
      span.dtstart,
    ]);
    expect(eventsStartingOnDay(events, new Date(2026, 8, 8)).map((e) => e.dtstart)).toEqual([
      single.dtstart,
    ]);
    // no repetition on covered-but-later days
    expect(eventsStartingOnDay(events, new Date(2026, 8, 15))).toEqual([]);
  });
});

describe('year view helpers', () => {
  it('steps whole years and lists twelve month anchors', () => {
    const a = new Date(2026, 8, 15);
    const next = addViewOffset(a, 'year', 1);
    expect(next.getFullYear()).toBe(2027);
    expect(next.getMonth()).toBe(8);
    const months = yearMonths(a);
    expect(months).toHaveLength(12);
    expect(months[0]!.getMonth()).toBe(0);
    expect(months[11]!.getMonth()).toBe(11);
    months.forEach((m) => expect(m.getFullYear()).toBe(2026));
  });

  it('year visible range spans Jan 1 to next Jan 1', () => {
    const { start, end } = visibleRangeIso(new Date(2026, 5, 1), 'year');
    expect(start).toBe(new Date(2026, 0, 1).toISOString());
    expect(end).toBe(new Date(2027, 0, 1).toISOString());
  });
});
