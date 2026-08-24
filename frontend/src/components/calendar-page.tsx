/**
 * Calendar page with CalDAV sync support.
 *
 * Displays calendars and events from all configured accounts.
 */

import { useState, useEffect } from 'react';
import { Calendar, ChevronLeft, ChevronRight, X } from 'lucide-react';
import { t } from '../i18n';
import { api } from '../lib/api-client';
import { EmptyState } from './empty-state';
import { SecondaryPage } from './secondary-page';
import { Button } from '@/components/ui/button';
import { cn } from '@/lib/utils';
import { useUIStore } from '../stores/ui';

interface Calendar {
  id: string;
  accountId: string;
  name: string;
  color?: string;
  description?: string;
  timezone?: string;
  isActive: boolean;
  createdAt: string;
  updatedAt: string;
}

interface CalendarEvent {
  id: string;
  calendarId: string;
  summary?: string;
  description?: string;
  dtstart?: string;
  dtend?: string;
  location?: string;
  isAllDay: boolean;
  status?: string;
  recurrenceRule?: string;
  createdAt: string;
  updatedAt: string;
}

export function CalendarPage() {
  const locale = useUIStore((s) => s.locale);
  const [calendars, setCalendars] = useState<Calendar[]>([]);
  const [events, setEvents] = useState<CalendarEvent[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [selectedCalendar, setSelectedCalendar] = useState<Calendar | null>(null);
  const [selectedEvent, setSelectedEvent] = useState<CalendarEvent | null>(null);
  const [currentDate, setCurrentDate] = useState(new Date());

  useEffect(() => {
    fetchCalendars();
  }, []);

  useEffect(() => {
    if (selectedCalendar) {
      fetchEvents(selectedCalendar.id);
    }
  }, [selectedCalendar]);

  async function fetchCalendars() {
    try {
      setLoading(true);
      const data = await api<Calendar[]>('/calendars');
      setCalendars(data);
      if (data.length > 0) {
        setSelectedCalendar(data[0]);
      }
    } catch (err: any) {
      setError(err.message);
    } finally {
      setLoading(false);
    }
  }

  async function fetchEvents(calendarId: string) {
    try {
      const data = await api<CalendarEvent[]>(`/calendars/${calendarId}/events`);
      setEvents(data);
    } catch (err: any) {
      setError(err.message);
    }
  }

  function formatEventTime(event: CalendarEvent): string {
    if (event.isAllDay) return t(locale, 'calendar.allDay');
    if (!event.dtstart) return '';
    const start = new Date(event.dtstart);
    return start.toLocaleTimeString(undefined, {
      hour: '2-digit',
      minute: '2-digit',
    });
  }

  function formatEventDate(event: CalendarEvent): string {
    if (!event.dtstart) return '';
    const start = new Date(event.dtstart);
    return start.toLocaleDateString(undefined, {
      weekday: 'short',
      month: 'short',
      day: 'numeric',
    });
  }

  function getEventsForDate(date: Date): CalendarEvent[] {
    return events.filter((event) => {
      if (!event.dtstart) return false;
      const eventDate = new Date(event.dtstart);
      return (
        eventDate.getFullYear() === date.getFullYear() &&
        eventDate.getMonth() === date.getMonth() &&
        eventDate.getDate() === date.getDate()
      );
    });
  }

  function renderCalendarGrid() {
    const year = currentDate.getFullYear();
    const month = currentDate.getMonth();
    const firstDay = new Date(year, month, 1);
    const lastDay = new Date(year, month + 1, 0);
    const startDate = new Date(firstDay);
    startDate.setDate(startDate.getDate() - firstDay.getDay());

    const days: Date[] = [];
    const current = new Date(startDate);

    while (current <= lastDay || current.getDay() !== 0) {
      days.push(new Date(current));
      current.setDate(current.getDate() + 1);
    }

    const weekdays = ['sun', 'mon', 'tue', 'wed', 'thu', 'fri', 'sat'];

    return (
      <div className="grid grid-cols-7 gap-px overflow-hidden rounded-lg border bg-border">
        {weekdays.map((day) => (
          <div
            key={day}
            className="bg-muted px-2 py-1.5 text-center text-xs font-medium text-muted-foreground"
          >
            {t(locale, `calendar.days.${day}`)}
          </div>
        ))}
        {days.map((day, i) => {
          const dayEvents = getEventsForDate(day);
          const isCurrentMonth = day.getMonth() === month;
          const isToday = day.toDateString() === new Date().toDateString();

          return (
            <div
              key={i}
              className={cn(
                'min-h-24 bg-background p-1.5',
                !isCurrentMonth && 'bg-muted/40 text-muted-foreground',
              )}
            >
              <div
                className={cn(
                  'flex h-6 w-6 items-center justify-center rounded-full text-xs',
                  isToday && 'bg-primary font-semibold text-primary-foreground',
                )}
              >
                {day.getDate()}
              </div>
              <div className="mt-1 space-y-0.5">
                {dayEvents.slice(0, 3).map((event) => (
                  <button
                    key={event.id}
                    type="button"
                    className="block w-full truncate rounded px-1 py-0.5 text-left text-xs text-white"
                    style={{ backgroundColor: selectedCalendar?.color || '#4f46e5' }}
                    onClick={() => setSelectedEvent(event)}
                  >
                    {event.summary || t(locale, 'calendar.noTitle')}
                  </button>
                ))}
                {dayEvents.length > 3 && (
                  <div className="px-1 text-xs text-muted-foreground">
                    {t(locale, 'calendar.moreEvents', { count: dayEvents.length - 3 })}
                  </div>
                )}
              </div>
            </div>
          );
        })}
      </div>
    );
  }

  function navigateMonth(offset: number) {
    setCurrentDate((prev) => {
      const next = new Date(prev);
      next.setMonth(next.getMonth() + offset);
      return next;
    });
  }

  return (
    <SecondaryPage title={t(locale, 'calendar.title')}>
      <div className="mx-auto flex max-w-5xl gap-6">
        <div className="w-48 shrink-0 space-y-2">
          <h2 className="text-sm font-medium text-muted-foreground">
            {t(locale, 'calendar.calendars')}
          </h2>
          {loading ? (
            <div className="text-sm text-muted-foreground">{t(locale, 'common.loading')}</div>
          ) : error ? (
            <div className="text-sm text-destructive">{t(locale, 'calendar.loadError')}</div>
          ) : calendars.length === 0 ? (
            <EmptyState
              icon={Calendar}
              title={t(locale, 'calendar.empty')}
              hint={t(locale, 'calendar.emptyHint')}
            />
          ) : (
            <div className="space-y-1">
              {calendars.map((calendar) => (
                <button
                  key={calendar.id}
                  type="button"
                  className={cn(
                    'flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm transition-colors hover:bg-accent',
                    selectedCalendar?.id === calendar.id && 'bg-muted font-medium',
                  )}
                  onClick={() => setSelectedCalendar(calendar)}
                >
                  <span
                    className="h-3 w-3 shrink-0 rounded-full"
                    style={{ backgroundColor: calendar.color || '#4f46e5' }}
                  />
                  <span className="truncate">{calendar.name}</span>
                </button>
              ))}
            </div>
          )}
        </div>

        <div className="min-w-0 flex-1 space-y-4">
          <div className="flex items-center gap-2">
            <Button
              variant="outline"
              size="icon"
              className="h-8 w-8"
              onClick={() => navigateMonth(-1)}
            >
              <ChevronLeft className="h-4 w-4" />
            </Button>
            <h2 className="min-w-40 text-center text-base font-semibold">
              {currentDate.toLocaleDateString(locale === 'zh' ? 'zh-CN' : 'en-US', {
                month: 'long',
                year: 'numeric',
              })}
            </h2>
            <Button
              variant="outline"
              size="icon"
              className="h-8 w-8"
              onClick={() => navigateMonth(1)}
            >
              <ChevronRight className="h-4 w-4" />
            </Button>
            <Button
              variant="outline"
              size="sm"
              className="ml-auto"
              onClick={() => setCurrentDate(new Date())}
            >
              {t(locale, 'calendar.today')}
            </Button>
          </div>

          {renderCalendarGrid()}
        </div>

        {selectedEvent && (
          <div className="w-72 shrink-0 space-y-3 self-start rounded-lg border p-4">
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
            <div className="text-sm text-muted-foreground">
              {formatEventDate(selectedEvent)} • {formatEventTime(selectedEvent)}
            </div>
            {selectedEvent.location && <div className="text-sm">{selectedEvent.location}</div>}
            {selectedEvent.description && (
              <div className="text-sm whitespace-pre-wrap">{selectedEvent.description}</div>
            )}
          </div>
        )}
      </div>
    </SecondaryPage>
  );
}
