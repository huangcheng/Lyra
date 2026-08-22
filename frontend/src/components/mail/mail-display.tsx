/**
 * Reading pane — shadcn v3 mail-display.
 */

import { addDays, addHours, format, nextSaturday } from 'date-fns';
import DOMPurify from 'dompurify';
import {
  Archive,
  ArchiveX,
  Clock,
  Forward,
  MoreVertical,
  Reply,
  ReplyAll,
  Trash2,
} from 'lucide-react';
import { useEffect, useState } from 'react';

import { Avatar, AvatarFallback, AvatarImage } from '@/components/ui/avatar';
import { Button } from '@/components/ui/button';
import { Calendar } from '@/components/ui/calendar';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu';
import { Label } from '@/components/ui/label';
import { Popover, PopoverContent, PopoverTrigger } from '@/components/ui/popover';
import { Separator } from '@/components/ui/separator';
import { Switch } from '@/components/ui/switch';
import { Textarea } from '@/components/ui/textarea';
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip';
import { t } from '@/i18n';
import { mapApiMessage, type ApiMessage } from '@/lib/mail-api';
import { getInitials } from '@/lib/utils';
import { useAuthStore } from '@/stores/auth';
import { useMailStore } from '@/stores/mail';
import { useUIStore } from '@/stores/ui';

/**
 * Sanitize attacker-controlled email HTML before rendering.
 * Defense in depth: the backend sanitizes at ingest; this guards the render
 * path against anything stored before that, or from other sources.
 * Strict config: no iframes/forms/embeds, no unknown protocols.
 */
function sanitizeEmailHtml(html: string): string {
  return DOMPurify.sanitize(html, {
    FORBID_TAGS: ['iframe', 'object', 'embed', 'form', 'meta', 'link', 'base', 'style'],
  });
}

function quoteBody(message: {
  from: { email: string };
  date: string;
  bodyText?: string;
  snippet: string;
}) {
  const quoted = (message.bodyText ?? message.snippet)
    .split('\n')
    .map((line) => `> ${line}`)
    .join('\n');
  return `\n\nOn ${message.date}, ${message.from.email} wrote:\n${quoted}`;
}

