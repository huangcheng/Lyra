/**
 * Calendar subsystem — CalDAV sources + ICS / webcal subscriptions.
 *
 * Peer destination to Mail. One left panel (brand header + mini month +
 * sources, dida/Notion pattern), main area with day/week/month/year views.
 * Amber is reserved for "today"; events wear their source color as a
 * tinted chip, never as a solid block.
 */

import {
  useState,
  useEffect,
  useMemo,
  type CSSProperties,
  type FormEvent,
  type ReactNode,
} from 'react';
import {
  Calendar as CalendarIcon,
  ChevronLeft,
  ChevronRight,
  Clock,
  MapPin,
  Plus,
  RefreshCw,
  Trash2,
  X,
} from 'lucide-react';
import { t } from '../i18n';
import type { SupportedLocale } from '../types';
import { api } from '../lib/api-client';
import {
  addViewOffset,
  eventsForDay,
  eventsStartingOnDay,
  hourSlots,
  monthGridDays,
  sameLocalDay,
  spansMultipleDays,
  viewTitle,
  visibleRangeIso,
  weekDays,
  yearMonths,
  type CalendarView,
} from '@/lib/calendar-grid';
import { useNavigate } from '@tanstack/react-router';
import { EmptyState } from './empty-state';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { SlimPanelHeader } from '@/components/slim-page-nav';
import { cn } from '@/lib/utils';
import { useUIStore } from '../stores/ui';
import { expandEventsForRange, type ExpandableEvent } from '@/lib/calendar-rrule';

type SourceKind = 'caldav' | 'ics';

interface CalSource {
  kind: SourceKind;
  id: string;
  /** Present for CalDAV only. */
  accountId?: string;
  name: string;
  color?: string;
  url?: string;
  lastError?: string | null;
}

interface CalEvent {
  id: string;
  summary?: string;
  description?: string;
  dtstart?: string;
  dtend?: string;
  location?: string;
  isAllDay: boolean;
  _sourceId?: string;
  _color?: string;
}

interface SubApi {
  id: string;
  url: string;
  name: string;
  color?: string;
  isActive: boolean;
  lastError?: string | null;
}

interface CalApi {
  id: string;
  accountId: string;
  name: string;
  color?: string;
  isActive: boolean;
}

const WEEKDAY_ORDER = ['mon', 'tue', 'wed', 'thu', 'fri', 'sat', 'sun'] as const;
const VIEW_ORDER: CalendarView[] = ['day', 'week', 'month', 'year'];

/** Fastmail's tasks collection leaks this server-side constant as its
 * display name; show it as what it is instead. */
const TASKS_CALENDAR_NAME = 'DEFAULT_TASK_CALENDAR_NAME';

function displayName(locale: SupportedLocale, name: string): string {
  return name === TASKS_CALENDAR_NAME ? t(locale, 'calendar.tasks') : name;
}

/** Tinted chip background from a source color (quiet, never solid). */
function tint(color: string | undefined, strength: number): string {
  const c = color || 'var(--unread)';
  return `color-mix(in srgb, ${c} ${strength}%, transparent)`;
}

/** Narrow weekday initials (一二三四五六日 / MTWTFSS). */
function weekdayInitials(locTag: string): string[] {
  // 2023-05-01 was a Monday.
  return Array.from({ length: 7 }, (_, i) =>
    new Date(2023, 4, i + 1).toLocaleDateString(locTag, { weekday: 'narrow' }),
  );
}

