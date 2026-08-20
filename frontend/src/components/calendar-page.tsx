/**
 * Calendar page with CalDAV sync support.
 *
 * Displays calendars and events from all configured accounts.
 */

import { useState, useEffect } from 'react';
import { t } from '../i18n';
import { useUIStore } from '../stores/ui';
import { useAuthStore } from '../stores/auth';

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
  const token = useAuthStore((s) => s.token);
  const [calendars, setCalendars] = useState<Calendar[]>([]);
  const [events, setEvents] = useState<CalendarEvent[]>([]);
  const [loading, setLoading] = useState(true);
  const [, setError] = useState<string | null>(null);
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
      const res = await fetch('/api/calendars', {
        headers: { Authorization: `Bearer ${token}` },
      });
      if (!res.ok) throw new Error('Failed to fetch calendars');
      const data = await res.json();
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
      const res = await fetch(`/api/calendars/${calendarId}/events`, {
        headers: { Authorization: `Bearer ${token}` },
      });
      if (!res.ok) throw new Error('Failed to fetch events');
      const data = await res.json();
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

    const days = [];
    const current = new Date(startDate);

    while (current <= lastDay || current.getDay() !== 0) {
      days.push(new Date(current));
      current.setDate(current.getDate() + 1);
    }

    return (
      <div className="calendar-grid">
        <div className="calendar-header">
          {['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'].map((day) => (
            <div key={day} className="calendar-day-header">
              {t(locale, `calendar.days.${day.toLowerCase()}`)}
            </div>
          ))}
        </div>
        <div className="calendar-body">
          {days.map((day, i) => {
            const dayEvents = getEventsForDate(day);
            const isCurrentMonth = day.getMonth() === month;
            const isToday = day.toDateString() === new Date().toDateString();

            return (
              <div
                key={i}
                className={`calendar-day ${!isCurrentMonth ? 'other-month' : ''} ${isToday ? 'today' : ''}`}
              >
                <div className="day-number">{day.getDate()}</div>
                <div className="day-events">
                  {dayEvents.slice(0, 3).map((event) => (
                    <div
                      key={event.id}
                      className="event-chip"
                      style={{
                        backgroundColor: selectedCalendar?.color || '#3b82f6',
                      }}
                      onClick={() => setSelectedEvent(event)}
                    >
                      {event.summary || t(locale, 'calendar.noTitle')}
                    </div>
                  ))}
                  {dayEvents.length > 3 && (
                    <div className="event-more">+{dayEvents.length - 3} more</div>
                  )}
                </div>
              </div>
            );
          })}
        </div>
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
    <div className="calendar-page">
      <div className="calendar-sidebar">
        <div className="calendar-list-header">
          <h2>{t(locale, 'calendar.calendars')}</h2>
        </div>
        {loading ? (
          <div className="loading">{t(locale, 'common.loading')}</div>
        ) : (
          <div className="calendar-list">
            {calendars.map((calendar) => (
              <div
                key={calendar.id}
                className={`calendar-item ${selectedCalendar?.id === calendar.id ? 'selected' : ''}`}
                onClick={() => setSelectedCalendar(calendar)}
              >
                <div
                  className="calendar-color"
                  style={{ backgroundColor: calendar.color || '#3b82f6' }}
                />
                <div className="calendar-name">{calendar.name}</div>
              </div>
            ))}
          </div>
        )}
      </div>

      <div className="calendar-main">
        <div className="calendar-toolbar">
          <button onClick={() => navigateMonth(-1)}>←</button>
          <h2>
            {currentDate.toLocaleDateString(undefined, {
              month: 'long',
              year: 'numeric',
            })}
          </h2>
          <button onClick={() => navigateMonth(1)}>→</button>
          <button onClick={() => setCurrentDate(new Date())}>{t(locale, 'calendar.today')}</button>
        </div>

        {renderCalendarGrid()}
      </div>

      {selectedEvent && (
        <div className="event-detail-panel">
          <div className="event-detail-header">
            <h3>{selectedEvent.summary || t(locale, 'calendar.noTitle')}</h3>
            <button onClick={() => setSelectedEvent(null)}>×</button>
          </div>
          <div className="event-detail-body">
            <div className="event-time">
              {formatEventDate(selectedEvent)} • {formatEventTime(selectedEvent)}
            </div>
            {selectedEvent.location && (
              <div className="event-location">📍 {selectedEvent.location}</div>
            )}
            {selectedEvent.description && (
              <div className="event-description">{selectedEvent.description}</div>
            )}
          </div>
        </div>
      )}
    </div>
  );
}