export function MailDisplay() {
  const locale = useUIStore((s) => s.locale);
  const selectedMessageId = useUIStore((s) => s.selectedMessageId);
  const setSelectedMessage = useUIStore((s) => s.setSelectedMessage);
  const openCompose = useUIStore((s) => s.openCompose);
  const token = useAuthStore((s) => s.token);
  const cached = useMailStore((s) =>
    selectedMessageId ? s.messages[selectedMessageId] : undefined,
  );
  const upsertMessage = useMailStore((s) => s.upsertMessage);
  const markMessageRead = useMailStore((s) => s.markMessageRead);
  const toggleStar = useMailStore((s) => s.toggleStar);
  const removeMessage = useMailStore((s) => s.removeMessage);

  const [loadError, setLoadError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [replyText, setReplyText] = useState('');
  const today = new Date();

  useEffect(() => {
    setReplyText('');
  }, [selectedMessageId]);

  useEffect(() => {
    if (!selectedMessageId || !token) return;
    let cancelled = false;

    const load = async () => {
      setLoadError(null);
      try {
        const res = await fetch(`/api/messages/${selectedMessageId}`, {
          headers: { Authorization: `Bearer ${token}` },
        });
        if (!res.ok) throw new Error('Failed to load message');
        const msg = (await res.json()) as ApiMessage;
        if (cancelled) return;
        upsertMessage(mapApiMessage(msg));
        if (!msg.isRead) {
          const patch = await fetch(`/api/messages/${selectedMessageId}`, {
            method: 'PATCH',
            headers: {
              Authorization: `Bearer ${token}`,
              'Content-Type': 'application/json',
            },
            body: JSON.stringify({ isRead: true }),
          });
          if (patch.ok) markMessageRead(selectedMessageId);
        }
      } catch (err: unknown) {
        if (!cancelled) {
          setLoadError(err instanceof Error ? err.message : 'Failed to load message');
        }
      }
    };

    void load();
    return () => {
      cancelled = true;
    };
  }, [selectedMessageId, token, upsertMessage, markMessageRead]);

  const mail = cached ?? null;

  const handleReply = (all = false) => {
    if (!mail) return;
    const to = all
      ? [mail.from.email, ...mail.to.map((a) => a.email)].filter(Boolean).join(', ')
      : mail.from.email;
    openCompose({
      mode: 'reply',
      to,
      subject: mail.subject.startsWith('Re:') ? mail.subject : `Re: ${mail.subject}`,
      body: replyText || quoteBody(mail),
    });
  };

  const handleForward = () => {
    if (!mail) return;
    openCompose({
      mode: 'forward',
      to: '',
      subject: mail.subject.startsWith('Fwd:') ? mail.subject : `Fwd: ${mail.subject}`,
      body: quoteBody(mail),
    });
  };

  const handleAction = async (action: 'trash' | 'archive') => {
    if (!token || !mail || busy) return;
    setBusy(true);
    try {
      const res = await fetch(`/api/messages/${mail.id}/${action}`, {
        method: 'POST',
        headers: { Authorization: `Bearer ${token}` },
      });
      if (!res.ok) throw new Error(`${action} failed`);
      removeMessage(mail.id);
      setSelectedMessage(null);
    } catch {
      /* retry */
    } finally {
      setBusy(false);
    }
  };

  const handleSnooze = async (until: Date) => {
    if (!token || !mail || busy) return;
    setBusy(true);
    try {
      const res = await fetch(`/api/messages/${mail.id}/snooze`, {
        method: 'POST',
        headers: {
          Authorization: `Bearer ${token}`,
          'Content-Type': 'application/json',
        },
        body: JSON.stringify({ until: until.toISOString() }),
      });
      if (!res.ok) throw new Error('snooze failed');
      removeMessage(mail.id);
      setSelectedMessage(null);
    } catch {
      /* retry */
    } finally {
      setBusy(false);
    }
  };

  const handlePatch = async (body: { isRead?: boolean; isStarred?: boolean }) => {
    if (!token || !mail) return;
    const res = await fetch(`/api/messages/${mail.id}`, {
      method: 'PATCH',
      headers: {
        Authorization: `Bearer ${token}`,
        'Content-Type': 'application/json',
      },
      body: JSON.stringify(body),
    });
    if (!res.ok) return;
    if (body.isRead === false) {
      upsertMessage({ ...mail, isRead: false });
    }
    if (body.isStarred !== undefined) {
      toggleStar(mail.id);
    }
  };

  const fromLabel = mail ? (mail.from.name ?? mail.from.email) : '';
  const disabled = !mail || busy;
  const toolbarIconClass = 'text-foreground disabled:opacity-100';

  return (
    <div className="flex h-full flex-col">
      <div className="flex items-center p-2">
        <div className="flex items-center gap-2">
          <Tooltip>
            <TooltipTrigger asChild>
              <Button
                variant="ghost"
                size="icon"
                className={toolbarIconClass}
                disabled={disabled}
                onClick={() => void handleAction('archive')}
              >
                <Archive className="h-4 w-4" />
                <span className="sr-only">{t(locale, 'mail.archive')}</span>
              </Button>
            </TooltipTrigger>
            <TooltipContent>{t(locale, 'mail.archive')}</TooltipContent>
          </Tooltip>
          <Tooltip>
            <TooltipTrigger asChild>
              <Button
                variant="ghost"
                size="icon"
                className={toolbarIconClass}
                disabled={disabled}
                onClick={() => void handleAction('trash')}
              >
                <ArchiveX className="h-4 w-4" />
                <span className="sr-only">{t(locale, 'mail.moveToJunk')}</span>
              </Button>
            </TooltipTrigger>
            <TooltipContent>{t(locale, 'mail.moveToJunk')}</TooltipContent>
          </Tooltip>
          <Tooltip>
            <TooltipTrigger asChild>
              <Button
                variant="ghost"
                size="icon"
                className={toolbarIconClass}
                disabled={disabled}
                onClick={() => void handleAction('trash')}
              >
                <Trash2 className="h-4 w-4" />
                <span className="sr-only">{t(locale, 'mail.moveToTrash')}</span>
              </Button>
            </TooltipTrigger>
            <TooltipContent>{t(locale, 'mail.moveToTrash')}</TooltipContent>
          </Tooltip>
          <Separator orientation="vertical" className="mx-1 h-6" />
          <Tooltip>
            <Popover>
              <PopoverTrigger asChild>
                <TooltipTrigger asChild>
                  <Button
                    variant="ghost"
                    size="icon"
                    className={toolbarIconClass}
                    disabled={disabled}
                  >
                    <Clock className="h-4 w-4" />
                    <span className="sr-only">{t(locale, 'mail.snooze')}</span>
                  </Button>
                </TooltipTrigger>
              </PopoverTrigger>
              <PopoverContent className="flex w-[535px] p-0">
                <div className="flex flex-col gap-2 border-r px-2 py-4">
                  <div className="px-4 text-sm font-medium">{t(locale, 'mail.snoozeUntil')}</div>
                  <div className="grid min-w-[250px] gap-1">
                    <Button
                      variant="ghost"
                      className="justify-start font-normal"
                      onClick={() => void handleSnooze(addHours(today, 4))}
                    >
                      {t(locale, 'mail.laterToday')}{' '}
                      <span className="ml-auto text-muted-foreground">
                        {format(addHours(today, 4), 'E, h:m b')}
                      </span>
                    </Button>
                    <Button
                      variant="ghost"
                      className="justify-start font-normal"
                      onClick={() => void handleSnooze(addDays(today, 1))}
                    >
                      {t(locale, 'mail.tomorrow')}
                      <span className="ml-auto text-muted-foreground">
                        {format(addDays(today, 1), 'E, h:m b')}
                      </span>
                    </Button>
                    <Button
                      variant="ghost"
                      className="justify-start font-normal"
                      onClick={() => void handleSnooze(nextSaturday(today))}
                    >
                      {t(locale, 'mail.thisWeekend')}
                      <span className="ml-auto text-muted-foreground">
                        {format(nextSaturday(today), 'E, h:m b')}
                      </span>
                    </Button>
                    <Button
                      variant="ghost"
                      className="justify-start font-normal"
                      onClick={() => void handleSnooze(addDays(today, 7))}
                    >
                      {t(locale, 'mail.nextWeek')}
                      <span className="ml-auto text-muted-foreground">
                        {format(addDays(today, 7), 'E, h:m b')}
                      </span>
                    </Button>
                  </div>
                </div>
                <div className="p-2">
                  <Calendar
                    mode="single"
                    onSelect={(date) => {
                      if (!date) return;
                      const until = new Date(date);
                      until.setHours(18, 0, 0, 0);
                      void handleSnooze(until);
                    }}
                  />
                </div>
              </PopoverContent>
            </Popover>
            <TooltipContent>{t(locale, 'mail.snooze')}</TooltipContent>
          </Tooltip>
        </div>
        <div className="ml-auto flex items-center gap-2">
          <Tooltip>
            <TooltipTrigger asChild>
              <Button
                variant="ghost"
                size="icon"
                className={toolbarIconClass}
                disabled={disabled}
                onClick={() => handleReply()}
              >
                <Reply className="h-4 w-4" />
                <span className="sr-only">{t(locale, 'mail.reply')}</span>
              </Button>
            </TooltipTrigger>
            <TooltipContent>{t(locale, 'mail.reply')}</TooltipContent>
          </Tooltip>
          <Tooltip>
            <TooltipTrigger asChild>
              <Button
                variant="ghost"
                size="icon"
                className={toolbarIconClass}
                disabled={disabled}
                onClick={() => handleReply(true)}
              >
                <ReplyAll className="h-4 w-4" />
                <span className="sr-only">{t(locale, 'mail.replyAll')}</span>
              </Button>
            </TooltipTrigger>
            <TooltipContent>{t(locale, 'mail.replyAll')}</TooltipContent>
          </Tooltip>
          <Tooltip>
            <TooltipTrigger asChild>
              <Button
                variant="ghost"
                size="icon"
                className={toolbarIconClass}
                disabled={disabled}
                onClick={handleForward}
              >
                <Forward className="h-4 w-4" />
                <span className="sr-only">{t(locale, 'mail.forward')}</span>
              </Button>
            </TooltipTrigger>
            <TooltipContent>{t(locale, 'mail.forward')}</TooltipContent>
          </Tooltip>
        </div>
        <Separator orientation="vertical" className="mx-2 h-6" />
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button variant="ghost" size="icon" className={toolbarIconClass} disabled={disabled}>
              <MoreVertical className="h-4 w-4" />
              <span className="sr-only">{t(locale, 'mail.more')}</span>
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end">
            <DropdownMenuItem onClick={() => void handlePatch({ isRead: false })}>
              {t(locale, 'mail.markUnread')}
            </DropdownMenuItem>
            <DropdownMenuItem onClick={() => void handlePatch({ isStarred: !mail?.isStarred })}>
              {t(locale, 'mail.starThread')}
            </DropdownMenuItem>
            <DropdownMenuItem disabled>{t(locale, 'mail.addLabel')}</DropdownMenuItem>
            <DropdownMenuItem disabled>{t(locale, 'mail.muteThread')}</DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </div>
      <Separator />
      {mail ? (
        <div className="flex flex-1 flex-col">
          <div className="flex items-start p-4">
            <div className="flex items-start gap-4 text-sm">
              <Avatar className="h-10 w-10">
                <AvatarImage alt={fromLabel} />
                <AvatarFallback>{getInitials(fromLabel)}</AvatarFallback>
              </Avatar>
              <div className="grid gap-1">
                <div className="font-semibold">{fromLabel}</div>
                <div className="line-clamp-1 text-xs">{mail.subject}</div>
                <div className="line-clamp-1 text-xs">
                  <span className="font-medium">{t(locale, 'mail.replyTo')}:</span>{' '}
                  {mail.from.email}
                </div>
              </div>
            </div>
            {mail.date ? (
              <div className="ml-auto text-xs text-muted-foreground">
                {format(new Date(mail.date), 'PPpp')}
              </div>
            ) : null}
          </div>
          <Separator />
          <div className="flex-1 overflow-auto p-4 text-sm whitespace-pre-wrap">
            {loadError ? (
              <p className="text-muted-foreground">{loadError}</p>
            ) : mail.bodyHtml ? (
              <div dangerouslySetInnerHTML={{ __html: sanitizeEmailHtml(mail.bodyHtml) }} />
            ) : (
              (mail.bodyText ?? mail.snippet)
            )}
          </div>
          <Separator className="mt-auto" />
          <div className="p-4">
            <form
              onSubmit={(e) => {
                e.preventDefault();
                handleReply();
              }}
            >
              <div className="grid gap-4">
                <Textarea
                  className="p-4"
                  placeholder={t(locale, 'mail.replyPlaceholder', { name: fromLabel })}
                  value={replyText}
                  onChange={(e) => setReplyText(e.target.value)}
                />
                <div className="flex items-center">
                  <Label htmlFor="mute" className="flex items-center gap-2 text-xs font-normal">
                    <Switch id="mute" aria-label={t(locale, 'mail.muteThread')} />{' '}
                    {t(locale, 'mail.muteThread')}
                  </Label>
                  <Button type="submit" size="sm" className="ml-auto">
                    {t(locale, 'mail.send')}
                  </Button>
                </div>
              </div>
            </form>
          </div>
        </div>
      ) : (
        <div className="p-8 text-center text-muted-foreground">
          {t(locale, 'mail.selectMessage')}
        </div>
      )}
    </div>
  );
}
