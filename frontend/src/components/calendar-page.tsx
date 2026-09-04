/**
 * Calendar subsystem — Notion/Thunderbird-style shell over CalDAV data.
 */

import { useState, useEffect, useMemo, type CSSProperties } from 'react';
import { Link } from '@tanstack/react-router';
import { Calendar as CalendarIcon, ChevronLeft, ChevronRight, RefreshCw, X } from 'lucide-react';
import { t } from '../i18n';
import { api } from '../lib/api-client';
import {
  addViewOffset,
  eventsForDay,
  hourSlots,
  monthGridDays,
  sameLocalDay,
  viewTitle,
  visibleRangeIso,
  weekDays,
  type CalendarView,
} from '@/lib/calendar-grid';
import { EmptyState } from './empty-state';
import { Button } from '@/components/ui/button';
import { cn } from '@/lib/utils';
import { useUIStore } from '../stores/ui';

interface CalSource {
  id: string;
  accountId: string;
  name: string;
  color?: string;
  isActive: boolean;
}

interface CalEvent {
  id: string;
  calendarId?: string;
  summary?: string;
  description?: string;
  dtstart?: string;
  dtend?: string;
  location?: string;
  isAllDay: boolean;
  status?: string;
  recurrenceRule?: string;
  /** Set client-side when merging multi-calendar fetches. */
  _calendarId?: string;
  _color?: string;
}

const WEEKDAY_ORDER = ['mon', 'tue', 'wed', 'thu', 'fri', 'sat', 'sun'] as const;

