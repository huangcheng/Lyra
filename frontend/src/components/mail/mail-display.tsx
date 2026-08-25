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
  MailOpen,
  MoreVertical,
  Reply,
  ReplyAll,
  Shield,
  Trash2,
} from 'lucide-react';
import { useCallback, useEffect, useRef, useState } from 'react';

import { EmptyState } from '@/components/empty-state';

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
import { api } from '@/lib/api-client';
import { MARK_READ_OPEN_DWELL_MS } from '@/lib/mark-read-policy';
import { markMessageReadOnServer } from '@/lib/mark-message-read';
import { mapApiMessage, type ApiMessage } from '@/lib/mail-api';
import { allowSenderPrivacy } from '@/lib/privacy-api';
import { getInitials } from '@/lib/utils';
import { useAuthStore } from '@/stores/auth';
import { useMailStore } from '@/stores/mail';
import { useUIStore } from '@/stores/ui';

/**
 * Sanitize attacker-controlled email HTML before rendering.
 * Defense in depth: the backend sanitizes at ingest; this guards the render
 * path against anything stored before that, or from other sources.
 * Strict config: no iframes/forms/embeds, no unknown protocols. CSS (`style`
 * tags and attributes) is allowed: HTML email layout depends on it, and the
 * sandboxed iframe + CSP confines what CSS can reach.
 */
function sanitizeEmailHtml(html: string): string {
  // Deliberately looser than the backend ingest policy (e.g. data: images
  // survive here): this layer also guards legacy rows stored before backend
  // sanitization existed, without re-deciding ingest-time trade-offs.
  return DOMPurify.sanitize(html, {
    FORBID_TAGS: ['iframe', 'object', 'embed', 'form', 'meta', 'link', 'base'],
  });
}

/**
 * Sandboxed iframe document with strict CSP for mail HTML (M1 remote-image
 * privacy). The iframe uses `allow-same-origin` so the parent can auto-size
 * it to the content; scripts still cannot run (no `allow-scripts`, and the
 * CSP blocks every load), and links open in a new, unsandboxed tab via
 * `allow-popups(-to-escape-sandbox)`.
 *
 * The base stylesheet supplies app-consistent typography for emails that
 * don't style every element themselves (otherwise they fall back to the
 * browser's serif defaults). The email's own inline styles always win.
 */
