/**
 * Reading pane — conversation stack.
 *
 * Messages that share an account + normalized subject (Re:/Fwd: stripped)
 * render as Fastmail-style stacked cards: oldest first, unread and the
 * latest expanded, the rest collapsed to a one-line header. Toolbar
 * actions and the inline reply act on the selected message.
 */

import { addDays, addHours, format, nextSaturday } from 'date-fns';
import {
  Archive,
  ArchiveX,
  Check,
  ChevronLeft,
  Clock,
  FolderInput,
  Forward,
  MailOpen,
  MoreVertical,
  Reply,
  ReplyAll,
  Trash2,
} from 'lucide-react';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';

import { EmptyState } from '@/components/empty-state';
import { MessageCard } from '@/components/mail/message-card';
import { RichTextEditor } from '@/components/compose/rich-text-editor';

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
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip';
import { t } from '@/i18n';
import { api } from '@/lib/api-client';
import { baseSubject, conversationKeyOf, conversationMembers } from '@/lib/conversation';
import { MARK_READ_OPEN_DWELL_MS } from '@/lib/mark-read-policy';
import { markMessageReadOnServer } from '@/lib/mark-message-read';
import { forwardHtml, quotedReplyHtml, textToHtml } from '@/lib/compose-html';
import { mapApiMessage, type ApiMessage } from '@/lib/mail-api';
import { sanitizeEmailHtml } from '@/lib/sanitize-email-html';
import { useMediaQuery } from '@/lib/use-media-query';
import { useAuthStore } from '@/stores/auth';
import { useMailStore } from '@/stores/mail';
import { useUIStore } from '@/stores/ui';

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

/** Plain-text fallback for an HTML reply body (block tags become newlines). */
function htmlToPlainText(html: string): string {
  const withBreaks = html
    .replace(/<br\s*\/?>/gi, '\n')
    .replace(/<\/(p|div|li|blockquote|h[1-6])>/gi, '\n');
  const doc = new DOMParser().parseFromString(withBreaks, 'text/html');
  return (doc.body.textContent ?? '').replace(/\n{3,}/g, '\n\n').trim();
}

