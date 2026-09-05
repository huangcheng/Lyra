/**
 * Calendar subsystem — CalDAV sources + ICS / webcal subscriptions.
 */

import { useState, useEffect, useMemo, type CSSProperties, type FormEvent } from 'react';
import { Link } from '@tanstack/react-router';
import {
  Calendar as CalendarIcon,
  ChevronLeft,
  ChevronRight,
  RefreshCw,
  Trash2,
  X,
} from 'lucide-react';
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
import { Input } from '@/components/ui/input';
import { cn } from '@/lib/utils';
import { useUIStore } from '../stores/ui';

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

export function CalendarPage() {
  const locale = useUIStore((s) => s.locale);
  const locTag = locale === 'zh' ? 'zh-CN' : 'en-US';
  const [sources, setSources] = useState<CalSource[]>([]);
  const [visibleIds, setVisibleIds] = useState<Set<string>>(new Set());
  const [events, setEvents] = useState<CalEvent[]>([]);
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
        ...new Set(sources.filter((s) => s.kind === 'caldav').map((s) => s.accountId!).filter(Boolean)),
      ];
      await Promise.all([
        ...accountIds.map((aid) => api(`/accounts/${aid}/calendars/sync`).catch(() => undefined)),
        ...sources
          .filter((s) => s.kind === 'ics')
          .map((s) =>
            api(`/calendar-subscriptions/${s.id}/refresh`, { method: 'POST' }).catch(() => undefined),
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
      const merged = await loadSources();
      setVisibleIds((prev) => new Set([...prev, created.id]));
      await loadEvents(new Set([...visibleIds, created.id]), anchor, view);
      void merged;
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
        <Button type="button" variant="outline" size="sm" className="h-8" onClick={() => setAnchor(new Date())}>
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
        <aside className="flex w-56 shrink-0 flex-col gap-1 overflow-y-auto border-r bg-muted/15 p-3">
          <p className="px-1 text-[10.5px] font-medium tracking-wide text-muted-foreground uppercase">
            {t(locale, 'calendar.sources')}
          </p>
          {loading ? (
            <div className="px-1 text-sm text-muted-foreground">{t(locale, 'common.loading')}</div>
          ) : error ? (
            <div className="px-1 text-sm text-destructive">{t(locale, 'calendar.loadError')}</div>
          ) : sources.length === 0 ? (
            <EmptyState
              icon={CalendarIcon}
              title={t(locale, 'calendar.empty')}
              hint={t(locale, 'calendar.emptyHint')}
            />
          ) : (
            sources.map((src) => (
              <div key={src.id} className="group flex items-start gap-1 rounded-md px-1 py-1 hover:bg-accent">
                <label className="flex min-w-0 flex-1 cursor-pointer items-center gap-2 py-0.5 text-sm">
                  <input
                    type="checkbox"
                    className="size-3.5 accent-[var(--unread)]"
                    checked={visibleIds.has(src.id)}
                    onChange={() => toggleSource(src.id)}
                  />
                  <span
                    className="mt-0.5 h-2.5 w-2.5 shrink-0 rounded-full"
                    style={{ backgroundColor: src.color || 'var(--unread)' }}
                  />
                  <span className="min-w-0 truncate">
                    {src.name}
                    {src.kind === 'ics' ? (
                      <span className="ml-1 text-[10px] text-muted-foreground">ICS</span>
                    ) : null}
                  </span>
                </label>
                {src.kind === 'ics' ? (
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon"
                    className="h-6 w-6 opacity-0 group-hover:opacity-100"
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
            <p className="px-1 text-[10px] text-destructive">
              {sources.find((s) => s.lastError)?.lastError}
            </p>
          ) : null}
          <Button
            type="button"
            variant="outline"
            size="sm"
            className="mt-auto h-8 justify-start text-xs"
            onClick={() => setAddOpen(true)}
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
            {selectedEvent.location ? <div className="text-sm">{selectedEvent.location}</div> : null}
            {selectedEvent.description ? (
              <div className="text-sm whitespace-pre-wrap text-muted-foreground">
                {selectedEvent.description}
              </div>
            ) : null}
          </aside>
        ) : null}
      </div>

      {addOpen ? (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-4">
          <form
            onSubmit={(e) => void submitAdd(e)}
            className="w-full max-w-md space-y-3 rounded-lg border bg-background p-4 shadow-lg"
          >
            <h2 className="text-base font-semibold">{t(locale, 'calendar.addSubscription')}</h2>
            <p className="text-xs text-muted-foreground">{t(locale, 'calendar.addSubscriptionHint')}</p>
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
