/**
 * Calendar-from-email confirm dialog: AI proposes, the user edits and
 * confirms; creation goes through the existing CalDAV create endpoint.
 */

import { CalendarPlus } from 'lucide-react';
import { ThinkingOrb } from 'thinking-orbs';
import { useEffect, useState } from 'react';

import { Button } from '@/components/ui/button';
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog';
import { Input } from '@/components/ui/input';
import { Switch } from '@/components/ui/switch';
import { api } from '@/lib/api-client';
import { suggestAiEvent, type AiEventSuggestion } from '@/lib/ai-api';
import { t, type SupportedLocale } from '@/i18n';

interface CalApi {
  id: string;
  name: string;
}

const inputCls = 'h-8 w-full rounded-lg border border-input bg-background px-2.5 text-[13px]';

function toLocalInput(rfc: string): string {
  // datetime-local value from RFC3339 or YYYY-MM-DD.
  if (rfc.length === 10) return rfc;
  const d = new Date(rfc);
  if (Number.isNaN(d.getTime())) return rfc;
  const pad = (n: number) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

function fromLocalInput(v: string, allDay: boolean): string {
  return allDay ? v.slice(0, 10) : v ? new Date(v).toISOString() : v;
}

export function AiEventDialog({
  messageId,
  locale,
  onClose,
}: {
  messageId: string;
  locale: SupportedLocale;
  onClose: () => void;
}) {
  const [suggestion, setSuggestion] = useState<AiEventSuggestion | null>(null);
  const [calendars, setCalendars] = useState<CalApi[]>([]);
  const [calendarId, setCalendarId] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [saving, setSaving] = useState(false);
  const [created, setCreated] = useState(false);

  useEffect(() => {
    setBusy(true);
    void Promise.all([suggestAiEvent(messageId), api<CalApi[]>('/calendars').catch(() => [])])
      .then(([s, cals]) => {
        setSuggestion(s);
        setCalendars(cals);
        setCalendarId(cals[0]?.id ?? '');
      })
      .catch((e) => setError(e instanceof Error ? e.message : String(e)))
      .finally(() => setBusy(false));
  }, [messageId]);

  const save = async () => {
    if (!suggestion || !calendarId || saving) return;
    setSaving(true);
    setError(null);
    try {
      await api(`/calendars/${calendarId}/events`, {
        method: 'POST',
        body: JSON.stringify({
          summary: suggestion.summary,
          dtstart: fromLocalInput(suggestion.dtstart, suggestion.isAllDay),
          dtend: suggestion.dtend ? fromLocalInput(suggestion.dtend, suggestion.isAllDay) : null,
          isAllDay: suggestion.isAllDay,
          location: suggestion.location || null,
          description: suggestion.description || null,
        }),
      });
      setCreated(true);
      window.setTimeout(onClose, 900);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  };

  const patch = (p: Partial<AiEventSuggestion>) => setSuggestion((s) => (s ? { ...s, ...p } : s));

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2 text-[13px]">
            <CalendarPlus className="size-4 text-muted-foreground" aria-hidden />
            {t(locale, 'aiEvent.title')}
          </DialogTitle>
        </DialogHeader>

        {busy ? (
          <div className="flex items-center gap-2 py-6 text-sm text-muted-foreground">
            <ThinkingOrb state="solving" size={20} className="size-4 shrink-0" aria-hidden />
            {t(locale, 'aiEvent.detecting')}
          </div>
        ) : error && !suggestion ? (
          <div className="py-2 text-xs text-destructive">{error}</div>
        ) : suggestion ? (
          <div className="space-y-3">
            <label className="flex flex-col gap-1.5">
              <span className="text-xs text-muted-foreground">{t(locale, 'aiEvent.summary')}</span>
              <Input
                className={inputCls}
                value={suggestion.summary}
                onChange={(e) => patch({ summary: e.target.value })}
              />
            </label>
            <div className="grid grid-cols-2 gap-3">
              <label className="flex flex-col gap-1.5">
                <span className="text-xs text-muted-foreground">{t(locale, 'aiEvent.start')}</span>
                <Input
                  type={suggestion.isAllDay ? 'date' : 'datetime-local'}
                  className={inputCls}
                  value={toLocalInput(suggestion.dtstart)}
                  onChange={(e) => patch({ dtstart: e.target.value })}
                />
              </label>
              <label className="flex flex-col gap-1.5">
                <span className="text-xs text-muted-foreground">{t(locale, 'aiEvent.end')}</span>
                <Input
                  type={suggestion.isAllDay ? 'date' : 'datetime-local'}
                  className={inputCls}
                  value={suggestion.dtend ? toLocalInput(suggestion.dtend) : ''}
                  onChange={(e) => patch({ dtend: e.target.value || null })}
                />
              </label>
            </div>
            <div className="flex items-center justify-between">
              <span className="text-xs text-muted-foreground">{t(locale, 'aiEvent.allDay')}</span>
              <Switch
                checked={suggestion.isAllDay}
                onCheckedChange={(allDay) => patch({ isAllDay: allDay })}
              />
            </div>
            <label className="flex flex-col gap-1.5">
              <span className="text-xs text-muted-foreground">{t(locale, 'aiEvent.location')}</span>
              <Input
                className={inputCls}
                value={suggestion.location ?? ''}
                onChange={(e) => patch({ location: e.target.value })}
              />
            </label>
            <label className="flex flex-col gap-1.5">
              <span className="text-xs text-muted-foreground">{t(locale, 'aiEvent.calendar')}</span>
              <select
                className={inputCls}
                value={calendarId}
                onChange={(e) => setCalendarId(e.target.value)}
              >
                {calendars.length === 0 ? (
                  <option value="">{t(locale, 'aiEvent.noCalendar')}</option>
                ) : (
                  calendars.map((c) => (
                    <option key={c.id} value={c.id}>
                      {c.name}
                    </option>
                  ))
                )}
              </select>
            </label>
            {error ? <div className="text-xs text-destructive">{error}</div> : null}
            {created ? <div className="text-xs text-ok">{t(locale, 'aiEvent.created')}</div> : null}
          </div>
        ) : null}

        <DialogFooter>
          <Button variant="ghost" size="sm" onClick={onClose}>
            {t(locale, 'common.cancel')}
          </Button>
          <Button
            size="sm"
            disabled={!suggestion || !calendarId || saving || created}
            onClick={() => void save()}
          >
            {t(locale, 'aiEvent.create')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
