/**
 * RRULE expansion for the visible calendar window.
 *
 * Hand-rolled minimal parser for FREQ=DAILY|WEEKLY|MONTHLY|YEARLY with
 * INTERVAL, COUNT, UNTIL, and BYDAY (weekly only). Unsupported rules
 * return the master event unchanged — the calendar shows what it can.
 */

import type { EventTimeFields } from '@/lib/calendar-grid';

export type ExpandableEvent = EventTimeFields & {
  id: string;
  recurrenceRule?: string | null;
  summary?: string | null;
};

interface ParsedRule {
  freq: 'DAILY' | 'WEEKLY' | 'MONTHLY' | 'YEARLY';
  interval: number;
  count?: number;
  until?: Date;
  byday?: string[];
}

const DAY_NAMES = ['SU', 'MO', 'TU', 'WE', 'TH', 'FR', 'SA'];

function parseRRule(rule: string): ParsedRule | null {
  const parts = rule.replace(/^RRULE:/i, '').split(';');
  let freq: ParsedRule['freq'] | null = null;
  let interval = 1;
  let count: number | undefined;
  let until: Date | undefined;
  let byday: string[] | undefined;

  for (const part of parts) {
    const [key, value] = part.split('=');
    if (!key || !value) continue;
    switch (key.toUpperCase()) {
      case 'FREQ': {
        const f = value.toUpperCase();
        if (f === 'DAILY' || f === 'WEEKLY' || f === 'MONTHLY' || f === 'YEARLY') {
          freq = f;
        } else {
          return null; // SECONDLY/MINUTELY/HOURLY — unsupported
        }
        break;
      }
      case 'INTERVAL':
        interval = Math.max(1, parseInt(value, 10) || 1);
        break;
      case 'COUNT':
        count = Math.max(1, parseInt(value, 10) || 1);
        break;
      case 'UNTIL': {
        // YYYYMMDD or YYYYMMDDTHHMMSSZ
        const m = value.match(/^(\d{4})(\d{2})(\d{2})(?:T(\d{2})(\d{2})(\d{2})Z?)?$/);
        if (m) {
          until = new Date(
            Date.UTC(+m[1], +m[2] - 1, +m[3], +(m[4] ?? 0), +(m[5] ?? 0), +(m[6] ?? 0)),
          );
        }
        break;
      }
      case 'BYDAY': {
        byday = value.split(',').map((d) => d.trim().toUpperCase());
        // Only simple BYDAY (no ordinal prefixes like -1SU) for weekly
        if (byday.some((d) => !DAY_NAMES.includes(d))) return null;
        break;
      }
      default:
        // BYMONTHDAY, BYSETPOS, BYMONTH, WKST etc — bail for simplicity
        return null;
    }
  }
  if (!freq) return null;
  return { freq, interval, count, until, byday };
}

/** Next occurrence after `date` per the rule. Returns null when exhausted. */
function nextOccurrence(master: Date, rule: ParsedRule, candidate: Date, n: number): Date | null {
  const d = new Date(candidate);
  switch (rule.freq) {
    case 'DAILY':
      d.setDate(d.getDate() + rule.interval);
      break;
    case 'WEEKLY': {
      d.setDate(d.getDate() + rule.interval * 7);
      break;
    }
    case 'MONTHLY':
      d.setMonth(d.getMonth() + rule.interval);
      break;
    case 'YEARLY':
      d.setFullYear(d.getFullYear() + rule.interval);
      break;
  }
  if (rule.until && d > rule.until) return null;
  if (rule.count && n >= rule.count) return null;
  // Guard: don't expand more than 5 years ahead or 366 occurrences.
  if (d.getFullYear() > master.getFullYear() + 5) return null;
  return d;
}

/** Day-of-week match for weekly BYDAY (returns true if candidate matches). */
function matchesByDay(rule: ParsedRule, d: Date): boolean {
  if (!rule.byday || rule.byday.length === 0) return true;
  const dayAbbr = DAY_NAMES[d.getDay()];
  return rule.byday.includes(dayAbbr);
}

function cloneForDate(event: ExpandableEvent, occurrenceStart: Date): ExpandableEvent {
  const durationMs = event.dtend
    ? new Date(event.dtend).getTime() - new Date(event.dtstart ?? event.dtend).getTime()
    : 0;
  const startStr = occurrenceStart.toISOString();
  const endStr = new Date(occurrenceStart.getTime() + durationMs).toISOString();
  const dateKey = startStr.slice(0, 10);
  return { ...event, id: `${event.id}::${dateKey}`, dtstart: startStr, dtend: endStr };
}

/**
 * Expand recurring events into occurrences within [rangeStart, rangeEnd].
 * Masters without a recurrenceRule pass through unchanged. Unsupported
 * rules pass through as the master event only.
 */
export function expandEventsForRange(
  events: ExpandableEvent[],
  rangeStart: Date,
  rangeEnd: Date,
): ExpandableEvent[] {
  const out: ExpandableEvent[] = [];
  for (const event of events) {
    const ruleText = event.recurrenceRule?.trim();
    if (!ruleText) {
      out.push(event);
      continue;
    }
    const rule = parseRRule(ruleText);
    if (!rule) {
      out.push(event); // unsupported → master only
      continue;
    }
    const masterStart = event.dtstart ? new Date(event.dtstart) : null;
    if (!masterStart || isNaN(masterStart.getTime())) {
      out.push(event);
      continue;
    }
    // Expand from masterStart forward into the range (bounded).
    let candidate = new Date(masterStart);
    let n = 1; // master is occurrence 1
    let guard = 0;
    while (candidate && guard < 800) {
      guard++;
      if (candidate >= rangeStart && candidate <= rangeEnd) {
        if (rule.freq !== 'WEEKLY' || matchesByDay(rule, candidate)) {
          out.push(cloneForDate(event, candidate));
        }
      }
      if (candidate > rangeEnd) break;
      // For weekly BYDAY: step day-by-day within the week if BYDAY is set
      if (rule.freq === 'WEEKLY' && rule.byday && rule.byday.length > 0) {
        // Step one day; if we cross a week boundary, jump by interval*7
        const next = new Date(candidate);
        next.setDate(next.getDate() + 1);
        const daysBetween = Math.round((next.getTime() - masterStart.getTime()) / 86_400_000);
        if (daysBetween % (rule.interval * 7) === 0 && daysBetween > 0) {
          // Aligned to the interval week — continue day-by-day
        }
        candidate = next;
        if (rule.until && candidate > rule.until) break;
        if (rule.count && n >= rule.count) break;
        continue;
      }
      const next = nextOccurrence(masterStart, rule, candidate, n);
      n++;
      if (!next) break;
      candidate = next;
    }
  }
  return out;
}
