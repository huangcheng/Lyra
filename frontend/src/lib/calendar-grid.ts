/**
 * Calendar grid date helpers (Monday-start weeks; local timezone).
 */

export type CalendarView = 'month' | 'week' | 'day';

export type EventTimeFields = {
  dtstart?: string | null;
  dtend?: string | null;
  isAllDay: boolean;
};

function atLocalMidnight(d: Date): Date {
  return new Date(d.getFullYear(), d.getMonth(), d.getDate());
}

/** Local midnight of the Monday on or before `d`. */
export function startOfWeekMonday(d: Date): Date {
  const day = atLocalMidnight(d);
  const dow = day.getDay(); // 0 Sun … 6 Sat
  const offset = dow === 0 ? -6 : 1 - dow;
  day.setDate(day.getDate() + offset);
  return day;
}

/** 35 or 42 days covering `monthIndex` (0–11), starting Monday. */
export function monthGridDays(year: number, monthIndex: number): Date[] {
  const first = new Date(year, monthIndex, 1);
  const start = startOfWeekMonday(first);
  const last = new Date(year, monthIndex + 1, 0);
  const end = startOfWeekMonday(last);
  end.setDate(end.getDate() + 6);
  const days: Date[] = [];
  const cur = new Date(start);
  while (cur <= end) {
    days.push(new Date(cur));
    cur.setDate(cur.getDate() + 1);
  }
  // Prefer 6 weeks when the month spills past 5 weeks
  while (days.length < 35) {
    days.push(new Date(cur));
    cur.setDate(cur.getDate() + 1);
  }
  return days;
}

export function weekDays(anchor: Date): Date[] {
  const start = startOfWeekMonday(anchor);
  return Array.from({ length: 7 }, (_, i) => {
    const d = new Date(start);
    d.setDate(start.getDate() + i);
    return d;
  });
}

export function addViewOffset(anchor: Date, view: CalendarView, delta: number): Date {
  const next = new Date(anchor);
  if (view === 'month') {
    next.setMonth(next.getMonth() + delta);
  } else if (view === 'week') {
    next.setDate(next.getDate() + delta * 7);
  } else {
    next.setDate(next.getDate() + delta);
  }
  return next;
}

export function viewTitle(anchor: Date, view: CalendarView, locale: string): string {
  if (view === 'month') {
    return anchor.toLocaleDateString(locale, { month: 'long', year: 'numeric' });
  }
  if (view === 'day') {
    return anchor.toLocaleDateString(locale, {
      weekday: 'long',
      month: 'long',
      day: 'numeric',
      year: 'numeric',
    });
  }
  const days = weekDays(anchor);
  const a = days[0]!;
  const b = days[6]!;
  const sameMonth = a.getMonth() === b.getMonth();
  if (sameMonth) {
    return `${a.toLocaleDateString(locale, { month: 'long', year: 'numeric' })} · ${a.getDate()}–${b.getDate()}`;
  }
  return `${a.toLocaleDateString(locale, { month: 'short', day: 'numeric' })} – ${b.toLocaleDateString(locale, { month: 'short', day: 'numeric', year: 'numeric' })}`;
}

function dayBounds(day: Date): { start: number; end: number } {
  const start = atLocalMidnight(day).getTime();
  const end = start + 24 * 60 * 60 * 1000;
  return { start, end };
}

function dayKeyLocal(d: Date): number {
  return d.getFullYear() * 10_000 + (d.getMonth() + 1) * 100 + d.getDate();
}

/** Calendar date key for all-day stamps (UTC Y-M-D — matches iCal DATE / midnight Z). */
function dayKeyUtc(d: Date): number {
  return d.getUTCFullYear() * 10_000 + (d.getUTCMonth() + 1) * 100 + d.getUTCDate();
}

/**
 * True if the event overlaps the local calendar day.
 * All-day: treat `dtend` as exclusive (iCal DATE); compare by calendar date.
 * Timed: range overlap in local time.
 */