export function MailDisplay() {
  const locale = useUIStore((s) => s.locale);
  const markReadPolicy = useUIStore((s) => s.markReadPolicy);
  const selectedMessageId = useUIStore((s) => s.selectedMessageId);
  const setSelectedMessage = useUIStore((s) => s.setSelectedMessage);
  const openCompose = useUIStore((s) => s.openCompose);
  const mutedMessageIds = useUIStore((s) => s.mutedMessageIds);
  const toggleMuteMessage = useUIStore((s) => s.toggleMuteMessage);
  const token = useAuthStore((s) => s.token);
  const isMobile = useMediaQuery('(max-width: 1023px)');
  const mail = useMailStore((s) => (selectedMessageId ? s.messages[selectedMessageId] : null));
  const messages = useMailStore((s) => s.messages);
  const folders = useMailStore((s) => s.folders);
  const accounts = useMailStore((s) => s.accounts);
  const upsertMessage = useMailStore((s) => s.upsertMessage);
  const markMessageRead = useMailStore((s) => s.markMessageRead);
  const toggleStar = useMailStore((s) => s.toggleStar);
  const removeMessage = useMailStore((s) => s.removeMessage);

  const [actionError, setActionError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [replyHtml, setReplyHtml] = useState('');
  const [replyNonce, setReplyNonce] = useState(0);
  const bodyScrollRef = useRef<HTMLDivElement>(null);
  const autoMarkedIdRef = useRef<string | null>(null);
  const today = new Date();

  const conversation = useMemo(
    () => (mail ? conversationMembers(mail, messages) : []),
    [mail, messages],
  );
  const conversationKey = mail ? conversationKeyOf(mail) : null;

  // Conversation partners may live in folders the current view never
  // loaded (e.g. the Inbox original while reading Sent). Subject search is
  // unreliable for long special-char subjects (FTS tokenization), so
  // prefetch the whole account listing once per account instead.
  const accountPrefetchRef = useRef<Set<string>>(new Set());
  useEffect(() => {
    if (!mail || !token) return;
    const accountId = mail.accountId;
    if (accountPrefetchRef.current.has(accountId)) return;
    accountPrefetchRef.current.add(accountId);
    const params = new URLSearchParams({ accountId });
    void api<ApiMessage[]>(`/messages?${params}`)
      .then((data) => {
        for (const raw of data) upsertMessage(mapApiMessage(raw));
      })
      .catch(() => {
        accountPrefetchRef.current.delete(accountId);
      });
  }, [mail, token, upsertMessage]);

  // Hide partners sitting in Trash unless the selected message is there.
  const visibleConversation = useMemo(() => {
    if (!mail) return [];
    const selectedInTrash = folders[mail.folderId]?.role === 'trash';
    if (selectedInTrash) return conversation;
    return conversation.filter((m) => folders[m.folderId]?.role !== 'trash');
  }, [mail, conversation, folders]);

  // Expand state: unread and the latest message open by default; the user
  // can toggle any card. Overrides reset when the conversation changes.
  const [expandOverrides, setExpandOverrides] = useState<Record<string, boolean>>({});
  useEffect(() => {
    setExpandOverrides({});
  }, [conversationKey]);
  const latestId = visibleConversation.length
    ? visibleConversation[visibleConversation.length - 1].id
    : null;
  const isExpanded = useCallback(
    (id: string, isRead: boolean) => expandOverrides[id] ?? (!isRead || id === latestId),
    [expandOverrides, latestId],
  );

  useEffect(() => {
    autoMarkedIdRef.current = null;
    setReplyHtml('');
  }, [selectedMessageId]);

  const tryAutoMarkRead = useCallback(async () => {
    if (!selectedMessageId || !token || markReadPolicy === 'manual') return;
    if (autoMarkedIdRef.current === selectedMessageId) return;
    const ok = await markMessageReadOnServer(selectedMessageId);
    if (ok) autoMarkedIdRef.current = selectedMessageId;
  }, [selectedMessageId, token, markReadPolicy]);

  useEffect(() => {
    if (markReadPolicy !== 'on_open' || !mail || mail.isRead) return;
    const timer = window.setTimeout(() => {
      void tryAutoMarkRead();
    }, MARK_READ_OPEN_DWELL_MS);
    return () => window.clearTimeout(timer);
  }, [markReadPolicy, selectedMessageId, mail?.id, mail?.isRead, tryAutoMarkRead]);

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
  }, [markReadPolicy, mail?.id, mail?.isRead, tryAutoMarkRead, visibleConversation.length]);

  const handleReply = (all = false) => {
    if (!mail) return;
    const to = all
      ? [mail.from.email, ...mail.to.map((a) => a.email)].filter(Boolean).join(', ')
      : mail.from.email;
    const source = {
      fromName: mail.from.name ?? '',
      fromEmail: mail.from.email,
      date: mail.date,
      bodyHtml: mail.bodyHtml ? sanitizeEmailHtml(mail.bodyHtml) : undefined,
      bodyText: mail.bodyText,
    };
    openCompose({
      mode: 'reply',
      to,
      subject: mail.subject.startsWith('Re:') ? mail.subject : `Re: ${mail.subject}`,
      body: quoteBody(mail),
      initialHtml: quotedReplyHtml(source, signatureOf(mail.accountId)),
    });
  };

  const handleForward = () => {
    if (!mail) return;
    // Forward carries the original's regular attachments (inline images stay
    // in the quoted body). Reply intentionally drops them.
    const forwardAttachments = (mail.attachments ?? [])
      .filter((a) => !a.isInline)
      .map((a) => ({ id: a.id, filename: a.filename, contentType: a.contentType }));
    const source = {
      fromName: mail.from.name ?? '',
      fromEmail: mail.from.email,
      date: mail.date,
      bodyHtml: mail.bodyHtml ? sanitizeEmailHtml(mail.bodyHtml) : undefined,
      bodyText: mail.bodyText,
    };
    openCompose({
      mode: 'forward',
      to: '',
      subject: mail.subject.startsWith('Fwd:') ? mail.subject : `Fwd: ${mail.subject}`,
      body: quoteBody(mail),
      initialHtml: forwardHtml(source, signatureOf(mail.accountId)),
      forwardAttachments: forwardAttachments.length > 0 ? forwardAttachments : undefined,
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

  const handleEditDraft = () => {
    if (!mail) return;
    openCompose({
      mode: 'draft',
      to: mail.to.map((a) => a.email).join(', '),
      cc: (mail.cc ?? []).map((a) => a.email).join(', '),
      subject: mail.subject ?? '',
      body: mail.bodyText ?? '',
      initialHtml: mail.bodyHtml ?? textToHtml(mail.bodyText ?? ''),
      draftMessageId: mail.id,
    });
  };

  const handleMoveToFolder = async (folderId: string) => {
    if (!token || !mail || busy || folderId === mail.folderId) return;
    setBusy(true);
    setActionError(null);
    try {
      await api(`/messages/${mail.id}/move`, {
        method: 'POST',
        body: JSON.stringify({ folderId }),
      });
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
    setActionError(null);
    try {
      await api(`/messages/${mail.id}/snooze`, {
        method: 'POST',
        body: JSON.stringify({ until: until.toISOString() }),
      });
      removeMessage(mail.id);
      setSelectedMessage(null);
    } catch (err: unknown) {
      setActionError(err instanceof Error ? err.message : t(locale, 'common.error'));
    } finally {
      setBusy(false);
    }
  };

  const handleInlineSend = async () => {
    if (!token || !mail || busy) return;
    const html = replyHtml;
    const text = htmlToPlainText(html);
    if (!text) {
      handleReply();
      return;
    }
    const accountId = mail.accountId || accounts[0]?.id;
    if (!accountId) {
      setActionError(t(locale, 'settings.accounts.empty'));
      return;
    }
    setBusy(true);
    setActionError(null);
    try {
      await api('/messages/send', {
        method: 'POST',
        body: JSON.stringify({
          accountId,
          to: [{ email: mail.from.email }],
          subject: mail.subject.startsWith('Re:') ? mail.subject : `Re: ${mail.subject}`,
          bodyText: text,
          bodyHtml: html,
        }),
      });
      setReplyHtml('');
      setReplyNonce((n) => n + 1); // remount the editor cleared
    } catch (err: unknown) {
      setActionError(err instanceof Error ? err.message : t(locale, 'mail.sendError'));
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

  const signatureOf = (accountId: string): string | undefined =>
    accounts.find((a) => a.id === accountId)?.signature ?? undefined;

  const accountFolders = useMemo(
    () =>
      Object.values(folders)
        .filter((f) => f.accountId === mail?.accountId)
        .sort((a, b) => a.sortOrder - b.sortOrder || a.name.localeCompare(b.name)),
    [folders, mail?.accountId],
  );

  const fromLabel = mail ? (mail.from.name ?? mail.from.email) : '';
  const disabled = !mail || busy;
  const toolbarIconClass =
    'rounded-[7px] text-ter-foreground hover:bg-accent hover:text-foreground disabled:opacity-50';

  return (
    <div className="flex h-full flex-col">
      <div className="flex items-center overflow-x-auto p-2">
        <div className="flex items-center gap-1.5">
          {isMobile ? (
            <Button
              variant="ghost"
              size="icon"
              className={toolbarIconClass}
              onClick={() => setSelectedMessage(null)}
            >
              <ChevronLeft className="h-4 w-4" />
              <span className="sr-only">{t(locale, 'common.back')}</span>
            </Button>
          ) : null}
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
                    <FolderInput className="h-4 w-4" />
                    <span className="sr-only">{t(locale, 'mail.moveToFolder')}</span>
                  </Button>
                </TooltipTrigger>
              </PopoverTrigger>
              <TooltipContent>{t(locale, 'mail.moveToFolder')}</TooltipContent>
              <PopoverContent className="w-60 p-1" align="start">
                <div className="max-h-72 overflow-y-auto">
                  {accountFolders.length === 0 ? (
                    <p className="px-2 py-1.5 text-xs text-muted-foreground">
                      {t(locale, 'mail.noFolders')}
                    </p>
                  ) : (
                    accountFolders.map((f) => (
                      <button
                        key={f.id}
                        type="button"
                        disabled={busy || f.id === mail?.folderId}
                        className="flex w-full items-center justify-between gap-2 rounded-sm px-2 py-1.5 text-sm hover:bg-accent disabled:opacity-40"
                        onClick={() => void handleMoveToFolder(f.id)}
                      >
                        <span className="truncate">{f.name}</span>
                        {f.id === mail?.folderId ? <Check className="size-3.5 shrink-0" /> : null}
                      </button>
                    ))
                  )}
                </div>
              </PopoverContent>
            </Popover>
          </Tooltip>
          {mail?.isDraft ? (
            <Tooltip>
              <TooltipTrigger asChild>
                <Button
                  variant="ghost"
                  size="icon"
                  className={toolbarIconClass}
                  disabled={disabled}
                  onClick={handleEditDraft}
                >
                  <Reply className="h-4 w-4" />
                  <span className="sr-only">{t(locale, 'mail.editDraft')}</span>
                </Button>
              </TooltipTrigger>
              <TooltipContent>{t(locale, 'mail.editDraft')}</TooltipContent>
            </Tooltip>
          ) : null}
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
            <DropdownMenuItem
              onClick={() => {
                if (!mail) return;
                toggleMuteMessage(mail.id);
                if (!mutedMessageIds.includes(mail.id)) {
                  setSelectedMessage(null);
                }
              }}
            >
              {mail && mutedMessageIds.includes(mail.id)
                ? t(locale, 'mail.unmuteThread')
                : t(locale, 'mail.muteThread')}
            </DropdownMenuItem>
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
          <div ref={bodyScrollRef} className="min-h-0 flex-1 overflow-auto">
            {visibleConversation.length > 1 ? (
              <div className="border-b border-border/70 px-4 pt-4 pb-3">
                <h2 className="font-display text-lg font-medium">
                  {baseSubject(mail.subject) || mail.subject}
                </h2>
                <p className="mt-0.5 text-xs text-muted-foreground">
                  {t(locale, 'mail.conversationCount', { count: visibleConversation.length })}
                </p>
              </div>
            ) : null}
            {visibleConversation.map((member) => (
              <MessageCard
                key={member.id}
                messageId={member.id}
                expanded={isExpanded(member.id, member.isRead)}
                hideSubject={visibleConversation.length > 1}
                onToggle={() =>
                  setExpandOverrides((prev) => ({
                    ...prev,
                    [member.id]: !isExpanded(member.id, member.isRead),
                  }))
                }
              />
            ))}
          </div>
          <Separator className="mt-auto" />
          <div className="p-4">
            <form
              onSubmit={(e) => {
                e.preventDefault();
                void handleInlineSend();
              }}
            >
              <div className="rounded-lg border border-input bg-card">
                <RichTextEditor
                  key={`${selectedMessageId}:${replyNonce}`}
                  className="rounded-none border-0"
                  contentClassName="max-h-48 min-h-16"
                  initialHtml=""
                  onChange={setReplyHtml}
                  placeholder={t(locale, 'mail.replyPlaceholder', { name: fromLabel })}
                  disabled={busy}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
                      e.preventDefault();
                      void handleInlineSend();
                    }
                  }}
                />
                <div className="flex items-center px-3.5 pb-2.5">
                  <Label htmlFor="mute" className="flex items-center gap-2 text-xs font-normal">
                    <Switch
                      id="mute"
                      checked={Boolean(mail && mutedMessageIds.includes(mail.id))}
                      onCheckedChange={() => {
                        if (!mail) return;
                        const willMute = !mutedMessageIds.includes(mail.id);
                        toggleMuteMessage(mail.id);
                        if (willMute) setSelectedMessage(null);
                      }}
                      aria-label={t(locale, 'mail.muteThread')}
                    />{' '}
                    {t(locale, 'mail.muteThread')}
                  </Label>
                  <Button
                    type="submit"
                    variant="outline"
                    size="sm"
                    className="ml-auto rounded-full px-4"
                    disabled={busy}
                  >
                    {busy ? t(locale, 'mail.sending') : t(locale, 'mail.send')}
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