function mailHtmlSrcDoc(bodyHtml: string, allowRemoteImages: boolean, dark: boolean): string {
  const imgSrc = allowRemoteImages
    ? "'self' blob: cid: data: https: http:"
    : "'self' blob: cid: data:";
  const csp = `default-src 'none'; img-src ${imgSrc}; style-src 'unsafe-inline'`;
  const safe = sanitizeEmailHtml(bodyHtml);
  const baseCss =
    'body{margin:0;font:14px/1.55 -apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,"Helvetica Neue",Arial,"PingFang SC","Hiragino Sans GB","Microsoft YaHei",sans-serif;' +
    `color:${dark ? '#e4e4e7' : '#1a1a1a'};word-wrap:break-word;overflow-wrap:break-word}` +
    'img{max-width:100%;height:auto}table{max-width:100%}' +
    'pre,code{font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;font-size:.92em}' +
    'pre{white-space:pre-wrap}' +
    `blockquote{margin:0;padding-left:.8em;border-left:3px solid ${dark ? '#3f3f46' : '#dddddd'};color:${dark ? '#a1a1aa' : '#555555'}}` +
    'h1,h2,h3,h4{line-height:1.3}' +
    `hr{border:0;border-top:1px solid ${dark ? '#3f3f46' : '#e5e5e5'}}`;
  return `<!DOCTYPE html><html><head><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="${csp}"><style>${baseCss}</style></head><body>${safe}</body></html>`;
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

const SCROLL_END_THRESHOLD_PX = 32;

function isScrolledToBottom(el: HTMLElement): boolean {
  return el.scrollHeight - el.scrollTop - el.clientHeight <= SCROLL_END_THRESHOLD_PX;
}

export function MailDisplay() {
  const locale = useUIStore((s) => s.locale);
  const theme = useUIStore((s) => s.theme);
  const isDark =
    theme === 'dark' ||
    (theme === 'system' && window.matchMedia('(prefers-color-scheme: dark)').matches);
  const markReadPolicy = useUIStore((s) => s.markReadPolicy);
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
  const [actionError, setActionError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [replyText, setReplyText] = useState('');
  const [allowRemoteContent, setAllowRemoteContent] = useState(false);
  const [frameHeight, setFrameHeight] = useState(192);
  const [frameLoaded, setFrameLoaded] = useState(false);
  const today = new Date();
  const bodyScrollRef = useRef<HTMLDivElement>(null);
  const autoMarkedIdRef = useRef<string | null>(null);

  useEffect(() => {
    autoMarkedIdRef.current = null;
    setAllowRemoteContent(false);
    setFrameHeight(192);
    setFrameLoaded(false);
  }, [selectedMessageId]);

  const tryAutoMarkRead = useCallback(async () => {
    if (!selectedMessageId || !token || markReadPolicy === 'manual') return;
    if (autoMarkedIdRef.current === selectedMessageId) return;
    const ok = await markMessageReadOnServer(selectedMessageId);
    if (ok) autoMarkedIdRef.current = selectedMessageId;
  }, [selectedMessageId, token, markReadPolicy]);

  useEffect(() => {
    setReplyText('');
  }, [selectedMessageId]);

  useEffect(() => {
    if (!selectedMessageId || !token) return;
    let cancelled = false;

    const load = async () => {
      setLoadError(null);
      try {
        const qs = allowRemoteContent ? '?remote_content=allow' : '';
        const msg = await api<ApiMessage>(`/messages/${selectedMessageId}${qs}`);
        if (cancelled) return;
        upsertMessage(mapApiMessage(msg));
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
  }, [selectedMessageId, token, upsertMessage, allowRemoteContent]);

  const mail = cached ?? null;

  useEffect(() => {
    if (markReadPolicy !== 'on_open' || !mail || mail.isRead || loadError) return;
    const timer = window.setTimeout(() => {
      void tryAutoMarkRead();
    }, MARK_READ_OPEN_DWELL_MS);
    return () => window.clearTimeout(timer);
  }, [markReadPolicy, selectedMessageId, mail?.id, mail?.isRead, loadError, tryAutoMarkRead]);

  useEffect(() => {
    if (markReadPolicy !== 'on_scroll_end' || !mail || mail.isRead) return;
    const el = bodyScrollRef.current;
    if (!el) return;

    const onScroll = () => {
      if (isScrolledToBottom(el)) void tryAutoMarkRead();
    };

    const raf = requestAnimationFrame(() => {
      if (isScrolledToBottom(el)) void tryAutoMarkRead();
    });

    el.addEventListener('scroll', onScroll, { passive: true });
    return () => {
      cancelAnimationFrame(raf);
      el.removeEventListener('scroll', onScroll);
    };
  }, [
    markReadPolicy,
    mail?.id,
    mail?.isRead,
    mail?.bodyHtml,
    mail?.bodyText,
    mail?.snippet,
    tryAutoMarkRead,
  ]);

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

  const handleAction = async (action: 'trash' | 'archive' | 'spam') => {
    if (!token || !mail || busy) return;
    setBusy(true);
    setActionError(null);
    try {
      await api(`/messages/${mail.id}/${action}`, { method: 'POST' });
      removeMessage(mail.id);
      setSelectedMessage(null);
    } catch (err: unknown) {
      setActionError(err instanceof Error ? err.message : t(locale, 'common.error'));
    } finally {
      setBusy(false);
    }
  };

  const handleSnooze = async (until: Date) => {
    if (!token || !mail || busy) return;
    setBusy(true);
    try {
      await api(`/messages/${mail.id}/snooze`, {
        method: 'POST',
        body: JSON.stringify({ until: until.toISOString() }),
      });
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
    try {
      await api(`/messages/${mail.id}`, {
        method: 'PATCH',
        body: JSON.stringify(body),
      });
    } catch {
      return;
    }
    if (body.isRead === false) {
      upsertMessage({ ...mail, isRead: false });
    }
    if (body.isRead === true) {
      markMessageRead(mail.id);
    }
    if (body.isStarred !== undefined) {
      toggleStar(mail.id);
    }
  };

  const fromLabel = mail ? (mail.from.name ?? mail.from.email) : '';
  const disabled = !mail || busy;
  const toolbarIconClass =
    'rounded-[7px] border border-input bg-card text-foreground shadow-xs hover:bg-accent disabled:opacity-50';
  const showRemoteBanner = Boolean(mail?.remoteContentBlocked);

  const handleShowRemoteContent = () => {
    setAllowRemoteContent(true);
  };

  const handleAlwaysShowFromSender = async () => {
    if (!mail) return;
    try {
      await allowSenderPrivacy(mail.from.email);
      setAllowRemoteContent(true);
    } catch {
      /* retry */
    }
  };

  return (
    <div className="flex h-full flex-col">
      <div className="flex items-center p-2">
        <div className="flex items-center gap-1.5">
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
                onClick={() => void handleAction('spam')}
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
                        {format(addHours(today, 4), 'h:mm a')}
                      </span>
                    </Button>
                    <Button
                      variant="ghost"
                      className="justify-start font-normal"
                      onClick={() => void handleSnooze(addDays(today, 1))}
                    >
                      {t(locale, 'mail.tomorrow')}
                      <span className="ml-auto text-muted-foreground">
                        {format(addDays(today, 1), 'h:mm a')}
                      </span>
                    </Button>
                    <Button
                      variant="ghost"
                      className="justify-start font-normal"
                      onClick={() => void handleSnooze(nextSaturday(today))}
                    >
                      {t(locale, 'mail.thisWeekend')}
                      <span className="ml-auto text-muted-foreground">
                        {format(nextSaturday(today), 'h:mm a')}
                      </span>
                    </Button>
                    <Button
                      variant="ghost"
                      className="justify-start font-normal"
                      onClick={() => void handleSnooze(addDays(today, 7))}
                    >
                      {t(locale, 'mail.nextWeek')}
                      <span className="ml-auto text-muted-foreground">
                        {format(addDays(today, 7), 'h:mm a')}
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
        <div className="ml-auto flex items-center gap-1.5">
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
            {mail && !mail.isRead ? (
              <DropdownMenuItem onClick={() => void handlePatch({ isRead: true })}>
                {t(locale, 'mail.markRead')}
              </DropdownMenuItem>
            ) : null}
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
      {actionError ? (
        <div className="border-b bg-destructive/10 px-4 py-2 text-sm text-destructive">
          {actionError}
        </div>
      ) : null}
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
                <div className="line-clamp-1 text-[13px] font-medium">{mail.subject}</div>
                <div className="line-clamp-1 text-xs text-muted-foreground">
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
          {showRemoteBanner ? (
            <div className="px-4 pt-3">
              <div className="flex items-center gap-2 rounded-lg border border-border px-3.5 py-2.5 text-[12.5px] text-muted-foreground">
                <Shield className="size-3.5 shrink-0" aria-hidden />
                <span className="min-w-0 flex-1">{t(locale, 'mail.remoteContentHidden')}</span>
                <button
                  type="button"
                  className="shrink-0 font-medium text-foreground underline underline-offset-2 hover:text-foreground/80"
                  onClick={handleShowRemoteContent}
                >
                  {t(locale, 'mail.showRemoteContent')}
                </button>
                <button
                  type="button"
                  className="shrink-0 font-medium text-foreground underline underline-offset-2 hover:text-foreground/80"
                  onClick={() => void handleAlwaysShowFromSender()}
                >
                  {t(locale, 'mail.alwaysShowFromSender')}
                </button>
              </div>
            </div>
          ) : null}
          <div ref={bodyScrollRef} className="flex-1 overflow-auto p-4 text-sm whitespace-pre-wrap">
            {loadError ? (
              <p className="text-destructive">{loadError}</p>
            ) : mail.bodyHtml ? (
              <div className="relative">
                {frameLoaded ? null : (
                  <div className="space-y-3 py-1" aria-hidden>
                    <div className="h-4 w-2/3 animate-pulse rounded bg-muted" />
                    <div className="h-4 w-full animate-pulse rounded bg-muted" />
                    <div className="h-4 w-5/6 animate-pulse rounded bg-muted" />
                    <div className="h-32 w-full animate-pulse rounded bg-muted" />
                  </div>
                )}
                <iframe
                  title={mail.subject}
                  sandbox="allow-same-origin allow-popups allow-popups-to-escape-sandbox"
                  className={
                    frameLoaded
                      ? 'w-full border-0 bg-transparent opacity-100 transition-opacity duration-150'
                      : 'absolute inset-x-0 top-0 w-full border-0 bg-transparent opacity-0'
                  }
                  style={{ height: frameHeight }}
                  onLoad={(e) => {
                    const doc = e.currentTarget.contentDocument;
                    if (!doc) return;
                    const height = Math.max(
                      192,
                      Math.min(doc.documentElement.scrollHeight + 2, 100_000),
                    );
                    setFrameHeight(height);
                    setFrameLoaded(true);
                  }}
                  srcDoc={mailHtmlSrcDoc(mail.bodyHtml, !mail.remoteContentBlocked, isDark)}
                />
              </div>
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
              <div className="rounded-lg border border-input bg-card shadow-xs">
                <div className="flex items-start gap-2.5 px-3.5 pt-3">
                  <Reply className="mt-2 size-4 shrink-0 text-muted-foreground" aria-hidden />
                  <Textarea
                    className="min-h-12 resize-none border-0 px-0 py-1.5 shadow-none focus-visible:border-transparent focus-visible:ring-0 dark:bg-transparent"
                    placeholder={t(locale, 'mail.replyPlaceholder', { name: fromLabel })}
                    value={replyText}
                    onChange={(e) => setReplyText(e.target.value)}
                  />
                </div>
                <div className="flex items-center px-3.5 pb-2.5">
                  <Label htmlFor="mute" className="flex items-center gap-2 text-xs font-normal">
                    <Switch id="mute" aria-label={t(locale, 'mail.muteThread')} />{' '}
                    {t(locale, 'mail.muteThread')}
                  </Label>
                  <Button type="submit" size="sm" className="ml-auto rounded-full px-4">
                    {t(locale, 'mail.send')}
                  </Button>
                </div>
              </div>
            </form>
          </div>
        </div>
      ) : (
        <EmptyState icon={MailOpen} title={t(locale, 'mail.selectMessage')} />
      )}
    </div>
  );
}