export function CalendarPage() {
  const locale = useUIStore((s) => s.locale);
  const locTag = locale === 'zh' ? 'zh-CN' : 'en-US';
  const [sources, setSources] = useState<CalSource[]>([]);
  const [visibleIds, setVisibleIds] = useState<Set<string>>(new Set());
  const [events, setEvents] = useState<CalEvent[]>([]);
  const navigate = useNavigate();
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [view, setView] = useState<CalendarView>('month');
  const [anchor, setAnchor] = useState(() => new Date());
  const [selectedEvent, setSelectedEvent] = useState<CalEvent | null>(null);
  const [now, setNow] = useState(() => new Date());
  const [addOpen, setAddOpen] = useState(false);
  const [addUrl, setAddUrl] = useState('');
  const [addName, setAddName] = useState('');
  const [addBusy, setAddBusy] = useState(false);
  const [addError, setAddError] = useState<string | null>(null);

  useEffect(() => {
    const id = window.setInterval(() => setNow(new Date()), 60_000);
    return () => window.clearInterval(id);
  }, []);

  async function loadSources(): Promise<CalSource[]> {
    const [cals, subs] = await Promise.all([
      api<CalApi[]>('/calendars'),
      api<SubApi[]>('/calendar-subscriptions').catch(() => [] as SubApi[]),
    ]);
    const merged: CalSource[] = [
      ...cals.map((c) => ({
        kind: 'caldav' as const,
        id: c.id,
        accountId: c.accountId,
        name: c.name,
        color: c.color,
      })),
      ...subs.map((s) => ({
        kind: 'ics' as const,
        id: s.id,
        name: s.name,
        color: s.color,
        url: s.url,
        lastError: s.lastError,
      })),
    ];
    setSources(merged);
    return merged;
  }

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        setLoading(true);
        setError(null);
        const merged = await loadSources();
        if (cancelled) return;
        setVisibleIds(new Set(merged.map((c) => c.id)));
      } catch (err: unknown) {
        if (!cancelled) {
          setError(err instanceof Error ? err.message : String(err));
        }
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  const colorById = useMemo(() => {
    const m = new Map<string, string>();
    for (const c of sources) {
      m.set(c.id, c.color || 'var(--unread)');
    }
    return m;
  }, [sources]);

  const kindById = useMemo(() => {
    const m = new Map<string, SourceKind>();
    for (const c of sources) m.set(c.id, c.kind);
    return m;
  }, [sources]);

  async function loadEvents(ids: Set<string>, when: Date, v: CalendarView) {
    if (ids.size === 0) {
      setEvents([]);
      return;
    }
    const { start, end } = visibleRangeIso(when, v);
    const q = `start=${encodeURIComponent(start)}&end=${encodeURIComponent(end)}`;
    const chunks = await Promise.all(
      [...ids].map(async (id) => {
        const kind = kindById.get(id) ?? 'caldav';
        const path =
          kind === 'ics'
            ? `/calendar-subscriptions/${id}/events?${q}`
            : `/calendars/${id}/events?${q}`;
        try {
          const rows = await api<CalEvent[]>(path);
          return rows.map((e) => ({
            ...e,
            _sourceId: id,
            _color: colorById.get(id) || 'var(--unread)',
          }));
        } catch {
          return [] as CalEvent[];
        }
      }),
    );
    const raw = chunks.flat();
    // Expand simple RRULEs (DAILY/WEEKLY/MONTHLY/YEARLY) into occurrences
    // within the visible window (±1 day padding for week spillover).
    const { start: rangeStartIso, end: rangeEndIso } = visibleRangeIso(anchor, view);
    const rangeStart = new Date(rangeStartIso);
    rangeStart.setDate(rangeStart.getDate() - 1);
    const rangeEnd = new Date(rangeEndIso);
    rangeEnd.setDate(rangeEnd.getDate() + 1);
    setEvents(expandEventsForRange(raw as ExpandableEvent[], rangeStart, rangeEnd) as CalEvent[]);
  }

  useEffect(() => {
    if (loading) return;
    let cancelled = false;
    void (async () => {
      setRefreshing(true);
      try {
        if (!cancelled) await loadEvents(visibleIds, anchor, view);
      } finally {
        if (!cancelled) setRefreshing(false);
      }
    })();
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps -- fetch drivers
  }, [loading, visibleIds, anchor, view, sources]);

  function toggleSource(id: string) {
    setVisibleIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  async function refresh() {
    setRefreshing(true);
    try {
      const accountIds = [
        ...new Set(
          sources
            .filter((s) => s.kind === 'caldav')
            .map((s) => s.accountId!)
            .filter(Boolean),
        ),
      ];
      await Promise.all([
        ...accountIds.map((aid) => api(`/accounts/${aid}/calendars/sync`).catch(() => undefined)),
        ...sources
          .filter((s) => s.kind === 'ics')
          .map((s) =>
            api(`/calendar-subscriptions/${s.id}/refresh`, { method: 'POST' }).catch(
              () => undefined,
            ),
          ),
      ]);
      const merged = await loadSources();
      const nextVisible = new Set<string>();
      for (const s of merged) {
        if (visibleIds.has(s.id) || visibleIds.size === 0) nextVisible.add(s.id);
      }
      if (nextVisible.size === 0) merged.forEach((s) => nextVisible.add(s.id));
      setVisibleIds(nextVisible);
      await loadEvents(nextVisible, anchor, view);
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setRefreshing(false);
    }
  }

  async function submitAdd(e: FormEvent) {
    e.preventDefault();
    setAddBusy(true);
    setAddError(null);
    try {
      const created = await api<SubApi>('/calendar-subscriptions', {
        method: 'POST',
        body: JSON.stringify({
          url: addUrl.trim(),
          name: addName.trim() || undefined,
        }),
      });
      setAddOpen(false);
      setAddUrl('');
      setAddName('');
      await loadSources();
      setVisibleIds((prev) => new Set([...prev, created.id]));
      await loadEvents(new Set([...visibleIds, created.id]), anchor, view);
    } catch (err: unknown) {
      setAddError(err instanceof Error ? err.message : String(err));
    } finally {
      setAddBusy(false);
    }
  }

  async function removeSub(id: string) {
    await api(`/calendar-subscriptions/${id}`, { method: 'DELETE' });
    setVisibleIds((prev) => {
      const next = new Set(prev);
      next.delete(id);
      return next;
    });
    await loadSources();
  }

  function formatEventTime(event: CalEvent): string {
    if (event.isAllDay) return t(locale, 'calendar.allDay');
    if (!event.dtstart) return '';
    return new Date(event.dtstart).toLocaleTimeString(locTag, {
      hour: '2-digit',
      minute: '2-digit',
    });
  }

  function timedBlockStyle(event: CalEvent): CSSProperties {
    if (!event.dtstart || event.isAllDay) return {};
    const start = new Date(event.dtstart);
    const end = event.dtend ? new Date(event.dtend) : new Date(start.getTime() + 60 * 60 * 1000);
    const startMin = start.getHours() * 60 + start.getMinutes();
    const endMin = Math.max(startMin + 30, end.getHours() * 60 + end.getMinutes());
    const top = (startMin / (24 * 60)) * 100;
    const height = ((endMin - startMin) / (24 * 60)) * 100;
    return {
      top: `${top}%`,
      height: `${Math.max(height, 2)}%`,
      backgroundColor: tint(event._color, 12),
      borderLeft: `2px solid ${event._color || 'var(--unread)'}`,
    };
  }

  const nowMinutes = now.getHours() * 60 + now.getMinutes();
  const nowTop = (nowMinutes / (24 * 60)) * 100;

  /** Quiet event chip: source-colored dot on a tinted background. */
  function EventChip({
    event,
    continued,
    className,
  }: {
    event: CalEvent;
    continued?: boolean;
    className?: string;
  }) {
    const c = event._color || 'var(--unread)';
    return (
      <button
        key={event.id}
        type="button"
        className={cn(
          'flex w-full items-center gap-1.5 overflow-hidden rounded-[4px] px-1.5 py-[2px] text-left text-[10.5px] leading-tight text-foreground transition-colors hover:brightness-95 dark:hover:brightness-110',
          className,
        )}
        style={{ backgroundColor: tint(c, 14) }}
        onClick={() => setSelectedEvent(event)}
      >
        <span className="size-1.5 shrink-0 rounded-full" style={{ backgroundColor: c }} />
        <span className="truncate">{event.summary || t(locale, 'calendar.noTitle')}</span>
        {continued ? <span className="shrink-0 text-muted-foreground">→</span> : null}
      </button>
    );
  }

  /** Compact month navigator for the left panel (dida/Notion pattern):
   * today in amber, click a day to jump the anchor there. */
  function MiniMonth() {
    const year = anchor.getFullYear();
    const month = anchor.getMonth();
    const days = monthGridDays(year, month);
    return (
      <div className="mb-3 px-2.5">
        <div className="grid grid-cols-7">
          {weekdayInitials(locTag).map((w, i) => (
            <span key={i} className="pb-1 text-center text-[9.5px] text-muted-foreground/70">
              {w}
            </span>
          ))}
          {days.map((day, i) => {
            const inMonth = day.getMonth() === month;
            const isToday = sameLocalDay(day, now);
            return (
              <button
                key={i}
                type="button"
                className={cn(
                  'flex h-6 w-6 items-center justify-center justify-self-center rounded-full text-[10.5px] transition-colors hover:bg-accent',
                  !inMonth && 'invisible',
                  isToday && 'bg-[var(--unread)] font-semibold text-[#1a1b1f]',
                )}
                onClick={() => setAnchor(day)}
                aria-label={day.toLocaleDateString(locTag)}
              >
                {day.getDate()}
              </button>
            );
          })}
        </div>
      </div>
    );
  }

  function renderMonth() {
    const year = anchor.getFullYear();
    const month = anchor.getMonth();
    const days = monthGridDays(year, month);
    return (
      <div className="grid min-h-0 flex-1 grid-cols-7 grid-rows-[auto_repeat(6,minmax(0,1fr))] gap-px overflow-hidden bg-border/70">
        {WEEKDAY_ORDER.map((day) => (
          <div
            key={day}
            className="bg-background px-2.5 py-2 text-center text-[11px] font-medium text-muted-foreground"
          >
            {t(locale, `calendar.days.${day}`)}
          </div>
        ))}
        {days.map((day, i) => {
          const inMonth = day.getMonth() === month;
          // Adjacent-month spillover stays empty; multi-day events anchor
          // their chip on the first covered day (with →) instead of
          // repeating on every day of the span.
          const dayEvents = inMonth ? eventsStartingOnDay(events, day) : [];
          const isToday = sameLocalDay(day, now);
          return (
            <div key={i} className="flex min-h-20 flex-col gap-1 bg-background p-1.5">
              <span
                className={cn(
                  'flex h-6 w-6 shrink-0 items-center justify-center self-end text-xs',
                  isToday
                    ? 'rounded-full bg-[var(--unread)] font-semibold text-[#1a1b1f]'
                    : inMonth
                      ? 'text-foreground/75'
                      : 'text-muted-foreground/35',
                )}
              >
                {day.getDate()}
              </span>
              <div className="flex min-h-0 flex-1 flex-col gap-0.5 overflow-hidden">
                {dayEvents.slice(0, 3).map((event) => (
                  <EventChip key={event.id} event={event} continued={spansMultipleDays(event)} />
                ))}
                {dayEvents.length > 3 ? (
                  <span className="px-1.5 text-[10px] text-muted-foreground">
                    {t(locale, 'calendar.moreEvents', { count: dayEvents.length - 3 })}
                  </span>
                ) : null}
              </div>
            </div>
          );
        })}
      </div>
    );
  }

  /** Twelve mini months with per-day event dots; click a day → day view,
   * click a month label → month view (dida year-view pattern). */
  function renderYear() {
    const months = yearMonths(anchor);
    return (
      <div className="grid min-h-0 flex-1 auto-rows-min content-start gap-x-6 gap-y-5 overflow-y-auto p-5 lg:grid-cols-2 2xl:grid-cols-3">
        {months.map((m) => {
          const days = monthGridDays(m.getFullYear(), m.getMonth());
          return (
            <div key={m.getMonth()} className="flex flex-col gap-1.5">
              <button
                type="button"
                className="self-start rounded-[5px] px-1.5 py-0.5 text-left text-[12.5px] font-medium hover:bg-accent"
                onClick={() => {
                  setAnchor(m);
                  setView('month');
                }}
              >
                {m.toLocaleDateString(locTag, { month: 'long' })}
              </button>
              <div className="grid grid-cols-7">
                {weekdayInitials(locTag).map((w, i) => (
                  <span key={i} className="text-center text-[9.5px] text-muted-foreground/70">
                    {w}
                  </span>
                ))}
                {days.map((day, i) => {
                  const inMonth = day.getMonth() === m.getMonth();
                  const isToday = sameLocalDay(day, now);
                  const dots = eventsForDay(events, day)
                    .slice(0, 3)
                    .map((e) => e._color || 'var(--unread)');
                  return (
                    <button
                      key={i}
                      type="button"
                      className={cn(
                        'flex h-9 flex-col items-center justify-start gap-0.5 rounded-[5px] pt-0.5 hover:bg-accent',
                        !inMonth && 'opacity-25',
                      )}
                      onClick={() => {
                        setAnchor(day);
                        setView('day');
                      }}
                      aria-label={day.toLocaleDateString(locTag)}
                    >
                      <span
                        className={cn(
                          'flex h-5 w-5 items-center justify-center rounded-full text-[11px]',
                          isToday
                            ? 'bg-[var(--unread)] font-semibold text-[#1a1b1f]'
                            : 'text-foreground/80',
                        )}
                      >
                        {day.getDate()}
                      </span>
                      <span className="flex h-1 items-center gap-0.5">
                        {dots.map((c, j) => (
                          <span
                            key={j}
                            className="size-1 rounded-full"
                            style={{ backgroundColor: c }}
                          />
                        ))}
                      </span>
                    </button>
                  );
                })}
              </div>
            </div>
          );
        })}
      </div>
    );
  }

  function renderTimeGrid(days: Date[]) {
    const hours = hourSlots();
    return (
      <div className="flex min-h-0 flex-1 flex-col overflow-hidden bg-border/70">
        <div
          className="grid shrink-0 gap-px border-b border-border bg-border/70"
          style={{ gridTemplateColumns: `3.5rem repeat(${days.length}, minmax(0, 1fr))` }}
        >
          <div className="bg-background" />
          {days.map((day) => {
            const isToday = sameLocalDay(day, now);
            return (
              <div key={day.toISOString()} className="bg-background px-2 py-2 text-center">
                <div className="text-[11px] text-muted-foreground">
                  {day.toLocaleDateString(locTag, { weekday: 'short' })}
                </div>
                <div
                  className={cn(
                    'mx-auto mt-1 flex h-7 w-7 items-center justify-center rounded-full text-sm',
                    isToday
                      ? 'bg-[var(--unread)] font-semibold text-[#1a1b1f]'
                      : 'font-medium text-foreground',
                  )}
                >
                  {day.getDate()}
                </div>
              </div>
            );
          })}
        </div>
        <div
          className="grid shrink-0 gap-px border-b border-border bg-border/70"
          style={{ gridTemplateColumns: `3.5rem repeat(${days.length}, minmax(0, 1fr))` }}
        >
          <div className="bg-background px-1.5 py-1.5 text-[10px] text-muted-foreground">
            {t(locale, 'calendar.allDay')}
          </div>
          {days.map((day) => {
            const allDay = eventsForDay(events, day).filter((e) => e.isAllDay);
            return (
              <div
                key={day.toISOString()}
                className="flex min-h-9 flex-col gap-0.5 bg-background p-1"
              >
                {allDay.map((event) => (
                  <EventChip key={event.id} event={event} />
                ))}
              </div>
            );
          })}
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto bg-background">
          <div
            className="grid"
            style={{
              gridTemplateColumns: `3.5rem repeat(${days.length}, minmax(0, 1fr))`,
              height: '48rem',
            }}
          >
            <div className="relative border-r border-border/70">
              {hours.map((h) => (
                <span
                  key={h}
                  className="absolute right-1.5 text-[10px] tabular-nums text-muted-foreground"
                  style={{ top: `${(h / 24) * 100}%`, transform: 'translateY(-50%)' }}
                >
                  {String(h).padStart(2, '0')}:00
                </span>
              ))}
            </div>
            {days.map((day) => {
              const timed = eventsForDay(events, day).filter((e) => !e.isAllDay);
              const showNow = sameLocalDay(day, now);
              return (
                <div
                  key={day.toISOString()}
                  className="relative border-r border-border/70 last:border-r-0"
                >
                  {hours.map((h) => (
                    <div
                      key={h}
                      className="absolute inset-x-0 border-t border-border/50"
                      style={{ top: `${(h / 24) * 100}%` }}
                    />
                  ))}
                  {timed.map((event) => (
                    <button
                      key={event.id}
                      type="button"
                      className="absolute right-1 left-1 overflow-hidden rounded-[4px] px-1.5 py-0.5 text-left text-[10.5px] leading-tight text-foreground"
                      style={timedBlockStyle(event)}
                      onClick={() => setSelectedEvent(event)}
                    >
                      <span className="mr-1 text-muted-foreground">{formatEventTime(event)}</span>
                      {event.summary || t(locale, 'calendar.noTitle')}
                    </button>
                  ))}
                  {showNow ? (
                    <div
                      className="pointer-events-none absolute right-0 left-0 z-10 border-t border-[var(--unread)]"
                      style={{ top: `${nowTop}%` }}
                    >
                      <span className="absolute top-0 left-0 -translate-y-1/2 rounded-full bg-[var(--unread)] px-1 text-[9px] font-medium text-[#1a1b1f] tabular-nums">
                        {now.toLocaleTimeString(locTag, {
                          hour: '2-digit',
                          minute: '2-digit',
                        })}
                      </span>
                    </div>
                  ) : null}
                </div>
              );
            })}
          </div>
        </div>
      </div>
    );
  }

  function DetailRow({
    icon: Icon,
    label,
    children,
  }: {
    icon: typeof Clock;
    label: string;
    children: ReactNode;
  }) {
    return (
      <div className="flex gap-3">
        <Icon className="mt-0.5 size-3.5 shrink-0 text-muted-foreground" />
        <div className="min-w-0">
          <p className="text-[10.5px] font-medium tracking-wide text-muted-foreground uppercase">
            {label}
          </p>
          <div className="mt-0.5 text-[13px] text-foreground">{children}</div>
        </div>
      </div>
    );
  }

  return (
    <div className="flex h-svh">
      {/* Left panel: brand header + mini month + calendar sources */}
      <aside className="flex w-[232px] shrink-0 flex-col overflow-y-auto border-r bg-secondary px-2 py-3">
        <SlimPanelHeader />
        <MiniMonth />
        <p className="px-2.5 pb-1 text-[10.5px] font-medium tracking-wide text-muted-foreground uppercase">
          {t(locale, 'calendar.sources')}
        </p>
        {loading ? (
          <div className="px-2.5 text-sm text-muted-foreground">{t(locale, 'common.loading')}</div>
        ) : error ? (
          <div className="px-2.5 text-sm text-destructive">{t(locale, 'calendar.loadError')}</div>
        ) : sources.length === 0 ? (
          <div className="flex flex-col items-center pt-6">
            <EmptyState
              icon={CalendarIcon}
              title={t(locale, 'calendar.empty')}
              hint={t(locale, 'calendar.emptyHint')}
            />
            <Button
              variant="outline"
              size="sm"
              className="mt-3"
              onClick={() => void navigate({ to: '/settings', search: { pim: true } })}
            >
              {t(locale, 'calendar.connectDav')}
            </Button>
          </div>
        ) : (
          sources.map((src) => (
            <div
              key={src.id}
              className="group flex items-center gap-2 rounded-[7px] px-2.5 py-1.5 hover:bg-accent"
            >
              <label className="flex min-w-0 flex-1 cursor-pointer items-center gap-2.5">
                <input
                  type="checkbox"
                  className="size-3.5 shrink-0 accent-[var(--unread)]"
                  checked={visibleIds.has(src.id)}
                  onChange={() => toggleSource(src.id)}
                />
                <span
                  className="size-2 shrink-0 rounded-full"
                  style={{ backgroundColor: src.color || 'var(--unread)' }}
                />
                <span className="min-w-0 truncate text-[13px]" title={src.name}>
                  {displayName(locale, src.name)}
                </span>
                {src.kind === 'ics' ? (
                  <span className="ml-auto shrink-0 rounded bg-muted px-1 py-px text-[9.5px] font-medium text-muted-foreground">
                    ICS
                  </span>
                ) : null}
              </label>
              {src.kind === 'ics' ? (
                <Button
                  type="button"
                  variant="ghost"
                  size="icon"
                  className="h-6 w-6 shrink-0 opacity-0 group-hover:opacity-100"
                  aria-label={t(locale, 'common.delete')}
                  onClick={() => void removeSub(src.id)}
                >
                  <Trash2 className="size-3" />
                </Button>
              ) : null}
            </div>
          ))
        )}
        {sources.some((s) => s.lastError) ? (
          <p className="px-2.5 text-[10px] text-destructive">
            {sources.find((s) => s.lastError)?.lastError}
          </p>
        ) : null}
        <Button
          type="button"
          variant="ghost"
          size="sm"
          className="mt-auto justify-start text-muted-foreground"
          onClick={() => setAddOpen(true)}
        >
          <Plus className="size-3.5" />
          {t(locale, 'calendar.addSubscription')}
        </Button>
      </aside>

      <main className="flex min-w-0 flex-1 flex-col">
        <header className="flex h-14 shrink-0 items-center gap-3 border-b px-5">
          <h1 className="font-display truncate text-xl font-medium">
            {viewTitle(anchor, view, locTag)}
          </h1>
          <div className="ml-auto flex items-center gap-1.5">
            <div className="flex items-center rounded-md border border-border p-0.5">
              {VIEW_ORDER.map((v) => (
                <Button
                  key={v}
                  type="button"
                  variant={view === v ? 'secondary' : 'ghost'}
                  size="sm"
                  className="h-7 px-2.5 text-xs"
                  onClick={() => setView(v)}
                >
                  {t(locale, `calendar.view.${v}`)}
                </Button>
              ))}
            </div>
            <Button
              type="button"
              variant="outline"
              size="sm"
              className="h-8"
              onClick={() => setAnchor(new Date())}
            >
              {t(locale, 'calendar.today')}
            </Button>
            <div className="flex items-center">
              <Button
                type="button"
                variant="ghost"
                size="icon"
                className="h-8 w-8"
                aria-label="Previous"
                onClick={() => setAnchor((a) => addViewOffset(a, view, -1))}
              >
                <ChevronLeft className="size-4" />
              </Button>
              <Button
                type="button"
                variant="ghost"
                size="icon"
                className="h-8 w-8"
                aria-label="Next"
                onClick={() => setAnchor((a) => addViewOffset(a, view, 1))}
              >
                <ChevronRight className="size-4" />
              </Button>
            </div>
            <Button
              type="button"
              variant="ghost"
              size="icon"
              className="h-8 w-8"
              disabled={refreshing}
              onClick={() => void refresh()}
              aria-label={t(locale, 'calendar.refresh')}
            >
              <RefreshCw className={cn('size-4', refreshing && 'animate-spin')} />
            </Button>
          </div>
        </header>

        <div className="flex min-h-0 flex-1">
          <section className="flex min-w-0 flex-1 flex-col">
            {view === 'month'
              ? renderMonth()
              : view === 'year'
                ? renderYear()
                : renderTimeGrid(view === 'week' ? weekDays(anchor) : [anchor])}
          </section>

          {selectedEvent ? (
            <aside className="w-80 shrink-0 space-y-5 overflow-y-auto border-l p-5">
              <div className="flex items-start justify-between gap-2">
                <h2 className="font-display text-lg leading-snug font-medium">
                  {selectedEvent.summary || t(locale, 'calendar.noTitle')}
                </h2>
                <Button
                  variant="ghost"
                  size="icon"
                  className="h-6 w-6 shrink-0"
                  aria-label="Close"
                  onClick={() => setSelectedEvent(null)}
                >
                  <X className="size-4" />
                </Button>
              </div>
              <DetailRow icon={Clock} label={t(locale, 'calendar.when')}>
                {selectedEvent.dtstart
                  ? new Date(selectedEvent.dtstart).toLocaleString(locTag, {
                      weekday: 'short',
                      month: 'short',
                      day: 'numeric',
                      hour: selectedEvent.isAllDay ? undefined : '2-digit',
                      minute: selectedEvent.isAllDay ? undefined : '2-digit',
                    })
                  : formatEventTime(selectedEvent)}
                {selectedEvent.isAllDay ? ` · ${t(locale, 'calendar.allDay')}` : ''}
              </DetailRow>
              {selectedEvent.location ? (
                <DetailRow icon={MapPin} label={t(locale, 'calendar.where')}>
                  {selectedEvent.location}
                </DetailRow>
              ) : null}
              {selectedEvent.description ? (
                <div className="border-t pt-4 text-[13px] whitespace-pre-wrap text-muted-foreground">
                  {selectedEvent.description}
                </div>
              ) : null}
            </aside>
          ) : null}
        </div>
      </main>

      {addOpen ? (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-4">
          <form
            onSubmit={(e) => void submitAdd(e)}
            className="w-full max-w-md space-y-3 rounded-lg border bg-background p-5 shadow-lg"
          >
            <h2 className="text-base font-semibold">{t(locale, 'calendar.addSubscription')}</h2>
            <p className="text-xs text-muted-foreground">
              {t(locale, 'calendar.addSubscriptionHint')}
            </p>
            <Input
              required
              placeholder="https://… or webcal://…"
              value={addUrl}
              onChange={(e) => setAddUrl(e.target.value)}
              autoFocus
            />
            <Input
              placeholder={t(locale, 'calendar.subscriptionName')}
              value={addName}
              onChange={(e) => setAddName(e.target.value)}
            />
            {addError ? <p className="text-sm text-destructive">{addError}</p> : null}
            <div className="flex justify-end gap-2">
              <Button type="button" variant="ghost" onClick={() => setAddOpen(false)}>
                {t(locale, 'common.cancel')}
              </Button>
              <Button type="submit" disabled={addBusy || !addUrl.trim()}>
                {t(locale, 'common.add')}
              </Button>
            </div>
          </form>
        </div>
      ) : null}
    </div>
  );
}