export function eventOccursOnDay(event: EventTimeFields, day: Date): boolean {
  if (!event.dtstart) return false;
  const start = new Date(event.dtstart);
  if (Number.isNaN(start.getTime())) return false;

  if (event.isAllDay) {
    const startKey = dayKeyUtc(start);
    let endKey: number;
    if (event.dtend) {
      const end = new Date(event.dtend);
      endKey = Number.isNaN(end.getTime()) ? startKey + 1 : dayKeyUtc(end);
    } else {
      endKey = startKey + 1;
    }
    const key = dayKeyLocal(day);
    return key >= startKey && key < endKey;
  }

  const startMs = start.getTime();
  let endMs: number;
  if (event.dtend) {
    endMs = new Date(event.dtend).getTime();
    if (Number.isNaN(endMs)) endMs = startMs + 60 * 60 * 1000;
  } else {
    endMs = startMs + 60 * 60 * 1000;
  }
  const { start: dayStart, end: dayEnd } = dayBounds(day);
  return startMs < dayEnd && endMs > dayStart;
}

export function eventsForDay<T extends EventTimeFields>(events: T[], day: Date): T[] {
  return events.filter((e) => eventOverlapsLocalDay(e, day));
}

export function hourSlots(): number[] {
  return Array.from({ length: 24 }, (_, i) => i);
}

/** Visible range ISO bounds for event fetch (local → UTC ISO). */
export function visibleRangeIso(anchor: Date, view: CalendarView): { start: string; end: string } {
  if (view === 'month') {
    const days = monthGridDays(anchor.getFullYear(), anchor.getMonth());
    const start = days[0]!;
    const end = new Date(days[days.length - 1]!);
    end.setDate(end.getDate() + 1);
    return { start: start.toISOString(), end: end.toISOString() };
  }
  if (view === 'week') {
    const days = weekDays(anchor);
    const end = new Date(days[6]!);
    end.setDate(end.getDate() + 1);
    return { start: days[0]!.toISOString(), end: end.toISOString() };
  }
  const start = atLocalMidnight(anchor);
  const end = new Date(start);
  end.setDate(end.getDate() + 1);
  return { start: start.toISOString(), end: end.toISOString() };
}

export function sameLocalDay(a: Date, b: Date): boolean {
  return (
    a.getFullYear() === b.getFullYear() &&
    a.getMonth() === b.getMonth() &&
    a.getDate() === b.getDate()
  );
}

/**
 * Inclusive local calendar dates the event occupies.
 * All-day: dtend is exclusive (RFC 5545) → last day = dtend − 1 day.
 * Timed: split by local calendar date of start/end.
 */
export function eventSpanDays(event: EventTimeFields): Date[] {
  const start = event.dtstart ? new Date(event.dtstart) : null;
  const end = event.dtend ? new Date(event.dtend) : null;
  if (!start || isNaN(start.getTime())) return [];
  if (!end || isNaN(end.getTime())) return [atLocalMidnight(start)];

  const startDay = atLocalMidnight(start);
  let endDay = atLocalMidnight(end);
  if (event.isAllDay && endDay > startDay) {
    endDay.setDate(endDay.getDate() - 1); // exclusive end
  }
  const days: Date[] = [];
  const cur = new Date(startDay);
  while (cur <= endDay) {
    days.push(new Date(cur));
    cur.setDate(cur.getDate() + 1);
  }
  return days;
}

/** True if the event's time interval intersects the local day [day, day+1). */
export function eventOverlapsLocalDay(event: EventTimeFields, day: Date): boolean {
  const start = event.dtstart ? new Date(event.dtstart) : null;
  const end = event.dtend ? new Date(event.dtend) : null;
  if (!start || isNaN(start.getTime())) return false;
  const dayStart = atLocalMidnight(day);
  const dayEnd = new Date(dayStart);
  dayEnd.setDate(dayEnd.getDate() + 1);
  const evStart = event.isAllDay ? atLocalMidnight(start) : start;
  const evEnd =
    !end || isNaN(end.getTime())
      ? new Date(evStart.getTime() + 3600_000)
      : event.isAllDay
        ? atLocalMidnight(end)
        : end;
  return evStart < dayEnd && evEnd > dayStart;
}