export function CalendarPage() {
  const locale = useUIStore((s) => s.locale);
  const locTag = locale === 'zh' ? 'zh-CN' : 'en-US';
  const [calendars, setCalendars] = useState<CalSource[]>([]);
  const [visibleIds, setVisibleIds] = useState<Set<string>>(new Set());
  const [events, setEvents] = useState<CalEvent[]>([]);
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [view, setView] = useState<CalendarView>('month');
  const [anchor, setAnchor] = useState(() => new Date());
  const [selectedEvent, setSelectedEvent] = useState<CalEvent | null>(null);
  const [now, setNow] = useState(() => new Date());

  useEffect(() => {
    const id = window.setInterval(() => setNow(new Date()), 60_000);
    return () => window.clearInterval(id);
  }, []);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        setLoading(true);
        setError(null);
        const data = await api<CalSource[]>('/calendars');
        if (cancelled) return;
        setCalendars(data);
        setVisibleIds(new Set(data.map((c) => c.id)));
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
    for (const c of calendars) {
      m.set(c.id, c.color || 'var(--unread)');
    }
    return m;
  }, [calendars]);

  async function loadEvents(ids: Set<string>, when: Date, v: CalendarView) {
    if (ids.size === 0) {
      setEvents([]);
      return;
    }
    const { start, end } = visibleRangeIso(when, v);
    const q = `start=${encodeURIComponent(start)}&end=${encodeURIComponent(end)}`;
    const chunks = await Promise.all(
      [...ids].map(async (id) => {
        try {
          const rows = await api<CalEvent[]>(`/calendars/${id}/events?${q}`);
          return rows.map((e) => ({
            ...e,
            _calendarId: id,
            _color: colorById.get(id) || 'var(--unread)',
          }));
        } catch {
          return [] as CalEvent[];
        }
      }),
    );
    setEvents(chunks.flat());
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
    // colorById identity changes with calendars; visibleIds/anchor/view drive fetch
    // eslint-disable-next-line react-hooks/exhaustive-deps -- intentional
  }, [loading, visibleIds, anchor, view, calendars]);

  function toggleCalendar(id: string) {
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
      // Re-sync CalDAV for each distinct account, then reload events
      const accountIds = [...new Set(calendars.map((c) => c.accountId))];
      await Promise.all(
        accountIds.map((aid) => api(`/accounts/${aid}/calendars/sync`).catch(() => undefined)),
      );
      const data = await api<CalSource[]>('/calendars');
      setCalendars(data);
      setVisibleIds((prev) => {
        const next = new Set<string>();
        for (const c of data) {
          if (prev.has(c.id) || prev.size === 0) next.add(c.id);
        }
        if (next.size === 0) data.forEach((c) => next.add(c.id));
        return next;
      });
      await loadEvents(
        new Set(data.filter((c) => visibleIds.has(c.id) || visibleIds.size === 0).map((c) => c.id)),
        anchor,
        view,
      );
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setRefreshing(false);
    }
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
      backgroundColor: event._color || 'var(--unread)',
    };
  }

  const nowMinutes = now.getHours() * 60 + now.getMinutes();
  const nowTop = (nowMinutes / (24 * 60)) * 100;

  function renderMonth() {
    const year = anchor.getFullYear();
    const month = anchor.getMonth();
    const days = monthGridDays(year, month);
    return (
      <div className="grid min-h-0 flex-1 grid-cols-7 grid-rows-[auto_repeat(6,minmax(0,1fr))] gap-px overflow-hidden border bg-border">
        {WEEKDAY_ORDER.map((day) => (
          <div
            key={day}
            className="bg-muted/60 px-2 py-1.5 text-center text-[11px] font-medium text-muted-foreground"
          >
            {t(locale, `calendar.days.${day}`)}
          </div>
        ))}
        {days.map((day, i) => {
          const dayEvents = eventsForDay(events, day);
          const inMonth = day.getMonth() === month;
          const isToday = sameLocalDay(day, now);
          return (
            <div
              key={i}
              className={cn(
                'flex min-h-20 flex-col bg-background p-1',
                !inMonth && 'bg-muted/30 text-muted-foreground',
              )}
            >
              <div
                className={cn(
                  'mb-0.5 flex h-6 w-6 items-center justify-center self-end rounded-full text-xs',
                  isToday && 'bg-[var(--unread)] font-semibold text-[#1a1b1f]',
                )}
              >
                {day.getDate()}
              </div>
              <div className="min-h-0 flex-1 space-y-0.5 overflow-hidden">
                {dayEvents.slice(0, 3).map((event) => (
                  <button
                    key={event.id}
                    type="button"
                    className="block w-full truncate rounded-sm px-1 py-0.5 text-left text-[10.5px] leading-tight text-[#1a1b1f]"
                    style={{ backgroundColor: event._color || 'var(--unread)' }}
                    onClick={() => setSelectedEvent(event)}
                  >
                    {event.summary || t(locale, 'calendar.noTitle')}
                  </button>
                ))}
                {dayEvents.length > 3 ? (
                  <div className="px-1 text-[10px] text-muted-foreground">
                    {t(locale, 'calendar.moreEvents', { count: dayEvents.length - 3 })}
                  </div>
                ) : null}
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
      <div className="flex min-h-0 flex-1 flex-col overflow-hidden border">
        <div
          className="grid shrink-0 border-b bg-muted/40"
          style={{ gridTemplateColumns: `3.5rem repeat(${days.length}, minmax(0, 1fr))` }}
        >
          <div className="border-r" />
          {days.map((day) => {
            const isToday = sameLocalDay(day, now);
            return (
              <div
                key={day.toISOString()}
                className="border-r px-2 py-2 text-center text-xs last:border-r-0"
              >
                <div className="text-muted-foreground">
                  {day.toLocaleDateString(locTag, { weekday: 'short' })}
                </div>
                <div
                  className={cn(
                    'mx-auto mt-0.5 flex h-7 w-7 items-center justify-center rounded-full text-sm font-medium',
                    isToday && 'bg-[var(--unread)] text-[#1a1b1f]',
                  )}
                >
                  {day.getDate()}
                </div>
              </div>
            );
          })}
        </div>

        {/* All-day band */}
        <div
          className="grid shrink-0 border-b bg-background"
          style={{ gridTemplateColumns: `3.5rem repeat(${days.length}, minmax(0, 1fr))` }}
        >
          <div className="border-r px-1 py-1 text-[10px] text-muted-foreground">
            {t(locale, 'calendar.allDay')}
          </div>
          {days.map((day) => {
            const allDay = eventsForDay(events, day).filter((e) => e.isAllDay);
            return (
              <div
                key={day.toISOString()}
                className="min-h-8 space-y-0.5 border-r p-0.5 last:border-r-0"
              >
                {allDay.map((event) => (
                  <button
                    key={event.id}
                    type="button"
                    className="block w-full truncate rounded-sm px-1 text-left text-[10px] text-[#1a1b1f]"
                    style={{ backgroundColor: event._color || 'var(--unread)' }}
                    onClick={() => setSelectedEvent(event)}
                  >
                    {event.summary || t(locale, 'calendar.noTitle')}
                  </button>
                ))}
              </div>
            );
          })}
        </div>

        <div className="relative min-h-0 flex-1 overflow-y-auto">
          <div
            className="grid"
            style={{
              gridTemplateColumns: `3.5rem repeat(${days.length}, minmax(0, 1fr))`,
              height: '48rem',
            }}
          >
            <div className="relative border-r">
              {hours.map((h) => (
                <div
                  key={h}
                  className="absolute right-1 text-[10px] text-muted-foreground"
                  style={{ top: `${(h / 24) * 100}%`, transform: 'translateY(-50%)' }}
                >
                  {String(h).padStart(2, '0')}:00
                </div>
              ))}
            </div>
            {days.map((day) => {
              const timed = eventsForDay(events, day).filter((e) => !e.isAllDay);
              const showNow = sameLocalDay(day, now);
              return (
                <div key={day.toISOString()} className="relative border-r last:border-r-0">
                  {hours.map((h) => (
                    <div
                      key={h}
                      className="absolute inset-x-0 border-t border-border/60"
                      style={{ top: `${(h / 24) * 100}%` }}
                    />
                  ))}
                  {timed.map((event) => (
                    <button
                      key={event.id}
                      type="button"
                      className="absolute right-0.5 left-0.5 overflow-hidden rounded-sm px-1 text-left text-[10px] leading-tight text-[#1a1b1f]"
                      style={timedBlockStyle(event)}
                      onClick={() => setSelectedEvent(event)}
                    >
                      {event.summary || t(locale, 'calendar.noTitle')}
                    </button>
                  ))}
                  {showNow ? (
                    <div
                      className="pointer-events-none absolute right-0 left-0 z-10 border-t-2 border-[var(--unread)]"
                      style={{ top: `${nowTop}%` }}
                    >
                      <span className="absolute -top-2.5 -left-0 rounded bg-[var(--unread)] px-1 text-[9px] font-medium text-[#1a1b1f]">
                        {now.toLocaleTimeString(locTag, { hour: '2-digit', minute: '2-digit' })}
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

  return (
    <div className="flex h-svh flex-col bg-background">
      <header className="flex h-14 shrink-0 items-center gap-2 border-b px-3">
        <Button variant="ghost" size="sm" asChild>
          <Link to="/">{t(locale, 'common.back')}</Link>
        </Button>
        <h1 className="hidden text-lg font-semibold sm:block">{t(locale, 'calendar.title')}</h1>
        <div className="mx-2 min-w-0 flex-1 truncate text-center text-sm font-semibold sm:text-base">
          {viewTitle(anchor, view, locTag)}
        </div>
        <div className="flex items-center gap-1 rounded-md border p-0.5">
          {(['day', 'week', 'month'] as CalendarView[]).map((v) => (
            <Button
              key={v}
              type="button"
              variant={view === v ? 'secondary' : 'ghost'}
              size="sm"
              className="h-7 px-2 text-xs"
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
        <Button
          type="button"
          variant="outline"
          size="icon"
          className="h-8 w-8"
          onClick={() => setAnchor((a) => addViewOffset(a, view, -1))}
        >
          <ChevronLeft className="size-4" />
        </Button>
        <Button
          type="button"
          variant="outline"
          size="icon"
          className="h-8 w-8"
          onClick={() => setAnchor((a) => addViewOffset(a, view, 1))}
        >
          <ChevronRight className="size-4" />
        </Button>
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
      </header>

      <div className="flex min-h-0 flex-1">
        <aside className="flex w-52 shrink-0 flex-col gap-1 overflow-y-auto border-r bg-muted/15 p-3">
          <p className="px-1 text-[10.5px] font-medium tracking-wide text-muted-foreground uppercase">
            {t(locale, 'calendar.sources')}
          </p>
          {loading ? (
            <div className="px-1 text-sm text-muted-foreground">{t(locale, 'common.loading')}</div>
          ) : error ? (
            <div className="px-1 text-sm text-destructive">{t(locale, 'calendar.loadError')}</div>
          ) : calendars.length === 0 ? (
            <EmptyState
              icon={CalendarIcon}
              title={t(locale, 'calendar.empty')}
              hint={t(locale, 'calendar.emptyHint')}
            />
          ) : (
            calendars.map((cal) => (
              <label
                key={cal.id}
                className="flex cursor-pointer items-center gap-2 rounded-md px-1.5 py-1.5 text-sm hover:bg-accent"
              >
                <input
                  type="checkbox"
                  className="size-3.5 accent-[var(--unread)]"
                  checked={visibleIds.has(cal.id)}
                  onChange={() => toggleCalendar(cal.id)}
                />
                <span
                  className="h-2.5 w-2.5 shrink-0 rounded-full"
                  style={{ backgroundColor: cal.color || 'var(--unread)' }}
                />
                <span className="truncate">{cal.name}</span>
              </label>
            ))
          )}
          <Button
            type="button"
            variant="outline"
            size="sm"
            className="mt-auto h-8 justify-start text-xs"
            disabled
            title={t(locale, 'calendar.addSubscriptionSoon')}
          >
            + {t(locale, 'calendar.addSubscription')}
          </Button>
        </aside>

        <section className="flex min-w-0 flex-1 flex-col p-3">
          {view === 'month'
            ? renderMonth()
            : renderTimeGrid(view === 'week' ? weekDays(anchor) : [anchor])}
        </section>

        {selectedEvent ? (
          <aside className="w-72 shrink-0 space-y-3 overflow-y-auto border-l p-4">
            <div className="flex items-start justify-between gap-2">
              <h3 className="text-sm font-semibold">
                {selectedEvent.summary || t(locale, 'calendar.noTitle')}
              </h3>
              <Button
                variant="ghost"
                size="icon"
                className="h-6 w-6"
                onClick={() => setSelectedEvent(null)}
              >
                <X className="h-4 w-4" />
              </Button>
            </div>
            <div className="text-sm text-muted-foreground">{formatEventTime(selectedEvent)}</div>
            {selectedEvent.dtstart ? (
              <div className="text-sm">
                {new Date(selectedEvent.dtstart).toLocaleString(locTag, {
                  weekday: 'short',
                  month: 'short',
                  day: 'numeric',
                  hour: selectedEvent.isAllDay ? undefined : '2-digit',
                  minute: selectedEvent.isAllDay ? undefined : '2-digit',
                })}
              </div>
            ) : null}
            {selectedEvent.location ? (
              <div className="text-sm">{selectedEvent.location}</div>
            ) : null}
            {selectedEvent.description ? (
              <div className="text-sm whitespace-pre-wrap text-muted-foreground">
                {selectedEvent.description}
              </div>
            ) : null}
          </aside>
        ) : null}
      </div>
    </div>
  );
}
